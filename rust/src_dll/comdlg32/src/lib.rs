//! Rust portal client for comdlg32's Unix side.
//!
//! Bridges Wine's classic file open/save dialogs to the host's native file
//! chooser via the XDG desktop portal (`org.freedesktop.portal.FileChooser`).
//! The crate is linked into `comdlg32.so` through `UNIX_LIBS` (see
//! `dlls/comdlg32/Makefile.in`) and invoked via `__wine_unix_call` — the C
//! prototype and the `struct file_chooser_params` ABI live in
//! `dlls/comdlg32/unixlib.{h,c}`.
//!
//! [`rs_portal_file_chooser`] is the only entry point. It marshals the request,
//! hands it to the active [`backend`] (a hand-rolled libdbus client today; see
//! [`backend_libdbus`] for the swap-to-`zbus` note), and copies the chosen host
//! path back. Anything other than a successful pick — no portal, an error, a
//! panic — maps to a status that makes the PE side fall back to Wine's dialog.

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

mod backend;
mod backend_libdbus;
mod convert;
mod status;

use backend::{FileChooserOutcome, FileChooserRequest};

/// Mirror of `struct file_chooser_params` (`unixlib.h`). 32-bit-only project, so
/// PE and Unix share the address space and these pointers are dereferenceable
/// here directly.
#[repr(C)]
struct FileChooserParams {
    save: i32, // BOOL
    title: *const c_char,
    initial_dir: *const c_char,
    current_name: *const c_char,
    filters: *const c_char,
    out_path: *mut c_char,
    out_size: u32, // ULONG: in = capacity, out = bytes needed (incl. NUL)
}

/// Unix-call entry point for `unix_file_chooser` (`NTSTATUS rs_portal_file_chooser(void *args)`).
///
/// # Safety
/// `args` must be null or point to a valid `file_chooser_params`; the PE side
/// (portal.c) guarantees this.
#[unsafe(no_mangle)]
pub extern "C" fn rs_portal_file_chooser(args: *mut c_void) -> i32 {
    if args.is_null() {
        return status::STATUS_NOT_SUPPORTED;
    }
    // A panic must never unwind into Wine's C; on panic, fall back gracefully.
    catch_unwind(AssertUnwindSafe(|| {
        let params = unsafe { &mut *(args as *mut FileChooserParams) };
        run(params)
    }))
    .unwrap_or(status::STATUS_NOT_SUPPORTED)
}

fn run(params: &mut FileChooserParams) -> i32 {
    let request = build_request(params);
    match backend::active().run(&request) {
        FileChooserOutcome::Unsupported => status::STATUS_NOT_SUPPORTED,
        FileChooserOutcome::Cancelled => status::STATUS_CANCELLED,
        FileChooserOutcome::Chosen(uris) => {
            match uris.first().and_then(|u| convert::file_uri_to_path(u)) {
                Some(path) => write_out_path(params, &path),
                None => status::STATUS_CANCELLED,
            }
        }
    }
}

fn build_request(p: &FileChooserParams) -> FileChooserRequest {
    FileChooserRequest {
        save: p.save != 0,
        title: cstr_opt(p.title).unwrap_or_default(),
        current_folder: cstr_opt(p.initial_dir),
        current_name: cstr_opt(p.current_name),
        filters: convert::parse_filters(&read_filter_blob(p.filters)),
    }
}

/// Copy `path` (+ NUL) into `params.out_path`, reporting the required size.
fn write_out_path(params: &mut FileChooserParams, path: &str) -> i32 {
    let bytes = path.as_bytes();
    let needed = bytes.len() + 1; // + NUL
    let capacity = params.out_size as usize;
    params.out_size = needed as u32; // report need either way

    if params.out_path.is_null() || needed > capacity {
        return status::STATUS_BUFFER_TOO_SMALL;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), params.out_path as *mut u8, bytes.len());
        *params.out_path.add(bytes.len()) = 0;
    }
    status::STATUS_SUCCESS
}

