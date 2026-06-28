//! PE-side Rust implementation of `avrt.dll`'s MMCSS API (the
//! Multimedia Class Scheduler Service: `AvSetMmThreadCharacteristics` and
//! friends), replacing the C stubs in `dlls/avrt/main.c`.
//!
//! ## What MMCSS does and why this is a real fix (not a stub)
//!
//! A multimedia app registers a thread with a *task class* ("Audio", "Games",
//! "Pro Audio", ...) so the OS scheduler favours it -- this is how audio/render
//! threads avoid glitches under load. Wine stubbed it (`fixme:avrt:...stub`), so
//! the request was a no-op. We honour it by actually raising the calling thread's
//! NT priority according to the task's scheduling category.
//!
//! The win is real because Wine already maps NT thread priority onto the host
//! scheduler: `server/thread.c:apply_thread_priority` translates the NT priority
//! band to a Unix `nice` level (and the realtime band where permitted). That path
//! is `RLIMIT_NICE`-gated and degrades to a harmless no-op when unprivileged, so
//! MMCSS needs no Unix-side change -- raising the NT priority here is sufficient
//! and inherits the graceful fallback for free.
//!
//! ## Shape (mirrors the `wine_ntdll_pe` staticlib)
//!
//! Wine links the PE dll without a C runtime, so this is a freestanding `no_std`
//! crate. It is heap-free: active registrations live in a fixed-size,
//! spinlock-guarded table (see [`mmcss`]), so there is no global allocator and
//! panics trap on an undefined instruction (`panic = "abort"`). Host `cargo test`
//! builds it as `std` and exercises the mapping + table lifecycle through a mock
//! [`mmcss::ThreadOs`].

#![cfg_attr(not(test), no_std)]

/// Freestanding runtime glue, only needed for the real `no_std` build. Under
/// `cargo test`, `std` provides the panic handler instead.
#[cfg(not(test))]
mod rt {
    /// Trap via an undefined instruction (i686/x86_64 PE targets). The table code
    /// is written never to panic; if one ever does, fail fast rather than call a
    /// libc `abort` this environment does not provide.
    fn trap() -> ! {
        // SAFETY: `ud2` raises an illegal-instruction fault and never returns.
        unsafe { core::arch::asm!("ud2", options(noreturn)) }
    }

    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        trap()
    }

    // The precompiled std rlibs (built with panic=unwind) reference these unwinding
    // symbols from cold paths. Under `panic = "abort"` they are never reached, but
    // the `-nodefaultlibs` PE link still needs them resolved, so provide inert stubs.
    #[unsafe(no_mangle)]
    extern "C" fn rust_eh_personality() {}

    #[unsafe(no_mangle)]
    extern "C" fn _Unwind_Resume() -> ! {
        trap()
    }
}

pub mod mmcss;
// The avrt Av* exports; real build only (use the Win32 FFI backend in win_os).
#[cfg(not(test))]
pub mod exports;
// The real Win32 FFI backend (Get/SetThreadPriority/SetLastError); host tests use
// a mock ThreadOs instead, so it is excluded there.
#[cfg(not(test))]
pub mod win_os;

/// Linker anchor for the PE-side static-archive link.
///
/// `avrt.dll.so` is linked with this archive placed *before* the objects that
/// reference it, so ld would otherwise discard its members. The dll's
/// `EXTRADLLFLAGS` forces this one symbol (`-Wl,-u,wine_avrt_pe_anchor`); because
/// the crate is built with `codegen-units = 1` it is a single object, so pulling
/// this symbol pulls every `#[no_mangle]` entry point with it. Add new exports
/// without touching the link flags. Mirrors `wine_ntdll_pe_anchor`.
#[unsafe(no_mangle)]
pub extern "C" fn wine_avrt_pe_anchor() {}
