//! Rust portal client for comdlg32's Unix side.
//!
//! Bridges Wine's classic file open/save dialogs to the host's native file
//! chooser via the XDG desktop portal (`org.freedesktop.portal.FileChooser`).
//! The crate is linked into `comdlg32.so` through `UNIX_LIBS` (see
//! `dlls/comdlg32/Makefile.in`) and invoked via `__wine_unix_call` — the C
//! prototype and the `struct file_chooser_params` ABI live in
//! `dlls/comdlg32/unixlib.{h,c}`.
//!
//! This commit only establishes the bridge: [`rs_portal_file_chooser`] is a stub
//! that reports the portal as unavailable, so comdlg32 always falls back to
//! Wine's own dialog. The real backend (libdbus, behind a swappable
//! `FileChooserBackend` seam) lands in the next commit.

use core::ffi::c_void;

mod status;

/// Unix-call entry point for `unix_file_chooser`. Mirrors the C prototype
/// `NTSTATUS rs_portal_file_chooser(void *args)` in `unixlib.c`.
///
/// Stub: no backend is wired yet, so this returns `STATUS_NOT_SUPPORTED` and the
/// PE side falls back to Wine's built-in file dialog.
#[unsafe(no_mangle)]
pub extern "C" fn rs_portal_file_chooser(_args: *mut c_void) -> i32 {
    status::STATUS_NOT_SUPPORTED
}
