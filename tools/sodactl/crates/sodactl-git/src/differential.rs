//! The differential harness: one `GitRead` that asks two backends and keeps
//! score.
//!
//! `Differential` wraps a *reference* backend (`GitCli` -- the oracle, whose
//! behaviour is the contract) and a *candidate* (`GitGix`), implements
//! `GitRead` by calling BOTH, recording agreement, and returning the
//! **reference's** answer. Two consequences are the whole point:
//!
//! 1. **Compiler-enforced completeness.** Adding a method to `GitRead` breaks
//!    this impl -- whoever adds it is forced to decide, in code, *compared* or
//!    *excluded-with-reason*. No reflection, no name list to forget.
//! 2. **Argument harvesting is free and real.** Because `Differential` is a
//!    `GitRead`, the entire production pipeline runs through it unmodified --
//!    the candidate is exercised with exactly the tier names, ranges, tags and
//!    oids production asks about, in production's distribution, not an
//!    invented sample. And because the reference's answer flows onward, the
//!    run's *output* is the golden-master output by construction while the
//!    candidate is tested on the side.
//!
//! Why this exists at all: the fold recorded at `lib.rs` (GitRead + GitQ)
//! happened because two implementations of one question drifted and *nothing
//! compared them*. The hybrid backend deliberately reintroduces a duplicate
//! for 18 methods -- and this module is the mechanism that made the last
//! duplication fatal, inverted: the comparison is a compiled, CI-run artifact.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::{AheadBehind, GitError, GitRead, Oid, Pin, Reapply, Result, StatusEntry};

/// One recorded disagreement, carrying everything a human needs to adjudicate.
#[derive(Debug)]
pub struct Disagreement {
    pub method: &'static str,
    /// `Debug` of the argument tuple, e.g. `("soda-core..soda-branding",)`.
    pub args: String,
    /// `Debug` of the reference's `Result`.
    pub reference: String,
    /// `Debug` of the candidate's `Result`.
    pub candidate: String,
    /// Set when both sides returned the same multiset in a different order --
    /// the expected failure class of this migration, called out by name so it
    /// cannot hide inside a wall of Debug output.
    pub order_only: bool,
    /// For sequences: the first index at which they differ, rendered.
    pub detail: Option<String>,
}

#[derive(Default)]
struct Log {
    calls: usize,
    agreements: usize,
    /// Both sides refused. Counted separately so a run of pure double-failures
    /// cannot masquerade as health -- 40 calls, 40 agreements, 40 of them
    /// Err/Err is a very different report from 40 real answers.
    err_err: usize,
    disagreements: Vec<Disagreement>,
    /// method -> call count for the excluded-by-decision methods. Seeded at
    /// construction so a method that was excluded but never called still
    /// renders ("0 calls -- NOT EXERCISED"), and an exclusion can neither
    /// hide nor rot.
    excluded: BTreeMap<&'static str, usize>,
}

/// The finished scorecard. Render it with `Display`; gate on
/// [`DifferentialReport::is_clean`].
pub struct DifferentialReport {
    pub calls: usize,
    pub agreements: usize,
    pub err_err: usize,
    pub disagreements: Vec<Disagreement>,
    pub excluded: Vec<(&'static str, usize)>,
    pub reference_label: &'static str,
    pub candidate_label: &'static str,
}

impl DifferentialReport {
    /// No disagreements. Note this deliberately does NOT require any
    /// agreements: a run that never called git at all is reported honestly by
    /// the counters, and the caller decides whether zero coverage is
    /// acceptable for its context (the hermetic test asserts a floor).
    pub fn is_clean(&self) -> bool {
        self.disagreements.is_empty()
    }
}

impl std::fmt::Display for DifferentialReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "DIFFERENTIAL sodactl-git  --  reference: {}   candidate: {}",
            self.reference_label, self.candidate_label
        )?;
        writeln!(
            f,
            "  compared   {} calls: {} agree ({} of them as Err/Err)",
            self.calls, self.agreements, self.err_err
        )?;
        writeln!(f, "  EXCLUDED BY DECISION (answered by the reference backend only):")?;
        for (m, n) in &self.excluded {
            let why = match *m {
                "status" => "gix dirwalk emits empty dirs + double-counts gitlinks (2026-07-19)",
                "missing_by_patch_id" => "gitoxide has no patch-id; reconstruction was silently wrong",
                _ => "(no rationale recorded -- add one)",
            };
            let calls = if *n == 0 {
                "0 calls -- NOT EXERCISED THIS RUN".to_string()
            } else {
                format!("{n} calls")
            };
            writeln!(f, "    {m:<20} {calls:<32} {why}")?;
        }
        if self.disagreements.is_empty() {
            writeln!(f, "  DISAGREEMENTS: none")?;
        } else {
            for (i, d) in self.disagreements.iter().enumerate() {
                writeln!(f, "  DISAGREEMENT #{}", i + 1)?;
                writeln!(f, "    method     {}", d.method)?;
                writeln!(f, "    args       {}", d.args)?;
                writeln!(f, "    reference  {}", d.reference)?;
                if d.order_only {
                    writeln!(f, "    candidate  {}   ORDER DIFFERS (multisets identical)", d.candidate)?;
                } else {
                    writeln!(f, "    candidate  {}", d.candidate)?;
                }
                if let Some(detail) = &d.detail {
                    writeln!(f, "      {detail}")?;
                }
            }
        }
        Ok(())
    }
}

