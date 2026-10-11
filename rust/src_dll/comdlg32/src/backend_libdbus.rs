//! NOTE - D-BUS TRANSPORT (read before extending this layer)
//! ----------------------------------------------------------------------------
//! This is the *safe* portal backend: a hand-rolled libdbus client, `dlopen`ed
//! at runtime and driven **synchronously on the calling thread**, so no async
//! runtime and no extra background thread ever runs inside Wine's address
//! space. That matters because Wine's Unix side has its own threading and
//! signal machinery; this mirrors how Wine's own `mountmgr.sys` /
//! `winebth.sys` talk to D-Bus.
//!
//! The traits in [`crate::backend`] exist precisely so this can be replaced.
//! The marshaling here is kept generic (one value reader, one `a{sv}` writer)
//! so that a new portal call is a few lines, not a new parser. **Before growing
//! it beyond that** (notifications, streams, properties, ...), add a
//! `backend_zbus.rs` built on the widely-used **`zbus`** stack behind a cargo
//! feature and select it in `crate::backend::active()`, once its in-Wine
//! threading is validated (drive zbus's executor with a manual
//! `executor().tick()` loop so it stays thread-free, like this backend).
//! ----------------------------------------------------------------------------
//!
//! Protocol: every portal method returns a `Request` object path at once; the
//! user's answer arrives later as that request's `Response` signal
//! `(u response, a{sv} results)`. We add a match rule, call the method, then
//! pump the connection until the matching `Response` arrives.

#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::backend::{FileChooserBackend, FileChooserOutcome, FileChooserRequest, PrintBackend, Value};
use crate::print::{self, DeliveryOutcome, PrintOutcome, PrintRequest};
use std::os::fd::AsRawFd;

// ---------------------------------------------------------------------------
// Minimal libdbus ABI
// ---------------------------------------------------------------------------

type DBusConnection = c_void;
type DBusMessage = c_void;
type DBusBool = u32; // dbus_bool_t is a 4-byte int

const DBUS_BUS_SESSION: c_int = 0;

// D-Bus type codes (the ASCII signature characters).
const DBUS_TYPE_INVALID: c_int = 0;
const DBUS_TYPE_STRING: c_int = b's' as c_int;
const DBUS_TYPE_OBJECT_PATH: c_int = b'o' as c_int;
const DBUS_TYPE_SIGNATURE: c_int = b'g' as c_int;
const DBUS_TYPE_ARRAY: c_int = b'a' as c_int;
const DBUS_TYPE_DICT_ENTRY: c_int = b'e' as c_int;
const DBUS_TYPE_VARIANT: c_int = b'v' as c_int;
const DBUS_TYPE_STRUCT: c_int = b'r' as c_int;
const DBUS_TYPE_BOOLEAN: c_int = b'b' as c_int;
const DBUS_TYPE_UINT32: c_int = b'u' as c_int;
const DBUS_TYPE_INT32: c_int = b'i' as c_int;
const DBUS_TYPE_DOUBLE: c_int = b'd' as c_int;
const DBUS_TYPE_BYTE: c_int = b'y' as c_int;
const DBUS_TYPE_UNIX_FD: c_int = b'h' as c_int;

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
    message_iter_get_element_type: unsafe extern "C" fn(*mut DBusMessageIter) -> c_int,
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
            message_iter_get_element_type: sym!("dbus_message_iter_get_element_type"),
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

const PORTAL_DEST: &CStr = c"org.freedesktop.portal.Desktop";
const PORTAL_PATH: &CStr = c"/org/freedesktop/portal/desktop";
const FILECHOOSER_IFACE: &CStr = c"org.freedesktop.portal.FileChooser";
const PRINT_IFACE: &CStr = c"org.freedesktop.portal.Print";
const REQUEST_IFACE: &CStr = c"org.freedesktop.portal.Request";
const RESPONSE_MEMBER: &CStr = c"Response";

// Portal response codes: 0 = success, 1 = cancelled by the user, 2 = other.
const RESPONSE_SUCCESS: u32 = 0;

