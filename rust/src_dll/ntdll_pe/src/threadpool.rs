//! Core object scheduler for the NT threadpool rewrite (stage 1a): the
//! priority-ordered, lock-free submission/dispatch path that today lives under
//! `pool->cs` in dlls/ntdll/threadpool.c (`tp_object_submit` /
//! `tp_object_prio_queue` / `threadpool_get_next_item` / the worker re-queue
//! loop in `threadpool_worker_proc`).
//!
//! ## Why a plain queue swap is not enough
//! In the C, one `pool->cs` guards BOTH the three priority lists AND
//! `num_busy_workers` / worker spawn / the wakeup condition variable, so
//! replacing only the list structure removes no contention. Here the per-object
//! pending count is an atomic and the item hand-off uses the two-lock
//! [`TwoLockQueue`], so `submit` and a worker no longer share a single lock for
//! scheduling.
//!
//! ## Concurrency protocol (validated by the stress test)
//! An object is enqueued on the 0->1 pending transition (`submit` sees
//! `fetch_add` return 0) and RE-enqueued by a worker iff, right after it claims a
//! callback, others remain (`fetch_sub` returns prev > 1) -- decided after
//! dequeue and before running the callback. Deciding via `fetch_sub`'s returned
//! previous value (rather than a separate load + decrement) is what keeps it
//! race-free *and* preserves Windows' "N posts -> up to N concurrent callbacks"
//! semantics. The cross-priority scan is not a single atomic snapshot, a minor
//! (and acceptable) relaxation of strict priority ordering under concurrency.
//!
//! Slice 1a is pure scheduling and fully unit-testable; worker spawn/terminate
//! and the callback dispatch (which need FFI) are slices 1b/1c.

use crate::work_queue::{Node, TwoLockQueue};
use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

/// Number of `TP_CALLBACK_PRIORITY` levels (high, normal, low) -- matches the C
/// `pool->pools[3]` ordering.
pub const PRIORITY_COUNT: usize = 3;

/// Per-object scheduling state the hot path touches: the Rust-native replacement
/// for the C object's `num_pending_callbacks` + `priority` + `pool_entry`. Embed
/// one in each threadpool object.
#[repr(C)]
pub struct SchedState {
    /// Outstanding callbacks owed for this object (the submit/TpPostWork count).
    num_pending: AtomicI32,
    /// Priority level: 0 = high, 1 = normal, 2 = low.
    priority: u8,
}

impl SchedState {
    pub const fn new(priority: u8) -> Self {
        SchedState { num_pending: AtomicI32::new(0), priority }
    }

    /// Current outstanding callback count (debug/teardown checks).
    #[inline]
    pub fn pending(&self) -> i32 {
        self.num_pending.load(Ordering::Acquire)
    }
}

/// Objects schedulable by [`Scheduler`] expose their [`SchedState`].
///
/// # Safety
/// `sched_state` must return a reference valid for as long as the object can be
/// in the scheduler, and any pointer handed to [`Scheduler::submit`] must stay
/// live until its last claimed callback completes (guaranteed by the object's
/// refcount, exactly as in the C).
pub unsafe trait Scheduled {
    fn sched_state(&self) -> &SchedState;
}

/// Priority-ordered object scheduler over three [`TwoLockQueue`]s.
pub struct Scheduler {
    queues: [TwoLockQueue; PRIORITY_COUNT],
    /// "Work in the system" = queued entries + in-flight callbacks. The
    /// Rust-native replacement for the C `pool->num_busy_workers`: incremented on
    /// every enqueue (as `tp_object_prio_queue` does) and decremented once per
    /// completed callback ([`callback_done`](Self::callback_done)). Drives the
    /// pool's worker-spawn decision.
    busy: AtomicI32,
}

