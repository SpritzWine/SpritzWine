//! Rust implementations of ntdll internals, mirroring Wine's `dlls/ntdll` tree.
//!
//! Each module here corresponds to a Wine C source file, e.g. [`system`] backs
//! `dlls/ntdll/unix/system.c`. Functions are exported via `#[no_mangle] extern "C"`
//! and linked into ntdll's unix lib (see `dlls/ntdll/Makefile.in`,
//! `$(WINE_RUST_LDFLAGS) -Wl,-lwine_ntdll`) and called from the corresponding C
//! source.
//!
//! This crate intentionally pulls in nothing from Wine (no bindgen, no link against
//! other Wine dlls), so `libwine_ntdll.so` builds standalone and can be linked into
//! ntdll without a circular dependency.

pub mod status;
pub mod system;
