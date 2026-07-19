//! A `GitRead` fake backed by a real (tiny) commit DAG.
//!
//! The obvious way to fake `GitRead` is a table of canned answers: one map per
//! method, keyed by argument. Do not do that. This domain's signature failure is
//! reasoning about a *replayed* artifact with ancestry, SHA equality or commit
//! counts -- and a canned-answer table encodes whatever mental model the test
//! author already had. If the author believes `is_ancestor(tier, stitch)` is
//! true, the table says true, the reconciler agrees, and the test passes green
//! on a bug. That is exactly how the four STITCH_STALE false positives got
//! confidently shipped.
//!
//! So this fake models the actual object graph -- commits with a parent and a
//! *patch id* -- and DERIVES every method from it, the same way git does.
//! Ancestry is a parent walk. Patch-id equivalence is a set comparison over a
//! range. The two then disagree for a cherry-pick because they genuinely
//! disagree, not because someone remembered to encode the disagreement.
//!
//! Oids are handed out as `c01`, `c02`, ... in creation order, so a test can
//! name the fork base in its manifest (`home_base = "c01"`) without plumbing a
//! value through. Patch ids are author-chosen strings: reusing one is what makes
//! a commit a replay of another.

#![allow(dead_code)] // each test binary uses a different slice of this

use sodactl_git::{AheadBehind, GitError, GitRead, Oid, Reapply, Result, StatusEntry};
use std::collections::{BTreeMap, BTreeSet};

struct Commit {
    parent: Option<Oid>,
    /// Stands in for `git patch-id`: equal patch id == equivalent change.
    patch: String,
}

#[derive(Default)]
pub struct FakeRepo {
    commits: BTreeMap<Oid, Commit>,
    /// Every ref by the exact string a caller would pass: `soda-core`,
    /// `codeberg/soda-core`, `spritzwine-11.13-3`.
    refs: BTreeMap<String, Oid>,
    branches: Vec<String>,
    tags: Vec<String>,
    gitlinks: BTreeMap<String, Oid>,
    /// Overrides for the reapply slow path, keyed `(commit, onto)`.
    ///
    /// Absent means "agree with patch-id" (`Missing`), so every pre-existing
    /// test keeps its behaviour. A test that wants to model the bug this path
    /// exists for -- patch-id crying missing on a change that IS present --
    /// registers `Reapply::Present` here.
    reapply: BTreeMap<(String, String), Reapply>,
    /// What the superproject pins this repo at -- `HEAD:<path>` and the index
    /// entry, kept apart because a staged-but-uncommitted bump is its own fact.
    ///
    /// Modelled explicitly, and absent by default, so a test that says nothing
    /// about the pin gets `Pin::Unknown` and every pin check reports "I could
    /// not determine this" rather than passing on a fabricated clean pin. The
    /// pin is NOT the branch tip -- consumers are force-pushed and tips run
    /// ahead of tags by design -- so a fake that silently answered with the tip
    /// would re-encode the exact wrong-subject bug these tests exist to catch.
    pin_committed: Option<Oid>,
    pin_staged: Option<Oid>,
    seq: u32,
}

impl FakeRepo {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- construction -----------------------------------------------------

    /// A parentless commit. Conventionally the fork base, and conventionally
    /// the first thing a test creates, so it lands on `c01`.
    pub fn root(&mut self, patch: &str) -> Oid {
        self.mk(None, patch)
    }

    /// Append commits to `start` (a ref name or an oid) without moving any ref.
    /// Returns the new tip.
    pub fn commits_on(&mut self, start: &str, patches: &[&str]) -> Oid {
        let mut tip = self.resolve(start).unwrap_or_else(|| panic!("no such rev `{start}`"));
        for p in patches {
            tip = self.mk(Some(tip), p);
        }
        tip
    }

    /// The oid a ref points at right now, for tests that need to capture a tip
    /// *before* moving it. Panics rather than returning `Option`: in a fixture
    /// an unresolvable rev is a typo in the test, not a state to handle.
    pub fn at(&self, rev: &str) -> Oid {
        self.resolve(rev).unwrap_or_else(|| panic!("no such rev `{rev}`"))
    }

