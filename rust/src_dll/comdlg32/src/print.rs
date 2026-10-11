//! Print dialog: request/outcome types and the pure mapping between the
//! portal's `PreparePrint` vocabulary and what `PrintDlg` needs to fill a
//! `DEVMODE`. No D-Bus here, so all of it is unit-tested on the host.
//!
//! Portal conventions this relies on (verified against the KDE and GTK
//! backends, 2026-10-11): `settings` values are all strings in GTK's
//! `GtkPrintSettings` spelling; `page-setup` carries `Width`/`Height`/margins
//! as doubles in millimetres plus `Orientation`; the host applies copies,
//! collate, duplex and n-up itself when the document is later handed to
//! `Print`, while page ranges are the application's job.

use crate::backend::Value;
use crate::convert;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Unknown,
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Duplex {
    #[default]
    Simplex,
    LongEdge,
    ShortEdge,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pages {
    #[default]
    All,
    /// The portal's `page-ranges` string, e.g. `1-3,5`, kept verbatim.
    Ranges(String),
    Current,
    Selection,
}

/// What the PE side knows before the dialog opens. Only hints that every
/// backend parses the same way are carried: boolean hints are spelled
/// "true"/"false" by GTK but "yes"/"no" by KDE, so none are sent.
#[derive(Debug, Clone, Default)]
pub struct PrintRequest {
    pub title: String,
    /// Initial copy count hint, 0 = none.
    pub copies: u32,
    pub orientation: Orientation,
    /// Whether "current page" / "selection" make sense for this app
    /// (`PD_NOCURRENTPAGE` / `PD_NOSELECTION` inverted).
    pub has_current_page: bool,
    pub has_selected_pages: bool,
    /// Whether the app allows print-to-file (`PD_DISABLEPRINTTOFILE` /
    /// `PD_HIDEPRINTTOFILE` not set). Honoured by backends that read
    /// `supported_output_file_formats` (GTK); KDE always offers it.
    pub print_to_file: bool,
}

/// What the user chose. Everything the host will apply itself (copies,
/// collate, duplex) is still reported so the app's `DEVMODE` can mirror it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrintChoice {
    /// Handed back to the portal's `Print` call to skip the dialog.
    pub token: u32,
    pub copies: u32,
    pub collate: bool,
    pub reverse: bool,
    pub duplex: Duplex,
    pub color: bool,
    /// dpi, 0 = not reported.
    pub resolution: u32,
    pub pages: Pages,
    pub orientation: Orientation,
    /// PPD name when the portal gives one, else the display name; may be empty.
    pub paper_name: String,
    /// Tenths of a millimetre (the `DEVMODE` unit), 0 = not reported.
    pub paper_width: u32,
    pub paper_height: u32,
    /// Host path when the user chose print-to-file.
    pub output_file: Option<String>,
}

pub enum PrintOutcome {
    Chosen(PrintChoice),
    Cancelled,
    /// No usable portal: the caller falls back to Wine's own dialog.
    Unsupported,
}

/// Outcome of handing a finished document to the portal's `Print`.
pub enum DeliveryOutcome {
    /// The portal took the document. The response code is deliberately not
    /// interpreted: the KDE backend returns `lpr`'s exit status in it, so 0 and
    /// "success" mean opposite things there.
    Delivered,
    /// No usable portal: the caller must deliver the job some other way.
    Unsupported,
}

// ---------------------------------------------------------------------------
// Token handoff between the dialog (comdlg32.so) and the spooler (localspl.so)
// ---------------------------------------------------------------------------
//
// Both unix libs link this one shared object, so a token normally travels in
// memory. The file copy covers an app that prints from a different process
// than the one that showed the dialog.

//
// A token is only meant for the job right after its dialog. An app that showed
// the dialog but never printed would otherwise leave one behind, and a later
// job (from any process) would silently go to that stale choice; hence the
// expiry.

/// (token, unix seconds when it was issued)
static LAST_TOKEN: std::sync::Mutex<Option<(u32, u64)>> = std::sync::Mutex::new(None);

const TOKEN_LIFETIME_SECS: u64 = 15 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn token_is_fresh(issued: u64, now: u64) -> bool {
    now >= issued && now - issued <= TOKEN_LIFETIME_SECS
}

fn token_file() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| std::path::PathBuf::from(dir).join("spritzwine-print-token"))
}

/// Keep `token` for the job that follows the dialog.
pub fn remember_token(token: u32) {
    let now = now_secs();
    *LAST_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((token, now));
    if let Some(path) = token_file() {
        let _ = std::fs::write(path, format!("{token} {now}"));
    }
}

/// Hand out the pending token, if there is a fresh one; it is consumed.
pub fn take_token() -> Option<u32> {
    let now = now_secs();
    let in_memory = LAST_TOKEN.lock().unwrap_or_else(|e| e.into_inner()).take();
    let path = token_file();
    let on_disk = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(p) = path {
        let _ = std::fs::remove_file(p);
    }
    in_memory
        .or_else(|| on_disk.and_then(|s| parse_token_file(&s)))
        .filter(|&(_, issued)| token_is_fresh(issued, now))
        .map(|(token, _)| token)
}

