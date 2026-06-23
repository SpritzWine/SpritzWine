//! PE-side Rust implementations for ntdll, mirroring functions that live in
//! Wine's PE `ntdll.dll` (the `C_SRCS` half, e.g. `dlls/ntdll/rtl.c`) rather than
//! the Unix half handled by the sibling `wine_ntdll` crate.
//!
//! Wine links the PE dll with `winegcc -nodefaultlibs`, and a real-PE `ntdll.dll`
//! has no C runtime, so this crate is built as a freestanding `no_std` static
//! library with no external dependencies:
//!
//! * it is fully allocation-free (the LZNT1 compressor uses fixed stack scratch,
//!   stack-probed by ntdll's own `__chkstk`) -- there is no global allocator,
//! * panics trap via an undefined instruction; the crate is compiled with
//!   `panic = "abort"`, and that environment has no Rust unwinder or libc `abort`.
//!
//! Under `cargo test` the crate builds as `std` so the logic can be unit-tested
//! on the host with the normal test harness.

#![cfg_attr(not(test), no_std)]

/// Freestanding runtime glue, only needed for the real `no_std` build. Under
/// `cargo test`, `std` provides the panic handler instead. The crate is fully
/// allocation-free, so no global allocator is supplied -- a real-PE `ntdll.dll`
/// has no C runtime/heap to back one.
#[cfg(not(test))]
mod rt {
    /// Trap via an undefined instruction (i686/x86_64 PE targets). The bounds-checked
    /// code paths are written never to panic; if one ever does, fail fast rather than
    /// call a libc `abort` this environment doesn't provide.
    fn trap() -> ! {
        // SAFETY: `ud2` raises an illegal-instruction fault and never returns.
        unsafe { core::arch::asm!("ud2", options(noreturn)) }
    }

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        trap()
    }

    // The precompiled std rlibs (built with panic=unwind) reference these unwinding
    // symbols from cold paths. Under `panic = "abort"` they're never actually
    // reached, but the `-nodefaultlibs` PE link still needs them resolved, so
    // provide inert stubs.
    #[unsafe(no_mangle)]
    extern "C" fn rust_eh_personality() {}

    #[unsafe(no_mangle)]
    extern "C" fn _Unwind_Resume() -> ! {
        trap()
    }
}

pub mod rtl_compress;
pub mod status;
pub mod unwind;
pub mod work_queue;

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