    pub fn branch(&mut self, name: &str, at: &str) -> Oid {
        let oid = self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`"));
        self.refs.insert(name.to_string(), oid.clone());
        if !self.branches.contains(&name.to_string()) {
            self.branches.push(name.to_string());
        }
        oid
    }

    /// A remote-tracking ref. Name it the way the reconciler asks for it:
    /// `codeberg/soda-core`.
    pub fn remote_branch(&mut self, name: &str, at: &str) -> Oid {
        let oid = self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`"));
        self.refs.insert(name.to_string(), oid.clone());
        oid
    }

    pub fn tag(&mut self, name: &str, at: &str) -> Oid {
        let oid = self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`"));
        self.refs.insert(name.to_string(), oid.clone());
        self.tags.push(name.to_string());
        oid
    }

    /// Authored history: new commits carrying NEW patch ids, with a branch on
    /// top. The start ref is explicit because "which parent tip was this
    /// authored on" is the whole staleness question -- pass an old tip and you
    /// have modelled a stale child.
    pub fn tier(&mut self, name: &str, start: &str, patches: &[&str]) -> Oid {
        let tip = self.commits_on(start, patches);
        self.branch(name, &tip)
    }

    /// Replayed history: new commits carrying EXISTING patch ids. Mechanically
    /// identical to `tier` -- the difference is that the patch ids are reused,
    /// which is precisely what a cherry-pick or a `rebase --onto` produces.
    /// Named separately so a test states which one it means, and so the
    /// resulting oids are visibly *not* the originals.
    pub fn replay(&mut self, name: &str, onto: &str, patches: &[&str]) -> Oid {
        let tip = self.commits_on(onto, patches);
        self.branch(name, &tip)
    }

    /// Move an existing branch forward. The point of having this as its own
    /// verb: a test that captures a tip, calls `extend`, then authors a child
    /// on the captured tip is telling the exact story that produces a stale
    /// child in the real repo.
    pub fn extend(&mut self, name: &str, patches: &[&str]) -> Oid {
        let tip = self.commits_on(name, patches);
        self.branch(name, &tip)
    }

    /// The superproject's committed gitlink. A test that cares about a pin must
    /// state which commit is pinned; it is deliberately not inferred.
    pub fn pin(&mut self, at: &str) {
        self.pin_committed = Some(self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`")));
    }

