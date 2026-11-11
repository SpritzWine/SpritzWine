//! I/O completion queue (stage 4): Rust replacement for the global `ioqueue` +
//! ioqueue_thread_proc + tp_ioqueue_lock + TpStartAsyncIoOperation.
//!
//! A single service thread blocks on the completion port; when the OS posts a
//! completion (keyed by the I/O object), it accounts one pending op, queues the
//! completion record on the object, and fires it via `on_submit` (the wiring
//! points this at Pool::submit). The pool worker then dispatches the I/O callback,
//! which pops that completion. The thread lives while any I/O object is bound,
//! and for WORKER_TIMEOUT after the last one goes.
//!
//! The per-object completion state ([`IoState`]) lives on the object (the C
//! `u.io`), guarded by its own small lock, since both the I/O thread (push) and a
//! pool worker (pop, at dispatch) touch it.
//!
//! Scope: the full post -> submit -> dispatch path plus the shutdown/skip/destroy
//! bookkeeping (the "expected completions" model, C `bdba5ba5f65`): async ops are
//! counted as `pending_count`; a cancel via TpWaitForIoCompletion(.,TRUE) moves
//! them to `skipped_count` (their completions still arrive at the port and must be
//! drained); TpReleaseIoCompletion sets `shutting_down` and only destroys the
//! object once no completions are outstanding, otherwise the I/O thread destroys it
//! when the last skipped completion drains. This is what keeps a completion from
//! being delivered to a freed object.

use crate::threadpool::{tp_object_release, PoolOs, ThreadpoolObject, WORKER_TIMEOUT_100NS};
use crate::work_queue::SpinLock;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

/// Completion slots reserved at TpAllocIoCompletion, as in the C.
const INITIAL_COMPLETIONS: usize = 8;

struct Completion {
    status: i32,
    information: usize,
    cvalue: usize,
}

/// What the I/O service thread must do with a completion it just dequeued.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum IoAction {
    /// Queue the completion + submit the object to the pool (fire the callback).
    Submit,
    /// The object is shutting down and this was its last outstanding completion:
    /// account it out of the queue and release the object.
    Destroy,
    /// Drop the completion (cancelled op, or a skipped one still draining).
    Ignore,
}

struct IoInner {
    /// Async ops started (C `TpStartAsyncIoOperation`) but not yet completed.
    pending_count: i32,
    /// Ops cancelled while pending (C `tp_object_cancel`): their completions still
    /// arrive at the port during shutdown and must be drained before destroying.
    skipped_count: i32,
    /// TpReleaseIoCompletion has been called (C `u.io.shutting_down`).
    shutting_down: bool,
    completions: Vec<Completion>,
}

/// Per-I/O-object completion state (the C `object->u.io`).
pub struct IoState {
    lock: SpinLock,
    inner: UnsafeCell<IoInner>,
}

// Touched by the I/O thread (push) and a pool worker (pop); all under `lock`.
unsafe impl Send for IoState {}
unsafe impl Sync for IoState {}

impl IoState {
    pub fn new() -> Self {
        IoState {
            lock: SpinLock::new(),
            inner: UnsafeCell::new(IoInner {
                pending_count: 0,
                skipped_count: 0,
                shutting_down: false,
                completions: Vec::new(),
            }),
        }
    }

    /// As [`new`](Self::new), reserving the C's initial completion array
    /// (`8 * sizeof(*completions)`); `None` if the heap refuses it.
    pub fn try_new() -> Option<Self> {
        let s = Self::new();
        unsafe { &mut *s.inner.get() }.completions.try_reserve_exact(INITIAL_COMPLETIONS).ok()?;
        Some(s)
    }

    /// Account an async op that is about to be started (C `TpStartAsyncIoOperation`).
    pub fn start_async(&self) {
        self.lock.lock();
        unsafe { &mut *self.inner.get() }.pending_count += 1;
        self.lock.unlock();
    }

