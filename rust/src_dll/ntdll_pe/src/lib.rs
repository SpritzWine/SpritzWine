//! PE-side Rust implementations for ntdll, mirroring functions that live in
//! Wine's PE `ntdll.dll` (the `C_SRCS` half, e.g. `dlls/ntdll/rtl.c`) rather than
//! the Unix half handled by the sibling `wine_ntdll` crate.
//!
//! Wine links the PE dll with `winegcc -nodefaultlibs` against musl + winecrt0,
//! so this crate is built as a `no_std` + `alloc` static library:
//!
//! * allocation is routed to Wine's libc (`posix_memalign`/`free`),
//! * panics `abort()` — that environment has no Rust unwinder, and the crate is
//!   compiled with `panic = "abort"`.
//!
//! Under `cargo test` the crate builds as `std` so the logic can be unit-tested
//! on the host with the normal test harness.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// Freestanding runtime glue, only needed for the real `no_std` build. Under
/// `cargo test`, `std` provides the allocator and panic handler instead.
#[cfg(not(test))]
mod rt {
    use core::alloc::{GlobalAlloc, Layout};
    use core::ffi::c_void;

    unsafe extern "C" {
        fn posix_memalign(memptr: *mut *mut c_void, align: usize, size: usize) -> i32;
        fn free(ptr: *mut c_void);
        fn abort() -> !;
    }

    /// Global allocator backed by Wine's libc heap.
    struct WineAlloc;

    unsafe impl GlobalAlloc for WineAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // posix_memalign requires a power-of-two alignment that is at least
            // sizeof(void*); layout.align() is already a power of two.
            let align = layout.align().max(core::mem::size_of::<usize>());
            let mut ptr: *mut c_void = core::ptr::null_mut();
            if unsafe { posix_memalign(&mut ptr, align, layout.size()) } != 0 {
                core::ptr::null_mut()
            } else {
                ptr as *mut u8
            }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            unsafe { free(ptr as *mut c_void) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: WineAlloc = WineAlloc;

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        unsafe { abort() }
    }

    // The precompiled std rlibs (built with panic=unwind) reference these
    // unwinding symbols from cold paths. Under `panic = "abort"` they're never
    // actually reached, but the `-nodefaultlibs` PE link (no libgcc) still needs
    // them resolved, so provide inert stubs.
    #[unsafe(no_mangle)]
    extern "C" fn rust_eh_personality() {}

    #[unsafe(no_mangle)]
    extern "C" fn _Unwind_Resume() -> ! {
        unsafe { abort() }
    }
}

pub mod rtl_compress;
pub mod status;

/// Linker anchor for the PE-side static-archive link.
///
/// `ntdll.dll.so` is linked with this archive placed *before* the objects that
/// reference it, so ld would otherwise discard its members. The dll's
/// `EXTRADLLFLAGS` forces this one symbol with `-Wl,-u,wine_ntdll_pe_anchor`;
/// because the crate is built with `codegen-units = 1` it is a single object, so
/// pulling this symbol pulls every `#[no_mangle]` entry point with it. Add new PE
/// functions without touching the link flags.
#[unsafe(no_mangle)]
pub extern "C" fn wine_ntdll_pe_anchor() {}
