//! Two-lock concurrent MPMC queue (Michael & Scott, "Simple, Fast, and Practical
//! Non-Blocking and Blocking Concurrent Queue Algorithms", PODC'96 -- the blocking
//! two-lock variant).
//!
//! Separate `head` (dequeue) and `tail` (enqueue) locks let producers and
//! consumers proceed without contending *each other* -- the throughput win for
//! the threadpool's submit↔worker path, where today a single `pool->cs`
//! serializes both. Unlike a fully lock-free queue it has no ABA/reclamation
//! hazard, and unlike a single-lock queue producers don't block consumers.
//!
//! It is **intrusive** and **allocation-free**: the caller embeds a [`Node`] in
//! its own object and hands the queue raw node pointers, so it runs in the
//! freestanding PE `ntdll` (which has no C heap). The queue keeps one dummy node
//! so `head` and `tail` never alias while non-empty; each [`dequeue`] returns the
//! payload plus the now-free *previous* node for the caller to recycle
//! (the standard two-lock node hand-off).
//!
//! [`dequeue`]: TwoLockQueue::dequeue

use core::ffi::c_void;
use core::hint::spin_loop;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Intrusive queue link. Embed one in the queued object; the queue only touches
/// `next` (its own bookkeeping) and `value` (the payload set at enqueue time).
#[repr(C)]
pub struct Node {
    next: AtomicPtr<Node>,
    value: *mut c_void,
}

impl Node {
    pub const fn new() -> Self {
        Node { next: AtomicPtr::new(ptr::null_mut()), value: ptr::null_mut() }
    }
}

