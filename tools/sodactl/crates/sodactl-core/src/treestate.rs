//! Shared working-tree observations, factored so two consumers cannot drift.
//!
//! `checks/tree.rs` RENDERS these observations as findings (`REBASE_IN_FLIGHT`,
//! `SUBMODULE_DIRTY`, `SUBMODULE_UNTRACKED`) and `plan::preflight` REFUSES on
//! them before any mutating verb runs. The two used to be one code path away
//! from divergence: a probe fixed in the check but not the verb lets the verb
//! rebase over exactly the half-done state the check warns about. So the
//! observation lives here once, and both callers consume the same structs.
//!
//! Nothing in this module decides severity or wording of findings -- that stays
//! with the check, whose output is pinned byte-for-byte by the golden master.

use std::path::Path;

use crate::checks::glob_match;
use sodactl_git::StatusEntry;

/// A replay half-done in a repository, observed structurally.
#[derive(Debug)]
pub struct InFlight {
    /// Which gitdir entries exist, in probe order.
    pub markers: Vec<&'static str>,
    /// A leading-space narrative fragment (" replaying X onto Y, at step
    /// n/e"), or empty when the rebase metadata is not readable. Shaped so the
    /// check's message stays byte-identical to what it printed before the
    /// factor-out.
    pub narrative: String,
}

/// Probe a RESOLVED gitdir for a half-done replay.
///
/// The gitdir must be resolved, not guessed: `wine/.git` is a file holding
/// `gitdir: ../.git/modules/wine`, so a probe that stats
/// `wine/.git/rebase-merge` finds nothing and passes forever. Callers get the
/// path from `GitRead::git_dir()`.
pub fn in_flight(gitdir: &Path) -> Option<InFlight> {
    let markers: Vec<&'static str> =
        ["rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD", "MERGE_RR", "sequencer"]
            .into_iter()
            .filter(|n| {
                let p = gitdir.join(n);
                // MERGE_RR is rerere's list of paths with recorded-but-
                // unresolved conflicts, and git leaves it behind EMPTY after a
                // rerere-enabled rebase completes (measured; `git status` and
                // even `git rerere` do not remove it). An empty file records
                // zero such paths -- counting it made every rebase that ever
                // touched rerere read as permanently in flight, wedging both
                // the preflight gate and regen's conflict loop.
                if *n == "MERGE_RR" {
                    return std::fs::metadata(&p).is_ok_and(|md| md.len() > 0);
                }
                p.exists()
            })
            .collect();
    if markers.is_empty() {
        return None;
    }
    // Observe richly: which rebase, how far in. A bare boolean tells the
    // caller to stop without telling the human what they are in the middle of.
    let onto = read_trimmed(&gitdir.join("rebase-merge/onto"));
    let head_name = read_trimmed(&gitdir.join("rebase-merge/head-name"));
    let progress = match (
        read_trimmed(&gitdir.join("rebase-merge/msgnum")),
        read_trimmed(&gitdir.join("rebase-merge/end")),
    ) {
        (Some(n), Some(e)) => format!(", at step {n}/{e}"),
        _ => String::new(),
    };
    let narrative = match (head_name, onto) {
        (Some(h), Some(o)) => format!(" replaying {h} onto {o}{progress}"),
        _ => progress,
    };
    Some(InFlight { markers, narrative })
}

/// Is a REBASE specifically in flight? -- the sequencer's own two state dirs,
/// nothing else.
///
/// Narrower than `in_flight` on purpose: regen's conflict loop must terminate
/// exactly when `git rebase --continue` would say "no rebase in progress",
/// and the broad probe's extra markers outlive a completed rebase (a
/// non-empty `MERGE_RR` survives whenever a recorded conflict was resolved by
/// rule -- deletion, binary take -- because rerere never captured a
/// postimage for it). Driving the loop off the broad probe made every such
/// pour end in a false NoProgress.
pub fn rebase_in_flight(gitdir: &Path) -> bool {
    gitdir.join("rebase-merge").exists() || gitdir.join("rebase-apply").exists()
}