/// "<token> <unix seconds>"; a bare token (no timestamp) counts as stale.
fn parse_token_file(s: &str) -> Option<(u32, u64)> {
    let mut parts = s.split_whitespace();
    let token = parts.next()?.parse().ok()?;
    let issued = parts.next()?.parse().ok()?;
    Some((token, issued))
}

/// Initial `settings` for `PreparePrint`, in the portal's string spelling.
pub fn settings_hints(req: &PrintRequest) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if req.copies > 0 {
        out.push(("n-copies".into(), Value::Str(req.copies.to_string())));
    }
    if let Some(o) = orientation_str(req.orientation) {
        out.push(("orientation".into(), Value::Str(o.into())));
    }
    out
}

/// Initial `page_setup` for `PreparePrint`.
pub fn page_setup_hints(req: &PrintRequest) -> Vec<(String, Value)> {
    match orientation_str(req.orientation) {
        Some(o) => vec![("Orientation".into(), Value::Str(o.into()))],
        None => Vec::new(),
    }
}

/// Map the `Response` results of `PreparePrint` to a [`PrintChoice`].
/// `None` when the reply carries no token, which is the one thing nothing
/// downstream can work without.
pub fn choice_from_results(results: &Value) -> Option<PrintChoice> {
    let token = results.get("token")?.as_u32()?;
    let empty = Value::Dict(Vec::new());
    let settings = results.get("settings").unwrap_or(&empty);
    let setup = results.get("page-setup").unwrap_or(&empty);

    let str_of = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let bool_of = |v: &Value, key: &str| str_of(v, key).map(|s| parse_bool(&s));

    let pages = match str_of(settings, "print-pages").as_deref() {
        Some("ranges") => Pages::Ranges(str_of(settings, "page-ranges").unwrap_or_default()),
        Some("current") => Pages::Current,
        Some("selection") => Pages::Selection,
        _ => Pages::All,
    };

    // GTK's vocabulary: "horizontal" = long edge, "vertical" = short edge. KDE
    // reads them the other way round, but the host applies duplex itself; the
    // DEVMODE only mirrors that a duplex mode was chosen.
    let duplex = match str_of(settings, "duplex").as_deref() {
        Some("horizontal") => Duplex::LongEdge,
        Some("vertical") => Duplex::ShortEdge,
        _ => Duplex::Simplex,
    };

    // The page setup is authoritative for orientation; the settings key is a
    // fallback for backends that only fill one of them.
    let orientation = str_of(setup, "Orientation")
        .or_else(|| str_of(settings, "orientation"))
        .map(|s| parse_orientation(&s))
        .unwrap_or_default();

    let paper_name = str_of(setup, "PPDName")
        .or_else(|| str_of(setup, "DisplayName"))
        .or_else(|| str_of(setup, "Name"))
        .unwrap_or_default();

    let output_file = str_of(settings, "output-uri")
        .filter(|s| !s.is_empty())
        .and_then(|uri| convert::file_uri_to_path(&uri));

    Some(PrintChoice {
        token,
        copies: str_of(settings, "n-copies").and_then(|s| s.parse().ok()).unwrap_or(1).max(1),
        collate: bool_of(settings, "collate").unwrap_or(false),
        reverse: bool_of(settings, "reverse").unwrap_or(false),
        duplex,
        color: bool_of(settings, "use-color").unwrap_or(true),
        resolution: str_of(settings, "resolution").and_then(|s| s.parse().ok()).unwrap_or(0),
        pages,
        orientation,
        paper_name,
        paper_width: mm_to_tenths(setup.get("Width")),
        paper_height: mm_to_tenths(setup.get("Height")),
        output_file,
    })
}

fn mm_to_tenths(v: Option<&Value>) -> u32 {
    match v.and_then(Value::as_f64) {
        Some(mm) if mm > 0.0 => (mm * 10.0).round() as u32,
        _ => 0,
    }
}

/// GTK writes booleans as "true"/"false", KDE as "yes"/"no"; accept both.
fn parse_bool(s: &str) -> bool {
    matches!(s, "true" | "yes" | "1")
}

fn parse_orientation(s: &str) -> Orientation {
    match s {
        "portrait" | "reverse_portrait" => Orientation::Portrait,
        "landscape" | "reverse_landscape" => Orientation::Landscape,
        _ => Orientation::Unknown,
    }
}