    /// Account an async op that will not complete (C `TpCancelAsyncIoOperation`):
    /// just drops one pending op. Its completion, if it still arrives, is dropped
    /// because `pending_count` is 0 by then.
    pub fn cancel_async(&self) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        if inner.pending_count > 0 {
            inner.pending_count -= 1;
        }
        self.lock.unlock();
    }

    /// Ops pending but cancelled become "skipped" (C `tp_object_cancel` for I/O):
    /// their completions still reach the port and must be drained during shutdown.
    pub(crate) fn cancel_to_skipped(&self) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        inner.skipped_count += inner.pending_count;
        inner.pending_count = 0;
        self.lock.unlock();
    }

    /// Whether the object still owes an I/O callback -- either an async op is
    /// outstanding (C `object_is_finished` checks `u.io.pending_count`) OR a
    /// completion has been queued but its callback hasn't been dispatched yet.
    ///
    /// The second half closes a race the C avoids by doing pending-- and the submit
    /// accounting under one lock: here `handle_completion` decrements pending and
    /// queues the completion atomically, but the submit's running++ happens later,
    /// so a queued completion must count as busy or TpWaitForIoCompletion(.,FALSE)
    /// could return before the callback runs.
    pub(crate) fn is_busy(&self) -> bool {
        self.lock.lock();
        let inner = unsafe { &*self.inner.get() };
        let busy = inner.pending_count > 0 || !inner.completions.is_empty();
        self.lock.unlock();
        busy
    }

    /// Begin teardown (C `TpReleaseIoCompletion`): mark shutting-down and report
    /// whether the object can be destroyed right now (nothing left to drain).
    pub(crate) fn begin_shutdown(&self) -> bool {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        inner.shutting_down = true;
        let can_destroy = inner.pending_count == 0 && inner.skipped_count == 0;
        self.lock.unlock();
        can_destroy
    }

    /// A completion just arrived from the port (the body of C `ioqueue_thread_proc`).
    /// Decide what the service thread does with it.
    pub(crate) fn handle_completion(&self, status: i32, information: usize, cvalue: usize) -> IoAction {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        if inner.shutting_down && inner.pending_count == 0 {
            // Draining: consume one skipped completion; destroy on the last.
            if inner.skipped_count > 0 {
                inner.skipped_count -= 1;
            }
            let act = if inner.skipped_count > 0 { IoAction::Ignore } else { IoAction::Destroy };
            self.lock.unlock();
            return act;
        }
        if inner.pending_count > 0 {
            inner.pending_count -= 1;
            inner.completions.push(Completion { status, information, cvalue });
            self.lock.unlock();
            return IoAction::Submit;
        }
        // No op pending (e.g. cancelled via TpCancelAsyncIoOperation): drop it.
        self.lock.unlock();
        IoAction::Ignore
    }

    /// Pop the next queued completion (at dispatch). `(status, information, cvalue)`.
    pub(crate) fn pop_completion(&self) -> Option<(i32, usize, usize)> {
        self.lock.lock();
        let c = unsafe { &mut *self.inner.get() }.completions.pop();
        self.lock.unlock();
        c.map(|c| (c.status, c.information, c.cvalue))
    }
}

impl Default for IoState {
    fn default() -> Self {
        Self::new()
    }
}

/// OS completion-port primitives (wine: NtCreateIoCompletion / NtSetInformationFile
/// / NtRemoveIoCompletion / NtSetIoCompletion). Handles are opaque `u64`.
pub trait IoOs: PoolOs {
    fn create_port(&self) -> u64;
    fn close_port(&self, port: u64);
    /// Bind `file` to `port` with completion key `key` (wine: NtSetInformationFile).
    fn associate(&self, port: u64, file: u64, key: u64);
    /// Block until a completion is dequeued: `(key, cvalue, status, information)`.
    /// With a timeout, `None` once it elapses with nothing dequeued.
    fn remove_completion(&self, port: u64, timeout_100ns: Option<i64>) -> Option<(u64, usize, i32, usize)>;
    /// Post a completion (shutdown/wakeup sentinel; wine: NtSetIoCompletion).
    fn post_completion(&self, port: u64, key: u64, cvalue: usize, status: i32, information: usize);
}

struct IoQInner {
    port: u64,
    objcount: i32,
    thread_running: bool,
}

/// The global I/O completion service.
pub struct IoQueue<Os: IoOs> {
    lock: SpinLock,
    inner: UnsafeCell<IoQInner>,
    shutdown: AtomicBool,
    /// How long the service thread outlives the last I/O object.
    idle_timeout_100ns: i64,
    on_submit: fn(*mut ThreadpoolObject),
    os: Os,
}

unsafe impl<Os: IoOs> Sync for IoQueue<Os> {}
unsafe impl<Os: IoOs> Send for IoQueue<Os> {}

extern "system" fn io_service_entry<Os: IoOs>(arg: *mut c_void) -> u32 {
    // SAFETY: the IoQueue outlives the thread (joined at shutdown).
    let q = unsafe { &*(arg as *const IoQueue<Os>) };
    q.run_io();
    0
}

