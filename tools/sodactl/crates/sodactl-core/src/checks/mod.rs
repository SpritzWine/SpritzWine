//! The v0 check set beyond `reconcile()`.
//!
//! `reconcile()` answers "is the tier DAG self-consistent?". These answer the
//! questions that break a build rather than a graph: is the superproject's pin
//! fetchable, is the rerere cache actually able to help, do the tags mean what
//! the release ritual assumes, is the tree safe to replay onto.
//!
//! Two rules govern everything in this directory.
//!
//! **One: a check that cannot determine its answer emits nothing.** Not a
//! benign default, not "probably fine". Every collapse of "unknown" into a
//! definite value in this tool has produced a confident wrong answer -- a
//! `git cherry` that exits 128 on a typo and reads as "stitch is clean", an
//! `is_ancestor` that reads a broken ref as "not an ancestor" and prints a
//! rebase instruction. So `sodactl_git::GitRead` distinguishes *exit code 1*
//! (the answer is no) from *any other failure* (there is no answer), and the
//! latter is an error that reaches the caller. Its shape is the part that
//! matters: every fallible query returns either a definite answer or an error,
//! and `Option` means *git said no such thing*, never *the query failed*.
//!
//! **Two: never reason about a replayed artifact with ancestry, SHA equality or
//! commit counts.** A stitch is cherry-picked and a consumer is rebased; the
//! original commits are not ancestors of either and never share a SHA. Patch-id
//! is the right primitive for a *same-base* cherry-pick (a stitch) and is
//! *insufficient* for a *cross-base* rebase (a consumer), because conflict
//! resolution and `binary_resolution` legitimately change the diff. See
//! `consumers.rs`, which is where that distinction is load-bearing.

use crate::finding::Finding;
use crate::manifest::Manifest;
use std::path::PathBuf;

pub mod checkout;
pub mod consumers;
pub mod pin;
pub mod rr;
pub mod tagcheck;
pub mod tiers;
pub mod tree;

pub use sodactl_git::{AheadBehind, GitRead, Oid, StatusEntry};
#[cfg(test)]
use sodactl_git::{GitError, Reapply, Result};

/// The two repositories every pin check needs at once.
///
/// `reconcile()` takes one `GitRead` and therefore can only ask questions about
/// the wine repo -- which is why its `PINNED_CONSUMER_UNTAGGED` reads the
/// consumer's *branch tip*. The branch tip is not the pin. The pin is the
/// superproject's gitlink, and the model's own operational fact (consumers are
/// force-pushed, so tips routinely run ahead of tags) guarantees the two differ
/// in normal operation.
pub struct Workspace<'a> {
    pub superproject: &'a dyn GitRead,
    pub wine: &'a dyn GitRead,
    /// Path of the superproject root, for filesystem work (rr-cache).
    pub superproject_path: PathBuf,
    /// The wine submodule path as the superproject records it, e.g. `wine`.
    pub wine_path: String,
}

/// The resolved policy the checks consume.
///
/// Every field here is a decision, not an observation, and every one of them is
/// now DECLARED -- `Policy::from_manifest` builds this from `[policy]` and the
/// per-consumer `exclude` keys. It stays a separate type from the manifest's
/// `PolicyDecl` on purpose: the checks want a flat, already-resolved shape
/// (equivalences without their prose, excludes already keyed by consumer), and
/// coupling them to the on-disk schema would make every future manifest key an
/// edit to six check modules.
pub struct Policy {
    /// Tag pairs that are *intended* to name the same commit. BRANCHING.md
    /// documents `spritzvalve-11.0-1` == `-11.0-3` ("the family rolled, this
    /// glass did not need re-pouring") and `soda-all` == `soda-11.12-1`.
    pub tag_equivalences: Vec<Vec<String>>,
    /// Base-free tag names that are ours. Must be declared, never pattern-
    /// guessed, or `soda-all` reads as Foreign and every alias is invisible.
    pub tag_aliases: Vec<String>,
    /// Local branches that are documented artifacts, not debris.
    /// `spritzvalve-full` is BRANCHING.md's standing example.
    pub ignore_branches: Vec<String>,
    /// Severity for `TAG_FAMILY_SKEW`. See `tagcheck.rs`.
    pub family_skew: SkewPolicy,
    /// Tiers deliberately excluded from a consumer's replay, by consumer name.
    /// Sourced from `[[consumer]] exclude`, not from a global keyed list -- see
    /// `manifest::Consumer::exclude` for why the key moved.
    pub consumer_excludes: Vec<(String, Vec<String>)>,
    /// Where the tracked rr-cache lives, relative to the superproject.
    pub rr_cache: String,
}

/// Serde-parsed so an unrecognised `family_skew` is a parse error naming the
/// key, rather than a quiet fallback to whichever variant the binary prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkewPolicy {
    Blocker,
    Drift,
    Off,
}