/// Answers that can hint *how* they differ. Default: no hint.
trait Answer: PartialEq + std::fmt::Debug {
    fn order_only(&self, _other: &Self) -> bool {
        false
    }
    fn first_divergence(&self, _other: &Self) -> Option<String> {
        None
    }
}

impl<T: Ord + Clone + std::fmt::Debug> Answer for Vec<T> {
    fn order_only(&self, other: &Self) -> bool {
        let (mut a, mut b) = (self.clone(), other.clone());
        a.sort();
        b.sort();
        a == b
    }
    fn first_divergence(&self, other: &Self) -> Option<String> {
        let i = self.iter().zip(other).position(|(a, b)| a != b).unwrap_or_else(|| self.len().min(other.len()));
        Some(format!(
            "first divergence at [{i}]: reference {:?} vs candidate {:?} (lengths {} / {})",
            self.get(i),
            other.get(i),
            self.len(),
            other.len()
        ))
    }
}

/// The scalar answers: equal or not, nothing more to say.
macro_rules! scalar_answer {
    ($($ty:ty),* $(,)?) => {$( impl Answer for $ty {} )*};
}
scalar_answer!(Option<Oid>, PathBuf, bool, u32, Reapply, Option<AheadBehind>, Pin);

pub struct Differential<'a> {
    reference: &'a dyn GitRead,
    candidate: &'a dyn GitRead,
    /// Single-threaded by design; the CLI is one-shot and the harness runs in
    /// one thread. `RefCell` makes that a compile-time property (`!Sync`)
    /// rather than a comment.
    log: RefCell<Log>,
}

impl<'a> Differential<'a> {
    pub fn new(reference: &'a dyn GitRead, candidate: &'a dyn GitRead) -> Self {
        let mut excluded = BTreeMap::new();
        // Seeded, not lazily inserted: the report must show the exclusions
        // even on a run that never reached them.
        excluded.insert("status", 0);
        excluded.insert("missing_by_patch_id", 0);
        Self { reference, candidate, log: RefCell::new(Log { excluded, ..Default::default() }) }
    }

    fn record<T: Answer>(&self, method: &'static str, args: String, r: &Result<T>, c: &Result<T>) {
        let mut log = self.log.borrow_mut();
        log.calls += 1;
        match (r, c) {
            (Ok(a), Ok(b)) if a == b => log.agreements += 1,
            // Both refused: agreement of CLASSIFICATION. Messages are not
            // compared -- git stderr prose and gix Display legitimately
            // differ, and comparing them would re-create the German-stderr
            // bug as a permanent flake. `Unsupported` is checked on BOTH
            // sides: it used to be candidate-only, which would have let a
            // reference-side Unsupported against a real candidate error pass
            // as agreement (moot while the reference is the complete GitCli,
            // but the harness must not depend on that staying true).
            (Err(e), Err(f))
                if !matches!(e, GitError::Unsupported { .. })
                    && !matches!(f, GitError::Unsupported { .. }) =>
            {
                log.agreements += 1;
                log.err_err += 1;
            }
            // Everything else disagrees -- including Unsupported from EITHER
            // side on a compared method, which means that side did not even
            // try: a routing bug, not a wrong answer.
            (r, c) => {
                let (order_only, detail) = match (r, c) {
                    (Ok(a), Ok(b)) => (a.order_only(b), a.first_divergence(b)),
                    _ => (false, None),
                };
                log.disagreements.push(Disagreement {
                    method,
                    args,
                    reference: format!("{r:?}"),
                    candidate: format!("{c:?}"),
                    order_only,
                    detail,
                });
            }
        }
    }

    fn excluded(&self, method: &'static str) {
        *self.log.borrow_mut().excluded.entry(method).or_default() += 1;
    }

    pub fn finish(self) -> DifferentialReport {
        let log = self.log.into_inner();
        DifferentialReport {
            calls: log.calls,
            agreements: log.agreements,
            err_err: log.err_err,
            disagreements: log.disagreements,
            excluded: log.excluded.into_iter().collect(),
            reference_label: "git binary",
            candidate_label: "gitoxide 0.85",
        }
    }
}

