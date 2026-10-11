//! Backend seam for the portal dialogs (file chooser, print).
//!
//! The rest of the crate is written against the traits here so the transport
//! that actually talks to the host can be swapped without touching the FFI /
//! marshaling code. Today the only backend is [`crate::backend_libdbus`], a
//! hand-rolled libdbus client chosen for safety inside Wine's address space;
//! see its module-level NOTE for when/why a `zbus`-based backend would be added
//! instead.

use crate::print::{DeliveryOutcome, PrintOutcome, PrintRequest};

/// A decoded D-Bus value, as far as the portal replies need. Produced by the
/// transport, consumed by the pure mapping code so that code stays testable
/// without a bus.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Bool(bool),
    U32(u32),
    I32(i32),
    Double(f64),
    Array(Vec<Value>),
    /// An `a{sv}`-style dictionary (string keys), in message order.
    Dict(Vec<(String, Value)>),
    Struct(Vec<Value>),
    /// Anything this crate has no use for.
    Other,
}

impl Value {
    /// Look up `key` in a dictionary value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(v) => Some(*v),
            Value::I32(v) if *v >= 0 => Some(*v as u32),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Double(v) => Some(*v),
            Value::U32(v) => Some(*v as f64),
            Value::I32(v) => Some(*v as f64),
            _ => None,
        }
    }
}

/// A backend-neutral file-chooser request, built from the PE-side
/// `file_chooser_params` (see `lib.rs`).
pub struct FileChooserRequest {
    /// `true` for a Save dialog, `false` for Open.
    pub save: bool,
    /// Allow selecting more than one file.
    pub multiple: bool,
    /// Pick a directory rather than a file.
    pub directory: bool,
    /// Dialog title (may be empty).
    pub title: String,
    /// Directory to start in, as a host path (e.g. `/home/user`).
    pub current_folder: Option<String>,
    /// Suggested file name for a Save dialog.
    pub current_name: Option<String>,
    /// `(label, globs)` pairs, e.g. `("Text", ["*.txt", "*.log"])`.
    pub filters: Vec<(String, Vec<String>)>,
    /// 0-based index into `filters` to pre-select, if any.
    pub current_filter: Option<usize>,
}

/// Outcome of showing the chooser.
pub enum FileChooserOutcome {
    /// The user accepted.
    Chosen {
        /// URIs (typically `file://...`) in selection order.
        uris: Vec<String>,
        /// 0-based index into the request's `filters` that the user had active,
        /// if the portal reported one.
        filter_index: Option<usize>,
    },
    /// The user dismissed / cancelled the dialog.
    Cancelled,
    /// No usable portal on the bus: the caller should fall back to Wine's own
    /// dialog.
    Unsupported,
}

/// A transport that can show a host file chooser and report what was picked.
pub trait FileChooserBackend {
    fn run(&self, request: &FileChooserRequest) -> FileChooserOutcome;
}

/// A transport that can show the host print dialog (`PreparePrint`) and hand
/// a finished document to it (`Print`).
pub trait PrintBackend {
    fn prepare_print(&self, request: &PrintRequest) -> PrintOutcome;
    /// `token` skips the dialog; without one the portal shows it again.
    fn print_file(&self, path: &str, title: &str, token: Option<u32>) -> DeliveryOutcome;
}

/// The active backend. This is the single seam point: swapping transports (e.g.
/// to a future `zbus` backend) is a change here, not across the crate.
pub fn active() -> &'static (impl FileChooserBackend + PrintBackend) {
    crate::backend_libdbus::backend()
}