/// Test-and-test-and-set spinlock. The crate is freestanding (no `std::Mutex`),
/// and a two-lock queue's critical sections are a couple of pointer stores, so a
/// brief spinlock is the right primitive here.
struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        SpinLock { locked: AtomicBool::new(false) }
    }
    #[inline]
    fn lock(&self) {
        while self.locked.swap(true, Ordering::Acquire) {
            while self.locked.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
    }
    #[inline]
    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

/// A two-lock MPMC queue over caller-owned [`Node`]s carrying `*mut c_void`
/// payloads.
pub struct TwoLockQueue {
    head: AtomicPtr<Node>, // dequeue end; guarded by head_lock (atomic only for init/empty reads)
    tail: AtomicPtr<Node>, // enqueue end; guarded by tail_lock
    head_lock: SpinLock,
    tail_lock: SpinLock,
}

// The whole point: safe to share across producer/consumer threads.
unsafe impl Sync for TwoLockQueue {}
unsafe impl Send for TwoLockQueue {}

impl TwoLockQueue {
    /// Create a queue seeded with `dummy` as the initial empty-sentinel node.
    ///
    /// # Safety
    /// `dummy` must point to a live [`Node`] that outlives every queue operation
    /// and is owned by the queue until it is handed back from a later
    /// [`dequeue`](Self::dequeue) (as the recycled node) or via [`drain`](Self::drain).
    pub const unsafe fn with_dummy(dummy: *mut Node) -> Self {
        TwoLockQueue {
            head: AtomicPtr::new(dummy),
            tail: AtomicPtr::new(dummy),
            head_lock: SpinLock::new(),
            tail_lock: SpinLock::new(),
        }
    }

    /// Append `node` (carrying `value`) to the tail. O(1), contends only other
    /// producers.
    ///
    /// # Safety
    /// `node` must point to a live [`Node`] not already in this (or another)
    /// queue; the queue owns it until it is returned from a later dequeue.
    pub unsafe fn enqueue(&self, node: *mut Node, value: *mut c_void) {
        unsafe {
            (*node).next.store(ptr::null_mut(), Ordering::Relaxed);
            (*node).value = value;
        }
        self.tail_lock.lock();
        // SAFETY: tail is only mutated under tail_lock; the previous tail is live.
        let prev = self.tail.load(Ordering::Relaxed);
        unsafe { (*prev).next.store(node, Ordering::Release) };
        self.tail.store(node, Ordering::Relaxed);
        self.tail_lock.unlock();
    }

    /// Remove the oldest payload. Returns `(value, recycled)` where `recycled` is
    /// a now-free [`Node`] the caller owns again (the former dummy), or `None`
    /// when empty. Contends only other consumers.
    pub fn dequeue(&self) -> Option<(*mut c_void, *mut Node)> {
        self.head_lock.lock();
        let head = self.head.load(Ordering::Relaxed);
        // SAFETY: head is a live node (dummy or a previously enqueued one).
        let next = unsafe { (*head).next.load(Ordering::Acquire) };
        if next.is_null() {
            self.head_lock.unlock();
            return None;
        }
        // The payload lives in `next`, which becomes the new dummy; `head` (the old
        // dummy) is now free to hand back.
        let value = unsafe { (*next).value };
        self.head.store(next, Ordering::Relaxed);
        self.head_lock.unlock();
        Some((value, head))
    }

    /// Best-effort emptiness peek (non-destructive). A freshly enqueued node is
    /// published via `prev->next` with `Release`, so an `Acquire` load of
    /// `head->next` observes it; racing with a concurrent dequeue advancing
    /// `head` only affects liveness, not the "is there work?" answer this is used
    /// for (the worker's terminate guard).
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        // SAFETY: head is always a live node (the current dummy).
        unsafe { (*head).next.load(Ordering::Acquire).is_null() }
    }

    /// Pop every remaining payload, invoking `f(value)` for each, and return the
    /// final dummy node so the caller can reclaim it. Single-threaded teardown
    /// helper (no concurrent ops allowed).
    ///
    /// # Safety
    /// Must not run concurrently with `enqueue`/`dequeue`.
    pub unsafe fn drain(&self, mut f: impl FnMut(*mut c_void)) -> *mut Node {
        while let Some((value, _recycled)) = self.dequeue() {
            f(value);
        }
        self.head.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate alloc;
    extern crate std;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use core::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

    fn boxed_node() -> *mut Node {
        Box::into_raw(Box::new(Node::new()))
    }
    unsafe fn free_node(n: *mut Node) {
        drop(unsafe { Box::from_raw(n) });
    }

    #[test]
    fn fifo_single_threaded() {
        let q = unsafe { TwoLockQueue::with_dummy(boxed_node()) };
        // Enqueue 1,2,3 then dequeue and confirm FIFO order + node recycling.
        let mut recycled = Vec::new();
        for v in 1..=3usize {
            unsafe { q.enqueue(boxed_node(), v as *mut c_void) };
        }
        for expect in 1..=3usize {
            let (value, rec) = q.dequeue().expect("non-empty");
            assert_eq!(value as usize, expect);
            recycled.push(rec);
        }
        assert!(q.dequeue().is_none(), "drained");
        // Reclaim everything (recycled nodes + the final dummy).
        for n in recycled {
            unsafe { free_node(n) };
        }
        unsafe { free_node(q.head.load(Ordering::Relaxed)) };
    }

    #[test]
    fn empty_returns_none() {
        let dummy = boxed_node();
        let q = unsafe { TwoLockQueue::with_dummy(dummy) };
        assert!(q.dequeue().is_none());
        assert!(q.dequeue().is_none());
        unsafe { free_node(dummy) };
    }

    /// Many producers and many consumers hammer the queue; every enqueued value
    /// must be dequeued exactly once (sum check) and no node leaks/double-frees.
    #[test]
    fn mpmc_stress_conserves_all_items() {
        const PRODUCERS: usize = 4;
        const CONSUMERS: usize = 4;
        const PER_PRODUCER: usize = 20_000;
        let total = PRODUCERS * PER_PRODUCER;

        let q = Arc::new(unsafe { TwoLockQueue::with_dummy(boxed_node()) });
        let consumed = Arc::new(AtomicUsize::new(0));
        let sum = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));

        let mut handles = Vec::new();
        for p in 0..PRODUCERS {
            let q = q.clone();
            handles.push(thread::spawn(move || {
                for i in 0..PER_PRODUCER {
                    // value = 1-based unique-ish payload; sum is deterministic.
                    let v = (p * PER_PRODUCER + i + 1) as *mut c_void;
                    unsafe { q.enqueue(boxed_node(), v) };
                }
            }));
        }
        for _ in 0..CONSUMERS {
            let q = q.clone();
            let consumed = consumed.clone();
            let sum = sum.clone();
            let done = done.clone();
            handles.push(thread::spawn(move || {
                loop {
                    match q.dequeue() {
                        Some((value, recycled)) => {
                            sum.fetch_add(value as usize, Ordering::Relaxed);
                            consumed.fetch_add(1, Ordering::Relaxed);
                            unsafe { free_node(recycled) };
                        }
                        None => {
                            if done.load(Ordering::Acquire) && q.dequeue().is_none() {
                                break;
                            }
                            spin_loop();
                        }
                    }
                }
            }));
        }
        // wait for producers, then signal consumers to drain and exit.
        for h in handles.drain(..PRODUCERS) {
            h.join().unwrap();
        }
        done.store(true, Ordering::Release);
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(consumed.load(Ordering::Relaxed), total, "every item dequeued once");
        // sum of 1..=total
        let expected_sum: usize = (1..=total).sum();
        assert_eq!(sum.load(Ordering::Relaxed), expected_sum, "no lost/duplicated items");
        // free the trailing dummy
        unsafe { free_node(q.head.load(Ordering::Relaxed)) };
    }
}