impl FileChooserBackend for LibDbusBackend {
    fn run(&self, req: &FileChooserRequest) -> FileChooserOutcome {
        let Some(lib) = libdbus() else {
            return FileChooserOutcome::Unsupported;
        };
        let method = if req.save { c"SaveFile" } else { c"OpenFile" };
        let reply = unsafe {
            call_portal(lib, FILECHOOSER_IFACE, method, |lib, top| append_file_chooser_args(lib, top, req))
        };
        match reply {
            Err(Failure::Unsupported) => FileChooserOutcome::Unsupported,
            Err(Failure::NoAnswer) => FileChooserOutcome::Cancelled,
            Ok(reply) if reply.code != RESPONSE_SUCCESS => FileChooserOutcome::Cancelled,
            Ok(reply) => {
                let uris: Vec<String> = match reply.results.get("uris") {
                    Some(Value::Array(items)) => {
                        items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()
                    }
                    _ => Vec::new(),
                };
                if uris.is_empty() {
                    return FileChooserOutcome::Cancelled;
                }
                // `current_filter` is a `(sa(us))`; match the request's filters
                // on its name.
                let filter_index = match reply.results.get("current_filter") {
                    Some(Value::Struct(fields)) => fields
                        .first()
                        .and_then(Value::as_str)
                        .and_then(|name| req.filters.iter().position(|(label, _)| label == name)),
                    _ => None,
                };
                FileChooserOutcome::Chosen { uris, filter_index }
            }
        }
    }
}

impl PrintBackend for LibDbusBackend {
    fn prepare_print(&self, req: &PrintRequest) -> PrintOutcome {
        let Some(lib) = libdbus() else {
            return PrintOutcome::Unsupported;
        };
        let reply = unsafe {
            call_portal(lib, PRINT_IFACE, c"PreparePrint", |lib, top| append_prepare_print_args(lib, top, req))
        };
        match reply {
            Err(Failure::Unsupported) => PrintOutcome::Unsupported,
            Err(Failure::NoAnswer) => PrintOutcome::Cancelled,
            Ok(reply) if reply.code != RESPONSE_SUCCESS => PrintOutcome::Cancelled,
            Ok(reply) => match print::choice_from_results(&reply.results) {
                Some(choice) => PrintOutcome::Chosen(choice),
                None => PrintOutcome::Cancelled,
            },
        }
    }

    fn print_file(&self, path: &str, title: &str, token: Option<u32>) -> DeliveryOutcome {
        let Some(lib) = libdbus() else {
            return DeliveryOutcome::Unsupported;
        };
        let Ok(file) = std::fs::File::open(path) else {
            return DeliveryOutcome::Unsupported;
        };
        let reply = unsafe {
            call_portal(lib, PRINT_IFACE, c"Print", |lib, top| {
                append_print_args(lib, top, title, file.as_raw_fd(), token)
            })
        };
        match reply {
            Err(Failure::Unsupported) => DeliveryOutcome::Unsupported,
            // A dropped bus after the call went out, or an unreadable
            // response: the backend may well have printed, do not re-deliver.
            Err(Failure::NoAnswer) | Ok(_) => DeliveryOutcome::Delivered,
        }
    }
}

// ---------------------------------------------------------------------------
// Generic portal call: method -> Request handle -> Response signal
// ---------------------------------------------------------------------------

struct Reply {
    code: u32,
    results: Value,
}

enum Failure {
    /// No bus, no portal service, malformed reply: fall back to Wine's dialog.
    Unsupported,
    /// The bus went away or the portal never answered: treat as cancelled.
    NoAnswer,
}

unsafe fn call_portal(
    lib: &Libdbus,
    iface: &CStr,
    method: &CStr,
    append: impl FnOnce(&Libdbus, &mut DBusMessageIter),
) -> Result<Reply, Failure> {
    let mut err: DBusError = std::mem::zeroed();
    (lib.error_init)(&mut err);

    let conn = (lib.bus_get_private)(DBUS_BUS_SESSION, &mut err);
    if conn.is_null() {
        (lib.error_free)(&mut err);
        return Err(Failure::Unsupported); // no session bus
    }
    // Never let a bus disconnect take the whole Wine process down with it.
    (lib.connection_set_exit_on_disconnect)(conn, 0);

    let result = call_on_connection(lib, conn, iface, method, append, &mut err);

    (lib.connection_close)(conn);
    (lib.connection_unref)(conn);
    (lib.error_free)(&mut err);
    result
}