    /// A staged gitlink bump -- `git add wine` caught mid-flight.
    pub fn pin_staged(&mut self, at: &str) {
        self.pin_staged = Some(self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`")));
    }

    /// Model the reapply slow path disagreeing with patch-id for one commit.
    ///
    /// `Present` is the case that matters: patch-id says the change is absent
    /// (because rerere or drift changed its hash) while reapplying it is a
    /// no-op, proving it IS there.
    pub fn reapply(&mut self, commit: &str, onto: &str, verdict: Reapply) {
        let c = self.at(commit);
        self.reapply.insert((c, onto.to_string()), verdict);
    }

    pub fn gitlink(&mut self, path: &str, at: &str) {
        let oid = self.resolve(at).unwrap_or_else(|| panic!("no such rev `{at}`"));
        self.gitlinks.insert(path.to_string(), oid);
    }

    // ---- derivation -------------------------------------------------------

    fn mk(&mut self, parent: Option<Oid>, patch: &str) -> Oid {
        self.seq += 1;
        let oid = format!("c{:02}", self.seq);
        self.commits.insert(oid.clone(), Commit { parent, patch: patch.to_string() });
        oid
    }

    fn resolve(&self, rev: &str) -> Option<Oid> {
        self.refs.get(rev).cloned().or_else(|| self.commits.contains_key(rev).then(|| rev.to_string()))
    }

    /// Reachability set, inclusive of `oid` itself.
    fn ancestors(&self, oid: &str) -> BTreeSet<Oid> {
        let mut out = BTreeSet::new();
        let mut cur = Some(oid.to_string());
        while let Some(c) = cur {
            if !out.insert(c.clone()) {
                break;
            }
            cur = self.commits.get(&c).and_then(|k| k.parent.clone());
        }
        out
    }

    /// `<exclude>..<include>` as git means it: reachable from one, not the
    /// other. A missing endpoint yields an empty range rather than an error,
    /// mirroring `GitCli`'s `run_opt` swallow.
    fn range(&self, exclude: &str, include: &str) -> BTreeSet<Oid> {
        let (Some(e), Some(i)) = (self.resolve(exclude), self.resolve(include)) else {
            return BTreeSet::new();
        };
        let ex = self.ancestors(&e);
        self.ancestors(&i).into_iter().filter(|c| !ex.contains(c)).collect()
    }

    fn patches_of(&self, set: &BTreeSet<Oid>) -> BTreeSet<String> {
        set.iter().filter_map(|c| self.commits.get(c)).map(|k| k.patch.clone()).collect()
    }
}

impl GitRead for FakeRepo {
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
        Ok(self.resolve(rev))
    }

    /// The fake has no peeling -- an oid is an oid -- so raw and peeled
    /// resolution are genuinely the same operation here, not a shortcut.
    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>> {
        self.rev_parse(rev)
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let (Some(a), Some(d)) = (self.resolve(ancestor), self.resolve(descendant)) else {
            return Ok(false);
        };
        Ok(self.ancestors(&d).contains(&a))
    }

    /// `git cherry <upstream> <head> <limit>`: list `limit..head`, and report
    /// each commit whose patch id is absent from `limit..upstream`.
    ///
    /// Note what is deliberately NOT here: no ancestry test, no oid comparison.
    /// A replayed commit has a fresh oid and is nobody's ancestor, and this
    /// method is the only reason the reconciler can still see it.
    fn missing_by_patch_id(&self, upstream: &str, head: &str, limit: &str) -> Result<Vec<String>> {
        let have = self.patches_of(&self.range(limit, upstream));
        Ok(self
            .range(limit, head)
            .into_iter()
            .filter(|c| self.commits.get(c).is_some_and(|k| !have.contains(&k.patch)))
            .collect())
    }

    fn reapply_verdict(&self, commit: &str, onto: &str) -> Result<Reapply> {
        Ok(*self
            .reapply
            .get(&(commit.to_string(), onto.to_string()))
            .unwrap_or(&Reapply::Missing))
    }

    fn ahead_behind(&self, base: &str, head: &str) -> Result<Option<AheadBehind>> {
        // A missing tracking ref is `None`, not zero -- "never pushed" and "in
        // sync" are different facts and the reconciler branches on which.
        if self.resolve(base).is_none() || self.resolve(head).is_none() {
            return Ok(None);
        }
        Ok(Some(AheadBehind {
            ahead: self.range(base, head).len() as u32,
            behind: self.range(head, base).len() as u32,
        }))
    }

    fn count_commits(&self, range: &str) -> Result<u32> {
        Ok(match range.split_once("..") {
            Some((e, i)) => self.range(e, i).len() as u32,
            None => self.resolve(range).map(|o| self.ancestors(&o).len()).unwrap_or(0) as u32,
        })
    }

    fn local_branches(&self) -> Result<Vec<String>> {
        Ok(self.branches.clone())
    }

    fn tags(&self) -> Result<Vec<String>> {
        Ok(self.tags.clone())
    }

    /// Derived from the tag list and the ref table, the same way git derives
    /// it -- this fake's whole ethos is that nothing is canned.
    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
        Ok(self
            .tags
            .iter()
            .filter_map(|t| self.resolve(t).map(|o| (t.clone(), o)))
            .collect())
    }

    /// Derived from the DAG: a branch contains `oid` iff `oid` is in its
    /// ancestor set. `remote` selects on the `remote/branch` naming convention,
    /// matching how `remote_branch` registers refs.
    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>> {
        Ok(self
            .refs
            .iter()
            .filter(|(n, _)| n.contains('/') == remote)
            .filter(|(_, o)| self.ancestors(o).contains(oid))
            .map(|(n, _)| n.clone())
            .collect())
    }

    // Not modelled -> an ERROR, so a test cannot pass on a question this
    // fixture was never taught to answer.
    fn git_dir(&self) -> Result<std::path::PathBuf> {
        Err(GitError::Unsupported { method: "git_dir" })
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
    fn head_branch(&self) -> Result<Option<String>> {
        Err(GitError::Unsupported { method: "head_branch" })
    }
    fn config_file_get(&self, _: &str, _: &str) -> Result<Option<String>> {
        Err(GitError::Unsupported { method: "config_file_get" })
    }

    /// `Unknown` until a test states a pin, which is the honest answer and not
    /// a benign default: every collapse of "unknown" into a definite value in
    /// this tool has produced a confident wrong answer.
    fn superproject_pin(&self) -> Result<sodactl_git::Pin> {
        match (&self.pin_committed, &self.pin_staged) {
            (None, None) => Ok(sodactl_git::Pin::Unknown {
                reason: "this fake was not given a pin".into(),
            }),
            (committed, staged) => Ok(sodactl_git::Pin::Known {
                committed: committed.clone(),
                staged: staged.clone(),
            }),
        }
    }

    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>> {
        Ok(self.gitlinks.get(path).cloned())
    }
}

// ---- manifest ergonomics --------------------------------------------------

/// Wrap a fragment in the boilerplate every manifest needs. `home_base` is
/// `c01` because tests create the fork base first; keep that convention.
pub fn manifest(body: &str) -> sodactl_core::Manifest {
    let src = format!(
        r#"schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "c01"
describes = "fake-base"
derivation = "fake"
{body}"#
    );
    sodactl_core::Manifest::parse(&src).expect("fixture manifest must parse")
}

/// Finding codes for one subject. Assertions read better as sets than as
/// indexes into a sorted vec that a new finding would silently shift.
pub fn codes_for(r: &sodactl_core::Report, subject: &str) -> BTreeSet<String> {
    r.findings
        .iter()
        .filter(|f| f.subject == subject)
        .map(|f| f.code.to_string())
        .collect()
}

pub fn all_codes(r: &sodactl_core::Report) -> BTreeSet<String> {
    r.findings.iter().map(|f| f.code.to_string()).collect()
}

// ---- the fake's own tests -------------------------------------------------
//
// A fake that lies is worse than no test, so it gets tested too. These assert
// the one property the whole strategy rests on: that ancestry and patch-id
// equivalence disagree about a replay, and that the fake reproduces the
// disagreement rather than papering over it.

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn replay_is_patch_equivalent_but_not_an_ancestor() {
        let mut g = FakeRepo::new();
        g.root("base");
        g.branch("base", "c01");
        g.tier("feature", "base", &["f1"]);
        g.replay("stitch", "base", &["f1"]);

        // The trap: the stitch carries `feature`'s change, yet `feature` is not
        // in its history and their tips are different objects.
        assert!(!g.is_ancestor("feature", "stitch").unwrap());
        assert_ne!(g.rev_parse("feature").unwrap(), g.rev_parse("stitch").unwrap());
        assert_eq!(g.count_commits("base..feature").unwrap(), g.count_commits("base..stitch").unwrap());

        // ...and only patch-id sees it.
        assert!(g.missing_by_patch_id("stitch", "feature", "base").unwrap().is_empty());
    }

    #[test]
    fn a_genuinely_absent_change_is_reported() {
        let mut g = FakeRepo::new();
        g.root("base");
        g.branch("base", "c01");
        g.tier("feature", "base", &["f1", "f2"]);
        g.replay("stitch", "base", &["f1"]); // stitched before f2 landed

        assert_eq!(g.missing_by_patch_id("stitch", "feature", "base").unwrap().len(), 1);
    }

    #[test]
    fn ancestry_and_ranges_track_the_parent_walk() {
        let mut g = FakeRepo::new();
        let base = g.root("base");
        let old = g.commits_on(&base, &["p1"]);
        g.tier("parent", &old, &["p2"]);
        g.tier("child", &old, &["k1"]); // authored on the OLD parent tip

        assert!(!g.is_ancestor("parent", "child").unwrap());
        assert!(g.is_ancestor(&old, "child").unwrap());
        assert_eq!(g.count_commits("parent..child").unwrap(), 1);
        assert_eq!(g.ahead_behind("parent", "child").unwrap().unwrap().behind, 1);
    }

    #[test]
    fn a_missing_tracking_ref_is_none_not_zero() {
        let mut g = FakeRepo::new();
        g.root("base");
        g.tier("t", "c01", &["t1"]);
        assert!(g.ahead_behind("codeberg/t", "t").unwrap().is_none());
    }
}