impl<Os: IoOs> IoQueue<Os> {
    pub fn new(os: Os, on_submit: fn(*mut ThreadpoolObject)) -> Self {
        IoQueue {
            lock: SpinLock::new(),
            inner: UnsafeCell::new(IoQInner { port: 0, objcount: 0, thread_running: false }),
            shutdown: AtomicBool::new(false),
            idle_timeout_100ns: WORKER_TIMEOUT_100NS,
            on_submit,
            os,
        }
    }

    /// Override the idle timeout (before the queue is shared).
    pub fn set_idle_timeout(&mut self, t100ns: i64) {
        self.idle_timeout_100ns = t100ns;
    }

    /// Bind an I/O object + its `file` to the port (C `tp_ioqueue_lock`): create
    /// the port + service thread on first use, associate the file (keyed by the
    /// object), and account the object so the thread stays alive.
    ///
    /// # Safety
    /// `io` must be a live I/O [`ThreadpoolObject`] (from `new_io`) until [`obj_removed`].
    pub unsafe fn obj_added(&self, io: *mut ThreadpoolObject, file: u64) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        if inner.port == 0 {
            inner.port = self.os.create_port();
        }
        if !inner.thread_running
            && self.os.spawn(io_service_entry::<Os>, self as *const _ as *mut c_void)
        {
            inner.thread_running = true;
        }
        let port = inner.port;
        inner.objcount += 1;
        self.lock.unlock();
        self.os.associate(port, file, io as u64);
    }

    /// Account an I/O object leaving the queue on the immediate-destroy path (C
    /// `tp_ioqueue_unlock`, via tp_object_prepare_shutdown). Wake the thread if it
    /// was the last, so it starts its idle timeout.
    pub fn unlock(&self) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        inner.objcount -= 1;
        let last = inner.objcount == 0;
        let port = inner.port;
        self.lock.unlock();
        if last {
            // Sentinel (key 0) so the thread re-checks objcount.
            self.os.post_completion(port, 0, 0, 0, 0);
        }
    }

    fn run_io(&self) {
        let port = {
            self.lock.lock();
            let p = unsafe { &*self.inner.get() }.port;
            self.lock.unlock();
            p
        };
        let mut timed_out = false;
        loop {
            self.lock.lock();
            let inner = unsafe { &mut *self.inner.get() };
            let idle = inner.objcount == 0;
            // No I/O object left: exit only after a full idle timeout in which
            // none was created (C ioqueue_thread_proc).
            let done = self.shutdown.load(Ordering::Acquire) || (idle && timed_out);
            if done {
                inner.thread_running = false;
            }
            self.lock.unlock();
            if done {
                return; // port persists for a future obj_added to reuse
            }

            let timeout = if idle { Some(self.idle_timeout_100ns) } else { None };
            let Some((key, cvalue, status, information)) = self.os.remove_completion(port, timeout) else {
                timed_out = true;
                continue;
            };
            timed_out = false;
            if key != 0 {
                let io = key as *mut ThreadpoolObject;
                // SAFETY: the key stays a live I/O object until it is destroyed here
                // (Destroy) or on the immediate-release path; the "expected
                // completions" accounting guarantees no completion outlives it.
                match unsafe { (*io).io_state() }.handle_completion(status, information, cvalue) {
                    IoAction::Submit => (self.on_submit)(io),
                    IoAction::Destroy => {
                        self.lock.lock();
                        let inner = unsafe { &mut *self.inner.get() };
                        inner.objcount -= 1;
                        self.lock.unlock();
                        unsafe { (*io).set_shutdown() };
                        tp_object_release(io);
                    }
                    IoAction::Ignore => {}
                }
            }
        }
    }

    #[cfg(test)]
    fn thread_running(&self) -> bool {
        self.lock.lock();
        let r = unsafe { &*self.inner.get() }.thread_running;
        self.lock.unlock();
        r
    }

    /// Signal shutdown, wake the thread, join it, and close the port (teardown).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.lock.lock();
        let port = unsafe { &*self.inner.get() }.port;
        self.lock.unlock();
        if port != 0 {
            self.os.post_completion(port, 0, 0, 0, 0);
        }
        self.os.join_all();
        if port != 0 {
            self.os.close_port(port);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threadpool::{dispatch_object, CallbackInstance, IoStatusBlock};
    extern crate std;
    use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize};
    use std::collections::VecDeque;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    /// Mock completion port: a FIFO of (key, cvalue, status, info) the test posts.
    struct MockPort {
        q: Mutex<VecDeque<(u64, usize, i32, usize)>>,
        cv: Condvar,
        next: AtomicU64,
    }
    impl MockPort {
        fn new() -> Self {
            MockPort { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), next: AtomicU64::new(1) }
        }
        fn post(&self, key: u64, cvalue: usize, status: i32, info: usize) {
            self.q.lock().unwrap().push_back((key, cvalue, status, info));
            self.cv.notify_all();
        }
        fn remove(&self, timeout_100ns: Option<i64>) -> Option<(u64, usize, i32, usize)> {
            let deadline = timeout_100ns.map(|t| Instant::now() + Duration::from_nanos(t as u64 * 100));
            let mut g = self.q.lock().unwrap();
            loop {
                if let Some(c) = g.pop_front() {
                    return Some(c);
                }
                g = match deadline {
                    None => self.cv.wait(g).unwrap(),
                    Some(d) => {
                        let left = d.checked_duration_since(Instant::now())?;
                        self.cv.wait_timeout(g, left).unwrap().0
                    }
                };
            }
        }
    }

    struct MockIoOs {
        port: Arc<MockPort>,
        handles: Mutex<Vec<JoinHandle<()>>>,
    }
    impl PoolOs for MockIoOs {
        fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool {
            let arg = arg as usize;
            self.handles.lock().unwrap().push(thread::spawn(move || {
                entry(arg as *mut c_void);
            }));
            true
        }
        fn futex_wait(&self, _a: &AtomicU32, _e: u32, _t: i64) -> bool {
            true
        }
        fn futex_wake_one(&self, _a: &AtomicU32) {}
        fn futex_wake_all(&self, _a: &AtomicU32) {}
        fn join_all(&self) {
            for h in self.handles.lock().unwrap().drain(..) {
                h.join().unwrap();
            }
        }
        fn now_mono_100ns(&self) -> i64 {
            0
        }
        fn now_system_100ns(&self) -> i64 {
            0
        }
    }
    impl IoOs for MockIoOs {
        fn create_port(&self) -> u64 {
            self.port.next.fetch_add(1, Ordering::Relaxed)
        }
        fn close_port(&self, _port: u64) {}
        fn associate(&self, _port: u64, _file: u64, _key: u64) {}
        fn remove_completion(&self, _port: u64, timeout_100ns: Option<i64>) -> Option<(u64, usize, i32, usize)> {
            self.port.remove(timeout_100ns)
        }
        fn post_completion(&self, _port: u64, key: u64, cvalue: usize, status: i32, info: usize) {
            self.port.post(key, cvalue, status, info);
        }
    }

    struct IoResult {
        fired: AtomicI32,
        status: AtomicI32,
        information: AtomicUsize,
        cvalue: AtomicUsize,
    }

    extern "system" fn io_cb(
        _inst: *mut CallbackInstance,
        userdata: *mut c_void,
        cvalue: *mut c_void,
        iosb: *mut IoStatusBlock,
        _io: *mut c_void,
    ) {
        let r = unsafe { &*(userdata as *const IoResult) };
        let sb = unsafe { &*iosb };
        r.status.store(sb.status, Ordering::Relaxed);
        r.information.store(sb.information, Ordering::Relaxed);
        r.cvalue.store(cvalue as usize, Ordering::Relaxed);
        r.fired.fetch_add(1, Ordering::Relaxed);
    }

    /// Model the pool's submit (refcount++) + the worker's dispatch (which
    /// releases), so the object's refcount stays balanced and the test keeps
    /// owning it.
    fn io_on_submit(obj: *mut ThreadpoolObject) {
        unsafe { (*obj).add_ref() };
        dispatch_object(obj);
    }

    fn spin_until(mut cond: impl FnMut() -> bool, what: &str) {
        let mut n: u64 = 0;
        while !cond() {
            n += 1;
            assert!(n < 2_000_000_000, "timed out waiting for {what}");
            core::hint::spin_loop();
        }
    }

    /// A posted completion flows port -> on_completion -> submit -> dispatch ->
    /// the I/O callback, carrying the right status / information / cvalue.
    #[test]
    fn io_completion_dispatches() {
        let port = Arc::new(MockPort::new());
        let res = Box::new(IoResult {
            fired: AtomicI32::new(0),
            status: AtomicI32::new(-1),
            information: AtomicUsize::new(0),
            cvalue: AtomicUsize::new(0),
        });
        let io = Box::new(ThreadpoolObject::new_io(
            io_cb,
            Box::as_ref(&res) as *const IoResult as *mut c_void,
            1,
        ));

        let os = MockIoOs { port: port.clone(), handles: Mutex::new(Vec::new()) };
        let q: Box<IoQueue<MockIoOs>> = Box::new(IoQueue::new(os, io_on_submit as fn(*mut ThreadpoolObject)));
        let q: &IoQueue<MockIoOs> = &q;

        unsafe { q.obj_added(io.as_ptr(), 0x1234 /* dummy file */) };
        io.io_state().start_async(); // an async op is now in flight

        // The kernel completes it: post (key=io, cvalue, status, information).
        port.post(io.as_ptr() as u64, 0xCAFE, 0x103 /* STATUS_PENDING-ish */, 4096);

        spin_until(|| res.fired.load(Ordering::Relaxed) == 1, "io callback");
        assert_eq!(res.status.load(Ordering::Relaxed), 0x103, "iosb.status");
        assert_eq!(res.information.load(Ordering::Relaxed), 4096, "iosb.information");
        assert_eq!(res.cvalue.load(Ordering::Relaxed), 0xCAFE, "cvalue");

        q.unlock();
        q.shutdown();
    }

    /// The service thread outlives the last I/O object by the idle timeout (C
    /// THREADPOOL_WORKER_TIMEOUT in ioqueue_thread_proc): an object created
    /// within it reuses the thread, and only a full idle period ends it.
    #[test]
    fn io_service_lingers_when_idle() {
        let res = IoResult {
            fired: AtomicI32::new(0),
            status: AtomicI32::new(-1),
            information: AtomicUsize::new(0),
            cvalue: AtomicUsize::new(0),
        };
        let io = Box::new(ThreadpoolObject::new_io(io_cb, &res as *const IoResult as *mut c_void, 1));
        let os = MockIoOs { port: Arc::new(MockPort::new()), handles: Mutex::new(Vec::new()) };
        let mut q: Box<IoQueue<MockIoOs>> = Box::new(IoQueue::new(os, io_on_submit as fn(*mut ThreadpoolObject)));
        q.set_idle_timeout(200 * 10_000); // 200 ms
        let q: &IoQueue<MockIoOs> = &q;
        let spawned = || q.os.handles.lock().unwrap().len();

        unsafe { q.obj_added(io.as_ptr(), 0x1234) };
        q.unlock();
        thread::sleep(Duration::from_millis(20));
        assert!(q.thread_running(), "service thread exited without the idle timeout");

        unsafe { q.obj_added(io.as_ptr(), 0x1234) };
        assert_eq!(spawned(), 1, "an object created within the timeout reuses the thread");
        thread::sleep(Duration::from_millis(300));
        assert!(q.thread_running(), "service thread exited with an object bound");

        q.unlock();
        spin_until(|| !q.thread_running(), "idle service thread exit");
        unsafe { q.obj_added(io.as_ptr(), 0x1234) };
        assert_eq!(spawned(), 2, "a new object restarts the service thread");
        q.unlock();
        q.shutdown();
    }

    /// The "expected completions" accounting (C bdba5ba5f65), exercised at the
    /// IoState level without freeing objects.
    #[test]
    fn io_state_expected_completions() {
        let s = IoState::new();

        // A pending op's completion is queued + submitted, and keeps the object busy
        // until dispatched (popped); a completion with nothing pending is dropped.
        s.start_async();
        assert!(s.is_busy());
        assert_eq!(s.handle_completion(0, 0, 0), IoAction::Submit);
        assert!(s.is_busy(), "a queued completion keeps it busy until dispatched");
        assert!(s.pop_completion().is_some());
        assert!(!s.is_busy());
        assert_eq!(s.handle_completion(0, 0, 0), IoAction::Ignore);

        // Start two ops, cancel them to skipped (TpWaitForIoCompletion(.,TRUE)).
        s.start_async();
        s.start_async();
        s.cancel_to_skipped();
        assert!(!s.is_busy());

        // begin_shutdown must report "cannot destroy yet" (2 skipped outstanding).
        assert!(!s.begin_shutdown());
        // Drain: first skipped completion is skipped, the last one destroys.
        assert_eq!(s.handle_completion(0, 0, 0), IoAction::Ignore);
        assert_eq!(s.handle_completion(0, 0, 0), IoAction::Destroy);
    }

    /// try_new reserves the C's initial completion array and behaves like new.
    #[test]
    fn io_state_try_new_reserves_completions() {
        let s = IoState::try_new().expect("allocation");
        assert!(unsafe { &*s.inner.get() }.completions.capacity() >= INITIAL_COMPLETIONS);
        assert!(!s.is_busy());
        s.start_async();
        assert_eq!(s.handle_completion(0, 0, 0), IoAction::Submit);
        assert!(s.pop_completion().is_some());
    }

    /// With nothing outstanding, begin_shutdown authorises immediate destruction.
    #[test]
    fn io_state_immediate_destroy() {
        let s = IoState::new();
        assert!(s.begin_shutdown());
    }
}