// Shared across the submit side and every worker.
unsafe impl Sync for Scheduler {}
unsafe impl Send for Scheduler {}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            queues: [
                unsafe { TwoLockQueue::with_dummy(new_carrier()) },
                unsafe { TwoLockQueue::with_dummy(new_carrier()) },
                unsafe { TwoLockQueue::with_dummy(new_carrier()) },
            ],
            busy: AtomicI32::new(0),
        }
    }

    /// Current "busy" count (queued + in-flight callbacks).
    #[inline]
    pub fn busy(&self) -> i32 {
        self.busy.load(Ordering::Acquire)
    }

    /// Account for one completed callback (the pool calls this after each
    /// dispatch, mirroring the C `pool->num_busy_workers--`).
    #[inline]
    pub fn callback_done(&self) {
        self.busy.fetch_sub(1, Ordering::AcqRel);
    }

    /// Best-effort: are all priority queues empty? Used by a worker's terminate
    /// guard (the Rust `!threadpool_get_next_item`); see [`TwoLockQueue::is_empty`].
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.queues.iter().all(TwoLockQueue::is_empty)
    }

    /// Append `obj` to its priority queue on a fresh carrier node.
    ///
    /// # Safety
    /// `obj` must satisfy the [`Scheduled`] contract.
    unsafe fn enqueue<T: Scheduled>(&self, obj: *mut T) {
        let prio = unsafe { (*obj).sched_state().priority } as usize;
        debug_assert!(prio < PRIORITY_COUNT);
        self.busy.fetch_add(1, Ordering::AcqRel);
        unsafe { self.queues[prio].enqueue(new_carrier(), obj.cast()) };
    }

    /// Submit one callback for `obj`. Enqueues the object on the 0->1 pending
    /// transition; otherwise it is already (or about to be) scheduled. Returns
    /// `true` on that newly-enqueued edge so the caller can mirror the C's
    /// busy-worker bump / worker-spawn decision (slice 1b).
    ///
    /// # Safety
    /// `obj` must satisfy the [`Scheduled`] contract.
    pub unsafe fn submit<T: Scheduled>(&self, obj: *mut T) -> bool {
        let prev = unsafe { (*obj).sched_state() }
            .num_pending
            .fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            unsafe { self.enqueue(obj) };
            true
        } else {
            false
        }
    }

    /// Claim the next highest-priority object to run. Accounts for one of its
    /// pending callbacks and re-queues it (before the caller runs the callback)
    /// if more remain, so a sibling callback can run concurrently. Returns the
    /// object to invoke, or `None` if every queue is empty.
    ///
    /// # Safety
    /// The returned pointer is valid until the caller finishes the callback
    /// (the object's refcount keeps it alive, as in the C).
    pub unsafe fn worker_take<T: Scheduled>(&self) -> Option<*mut T> {
        for q in &self.queues {
            if let Some((val, recycled)) = q.dequeue() {
                free_carrier(recycled);
                let obj = val.cast::<T>();
                let prev = unsafe { (*obj).sched_state() }
                    .num_pending
                    .fetch_sub(1, Ordering::AcqRel);
                if prev > 1 {
                    unsafe { self.enqueue(obj) };
                }
                return Some(obj);
            }
        }
        None
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // Reclaim each queue's trailing dummy carrier. Production pools live for
        // the process; this keeps the type leak-free for tests/teardown. Callers
        // must have drained all objects first (an object outliving its scheduler
        // is a refcount bug, not handled here).
        for q in &self.queues {
            let dummy = unsafe { q.drain(|_| {}) };
            free_carrier(dummy);
        }
    }
}

/// Allocate a fresh queue carrier node (recycled-free-list is a later optz; with
/// the process heap allocator a Box is just an `RtlAllocateHeap`).
#[inline]
fn new_carrier() -> *mut Node {
    Box::into_raw(Box::new(Node::new()))
}

#[inline]
fn free_carrier(n: *mut Node) {
    drop(unsafe { Box::from_raw(n) });
}

/// Default surplus-worker idle timeout before termination, in 100ns units
/// (THREADPOOL_WORKER_TIMEOUT = 5000 ms in the C).
pub const WORKER_TIMEOUT_100NS: i64 = 5000 * 10_000;

/// OS primitives the pool needs, abstracted so the worker lifecycle is unit-
/// testable on the host (std threads + a condvar-backed futex) while the real
/// build uses ntdll's own `RtlCreateUserThread` / `RtlWaitOnAddress` (wired in
/// slice 1c). The futex contract matches `RtlWaitOnAddress`: a waiter that reads
/// the generation, then waits comparing against it, never misses a wake that
/// bumped the generation first.
pub trait PoolOs: Sync {
    /// Spawn a worker thread that runs `entry(arg)`. Returns false on failure.
    fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool;
    /// Block while `*addr == expected`, up to `timeout_100ns` (relative). Returns
    /// true if woken, false if it timed out.
    fn futex_wait(&self, addr: &AtomicU32, expected: u32, timeout_100ns: i64) -> bool;
    /// Wake one / all waiters on `addr`.
    fn futex_wake_one(&self, addr: &AtomicU32);
    fn futex_wake_all(&self, addr: &AtomicU32);
    /// Join all spawned workers (teardown; no-op in the real build, which exits
    /// workers via the thread machinery).
    fn join_all(&self) {}
}

/// The NT threadpool's worker-management half (stage 1b): atomic worker counts +
/// futex wakeup over the lock-free [`Scheduler`]. The single fat `pool->cs` of
/// the C is gone -- item hand-off and the pending/busy counts are lock-free, and
/// only the (tiny, racy-tolerant) spawn/terminate policy reads the worker atomics.
///
/// Generic over [`PoolOs`] and the object type `T`; `run` is the per-callback
/// dispatch seam that slice 1c fills with `tp_object_execute`.
pub struct Pool<T: Scheduled, Os: PoolOs> {
    scheduler: Scheduler,
    num_workers: AtomicI32,
    min_workers: AtomicI32,
    max_workers: AtomicI32,
    /// Objects bound to the pool; keeps the last worker alive while > 0 (C `objcount`).
    objcount: AtomicI32,
    shutdown: AtomicBool,
    /// Futex generation; bumped before each submit-wake so a worker between
    /// reading it and sleeping cannot miss the wake.
    update: AtomicU32,
    worker_timeout_100ns: i64,
    run: fn(*mut T),
    os: Os,
}