fn orientation_str(o: Orientation) -> Option<&'static str> {
    match o {
        Orientation::Portrait => Some("portrait"),
        Orientation::Landscape => Some("landscape"),
        Orientation::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, Value)]) -> Value {
        Value::Dict(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }
    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    /// The reply the KDE backend gave in the 2026-10-11 spike, verbatim.
    fn kde_reply() -> Value {
        dict(&[
            (
                "settings",
                dict(&[
                    ("collate", s("yes")),
                    ("duplex", s("simplex")),
                    ("n-copies", s("1")),
                    ("number-up", s("1")),
                    ("number-up-layout", s("lrtb")),
                    ("print-pages", s("all")),
                    ("resolution", s("96")),
                    ("reverse", s("no")),
                    ("use-color", s("yes")),
                ]),
            ),
            (
                "page-setup",
                dict(&[
                    ("DisplayName", s("A4")),
                    ("Height", Value::Double(297.0)),
                    ("MarginBottom", Value::Double(3.53)),
                    ("Orientation", s("portrait")),
                    ("PPDName", s("A4")),
                    ("Width", Value::Double(210.0)),
                ]),
            ),
            ("token", Value::U32(1791709836)),
        ])
    }

    #[test]
    fn kde_defaults_map() {
        let c = choice_from_results(&kde_reply()).unwrap();
        assert_eq!(c.token, 1791709836);
        assert_eq!(c.copies, 1);
        assert!(c.collate);
        assert!(!c.reverse);
        assert_eq!(c.duplex, Duplex::Simplex);
        assert!(c.color);
        assert_eq!(c.resolution, 96);
        assert_eq!(c.pages, Pages::All);
        assert_eq!(c.orientation, Orientation::Portrait);
        assert_eq!(c.paper_name, "A4");
        assert_eq!((c.paper_width, c.paper_height), (2100, 2970));
        assert_eq!(c.output_file, None);
    }

    #[test]
    fn gtk_spelling_and_print_to_file() {
        let r = dict(&[
            (
                "settings",
                dict(&[
                    ("collate", s("false")),
                    ("reverse", s("true")),
                    ("use-color", s("false")),
                    ("n-copies", s("3")),
                    ("duplex", s("vertical")),
                    ("print-pages", s("ranges")),
                    ("page-ranges", s("2-3,5")),
                    ("output-uri", s("file:///home/u/out%20dir/job.ps")),
                    ("orientation", s("landscape")),
                ]),
            ),
            (
                "page-setup",
                dict(&[
                    ("Name", s("na_letter_8.5x11in")),
                    ("Width", Value::Double(215.9)),
                    ("Height", Value::Double(279.4)),
                    ("Orientation", s("reverse_landscape")),
                ]),
            ),
            ("token", Value::U32(7)),
        ]);
        let c = choice_from_results(&r).unwrap();
        assert_eq!(c.copies, 3);
        assert!(!c.collate);
        assert!(c.reverse);
        assert!(!c.color);
        assert_eq!(c.duplex, Duplex::ShortEdge);
        assert_eq!(c.pages, Pages::Ranges("2-3,5".into()));
        assert_eq!(c.orientation, Orientation::Landscape);
        assert_eq!(c.paper_name, "na_letter_8.5x11in");
        assert_eq!((c.paper_width, c.paper_height), (2159, 2794));
        assert_eq!(c.output_file.as_deref(), Some("/home/u/out dir/job.ps"));
    }

    #[test]
    fn missing_token_is_none() {
        let r = dict(&[("settings", dict(&[]))]);
        assert!(choice_from_results(&r).is_none());
    }

    #[test]
    fn sparse_reply_gets_safe_defaults() {
        let c = choice_from_results(&dict(&[("token", Value::U32(1))])).unwrap();
        assert_eq!(c.copies, 1);
        assert!(c.color);
        assert_eq!(c.pages, Pages::All);
        assert_eq!(c.orientation, Orientation::Unknown);
        assert_eq!(c.paper_width, 0);
        assert_eq!(c.paper_name, "");
    }

    #[test]
    fn zero_copies_clamped_and_long_edge() {
        let r = dict(&[
            ("settings", dict(&[("n-copies", s("0")), ("duplex", s("horizontal"))])),
            ("token", Value::U32(1)),
        ]);
        let c = choice_from_results(&r).unwrap();
        assert_eq!(c.copies, 1);
        assert_eq!(c.duplex, Duplex::LongEdge);
    }

    #[test]
    fn token_expiry_and_file_format() {
        assert!(token_is_fresh(1000, 1000));
        assert!(token_is_fresh(1000, 1000 + TOKEN_LIFETIME_SECS));
        assert!(!token_is_fresh(1000, 1001 + TOKEN_LIFETIME_SECS));
        assert!(!token_is_fresh(1000, 999)); // clock went backwards: do not trust it
        assert_eq!(parse_token_file("42 1700000000\n"), Some((42, 1700000000)));
        assert_eq!(parse_token_file("42"), None);
        assert_eq!(parse_token_file("x 5"), None);
    }

    #[test]
    fn hints_follow_request() {
        let req = PrintRequest { copies: 2, orientation: Orientation::Landscape, ..Default::default() };
        assert_eq!(
            settings_hints(&req),
            vec![("n-copies".to_string(), s("2")), ("orientation".to_string(), s("landscape"))]
        );
        assert_eq!(page_setup_hints(&req), vec![("Orientation".to_string(), s("landscape"))]);
        assert!(settings_hints(&PrintRequest::default()).is_empty());
        assert!(page_setup_hints(&PrintRequest::default()).is_empty());
    }
}
