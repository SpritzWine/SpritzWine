//! Wait queue (stage 3): Rust replacement for the global `waitqueue` +
//! waitqueue_thread_proc + tp_waitqueue_lock/unlock + TpSetWait's scheduling.
//!
//! Wait objects are grouped into buckets of at most [`MAX_WAIT_OBJECTS`] handles;
//! each bucket has a service thread that `wait_multiple`s on its armed handles
//! plus a per-bucket `update_event`, and on a signal/timeout fires the object via
//! `on_signal` (the wiring points this at Pool::submit). A bucket's thread lives
//! while the bucket has objects and exits on an idle grace timeout.
//!
//! Unlike the pool/timer futex, the wakeup here is a real auto-reset *event*
//! (NtSetEvent stays signaled until a waiter consumes it), so a set_wait can't
//! lose a wake -- no terminate re-check needed (the LossyOs hazard does not apply).
//!
//! ## Scope
//! Faithful core for the Vista `TP_WAIT` (one-shot: a wait fires once per
//! TpSetWait, then disarms). Deferred: bucket merging (an optimization), the
//! legacy RtlRegisterWait repeating / WT_EXECUTEIN{WAIT,IO}THREAD in-thread
//! execution flags (those belong with the RtlRegisterWait wrapper).

use crate::threadpool::{PoolOs, ThreadpoolObject};
use crate::work_queue::SpinLock;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::ptr;

/// MAXIMUM_WAITQUEUE_OBJECTS (one handle slot is reserved for the update_event).
pub const MAX_WAIT_OBJECTS: usize = 63;
/// Sentinel "no timeout" deadline.
const NO_DEADLINE: i64 = i64::MAX;
/// Idle grace before an empty bucket's thread exits (THREADPOOL_WORKER_TIMEOUT).
const WAIT_GRACE_100NS: i64 = 5000 * 10_000;

/// OS event + multi-wait primitives (wine: NtCreateEvent / NtSetEvent /
/// NtWaitForMultipleObjects). Handles are opaque `u64` (a wine HANDLE).
pub trait WaitOs: PoolOs {
    /// Create an auto-reset (synchronization) event.
    fn create_event(&self) -> u64;
    fn set_event(&self, event: u64);
    /// Close a handle (NtClose). Used for both bucket events and duped wait handles.
    fn close_event(&self, event: u64);
    /// Duplicate a caller-supplied wait handle into our process (NtDuplicateObject),
    /// so the service thread waits on a handle we own and the app may close its copy
    /// immediately after TpSetWait. Returns 0 on failure (fall back to the raw handle).
    fn duplicate_handle(&self, handle: u64) -> u64;
    /// Wait until one of `handles` signals or `timeout_100ns` elapses. Returns the
    /// index of the signaled handle, or `None` on timeout.
    fn wait_multiple(&self, handles: &[u64], timeout_100ns: i64) -> Option<usize>;
}

/// One wait object's scheduling state (the C `u.wait`), owned by its bucket.
struct WaitEntry {
    obj: *mut ThreadpoolObject,
    handle: u64,        // 0 = reserved (no handle armed); the caller's raw handle
    duped_handle: u64,  // our private duplicate of `handle` (0 = none); waited on
    deadline: i64, // absolute system-time 100ns; NO_DEADLINE = infinite
    serial: u32,   // bumped on handle change, to drop a stale signal
    /// Repeating (legacy RtlRegisterWait without WT_EXECUTEONLYONCE): re-arm after
    /// each fire instead of disarming. Tp* waits and ONLYONCE are one-shot.
    repeating: bool,
    /// Re-arm timeout (relative 100ns; NO_DEADLINE = no timeout) for a repeating wait.
    rearm_rel: i64,
}

struct Bucket<Os: WaitOs> {
    objcount: i32,
    entries: Vec<WaitEntry>,
    update_event: u64,
    alertable: bool,
    shutdown: bool,
    wq: *const WaitQueue<Os>,
}

struct WaitInner<Os: WaitOs> {
    buckets: Vec<*mut Bucket<Os>>,
}

/// The global wait service.
pub struct WaitQueue<Os: WaitOs> {
    lock: SpinLock,
    inner: UnsafeCell<WaitInner<Os>>,
    on_signal: fn(*mut ThreadpoolObject, bool), // (obj, signaled?)
    os: Os,
}

