//! Presentation cells: the strings, not the printing.
//!
//! Every fn here maps one derived fact to the short string a table cell shows.
//! They return `String`/`&str` and never print -- `sodactl-cli` owns every
//! `println!` in the workspace (and `no_stdout_writes` in sodactl-mcp greps this
//! crate to keep it that way). They live in core rather than the CLI because the
//! TUI shows the same facts, and two hand-maintained renderings of a tri-state
//! WILL diverge on exactly the value nobody re-tests -- the `None` arm, which is
//! the "could not determine" case this project refuses to render as anything
//! quiet.

use sodactl_git::Pin;

/// Abbreviated oid: 11 hex chars, matching what `git describe` shows on this
/// repo's history. Slices bytes, which is safe here because an oid is ASCII.
pub fn shorten(s: &str) -> String {
    s[..s.len().min(11)].to_string()
}

/// `shorten` over a fact that may be absent; `-` keeps the table column
/// visibly empty rather than blank.
pub fn short_opt(o: &Option<String>) -> String {
    o.as_deref().map(shorten).unwrap_or_else(|| "-".into())
}

/// The `vs remote` cell.
///
/// Ahead AND behind is a restack, and its remedy is a FORCE push -- a
/// column showing only `ahead` renders a diverged tier as merely
/// unpushed and sends the reader into a rejected `git push`.
pub fn remote_cell(ahead: Option<u32>, behind: Option<u32>) -> String {
    match (ahead, behind) {
        (Some(0), Some(0)) => "=".to_string(),
        (Some(a), Some(b)) => format!("+{a}/-{b}"),
        (Some(a), None) => format!("+{a}/?"),
        _ => "-".to_string(),
    }
}

/// The `on parent tip` cell. `None` renders as `?`, not as either verdict:
/// "could not determine" said out loud is the standing rule.
pub fn parent_tip_cell(contains_parent_tip: Option<bool>) -> &'static str {
    match contains_parent_tip {
        Some(true) => "yes",
        Some(false) => "NO -- stale",
        None => "?",
    }
}

/// The consumer `current` cell.
///
/// `current` is deliberately not a boolean with a default. A consumer is
/// a CROSS-BASE replay, so patch-id equivalence against the soda tip is
/// not evidence either way -- conflict resolution and binary_resolution
/// legitimately change the diff. Without a generation ledger the honest
/// answer is "unknown", and printing "no" would be a fabrication.
pub fn current_cell(current: Option<bool>) -> &'static str {
    match current {
        Some(true) => "yes",
        Some(false) => "STALE",
        None => "unknown",
    }
}

/// The gitlink, shown next to the branch tip because they are different commits
/// BY DESIGN -- consumers are force-pushed and a local tip routinely runs ahead
/// of its tag. Any reader eyeballing a pin finding needs to see both or they
/// cannot tell a true positive from the tool reading the wrong subject.
pub fn pin_cell(pin: &Option<Pin>) -> String {
    match pin {
        None => "-".into(),
        Some(Pin::Unknown { reason }) => format!("UNKNOWN ({reason})"),
        Some(Pin::Known { committed, staged }) => {
            let head = committed.as_deref().map(shorten).unwrap_or_else(|| "none".into());
            match staged {
                Some(s) if Some(s) != committed.as_ref() => {
                    format!("{head} (STAGED {})", shorten(s))
                }
                _ => head,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `None` arms are the ones worth pinning: they are the "could not
    // determine" renderings, and a regression there is exactly the silent
    // breakdown the project rule forbids.
    #[test]
    fn tri_states_render_unknown_loudly() {
        assert_eq!(parent_tip_cell(None), "?");
        assert_eq!(current_cell(None), "unknown");
        assert_eq!(remote_cell(None, None), "-");
        assert_eq!(remote_cell(Some(3), None), "+3/?");
        assert_eq!(short_opt(&None), "-");
    }

    #[test]
    fn remote_cell_shows_divergence_not_just_ahead() {
        assert_eq!(remote_cell(Some(2), Some(1)), "+2/-1");
        assert_eq!(remote_cell(Some(0), Some(0)), "=");
    }

    // The STAGED divergence rendering is Blocker-adjacent (an uncommitted pin
    // move) and must exist exactly once -- this is that once.
    #[test]
    fn pin_cell_staged_divergence() {
        let oid = "0123456789abcdef0123456789abcdef01234567".to_string();
        let other = "fedcba9876543210fedcba9876543210fedcba98".to_string();
        let same = Pin::Known { committed: Some(oid.clone()), staged: Some(oid.clone()) };
        assert_eq!(pin_cell(&Some(same)), "0123456789a");
        let moved = Pin::Known { committed: Some(oid), staged: Some(other) };
        assert_eq!(pin_cell(&Some(moved)), "0123456789a (STAGED fedcba98765)");
        let unborn = Pin::Known { committed: None, staged: None };
        assert_eq!(pin_cell(&Some(unborn)), "none");
        assert_eq!(pin_cell(&Some(Pin::Unknown { reason: "gitlink unreadable".into() })), "UNKNOWN (gitlink unreadable)");
        assert_eq!(pin_cell(&None), "-");
    }
}