unsafe impl<T: Scheduled, Os: PoolOs> Sync for Pool<T, Os> {}
unsafe impl<T: Scheduled, Os: PoolOs> Send for Pool<T, Os> {}

/// Worker thread entry: OS-agnostic, instantiated per `<T, Os>`. Returning exits
/// the thread (the real build's thread machinery calls `RtlExitUserThread` on
/// return; std joins it).
extern "system" fn worker_entry<T: Scheduled, Os: PoolOs>(arg: *mut c_void) -> u32 {
    // SAFETY: `arg` is the `&Pool` passed to `spawn`, alive for the worker's life
    // (refcount in the real build; the test joins before drop).
    let pool = unsafe { &*(arg as *const Pool<T, Os>) };
    pool.run_worker();
    0
}

impl<T: Scheduled, Os: PoolOs> Pool<T, Os> {
    pub fn new(os: Os, run: fn(*mut T)) -> Self {
        Pool {
            scheduler: Scheduler::new(),
            num_workers: AtomicI32::new(0),
            min_workers: AtomicI32::new(0),
            max_workers: AtomicI32::new(500),
            objcount: AtomicI32::new(0),
            shutdown: AtomicBool::new(false),
            update: AtomicU32::new(0),
            worker_timeout_100ns: WORKER_TIMEOUT_100NS,
            run,
            os,
        }
    }

    pub fn num_workers(&self) -> i32 {
        self.num_workers.load(Ordering::Acquire)
    }
    pub fn set_min_workers(&self, n: i32) {
        self.min_workers.store(n, Ordering::Release);
    }
    pub fn set_max_workers(&self, n: i32) {
        self.max_workers.store(n, Ordering::Release);
    }
    pub fn set_worker_timeout(&mut self, t100ns: i64) {
        self.worker_timeout_100ns = t100ns;
    }
    /// Bind / unbind an object to the pool (C `objcount++/--`, via tp_threadpool_lock/unlock).
    pub fn obj_added(&self) {
        self.objcount.fetch_add(1, Ordering::AcqRel);
    }
    pub fn obj_removed(&self) {
        self.objcount.fetch_sub(1, Ordering::AcqRel);
    }

    /// Ensure at least one worker exists (the C `tp_threadpool_lock` guarantee).
    pub fn ensure_started(&self) {
        if self.num_workers.load(Ordering::Acquire) == 0 {
            self.spawn_one();
        }
    }

    /// Try to spawn one worker, reserving its count slot first so concurrent
    /// callers don't over-spawn; gives the slot back on failure. Returns success.
    fn spawn_one(&self) -> bool {
        self.num_workers.fetch_add(1, Ordering::AcqRel);
        if self.os.spawn(worker_entry::<T, Os>, self as *const _ as *mut c_void) {
            true
        } else {
            self.num_workers.fetch_sub(1, Ordering::AcqRel);
            false
        }
    }

    /// Spawn a worker iff the policy wants one for a submit whose pre-submit busy
    /// count was `busy_before` (C: `num_busy_workers >= num_workers && num_workers
    /// < max_workers`). Returns true if a worker was spawned.
    fn maybe_spawn(&self, busy_before: i32) -> bool {
        let workers = self.num_workers.load(Ordering::Acquire);
        let max = self.max_workers.load(Ordering::Acquire);
        if busy_before >= workers && workers < max {
            return self.spawn_one();
        }
        false
    }

    /// Submit one callback for `obj` (the worker-managed `tp_object_submit`):
    /// enqueue (so any worker we spawn next will see it), then either spawn a new
    /// worker or wake an idle one.
    ///
    /// # Safety
    /// `obj` must satisfy [`Scheduled`] and outlive its pending callbacks.
    pub unsafe fn submit(&self, obj: *mut T) {
        let busy_before = self.scheduler.busy();
        unsafe { self.scheduler.submit(obj) };
        if !self.maybe_spawn(busy_before) {
            // Bump the generation before waking so a worker that just read it
            // (and is about to sleep) sees the change and does not miss this item.
            self.update.fetch_add(1, Ordering::AcqRel);
            self.os.futex_wake_one(&self.update);
        }
    }

    /// A surplus worker may terminate on idle (C: `num_workers > max(min,1) ||
    /// (!min && !objcount)`).
    fn can_terminate(&self) -> bool {
        let workers = self.num_workers.load(Ordering::Acquire);
        let min = self.min_workers.load(Ordering::Acquire);
        let objcount = self.objcount.load(Ordering::Acquire);
        workers > min.max(1) || (min == 0 && objcount == 0)
    }

