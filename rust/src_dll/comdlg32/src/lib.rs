//! Rust portal client for comdlg32's Unix side.
//!
//! Bridges Wine's classic file open/save dialogs and its print dialog to the
//! host's native ones via the XDG desktop portal
//! (`org.freedesktop.portal.FileChooser`, `org.freedesktop.portal.Print`).
//! The crate is linked into `comdlg32.so` through `UNIX_LIBS` (see
//! `dlls/comdlg32/Makefile.in`) and invoked via `__wine_unix_call`; the C
//! prototypes and the `struct *_params` ABIs live in
//! `dlls/comdlg32/unixlib.{h,c}`.
//!
//! [`rs_portal_file_chooser`] and [`rs_portal_prepare_print`] are the entry
//! points. Each marshals its request, hands it to the active [`backend`] (a
//! hand-rolled libdbus client today; see [`backend_libdbus`] for the
//! swap-to-`zbus` note), and copies the result back. Anything other than a
//! successful pick (no portal, an error, a panic) maps to a status that makes
//! the PE side fall back to Wine's dialog.

use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};

mod backend;
mod backend_libdbus;
mod convert;
mod print;
mod status;

use backend::{FileChooserBackend, FileChooserOutcome, FileChooserRequest, PrintBackend};
use print::{DeliveryOutcome, Duplex, Orientation, Pages, PrintChoice, PrintOutcome, PrintRequest};

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

// ---------------------------------------------------------------------------
// Print dialog
// ---------------------------------------------------------------------------

/// Mirror of `struct prepare_print_params` (`unixlib.h`). Only `title` is a
/// pointer; the string results are fixed arrays so the WoW64 thunk has a
/// single field to widen.
#[repr(C)]
struct PreparePrintParams {
    title: *const c_char,
    copies: u32,           // in: hint (0 = none); out: chosen
    collate: i32,          // BOOL, out (no hint: backends disagree on its spelling)
    orientation: u32,      // in/out: 0 unknown, 1 portrait, 2 landscape
    has_current_page: i32, // BOOL, in
    has_selected_pages: i32, // BOOL, in
    print_to_file: i32,    // BOOL, in: the app allows print-to-file
    token: u32,            // out: for the portal's Print call
    duplex: u32,           // out: 0 simplex, 1 long edge, 2 short edge
    color: i32,            // BOOL, out
    resolution: u32,       // out: dpi, 0 unknown
    reverse: i32,          // BOOL, out
    print_pages: u32,      // out: 0 all, 1 ranges, 2 current, 3 selection
    paper_width: u32,      // out: tenths of mm, 0 unknown
    paper_height: u32,
    paper_name: [c_char; 64],
    page_ranges: [c_char; 256],
    output_file: [c_char; 1024],
}

const ORIENTATION_PORTRAIT: u32 = 1;
const ORIENTATION_LANDSCAPE: u32 = 2;

/// Unix-call entry point for `unix_prepare_print` (`NTSTATUS rs_portal_prepare_print(void *args)`).
///
/// # Safety
/// `args` must be null or point to a valid `prepare_print_params`.
#[unsafe(no_mangle)]
pub extern "C" fn rs_portal_prepare_print(args: *mut c_void) -> i32 {
    if args.is_null() {
        return status::STATUS_NOT_SUPPORTED;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let params = unsafe { &mut *(args as *mut PreparePrintParams) };
        let request = build_print_request(params);
        apply_print_outcome(params, backend::active().prepare_print(&request))
    }))
    .unwrap_or(status::STATUS_NOT_SUPPORTED)
}

fn build_print_request(p: &PreparePrintParams) -> PrintRequest {
    PrintRequest {
        title: cstr_opt(p.title).unwrap_or_default(),
        copies: p.copies,
        orientation: match p.orientation {
            ORIENTATION_PORTRAIT => Orientation::Portrait,
            ORIENTATION_LANDSCAPE => Orientation::Landscape,
            _ => Orientation::Unknown,
        },
        has_current_page: p.has_current_page != 0,
        has_selected_pages: p.has_selected_pages != 0,
        print_to_file: p.print_to_file != 0,
    }
}

fn apply_print_outcome(p: &mut PreparePrintParams, outcome: PrintOutcome) -> i32 {
    match outcome {
        PrintOutcome::Unsupported => status::STATUS_NOT_SUPPORTED,
        PrintOutcome::Cancelled => status::STATUS_CANCELLED,
        PrintOutcome::Chosen(choice) => {
            write_print_choice(p, &choice);
            print::remember_token(choice.token);
            status::STATUS_SUCCESS
        }
    }
}

/// Mirror of `struct portal_print_params`, declared in both
/// `dlls/comdlg32/unixlib.h` and `dlls/localspl/cups.c`.
#[repr(C)]
struct PortalPrintParams {
    path: *const c_char,  // host path of the finished PostScript document
    title: *const c_char, // job title, or NULL
}

/// Deliver a spooled document through the portal, using the token of the last
/// dialog if there is one. Called by localspl.so for the `PORTAL:` port.
///
/// # Safety
/// `args` must be null or point to a valid `portal_print_params`.
#[unsafe(no_mangle)]
pub extern "C" fn rs_portal_print(args: *mut c_void) -> i32 {
    if args.is_null() {
        return status::STATUS_NOT_SUPPORTED;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let params = unsafe { &*(args as *const PortalPrintParams) };
        let Some(path) = cstr_opt(params.path) else {
            return status::STATUS_NOT_SUPPORTED;
        };
        let title = cstr_opt(params.title).unwrap_or_default();
        match backend::active().print_file(&path, &title, print::take_token()) {
            DeliveryOutcome::Delivered => status::STATUS_SUCCESS,
            DeliveryOutcome::Unsupported => status::STATUS_NOT_SUPPORTED,
        }
    }))
    .unwrap_or(status::STATUS_NOT_SUPPORTED)
}