/// The compared methods, one macro so the bodies cannot drift: call both,
/// record, return the REFERENCE's answer (the run is the golden run).
macro_rules! compared {
    ($(fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {$(
        fn $name(&self $(, $arg: $ty)*) -> Result<$ret> {
            let r = self.reference.$name($($arg),*);
            let c = self.candidate.$name($($arg),*);
            self.record(stringify!($name), format!("{:?}", ($($arg),*)), &r, &c);
            r
        }
    )*};
}

impl GitRead for Differential<'_> {
    compared! {
        fn rev_parse(&self, rev: &str) -> Option<Oid>;
        fn rev_parse_raw(&self, rev: &str) -> Option<Oid>;
        fn git_dir(&self) -> PathBuf;
        fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool;
        fn reapply_verdict(&self, commit: &str, onto: &str) -> Reapply;
        fn ahead_behind(&self, base: &str, head: &str) -> Option<AheadBehind>;
        fn count_commits(&self, range: &str) -> u32;
        fn local_branches(&self) -> Vec<String>;
        fn tags(&self) -> Vec<String>;
        fn tags_with_targets(&self) -> Vec<(String, Oid)>;
        fn tags_pointing_at(&self, oid: &str) -> Vec<String>;
        fn branches_containing(&self, oid: &str, remote: bool) -> Vec<String>;
        fn log_subjects(&self, range: &str) -> Vec<(Oid, String)>;
        fn merges_in(&self, range: &str) -> Vec<Oid>;
        fn submodule_index_oid(&self, path: &str) -> Option<Oid>;
        fn head_branch(&self) -> Option<String>;
        fn config_file_get(&self, file: &str, key: &str) -> Option<String>;
        fn superproject_pin(&self) -> Pin;
    }

    // EXCLUDED BY DECISION -- explicit bodies, never a macro arm, so the
    // exclusion is a grep-able statement with its rationale attached.

    /// Binary-only. gix's dirwalk emits `.brv` (7 empty directories git cannot
    /// represent under any `--untracked-files` setting) and double-counts the
    /// `wine` gitlink as TreeIndex + IndexWorktree where git emits one record;
    /// `checks/rr.rs` reports `dirty.len()`, so a naive port inflates it.
    /// Decided 2026-07-19; revisit only with a measured gix `status` that
    /// matches git on this repo.
    fn status(&self, include_untracked: bool) -> Result<Vec<StatusEntry>> {
        self.excluded("status");
        self.reference.status(include_untracked)
    }

    /// Binary-only, permanently. gitoxide has no patch-id (zero hits for
    /// `patch[_-]id` across all 48 gix crates, nothing on the roadmap); the
    /// ~130 LOC reconstruction was tried and produced a SILENT FALSE CLEAN --
    /// it merged "unlink frame elements" with "unlink iframe elements" into
    /// one equivalence class and suppressed a real STITCH_STALE. Never
    /// reimplement this.
    fn missing_by_patch_id(&self, u: &str, h: &str, l: &str) -> Result<Vec<String>> {
        self.excluded("missing_by_patch_id");
        self.reference.missing_by_patch_id(u, h, l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake that answers a fixed script, for exercising the scorekeeping
    /// without a repository.
    struct Scripted {
        head: Head,
        tags: Vec<String>,
    }

    /// What `head_branch` answers: a value, a typed hole, or a REAL failure.
    /// The distinction matters to the scorekeeper -- Err/Err agrees only when
    /// neither side is `Unsupported`.
    enum Head {
        Val(Option<String>),
        Unsupported,
        Broken,
    }

    impl GitRead for Scripted {
        fn rev_parse(&self, _: &str) -> Result<Option<Oid>> {
            Err(GitError::Unsupported { method: "rev_parse" })
        }
        fn rev_parse_raw(&self, _: &str) -> Result<Option<Oid>> {
            Err(GitError::Unsupported { method: "rev_parse_raw" })
        }
        fn git_dir(&self) -> Result<PathBuf> {
            Err(GitError::Unsupported { method: "git_dir" })
        }
        fn is_ancestor(&self, _: &str, _: &str) -> Result<bool> {
            Err(GitError::Unsupported { method: "is_ancestor" })
        }
        fn missing_by_patch_id(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
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
            Ok(self.tags.clone())
        }
        fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
            Err(GitError::Unsupported { method: "tags_with_targets" })
        }
        fn branches_containing(&self, _: &str, _: bool) -> Result<Vec<String>> {
            Err(GitError::Unsupported { method: "branches_containing" })
        }
        fn log_subjects(&self, _: &str) -> Result<Vec<(Oid, String)>> {
            Err(GitError::Unsupported { method: "log_subjects" })
        }
        fn merges_in(&self, _: &str) -> Result<Vec<Oid>> {
            Err(GitError::Unsupported { method: "merges_in" })
        }
        fn status(&self, _: bool) -> Result<Vec<StatusEntry>> {
            Ok(vec![])
        }
        fn submodule_index_oid(&self, _: &str) -> Result<Option<Oid>> {
            Err(GitError::Unsupported { method: "submodule_index_oid" })
        }
        fn head_branch(&self) -> Result<Option<String>> {
            match &self.head {
                Head::Val(v) => Ok(v.clone()),
                Head::Unsupported => Err(GitError::Unsupported { method: "head_branch" }),
                Head::Broken => Err(GitError::Gix {
                    op: "head",
                    cwd: ".".into(),
                    detail: "scripted failure".into(),
                }),
            }
        }
        fn config_file_get(&self, _: &str, _: &str) -> Result<Option<String>> {
            Err(GitError::Unsupported { method: "config_file_get" })
        }
    }

    fn fake(head: Head, tags: &[&str]) -> Scripted {
        Scripted { head, tags: tags.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn agreement_and_disagreement_are_scored() {
        let a = fake(Head::Val(Some("main".into())), &[]);
        let b = fake(Head::Val(Some("main".into())), &[]);
        let d = Differential::new(&a, &b);
        assert_eq!(d.head_branch().unwrap().as_deref(), Some("main"));
        let r = d.finish();
        assert!(r.is_clean());
        assert_eq!((r.calls, r.agreements), (1, 1));

        let c = fake(Head::Val(Some("other".into())), &[]);
        let d = Differential::new(&a, &c);
        // The REFERENCE's answer flows onward -- the run stays the golden run
        // even while the candidate is wrong.
        assert_eq!(d.head_branch().unwrap().as_deref(), Some("main"));
        let r = d.finish();
        assert!(!r.is_clean());
        assert_eq!(r.disagreements.len(), 1);
        assert_eq!(r.disagreements[0].method, "head_branch");
    }

    #[test]
    fn an_unsupported_candidate_on_a_compared_method_is_a_disagreement() {
        // Err/Err normally agrees (classification), but Unsupported means the
        // candidate did not even try -- a routing bug the harness must not
        // absorb as agreement.
        let a = fake(Head::Unsupported, &[]); // reference: Unsupported here too
        let b = fake(Head::Unsupported, &[]);
        let d = Differential::new(&a, &b);
        let _ = d.head_branch();
        let r = d.finish();
        assert!(!r.is_clean(), "candidate Unsupported must never count as agreement");
    }

    #[test]
    fn an_unsupported_reference_against_a_real_error_is_a_disagreement() {
        // The converse of the arm above: the guard used to check only the
        // CANDIDATE for Unsupported, so a reference that did not even try,
        // paired with a candidate that tried and failed, scored as agreement.
        // Moot while the reference is the complete GitCli -- but the harness
        // must not lean on that staying true.
        let a = fake(Head::Unsupported, &[]);
        let b = fake(Head::Broken, &[]);
        let d = Differential::new(&a, &b);
        let _ = d.head_branch();
        let r = d.finish();
        assert!(!r.is_clean(), "reference Unsupported must never count as agreement");
    }

    #[test]
    fn two_real_errors_still_agree_as_classification() {
        // The converse of the converse, so the tightened guard cannot become
        // a mute button on legitimate Err/Err agreement: two REAL failures
        // (neither a typed hole) are agreement of classification.
        let a = fake(Head::Broken, &[]);
        let b = fake(Head::Broken, &[]);
        let d = Differential::new(&a, &b);
        let _ = d.head_branch();
        let r = d.finish();
        assert!(r.is_clean(), "{r}");
        assert_eq!((r.calls, r.agreements, r.err_err), (1, 1, 1));
    }

    #[test]
    fn order_only_divergence_is_named_as_such() {
        let a = fake(Head::Val(None), &["b", "a"]);
        let b = fake(Head::Val(None), &["a", "b"]);
        let d = Differential::new(&a, &b);
        let _ = d.tags();
        let r = d.finish();
        assert_eq!(r.disagreements.len(), 1);
        assert!(r.disagreements[0].order_only, "same multiset, different order");
        assert!(r.disagreements[0].detail.as_deref().unwrap().contains("[0]"));
    }

    #[test]
    fn excluded_methods_render_even_when_never_called() {
        let a = fake(Head::Val(None), &[]);
        let b = fake(Head::Val(None), &[]);
        let d = Differential::new(&a, &b);
        let _ = d.status(false); // exercised
        let r = d.finish();
        let text = r.to_string();
        assert!(text.contains("status") && text.contains("1 calls"), "{text}");
        assert!(
            text.contains("missing_by_patch_id") && text.contains("NOT EXERCISED"),
            "an exclusion must be visible even at zero calls: {text}"
        );
        // Excluded calls are answered by the reference and never scored.
        assert_eq!(r.calls, 0);
    }
}
