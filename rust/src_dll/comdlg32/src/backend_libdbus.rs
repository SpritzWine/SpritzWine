//! NOTE — D-BUS TRANSPORT (read before extending this layer)
//! ----------------------------------------------------------------------------
//! This is the *safe* file-chooser backend: a hand-rolled libdbus client,
//! `dlopen`ed at runtime and driven **synchronously on the calling thread**, so
//! no async runtime and no extra background thread ever runs inside Wine's
//! address space. That matters because Wine's Unix side has its own threading
//! and signal machinery; this mirrors how Wine's own `mountmgr.sys` /
//! `winebth.sys` talk to D-Bus.
//!
//! The [`crate::backend::FileChooserBackend`] seam exists precisely so this can
//! be replaced. **Before growing this hand-rolled marshaling** (more portals,
//! more options, settings/notifications, …), add a `backend_zbus.rs` built on the
//! widely-used **`zbus`** stack (optionally `ashpd`) behind a cargo feature and
//! select it in `crate::backend::active()` — adopting the standard, maintained
//! package once its in-Wine threading is validated (drive zbus's executor with a
//! manual `executor().tick()` loop so it stays thread-free, like this backend).
//! Prefer that over piling more by-hand D-Bus code here.
//! ----------------------------------------------------------------------------
//!
//! Protocol: `org.freedesktop.portal.FileChooser.OpenFile`/`SaveFile` returns a
//! `Request` object path; the chosen files arrive later as that request's
//! `Response` signal `(u response, a{sv} results)`, where `results["uris"]` is an
//! array of `file://` URIs. We add a match rule, call the method, then pump the
//! connection until the matching `Response` arrives.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::backend::{FileChooserBackend, FileChooserOutcome, FileChooserRequest};

// ---------------------------------------------------------------------------
// Minimal libdbus ABI
// ---------------------------------------------------------------------------

type DBusConnection = c_void;
type DBusMessage = c_void;
type DBusBool = u32; // dbus_bool_t is a 4-byte int

const DBUS_BUS_SESSION: c_int = 0;

// D-Bus type codes (the ASCII signature characters).
const DBUS_TYPE_STRING: c_int = b's' as c_int;
const DBUS_TYPE_OBJECT_PATH: c_int = b'o' as c_int;
const DBUS_TYPE_ARRAY: c_int = b'a' as c_int;
const DBUS_TYPE_DICT_ENTRY: c_int = b'e' as c_int;
const DBUS_TYPE_VARIANT: c_int = b'v' as c_int;
const DBUS_TYPE_STRUCT: c_int = b'r' as c_int;
const DBUS_TYPE_BOOLEAN: c_int = b'b' as c_int;
const DBUS_TYPE_UINT32: c_int = b'u' as c_int;
const DBUS_TYPE_BYTE: c_int = b'y' as c_int;

/// Mirror of `DBusError` (name, message, packed dummy bits, padding pointer).
#[repr(C)]
struct DBusError {
    name: *const c_char,
    message: *const c_char,
    dummy_bits: u32,
    padding1: *mut c_void,
}

/// Opaque `DBusMessageIter`. The real struct is ~56 bytes on 32-bit; this is
/// generously oversized and pointer-aligned, which is all libdbus requires.
#[repr(C)]
struct DBusMessageIter {
    _opaque: [usize; 24],
}

impl DBusMessageIter {
    fn new() -> Self {
        Self { _opaque: [0; 24] }
    }
}

