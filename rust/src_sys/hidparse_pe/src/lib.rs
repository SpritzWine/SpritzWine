//! PE-side Rust implementation of `hidparse.sys`'s report-descriptor parser,
//! replacing the hand-rolled C pointer walk in `dlls/hidparse.sys/main.c`
//! (`parse_descriptor` and the helper cluster it drives).
//!
//! ## Why this is the right thing to move to Rust
//!
//! `parse_descriptor` consumes a **HID report descriptor** -- an untrusted,
//! device-supplied byte stream (a real or emulated USB/Bluetooth gadget can hand
//! over anything). The C item loop advances with raw pointer arithmetic and a
//! `ptr != end` terminator, with a bounds check (`ptr + size > end`) that is
//! off-by-one: a 4-byte item value whose last byte lands exactly on `end` is read
//! one byte past the buffer, and because the loop compares `!=` rather than `<`,
//! that same malformed item makes `ptr` step *over* `end` and run away. This is
//! exactly the class of bug bounds-checked slicing eliminates -- the same argument
//! that motivated the `ntdll_pe` unwind opcode interpreter.
//!
//! ## Shape (mirrors the `wine_ntdll_pe` / `wine_avrt_pe` staticlibs)
//!
//! Wine links the PE module without a C runtime, so this is a freestanding
//! `no_std` crate that aborts on panic (the environment has no Rust unwinder).
//! The parser's growable arrays (the C `realloc`-backed stack / collections /
//! value-caps) become `Vec`s over a global allocator backed by the kernel pool
//! (`ExAllocatePool`, see [`heap`]) -- `hidparse.sys` imports only `ntoskrnl`, so
//! pool is the one allocator guaranteed resolvable. The returned preparsed buffer
//! is allocated with `ExAllocatePool` directly, so the existing C callers free it
//! with `ExFreePool` unchanged.
//!
//! Under `cargo test` the crate builds as `std` and exercises the parser on real
//! and deliberately malformed descriptors through the normal test harness.

#![cfg_attr(not(test), no_std)]

// The crate provides a kernel-pool-backed global allocator (see `heap`), so the
// `alloc` crate (Box/Vec/...) is available even in the freestanding PE build.
extern crate alloc;

/// Freestanding runtime glue, only needed for the real `no_std` build. Under
/// `cargo test`, `std` provides the panic handler instead.
#[cfg(not(test))]
mod rt {
    /// Trap via an undefined instruction (i686/x86_64 PE targets). The parser is
    /// written never to panic; if one ever does, fail fast rather than call a libc
    /// `abort` this environment does not provide.
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
    // reached, but the `-nodefaultlibs` PE link still needs them resolved.
    #[unsafe(no_mangle)]
    extern "C" fn rust_eh_personality() {}

    #[unsafe(no_mangle)]
    extern "C" fn _Unwind_Resume() -> ! {
        trap()
    }
}

pub mod heap;
pub mod parser;

/// Linker anchor for the PE-side static-archive link.
///
/// `hidparse.sys` is linked with this archive placed *before* the objects that
/// reference it, so ld would otherwise discard its members. The module's
/// `EXTRADLLFLAGS` forces this one symbol with `-Wl,/include:`; because the crate
/// is built with `codegen-units = 1` it is a single object, so pulling this symbol
/// pulls every `#[no_mangle]` entry point with it. Add new PE functions without
/// touching the link flags.
#[unsafe(no_mangle)]
pub extern "C" fn wine_hidparse_pe_anchor() {}
