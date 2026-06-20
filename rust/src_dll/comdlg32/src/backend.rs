//! Backend seam for the file chooser.
//!
//! The rest of the crate is written against [`FileChooserBackend`] so the
//! transport that actually talks to the host can be swapped without touching the
//! FFI / marshaling code. Today the only backend is [`crate::backend_libdbus`],
//! a hand-rolled libdbus client chosen for safety inside Wine's address space;
//! see its module-level NOTE for when/why a `zbus`-based backend would be added
//! instead.

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
        /// URIs (typically `file://…`) in selection order.
        uris: Vec<String>,
        /// 0-based index into the request's `filters` that the user had active,
        /// if the portal reported one.
        filter_index: Option<usize>,
    },
    /// The user dismissed / cancelled the dialog.
    Cancelled,
    /// No usable portal on the bus — the caller should fall back to Wine's own
    /// dialog.
    Unsupported,
}

/// A transport that can show a host file chooser and report what was picked.
pub trait FileChooserBackend {
    fn run(&self, request: &FileChooserRequest) -> FileChooserOutcome;
}

/// The active backend. This is the single seam point: swapping transports (e.g.
/// to a future `zbus` backend) is a change here, not across the crate.
pub fn active() -> &'static dyn FileChooserBackend {
    crate::backend_libdbus::backend()
}
