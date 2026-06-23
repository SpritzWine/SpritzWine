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
use core::sync::atomic::{AtomicI32, Ordering};

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
        }
    }

    /// Append `obj` to its priority queue on a fresh carrier node.
    ///
    /// # Safety
    /// `obj` must satisfy the [`Scheduled`] contract.
    unsafe fn enqueue<T: Scheduled>(&self, obj: *mut T) {
        let prio = unsafe { (*obj).sched_state().priority } as usize;
        debug_assert!(prio < PRIORITY_COUNT);
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

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use core::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;
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
                // spins so a bug fails instead of deadlocking the test.
                let mut idle: u64 = 0;
                loop {
                    match unsafe { sched.worker_take::<TestObj>() } {
                        Some(p) => {
                            unsafe { (*p).ran.fetch_add(1, Ordering::Relaxed) };
                            idle = 0;
                        }
                        None => {
                            if done.load(Ordering::Acquire) {
                                // Final confirm-empty pass.
                                if unsafe { sched.worker_take::<TestObj>() }.is_none() {
                                    break;
                                }
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
}
