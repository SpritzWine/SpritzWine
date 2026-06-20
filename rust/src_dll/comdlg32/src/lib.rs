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
    save: i32,      // BOOL
    multiple: i32,  // BOOL
    directory: i32, // BOOL
    title: *const c_char,
    initial_dir: *const c_char,
    current_name: *const c_char,
    filters: *const c_char,
    current_filter: u32, // ULONG: 1-based filter to pre-select, 0 = none
    out_paths: *mut c_char,
    out_size: u32,  // ULONG: in = capacity, out = bytes needed (incl. final NUL)
    out_filter: u32, // ULONG: out = 1-based selected filter, 0 = unknown
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
        FileChooserOutcome::Chosen { uris, filter_index } => {
            let paths: Vec<String> =
                uris.iter().filter_map(|u| convert::file_uri_to_path(u)).collect();
            if paths.is_empty() {
                return status::STATUS_CANCELLED;
            }
            // ABI uses a 1-based filter index (0 = unknown).
            params.out_filter = filter_index.map(|i| i as u32 + 1).unwrap_or(0);
            write_out_paths(params, &paths)
        }
    }
}

fn build_request(p: &FileChooserParams) -> FileChooserRequest {
    FileChooserRequest {
        save: p.save != 0,
        multiple: p.multiple != 0,
        directory: p.directory != 0,
        title: cstr_opt(p.title).unwrap_or_default(),
        current_folder: cstr_opt(p.initial_dir),
        current_name: cstr_opt(p.current_name),
        filters: convert::parse_filters(&read_filter_blob(p.filters)),
        // ABI is 1-based (0 = none); the request is 0-based.
        current_filter: (p.current_filter > 0).then(|| p.current_filter as usize - 1),
    }
}

/// Serialize `paths` into `params.out_paths` as a NUL-separated, double-NUL-
/// terminated list, reporting the required size.
fn write_out_paths(params: &mut FileChooserParams, paths: &[String]) -> i32 {
    let mut buf: Vec<u8> = Vec::new();
    for p in paths {
        buf.extend_from_slice(p.as_bytes());
        buf.push(0);
    }
    buf.push(0); // final terminator

    let capacity = params.out_size as usize;
    params.out_size = buf.len() as u32; // report need either way

    if params.out_paths.is_null() || buf.len() > capacity {
        return status::STATUS_BUFFER_TOO_SMALL;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(buf.as_ptr(), params.out_paths as *mut u8, buf.len());
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

    struct Fake(FileChooserOutcome);
    impl FileChooserBackend for Fake {
        fn run(&self, _r: &FileChooserRequest) -> FileChooserOutcome {
            match &self.0 {
                FileChooserOutcome::Chosen { uris, filter_index } => FileChooserOutcome::Chosen {
                    uris: uris.clone(),
                    filter_index: *filter_index,
                },
                FileChooserOutcome::Cancelled => FileChooserOutcome::Cancelled,
                FileChooserOutcome::Unsupported => FileChooserOutcome::Unsupported,
            }
        }
    }

    fn chosen(uris: &[&str], filter: Option<usize>) -> FileChooserOutcome {
        FileChooserOutcome::Chosen {
            uris: uris.iter().map(|s| s.to_string()).collect(),
            filter_index: filter,
        }
    }

    /// Drive the same outcome-mapping `run` uses, against a fake backend.
    /// Returns (status, out_size, out_filter).
    fn run_with(outcome: FileChooserOutcome, buf: &mut [u8]) -> (i32, u32, u32) {
        let request = FileChooserRequest {
            save: false,
            multiple: false,
            directory: false,
            title: String::new(),
            current_folder: None,
            current_name: None,
            filters: Vec::new(),
            current_filter: None,
        };
        let mut params = FileChooserParams {
            save: 0,
            multiple: 0,
            directory: 0,
            title: std::ptr::null(),
            initial_dir: std::ptr::null(),
            current_name: std::ptr::null(),
            filters: std::ptr::null(),
            current_filter: 0,
            out_paths: buf.as_mut_ptr() as *mut c_char,
            out_size: buf.len() as u32,
            out_filter: 0,
        };
        let st = match Fake(outcome).run(&request) {
            FileChooserOutcome::Unsupported => status::STATUS_NOT_SUPPORTED,
            FileChooserOutcome::Cancelled => status::STATUS_CANCELLED,
            FileChooserOutcome::Chosen { uris, filter_index } => {
                let paths: Vec<String> =
                    uris.iter().filter_map(|u| convert::file_uri_to_path(u)).collect();
                if paths.is_empty() {
                    status::STATUS_CANCELLED
                } else {
                    params.out_filter = filter_index.map(|i| i as u32 + 1).unwrap_or(0);
                    write_out_paths(&mut params, &paths)
                }
            }
        };
        (st, params.out_size, params.out_filter)
    }

    #[test]
    fn single_path_is_double_nul_list() {
        let mut buf = [0u8; 64];
        let (st, size, _) = run_with(chosen(&["file:///home/u/a%20b.txt"], None), &mut buf);
        assert_eq!(st, status::STATUS_SUCCESS);
        assert_eq!(&buf[..size as usize], b"/home/u/a b.txt\0\0");
    }

    #[test]
    fn multiple_paths_nul_separated() {
        let mut buf = [0u8; 128];
        let (st, size, _) =
            run_with(chosen(&["file:///d/one.txt", "file:///d/two.txt"], None), &mut buf);
        assert_eq!(st, status::STATUS_SUCCESS);
        assert_eq!(&buf[..size as usize], b"/d/one.txt\0/d/two.txt\0\0");
    }

    #[test]
    fn filter_index_is_one_based_out() {
        let mut buf = [0u8; 64];
        assert_eq!(run_with(chosen(&["file:///d/x.txt"], Some(0)), &mut buf).2, 1);
        assert_eq!(run_with(chosen(&["file:///d/x.txt"], Some(2)), &mut [0u8; 64]).2, 3);
        assert_eq!(run_with(chosen(&["file:///d/x.txt"], None), &mut [0u8; 64]).2, 0);
    }

    #[test]
    fn buffer_too_small_reports_need() {
        let mut buf = [0u8; 4];
        let (st, size, _) = run_with(chosen(&["file:///home/u/a.txt"], None), &mut buf);
        assert_eq!(st, status::STATUS_BUFFER_TOO_SMALL);
        // path + separator NUL + final NUL
        assert_eq!(size, "/home/u/a.txt".len() as u32 + 2);
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
        assert_eq!(run_with(chosen(&["http://x/y"], None), &mut buf).0, status::STATUS_CANCELLED);
    }
}
