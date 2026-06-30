//! Global allocator for the freestanding PE `hidparse.sys` crate, backed by the
//! kernel pool via ntoskrnl's `ExAllocatePool` / `ExFreePool`. This unlocks the
//! `alloc` crate (`Box`, `Vec`, ...) so the descriptor parser's growable arrays
//! (the C `realloc`-backed stack / collections / value-caps) become `Vec`s.
//!
//! Pool (not the C runtime `malloc`) is used because `hidparse.sys` imports only
//! `ntoskrnl`, so `Ex{Allocate,Free}Pool` are guaranteed resolvable; the returned
//! preparsed buffer is allocated separately, also via `ExAllocatePool`, so the
//! existing C callers free it with `ExFreePool` unchanged (see `parser`).
//!
//! Under `cargo test` the crate builds as `std`, which supplies its own
//! allocator, so this is installed only for the real `no_std` PE build.

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;

// POOL_TYPE: NonPagedPool == 0 (see include/ddk/wdm.h). Pool allocations are
// safe at the passive level the HID PnP path runs at; NonPagedPool is valid at
// any IRQL, matching the C `calloc`/`realloc` heap's unconditional availability.
const NON_PAGED_POOL: u32 = 0;

// MEMORY_ALLOCATION_ALIGNMENT: pool blocks are 16-byte aligned on x64, 8 on x86 --
// enough for every type the parser uses (`hid_value_caps` is 4-aligned).
#[cfg(target_pointer_width = "64")]
const POOL_ALIGN: usize = 16;
#[cfg(target_pointer_width = "32")]
const POOL_ALIGN: usize = 8;

// stdcall on i386 (`@N` decoration), plain C on x86_64 -> extern "system".
// Prototypes match include/ddk/wdm.h: ExAllocatePool(POOL_TYPE, SIZE_T).
unsafe extern "system" {
    pub fn ExAllocatePool(pool_type: u32, size: usize) -> *mut c_void;
    pub fn ExFreePool(ptr: *mut c_void);
}

/// `GlobalAlloc` over the kernel pool. `alloc_zeroed` and `realloc` use the trait
/// defaults (alloc + zero / alloc + copy + free), which is correct here.
pub struct WinePool;

unsafe impl GlobalAlloc for WinePool {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // No over-aligned types are used here; refuse rather than mis-align.
        if layout.align() > POOL_ALIGN {
            return core::ptr::null_mut();
        }
        unsafe { ExAllocatePool(NON_PAGED_POOL, layout.size()).cast() }
    }
    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { ExFreePool(ptr.cast()) }
    }
}

/// Installed only for the real PE build; `cargo test` uses std's allocator.
#[cfg(not(test))]
#[global_allocator]
static WINE_POOL: WinePool = WinePool;