fn write_print_choice(p: &mut PreparePrintParams, c: &PrintChoice) {
    p.token = c.token;
    p.copies = c.copies;
    p.collate = c.collate as i32;
    p.reverse = c.reverse as i32;
    p.color = c.color as i32;
    p.resolution = c.resolution;
    p.orientation = match c.orientation {
        Orientation::Portrait => ORIENTATION_PORTRAIT,
        Orientation::Landscape => ORIENTATION_LANDSCAPE,
        Orientation::Unknown => 0,
    };
    p.duplex = match c.duplex {
        Duplex::Simplex => 0,
        Duplex::LongEdge => 1,
        Duplex::ShortEdge => 2,
    };
    let (print_pages, ranges) = match &c.pages {
        Pages::All => (0, ""),
        Pages::Ranges(r) => (1, r.as_str()),
        Pages::Current => (2, ""),
        Pages::Selection => (3, ""),
    };
    p.print_pages = print_pages;
    p.paper_width = c.paper_width;
    p.paper_height = c.paper_height;
    copy_cstr(&mut p.paper_name, &c.paper_name);
    copy_cstr(&mut p.page_ranges, ranges);
    copy_cstr(&mut p.output_file, c.output_file.as_deref().unwrap_or(""));
}

/// Copy `s` into a fixed C buffer, NUL-terminated, truncating on a UTF-8
/// character boundary.
fn copy_cstr(dst: &mut [c_char], s: &str) {
    let max = dst.len().saturating_sub(1);
    let mut end = s.len().min(max);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    for (d, b) in dst.iter_mut().zip(s.as_bytes()[..end].iter()) {
        *d = *b as c_char;
    }
    dst[end] = 0;
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

    // ---- print ------------------------------------------------------------

    fn print_params() -> PreparePrintParams {
        PreparePrintParams {
            title: std::ptr::null(),
            copies: 0,
            collate: 0,
            orientation: 0,
            has_current_page: 0,
            has_selected_pages: 0,
            print_to_file: 0,
            token: 0,
            duplex: 0,
            color: 0,
            resolution: 0,
            reverse: 0,
            print_pages: 0,
            paper_width: 0,
            paper_height: 0,
            paper_name: [0; 64],
            page_ranges: [0; 256],
            output_file: [0; 1024],
        }
    }

    fn cbuf(buf: &[c_char]) -> String {
        let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn print_choice_is_written_to_params() {
        let mut p = print_params();
        let choice = PrintChoice {
            token: 42,
            copies: 2,
            collate: true,
            reverse: false,
            duplex: Duplex::ShortEdge,
            color: false,
            resolution: 600,
            pages: Pages::Ranges("2-3".into()),
            orientation: Orientation::Landscape,
            paper_name: "A4".into(),
            paper_width: 2100,
            paper_height: 2970,
            output_file: Some("/tmp/out.ps".into()),
        };
        assert_eq!(apply_print_outcome(&mut p, PrintOutcome::Chosen(choice)), status::STATUS_SUCCESS);
        assert_eq!((p.token, p.copies, p.collate, p.reverse), (42, 2, 1, 0));
        assert_eq!((p.duplex, p.color, p.resolution), (2, 0, 600));
        assert_eq!((p.print_pages, p.orientation), (1, ORIENTATION_LANDSCAPE));
        assert_eq!((p.paper_width, p.paper_height), (2100, 2970));
        assert_eq!(cbuf(&p.paper_name), "A4");
        assert_eq!(cbuf(&p.page_ranges), "2-3");
        assert_eq!(cbuf(&p.output_file), "/tmp/out.ps");
    }

    #[test]
    fn print_outcomes_map_to_status() {
        let mut p = print_params();
        assert_eq!(apply_print_outcome(&mut p, PrintOutcome::Cancelled), status::STATUS_CANCELLED);
        assert_eq!(apply_print_outcome(&mut p, PrintOutcome::Unsupported), status::STATUS_NOT_SUPPORTED);
        assert_eq!(p.token, 0);
    }

    #[test]
    fn print_request_from_params() {
        let mut p = print_params();
        p.copies = 3;
        p.collate = 1;
        p.orientation = ORIENTATION_PORTRAIT;
        p.has_selected_pages = 1;
        p.print_to_file = 1;
        let r = build_print_request(&p);
        assert_eq!(r.copies, 3);
        assert_eq!(r.orientation, Orientation::Portrait);
        assert!(!r.has_current_page && r.has_selected_pages && r.print_to_file);
    }

    #[test]
    fn copy_cstr_truncates_on_char_boundary() {
        let mut buf = [1 as c_char; 6];
        copy_cstr(&mut buf, "ab\u{e9}cd"); // e-acute is 2 bytes
        assert_eq!(cbuf(&buf), "ab\u{e9}c");
        copy_cstr(&mut buf, "abcd\u{e9}");
        assert_eq!(cbuf(&buf), "abcd"); // the 2-byte char does not fit
        copy_cstr(&mut buf, "");
        assert_eq!(cbuf(&buf), "");
    }
}