#[allow(non_snake_case)]
struct Libdbus {
    error_init: unsafe extern "C" fn(*mut DBusError),
    error_is_set: unsafe extern "C" fn(*const DBusError) -> DBusBool,
    error_free: unsafe extern "C" fn(*mut DBusError),
    bus_get_private: unsafe extern "C" fn(c_int, *mut DBusError) -> *mut DBusConnection,
    connection_set_exit_on_disconnect: unsafe extern "C" fn(*mut DBusConnection, DBusBool),
    connection_close: unsafe extern "C" fn(*mut DBusConnection),
    connection_unref: unsafe extern "C" fn(*mut DBusConnection),
    bus_add_match: unsafe extern "C" fn(*mut DBusConnection, *const c_char, *mut DBusError),
    message_new_method_call:
        unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> *mut DBusMessage,
    message_unref: unsafe extern "C" fn(*mut DBusMessage),
    message_iter_init_append: unsafe extern "C" fn(*mut DBusMessage, *mut DBusMessageIter),
    message_iter_append_basic: unsafe extern "C" fn(*mut DBusMessageIter, c_int, *const c_void) -> DBusBool,
    message_iter_open_container:
        unsafe extern "C" fn(*mut DBusMessageIter, c_int, *const c_char, *mut DBusMessageIter) -> DBusBool,
    message_iter_close_container: unsafe extern "C" fn(*mut DBusMessageIter, *mut DBusMessageIter) -> DBusBool,
    message_iter_append_fixed_array:
        unsafe extern "C" fn(*mut DBusMessageIter, c_int, *const c_void, c_int) -> DBusBool,
    connection_send_with_reply_and_block:
        unsafe extern "C" fn(*mut DBusConnection, *mut DBusMessage, c_int, *mut DBusError) -> *mut DBusMessage,
    message_iter_init: unsafe extern "C" fn(*mut DBusMessage, *mut DBusMessageIter) -> DBusBool,
    message_iter_next: unsafe extern "C" fn(*mut DBusMessageIter) -> DBusBool,
    message_iter_get_arg_type: unsafe extern "C" fn(*mut DBusMessageIter) -> c_int,
    message_iter_recurse: unsafe extern "C" fn(*mut DBusMessageIter, *mut DBusMessageIter),
    message_iter_get_basic: unsafe extern "C" fn(*mut DBusMessageIter, *mut c_void),
    connection_read_write: unsafe extern "C" fn(*mut DBusConnection, c_int) -> DBusBool,
    connection_pop_message: unsafe extern "C" fn(*mut DBusConnection) -> *mut DBusMessage,
    message_is_signal: unsafe extern "C" fn(*mut DBusMessage, *const c_char, *const c_char) -> DBusBool,
    message_get_path: unsafe extern "C" fn(*mut DBusMessage) -> *const c_char,
    _handle: *mut c_void,
}

// Only read after initialization; the contained handle/pointers are never
// mutated, so sharing a &'static is sound.
unsafe impl Sync for Libdbus {}
unsafe impl Send for Libdbus {}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

fn load_libdbus() -> Option<Libdbus> {
    unsafe {
        let mut handle = dlopen(c"libdbus-1.so.3".as_ptr(), RTLD_NOW);
        if handle.is_null() {
            handle = dlopen(c"libdbus-1.so".as_ptr(), RTLD_NOW);
        }
        if handle.is_null() {
            return None;
        }

        macro_rules! sym {
            ($name:literal) => {{
                let p = dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char);
                if p.is_null() {
                    return None;
                }
                std::mem::transmute(p)
            }};
        }

        Some(Libdbus {
            error_init: sym!("dbus_error_init"),
            error_is_set: sym!("dbus_error_is_set"),
            error_free: sym!("dbus_error_free"),
            bus_get_private: sym!("dbus_bus_get_private"),
            connection_set_exit_on_disconnect: sym!("dbus_connection_set_exit_on_disconnect"),
            connection_close: sym!("dbus_connection_close"),
            connection_unref: sym!("dbus_connection_unref"),
            bus_add_match: sym!("dbus_bus_add_match"),
            message_new_method_call: sym!("dbus_message_new_method_call"),
            message_unref: sym!("dbus_message_unref"),
            message_iter_init_append: sym!("dbus_message_iter_init_append"),
            message_iter_append_basic: sym!("dbus_message_iter_append_basic"),
            message_iter_open_container: sym!("dbus_message_iter_open_container"),
            message_iter_close_container: sym!("dbus_message_iter_close_container"),
            message_iter_append_fixed_array: sym!("dbus_message_iter_append_fixed_array"),
            connection_send_with_reply_and_block: sym!("dbus_connection_send_with_reply_and_block"),
            message_iter_init: sym!("dbus_message_iter_init"),
            message_iter_next: sym!("dbus_message_iter_next"),
            message_iter_get_arg_type: sym!("dbus_message_iter_get_arg_type"),
            message_iter_recurse: sym!("dbus_message_iter_recurse"),
            message_iter_get_basic: sym!("dbus_message_iter_get_basic"),
            connection_read_write: sym!("dbus_connection_read_write"),
            connection_pop_message: sym!("dbus_connection_pop_message"),
            message_is_signal: sym!("dbus_message_is_signal"),
            message_get_path: sym!("dbus_message_get_path"),
            _handle: handle,
        })
    }
}