unsafe fn call_on_connection(
    lib: &Libdbus,
    conn: *mut DBusConnection,
    iface: &CStr,
    method: &CStr,
    append: impl FnOnce(&Libdbus, &mut DBusMessageIter),
    err: &mut DBusError,
) -> Result<Reply, Failure> {
    // Listen for the request's Response signal before issuing the call.
    let rule = c"type='signal',interface='org.freedesktop.portal.Request',member='Response'";
    (lib.bus_add_match)(conn, rule.as_ptr(), err);
    if (lib.error_is_set)(err) != 0 {
        return Err(Failure::Unsupported);
    }

    let msg = (lib.message_new_method_call)(PORTAL_DEST.as_ptr(), PORTAL_PATH.as_ptr(), iface.as_ptr(), method.as_ptr());
    if msg.is_null() {
        return Err(Failure::Unsupported);
    }
    let mut top = DBusMessageIter::new();
    (lib.message_iter_init_append)(msg, &mut top);
    append(lib, &mut top);

    // The method itself returns promptly with the Request handle (it does not
    // wait for the user), so a short timeout is fine.
    let reply = (lib.connection_send_with_reply_and_block)(conn, msg, 25_000, err);
    (lib.message_unref)(msg);
    if reply.is_null() || (lib.error_is_set)(err) != 0 {
        // No portal service on the bus, etc. -> fall back to Wine's dialog.
        if !reply.is_null() {
            (lib.message_unref)(reply);
        }
        return Err(Failure::Unsupported);
    }

    let handle = read_object_path(lib, reply);
    (lib.message_unref)(reply);
    let handle = handle.ok_or(Failure::Unsupported)?;

    wait_for_response(lib, conn, &handle).ok_or(Failure::NoAnswer)
}

unsafe fn wait_for_response(lib: &Libdbus, conn: *mut DBusConnection, handle: &str) -> Option<Reply> {
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
                let reply = parse_response(lib, msg);
                (lib.message_unref)(msg);
                return reply;
            }
            (lib.message_unref)(msg);
        }

        if Instant::now() >= deadline {
            return None;
        }
        // Pull I/O without dispatching (we read the queue ourselves). FALSE
        // means the connection dropped.
        if (lib.connection_read_write)(conn, 250) == 0 {
            return None;
        }
    }
}

unsafe fn signal_path_matches(lib: &Libdbus, msg: *mut DBusMessage, handle: &str) -> bool {
    let path = (lib.message_get_path)(msg);
    !path.is_null() && CStr::from_ptr(path).to_bytes() == handle.as_bytes()
}

/// `Response(u code, a{sv} results)`.
unsafe fn parse_response(lib: &Libdbus, msg: *mut DBusMessage) -> Option<Reply> {
    let mut iter = DBusMessageIter::new();
    if (lib.message_iter_init)(msg, &mut iter) == 0 {
        return None;
    }
    let code = match read_value(lib, &mut iter) {
        Value::U32(code) => code,
        _ => return None,
    };
    let results = if (lib.message_iter_next)(&mut iter) != 0 {
        read_value(lib, &mut iter)
    } else {
        Value::Dict(Vec::new())
    };
    Some(Reply { code, results })
}

unsafe fn read_object_path(lib: &Libdbus, msg: *mut DBusMessage) -> Option<String> {
    let mut iter = DBusMessageIter::new();
    if (lib.message_iter_init)(msg, &mut iter) == 0 {
        return None;
    }
    if (lib.message_iter_get_arg_type)(&mut iter) != DBUS_TYPE_OBJECT_PATH {
        return None;
    }
    iter_string(lib, &mut iter)
}

// ---- generic value reader -------------------------------------------------

/// Decode the value at the iterator's current position (not advancing it).
unsafe fn read_value(lib: &Libdbus, iter: &mut DBusMessageIter) -> Value {
    match (lib.message_iter_get_arg_type)(iter) {
        DBUS_TYPE_STRING | DBUS_TYPE_OBJECT_PATH | DBUS_TYPE_SIGNATURE => {
            Value::Str(iter_string(lib, iter).unwrap_or_default())
        }
        DBUS_TYPE_BOOLEAN => Value::Bool(iter_basic::<DBusBool>(lib, iter) != 0),
        DBUS_TYPE_UINT32 => Value::U32(iter_basic::<u32>(lib, iter)),
        DBUS_TYPE_INT32 => Value::I32(iter_basic::<i32>(lib, iter)),
        DBUS_TYPE_DOUBLE => Value::Double(iter_basic::<f64>(lib, iter)),
        DBUS_TYPE_VARIANT => {
            let mut inner = DBusMessageIter::new();
            (lib.message_iter_recurse)(iter, &mut inner);
            read_value(lib, &mut inner)
        }
        DBUS_TYPE_ARRAY => {
            let is_dict = (lib.message_iter_get_element_type)(iter) == DBUS_TYPE_DICT_ENTRY;
            let mut sub = DBusMessageIter::new();
            (lib.message_iter_recurse)(iter, &mut sub);
            if is_dict {
                let mut entries = Vec::new();
                while (lib.message_iter_get_arg_type)(&mut sub) == DBUS_TYPE_DICT_ENTRY {
                    let mut entry = DBusMessageIter::new();
                    (lib.message_iter_recurse)(&mut sub, &mut entry);
                    let key = read_value(lib, &mut entry);
                    let val = if (lib.message_iter_next)(&mut entry) != 0 {
                        read_value(lib, &mut entry)
                    } else {
                        Value::Other
                    };
                    if let Value::Str(key) = key {
                        entries.push((key, val));
                    }
                    (lib.message_iter_next)(&mut sub);
                }
                Value::Dict(entries)
            } else {
                Value::Array(read_sequence(lib, &mut sub))
            }
        }
        DBUS_TYPE_STRUCT => {
            let mut sub = DBusMessageIter::new();
            (lib.message_iter_recurse)(iter, &mut sub);
            Value::Struct(read_sequence(lib, &mut sub))
        }
        _ => Value::Other,
    }
}

