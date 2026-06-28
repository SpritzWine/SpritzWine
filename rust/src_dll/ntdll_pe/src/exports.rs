//! The ntdll `Tp*` / `Rtl*` threadpool entry points (`#[no_mangle] extern
//! "system"`), implemented over the host-tested Rust threadpool + [`WineOs`].
//! Real build only (`#[cfg(not(test))]` on the `mod` in lib.rs); the export bodies
//! use the FFI backend, so they are compile-checked here and exercised by the wine
//! conformance suite after the swap (phase 3), not by host unit tests.
//!
//! This is phase 2a: the pool + work/simple path. Timers, waits, io, cleanup
//! groups and the instance API follow in the same file.

use crate::cleanup_group::Group;
use crate::threadpool::{
    dispatch_object, tp_object_cancel, tp_object_release, tp_object_wait, Pool, PoolOps,
    SimpleCallback, ThreadpoolObject, WorkCallback,
};
use crate::wine_os::WineOs;
use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

/// The only pool type in the real build.
type WinePool = Pool<ThreadpoolObject, WineOs>;

const STATUS_SUCCESS: i32 = 0;
/// TP_CALLBACK_PRIORITY_NORMAL.
const PRIORITY_NORMAL: u8 = 1;

// --- pool ops vtable: how a (non-generic) object reaches its WinePool ----------

unsafe fn wine_submit(pool: *const c_void, obj: *mut ThreadpoolObject) {
    unsafe { (*(pool as *const WinePool)).submit(obj) };
}
unsafe fn wine_unbind(pool: *const c_void) {
    let p = pool as *const WinePool;
    unsafe {
        (*p).obj_removed();
        if (*p).release_ref() {
            drop(Box::from_raw(p as *mut WinePool));
        }
    }
}
static WINE_POOL_OPS: PoolOps = PoolOps { submit: wine_submit, unbind: wine_unbind };

// --- lazy global default pool (C default_threadpool) --------------------------

static DEFAULT_POOL: AtomicPtr<WinePool> = AtomicPtr::new(ptr::null_mut());

fn default_pool() -> *mut WinePool {
    let existing = DEFAULT_POOL.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(Pool::new(WineOs, dispatch_object)));
    match DEFAULT_POOL.compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh,
        Err(other) => {
            drop(unsafe { Box::from_raw(fresh) });
            other
        }
    }
}

// --- TP_CALLBACK_ENVIRON parsing ----------------------------------------------

/// TP_CALLBACK_ENVIRON_V3 (older versions are a prefix; we only read fields valid
/// for their version).
#[repr(C)]
struct TpCallbackEnviron {
    version: u32,
    pool: *mut WinePool,
    cleanup_group: *mut Group,
    cleanup_group_cancel_callback: Option<extern "system" fn(*mut c_void, *mut c_void)>,
    race_dll: *mut c_void,
    activation_context: *mut c_void,
    finalization_callback: *mut c_void,
    flags: u32,
    callback_priority: u32,
    size: u32,
}

struct EnvInfo {
    pool: *mut WinePool,
    priority: u8,
    group: *const Group,
    cancel_cb: Option<extern "system" fn(*mut c_void, *mut c_void)>,
}

unsafe fn parse_env(env: *const c_void) -> EnvInfo {
    if env.is_null() {
        return EnvInfo { pool: default_pool(), priority: PRIORITY_NORMAL, group: ptr::null(), cancel_cb: None };
    }
    let e = unsafe { &*(env as *const TpCallbackEnviron) };
    let pool = if e.pool.is_null() { default_pool() } else { e.pool };
    // CallbackPriority is only present from version 3.
    let priority = if e.version >= 3 { (e.callback_priority as u8).min(2) } else { PRIORITY_NORMAL };
    EnvInfo {
        pool,
        priority,
        group: e.cleanup_group as *const Group,
        cancel_cb: e.cleanup_group_cancel_callback,
    }
}

/// Bind a freshly-created object to its pool (+ group): account it, take a pool
/// reference, ensure a worker, install the pool ops, join the cleanup group.
/// (The C `tp_threadpool_lock` + `tp_object_initialize`.)
unsafe fn bind_object(obj: *mut ThreadpoolObject, info: &EnvInfo) {
    unsafe {
        (*info.pool).obj_added();
        (*info.pool).add_ref();
        (*info.pool).ensure_started();
        (*obj).set_pool(info.pool as *const c_void, &WINE_POOL_OPS);
        if !info.group.is_null() {
            (*obj).set_group_cancel_callback(info.cancel_cb);
            (*info.group).add_member(obj);
        }
    }
}

// --- pool exports -------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocPool(out: *mut *mut WinePool, _reserved: *mut c_void) -> i32 {
    let pool = Box::into_raw(Box::new(Pool::new(WineOs, dispatch_object)));
    unsafe { *out = pool };
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleasePool(pool: *mut WinePool) {
    unsafe {
        (*pool).shutdown(); // wake all workers (WineOs join_all is a no-op)
        if (*pool).release_ref() {
            drop(Box::from_raw(pool));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpSetPoolMinThreads(pool: *mut WinePool, minimum: u32) -> i32 {
    unsafe { (*pool).set_min_workers(minimum as i32) };
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpSetPoolMaxThreads(pool: *mut WinePool, maximum: u32) {
    unsafe { (*pool).set_max_workers(maximum.max(1) as i32) };
}

// --- work exports -------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocWork(
    out: *mut *mut ThreadpoolObject,
    callback: WorkCallback,
    userdata: *mut c_void,
    environment: *const c_void,
) -> i32 {
    unsafe {
        let info = parse_env(environment);
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_work(callback, userdata, info.priority)));
        bind_object(obj, &info);
        *out = obj;
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpPostWork(work: *mut ThreadpoolObject) {
    unsafe { (*work).submit_to_pool() };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseWork(work: *mut ThreadpoolObject) {
    // Drop the base reference; the allocation frees once outstanding callbacks
    // drain (each holds a reference).
    tp_object_release(work);
}

#[unsafe(no_mangle)]
pub extern "system" fn TpWaitForWork(work: *mut ThreadpoolObject, cancel_pending: u8) {
    if cancel_pending != 0 {
        tp_object_cancel(work);
    }
    tp_object_wait(work);
}

// --- simple callbacks ---------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn TpSimpleTryPost(
    callback: SimpleCallback,
    userdata: *mut c_void,
    environment: *const c_void,
) -> i32 {
    unsafe {
        let info = parse_env(environment);
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_simple(callback, userdata, info.priority)));
        bind_object(obj, &info);
        (*obj).submit_to_pool();
        // A simple callback has no TpRelease; drop its base reference now so it
        // frees once the (single) callback has run.
        tp_object_release(obj);
    }
    STATUS_SUCCESS
}