impl Policy {
    /// Resolve the declared policy, falling back to `Default` when the manifest
    /// carries no `[policy]` section at all.
    ///
    /// That fallback is deliberate and load-bearing, not leftover scaffolding:
    /// it is what makes moving these values into the manifest a pure refactor.
    /// Every manifest written before `[policy]` existed -- including any a user
    /// still has on disk -- keeps producing byte-identical findings.
    ///
    /// `consumer_excludes` is the ONE field the fallback cannot supply, because
    /// it no longer has a global home to be defaulted from: it is now a
    /// property of each `[[consumer]]`, so an undeclared consumer excludes
    /// nothing. That is the point of the move -- an exclusion that applies to a
    /// consumer nobody wrote it for is precisely the failure the old keyed list
    /// enabled -- and it is why both `tools/sodactl.toml` and the test fixture
    /// declare their excludes explicitly.
    pub fn from_manifest(m: &Manifest) -> Self {
        let consumer_excludes = m
            .consumer
            .iter()
            .filter(|c| !c.exclude.is_empty())
            .map(|c| (c.name.clone(), c.exclude.clone()))
            .collect();

        let Some(d) = &m.policy else {
            return Self { consumer_excludes, ..Self::default() };
        };

        Self {
            tag_equivalences: d.tag_equivalence.iter().map(|e| e.tags.clone()).collect(),
            tag_aliases: d.tag_aliases.clone(),
            ignore_branches: d.ignore_branches.clone(),
            family_skew: d.family_skew,
            consumer_excludes,
            rr_cache: d.rr_cache.clone(),
        }
    }
}

/// The pre-`[policy]` behaviour, frozen.
///
/// These values are no longer where the decisions LIVE -- `tools/sodactl.toml`
/// is, with a reason written beside each one. This impl exists so a manifest
/// without the section behaves exactly as it did before the section existed,
/// and it should be read as a compatibility record rather than as the current
/// policy. Changing a value here changes what an undeclared manifest does while
/// leaving every declared one untouched, which is almost certainly not what the
/// editor wants.
impl Default for Policy {
    fn default() -> Self {
        Self {
            tag_equivalences: vec![
                vec!["spritzvalve-11.0-1".into(), "spritzvalve-11.0-3".into()],
                vec!["soda-all".into(), "soda-11.12-1".into()],
            ],
            tag_aliases: vec!["soda-all".into(), "spritzwine-archive".into()],
            ignore_branches: vec!["spritzvalve-full".into()],
            // Drift, with the reasoning printed, until the open question is
            // answered: if the trailing integer means "cut together, use
            // together" this is a Blocker; if a component may lag between cuts
            // it is noise. Guessing either way is worse than saying so.
            family_skew: SkewPolicy::Drift,
            // Empty, and not by omission. `from_manifest` derives this from
            // `[[consumer]] exclude` and therefore always overwrites whatever
            // stands here, so a value would be unreachable code that reads like
            // live policy. An exclusion now requires a consumer that declares
            // it; there is no consumer-independent default to state.
            consumer_excludes: Vec::new(),
            rr_cache: "tools/rr-cache".into(),
        }
    }
}

/// Run every check in this module. Order is irrelevant -- the report is sorted
/// by severity -- but grouping is not: a caller that wants only the pin cluster
/// can call `pin::check` alone.
pub fn run_all(m: &Manifest, ws: &Workspace, p: &Policy) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();
    f.extend(pin::check(m, ws)?);
    f.extend(tiers::check(m, ws.wine)?);
    f.extend(consumers::check(m, ws.wine, p)?);
    f.extend(tree::check(m, ws)?);
    f.extend(checkout::check(m, ws)?);
    f.extend(tagcheck::check(m, ws.wine, p)?);
    f.extend(rr::check(m, ws, p)?);
    Ok(f)
}

/// Glob match over a slash-separated path. Supports `*` (no `/`), `**` (any),
/// `?`. Done in-process rather than via `git check-ignore`, which answers a
/// different question (`.gitignore`, not our allowlist).
/// The exclusive start of a tier's OWN commits: its declared parent, with
/// `$home_base` resolved. `None` for a tier the manifest does not declare --
/// silence rather than a guessed range.
///
/// Shared, because using the home base instead is the single most repeated
/// mistake in this codebase and it has now been made twice. Limiting at the home
/// base makes the range for EVERY child tier include soda-build's and
/// soda-core's commits, so a question asked about one tier is silently answered
/// about its whole ancestry. In `consumers.rs` that fired an exclusion Blocker
/// on a correctly poured spritzvalve; in `tagcheck.rs` it rendered one tier's
/// drift as five tiers' (see `TAG_ON_STALE_TARGET`). The question is only ever
/// about the commits the tier itself introduces.
pub fn own_limit(m: &Manifest, tier: &str, home: &str) -> Option<String> {
    let t = m.tier.iter().find(|t| t.name == tier)?;
    Some(if t.parent == "$home_base" { home.to_string() } else { t.parent.clone() })
}

pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn seg(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => {
                // `**` crosses separators; a single `*` does not.
                let double = p.get(1) == Some(&'*');
                let rest = if double { &p[2..] } else { &p[1..] };
                for i in 0..=s.len() {
                    if !double && s[..i].contains(&'/') {
                        break;
                    }
                    if seg(rest, &s[i..]) {
                        return true;
                    }
                }
                false
            }
            Some('?') if !s.is_empty() && s[0] != '/' => seg(&p[1..], &s[1..]),
            Some(c) if s.first() == Some(c) => seg(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    seg(&p, &s)
}

/// A `GitRead` that answers from tables, for tests.
///
/// Deliberately NOT a canned map of "method -> answer": a canned table encodes
/// whatever the test author already believed, which is exactly how the original
/// four `STITCH_STALE` false positives passed their own tests. What is modelled
/// here is the ref namespace and the tag namespace; anything a check asks that
/// the fixture does not model comes back as an ERROR, so a test cannot silently
/// pass on a question the fake was never taught to answer.
#[cfg(test)]
#[derive(Default)]
pub struct FakeGit {
    /// The branch HEAD is on; `None` models a detached checkout, which is the
    /// state `checkout.rs` treats as correct.
    pub head: Option<String>,
    pub refs: Vec<(String, String)>,
    pub tags: Vec<(String, String)>,
    pub index: Vec<(String, String)>,
    pub config: Vec<(String, String)>,
    pub branches: Vec<(String, String)>,
}

#[cfg(test)]
impl GitRead for FakeGit {
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
        Ok(self.refs.iter().find(|(n, _)| n == rev).map(|(_, o)| o.clone()))
    }
    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>> {
        self.rev_parse(rev)
    }
    /// Kept as an explicit override even though `GitRead` defaults it: the
    /// default routes through `tags()` + `rev_parse()`, and this fixture's
    /// `tags` table is `(name, oid)` pairs rather than the bare names `tags()`
    /// returns.
    fn tags_pointing_at(&self, oid: &str) -> Result<Vec<String>> {
        Ok(self.tags.iter().filter(|(_, o)| o == oid).map(|(n, _)| n.clone()).collect())
    }
    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
        Ok(self.tags.clone())
    }
    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>> {
        Ok(self
            .branches
            .iter()
            .filter(|(n, o)| o == oid && n.contains('/') == remote)
            .map(|(n, _)| n.clone())
            .collect())
    }
    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>> {
        Ok(self.index.iter().find(|(p, _)| p == path).map(|(_, o)| o.clone()))
    }
    fn config_file_get(&self, _f: &str, key: &str) -> Result<Option<String>> {
        Ok(self.config.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    }
    fn head_branch(&self) -> Result<Option<String>> {
        Ok(self.head.clone())
    }
    // Everything this fixture does not model is an ERROR, never a benign
    // default. A stub returning `Ok(vec![])` / `Ok(None)` / `Ok(false)` here
    // would let the nine-defects bug class back in through the test suite,
    // where it is least visible.
    fn git_dir(&self) -> Result<PathBuf> {
        Err(GitError::Unsupported { method: "git_dir" })
    }
    fn is_ancestor(&self, _: &str, _: &str) -> Result<bool> {
        Err(GitError::Unsupported { method: "is_ancestor" })
    }
    fn missing_by_patch_id(&self, _: &str, _: &str, _: &str) -> Result<Vec<Oid>> {
        Err(GitError::Unsupported { method: "missing_by_patch_id" })
    }
    fn reapply_verdict(&self, _: &str, _: &str) -> Result<Reapply> {
        Err(GitError::Unsupported { method: "reapply_verdict" })
    }
    fn ahead_behind(&self, _: &str, _: &str) -> Result<Option<AheadBehind>> {
        Err(GitError::Unsupported { method: "ahead_behind" })
    }
    fn count_commits(&self, _: &str) -> Result<u32> {
        Err(GitError::Unsupported { method: "count_commits" })
    }
    fn local_branches(&self) -> Result<Vec<String>> {
        Err(GitError::Unsupported { method: "local_branches" })
    }
    fn tags(&self) -> Result<Vec<String>> {
        Err(GitError::Unsupported { method: "tags" })
    }
    fn log_subjects(&self, _: &str) -> Result<Vec<(Oid, String)>> {
        Err(GitError::Unsupported { method: "log_subjects" })
    }
    fn merges_in(&self, _: &str) -> Result<Vec<Oid>> {
        Err(GitError::Unsupported { method: "merges_in" })
    }
    fn status(&self, _: bool) -> Result<Vec<StatusEntry>> {
        Err(GitError::Unsupported { method: "status" })
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn globs() {
        assert!(glob_match("configure", "configure"));
        assert!(!glob_match("configure", "configure.ac"));
        assert!(glob_match("**/*.o", "dlls/ntdll/x.o"));
        assert!(glob_match("dlls/*/x.o", "dlls/ntdll/x.o"));
        assert!(!glob_match("dlls/*/x.o", "dlls/a/b/x.o"));
        assert!(glob_match("dlls/msxml3/xslpattern.tab.?", "dlls/msxml3/xslpattern.tab.c"));
    }
}
