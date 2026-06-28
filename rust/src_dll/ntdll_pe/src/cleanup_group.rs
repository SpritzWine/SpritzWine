//! Cleanup groups (stage 5): Rust replacement for `struct threadpool_group` +
//! TpAllocCleanupGroup / TpReleaseCleanupGroup / TpReleaseCleanupGroupMembers,
//! plus the object refcount lifecycle they drive (which lives in `threadpool`:
//! [`tp_object_release`](crate::threadpool::tp_object_release) /
//! [`tp_object_cancel`](crate::threadpool::tp_object_cancel) /
//! [`tp_object_wait`](crate::threadpool::tp_object_wait)).
//!
//! A group owns its member objects; releasing the members cancels (optionally),
//! waits for in-flight callbacks, runs each object's group-cancel callback, and
//! releases the objects -- destroying the whole group at once. Group membership is
//! a plain object back-pointer + a flag; the group keeps the member list.

use crate::threadpool::{tp_object_cancel, tp_object_release, tp_object_wait, ThreadpoolObject};
use crate::work_queue::SpinLock;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicI32, Ordering};

struct GroupInner {
    members: Vec<*mut ThreadpoolObject>,
}

/// The C `struct threadpool_group`.
pub struct Group {
    lock: SpinLock,
    inner: UnsafeCell<GroupInner>,
    refcount: AtomicI32,
}

unsafe impl Sync for Group {}
unsafe impl Send for Group {}

impl Group {
    pub fn new() -> Self {
        Group {
            lock: SpinLock::new(),
            inner: UnsafeCell::new(GroupInner { members: Vec::new() }),
            refcount: AtomicI32::new(1),
        }
    }

    /// Add an object to the group at init (the C `tp_object_initialize` group
    /// branch).
    ///
    /// # Safety
    /// `obj` must be a live object not yet shared/submitted (so writing its plain
    /// `group` back-pointer is race-free).
    pub unsafe fn add_member(&self, obj: *mut ThreadpoolObject) {
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        unsafe { (*obj).group = self as *const Group };
        unsafe { &*obj }.is_group_member.store(true, Ordering::Release);
        inner.members.push(obj);
        self.lock.unlock();
        // The group's own refcount is its handle lifetime (1 until release); members
        // are tracked by the list, not by group references.
    }

    /// The C `TpReleaseCleanupGroupMembers`: detach all members, optionally cancel
    /// their pending callbacks, wait for in-flight ones, run each group-cancel
    /// callback (on a cancel), and release the objects (freeing them).
    pub fn release_members(&self, cancel_pending: bool, userdata: *mut c_void) {
        // Detach the whole member list under the lock, unsetting membership so the
        // later release's unlink is a no-op (matches the C's "unset under cs, then
        // process outside"). Take a temp reference on each to keep it alive.
        self.lock.lock();
        let inner = unsafe { &mut *self.inner.get() };
        let members: Vec<*mut ThreadpoolObject> = core::mem::take(&mut inner.members);
        for &m in members.iter() {
            let o = unsafe { &*m };
            o.is_group_member.store(false, Ordering::Release);
            o.add_ref();
        }
        self.lock.unlock();

        if cancel_pending {
            for &m in members.iter() {
                tp_object_cancel(m);
            }
        }

        for m in members {
            tp_object_wait(m, true); // wait for in-flight callbacks
            let o = unsafe { &*m };
            // A shutdown object (its callback already ran, or it was TpRelease*'d)
            // gets neither the cancel callback nor a base release here (C gates both
            // on !object->shutdown); only a still-pending/cancelled object does.
            if !o.is_shutdown() {
                if cancel_pending {
                    if let Some(cb) = o.group_cancel_callback {
                        cb(o.userdata(), userdata);
                    }
                }
                // A simple object already dropped its own base ref at TpSimpleTryPost,
                // so the group must not (C skips TP_OBJECT_TYPE_SIMPLE).
                if !o.is_simple() {
                    tp_object_release(m);
                }
            }
            tp_object_release(m); // the temp reference -> frees the object
        }
    }

    /// The C `tp_group_release`; frees the group when the last reference drops.
    pub fn release(&self) -> bool {
        self.refcount.fetch_sub(1, Ordering::AcqRel) == 1
    }
}

impl Default for Group {
    fn default() -> Self {
        Self::new()
    }
}

