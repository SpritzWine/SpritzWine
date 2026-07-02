//! The real ntdll FFI implementations of [`PoolOs`] / [`WaitOs`] / [`IoOs`] (the
//! `WineOs`), for the freestanding PE build. Resolved against ntdll itself at the
//! final link. The host unit tests use mocks instead, so this module is compiled
//! only for the real build (`#[cfg(not(test))]` on the `mod` in lib.rs).
//!
//! `LARGE_INTEGER` is passed as a bare `i64` (its `QuadPart`); a negative timeout
//! is relative (NT convention). Handles are `u64` in the OS traits and cast to the
//! native `HANDLE` (`*mut c_void`) at the boundary.

use crate::io_queue::IoOs;
use crate::threadpool::{IoStatusBlock, PoolOs};
use crate::wait_queue::WaitOs;
use core::ffi::c_void;
use core::sync::atomic::AtomicU32;

type Handle = *mut c_void;

const STATUS_TIMEOUT: i32 = 0x0000_0102;
/// SynchronizationEvent (auto-reset).
const SYNCHRONIZATION_EVENT: i32 = 1;
/// WaitAny.
const WAIT_ANY: i32 = 1;
/// STANDARD_RIGHTS_REQUIRED | SYNCHRONIZE | 0x3.
const ALL_ACCESS: u32 = 0x001F_0003;
/// FileCompletionInformation.
const FILE_COMPLETION_INFORMATION: i32 = 30;
/// NtDuplicateObject Options: keep the source's access mask.
const DUPLICATE_SAME_ACCESS: u32 = 0x0000_0002;

#[inline]
fn nt_current_process() -> Handle {
    // NtCurrentProcess() == (HANDLE)-1.
    core::ptr::without_provenance_mut(usize::MAX)
}

#[repr(C)]
struct FileCompletionInfo {
    port: Handle,
    key: usize,
}

// All ntdll exports use the WINAPI (stdcall on i686) calling convention.
unsafe extern "system" {
    fn RtlCreateUserThread(
        process: Handle,
        descriptor: *const c_void,
        suspended: u8,
        zero_bits: u32,
        stack_reserve: usize,
        stack_commit: usize,
        start: extern "system" fn(*mut c_void) -> u32,
        param: *mut c_void,
        thread: *mut Handle,
        client_id: *mut c_void,
    ) -> i32;
    fn NtClose(handle: Handle) -> i32;
    fn NtDuplicateObject(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        attributes: u32,
        options: u32,
    ) -> i32;

    fn RtlWaitOnAddress(addr: *const c_void, cmp: *const c_void, size: usize, timeout: *const i64) -> i32;
    fn RtlWakeAddressSingle(addr: *const c_void);
    fn RtlWakeAddressAll(addr: *const c_void);

    fn NtQueryPerformanceCounter(counter: *mut i64, frequency: *mut i64) -> i32;
    fn NtQuerySystemTime(time: *mut i64) -> i32;

    fn NtCreateEvent(
        handle: *mut Handle,
        access: u32,
        attr: *const c_void,
        event_type: i32,
        initial_state: u8,
    ) -> i32;
    fn NtSetEvent(handle: Handle, prev_state: *mut i32) -> i32;
    fn NtWaitForMultipleObjects(
        count: u32,
        handles: *const Handle,
        wait_type: i32,
        alertable: u8,
        timeout: *const i64,
    ) -> i32;

    fn NtCreateIoCompletion(handle: *mut Handle, access: u32, attr: *const c_void, count: u32) -> i32;
    fn NtSetInformationFile(
        file: Handle,
        iosb: *mut IoStatusBlock,
        info: *const c_void,
        len: u32,
        class: i32,
    ) -> i32;
    fn NtRemoveIoCompletion(
        port: Handle,
        key: *mut usize,
        value: *mut usize,
        iosb: *mut IoStatusBlock,
        timeout: *const i64,
    ) -> i32;
    fn NtSetIoCompletion(port: Handle, key: usize, value: usize, status: i32, info: usize) -> i32;
}

/// The real OS backend used by the threadpool in ntdll.
pub struct WineOs;

impl PoolOs for WineOs {
    fn spawn(&self, entry: extern "system" fn(*mut c_void) -> u32, arg: *mut c_void) -> bool {
        let mut thread: Handle = core::ptr::null_mut();
        let status = unsafe {
            RtlCreateUserThread(
                nt_current_process(),
                core::ptr::null(),
                0,
                0,
                0,
                0,
                entry,
                arg,
                &mut thread,
                core::ptr::null_mut(),
            )
        };
        if status == 0 {
            unsafe { NtClose(thread) };
            true
        } else {
            false
        }
    }