fn libdbus() -> Option<&'static Libdbus> {
    static LIB: OnceLock<Option<Libdbus>> = OnceLock::new();
    LIB.get_or_init(load_libdbus).as_ref()
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

pub struct LibDbusBackend;

static BACKEND: LibDbusBackend = LibDbusBackend;

pub fn backend() -> &'static LibDbusBackend {
    &BACKEND
}

impl FileChooserBackend for LibDbusBackend {
    fn run(&self, request: &FileChooserRequest) -> FileChooserOutcome {
        match libdbus() {
            Some(lib) => unsafe { run_portal(lib, request) },
            None => FileChooserOutcome::Unsupported,
        }
    }
}

const PORTAL_DEST: &CStr = c"org.freedesktop.portal.Desktop";
const PORTAL_PATH: &CStr = c"/org/freedesktop/portal/desktop";
const FILECHOOSER_IFACE: &CStr = c"org.freedesktop.portal.FileChooser";
const REQUEST_IFACE: &CStr = c"org.freedesktop.portal.Request";
const RESPONSE_MEMBER: &CStr = c"Response";

unsafe fn run_portal(lib: &Libdbus, req: &FileChooserRequest) -> FileChooserOutcome {
    let mut err: DBusError = std::mem::zeroed();
    (lib.error_init)(&mut err);

    let conn = (lib.bus_get_private)(DBUS_BUS_SESSION, &mut err);
    if conn.is_null() {
        (lib.error_free)(&mut err);
        return FileChooserOutcome::Unsupported; // no session bus
    }
    // Never let a bus disconnect take the whole Wine process down with it.
    (lib.connection_set_exit_on_disconnect)(conn, 0);

    let outcome = run_on_connection(lib, conn, req, &mut err);

    (lib.connection_close)(conn);
    (lib.connection_unref)(conn);
    (lib.error_free)(&mut err);
    outcome
}

unsafe fn run_on_connection(
    lib: &Libdbus,
    conn: *mut DBusConnection,
    req: &FileChooserRequest,
    err: &mut DBusError,
) -> FileChooserOutcome {
    // Listen for the request's Response signal before issuing the call.
    let rule = c"type='signal',interface='org.freedesktop.portal.Request',member='Response'";
    (lib.bus_add_match)(conn, rule.as_ptr(), err);
    if (lib.error_is_set)(err) != 0 {
        return FileChooserOutcome::Unsupported;
    }

    let method = if req.save { c"SaveFile" } else { c"OpenFile" };
    let msg = (lib.message_new_method_call)(
        PORTAL_DEST.as_ptr(),
        PORTAL_PATH.as_ptr(),
        FILECHOOSER_IFACE.as_ptr(),
        method.as_ptr(),
    );
    if msg.is_null() {
        return FileChooserOutcome::Unsupported;
    }
    append_args(lib, msg, req);

    // The method itself returns promptly with the Request handle (it does not
    // wait for the user), so a short timeout is fine.
    let reply = (lib.connection_send_with_reply_and_block)(conn, msg, 25_000, err);
    (lib.message_unref)(msg);
    if reply.is_null() || (lib.error_is_set)(err) != 0 {
        // No portal service on the bus, etc. -> fall back to Wine's dialog.
        if !reply.is_null() {
            (lib.message_unref)(reply);
        }
        return FileChooserOutcome::Unsupported;
    }

    let handle = read_object_path(lib, reply);
    (lib.message_unref)(reply);
    let handle = match handle {
        Some(h) => h,
        None => return FileChooserOutcome::Unsupported,
    };

    wait_for_response(lib, conn, &handle)
}

