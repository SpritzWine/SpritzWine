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

use crate::work_queue::{Node, SpinLock, TwoLockQueue};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

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
    /// Called by [`Pool::submit`] for each submitted callback (the C
    /// `InterlockedIncrement(&object->refcount)` in tp_object_submit). Default
    /// no-op (test objects without a refcount); [`ThreadpoolObject`] bumps its
    /// refcount so the allocation outlives the in-flight callback.
    fn on_submit(&self) {}
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
    /// Monotonic clock in 100ns units (relative timers; wine: NtQueryPerformanceCounter).
    fn now_mono_100ns(&self) -> i64;
    /// Wall-clock in 100ns units (absolute timers; wine: NtQuerySystemTime).
    fn now_system_100ns(&self) -> i64;
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
    /// Lifetime refcount (C `pool->refcount`): base 1, +1 per spawned worker; the
    /// allocation is freed when the last reference drops (so TpReleasePool can
    /// free a pool whose workers are still exiting).
    refcount: AtomicI32,
    shutdown: AtomicBool,
    /// Futex generation; bumped before each submit-wake so a worker between
    /// reading it and sleeping cannot miss the wake.
    update: AtomicU32,
    worker_timeout_100ns: i64,
    /// TP_POOL_STACK_INFORMATION (stored for TpQueryPoolStackInformation; worker
    /// creation currently uses the default stack).
    stack_reserve: AtomicUsize,
    stack_commit: AtomicUsize,
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
    let pool = arg as *mut Pool<T, Os>;
    unsafe { (*pool).run_worker() };
    // Drop this worker's reference; free the pool if it was the last (the C
    // tp_threadpool_release). Host-test pools keep their base ref (owned by the
    // test), so this never frees there.
    if unsafe { (*pool).release_ref() } {
        drop(unsafe { Box::from_raw(pool) });
    }
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
            refcount: AtomicI32::new(1), // base reference
            shutdown: AtomicBool::new(false),
            update: AtomicU32::new(0),
            worker_timeout_100ns: WORKER_TIMEOUT_100NS,
            stack_reserve: AtomicUsize::new(0),
            stack_commit: AtomicUsize::new(0),
            run,
            os,
        }
    }

    pub fn set_stack_info(&self, reserve: usize, commit: usize) {
        self.stack_reserve.store(reserve, Ordering::Release);
        self.stack_commit.store(commit, Ordering::Release);
    }
    pub fn stack_info(&self) -> (usize, usize) {
        (self.stack_reserve.load(Ordering::Acquire), self.stack_commit.load(Ordering::Acquire))
    }

    /// Take a pool reference (C `tp_threadpool_lock`'s refcount bump; each bound
    /// object holds one).
    pub fn add_ref(&self) {
        self.refcount.fetch_add(1, Ordering::AcqRel);
    }
    /// Drop one pool reference; returns true if it was the last (caller frees).
    pub fn release_ref(&self) -> bool {
        self.refcount.fetch_sub(1, Ordering::AcqRel) == 1
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
        self.refcount.fetch_add(1, Ordering::AcqRel); // the worker holds a pool ref
        if self.os.spawn(worker_entry::<T, Os>, self as *const _ as *mut c_void) {
            true
        } else {
            self.refcount.fetch_sub(1, Ordering::AcqRel);
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
        unsafe { (*obj).on_submit() }; // refcount++ before enqueue (C tp_object_submit)
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
/// `CALLBACK`-ABI timer callback: `void(PTP_CALLBACK_INSTANCE, PVOID context, PTP_TIMER)`.
pub type TimerCallback = extern "system" fn(*mut CallbackInstance, *mut c_void, *mut c_void);
/// `CALLBACK`-ABI wait callback: `void(PTP_CALLBACK_INSTANCE, PVOID, PTP_WAIT, TP_WAIT_RESULT)`.
pub type WaitCallback = extern "system" fn(*mut CallbackInstance, *mut c_void, *mut c_void, u32);
/// RTL_WAITORTIMERCALLBACKFUNC: `void(PVOID context, BOOLEAN timer_or_wait_fired)`.
pub type RtlWaitOrTimerCallback = extern "system" fn(*mut c_void, u8);
/// `CALLBACK`-ABI I/O callback:
/// `void(PTP_CALLBACK_INSTANCE, PVOID, PVOID overlapped, PIO_STATUS_BLOCK, PTP_IO)`.
pub type IoCallback =
    extern "system" fn(*mut CallbackInstance, *mut c_void, *mut c_void, *mut IoStatusBlock, *mut c_void);

/// `TP_WAIT_RESULT` values handed to a wait callback.
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 0x0000_0102;

/// `IO_STATUS_BLOCK` passed (by pointer) to an I/O callback.
#[repr(C)]
pub struct IoStatusBlock {
    pub status: i32,
    pub information: usize,
}

/// The callback and its type, stored per object (the C `union u` + `type` tag).
/// Timer scheduling state lives in the [`TimerQueue`]; wait scheduling state in
/// the [`WaitQueue`](crate::wait_queue::WaitQueue) (only the signaled count, used
/// at dispatch, is on the object); I/O completion state in the object's `io`.
pub enum Callback {
    Simple(SimpleCallback),
    Work(WorkCallback),
    Timer(TimerCallback),
    Wait(WaitCallback),
    Io(IoCallback),
}

/// Per-callback `TP_CALLBACK_INSTANCE`. Minimal for now -- enough to pass to the
/// callback; the TpCallback* cleanup/finalization slots are added with that API.
#[repr(C)]
pub struct CallbackInstance {
    object: *mut ThreadpoolObject,
    may_run_long: bool,
    /// Cleared by TpDisassociateCallback so a non-group wait stops waiting for
    /// this callback (the C `instance.associated`).
    associated: bool,
}

impl CallbackInstance {
    /// TpDisassociateCallback: detach this callback from its object's non-group
    /// wait. Idempotent.
    pub fn disassociate(&mut self) {
        if self.associated {
            self.associated = false;
            unsafe { &*self.object }.num_associated.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Operations a concrete pool exposes to its (non-generic) objects: submit one
/// callback, and detach the object at free (objcount-- + pool-ref release). A
/// `Pool<ThreadpoolObject, WineOs>` installs a `&'static PoolOps`, so the object
/// reaches its pool without itself being generic over the pool's `Os`.
pub struct PoolOps {
    pub submit: unsafe fn(pool: *const c_void, obj: *mut ThreadpoolObject),
    pub unbind: unsafe fn(pool: *const c_void),
}

/// Rust-native threadpool object (the C `struct threadpool_object`).
pub struct ThreadpoolObject {
    sched: SchedState,
    callback: Callback,
    userdata: *mut c_void,
    may_run_long: bool,
    /// Object lifetime refcount (C `object->refcount`): starts at 1 (the base
    /// reference released by TpRelease*/cleanup-group), +1 per submitted callback,
    /// -1 after each runs; the allocation is freed at 0 ([`tp_object_release`]).
    refcount: AtomicI32,
    /// Callbacks currently executing (C `num_running_callbacks`); a group wait
    /// ([`tp_object_wait`] with `group=true`) drains on this.
    num_running: AtomicI32,
    /// Like `num_running` but excluding callbacks that called TpDisassociateCallback
    /// (C `num_associated_callbacks`); a non-group wait drains on this.
    num_associated: AtomicI32,
    /// Set by [`tp_object_cancel`]; a worker skips a cancelled object's callback
    /// (still draining the pending count + releasing its reference).
    cancelled: AtomicBool,
    /// Owning pool (C `object->pool`), type-erased; reached via [`pool_ops`] so the
    /// object need not be generic over the pool's `Os`. Null for unbound objects.
    pub(crate) pool: *const c_void,
    pub(crate) pool_ops: *const PoolOps,
    /// Cleanup-group membership (C `object->group` / `is_group_member`); set at
    /// init via [`Group::add_member`](crate::cleanup_group::Group::add_member).
    pub(crate) group: *const crate::cleanup_group::Group,
    pub(crate) is_group_member: AtomicBool,
    /// Group cancel callback (C `group_cancel_callback`), run by
    /// TpReleaseCleanupGroupMembers on a cancel.
    pub(crate) group_cancel_callback: Option<extern "system" fn(*mut c_void, *mut c_void)>,
    /// Timer objects: whether the last TpSetTimer armed it with a timeout (C
    /// `object->u.timer.timer_set`); read by TpIsTimerSet, not cleared on fire.
    pub(crate) timer_set: AtomicBool,
    /// Wait objects: count of signaled (vs timed-out) callbacks owed, set by the
    /// [`WaitQueue`](crate::wait_queue::WaitQueue) and consumed at dispatch to
    /// pick `WAIT_OBJECT_0` vs `WAIT_TIMEOUT` (C `object->u.wait.signaled`).
    pub(crate) signaled: AtomicI32,
    /// RtlRegisterWait objects: the RTL callback the wait dispatch bridges to
    /// (C `object->u.wait.rtl_callback`); `userdata` is its context.
    pub(crate) rtl_wait_callback: Option<RtlWaitOrTimerCallback>,
    /// I/O objects only: the pending/completion state (C `object->u.io`). `None`
    /// for every other object type, so non-I/O objects pay one word.
    pub(crate) io: Option<Box<crate::io_queue::IoState>>,
}

unsafe impl Scheduled for ThreadpoolObject {
    fn sched_state(&self) -> &SchedState {
        &self.sched
    }
    fn on_submit(&self) {
        self.refcount.fetch_add(1, Ordering::AcqRel);
    }
}

// Shared across worker threads (as the C object is): `userdata`/`callback` are
// read-only after construction and the scheduling state is atomic.
unsafe impl Send for ThreadpoolObject {}
unsafe impl Sync for ThreadpoolObject {}

impl ThreadpoolObject {
    fn with_callback(callback: Callback, userdata: *mut c_void, priority: u8) -> Self {
        ThreadpoolObject {
            sched: SchedState::new(priority),
            callback,
            userdata,
            may_run_long: false,
            refcount: AtomicI32::new(1), // base reference
            num_running: AtomicI32::new(0),
            num_associated: AtomicI32::new(0),
            cancelled: AtomicBool::new(false),
            pool: core::ptr::null(),
            pool_ops: core::ptr::null(),
            group: core::ptr::null(),
            is_group_member: AtomicBool::new(false),
            group_cancel_callback: None,
            timer_set: AtomicBool::new(false),
            signaled: AtomicI32::new(0),
            rtl_wait_callback: None,
            io: None,
        }
    }

    pub(crate) fn set_rtl_wait_callback(&mut self, cb: RtlWaitOrTimerCallback) {
        self.rtl_wait_callback = Some(cb);
    }

    pub(crate) fn set_pool(&mut self, pool: *const c_void, ops: *const PoolOps) {
        self.pool = pool;
        self.pool_ops = ops;
    }
    /// Submit this object to its pool (C `tp_object_submit`, used by TpPostWork).
    pub(crate) fn submit_to_pool(&self) {
        if !self.pool_ops.is_null() {
            unsafe { ((*self.pool_ops).submit)(self.pool, self.as_ptr()) };
        }
    }
    /// Detach from the pool (objcount-- + pool ref release), at object free.
    fn unbind_from_pool(&self) {
        if !self.pool_ops.is_null() {
            unsafe { ((*self.pool_ops).unbind)(self.pool) };
        }
    }

    pub fn userdata(&self) -> *mut c_void {
        self.userdata
    }
    /// Add a reference (C `InterlockedIncrement(&object->refcount)`).
    pub(crate) fn add_ref(&self) {
        self.refcount.fetch_add(1, Ordering::AcqRel);
    }
    pub fn set_group_cancel_callback(
        &mut self,
        cb: Option<extern "system" fn(*mut c_void, *mut c_void)>,
    ) {
        self.group_cancel_callback = cb;
    }
    pub fn new_work(callback: WorkCallback, userdata: *mut c_void, priority: u8) -> Self {
        Self::with_callback(Callback::Work(callback), userdata, priority)
    }
    pub fn new_simple(callback: SimpleCallback, userdata: *mut c_void, priority: u8) -> Self {
        Self::with_callback(Callback::Simple(callback), userdata, priority)
    }
    pub fn new_timer(callback: TimerCallback, userdata: *mut c_void, priority: u8) -> Self {
        Self::with_callback(Callback::Timer(callback), userdata, priority)
    }
    pub fn new_wait(callback: WaitCallback, userdata: *mut c_void, priority: u8) -> Self {
        Self::with_callback(Callback::Wait(callback), userdata, priority)
    }
    pub fn new_io(callback: IoCallback, userdata: *mut c_void, priority: u8) -> Self {
        let mut o = Self::with_callback(Callback::Io(callback), userdata, priority);
        o.io = Some(Box::new(crate::io_queue::IoState::new()));
        o
    }

    /// I/O completion state, for the [`IoQueue`](crate::io_queue::IoQueue).
    ///
    /// # Panics
    /// Only valid on objects created via [`new_io`](Self::new_io).
    pub(crate) fn io_state(&self) -> &crate::io_queue::IoState {
        self.io.as_ref().expect("io_state on a non-I/O object")
    }

    /// Raw object pointer for use as a TP_* handle / scheduler key.
    pub fn as_ptr(&self) -> *mut ThreadpoolObject {
        self as *const ThreadpoolObject as *mut ThreadpoolObject
    }
}

/// Run one callback for `obj` (the Rust `tp_object_execute`, work/simple). This
/// is the `Pool::run` seam for `Pool<ThreadpoolObject, _>`.
///
/// # Safety
/// `obj` must point to a live [`ThreadpoolObject`] for the callback's duration.
pub fn dispatch_object(obj: *mut ThreadpoolObject) {
    // SAFETY: live for the callback per the scheduler's contract (refcount).
    let o = unsafe { &*obj };
    o.num_running.fetch_add(1, Ordering::AcqRel);
    o.num_associated.fetch_add(1, Ordering::AcqRel);
    let mut instance = CallbackInstance { object: obj, may_run_long: o.may_run_long, associated: true };

    // A cancelled object still drains its pending count + reference, but its
    // callback is not run (C tp_object_cancel removed it from the queue).
    if !o.cancelled.load(Ordering::Acquire) {
        let inst = &mut instance as *mut CallbackInstance;
        match o.callback {
            Callback::Simple(cb) => cb(inst, o.userdata),
            Callback::Work(cb) => cb(inst, o.userdata, obj.cast()),
            Callback::Timer(cb) => cb(inst, o.userdata, obj.cast()),
            Callback::Wait(cb) => {
                // One callback per submit; if signaled callbacks remain, consume
                // one (WAIT_OBJECT_0), else this is a timeout (C tp_object_execute).
                let prev = o.signaled.fetch_sub(1, Ordering::AcqRel);
                let result = if prev > 0 {
                    WAIT_OBJECT_0
                } else {
                    o.signaled.fetch_add(1, Ordering::AcqRel); // restore; was a timeout
                    WAIT_TIMEOUT
                };
                cb(inst, o.userdata, obj.cast(), result);
            }
            Callback::Io(cb) => {
                // Pop the completion this callback was submitted for (C pops
                // u.io.completions[--completion_count]).
                if let Some((status, information, cvalue)) = o.io_state().pop_completion() {
                    let mut iosb = IoStatusBlock { status, information };
                    cb(inst, o.userdata, cvalue as *mut c_void, &mut iosb as *mut IoStatusBlock, obj.cast());
                }
            }
        }
    }

    o.num_running.fetch_sub(1, Ordering::AcqRel);
    if instance.associated {
        o.num_associated.fetch_sub(1, Ordering::AcqRel);
    }
    // (later: finalization callback + instance cleanup tasks.)
    tp_object_release(obj);
}

/// True once no callbacks are queued or running for `obj` (C `object_is_finished`).
/// `group` waits on running callbacks; otherwise on associated ones (TpDisassociate-
/// Callback drops a callback from the latter only).
fn object_is_finished(obj: *const ThreadpoolObject, group: bool) -> bool {
    let o = unsafe { &*obj };
    let in_flight = if group {
        o.num_running.load(Ordering::Acquire)
    } else {
        o.num_associated.load(Ordering::Acquire)
    };
    o.sched.pending() <= 0 && in_flight <= 0
}

/// Drop one reference; free the object (and unlink it from its cleanup group) when
/// the last goes (the C `tp_object_release`).
///
/// # Safety
/// `obj` must be a live, heap-allocated [`ThreadpoolObject`] (from `Box::into_raw`).
pub fn tp_object_release(obj: *mut ThreadpoolObject) {
    let o = unsafe { &*obj };
    if o.refcount.fetch_sub(1, Ordering::AcqRel) == 1 {
        crate::cleanup_group::unlink_member(obj);
        o.unbind_from_pool(); // objcount-- + pool-ref release (may free the pool)
        drop(unsafe { Box::from_raw(obj) });
    }
}

/// Cancel an object: its still-pending callbacks are skipped by the workers as
/// they drain (the C removes the object from the queue immediately; here a worker
/// dequeues and skips). Needs the pool's workers to be draining (true during
/// cleanup, since the submits that queued the work spawned them).
pub fn tp_object_cancel(obj: *mut ThreadpoolObject) {
    unsafe { &*obj }.cancelled.store(true, Ordering::Release);
}

/// Wait until every queued/running callback of `obj` has finished (the C
/// `tp_object_wait`). Spin-wait for now; a futex/condvar is the refinement.
///
/// # Safety
/// The caller must hold a reference so `obj` stays alive across the wait.
pub fn tp_object_wait(obj: *mut ThreadpoolObject, group: bool) {
    while !object_is_finished(obj, group) {
        core::hint::spin_loop();
    }
}

// ===========================================================================
// Timer queue (stage 2): Rust replacement for the global `timerqueue` +
// timerqueue_thread_proc + tp_timerqueue_lock/unlock + TpSetTimer's scheduling.
//
// A service thread sleeps until the next armed timer's deadline (or until
// set_timer wakes it), fires expired timers via `on_expire` (the wiring points
// this at Pool::submit), and re-arms periodic ones. It stays alive while any
// timer object is bound (objcount), exactly like the C.
//
// Fidelity note: the C drives this with NT waitable-timer kernel objects; here
// it is a portable clock+futex. A lost futex wake can make a freshly-set earlier
// timer fire late, so sleeps are capped (TIMER_MAX_SLEEP) to bound that. Replace
// with NtCreateTimer/NtSetTimer for exactness later.
// ===========================================================================

/// Which clock a timer's deadline is measured against.
#[derive(Clone, Copy, PartialEq)]
pub enum Clock {
    /// Monotonic (relative timeouts; C `rel_timers` / NtQueryPerformanceCounter).
    Mono,
    /// Wall-clock (absolute timeouts; C `abs_timers` / NtQuerySystemTime).
    System,
}

/// One armed timer -- the C `u.timer` scheduling state, owned by the queue.
struct TimerEntry {
    obj: *mut ThreadpoolObject,
    deadline: i64, // absolute, in `clock`'s 100ns units
    clock: Clock,
    period_ms: i32, // 0 = one-shot
    #[allow(dead_code)]
    window_ms: i32, // coalescing window (advisory until update_timers coalescing lands)
}

/// Cap on a single sleep so a lost wake bounds late-firing (and lets an idle
/// thread re-poll cheaply) rather than stalling. 50 ms.
const TIMER_MAX_SLEEP_100NS: i64 = 50 * 10_000;

/// State guarded by `TimerQueue::lock`.
struct TimerInner {
    entries: Vec<TimerEntry>,
    thread_running: bool,
    objcount: i32, // timer objects bound (keeps the service thread alive)
}

/// The global timer service.
pub struct TimerQueue<Os: PoolOs> {
    lock: SpinLock,
    inner: UnsafeCell<TimerInner>,
    /// Futex generation; bumped before each wake (set_timer / obj_removed / shutdown).
    update: AtomicU32,
    shutdown: AtomicBool,
    on_expire: fn(*mut ThreadpoolObject),
    os: Os,
}

// Shared between the service thread and the API callers; all `inner` access is
// under `lock`.
unsafe impl<Os: PoolOs> Sync for TimerQueue<Os> {}
unsafe impl<Os: PoolOs> Send for TimerQueue<Os> {}

extern "system" fn timer_service_entry<Os: PoolOs>(arg: *mut c_void) -> u32 {
    // SAFETY: `arg` is the &TimerQueue passed to spawn; it outlives the thread.
    let tq = unsafe { &*(arg as *const TimerQueue<Os>) };
    tq.run_service();
    0
}

impl<Os: PoolOs> TimerQueue<Os> {
    pub fn new(os: Os, on_expire: fn(*mut ThreadpoolObject)) -> Self {
        TimerQueue {
            lock: SpinLock::new(),
            inner: UnsafeCell::new(TimerInner {
                entries: Vec::new(),
                thread_running: false,
                objcount: 0,
            }),
            update: AtomicU32::new(0),
            shutdown: AtomicBool::new(false),
            on_expire,
            os,
        }
    }

    /// Bind a timer object (C `tp_timerqueue_lock`): ensure the service thread is
    /// running and account the object so the thread stays alive.
    pub fn obj_added(&self) {
        self.lock.lock();
        // SAFETY: inner is only touched under `lock`.
        let inner = unsafe { &mut *self.inner.get() };
        inner.objcount += 1;
        if !inner.thread_running
            && self.os.spawn(timer_service_entry::<Os>, self as *const _ as *mut c_void)
        {
            inner.thread_running = true;
        }
        self.lock.unlock();
    }

    /// Unbind a timer object (C `tp_timerqueue_unlock`); wake the service so it can
    /// exit once the last object is gone.
    pub fn obj_removed(&self) {
        self.lock.lock();
        unsafe { &mut *self.inner.get() }.objcount -= 1;
        self.lock.unlock();
        self.update.fetch_add(1, Ordering::AcqRel);
        self.os.futex_wake_one(&self.update);
    }

    /// Arm / re-arm / disarm a timer (C `TpSetTimer`). `timeout`: `None` disarms;
    /// `Some(v)` with v>0 = absolute (wall clock), v<0 = relative (monotonic),
    /// v==0 = fire immediately (and, if periodic, also re-arm). `period_ms`/
    /// `window_ms` as in TpSetTimer.
    ///
    /// # Safety
    /// `obj` must be a live timer [`ThreadpoolObject`] bound via [`obj_added`].
    pub unsafe fn set_timer(
        &self,
        obj: *mut ThreadpoolObject,
        timeout: Option<i64>,
        period_ms: i32,
        window_ms: i32,
    ) {
        let now_mono = self.os.now_mono_100ns();
        let mut submit_now = false;
        let mut arm: Option<(i64, Clock)> = None;
        match timeout {
            None => {}
            Some(v) if v > 0 => arm = Some((v, Clock::System)),
            Some(v) if v < 0 => arm = Some((now_mono - v, Clock::Mono)),
            Some(_) => {
                // v == 0: fire now; if periodic, also arm relative to now.
                submit_now = true;
                if period_ms != 0 {
                    arm = Some((now_mono + period_ms as i64 * 10_000, Clock::Mono));
                }
            }
        }

        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        if let Some(pos) = inner.entries.iter().position(|e| e.obj == obj) {
            inner.entries.swap_remove(pos);
        }
        if let Some((deadline, clock)) = arm {
            inner.entries.push(TimerEntry { obj, deadline, clock, period_ms, window_ms });
        }
        self.lock.unlock();

        self.update.fetch_add(1, Ordering::AcqRel);
        self.os.futex_wake_one(&self.update);

        if submit_now {
            (self.on_expire)(obj);
        }
    }

    /// The service loop (the Rust `timerqueue_thread_proc`).
    fn run_service(&self) {
        loop {
            self.lock.lock();
            let inner = unsafe { &mut *self.inner.get() };
            let now_mono = self.os.now_mono_100ns();
            let now_sys = self.os.now_system_100ns();

            let mut to_fire: Vec<*mut ThreadpoolObject> = Vec::new();
            let mut i = 0;
            while i < inner.entries.len() {
                let now = if inner.entries[i].clock == Clock::Mono { now_mono } else { now_sys };
                if inner.entries[i].deadline <= now {
                    to_fire.push(inner.entries[i].obj);
                    if inner.entries[i].period_ms != 0 {
                        // Re-arm periodic relative to the monotonic clock (as the C does).
                        inner.entries[i].deadline =
                            now_mono + inner.entries[i].period_ms as i64 * 10_000;
                        inner.entries[i].clock = Clock::Mono;
                        i += 1;
                    } else {
                        inner.entries.swap_remove(i);
                    }
                } else {
                    i += 1;
                }
            }

            let mut sleep = TIMER_MAX_SLEEP_100NS;
            for e in inner.entries.iter() {
                let now = if e.clock == Clock::Mono { now_mono } else { now_sys };
                let rem = (e.deadline - now).max(0);
                if rem < sleep {
                    sleep = rem;
                }
            }

            let terminate = inner.objcount == 0 && inner.entries.is_empty();
            if terminate {
                inner.thread_running = false;
            }
            self.lock.unlock();

            for obj in to_fire {
                (self.on_expire)(obj);
            }

            if terminate {
                return;
            }
            if self.shutdown.load(Ordering::Acquire) {
                self.lock.lock();
                unsafe { &mut *self.inner.get() }.thread_running = false;
                self.lock.unlock();
                return;
            }

            let seen = self.update.load(Ordering::Acquire);
            self.os.futex_wait(&self.update, seen, sleep);
        }
    }

    /// Signal shutdown, wake the service thread, and join it (teardown).
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.update.fetch_add(1, Ordering::AcqRel);
        self.os.futex_wake_all(&self.update);
        self.os.join_all();
    }
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
        fn now_mono_100ns(&self) -> i64 {
            mono_now_100ns()
        }
        fn now_system_100ns(&self) -> i64 {
            system_now_100ns()
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
        fn now_mono_100ns(&self) -> i64 {
            mono_now_100ns()
        }
        fn now_system_100ns(&self) -> i64 {
            system_now_100ns()
        }
    }

    /// Process-monotonic time in 100ns units (host stand-in for the perf counter).
    fn mono_now_100ns() -> i64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        (START.get_or_init(Instant::now).elapsed().as_nanos() / 100) as i64
    }
    /// Wall-clock time in 100ns units (host stand-in for system time).
    fn system_now_100ns() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() / 100) as i64
    }

    /// The dispatch seam (slice 1c plugs in tp_object_execute); here just count.
    fn run_test_obj(o: *mut TestObj) {
        unsafe { (*o).ran.fetch_add(1, Ordering::Relaxed) };
    }

    /// Timer/service on_expire that, like a real submit, bumps the refcount before
    /// dispatch (whose release then balances it), so the test keeps owning the object.
    fn timer_on_expire(obj: *mut ThreadpoolObject) {
        unsafe { (*obj).add_ref() };
        dispatch_object(obj);
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

    /// Timer callback: records its firing through `userdata`.
    extern "system" fn record_timer_cb(
        _inst: *mut CallbackInstance,
        userdata: *mut c_void,
        _timer: *mut c_void,
    ) {
        unsafe { (*(userdata as *const AtomicI32)).fetch_add(1, Ordering::Relaxed) };
    }

    /// Timer queue: a relative one-shot fires once after its deadline, an
    /// immediate timer fires synchronously, a periodic timer keeps firing until
    /// disarmed. `on_expire = dispatch_object`, so this also exercises the Timer
    /// callback dispatch end to end.
    #[test]
    fn timer_relative_immediate_periodic() {
        let fired: Vec<AtomicI32> = (0..3).map(|_| AtomicI32::new(0)).collect();
        let mk = |i: usize| {
            Box::new(ThreadpoolObject::new_timer(
                record_timer_cb,
                &fired[i] as *const AtomicI32 as *mut c_void,
                1,
            ))
        };
        let one_shot = mk(0);
        let immediate = mk(1);
        let periodic = mk(2);

        let tq = Box::new(TimerQueue::new(TestOs::new(), timer_on_expire as fn(*mut ThreadpoolObject)));
        let tq: &TimerQueue<TestOs> = &tq;
        for _ in 0..3 {
            tq.obj_added();
        }

        // Relative one-shot ~20 ms; immediate; periodic every ~5 ms.
        unsafe {
            tq.set_timer(one_shot.as_ptr(), Some(-(20 * 10_000)), 0, 0);
            tq.set_timer(immediate.as_ptr(), Some(0), 0, 0);
            tq.set_timer(periodic.as_ptr(), Some(-(5 * 10_000)), 5, 0);
        }

        // Immediate fires synchronously in set_timer.
        assert_eq!(fired[1].load(Ordering::Relaxed), 1, "immediate timer should fire at once");

        // One-shot fires after its deadline, exactly once.
        spin_until(|| fired[0].load(Ordering::Relaxed) >= 1, "relative one-shot");
        thread::sleep(Duration::from_millis(40));
        assert_eq!(fired[0].load(Ordering::Relaxed), 1, "one-shot fired more than once");

        // Periodic has fired several times by now.
        let p = fired[2].load(Ordering::Relaxed);
        assert!(p >= 3, "periodic under-fired: {p}");

        // Disarm the periodic; it stops (allow one in-flight fire).
        unsafe { tq.set_timer(periodic.as_ptr(), None, 0, 0) };
        let after = fired[2].load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(30));
        let later = fired[2].load(Ordering::Relaxed);
        assert!(later <= after + 1, "periodic kept firing after disarm: {after} -> {later}");

        for _ in 0..3 {
            tq.obj_removed();
        }
        tq.shutdown();
    }
}