/// Read a NUL-terminated UTF-8 string, mapping null/empty to `None`.
fn cstr_opt(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

/// Read the double-NUL-terminated filter blob (everything up to the first `\0\0`).
fn read_filter_blob(ptr: *const c_char) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    let base = ptr as *const u8;
    let mut len = 0usize;
    const CAP: usize = 64 * 1024; // safety bound
    unsafe {
        while len < CAP {
            let cur = *base.add(len);
            let next = *base.add(len + 1);
            if cur == 0 && next == 0 {
                break;
            }
            len += 1;
        }
        std::slice::from_raw_parts(base, len).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend::FileChooserBackend;

    /// A backend that returns a canned outcome, so we can exercise `run`'s
    /// marshaling without a live bus.
    struct FakeBackend(FileChooserOutcome);
    impl backend::FileChooserBackend for FakeBackend {
        fn run(&self, _r: &FileChooserRequest) -> FileChooserOutcome {
            match &self.0 {
                FileChooserOutcome::Chosen(v) => FileChooserOutcome::Chosen(v.clone()),
                FileChooserOutcome::Cancelled => FileChooserOutcome::Cancelled,
                FileChooserOutcome::Unsupported => FileChooserOutcome::Unsupported,
            }
        }
    }

    fn run_with(outcome: FileChooserOutcome, buf: &mut [u8]) -> (i32, u32) {
        let request = FileChooserRequest {
            save: false,
            title: String::new(),
            current_folder: None,
            current_name: None,
            filters: Vec::new(),
        };
        let backend = FakeBackend(outcome);
        let mut params = FileChooserParams {
            save: 0,
            title: std::ptr::null(),
            initial_dir: std::ptr::null(),
            current_name: std::ptr::null(),
            filters: std::ptr::null(),
            out_path: buf.as_mut_ptr() as *mut c_char,
            out_size: buf.len() as u32,
        };
        let status = match backend.run(&request) {
            FileChooserOutcome::Unsupported => status::STATUS_NOT_SUPPORTED,
            FileChooserOutcome::Cancelled => status::STATUS_CANCELLED,
            FileChooserOutcome::Chosen(uris) => {
                match uris.first().and_then(|u| convert::file_uri_to_path(u)) {
                    Some(path) => write_out_path(&mut params, &path),
                    None => status::STATUS_CANCELLED,
                }
            }
        };
        (status, params.out_size)
    }

    #[test]
    fn chosen_writes_dos_free_host_path() {
        let mut buf = [0u8; 64];
        let (st, size) =
            run_with(FileChooserOutcome::Chosen(vec!["file:///home/u/a%20b.txt".into()]), &mut buf);
        assert_eq!(st, status::STATUS_SUCCESS);
        let written = &buf[..size as usize - 1];
        assert_eq!(written, b"/home/u/a b.txt");
        assert_eq!(buf[size as usize - 1], 0); // NUL-terminated
    }

    #[test]
    fn buffer_too_small_reports_need() {
        let mut buf = [0u8; 4];
        let (st, size) =
            run_with(FileChooserOutcome::Chosen(vec!["file:///home/u/a.txt".into()]), &mut buf);
        assert_eq!(st, status::STATUS_BUFFER_TOO_SMALL);
        assert_eq!(size, "/home/u/a.txt".len() as u32 + 1);
    }

    #[test]
    fn cancelled_and_unsupported_map_through() {
        let mut buf = [0u8; 16];
        assert_eq!(run_with(FileChooserOutcome::Cancelled, &mut buf).0, status::STATUS_CANCELLED);
        assert_eq!(
            run_with(FileChooserOutcome::Unsupported, &mut buf).0,
            status::STATUS_NOT_SUPPORTED
        );
    }

    #[test]
    fn non_file_uri_is_treated_as_cancel() {
        let mut buf = [0u8; 16];
        let (st, _) = run_with(FileChooserOutcome::Chosen(vec!["http://x/y".into()]), &mut buf);
        assert_eq!(st, status::STATUS_CANCELLED);
    }
}
