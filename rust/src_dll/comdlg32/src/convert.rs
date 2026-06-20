//! Pure, backend-independent transforms between Wine's data and the portal's.
//! Kept separate so they're unit-testable on the host without a live D-Bus.

/// Parse a Win32-style filter blob into `(label, globs)` pairs.
///
/// The PE side re-serializes comdlg32's `lpstrFilter` as NUL-separated,
/// double-NUL-terminated UTF-8 — e.g. `b"Text\0*.txt;*.log\0All\0*.*\0"`. Each
/// pattern field is split on ';'. Empty fields / patterns are dropped.
pub fn parse_filters(blob: &[u8]) -> Vec<(String, Vec<String>)> {
    // Split into NUL-terminated fields, dropping any trailing empties.
    let mut fields: Vec<&[u8]> = blob.split(|&b| b == 0).collect();
    while matches!(fields.last(), Some(f) if f.is_empty()) {
        fields.pop();
    }

    let mut out = Vec::new();
    for pair in fields.chunks_exact(2) {
        let label = String::from_utf8_lossy(pair[0]).into_owned();
        let globs: Vec<String> = String::from_utf8_lossy(pair[1])
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if !globs.is_empty() {
            out.push((label, globs));
        }
    }
    out
}

/// Convert a `file://` URI returned by the portal into a host filesystem path.
/// Returns `None` for anything that isn't a `file:` URI.
pub fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    // An optional authority (e.g. "localhost") may precede the path; the portal
    // uses an empty authority, giving "file:///path" -> "/path".
    let path = match rest.find('/') {
        Some(0) => rest,       // "file:///path"      -> "/path"
        Some(i) => &rest[i..], // "file://host/path"  -> "/path"
        None => return None,   // "file://something"  (no path)
    };
    Some(percent_decode(path))
}

/// Percent-decode a URI path (`%20` -> ' ', etc.). Malformed escapes are kept
/// verbatim.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_basic() {
        let f = parse_filters(b"Text\0*.txt;*.log\0All Files\0*.*\0");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].0, "Text");
        assert_eq!(f[0].1, vec!["*.txt", "*.log"]);
        assert_eq!(f[1].0, "All Files");
        assert_eq!(f[1].1, vec!["*.*"]);
    }

    #[test]
    fn filters_empty_and_trailing() {
        assert!(parse_filters(b"").is_empty());
        assert!(parse_filters(b"\0\0").is_empty());
        // A label with no usable globs is dropped.
        assert!(parse_filters(b"Label\0;;\0").is_empty());
        // Trailing single field (odd count) is ignored.
        let f = parse_filters(b"Docs\0*.doc\0Orphan\0");
        assert_eq!(f, vec![("Docs".into(), vec!["*.doc".to_string()])]);
    }

    #[test]
    fn uri_plain() {
        assert_eq!(file_uri_to_path("file:///home/u/a.txt").as_deref(), Some("/home/u/a.txt"));
    }

    #[test]
    fn uri_percent_decoded() {
        assert_eq!(
            file_uri_to_path("file:///home/u/My%20Docs/a%2Bb.txt").as_deref(),
            Some("/home/u/My Docs/a+b.txt")
        );
    }

    #[test]
    fn uri_with_authority() {
        assert_eq!(file_uri_to_path("file://localhost/etc/hosts").as_deref(), Some("/etc/hosts"));
    }

    #[test]
    fn uri_rejects_non_file() {
        assert_eq!(file_uri_to_path("http://example.com/x"), None);
        assert_eq!(file_uri_to_path("/already/a/path"), None);
    }

    #[test]
    fn percent_decode_keeps_malformed() {
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
        assert_eq!(percent_decode("%2F"), "/");
    }
}