// ---- request marshaling ---------------------------------------------------

unsafe fn append_args(lib: &Libdbus, msg: *mut DBusMessage, req: &FileChooserRequest) {
    let mut top = DBusMessageIter::new();
    (lib.message_iter_init_append)(msg, &mut top);

    // parent_window: empty -> chooser is unparented (floats). Proper x11:/
    // wayland: parenting is a later refinement.
    append_string(lib, &mut top, "");
    append_string(lib, &mut top, &req.title);

    // options: a{sv}
    let mut options = DBusMessageIter::new();
    (lib.message_iter_open_container)(&mut top, DBUS_TYPE_ARRAY, c"{sv}".as_ptr(), &mut options);

    append_opt_string(lib, &mut options, "handle_token", "spritzwine0");
    append_opt_bool(lib, &mut options, "multiple", false);
    append_opt_bool(lib, &mut options, "directory", false);
    if let Some(folder) = &req.current_folder {
        append_opt_byte_array(lib, &mut options, "current_folder", folder);
    }
    if req.save {
        if let Some(name) = &req.current_name {
            append_opt_string(lib, &mut options, "current_name", name);
        }
    }
    if !req.filters.is_empty() {
        append_opt_filters(lib, &mut options, &req.filters);
    }

    (lib.message_iter_close_container)(&mut top, &mut options);
}

unsafe fn append_string(lib: &Libdbus, iter: &mut DBusMessageIter, s: &str) {
    let c = CString::new(s).unwrap_or_default();
    let ptr = c.as_ptr();
    (lib.message_iter_append_basic)(iter, DBUS_TYPE_STRING, &ptr as *const _ as *const c_void);
}

/// Open a `DICT_ENTRY` whose key is `key`, run `f` to append the variant value,
/// then close the entry.
unsafe fn append_dict_entry(
    lib: &Libdbus,
    options: &mut DBusMessageIter,
    key: &str,
    f: impl FnOnce(&mut DBusMessageIter),
) {
    let mut entry = DBusMessageIter::new();
    (lib.message_iter_open_container)(options, DBUS_TYPE_DICT_ENTRY, std::ptr::null(), &mut entry);
    append_string(lib, &mut entry, key);
    f(&mut entry);
    (lib.message_iter_close_container)(options, &mut entry);
}

/// Open a `VARIANT` of signature `sig`, run `f`, close it.
unsafe fn append_variant(
    lib: &Libdbus,
    entry: &mut DBusMessageIter,
    sig: &CStr,
    f: impl FnOnce(&mut DBusMessageIter),
) {
    let mut var = DBusMessageIter::new();
    (lib.message_iter_open_container)(entry, DBUS_TYPE_VARIANT, sig.as_ptr(), &mut var);
    f(&mut var);
    (lib.message_iter_close_container)(entry, &mut var);
}

unsafe fn append_opt_string(lib: &Libdbus, options: &mut DBusMessageIter, key: &str, val: &str) {
    append_dict_entry(lib, options, key, |entry| {
        append_variant(lib, entry, c"s", |var| append_string(lib, var, val));
    });
}

unsafe fn append_opt_bool(lib: &Libdbus, options: &mut DBusMessageIter, key: &str, val: bool) {
    append_dict_entry(lib, options, key, |entry| {
        append_variant(lib, entry, c"b", |var| {
            let b: DBusBool = val as DBusBool;
            (lib.message_iter_append_basic)(var, DBUS_TYPE_BOOLEAN, &b as *const _ as *const c_void);
        });
    });
}