/// A status listing split by what it means for a replay: tracked
/// modifications a rebase would destroy, and untracked paths nobody declared.
/// Allowlisted build products appear in neither -- silent, not even counted.
#[derive(Debug)]
pub struct DirtySplit {
    pub tracked: Vec<String>,
    pub unknown_untracked: Vec<String>,
}

/// Split a status listing against the manifest's `build_artifacts` allowlist.
///
/// The allowlist applies to BOTH kinds, and restricting it to untracked paths
/// made it inert exactly where the manifest says it is needed: `configure` is
/// the allowlist's first non-stamp entry AND is tracked in upstream wine (the
/// manifest's own `binary_resolution` note says so), so the moment the build
/// runs autoreconf it becomes ` M configure`, takes the tracked branch, and
/// raises the SUBMODULE_DIRTY Blocker the allowlist exists to prevent.
///
/// This does not weaken the guard: the whole design here is that a DECLARED
/// build product is silent whatever its status, and an undeclared tracked
/// modification is still an unconditional Blocker (for the check) and a
/// refusal (for the verbs).
pub fn split_dirty(entries: Vec<StatusEntry>, build_artifacts: &[String]) -> DirtySplit {
    let mut tracked = Vec::new();
    let mut unknown_untracked = Vec::new();
    for e in entries {
        if build_artifacts.iter().any(|g| glob_match(g, &e.path)) {
            continue;
        }
        if e.is_untracked() {
            unknown_untracked.push(e.path);
        } else {
            tracked.push(e.path);
        }
    }
    DirtySplit { tracked, unknown_untracked }
}

fn read_trimmed(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sodactl_git::{StatusEntry, StatusKind};

    /// The probe reads plain files, so a plain directory is a faithful
    /// fixture -- no git needed to model a gitdir mid-rebase.
    #[test]
    fn in_flight_reads_the_rebase_metadata_it_can_and_shrugs_at_the_rest() {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-treestate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("rebase-merge")).unwrap();

        // Markers only, no metadata: present, but with nothing to narrate.
        let fl = in_flight(&dir).expect("rebase-merge exists");
        assert_eq!(fl.markers, vec!["rebase-merge"]);
        assert_eq!(fl.narrative, "");

        std::fs::write(dir.join("rebase-merge/onto"), "abc123\n").unwrap();
        std::fs::write(dir.join("rebase-merge/head-name"), "refs/heads/topic\n").unwrap();
        std::fs::write(dir.join("rebase-merge/msgnum"), "2\n").unwrap();
        std::fs::write(dir.join("rebase-merge/end"), "5\n").unwrap();
        let fl = in_flight(&dir).unwrap();
        assert_eq!(fl.narrative, " replaying refs/heads/topic onto abc123, at step 2/5");

        std::fs::remove_dir_all(dir.join("rebase-merge")).unwrap();
        assert!(in_flight(&dir).is_none(), "a clean gitdir must probe as not in flight");

        // MERGE_RR: git's leftover empty file after a completed rerere rebase
        // must not read as in flight -- only a NON-empty one (paths still
        // recorded unresolved) does.
        std::fs::write(dir.join("MERGE_RR"), b"").unwrap();
        assert!(in_flight(&dir).is_none(), "an empty MERGE_RR is a leftover, not a replay");
        std::fs::write(dir.join("MERGE_RR"), b"deadbeef\trr-id\n").unwrap();
        assert_eq!(in_flight(&dir).unwrap().markers, vec!["MERGE_RR"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_dirty_applies_the_allowlist_to_both_kinds() {
        let e = |kind, path: &str| StatusEntry { kind, xy: String::new(), path: path.into() };
        let entries = vec![
            e(StatusKind::Tracked, "configure"),          // allowlisted AND tracked
            e(StatusKind::Tracked, "dlls/ntdll/loader.c"), // real uncommitted work
            e(StatusKind::Untracked, ".build-env-stamp"), // allowlisted untracked
            e(StatusKind::Untracked, "notes.txt"),        // undeclared untracked
        ];
        let allow = vec!["configure".to_string(), ".build-env-stamp".to_string()];
        let s = split_dirty(entries, &allow);
        assert_eq!(s.tracked, vec!["dlls/ntdll/loader.c"]);
        assert_eq!(s.unknown_untracked, vec!["notes.txt"]);
    }
}