unsafe impl<Os: WaitOs> Sync for WaitQueue<Os> {}
unsafe impl<Os: WaitOs> Send for WaitQueue<Os> {}

extern "system" fn bucket_entry<Os: WaitOs>(arg: *mut c_void) -> u32 {
    let bucket = arg as *mut Bucket<Os>;
    // SAFETY: the bucket + its wq outlive the thread (joined at shutdown).
    let wq = unsafe { &*(*bucket).wq };
    wq.run_bucket(bucket);
    0
}

impl<Os: WaitOs> WaitQueue<Os> {
    pub fn new(os: Os, on_signal: fn(*mut ThreadpoolObject, bool)) -> Self {
        WaitQueue {
            lock: SpinLock::new(),
            inner: UnsafeCell::new(WaitInner { buckets: Vec::new() }),
            on_signal,
            os,
        }
    }

    /// Assign `obj` to a bucket (C `tp_waitqueue_lock`), creating a bucket + its
    /// service thread when none has room.
    ///
    /// # Safety
    /// `obj` must be a live wait [`ThreadpoolObject`] until [`obj_removed`].
    pub unsafe fn obj_added(&self, obj: *mut ThreadpoolObject, alertable: bool) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        let mut target: *mut Bucket<Os> = ptr::null_mut();
        for &b in inner.buckets.iter() {
            let bk = unsafe { &*b };
            if !bk.shutdown && bk.alertable == alertable && bk.objcount < MAX_WAIT_OBJECTS as i32 {
                target = b;
                break;
            }
        }
        if target.is_null() {
            let ev = self.os.create_event();
            let bucket = Box::into_raw(Box::new(Bucket {
                objcount: 0,
                entries: Vec::new(),
                update_event: ev,
                alertable,
                shutdown: false,
                wq: self as *const _,
            }));
            inner.buckets.push(bucket);
            self.os.spawn(bucket_entry::<Os>, bucket as *mut c_void);
            target = bucket;
        }
        let bk = unsafe { &mut *target };
        bk.entries.push(WaitEntry {
            obj,
            handle: 0,
            duped_handle: 0,
            deadline: NO_DEADLINE,
            serial: 0,
            repeating: false,
            rearm_rel: NO_DEADLINE,
        });
        bk.objcount += 1;
        self.lock.unlock();
    }

    /// Locate `obj`'s (bucket, entry index). Caller holds `lock`.
    fn find(&self, obj: *mut ThreadpoolObject) -> Option<(*mut Bucket<Os>, usize)> {
        let inner = unsafe { &*self.inner.get() };
        for &b in inner.buckets.iter() {
            let bk = unsafe { &*b };
            if let Some(i) = bk.entries.iter().position(|e| e.obj == obj) {
                return Some((b, i));
            }
        }
        None
    }

    /// Arm / re-arm / disarm a wait (C `TpSetWait`). `handle == 0` disarms;
    /// `timeout`: `None` = no timeout, `Some(v<0)` = relative, `Some(v>=0)` =
    /// absolute (v==0 fires on the next scan). `repeating` = re-arm after each fire
    /// (legacy RtlRegisterWait without WT_EXECUTEONLYONCE); Tp* waits pass false.
    ///
    /// # Safety
    /// `obj` must be bound via [`obj_added`].
    pub unsafe fn set_wait(
        &self,
        obj: *mut ThreadpoolObject,
        handle: u64,
        timeout: Option<i64>,
        repeating: bool,
    ) {
        self.lock.lock();
        let Some((b, idx)) = self.find(obj) else {
            self.lock.unlock();
            return;
        };
        let bk = unsafe { &mut *b };
        let e = &mut bk.entries[idx];
        let same = e.handle == handle;
        // Re-duplicate: drop the previous private copy, then duplicate the new handle
        // so the service thread waits on a handle we own. The app is allowed to close
        // `handle` right after this call (esp. waitable timers). Matches upstream
        // acf72ed171a "Duplicate handles for thread pool waits".
        if e.duped_handle != 0 {
            self.os.close_event(e.duped_handle);
            e.duped_handle = 0;
        }
        if handle != 0 {
            e.duped_handle = self.os.duplicate_handle(handle); // 0 -> fall back to raw
        }
        e.handle = handle;
        e.deadline = if handle != 0 {
            match timeout {
                None => NO_DEADLINE,
                Some(v) if v < 0 => self.os.now_system_100ns().wrapping_sub(v),
                Some(v) => v,
            }
        } else {
            NO_DEADLINE
        };
        // Relative re-arm interval for a repeating wait (no timeout stays infinite).
        e.repeating = repeating;
        e.rearm_rel = match timeout {
            Some(v) if v < 0 => -v,
            _ => NO_DEADLINE,
        };
        if !same {
            e.serial = e.serial.wrapping_add(1);
        }
        let ev = bk.update_event;
        self.lock.unlock();
        self.os.set_event(ev);
    }

    /// Unbind a wait (C `tp_waitqueue_unlock`).
    ///
    /// # Safety
    /// `obj` must have been bound via [`obj_added`].
    pub unsafe fn obj_removed(&self, obj: *mut ThreadpoolObject) {
        self.lock.lock();
        if let Some((b, idx)) = self.find(obj) {
            let bk = unsafe { &mut *b };
            let dup = bk.entries[idx].duped_handle;
            if dup != 0 {
                self.os.close_event(dup);
            }
            bk.entries.swap_remove(idx);
            bk.objcount -= 1;
            let ev = bk.update_event;
            self.lock.unlock();
            self.os.set_event(ev);
            return;
        }
        self.lock.unlock();
    }

    /// A bucket's service thread (the C `waitqueue_thread_proc`).
    fn run_bucket(&self, bucket: *mut Bucket<Os>) {
        loop {
            self.lock.lock();
            let bk = unsafe { &mut *bucket };
            if bk.shutdown {
                let inner = unsafe { &mut *self.inner.get() };
                if let Some(p) = inner.buckets.iter().position(|&x| x == bucket) {
                    inner.buckets.swap_remove(p);
                }
                let ev = bk.update_event;
                self.lock.unlock();
                // Close any wait handles still duplicated (normally none: obj_removed
                // frees them; this covers a shutdown with objects still bound).
                for e in bk.entries.iter() {
                    if e.duped_handle != 0 {
                        self.os.close_event(e.duped_handle);
                    }
                }
                self.os.close_event(ev);
                drop(unsafe { Box::from_raw(bucket) });
                return;
            }

            let now = self.os.now_system_100ns();
            let mut handles: Vec<u64> = Vec::new();
            let mut metas: Vec<(*mut ThreadpoolObject, u32)> = Vec::new();
            let mut to_fire: Vec<*mut ThreadpoolObject> = Vec::new();
            let mut min_deadline = NO_DEADLINE;
            for e in bk.entries.iter_mut() {
                if e.handle == 0 {
                    continue;
                }
                if e.deadline <= now {
                    to_fire.push(e.obj); // timed out
                    if e.repeating {
                        // Re-arm with a fresh timeout (handle stays armed).
                        e.deadline = if e.rearm_rel == NO_DEADLINE { NO_DEADLINE } else { now + e.rearm_rel };
                    } else {
                        e.handle = 0;
                        e.deadline = NO_DEADLINE; // one-shot disarm
                    }
                } else {
                    // Wait on our private duplicate so a closed caller handle can't
                    // fail the whole NtWaitForMultipleObjects (fall back to raw if the
                    // duplicate failed).
                    handles.push(if e.duped_handle != 0 { e.duped_handle } else { e.handle });
                    metas.push((e.obj, e.serial));
                    if e.deadline < min_deadline {
                        min_deadline = e.deadline;
                    }
                }
            }
            let objcount = bk.objcount;
            let update_event = bk.update_event;
            self.lock.unlock();

            for obj in to_fire {
                (self.on_signal)(obj, false);
            }

            let waiting = !handles.is_empty();
            handles.push(update_event); // wakeup is the last handle
            let timeout = if min_deadline == NO_DEADLINE {
                WAIT_GRACE_100NS
            } else {
                (min_deadline - now).max(0)
            };
            let update_idx = handles.len() - 1;
            let outcome = self.os.wait_multiple(&handles, timeout);

            match outcome {
                Some(i) if i < update_idx => {
                    let (obj, serial) = metas[i];
                    self.lock.lock();
                    let bk2 = unsafe { &mut *bucket };
                    let mut fire = false;
                    if let Some(idx) = bk2
                        .entries
                        .iter()
                        .position(|e| e.obj == obj && e.serial == serial && e.handle != 0)
                    {
                        let e = &mut bk2.entries[idx];
                        if e.repeating {
                            // Re-arm: keep the handle, reset the relative timeout.
                            let now = self.os.now_system_100ns();
                            e.deadline = if e.rearm_rel == NO_DEADLINE { NO_DEADLINE } else { now + e.rearm_rel };
                        } else {
                            e.handle = 0;
                            e.deadline = NO_DEADLINE; // one-shot disarm
                        }
                        fire = true;
                    }
                    self.lock.unlock();
                    if fire {
                        (self.on_signal)(obj, true); // signaled
                    }
                }
                None => {
                    // Idle grace timed out: if the bucket has no objects, exit.
                    if !waiting && objcount == 0 {
                        self.lock.lock();
                        let bk2 = unsafe { &mut *bucket };
                        if bk2.objcount == 0 {
                            bk2.shutdown = true; // teardown at the next loop top
                        }
                        self.lock.unlock();
                    }
                }
                _ => { /* update_event signaled -> re-scan */ }
            }
        }
    }

    /// Signal shutdown on every bucket, wake them, and join (teardown).
    pub fn shutdown(&self) {
        self.lock.lock();
        let inner = unsafe { &*self.inner.get() };
        let evs: Vec<u64> = inner
            .buckets
            .iter()
            .map(|&b| {
                let bk = unsafe { &mut *b };
                bk.shutdown = true;
                bk.update_event
            })
            .collect();
        self.lock.unlock();
        for ev in evs {
            self.os.set_event(ev);
        }
        self.os.join_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threadpool::{dispatch_object, CallbackInstance, WAIT_OBJECT_0, WAIT_TIMEOUT};
    extern crate std;
    use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
    use std::collections::HashMap;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// One shared kernel object: an auto-reset signaled bit + a handle refcount, so
    /// duplicating a handle keeps the object alive after the source handle is closed
    /// (models NtDuplicateObject / DUPLICATE_SAME_ACCESS).
    struct MockObj {
        signaled: bool,
        refs: u32,
    }
    struct MockState {
        objects: HashMap<u64, MockObj>, // object id -> state
        handles: HashMap<u64, u64>,     // handle id -> object id
    }
    /// Shared mock registry: objects reachable through one or more handles + a
    /// multi-wait, driven by the test (it creates, duplicates, signals + closes).
    struct MockReg {
        st: Mutex<MockState>,
        cv: Condvar,
        next: AtomicU64,
    }
    impl MockReg {
        fn new() -> Self {
            MockReg {
                st: Mutex::new(MockState { objects: HashMap::new(), handles: HashMap::new() }),
                cv: Condvar::new(),
                next: AtomicU64::new(1),
            }
        }
        fn create_event(&self) -> u64 {
            let obj = self.next.fetch_add(1, Ordering::Relaxed);
            let h = self.next.fetch_add(1, Ordering::Relaxed);
            let mut st = self.st.lock().unwrap();
            st.objects.insert(obj, MockObj { signaled: false, refs: 1 });
            st.handles.insert(h, obj);
            h
        }
        /// New handle to the same object (refs++). 0 if the source is unknown.
        fn duplicate_handle(&self, h: u64) -> u64 {
            let mut st = self.st.lock().unwrap();
            let Some(&obj) = st.handles.get(&h) else { return 0 };
            st.objects.get_mut(&obj).unwrap().refs += 1;
            let d = self.next.fetch_add(1, Ordering::Relaxed);
            st.handles.insert(d, obj);
            d
        }
        fn set_event(&self, h: u64) {
            let mut st = self.st.lock().unwrap();
            if let Some(&obj) = st.handles.get(&h) {
                st.objects.get_mut(&obj).unwrap().signaled = true;
                self.cv.notify_all();
            }
        }
        fn close_event(&self, h: u64) {
            let mut st = self.st.lock().unwrap();
            if let Some(obj) = st.handles.remove(&h) {
                let o = st.objects.get_mut(&obj).unwrap();
                o.refs -= 1;
                if o.refs == 0 {
                    st.objects.remove(&obj);
                }
            }
        }
        fn wait_multiple(&self, handles: &[u64], timeout_100ns: i64) -> Option<usize> {
            let mut g = self.st.lock().unwrap();
            let deadline = std::time::Instant::now()
                + Duration::from_nanos(timeout_100ns.max(0) as u64 * 100);
            loop {
                for (i, h) in handles.iter().enumerate() {
                    if let Some(&obj) = g.handles.get(h) {
                        let o = g.objects.get_mut(&obj).unwrap();
                        if o.signaled {
                            o.signaled = false; // auto-reset
                            return Some(i);
                        }
                    }
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return None;
                }
                let (ng, res) = self.cv.wait_timeout(g, deadline - now).unwrap();
                g = ng;
                if res.timed_out() {
                    // loop once more to re-check, then bail
                }
            }
        }
    }

    struct MockWaitOs {
        reg: Arc<MockReg>,
        handles: Mutex<Vec<JoinHandle<()>>>,
    }
    impl PoolOs for MockWaitOs {
        fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool {
            let arg = arg as usize;
            self.handles.lock().unwrap().push(thread::spawn(move || {
                entry(arg as *mut c_void);
            }));
            true
        }
        // Unused by WaitQueue (it uses events, not the futex / mono clock).
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
            (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() / 100) as i64
        }
    }
    impl WaitOs for MockWaitOs {
        fn create_event(&self) -> u64 {
            self.reg.create_event()
        }
        fn set_event(&self, e: u64) {
            self.reg.set_event(e)
        }
        fn close_event(&self, e: u64) {
            self.reg.close_event(e)
        }
        fn duplicate_handle(&self, h: u64) -> u64 {
            self.reg.duplicate_handle(h)
        }
        fn wait_multiple(&self, handles: &[u64], t: i64) -> Option<usize> {
            self.reg.wait_multiple(handles, t)
        }
    }

    struct WaitState {
        fired: AtomicI32,
        result: AtomicU32,
    }

    extern "system" fn wait_cb(
        _inst: *mut CallbackInstance,
        userdata: *mut c_void,
        _wait: *mut c_void,
        result: u32,
    ) {
        let s = unsafe { &*(userdata as *const WaitState) };
        s.result.store(result, Ordering::Relaxed);
        s.fired.fetch_add(1, Ordering::Relaxed);
    }

    /// on_signal seam: account a signaled callback then dispatch (so the Wait
    /// dispatch resolves WAIT_OBJECT_0 vs WAIT_TIMEOUT).
    fn wait_on_signal(obj: *mut ThreadpoolObject, signaled: bool) {
        if signaled {
            unsafe { (*obj).signaled.fetch_add(1, Ordering::AcqRel) };
        }
        // Model the pool submit (refcount++) so dispatch's release stays balanced.
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

    /// A signaled handle fires WAIT_OBJECT_0; an armed wait with a relative
    /// timeout and no signal fires WAIT_TIMEOUT.
    #[test]
    fn wait_signal_and_timeout() {
        let reg = Arc::new(MockReg::new());
        let s_sig = Box::new(WaitState { fired: AtomicI32::new(0), result: AtomicU32::new(123) });
        let s_to = Box::new(WaitState { fired: AtomicI32::new(0), result: AtomicU32::new(123) });
        let o_sig = Box::new(ThreadpoolObject::new_wait(
            wait_cb,
            Box::as_ref(&s_sig) as *const WaitState as *mut c_void,
            1,
        ));
        let o_to = Box::new(ThreadpoolObject::new_wait(
            wait_cb,
            Box::as_ref(&s_to) as *const WaitState as *mut c_void,
            1,
        ));

        let os = MockWaitOs { reg: reg.clone(), handles: Mutex::new(Vec::new()) };
        let wq: Box<WaitQueue<MockWaitOs>> = Box::new(WaitQueue::new(os, wait_on_signal));
        let wq: &WaitQueue<MockWaitOs> = &wq;

        unsafe {
            wq.obj_added(o_sig.as_ptr(), false);
            wq.obj_added(o_to.as_ptr(), false);
        }

        // Arm the signaled wait (no timeout), then signal it.
        let h = reg.create_event();
        unsafe { wq.set_wait(o_sig.as_ptr(), h, None, false) };
        // Arm the timeout wait for ~20 ms, never signaled.
        unsafe { wq.set_wait(o_to.as_ptr(), reg.create_event(), Some(-(20 * 10_000)), false) };

        reg.set_event(h);
        spin_until(|| s_sig.fired.load(Ordering::Relaxed) == 1, "signaled wait");
        assert_eq!(s_sig.result.load(Ordering::Relaxed), WAIT_OBJECT_0, "should be WAIT_OBJECT_0");

        spin_until(|| s_to.fired.load(Ordering::Relaxed) == 1, "timeout wait");
        assert_eq!(s_to.result.load(Ordering::Relaxed), WAIT_TIMEOUT, "should be WAIT_TIMEOUT");

        unsafe {
            wq.obj_removed(o_sig.as_ptr());
            wq.obj_removed(o_to.as_ptr());
        }
        wq.shutdown();
    }

    /// A repeating wait (RtlRegisterWait without WT_EXECUTEONLYONCE) re-fires on
    /// every signal; a one-shot fires only once.
    #[test]
    fn wait_repeating_refires() {
        let reg = Arc::new(MockReg::new());
        let s = Box::new(WaitState { fired: AtomicI32::new(0), result: AtomicU32::new(123) });
        let o = Box::new(ThreadpoolObject::new_wait(
            wait_cb,
            Box::as_ref(&s) as *const WaitState as *mut c_void,
            1,
        ));
        let os = MockWaitOs { reg: reg.clone(), handles: Mutex::new(Vec::new()) };
        let wq: Box<WaitQueue<MockWaitOs>> = Box::new(WaitQueue::new(os, wait_on_signal));
        let wq: &WaitQueue<MockWaitOs> = &wq;

        unsafe { wq.obj_added(o.as_ptr(), false) };
        let h = reg.create_event();
        unsafe { wq.set_wait(o.as_ptr(), h, None, true) }; // repeating, no timeout

        reg.set_event(h);
        spin_until(|| s.fired.load(Ordering::Relaxed) >= 1, "first fire");
        reg.set_event(h);
        spin_until(|| s.fired.load(Ordering::Relaxed) >= 2, "second fire (re-armed)");
        assert_eq!(s.result.load(Ordering::Relaxed), WAIT_OBJECT_0);

        unsafe { wq.obj_removed(o.as_ptr()) };
        wq.shutdown();
    }

    /// The caller may close its wait handle immediately after TpSetWait: the pool
    /// duplicates it, so the wait stays live and still fires when the (shared) object
    /// is signaled. Without the duplicate the service thread would wait on a closed
    /// handle and never fire (regression guard for the upstream duped_handle fix).
    #[test]
    fn wait_survives_caller_closing_handle() {
        let reg = Arc::new(MockReg::new());
        let s = Box::new(WaitState { fired: AtomicI32::new(0), result: AtomicU32::new(123) });
        let o = Box::new(ThreadpoolObject::new_wait(
            wait_cb,
            Box::as_ref(&s) as *const WaitState as *mut c_void,
            1,
        ));
        let os = MockWaitOs { reg: reg.clone(), handles: Mutex::new(Vec::new()) };
        let wq: Box<WaitQueue<MockWaitOs>> = Box::new(WaitQueue::new(os, wait_on_signal));
        let wq: &WaitQueue<MockWaitOs> = &wq;

        unsafe { wq.obj_added(o.as_ptr(), false) };
        let h = reg.create_event(); // caller's handle to object O
        // A second handle we keep so we can still signal O after the caller closes h.
        let signaler = reg.duplicate_handle(h);
        unsafe { wq.set_wait(o.as_ptr(), h, None, false) }; // pool duplicates h internally
        reg.close_event(h); // caller drops its handle immediately -- O stays alive

        reg.set_event(signaler);
        spin_until(|| s.fired.load(Ordering::Relaxed) == 1, "wait fired via duplicate");
        assert_eq!(s.result.load(Ordering::Relaxed), WAIT_OBJECT_0);

        unsafe { wq.obj_removed(o.as_ptr()) };
        reg.close_event(signaler);
        wq.shutdown();
    }
}