/// `current_folder`/`current_file` are byte arrays (`ay`) holding a
/// NUL-terminated path.
unsafe fn append_opt_byte_array(lib: &Libdbus, options: &mut DBusMessageIter, key: &str, val: &str) {
    let mut bytes = val.as_bytes().to_vec();
    bytes.push(0);
    append_dict_entry(lib, options, key, |entry| {
        append_variant(lib, entry, c"ay", |var| {
            let mut arr = DBusMessageIter::new();
            (lib.message_iter_open_container)(var, DBUS_TYPE_ARRAY, c"y".as_ptr(), &mut arr);
            let ptr = bytes.as_ptr();
            (lib.message_iter_append_fixed_array)(
                &mut arr,
                DBUS_TYPE_BYTE,
                &ptr as *const _ as *const c_void,
                bytes.len() as c_int,
            );
            (lib.message_iter_close_container)(var, &mut arr);
        });
    });
}

/// `filters` is `a(sa(us))`: a list of (label, list of (kind, pattern)). Kind 0
/// is a glob pattern.
unsafe fn append_opt_filters(
    lib: &Libdbus,
    options: &mut DBusMessageIter,
    filters: &[(String, Vec<String>)],
) {
    append_dict_entry(lib, options, "filters", |entry| {
        append_variant(lib, entry, c"a(sa(us))", |var| {
            let mut arr = DBusMessageIter::new();
            (lib.message_iter_open_container)(var, DBUS_TYPE_ARRAY, c"(sa(us))".as_ptr(), &mut arr);
            for (label, globs) in filters {
                let mut fstruct = DBusMessageIter::new();
                (lib.message_iter_open_container)(&mut arr, DBUS_TYPE_STRUCT, std::ptr::null(), &mut fstruct);
                append_string(lib, &mut fstruct, label);

                let mut garr = DBusMessageIter::new();
                (lib.message_iter_open_container)(&mut fstruct, DBUS_TYPE_ARRAY, c"(us)".as_ptr(), &mut garr);
                for glob in globs {
                    let mut gstruct = DBusMessageIter::new();
                    (lib.message_iter_open_container)(&mut garr, DBUS_TYPE_STRUCT, std::ptr::null(), &mut gstruct);
                    let kind: u32 = 0;
                    (lib.message_iter_append_basic)(&mut gstruct, DBUS_TYPE_UINT32, &kind as *const _ as *const c_void);
                    append_string(lib, &mut gstruct, glob);
                    (lib.message_iter_close_container)(&mut garr, &mut gstruct);
                }
                (lib.message_iter_close_container)(&mut fstruct, &mut garr);
                (lib.message_iter_close_container)(&mut arr, &mut fstruct);
            }
            (lib.message_iter_close_container)(var, &mut arr);
        });
    });
}

// ---- reply / signal parsing ----------------------------------------------

unsafe fn read_object_path(lib: &Libdbus, msg: *mut DBusMessage) -> Option<String> {
    let mut iter = DBusMessageIter::new();
    if (lib.message_iter_init)(msg, &mut iter) == 0 {
        return None;
    }
    if (lib.message_iter_get_arg_type)(&mut iter) != DBUS_TYPE_OBJECT_PATH {
        return None;
    }
    let mut p: *const c_char = std::ptr::null();
    (lib.message_iter_get_basic)(&mut iter, &mut p as *mut _ as *mut c_void);
    if p.is_null() {
        return None;
    }
    Some(CStr::from_ptr(p).to_string_lossy().into_owned())
}

unsafe fn wait_for_response(lib: &Libdbus, conn: *mut DBusConnection, handle: &str) -> FileChooserOutcome {
    // Cap the wait so a wedged portal can't hang the app forever.
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        loop {
            let msg = (lib.connection_pop_message)(conn);
            if msg.is_null() {
                break;
            }
            let is_response =
                (lib.message_is_signal)(msg, REQUEST_IFACE.as_ptr(), RESPONSE_MEMBER.as_ptr()) != 0;
            if is_response && signal_path_matches(lib, msg, handle) {
                let outcome = parse_response(lib, msg);
                (lib.message_unref)(msg);
                return outcome;
            }
            (lib.message_unref)(msg);
        }

        if Instant::now() >= deadline {
            return FileChooserOutcome::Cancelled;
        }
        // Pull I/O without dispatching (we read the queue ourselves). FALSE
        // means the connection dropped.
        if (lib.connection_read_write)(conn, 250) == 0 {
            return FileChooserOutcome::Cancelled;
        }
    }
}

