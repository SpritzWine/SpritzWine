//! The real [`ThreadOs`] backend for the freestanding PE build: thin wrappers
//! over the Win32 thread-priority calls, resolved against `kernel32` at the
//! avrt.dll link (see `dlls/avrt/Makefile.in` `IMPORTS`). Host tests use a mock
//! instead, so this module is compiled only for the real build.

use crate::mmcss::ThreadOs;
use core::ffi::c_void;

type Handle = *mut c_void;
type Bool = i32;

// All use the WINAPI (stdcall on i686) calling convention.
unsafe extern "system" {
    fn GetCurrentThread() -> Handle;
    fn GetThreadPriority(thread: Handle) -> i32;
    fn SetThreadPriority(thread: Handle, priority: i32) -> Bool;
    fn SetLastError(code: u32);
}

/// MMCSS operates on the *calling* thread, so every call targets the current
/// thread pseudo-handle (`GetCurrentThread()`, `(HANDLE)-2`).
pub struct WinThreadOs;

impl ThreadOs for WinThreadOs {
    fn get_priority(&self) -> i32 {
        unsafe { GetThreadPriority(GetCurrentThread()) }
    }
    fn set_priority(&self, prio: i32) {
        unsafe {
            SetThreadPriority(GetCurrentThread(), prio);
        }
    }
    fn set_last_error(&self, err: u32) {
        unsafe { SetLastError(err) };
    }
}