/// Remove `obj` from its cleanup group if it is still a member (called from
/// [`tp_object_release`](crate::threadpool::tp_object_release) as the object is
/// freed). A no-op once `release_members` has detached it.
pub fn unlink_member(obj: *mut ThreadpoolObject) {
    let o = unsafe { &*obj };
    if o.is_group_member.load(Ordering::Acquire) && !o.group.is_null() {
        let group = unsafe { &*o.group };
        group.lock.lock();
        let inner = unsafe { &mut *group.inner.get() };
        if let Some(p) = inner.members.iter().position(|&m| m == obj) {
            inner.members.swap_remove(p);
        }
        group.lock.unlock();
        o.is_group_member.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threadpool::ThreadpoolObject;
    extern crate std;
    use alloc::boxed::Box;
    use core::sync::atomic::AtomicI32;
    use std::vec::Vec;

    extern "system" fn noop_work(
        _inst: *mut crate::threadpool::CallbackInstance,
        _userdata: *mut c_void,
        _work: *mut c_void,
    ) {
    }

    /// Cancel callback: records (via the object's userdata) that it ran.
    extern "system" fn cancel_cb(object_userdata: *mut c_void, _cleanup: *mut c_void) {
        unsafe { (*(object_userdata as *const AtomicI32)).fetch_add(1, Ordering::Relaxed) };
    }

    /// Members with no pending work: releasing the group (with cancel) runs each
    /// object's group-cancel callback and frees the objects. Run under a leak
    /// checker mentally: the objects are `Box::into_raw`'d and reclaimed by
    /// `tp_object_release` (no double free, since the test doesn't own them).
    #[test]
    fn release_members_runs_cancel_callback_and_frees() {
        const N: usize = 5;
        let counters: Vec<AtomicI32> = (0..N).map(|_| AtomicI32::new(0)).collect();
        let group = Group::new();

        for i in 0..N {
            let mut obj = ThreadpoolObject::new_work(
                noop_work,
                &counters[i] as *const AtomicI32 as *mut c_void,
                1,
            );
            obj.set_group_cancel_callback(Some(cancel_cb));
            let raw = Box::into_raw(Box::new(obj));
            unsafe { group.add_member(raw) };
        }

        // Cancel + release: each object's cancel callback fires, then it is freed.
        group.release_members(true, core::ptr::null_mut());

        for (i, c) in counters.iter().enumerate() {
            assert_eq!(c.load(Ordering::Relaxed), 1, "object {i} cancel callback should run once");
        }
        assert!(group.release(), "group's last reference should free it");
    }

    /// Without cancel, releasing the members frees them but runs no cancel callback.
    #[test]
    fn release_members_no_cancel() {
        let counter = AtomicI32::new(0);
        let group = Group::new();
        let mut obj =
            ThreadpoolObject::new_work(noop_work, &counter as *const AtomicI32 as *mut c_void, 1);
        obj.set_group_cancel_callback(Some(cancel_cb));
        let raw = Box::into_raw(Box::new(obj));
        unsafe { group.add_member(raw) };

        group.release_members(false, core::ptr::null_mut());
        assert_eq!(counter.load(Ordering::Relaxed), 0, "no cancel -> no cancel callback");
    }

    /// Regression (conformance test_tp_group_cancel 1028): a *shutdown* member -- its
    /// callback already ran, or it was TpRelease*'d -- must NOT get the group cancel
    /// callback even on a cancelling release, unlike a still-pending member (which
    /// `release_members_runs_cancel_callback_and_frees` covers).
    #[test]
    fn release_members_skips_cancel_for_shutdown_member() {
        let counter = AtomicI32::new(0);
        let group = Group::new();
        let mut obj =
            ThreadpoolObject::new_work(noop_work, &counter as *const AtomicI32 as *mut c_void, 1);
        obj.set_group_cancel_callback(Some(cancel_cb));
        let raw = Box::into_raw(Box::new(obj));
        unsafe {
            group.add_member(raw);
            (*raw).set_shutdown(); // as if TpReleaseWork / a finished simple callback ran
        }

        // cancel = true, but the shutdown member must be left alone.
        group.release_members(true, core::ptr::null_mut());
        assert_eq!(counter.load(Ordering::Relaxed), 0, "shutdown member: no group cancel callback");

        // release_members skipped the shutdown member's base ref (it would have been
        // dropped by TpReleaseWork); drop it now to free the object.
        crate::threadpool::tp_object_release(raw);
    }
}
