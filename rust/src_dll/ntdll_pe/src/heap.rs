//! Global allocator for the freestanding PE `ntdll` crate, backed by ntdll's own
//! process heap via `RtlAllocateHeap`. This unlocks the `alloc` crate (`Box`,
//! `Vec`, ...) across the crate -- needed for the threadpool rewrite, which owns
//! heap-allocated pool and work objects.
//!
//! Under `cargo test` the crate builds as `std`, which supplies its own
//! allocator, so the global allocator is installed only for the real `no_std`
//! PE build (`#[cfg(not(test))]`).

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;

// Resolved at link against ntdll itself (the crate is linked into ntdll.dll).
// These are WINAPI (stdcall on i386), so they must be extern "system" -- extern "C"
// links on x86_64 (no decoration) but leaves `_Rtl*` undefined on i386, which
// decorates stdcall as `_Rtl*@N`.
unsafe extern "system" {
    fn RtlAllocateHeap(heap: *mut c_void, flags: u32, size: usize) -> *mut c_void;
    fn RtlReAllocateHeap(heap: *mut c_void, flags: u32, ptr: *mut c_void, size: usize) -> *mut c_void;
    fn RtlFreeHeap(heap: *mut c_void, flags: u32, ptr: *mut c_void) -> i32;
    fn RtlGetCurrentPeb() -> *mut c_void;
}

const HEAP_ZERO_MEMORY: u32 = 0x0000_0008;

/// `PEB->ProcessHeap` field offset (verified against wine include/winternl.h:
/// `HANDLE ProcessHeap; /* 018/030 */`).
#[cfg(target_pointer_width = "64")]
const PROCESS_HEAP_OFFSET: usize = 0x30;
#[cfg(target_pointer_width = "32")]
const PROCESS_HEAP_OFFSET: usize = 0x18;

/// `RtlAllocateHeap` returns memory aligned to MEMORY_ALLOCATION_ALIGNMENT
/// (16 on x64, 8 on x86) -- enough for every type the threadpool uses.
#[cfg(target_pointer_width = "64")]
const HEAP_ALIGN: usize = 16;
#[cfg(target_pointer_width = "32")]
const HEAP_ALIGN: usize = 8;

#[inline]
fn process_heap() -> *mut c_void {
    // SAFETY: RtlGetCurrentPeb returns the live PEB; ProcessHeap is a HANDLE at
    // the verified offset.
    unsafe {
        let peb = RtlGetCurrentPeb() as *const u8;
        peb.add(PROCESS_HEAP_OFFSET).cast::<*mut c_void>().read()
    }
}

/// `GlobalAlloc` over the ntdll process heap.
pub struct WineProcessHeap;

unsafe impl GlobalAlloc for WineProcessHeap {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // No over-aligned types are used here; refuse rather than mis-align.
        if layout.align() > HEAP_ALIGN {
            return core::ptr::null_mut();
        }
        unsafe { RtlAllocateHeap(process_heap(), 0, layout.size()).cast() }
    }
    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() > HEAP_ALIGN {
            return core::ptr::null_mut();
        }
        unsafe { RtlAllocateHeap(process_heap(), HEAP_ZERO_MEMORY, layout.size()).cast() }
    }
    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe {
            RtlFreeHeap(process_heap(), 0, ptr.cast());
        }
    }
    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, _layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { RtlReAllocateHeap(process_heap(), 0, ptr.cast(), new_size).cast() }
    }
}

/// Installed only for the real PE build; `cargo test` uses std's allocator.
#[cfg(not(test))]
#[global_allocator]
static WINE_HEAP: WineProcessHeap = WineProcessHeap;
