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
    dispatch_object, tp_object_cancel, tp_object_release, tp_object_wait, CallbackInstance, Pool,
    PoolOps, RtlWaitOrTimerCallback, SimpleCallback, ThreadpoolObject, TimerCallback, TimerQueue,
    WaitCallback, WorkCallback, WAIT_TIMEOUT,
};
use crate::io_queue::IoQueue;
use crate::threadpool::IoCallback;
use crate::wait_queue::WaitQueue;
use crate::wine_os::WineOs;
use alloc::boxed::Box;
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

/// The only pool type in the real build.
type WinePool = Pool<ThreadpoolObject, WineOs>;

const STATUS_SUCCESS: i32 = 0;
const STATUS_PENDING: i32 = 0x103;
const STATUS_TOO_MANY_THREADS: i32 = 0xC000_0129u32 as i32;
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
    let (reserve, commit) = image_default_stack();
    unsafe { (*fresh).set_stack_info(reserve, commit) };
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
    finalization: Option<SimpleCallback>,
}

unsafe fn parse_env(env: *const c_void) -> EnvInfo {
    if env.is_null() {
        return EnvInfo {
            pool: default_pool(),
            priority: PRIORITY_NORMAL,
            group: ptr::null(),
            cancel_cb: None,
            finalization: None,
        };
    }
    let e = unsafe { &*(env as *const TpCallbackEnviron) };
    let pool = if e.pool.is_null() { default_pool() } else { e.pool };
    // CallbackPriority is only present from version 3.
    let priority = if e.version >= 3 { (e.callback_priority as u8).min(2) } else { PRIORITY_NORMAL };
    let finalization = if e.finalization_callback.is_null() {
        None
    } else {
        Some(unsafe { core::mem::transmute::<*mut c_void, SimpleCallback>(e.finalization_callback) })
    };
    EnvInfo {
        pool,
        priority,
        group: e.cleanup_group as *const Group,
        cancel_cb: e.cleanup_group_cancel_callback,
        finalization,
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
        (*obj).set_finalization_callback(info.finalization);
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
    let (reserve, commit) = image_default_stack();
    unsafe {
        (*pool).set_stack_info(reserve, commit);
        *out = pool;
    }
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
    tp_object_wait(work, false);
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

// --- timers -------------------------------------------------------------------

type WineTimerQueue = TimerQueue<WineOs>;
static TIMER_QUEUE: AtomicPtr<WineTimerQueue> = AtomicPtr::new(ptr::null_mut());

/// A fired timer is submitted to its pool (the C submit_expired_timers path).
fn timer_expire(obj: *mut ThreadpoolObject) {
    unsafe { (*obj).submit_to_pool() };
}

fn timer_queue() -> *mut WineTimerQueue {
    let existing = TIMER_QUEUE.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(TimerQueue::new(WineOs, timer_expire)));
    match TIMER_QUEUE.compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh,
        Err(other) => {
            drop(unsafe { Box::from_raw(fresh) });
            other
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocTimer(
    out: *mut *mut ThreadpoolObject,
    callback: TimerCallback,
    userdata: *mut c_void,
    environment: *const c_void,
) -> i32 {
    unsafe {
        let info = parse_env(environment);
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_timer(callback, userdata, info.priority)));
        bind_object(obj, &info);
        (*timer_queue()).obj_added();
        *out = obj;
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpSetTimer(
    timer: *mut ThreadpoolObject,
    timeout: *const i64,
    period: i32,
    window_length: i32,
) {
    unsafe {
        let to = if timeout.is_null() { None } else { Some(*timeout) };
        (*timer).timer_set.store(!timeout.is_null(), Ordering::Release);
        (*timer_queue()).set_timer(timer, to, period, window_length);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpIsTimerSet(timer: *mut ThreadpoolObject) -> u8 {
    unsafe { (*timer).timer_set.load(Ordering::Acquire) as u8 }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpWaitForTimer(timer: *mut ThreadpoolObject, cancel_pending: u8) {
    if cancel_pending != 0 {
        tp_object_cancel(timer);
    }
    tp_object_wait(timer, false);
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseTimer(timer: *mut ThreadpoolObject) {
    unsafe {
        (*timer_queue()).set_timer(timer, None, 0, 0); // disarm before free
        (*timer_queue()).obj_removed();
        tp_object_release(timer);
    }
}

// --- waits --------------------------------------------------------------------

type WineWaitQueue = WaitQueue<WineOs>;
static WAIT_QUEUE: AtomicPtr<WineWaitQueue> = AtomicPtr::new(ptr::null_mut());

const INFINITE: u32 = 0xFFFF_FFFF;
const WT_EXECUTEINIOTHREAD: u32 = 0x0000_0001;
const WT_EXECUTEONLYONCE: u32 = 0x0000_0008;

/// A signaled/timed-out wait is submitted to its pool (signaled bumps the count
/// so the dispatch yields WAIT_OBJECT_0).
fn wait_signaled(obj: *mut ThreadpoolObject, signaled: bool) {
    unsafe {
        if signaled {
            (*obj).signaled.fetch_add(1, Ordering::AcqRel);
        }
        (*obj).submit_to_pool();
    }
}

fn wait_queue() -> *mut WineWaitQueue {
    let existing = WAIT_QUEUE.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(WaitQueue::new(WineOs, wait_signaled)));
    match WAIT_QUEUE.compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh,
        Err(other) => {
            drop(unsafe { Box::from_raw(fresh) });
            other
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocWait(
    out: *mut *mut ThreadpoolObject,
    callback: WaitCallback,
    userdata: *mut c_void,
    environment: *const c_void,
) -> i32 {
    unsafe {
        let info = parse_env(environment);
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_wait(callback, userdata, info.priority)));
        bind_object(obj, &info);
        (*wait_queue()).obj_added(obj, false);
        *out = obj;
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpSetWait(wait: *mut ThreadpoolObject, handle: *mut c_void, timeout: *const i64) {
    unsafe {
        let to = if timeout.is_null() { None } else { Some(*timeout) };
        (*wait_queue()).set_wait(wait, handle as u64, to, false);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpWaitForWait(wait: *mut ThreadpoolObject, cancel_pending: u8) {
    if cancel_pending != 0 {
        tp_object_cancel(wait);
    }
    tp_object_wait(wait, false);
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseWait(wait: *mut ThreadpoolObject) {
    unsafe {
        (*wait_queue()).obj_removed(wait);
        tp_object_release(wait);
    }
}

// --- old RTL wait wrappers (built on the Tp* wait queue) ----------------------

/// Bridges a Tp* wait callback to the RTL_WAITORTIMERCALLBACKFUNC (the C
/// `rtl_wait_callback`): the 2nd arg is TRUE on timeout, FALSE on signal.
extern "system" fn rtl_wait_bridge(
    _inst: *mut CallbackInstance,
    userdata: *mut c_void,
    wait: *mut c_void,
    result: u32,
) {
    let obj = wait as *const ThreadpoolObject;
    if let Some(cb) = unsafe { (*obj).rtl_wait_callback } {
        cb(userdata, (result == WAIT_TIMEOUT) as u8);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn RtlRegisterWait(
    out: *mut *mut ThreadpoolObject,
    handle: *mut c_void,
    callback: RtlWaitOrTimerCallback,
    context: *mut c_void,
    milliseconds: u32,
    flags: u32,
) -> i32 {
    unsafe {
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_wait(rtl_wait_bridge, context, PRIORITY_NORMAL)));
        (*obj).set_rtl_wait_callback(callback);
        let info = EnvInfo { pool: default_pool(), priority: PRIORITY_NORMAL, group: ptr::null(), cancel_cb: None, finalization: None };
        bind_object(obj, &info);
        let alertable = (flags & WT_EXECUTEINIOTHREAD) != 0;
        (*wait_queue()).obj_added(obj, alertable);
        let to = if milliseconds == INFINITE { None } else { Some(-(milliseconds as i64 * 10_000)) };
        // Without WT_EXECUTEONLYONCE the wait re-fires on every signal.
        let repeating = (flags & WT_EXECUTEONLYONCE) == 0;
        (*wait_queue()).set_wait(obj, handle as u64, to, repeating);
        *out = obj;
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn RtlDeregisterWaitEx(handle: *mut ThreadpoolObject, completion_event: *mut c_void) -> i32 {
    unsafe {
        // Disarm + unbind so no further callbacks are queued.
        (*wait_queue()).obj_removed(handle);
        // Completion-event arg: INVALID_HANDLE_VALUE (-1) = block until the in-flight
        // callback finishes (returns SUCCESS); NULL = don't block (returns PENDING if
        // one is still running); a real event = don't block, signal it when done.
        let is_invalid = completion_event as usize == usize::MAX;
        let running = (*handle).num_running() > 0;
        let status = if !running {
            // Nothing in flight: signal a provided completion event right away.
            if !completion_event.is_null() && !is_invalid {
                signal_event(completion_event as u64);
            }
            STATUS_SUCCESS
        } else if is_invalid {
            tp_object_wait(handle, false);
            STATUS_SUCCESS
        } else {
            if !completion_event.is_null() {
                (*handle).set_completion_event(completion_event as u64);
            }
            STATUS_PENDING
        };
        tp_object_release(handle);
        status
    }
}

/// Signal a wait/event handle (used by dispatch_object for a deregister completion
/// event). Best-effort; a stale handle is harmless here.
pub(crate) fn signal_event(event: u64) {
    unsafe {
        NtSetEvent(event as *mut c_void, ptr::null_mut());
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn RtlDeregisterWait(handle: *mut ThreadpoolObject) -> i32 {
    RtlDeregisterWaitEx(handle, ptr::null_mut())
}

// --- I/O completion -----------------------------------------------------------

type WineIoQueue = IoQueue<WineOs>;
static IO_QUEUE: AtomicPtr<WineIoQueue> = AtomicPtr::new(ptr::null_mut());

fn io_submit(obj: *mut ThreadpoolObject) {
    unsafe { (*obj).submit_to_pool() };
}

fn io_queue() -> *mut WineIoQueue {
    let existing = IO_QUEUE.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(IoQueue::new(WineOs, io_submit)));
    match IO_QUEUE.compare_exchange(ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh,
        Err(other) => {
            drop(unsafe { Box::from_raw(fresh) });
            other
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocIoCompletion(
    out: *mut *mut ThreadpoolObject,
    file: *mut c_void,
    callback: IoCallback,
    userdata: *mut c_void,
    environment: *const c_void,
) -> i32 {
    unsafe {
        let info = parse_env(environment);
        let obj = Box::into_raw(Box::new(ThreadpoolObject::new_io(callback, userdata, info.priority)));
        bind_object(obj, &info);
        (*io_queue()).obj_added(obj, file as u64);
        *out = obj;
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpStartAsyncIoOperation(io: *mut ThreadpoolObject) {
    unsafe { (*io).io_state().start_async() };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCancelAsyncIoOperation(io: *mut ThreadpoolObject) {
    unsafe { (*io).io_state().cancel_async() };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpWaitForIoCompletion(io: *mut ThreadpoolObject, cancel_pending: u8) {
    if cancel_pending != 0 {
        tp_object_cancel(io);
    }
    tp_object_wait(io, false);
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseIoCompletion(io: *mut ThreadpoolObject) {
    unsafe {
        (*io_queue()).obj_removed();
        tp_object_release(io);
    }
}

// --- cleanup groups -----------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn TpAllocCleanupGroup(out: *mut *mut Group) -> i32 {
    unsafe { *out = Box::into_raw(Box::new(Group::new())) };
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseCleanupGroup(group: *mut Group) {
    unsafe {
        if (*group).release() {
            drop(Box::from_raw(group));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpReleaseCleanupGroupMembers(
    group: *mut Group,
    cancel_pending: u8,
    userdata: *mut c_void,
) {
    unsafe { (*group).release_members(cancel_pending != 0, userdata) };
}

// --- callback instance API ----------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackMayRunLong(instance: *mut CallbackInstance) -> i32 {
    unsafe {
        let inst = &mut *instance;
        if inst.may_run_long {
            return STATUS_SUCCESS; // already promoted; idempotent
        }
        inst.may_run_long = true;
        let obj = inst.object;
        if obj.is_null() {
            return STATUS_SUCCESS;
        }
        let pool = (*obj).pool as *const WinePool;
        if pool.is_null() {
            return STATUS_SUCCESS;
        }
        // At capacity there is no spare worker to free up -> the caller must not
        // run long (the C returns STATUS_TOO_MANY_THREADS); otherwise ensure a
        // worker stays available for the rest of the pool's work.
        if (*pool).busy() >= (*pool).max_workers() {
            return STATUS_TOO_MANY_THREADS;
        }
        (*pool).ensure_started();
        STATUS_SUCCESS
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpDisassociateCallback(instance: *mut CallbackInstance) {
    unsafe { (*instance).disassociate() };
}

// --- pool stack information ---------------------------------------------------

#[repr(C)]
struct TpPoolStackInfo {
    reserve: usize,
    commit: usize,
}

#[unsafe(no_mangle)]
pub extern "system" fn TpSetPoolStackInformation(pool: *mut WinePool, info: *const c_void) -> i32 {
    unsafe {
        let si = &*(info as *const TpPoolStackInfo);
        (*pool).set_stack_info(si.reserve, si.commit);
    }
    STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub extern "system" fn TpQueryPoolStackInformation(pool: *mut WinePool, info: *mut c_void) -> i32 {
    unsafe {
        let (reserve, commit) = (*pool).stack_info();
        let si = &mut *(info as *mut TpPoolStackInfo);
        si.reserve = reserve;
        si.commit = commit;
    }
    STATUS_SUCCESS
}

// --- RtlQueueWorkItem (old RTL work API, over TpSimpleTryPost) -----------------

struct RtlWorkItem {
    function: extern "system" fn(*mut c_void),
    context: *mut c_void,
}

extern "system" fn rtl_work_bridge(_inst: *mut CallbackInstance, userdata: *mut c_void) {
    let item = unsafe { Box::from_raw(userdata as *mut RtlWorkItem) };
    (item.function)(item.context); // item freed on drop
}

#[unsafe(no_mangle)]
pub extern "system" fn RtlQueueWorkItem(
    function: extern "system" fn(*mut c_void),
    context: *mut c_void,
    _flags: u32,
) -> i32 {
    let item = Box::into_raw(Box::new(RtlWorkItem { function, context }));
    TpSimpleTryPost(rtl_work_bridge, item as *mut c_void, ptr::null())
}

// --- TpCallback*OnCompletion (instance cleanup actions) -----------------------

unsafe extern "system" {
    fn NtSetEvent(handle: *mut c_void, prev: *mut i32) -> i32;
    fn NtReleaseSemaphore(handle: *mut c_void, count: i32, prev: *mut i32) -> i32;
    fn NtReleaseMutant(handle: *mut c_void, prev: *mut i32) -> i32;
    fn RtlLeaveCriticalSection(cs: *mut c_void) -> i32;
    fn LdrUnloadDll(module: *mut c_void) -> i32;
    fn RtlImageNtHeader(base: *const c_void) -> *const u8;
}
// No args -> cdecl/stdcall are identical; match heap.rs's declaration (extern "C")
// to avoid a redeclaration-signature warning.
unsafe extern "C" {
    fn RtlGetCurrentPeb() -> *mut c_void;
}

/// The image's default thread stack (`OptionalHeader.SizeOfStackReserve`/`Commit`),
/// which a new pool reports until TpSetPoolStackInformation overrides it (the C
/// reads these from the main image at pool creation). Returns (0, 0) if unavailable.
fn image_default_stack() -> (usize, usize) {
    unsafe {
        let peb = RtlGetCurrentPeb() as *const u8;
        if peb.is_null() {
            return (0, 0);
        }
        // PEB.ImageBaseAddress: +0x10 (x64) / +0x08 (x86) -- see heap.rs for the
        // sibling ProcessHeap offset that pins this layout.
        #[cfg(target_pointer_width = "64")]
        let base = *(peb.add(0x10) as *const *const c_void);
        #[cfg(target_pointer_width = "32")]
        let base = *(peb.add(0x08) as *const *const c_void);
        let nt = RtlImageNtHeader(base);
        if nt.is_null() {
            return (0, 0);
        }
        // OptionalHeader = nt + Signature(4) + IMAGE_FILE_HEADER(20).
        let opt = nt.add(24);
        // SizeOfStackReserve/Commit within the OptionalHeader (PE32+ are u64@72/80,
        // PE32 are u32@72/76).
        #[cfg(target_pointer_width = "64")]
        {
            let r = (opt.add(72) as *const u64).read_unaligned();
            let c = (opt.add(80) as *const u64).read_unaligned();
            (r as usize, c as usize)
        }
        #[cfg(target_pointer_width = "32")]
        {
            let r = (opt.add(72) as *const u32).read_unaligned();
            let c = (opt.add(76) as *const u32).read_unaligned();
            (r as usize, c as usize)
        }
    }
}

/// Run the cleanup actions a callback registered via TpCallback*OnCompletion,
/// called by dispatch_object after the callback (C order: cs, mutex, semaphore,
/// event, library).
pub(crate) fn run_instance_cleanup(inst: &CallbackInstance) {
    unsafe {
        if !inst.cleanup_critical_section.is_null() {
            RtlLeaveCriticalSection(inst.cleanup_critical_section);
        }
        if inst.cleanup_mutex != 0 {
            NtReleaseMutant(inst.cleanup_mutex as *mut c_void, ptr::null_mut());
        }
        if inst.cleanup_semaphore != 0 {
            NtReleaseSemaphore(inst.cleanup_semaphore as *mut c_void, inst.cleanup_semaphore_count, ptr::null_mut());
        }
        if inst.cleanup_event != 0 {
            NtSetEvent(inst.cleanup_event as *mut c_void, ptr::null_mut());
        }
        if !inst.cleanup_library.is_null() {
            LdrUnloadDll(inst.cleanup_library);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackSetEventOnCompletion(instance: *mut CallbackInstance, event: *mut c_void) {
    unsafe { (*instance).cleanup_event = event as u64 };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackReleaseSemaphoreOnCompletion(
    instance: *mut CallbackInstance,
    semaphore: *mut c_void,
    count: u32,
) {
    unsafe {
        (*instance).cleanup_semaphore = semaphore as u64;
        (*instance).cleanup_semaphore_count = count as i32;
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackReleaseMutexOnCompletion(instance: *mut CallbackInstance, mutex: *mut c_void) {
    unsafe { (*instance).cleanup_mutex = mutex as u64 };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackLeaveCriticalSectionOnCompletion(instance: *mut CallbackInstance, cs: *mut c_void) {
    unsafe { (*instance).cleanup_critical_section = cs };
}

#[unsafe(no_mangle)]
pub extern "system" fn TpCallbackUnloadDllOnCompletion(instance: *mut CallbackInstance, module: *mut c_void) {
    unsafe { (*instance).cleanup_library = module };
}