    fn futex_wait(&self, addr: &AtomicU32, expected: u32, timeout_100ns: i64) -> bool {
        let cmp = expected;
        let timeout = -timeout_100ns; // relative
        let status = unsafe {
            RtlWaitOnAddress(
                addr as *const AtomicU32 as *const c_void,
                &cmp as *const u32 as *const c_void,
                4,
                &timeout,
            )
        };
        status != STATUS_TIMEOUT
    }
    fn futex_wake_one(&self, addr: &AtomicU32) {
        unsafe { RtlWakeAddressSingle(addr as *const AtomicU32 as *const c_void) };
    }
    fn futex_wake_all(&self, addr: &AtomicU32) {
        unsafe { RtlWakeAddressAll(addr as *const AtomicU32 as *const c_void) };
    }

    fn now_mono_100ns(&self) -> i64 {
        // wine's performance counter runs at 10 MHz (100ns ticks), matching the
        // C threadpool's mixing of perf-counter values with 100ns timeouts.
        let mut counter: i64 = 0;
        unsafe { NtQueryPerformanceCounter(&mut counter, core::ptr::null_mut()) };
        counter
    }
    fn now_system_100ns(&self) -> i64 {
        let mut time: i64 = 0;
        unsafe { NtQuerySystemTime(&mut time) };
        time
    }
}

impl WaitOs for WineOs {
    fn create_event(&self) -> u64 {
        let mut handle: Handle = core::ptr::null_mut();
        unsafe {
            NtCreateEvent(&mut handle, ALL_ACCESS, core::ptr::null(), SYNCHRONIZATION_EVENT, 0)
        };
        handle as u64
    }
    fn set_event(&self, event: u64) {
        unsafe { NtSetEvent(event as Handle, core::ptr::null_mut()) };
    }
    fn close_event(&self, event: u64) {
        unsafe { NtClose(event as Handle) };
    }
    fn duplicate_handle(&self, handle: u64) -> u64 {
        // Duplicate into our own process so the wait thread owns a handle the app
        // can't close out from under it (waitable timers/semaphores are commonly
        // closed right after TpSetWait). 0 on failure -> caller falls back to raw.
        let mut dup: Handle = core::ptr::null_mut();
        let status = unsafe {
            NtDuplicateObject(
                nt_current_process(),
                handle as Handle,
                nt_current_process(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if status == 0 { dup as u64 } else { 0 }
    }
    fn wait_multiple(&self, handles: &[u64], timeout_100ns: i64) -> Option<usize> {
        // MAX_WAIT_OBJECTS (63) armed handles + the update_event.
        let mut native: [Handle; 64] = [core::ptr::null_mut(); 64];
        let n = handles.len().min(native.len());
        for i in 0..n {
            native[i] = handles[i] as Handle;
        }
        let timeout = -timeout_100ns; // relative
        // Alertable, so a user APC queued to a WT_EXECUTEINIOTHREAD wait's thread
        // runs here (returns STATUS_USER_APC -> None -> the service re-loops).
        let status =
            unsafe { NtWaitForMultipleObjects(n as u32, native.as_ptr(), WAIT_ANY, 1, &timeout) };
        // STATUS_WAIT_0 == 0; a signaled handle returns its index in [0, n).
        if status >= 0 && (status as usize) < n {
            Some(status as usize)
        } else {
            None
        }
    }
}

impl IoOs for WineOs {
    fn create_port(&self) -> u64 {
        let mut handle: Handle = core::ptr::null_mut();
        unsafe { NtCreateIoCompletion(&mut handle, ALL_ACCESS, core::ptr::null(), 0) };
        handle as u64
    }
    fn close_port(&self, port: u64) {
        unsafe { NtClose(port as Handle) };
    }
    fn associate(&self, port: u64, file: u64, key: u64) {
        let info = FileCompletionInfo { port: port as Handle, key: key as usize };
        let mut iosb = IoStatusBlock { status: 0, information: 0 };
        unsafe {
            NtSetInformationFile(
                file as Handle,
                &mut iosb,
                &info as *const FileCompletionInfo as *const c_void,
                core::mem::size_of::<FileCompletionInfo>() as u32,
                FILE_COMPLETION_INFORMATION,
            )
        };
    }
    fn remove_completion(&self, port: u64) -> (u64, usize, i32, usize) {
        let mut key: usize = 0;
        let mut value: usize = 0;
        let mut iosb = IoStatusBlock { status: 0, information: 0 };
        unsafe {
            NtRemoveIoCompletion(port as Handle, &mut key, &mut value, &mut iosb, core::ptr::null())
        };
        (key as u64, value, iosb.status, iosb.information)
    }
    fn post_completion(&self, port: u64, key: u64, value: usize, status: i32, info: usize) {
        unsafe { NtSetIoCompletion(port as Handle, key as usize, value, status, info) };
    }
}