    /// The worker loop (the Rust `threadpool_worker_proc`): drain all ready
    /// callbacks, then sleep on the futex until a submit wakes us or the idle
    /// timeout lets a surplus worker exit.
    fn run_worker(&self) {
        loop {
            while let Some(obj) = unsafe { self.scheduler.worker_take::<T>() } {
                (self.run)(obj);
                self.scheduler.callback_done();
                // (slice 1c: release the object's refcount here.)
            }

            if self.shutdown.load(Ordering::Acquire) {
                self.num_workers.fetch_sub(1, Ordering::AcqRel);
                return;
            }

            // Read the generation, then sleep comparing against it. A submit that
            // raced our drain bumped the generation, so the wait returns at once.
            let seen = self.update.load(Ordering::Acquire);
            let woken = self.os.futex_wait(&self.update, seen, self.worker_timeout_100ns);
            if woken || !self.can_terminate() {
                continue; // more work may have arrived, or we must stay alive
            }

            // Idle timeout on a surplus worker. We must NOT trust `!woken` to mean
            // "no work": the lock-free futex (RtlWaitOnAddress) can return TIMEOUT
            // while a submit's wake is lost to that very timeout, leaving an item
            // queued. So re-check the queue before terminating -- the Rust
            // equivalent of the C worker proc's `!threadpool_get_next_item`.
            //
            // Decrement num_workers *first* so a submit that races this check sees
            // the lower count and spawns a replacement; then verify the queue. If
            // an item slipped in, rejoin (re-increment) and drain it.
            self.num_workers.fetch_sub(1, Ordering::AcqRel);
            if self.scheduler.is_empty() {
                return;
            }
            self.num_workers.fetch_add(1, Ordering::AcqRel);
            // (slice 1c: tp_threadpool_release on the real exit paths above.)
        }
    }

    /// Signal shutdown, wake every worker, and join them (teardown).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.update.fetch_add(1, Ordering::AcqRel);
        self.os.futex_wake_all(&self.update);
        self.os.join_all();
    }
}

// ===========================================================================
// Threadpool object model + callback dispatch (stage 1c, work/simple path).
//
// The Rust-native replacement for the C `struct threadpool_object` and the
// `tp_object_execute` callback switch, for the TP_OBJECT_TYPE_SIMPLE / WORK
// types. Timer/Wait/IO variants (and their event-source service threads) arrive
// in stages 2-4; the refcount-lifecycle + finalization + the TpCallback* cleanup
// API are completed alongside the wine wiring.
// ===========================================================================

/// `CALLBACK`-ABI work callback: `void(PTP_CALLBACK_INSTANCE, PVOID context, PTP_WORK)`.
pub type WorkCallback = extern "system" fn(*mut CallbackInstance, *mut c_void, *mut c_void);
/// `CALLBACK`-ABI simple callback: `void(PTP_CALLBACK_INSTANCE, PVOID context)`.
pub type SimpleCallback = extern "system" fn(*mut CallbackInstance, *mut c_void);

/// The callback and its type, stored per object (the C `union u` + `type` tag,
/// for the work/simple subset).
pub enum Callback {
    Simple(SimpleCallback),
    Work(WorkCallback),
}

/// Per-callback `TP_CALLBACK_INSTANCE`. Minimal for now -- enough to pass to the
/// callback; the TpCallback* cleanup/finalization slots are added with that API.
#[repr(C)]
pub struct CallbackInstance {
    object: *mut ThreadpoolObject,
    may_run_long: bool,
}

/// Rust-native threadpool object (work/simple).
pub struct ThreadpoolObject {
    sched: SchedState,
    callback: Callback,
    userdata: *mut c_void,
    may_run_long: bool,
    /// Object lifetime refcount (C `object->refcount`): +1 per pending callback,
    /// -1 after each runs; the allocation is freed at 0. Read once the release
    /// path is wired (refcount sub-slice of 1c).
    #[allow(dead_code)]
    refcount: AtomicI32,
}

unsafe impl Scheduled for ThreadpoolObject {
    fn sched_state(&self) -> &SchedState {
        &self.sched
    }
}

// Shared across worker threads (as the C object is): `userdata`/`callback` are
// read-only after construction and the scheduling state is atomic.
unsafe impl Send for ThreadpoolObject {}
unsafe impl Sync for ThreadpoolObject {}

impl ThreadpoolObject {
    pub fn new_work(callback: WorkCallback, userdata: *mut c_void, priority: u8) -> Self {
        ThreadpoolObject {
            sched: SchedState::new(priority),
            callback: Callback::Work(callback),
            userdata,
            may_run_long: false,
            refcount: AtomicI32::new(0),
        }
    }
    pub fn new_simple(callback: SimpleCallback, userdata: *mut c_void, priority: u8) -> Self {
        ThreadpoolObject {
            sched: SchedState::new(priority),
            callback: Callback::Simple(callback),
            userdata,
            may_run_long: false,
            refcount: AtomicI32::new(0),
        }
    }
}

