//! PE-side Rust implementation of `hid.dll`'s HidP_* value engine, replacing the
//! bit-field read/write core of `dlls/hid/hidp.c`.
//!
//! ## Why this is the right thing to move to Rust
//!
//! The `HidP_Get*/Set*UsageValue*` functions take a caller-supplied
//! `PHIDP_PREPARSED_DATA` and, crucially, a **live HID report buffer** -- the
//! bytes a real or emulated device just sent. Each value cap carries a
//! `start_byte` / `start_bit` / `bit_size` describing where its field sits in the
//! report; the C callbacks form `report_buf + start_byte + bit_offset/8` and copy
//! `bit_size` bits with `copy_bits`, which additionally reads a trailing 16-bit
//! word (`*(unsigned short *)src`) that can step one byte past the report. None of
//! this is bounded against `report_len`, so a report shorter than a cap implies
//! (or a crafted preparsed blob with an out-of-range `start_byte`) drives an
//! out-of-bounds read -- and, on the Set path, an out-of-bounds *write* -- over
//! attacker-influenced data. Bounds-checked slicing removes the whole class; the
//! same argument that motivated the `hidparse_pe` descriptor parser and the
//! `ntdll_pe` unwind interpreter.
//!
//! ## Threat model / trust boundary
//!
//! The preparsed blob is produced by our own hardened parser ([`hidparse_pe`])
//! and gated by its `"HidP KDR"` magic, so its declared cap counts are trusted
//! the way the C does. The **report buffer is untrusted** (live device data) and
//! its length *is* known (`report_len`), so every access into it is bounded here.
//! Any out-of-range byte read resolves to zero -- identical to the C result,
//! whose trailing over-read bytes are masked off anyway -- and any out-of-range
//! write is refused with `HIDP_STATUS_INVALID_REPORT_LENGTH`.
//!
//! ## Shape (mirrors the other `*_pe` staticlibs)
//!
//! Wine links the PE module without a C runtime, so this is a freestanding
//! `no_std` crate that aborts on panic. Unlike `hidparse_pe` it needs no
//! allocator: the engine only reads a preparsed blob and rewrites bit fields in
//! buffers the caller already owns. Under `cargo test` it builds as `std` and
//! exercises the engine (including the OOB cases) through the normal harness.

#![cfg_attr(not(test), no_std)]

/// Freestanding runtime glue, only needed for the real `no_std` build. Under
/// `cargo test`, `std` provides the panic handler instead.
#[cfg(not(test))]
mod rt {
    /// Trap via an undefined instruction (i686/x86_64 PE targets). The engine is
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
    // symbols from cold paths. Under `panic = "abort"` they're never reached, but
    // the `-nodefaultlibs` PE link still needs them resolved. Unlike the other *_pe
    // crates these are emitted as WEAK symbols: hidparse.sys links BOTH this crate
    // and wine_hidparse_pe (hidp.c is shared, PARENTSRC=../hid), and two strong
    // definitions of the same stub would be a "duplicate symbol" link error. Weak
    // lets the linker keep one; when this crate links alone (hid.dll) the weak
    // definition is still a definition. Emitted via asm so `.weak` applies (stable
    // Rust has no weak-linkage attribute); i386 PE prefixes symbols with `_`.
    #[cfg(target_arch = "x86_64")]
    core::arch::global_asm!(
        ".weak rust_eh_personality", ".weak _Unwind_Resume",
        "rust_eh_personality:", "_Unwind_Resume:", "ret",
    );
    #[cfg(target_arch = "x86")]
    core::arch::global_asm!(
        ".weak _rust_eh_personality", ".weak __Unwind_Resume",
        "_rust_eh_personality:", "__Unwind_Resume:", "ret",
    );
}

/// Single anchor symbol forced by the PE link (`-Wl,/include:`) so the whole
/// object -- every `rs_hidp_*` entry point -- is pulled in. Mirrors the other
/// `*_pe` crates' `*_anchor`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub extern "C" fn wine_hid_pe_anchor() {}

pub mod engine;
