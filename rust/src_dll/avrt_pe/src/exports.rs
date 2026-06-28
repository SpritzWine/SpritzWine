//! The avrt MMCSS exports (`#[no_mangle] extern "system"`), wiring the Win32 ABI
//! to the host-tested [`crate::mmcss`] core over the real [`WinThreadOs`]. The
//! ANSI `A` variants stay in `dlls/avrt/main.c` (they only widen the task name
//! and forward to the `W` functions here); the `AvRt*` thread-ordering-group
//! entries remain spec stubs.
//!
//! Real build only (`#[cfg(not(test))]` on the `mod` in lib.rs).

use crate::mmcss::{
    category_for_utf16, ThreadOs, ERROR_INVALID_HANDLE, ERROR_INVALID_TASK_NAME, MMCSS,
    SYSTEM_RESPONSIVENESS_DEFAULT,
};
use crate::win_os::WinThreadOs;
use core::ffi::c_void;
use core::ptr;

type Handle = *mut c_void;
type Bool = i32;
const TRUE: Bool = 1;
const FALSE: Bool = 0;

/// `AvSetMmThreadCharacteristicsW`: register the calling thread under a task and
/// boost its priority. Mirrors the old stub's argument validation (NULL name /
/// NULL index rejected) so it is a drop-in replacement.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn AvSetMmThreadCharacteristicsW(
    name: *const u16,
    index: *mut u32,
) -> Handle {
    let os = WinThreadOs;
    if name.is_null() {
        os.set_last_error(ERROR_INVALID_TASK_NAME);
        return ptr::null_mut();
    }
    if index.is_null() {
        os.set_last_error(ERROR_INVALID_HANDLE);
        return ptr::null_mut();
    }
    let category = unsafe { category_for_utf16(name) };
    let mut task_index = 0u32;
    match MMCSS.set_characteristics(&os, category, Some(&mut task_index)) {
        Ok(handle) => {
            unsafe { *index = task_index };
            handle as Handle
        }
        Err(err) => {
            os.set_last_error(err);
            ptr::null_mut()
        }
    }
}

/// `AvSetMmMaxThreadCharacteristicsW`: like the above but for two tasks, boosting
/// to the higher of the two scheduling categories. (The old stub rejected a
/// second task; honouring it is strictly more capable.)
#[unsafe(no_mangle)]
pub unsafe extern "system" fn AvSetMmMaxThreadCharacteristicsW(
    task1: *const u16,
    task2: *const u16,
    index: *mut u32,
) -> Handle {
    let os = WinThreadOs;
    if task1.is_null() {
        os.set_last_error(ERROR_INVALID_TASK_NAME);
        return ptr::null_mut();
    }
    if index.is_null() {
        os.set_last_error(ERROR_INVALID_HANDLE);
        return ptr::null_mut();
    }
    let mut category = unsafe { category_for_utf16(task1) };
    if !task2.is_null() {
        category = category.max(unsafe { category_for_utf16(task2) });
    }
    let mut task_index = 0u32;
    match MMCSS.set_characteristics(&os, category, Some(&mut task_index)) {
        Ok(handle) => {
            unsafe { *index = task_index };
            handle as Handle
        }
        Err(err) => {
            os.set_last_error(err);
            ptr::null_mut()
        }
    }
}

/// `AvSetMmThreadPriority`: adjust the registered thread's priority within its
/// task category.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn AvSetMmThreadPriority(handle: Handle, priority: i32) -> Bool {
    let os = WinThreadOs;
    match MMCSS.set_priority(&os, handle as usize, priority) {
        Ok(()) => TRUE,
        Err(err) => {
            os.set_last_error(err);
            FALSE
        }
    }
}

/// `AvRevertMmThreadCharacteristics`: restore the priority saved at registration
/// and release the handle.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn AvRevertMmThreadCharacteristics(handle: Handle) -> Bool {
    let os = WinThreadOs;
    match MMCSS.revert(&os, handle as usize) {
        Ok(()) => TRUE,
        Err(err) => {
            os.set_last_error(err);
            FALSE
        }
    }
}

/// `AvQuerySystemResponsiveness`: report the system responsiveness percentage for
/// a live MMCSS handle. (The old stub always failed; reporting the documented
/// default lets callers that gate on it proceed.)
#[unsafe(no_mangle)]
pub unsafe extern "system" fn AvQuerySystemResponsiveness(handle: Handle, value: *mut u32) -> Bool {
    let os = WinThreadOs;
    if !MMCSS.is_valid(handle as usize) || value.is_null() {
        os.set_last_error(ERROR_INVALID_HANDLE);
        return FALSE;
    }
    unsafe { *value = SYSTEM_RESPONSIVENESS_DEFAULT };
    TRUE
}