/// Run one callback for `obj` (the Rust `tp_object_execute`, work/simple). This
/// is the `Pool::run` seam for `Pool<ThreadpoolObject, _>`.
///
/// # Safety
/// `obj` must point to a live [`ThreadpoolObject`] for the callback's duration.
pub fn dispatch_object(obj: *mut ThreadpoolObject) {
    // SAFETY: live for the callback per the scheduler's contract.
    let o = unsafe { &*obj };
    let mut instance = CallbackInstance { object: obj, may_run_long: o.may_run_long };
    let inst = &mut instance as *mut CallbackInstance;
    match o.callback {
        Callback::Simple(cb) => cb(inst, o.userdata),
        Callback::Work(cb) => cb(inst, o.userdata, obj.cast()),
    }
    // (1c later: finalization callback, instance cleanup tasks, simple-object
    // auto-shutdown, and the refcount release.)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::thread::JoinHandle;
    use std::time::Duration;
    use std::vec::Vec;

    struct TestObj {
        sched: SchedState,
        ran: AtomicI32,
    }
    impl TestObj {
        fn new(priority: u8) -> Box<TestObj> {
            Box::new(TestObj { sched: SchedState::new(priority), ran: AtomicI32::new(0) })
        }
    }
    unsafe impl Scheduled for TestObj {
        fn sched_state(&self) -> &SchedState {
            &self.sched
        }
    }

    /// Host `PoolOs`: real std worker threads + a condvar-emulated futex. The
    /// generation is checked under the same lock the wake holds, so the
    /// `RtlWaitOnAddress` no-lost-wakeup contract holds.
    struct TestOs {
        handles: Mutex<Vec<JoinHandle<()>>>,
        lock: Mutex<()>,
        cv: Condvar,
    }
    impl TestOs {
        fn new() -> Self {
            TestOs { handles: Mutex::new(Vec::new()), lock: Mutex::new(()), cv: Condvar::new() }
        }
    }
    impl PoolOs for TestOs {
        fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool {
            let arg = arg as usize; // raw ptr isn't Send; the pool outlives the threads
            let h = thread::spawn(move || {
                entry(arg as *mut c_void);
            });
            self.handles.lock().unwrap().push(h);
            true
        }
        fn futex_wait(&self, addr: &AtomicU32, expected: u32, timeout_100ns: i64) -> bool {
            let guard = self.lock.lock().unwrap();
            if addr.load(Ordering::Acquire) != expected {
                return true; // already changed -> treat as woken
            }
            let dur = Duration::from_nanos(timeout_100ns.max(0) as u64 * 100);
            let (_g, res) = self.cv.wait_timeout(guard, dur).unwrap();
            !res.timed_out()
        }
        fn futex_wake_one(&self, _addr: &AtomicU32) {
            let _g = self.lock.lock().unwrap();
            self.cv.notify_one();
        }
        fn futex_wake_all(&self, _addr: &AtomicU32) {
            let _g = self.lock.lock().unwrap();
            self.cv.notify_all();
        }
        fn join_all(&self) {
            let mut hs = self.handles.lock().unwrap();
            for h in hs.drain(..) {
                h.join().unwrap();
            }
        }
    }

    /// Adversarial `PoolOs` modelling the *real* lock-free futex
    /// (`RtlWaitOnAddress`): every wake is lost and every wait "times out". A
    /// correct `run_worker` must still drain all work via its terminate re-check
    /// (+ spawn-on-submit), never stranding an item -- which `TestOs`'s condvar
    /// cannot stress, since it never loses a wakeup.
    struct LossyOs {
        handles: Mutex<Vec<JoinHandle<()>>>,
    }
    impl LossyOs {
        fn new() -> Self {
            LossyOs { handles: Mutex::new(Vec::new()) }
        }
    }
    impl PoolOs for LossyOs {
        fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool {
            let arg = arg as usize;
            let h = thread::spawn(move || {
                entry(arg as *mut c_void);
            });
            self.handles.lock().unwrap().push(h);
            true
        }
        fn futex_wait(&self, _addr: &AtomicU32, _expected: u32, _t: i64) -> bool {
            // Always "time out", never report a wake -> maximally lossy. The nap
            // makes a worker linger (so maybe_spawn defers to it rather than
            // spawning a replacement) -- the window where a missing terminate
            // re-check actually strands work.
            thread::sleep(Duration::from_millis(2));
            false
        }
        fn futex_wake_one(&self, _addr: &AtomicU32) {} // wake lost
        fn futex_wake_all(&self, _addr: &AtomicU32) {} // wake lost
        fn join_all(&self) {
            for h in self.handles.lock().unwrap().drain(..) {
                h.join().unwrap();
            }
        }
    }

    /// The dispatch seam (slice 1c plugs in tp_object_execute); here just count.
    fn run_test_obj(o: *mut TestObj) {
        unsafe { (*o).ran.fetch_add(1, Ordering::Relaxed) };
    }

    fn total_ran(objs: &[Box<TestObj>]) -> i32 {
        objs.iter().map(|o| o.ran.load(Ordering::Relaxed)).sum()
    }

    fn spin_until(mut cond: impl FnMut() -> bool, what: &str) {
        let mut spins: u64 = 0;
        while !cond() {
            spins += 1;
            assert!(spins < 2_000_000_000, "timed out waiting for {what}");
            core::hint::spin_loop();
        }
    }

    /// High priority drains before normal before low; FIFO within a priority.
    #[test]
    fn priority_and_fifo_order() {
        let sched = Scheduler::new();
        let low_a = TestObj::new(2);
        let low_b = TestObj::new(2);
        let high = TestObj::new(0);
        let normal = TestObj::new(1);
        let order = [&low_a, &low_b, &high, &normal];
        for o in order {
            let p = Box::as_ref(o) as *const TestObj as *mut TestObj;
            unsafe { assert!(sched.submit(p)) };
        }
        // Expected claim order: high, normal, low_a, low_b.
        let expect: [*const TestObj; 4] = [
            Box::as_ref(&high),
            Box::as_ref(&normal),
            Box::as_ref(&low_a),
            Box::as_ref(&low_b),
        ];
        for e in expect {
            let got = unsafe { sched.worker_take::<TestObj>() }.expect("non-empty");
            assert_eq!(got as *const TestObj, e);
        }
        assert!(unsafe { sched.worker_take::<TestObj>() }.is_none());
    }

    /// Each object's callback runs exactly as many times as it was submitted.
    #[test]
    fn runs_exactly_submit_count_single_thread() {
        let sched = Scheduler::new();
        let objs: Vec<Box<TestObj>> = (0..16).map(|i| TestObj::new((i % 3) as u8)).collect();
        let counts: Vec<i32> = (0..16).map(|i| (i * 7) % 5 + 1).collect();
        for (o, &k) in objs.iter().zip(&counts) {
            let p = Box::as_ref(o) as *const TestObj as *mut TestObj;
            for _ in 0..k {
                unsafe { sched.submit(p) };
            }
        }
        // Drain, running each claimed object once.
        while let Some(p) = unsafe { sched.worker_take::<TestObj>() } {
            unsafe { (*p).ran.fetch_add(1, Ordering::Relaxed) };
        }
        for (o, &k) in objs.iter().zip(&counts) {
            assert_eq!(o.ran.load(Ordering::Relaxed), k, "object ran != submitted");
            assert_eq!(o.sched.pending(), 0, "pending not drained");
        }
    }

    /// Many producers submit many callbacks across a shared object set while many
    /// workers drain; every object must run exactly its submitted count, with no
    /// lost or duplicated callbacks and no hang.
    #[test]
    fn mpmc_stress_conserves_callbacks() {
        const PRODUCERS: usize = 4;
        const WORKERS: usize = 4;
        const OBJECTS: usize = 32;
        const SUBMITS_PER_PRODUCER: usize = 25_000;

        let sched = Arc::new(Scheduler::new());
        let objs: Arc<Vec<Box<TestObj>>> =
            Arc::new((0..OBJECTS).map(|i| TestObj::new((i % 3) as u8)).collect());
        let submitted = Arc::new((0..OBJECTS).map(|_| AtomicI32::new(0)).collect::<Vec<_>>());
        let done = Arc::new(AtomicBool::new(false));

        let mut handles = Vec::new();
        for p in 0..PRODUCERS {
            let sched = sched.clone();
            let objs = objs.clone();
            let submitted = submitted.clone();
            handles.push(thread::spawn(move || {
                for i in 0..SUBMITS_PER_PRODUCER {
                    // Spread across objects deterministically per producer.
                    let idx = (p * 7 + i * 13) % OBJECTS;
                    submitted[idx].fetch_add(1, Ordering::Relaxed);
                    let ptr = Box::as_ref(&objs[idx]) as *const TestObj as *mut TestObj;
                    unsafe { sched.submit(ptr) };
                }
            }));
        }
        for _ in 0..WORKERS {
            let sched = sched.clone();
            let done = done.clone();
            handles.push(thread::spawn(move || {
                // Spin-bounded: a lost callback would otherwise hang; cap idle
                // spins so a bug fails instead of deadlocking the test. Every
                // dequeued object MUST be run -- never discard a `Some` (that
                // would itself lose a callback). Once producers are done, an empty
                // queue means we are finished: any in-flight re-queue is owned by
                // the worker still in its `Some` branch, which drains it before it
                // too observes an empty queue.
                let mut idle: u64 = 0;
                loop {
                    match unsafe { sched.worker_take::<TestObj>() } {
                        Some(p) => {
                            unsafe { (*p).ran.fetch_add(1, Ordering::Relaxed) };
                            idle = 0;
                        }
                        None => {
                            if done.load(Ordering::Acquire) {
                                break;
                            }
                            idle += 1;
                            assert!(idle < 2_000_000_000, "worker spun forever -- lost callback");
                            core::hint::spin_loop();
                        }
                    }
                }
            }));
        }

        for h in handles.drain(..PRODUCERS) {
            h.join().unwrap();
        }
        done.store(true, Ordering::Release);
        for h in handles {
            h.join().unwrap();
        }

        let total_submitted: i32 = submitted.iter().map(|a| a.load(Ordering::Relaxed)).sum();
        let total_ran: i32 = objs.iter().map(|o| o.ran.load(Ordering::Relaxed)).sum();
        assert_eq!(total_ran, total_submitted, "total callbacks lost/duplicated");
        for (i, o) in objs.iter().enumerate() {
            assert_eq!(
                o.ran.load(Ordering::Relaxed),
                submitted[i].load(Ordering::Relaxed),
                "object {i} ran != submitted"
            );
            assert_eq!(o.sched.pending(), 0, "object {i} pending not drained");
        }
    }

    /// Full worker lifecycle over the real Pool: concurrent producers drive
    /// spawn-on-demand, every callback runs exactly once, idle workers
    /// self-terminate on the (shortened) timeout, and a later batch re-spawns
    /// workers and still drains. Exercises the lock-free hand-off + atomic worker
    /// management together with no lost work and no hang.
    #[test]
    fn worker_lifecycle_spawn_drain_terminate_respawn() {
        const PRODUCERS: usize = 4;
        const PER_PRODUCER: usize = 10_000;
        const OBJECTS: usize = 16;

        let objs: Vec<Box<TestObj>> = (0..OBJECTS).map(|i| TestObj::new((i % 3) as u8)).collect();
        let submitted: Vec<AtomicI32> = (0..OBJECTS).map(|_| AtomicI32::new(0)).collect();

        let mut pool = Box::new(Pool::new(TestOs::new(), run_test_obj as fn(*mut TestObj)));
        pool.set_min_workers(0);
        pool.set_worker_timeout(200_000); // 20 ms, so idle termination is testable
        let pool: Box<Pool<TestObj, TestOs>> = pool; // freeze: no more &mut while shared
        let pool_ref: &Pool<TestObj, TestOs> = &pool;

        let obj_ptr = |i: usize| Box::as_ref(&objs[i]) as *const TestObj as *mut TestObj;

        // Phase 1: keep workers alive (one bound object set) and hammer from many
        // producers; the pool spawns workers on demand.
        pool_ref.obj_added();
        pool_ref.ensure_started();
        thread::scope(|s| {
            for p in 0..PRODUCERS {
                let submitted = &submitted;
                let objs_ref = &objs;
                s.spawn(move || {
                    let _ = &objs_ref;
                    for i in 0..PER_PRODUCER {
                        let idx = (p * 7 + i * 13) % OBJECTS;
                        submitted[idx].fetch_add(1, Ordering::Relaxed);
                        let ptr = Box::as_ref(&objs_ref[idx]) as *const TestObj as *mut TestObj;
                        unsafe { pool_ref.submit(ptr) };
                    }
                });
            }
        });
        let phase1: i32 = submitted.iter().map(|a| a.load(Ordering::Relaxed)).sum();
        spin_until(|| total_ran(&objs) == phase1, "phase 1 drain");
        for i in 0..OBJECTS {
            assert_eq!(
                objs[i].ran.load(Ordering::Relaxed),
                submitted[i].load(Ordering::Relaxed),
                "object {i} ran != submitted (phase 1)"
            );
            assert_eq!(objs[i].sched.pending(), 0, "object {i} pending (phase 1)");
        }
        assert!(pool_ref.num_workers() >= 1, "expected live workers in phase 1");

        // Phase 2: unbind the object set; idle workers must self-terminate.
        pool_ref.obj_removed();
        spin_until(|| pool_ref.num_workers() == 0, "idle worker termination");

        // Phase 3: submit again -> the pool must re-spawn a worker and drain.
        pool_ref.obj_added();
        let before = total_ran(&objs);
        for i in 0..OBJECTS {
            unsafe { pool_ref.submit(obj_ptr(i)) };
        }
        spin_until(|| total_ran(&objs) == before + OBJECTS as i32, "phase 3 drain");
        for i in 0..OBJECTS {
            assert_eq!(
                objs[i].ran.load(Ordering::Relaxed),
                submitted[i].load(Ordering::Relaxed) + 1,
                "object {i} ran != submitted (phase 3)"
            );
        }

        // Teardown: join every worker thread before the pool/objects drop.
        pool_ref.shutdown();
        assert_eq!(pool_ref.num_workers(), 0, "workers left after shutdown");
    }

    /// Under the maximally-lossy futex ([`LossyOs`], every wake lost), with
    /// `min_workers = 0` so every idle worker aggressively tries to terminate,
    /// concurrent producers must still see every callback run -- i.e. the
    /// `run_worker` terminate re-check (+ resurrect) strands no work. Without that
    /// re-check, the tail items submitted as workers exit are lost and this hangs.
    #[test]
    fn terminate_submit_race_loses_no_work() {
        const PRODUCERS: usize = 2;
        const PER_PRODUCER: usize = 80;
        const OBJECTS: usize = 8;

        let objs: Vec<Box<TestObj>> = (0..OBJECTS).map(|i| TestObj::new((i % 3) as u8)).collect();
        let submitted: Vec<AtomicI32> = (0..OBJECTS).map(|_| AtomicI32::new(0)).collect();

        let pool = Box::new(Pool::new(LossyOs::new(), run_test_obj as fn(*mut TestObj)));
        pool.set_min_workers(0); // aggressive termination -> maximal terminate/submit racing
        let pool: Box<Pool<TestObj, LossyOs>> = pool;
        let pool_ref: &Pool<TestObj, LossyOs> = &pool;
        pool_ref.ensure_started();

        thread::scope(|s| {
            for p in 0..PRODUCERS {
                let submitted = &submitted;
                let objs_ref = &objs;
                s.spawn(move || {
                    for i in 0..PER_PRODUCER {
                        let idx = (p * 3 + i * 7) % OBJECTS;
                        submitted[idx].fetch_add(1, Ordering::Relaxed);
                        let ptr = Box::as_ref(&objs_ref[idx]) as *const TestObj as *mut TestObj;
                        unsafe { pool_ref.submit(ptr) };
                        // Gap < the lossy wait, so each item tends to land while a
                        // worker is asleep (maybe_spawn defers to it) -- the window
                        // where a missing terminate re-check strands the item.
                        thread::sleep(Duration::from_millis(1));
                    }
                });
            }
        });

        let total: i32 = submitted.iter().map(|a| a.load(Ordering::Relaxed)).sum();
        spin_until(|| total_ran(&objs) == total, "lossy-futex drain (no stranded work)");
        for i in 0..OBJECTS {
            assert_eq!(
                objs[i].ran.load(Ordering::Relaxed),
                submitted[i].load(Ordering::Relaxed),
                "object {i} ran != submitted under lossy futex"
            );
        }
        pool_ref.shutdown();
    }

    /// Real work callback: records its run through `userdata` and checks it got a
    /// non-null work handle (the object pointer).
    extern "system" fn count_work_cb(
        _inst: *mut CallbackInstance,
        userdata: *mut c_void,
        work: *mut c_void,
    ) {
        assert!(!work.is_null(), "work handle must be non-null");
        unsafe { (*(userdata as *const AtomicI32)).fetch_add(1, Ordering::Relaxed) };
    }

    /// End-to-end: ThreadpoolObjects carrying real `extern "system"` work
    /// callbacks flow Pool::submit -> scheduler -> worker -> dispatch_object ->
    /// the callback, with the right userdata and work handle, exactly once each.
    #[test]
    fn work_objects_dispatch_real_callbacks() {
        const OBJECTS: usize = 12;
        const PRODUCERS: usize = 3;
        const PER_PRODUCER: usize = 5_000;

        let counters: Vec<AtomicI32> = (0..OBJECTS).map(|_| AtomicI32::new(0)).collect();
        let objs: Vec<Box<ThreadpoolObject>> = (0..OBJECTS)
            .map(|i| {
                let ud = &counters[i] as *const AtomicI32 as *mut c_void;
                Box::new(ThreadpoolObject::new_work(count_work_cb, ud, (i % 3) as u8))
            })
            .collect();
        let submitted: Vec<AtomicI32> = (0..OBJECTS).map(|_| AtomicI32::new(0)).collect();

        let pool = Box::new(Pool::new(TestOs::new(), dispatch_object as fn(*mut ThreadpoolObject)));
        pool.set_min_workers(1);
        let pool: Box<Pool<ThreadpoolObject, TestOs>> = pool;
        let pool_ref: &Pool<ThreadpoolObject, TestOs> = &pool;
        pool_ref.obj_added();
        pool_ref.ensure_started();

        thread::scope(|s| {
            for p in 0..PRODUCERS {
                let submitted = &submitted;
                let objs_ref = &objs;
                s.spawn(move || {
                    for i in 0..PER_PRODUCER {
                        let idx = (p * 5 + i * 11) % OBJECTS;
                        submitted[idx].fetch_add(1, Ordering::Relaxed);
                        let ptr = Box::as_ref(&objs_ref[idx]) as *const ThreadpoolObject
                            as *mut ThreadpoolObject;
                        unsafe { pool_ref.submit(ptr) };
                    }
                });
            }
        });

        let total: i32 = submitted.iter().map(|a| a.load(Ordering::Relaxed)).sum();
        spin_until(
            || counters.iter().map(|c| c.load(Ordering::Relaxed)).sum::<i32>() == total,
            "dispatch drain",
        );
        for i in 0..OBJECTS {
            assert_eq!(
                counters[i].load(Ordering::Relaxed),
                submitted[i].load(Ordering::Relaxed),
                "object {i} callback count != submitted"
            );
        }
        pool_ref.shutdown();
    }
}