/// Decode every element of a container iterator.
unsafe fn read_sequence(lib: &Libdbus, sub: &mut DBusMessageIter) -> Vec<Value> {
    let mut items = Vec::new();
    while (lib.message_iter_get_arg_type)(sub) != DBUS_TYPE_INVALID {
        items.push(read_value(lib, sub));
        if (lib.message_iter_next)(sub) == 0 {
            break;
        }
    }
    items
}

/// Read a fixed-size basic value. libdbus writes exactly the type's size.
unsafe fn iter_basic<T: Copy + Default>(lib: &Libdbus, iter: &mut DBusMessageIter) -> T {
    let mut v = T::default();
    (lib.message_iter_get_basic)(iter, &mut v as *mut T as *mut c_void);
    v
}

/// Read the string at a basic-string iterator position.
unsafe fn iter_string(lib: &Libdbus, iter: &mut DBusMessageIter) -> Option<String> {
    let mut p: *const c_char = std::ptr::null();
    (lib.message_iter_get_basic)(iter, &mut p as *mut _ as *mut c_void);
    (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Request marshaling
// ---------------------------------------------------------------------------

// parent_window: empty -> the dialog floats instead of being modal over the
// app window. Proper parenting needs an `x11:<xid>` or `wayland:<handle>`
// identifier; the Wayland handle requires xdg_foreign export support in Wine's
// Wayland driver, which doesn't exist yet, so this is deferred.
const NO_PARENT: &str = "";

/// `OpenFile`/`SaveFile(s parent, s title, a{sv} options)`.
unsafe fn append_file_chooser_args(lib: &Libdbus, top: &mut DBusMessageIter, req: &FileChooserRequest) {
    append_string(lib, top, NO_PARENT);
    append_string(lib, top, &req.title);

    let mut options = DBusMessageIter::new();
    (lib.message_iter_open_container)(top, DBUS_TYPE_ARRAY, c"{sv}".as_ptr(), &mut options);

    append_opt_string(lib, &mut options, "handle_token", "spritzwine0");
    append_opt_bool(lib, &mut options, "multiple", req.multiple);
    append_opt_bool(lib, &mut options, "directory", req.directory);
    if let Some(folder) = &req.current_folder {
        append_opt_byte_array(lib, &mut options, "current_folder", folder);
    }
    if let Some(name) = req.current_name.as_ref().filter(|_| req.save) {
        append_opt_string(lib, &mut options, "current_name", name);
    }
    if !req.filters.is_empty() {
        append_opt_filters(lib, &mut options, &req.filters);
        if let Some(filter) = req.current_filter.and_then(|i| req.filters.get(i)) {
            append_opt_current_filter(lib, &mut options, filter);
        }
    }

    (lib.message_iter_close_container)(top, &mut options);
}

/// `PreparePrint(s parent, s title, a{sv} settings, a{sv} page_setup, a{sv} options)`.
unsafe fn append_prepare_print_args(lib: &Libdbus, top: &mut DBusMessageIter, req: &PrintRequest) {
    append_string(lib, top, NO_PARENT);
    append_string(lib, top, &req.title);
    append_sv_dict(lib, top, &print::settings_hints(req));
    append_sv_dict(lib, top, &print::page_setup_hints(req));

    // wineps produces PostScript, so print-to-file must yield a .ps; an empty
    // list means the app allows no print-to-file at all.
    let formats = if req.print_to_file { vec![Value::Str("ps".into())] } else { Vec::new() };
    let options = [
        ("handle_token".to_string(), Value::Str("spritzwine1".into())),
        ("modal".to_string(), Value::Bool(true)),
        ("supported_output_file_formats".to_string(), Value::Array(formats)),
        ("has_current_page".to_string(), Value::Bool(req.has_current_page)),
        ("has_selected_pages".to_string(), Value::Bool(req.has_selected_pages)),
    ];
    append_sv_dict(lib, top, &options);
}

/// `Print(s parent, s title, h fd, a{sv} options)`. libdbus duplicates the fd
/// into the message, so the caller keeps ownership of `fd`.
unsafe fn append_print_args(lib: &Libdbus, top: &mut DBusMessageIter, title: &str, fd: c_int, token: Option<u32>) {
    append_string(lib, top, NO_PARENT);
    append_string(lib, top, title);
    (lib.message_iter_append_basic)(top, DBUS_TYPE_UNIX_FD, &fd as *const c_int as *const c_void);

    let mut options = vec![
        ("handle_token".to_string(), Value::Str("spritzwine2".into())),
        ("modal".to_string(), Value::Bool(true)),
    ];
    if let Some(token) = token {
        options.push(("token".to_string(), Value::U32(token)));
    }
    append_sv_dict(lib, top, &options);
}

/// Append an `a{sv}` built from values. Strings, booleans, uint32s, doubles
/// and string arrays are supported; anything else is skipped.
unsafe fn append_sv_dict(lib: &Libdbus, parent: &mut DBusMessageIter, entries: &[(String, Value)]) {
    let mut dict = DBusMessageIter::new();
    (lib.message_iter_open_container)(parent, DBUS_TYPE_ARRAY, c"{sv}".as_ptr(), &mut dict);
    for (key, value) in entries {
        match value {
            Value::Str(s) => append_opt_string(lib, &mut dict, key, s),
            Value::Bool(b) => append_opt_bool(lib, &mut dict, key, *b),
            Value::U32(u) => append_dict_entry(lib, &mut dict, key, |entry| {
                append_variant(lib, entry, c"u", |var| {
                    (lib.message_iter_append_basic)(var, DBUS_TYPE_UINT32, u as *const u32 as *const c_void);
                });
            }),
            Value::Double(d) => append_dict_entry(lib, &mut dict, key, |entry| {
                append_variant(lib, entry, c"d", |var| {
                    (lib.message_iter_append_basic)(var, DBUS_TYPE_DOUBLE, d as *const f64 as *const c_void);
                });
            }),
            Value::Array(items) => append_dict_entry(lib, &mut dict, key, |entry| {
                append_variant(lib, entry, c"as", |var| {
                    let mut arr = DBusMessageIter::new();
                    (lib.message_iter_open_container)(var, DBUS_TYPE_ARRAY, c"s".as_ptr(), &mut arr);
                    for item in items {
                        if let Some(s) = item.as_str() {
                            append_string(lib, &mut arr, s);
                        }
                    }
                    (lib.message_iter_close_container)(var, &mut arr);
                });
            }),
            _ => {}
        }
    }
    (lib.message_iter_close_container)(parent, &mut dict);
}

/// `current_filter` is a single `(sa(us))` (name, list of (kind, pattern)).
unsafe fn append_opt_current_filter(
    lib: &Libdbus,
    options: &mut DBusMessageIter,
    filter: &(String, Vec<String>),
) {
    append_dict_entry(lib, options, "current_filter", |entry| {
        append_variant(lib, entry, c"(sa(us))", |var| append_filter_struct(lib, var, filter));
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
            for filter in filters {
                append_filter_struct(lib, &mut arr, filter);
            }
            (lib.message_iter_close_container)(var, &mut arr);
        });
    });
}