unsafe fn signal_path_matches(lib: &Libdbus, msg: *mut DBusMessage, handle: &str) -> bool {
    let path = (lib.message_get_path)(msg);
    !path.is_null() && CStr::from_ptr(path).to_bytes() == handle.as_bytes()
}

unsafe fn parse_response(lib: &Libdbus, msg: *mut DBusMessage) -> FileChooserOutcome {
    let mut iter = DBusMessageIter::new();
    if (lib.message_iter_init)(msg, &mut iter) == 0 {
        return FileChooserOutcome::Cancelled;
    }
    if (lib.message_iter_get_arg_type)(&mut iter) != DBUS_TYPE_UINT32 {
        return FileChooserOutcome::Cancelled;
    }
    let mut response: u32 = 0;
    (lib.message_iter_get_basic)(&mut iter, &mut response as *mut _ as *mut c_void);
    if response != 0 {
        // 1 = cancelled by user, 2 = ended some other way.
        return FileChooserOutcome::Cancelled;
    }

    if (lib.message_iter_next)(&mut iter) == 0
        || (lib.message_iter_get_arg_type)(&mut iter) != DBUS_TYPE_ARRAY
    {
        return FileChooserOutcome::Cancelled;
    }
    let uris = extract_uris(lib, &mut iter);
    if uris.is_empty() {
        FileChooserOutcome::Cancelled
    } else {
        FileChooserOutcome::Chosen(uris)
    }
}

/// Walk the `a{sv}` results and pull the `uris` value (`as`).
unsafe fn extract_uris(lib: &Libdbus, results: &mut DBusMessageIter) -> Vec<String> {
    let mut out = Vec::new();
    let mut dict = DBusMessageIter::new();
    (lib.message_iter_recurse)(results, &mut dict);

    while (lib.message_iter_get_arg_type)(&mut dict) == DBUS_TYPE_DICT_ENTRY {
        let mut entry = DBusMessageIter::new();
        (lib.message_iter_recurse)(&mut dict, &mut entry);

        if (lib.message_iter_get_arg_type)(&mut entry) == DBUS_TYPE_STRING {
            let mut kp: *const c_char = std::ptr::null();
            (lib.message_iter_get_basic)(&mut entry, &mut kp as *mut _ as *mut c_void);
            let key = if kp.is_null() {
                String::new()
            } else {
                CStr::from_ptr(kp).to_string_lossy().into_owned()
            };

            (lib.message_iter_next)(&mut entry); // -> variant value
            if key == "uris" && (lib.message_iter_get_arg_type)(&mut entry) == DBUS_TYPE_VARIANT {
                let mut var = DBusMessageIter::new();
                (lib.message_iter_recurse)(&mut entry, &mut var);
                if (lib.message_iter_get_arg_type)(&mut var) == DBUS_TYPE_ARRAY {
                    let mut arr = DBusMessageIter::new();
                    (lib.message_iter_recurse)(&mut var, &mut arr);
                    while (lib.message_iter_get_arg_type)(&mut arr) == DBUS_TYPE_STRING {
                        let mut sp: *const c_char = std::ptr::null();
                        (lib.message_iter_get_basic)(&mut arr, &mut sp as *mut _ as *mut c_void);
                        if !sp.is_null() {
                            out.push(CStr::from_ptr(sp).to_string_lossy().into_owned());
                        }
                        (lib.message_iter_next)(&mut arr);
                    }
                }
            }
        }
        (lib.message_iter_next)(&mut dict);
    }

    out
}
