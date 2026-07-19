//! rr-cache survey. Pure filesystem -- no git, and no guessing.
//!
//! `regenerate.sh` seeds the submodule's live cache from the superproject's
//! tracked `tools/rr-cache`, so there are two directories and they drift. The
//! live one is NOT `wine/.git/rr-cache`: `wine/.git` is a *file* holding
//! `gitdir: ../.git/modules/wine`, so a check that guesses that path finds
//! nothing, reports clean, and is a no-op forever. Resolve the gitdir first.
//!
//! What an entry looks like: a directory named by the conflict-ID hash, holding
//! `preimage` (the conflicted file body), `postimage` (the resolution) and, for
//! an in-flight merge, `thisimage`.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RrEntry {
    /// The conflict-ID hash, i.e. the directory name.
    pub id: String,
    pub dir: PathBuf,
    pub has_preimage: bool,
    pub has_postimage: bool,
    pub has_thisimage: bool,
    /// The first few non-empty lines of `preimage`, verbatim.
    ///
    /// rerere records the conflicted *content*, never the path -- so this tool
    /// genuinely cannot say which file an entry belongs to. The hint is offered
    /// as evidence for a human to recognise, not as a derived fact.
    pub preimage_head: Vec<String>,
}

impl RrEntry {
    /// Recorded a conflict but never captured a resolution. This does not
    /// merely fail to help: it re-prompts on every single replay, forever,
    /// while looking like a cache hit that did not work.
    pub fn unresolved(&self) -> bool {
        self.has_preimage && !self.has_postimage
    }
}

#[derive(Debug, Default)]
pub struct RrSurvey {
    pub root: PathBuf,
    pub entries: Vec<RrEntry>,
    /// The directory does not exist at all -- distinct from "exists and is
    /// empty", which is a seeding failure rather than a fresh checkout.
    pub missing: bool,
}

impl RrSurvey {
    pub fn read(root: impl AsRef<Path>) -> std::io::Result<RrSurvey> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() {
            return Ok(RrSurvey { root, entries: Vec::new(), missing: true });
        }
        let mut entries = Vec::new();
        for e in fs::read_dir(&root)? {
            let e = e?;
            if !e.file_type()?.is_dir() {
                continue;
            }
            let dir = e.path();
            let id = e.file_name().to_string_lossy().to_string();
            let pre = dir.join("preimage");
            let has_preimage = pre.is_file();
            entries.push(RrEntry {
                preimage_head: if has_preimage { head_lines(&pre, 3) } else { Vec::new() },
                has_postimage: dir.join("postimage").is_file(),
                has_thisimage: dir.join("thisimage").is_file(),
                has_preimage,
                id,
                dir,
            });
        }
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(RrSurvey { root, entries, missing: false })
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &RrEntry> {
        self.entries.iter().filter(|e| e.unresolved())
    }
}

/// First `n` non-empty lines, truncated. Read lossily and bounded: a preimage
/// can be a multi-megabyte generated `configure`, and this is a display hint.
fn head_lines(p: &Path, n: usize) -> Vec<String> {
    let Ok(bytes) = fs::read(p) else { return Vec::new() };
    let head = &bytes[..bytes.len().min(8192)];
    String::from_utf8_lossy(head)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(n)
        .map(|l| l.chars().take(120).collect())
        .collect()
}

/// Does this entry's recorded content belong to a path a `[[binary_resolution]]`
/// rule already decides?
///
/// If so the entry can never be consulted -- `regen` applies the rule
/// pre-emptively -- so it is dead weight that will otherwise be counted as
/// "unresolved" forever, and a human triaging it wastes time resolving a
/// conflict the tool has already decided.
///
/// This is a **heuristic over content**, deliberately narrow. rerere does not
/// store the path, so the only honest options are a content signature or
/// silence, and silence is the default: an entry we cannot attribute simply
/// stays in the unresolved bucket where it does no harm.
pub fn shadowed_by(entry: &RrEntry, resolution_paths: &[String]) -> Option<String> {
    let head = entry.preimage_head.join("\n");
    for path in resolution_paths {
        // The generated `configure` is the live instance and it never names
        // itself; its autoconf banner is the only reliable signature.
        //
        // There is deliberately NO general fallback here. A substring match of
        // the declared path against the preimage was tried and was wrong on
        // real data: the two `configure.ac` entries in this repo contain the
        // string "configure" and were attributed to the generated `configure`,
        // which has a `[[binary_resolution]]` rule -- so two conflicts that a
        // human must genuinely resolve were reported as dead weight to prune.
        // Silence for an unattributable entry costs one triage; a wrong
        // attribution costs the resolution.
        if path == "configure" && head.contains("Generated by GNU Autoconf") {
            return Some(path.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(head: &[&str]) -> RrEntry {
        RrEntry {
            id: "x".into(),
            dir: PathBuf::new(),
            has_preimage: true,
            has_postimage: false,
            has_thisimage: false,
            preimage_head: head.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn missing_dir_is_distinguished_from_empty() {
        let s = RrSurvey::read("/nonexistent/sodactl/rr-cache").unwrap();
        assert!(s.missing);
        assert!(s.entries.is_empty());
    }

    #[test]
    fn autoconf_preimage_is_attributed_to_configure() {
        let paths = vec!["configure".to_string()];
        let e = entry(&["#! /bin/sh", "# Generated by GNU Autoconf 2.72 for Wine 11.12."]);
        assert_eq!(shadowed_by(&e, &paths), Some("configure".into()));
    }

    /// The discipline this whole module is built around: an entry we cannot
    /// attribute produces NO attribution, not a plausible one.
    #[test]
    fn unattributable_preimage_stays_unattributed() {
        let paths = vec!["configure".to_string(), "dlls/user32/resources/oic_winlogo.ico".into()];
        assert_eq!(shadowed_by(&entry(&["MODULE = avrt.dll"]), &paths), None);
    }

    /// Regression, from real data. `configure.ac` conflicts mention the word
    /// "configure"; attributing them to the generated `configure` would tell a
    /// human to prune two conflicts they actually have to resolve.
    #[test]
    fn configure_ac_is_not_the_generated_configure() {
        let paths = vec!["configure".to_string()];
        let e = entry(&["AC_INIT([Wine],[11.12])", "dnl configure.ac for Wine"]);
        assert_eq!(shadowed_by(&e, &paths), None);
    }
}