/// One `(sa(us))` filter struct.
unsafe fn append_filter_struct(lib: &Libdbus, parent: &mut DBusMessageIter, filter: &(String, Vec<String>)) {
    let mut fstruct = DBusMessageIter::new();
    (lib.message_iter_open_container)(parent, DBUS_TYPE_STRUCT, std::ptr::null(), &mut fstruct);
    append_string(lib, &mut fstruct, &filter.0);
    let mut garr = DBusMessageIter::new();
    (lib.message_iter_open_container)(&mut fstruct, DBUS_TYPE_ARRAY, c"(us)".as_ptr(), &mut garr);
    for glob in &filter.1 {
        let mut gstruct = DBusMessageIter::new();
        (lib.message_iter_open_container)(&mut garr, DBUS_TYPE_STRUCT, std::ptr::null(), &mut gstruct);
        let kind: u32 = 0;
        (lib.message_iter_append_basic)(&mut gstruct, DBUS_TYPE_UINT32, &kind as *const _ as *const c_void);
        append_string(lib, &mut gstruct, glob);
        (lib.message_iter_close_container)(&mut garr, &mut gstruct);
    }
    (lib.message_iter_close_container)(&mut fstruct, &mut garr);
    (lib.message_iter_close_container)(parent, &mut fstruct);
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
