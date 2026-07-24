//! `resume` -- continue a conflicted replay, after verifying what can be
//! verified. The design doc's rule 3: "never auto-resume. `sodactl resume`
//! re-verifies nothing else moved and runs `git rebase --continue`. Nothing
//! more."
//!
//! What "nothing else moved" can honestly mean here: the newest journal
//! recorded the plan's `expected_before` oids the moment the run started, and
//! a rebase does not move the branch ref until it finishes -- so every
//! journalled expectation must STILL hold while the replay sits conflicted.
//! One that does not means an external writer touched a planned ref
//! mid-recovery, and continuing would build on a fiction: refuse. Without a
//! journal there is nothing to verify against, and the honest move is the
//! opposite one -- WARN, naming exactly what could not be checked, and let the
//! manual workflow proceed (blocking would just push the human to raw
//! `git rebase --continue`, which verifies even less).

use std::fmt;
use std::path::PathBuf;

use crate::plan::Journal;
use crate::treestate;
use sodactl_git::GitRead;

/// What `resume` verified, warned about, and is about to run. Rendering is the
/// CLI's job; nothing here prints.
#[derive(Debug, serde::Serialize)]
pub struct ResumeReport {
    /// The in-flight markers found in the gitdir, e.g. `rebase-merge`.
    pub markers: Vec<String>,
    /// The check's own narrative fragment (" replaying X onto Y, at step
    /// n/e"), possibly empty.
    pub narrative: String,
    /// The branch the replay is rewriting (`rebase-merge/head-name`), when
    /// the metadata names one.
    pub replaying: Option<String>,
    /// The journal the verification ran against, when one existed.
    pub journal: Option<PathBuf>,
    /// That journal's verb -- the CLI uses `Some("restack")` to remind about
    /// the postcondition that never ran.
    pub journal_verb: Option<String>,
    /// One line per journalled ref that still sits where the plan expected.
    pub verified: Vec<String>,
    /// Everything that could NOT be verified, named. Clear over silent.
    pub warnings: Vec<String>,
}

/// The two ways `resume` refuses before touching anything.
#[derive(Debug)]
pub enum ResumeRefusal {
    /// Nothing is in flight -- there is nothing to continue.
    NotInFlight,
    /// The newest journal expected a ref somewhere it no longer is.
    JournalContradicted { refname: String, expected: String, live: String, journal: PathBuf },
}

impl fmt::Display for ResumeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResumeRefusal::NotInFlight => write!(
                f,
                "refused: nothing to resume -- no replay is in progress in the wine submodule"
            ),
            ResumeRefusal::JournalContradicted { refname, expected, live, journal } => write!(
                f,
                "refused: {refname} moved while the replay sat conflicted -- the journal ({}) \
                 expected {expected}, live is {live}; continuing would build on a ref someone \
                 else moved. Sort out who moved it, then re-run",
                journal.display()
            ),
        }
    }
}

impl std::error::Error for ResumeRefusal {}

/// Verify what can be verified; refuse or report. Read-only -- the actual
/// `rebase --continue` is one `GitMut` call the CLI makes after showing this.
pub fn check_resume(git: &dyn GitRead) -> anyhow::Result<ResumeReport> {
    let gd = git.git_dir()?;
    let Some(fl) = treestate::in_flight(&gd) else {
        return Err(anyhow::Error::new(ResumeRefusal::NotInFlight));
    };

    // Structural, not prose: the same gitdir files the in-flight probe stats.
    // (`rebase-apply` is the `git am`-backed rebase; both spell head-name the
    // same way.)
    let replaying = ["rebase-merge/head-name", "rebase-apply/head-name"]
        .iter()
        .find_map(|p| std::fs::read_to_string(gd.join(p)).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut warnings = Vec::new();
    let mut verified = Vec::new();

    // The newest journal, by its own UTC stamp (the format sorts
    // lexicographically). An unreadable journal is a warning with a name, not
    // a silent skip -- and not a hard error either, because the journal is
    // evidence FOR the human, and refusing to continue because the evidence
    // file is corrupt helps nobody.
    let jdir = gd.join("sodactl").join("journal");
    let mut journals: Vec<(String, PathBuf, Journal)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&jdir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|s| serde_json::from_str::<Journal>(&s).map_err(|e| e.to_string()))
            {
                Ok(j) => journals.push((j.started_at_utc.clone(), p, j)),
                Err(e) => warnings.push(format!(
                    "journal {} is unreadable ({e}) -- it verified nothing",
                    p.display()
                )),
            }
        }
    }
    journals.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    let (journal, journal_verb) = match journals.pop() {
        None => {
            warnings.push(format!(
                "no journal under {} -- whether other refs moved while the replay sat could not \
                 be verified; continuing means trusting the tree as-is",
                jdir.display()
            ));
            (None, None)
        }
        Some((_, path, j)) => {
            if j.completed_steps == j.plan.steps.len() {
                warnings.push(format!(
                    "the newest journal ({} `{}`) records a COMPLETED run -- this replay was not \
                     started by a sodactl plan, so there is nothing to verify it against",
                    path.display(),
                    j.verb
                ));
            } else {
                if let Some(r) = &replaying {
                    if !j.plan.mutations.iter().any(|mu| mu.refname == *r) {
                        warnings.push(format!(
                            "the replay in flight rewrites {r}, which the newest journal (`{}`) \
                             never planned -- its ref checks prove nothing about this replay",
                            j.verb
                        ));
                    }
                }
                for mu in &j.plan.mutations {
                    let live = git.rev_parse_raw(&mu.refname)?;
                    if live == mu.expected_before {
                        verified.push(format!(
                            "{} still at {} (as the journal expected)",
                            mu.refname,
                            mu.expected_before.as_deref().unwrap_or("<absent>")
                        ));
                    } else {
                        return Err(anyhow::Error::new(ResumeRefusal::JournalContradicted {
                            refname: mu.refname.clone(),
                            expected: mu
                                .expected_before
                                .clone()
                                .unwrap_or_else(|| "<absent>".into()),
                            live: live.unwrap_or_else(|| "<absent>".into()),
                            journal: path.clone(),
                        }));
                    }
                }
            }
            (Some(path), Some(j.verb))
        }
    };

    Ok(ResumeReport {
        markers: fl.markers.iter().map(|m| m.to_string()).collect(),
        narrative: fl.narrative,
        replaying,
        journal,
        journal_verb,
        verified,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Plan, PlannedValue, RefMutation, RepoSide};
    use sodactl_git::{GitCli, GitMut};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out =
            Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn git_may_fail(dir: &Path, args: &[&str]) {
        let _ = Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
    }

    fn scratch(name: &str) -> (Scratch, GitCli) {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-core-resume-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "one"]);
        (Scratch(dir.clone()), GitCli::new(&dir))
    }

    fn commit(dir: &Path, file: &str, content: &str, msg: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", msg]);
    }

    /// Wedge a real rebase on a real conflict; returns the topic's pre-rebase
    /// tip.
    fn wedge(dir: &Path, g: &GitCli) -> String {
        git(dir, &["checkout", "-q", "-b", "topic"]);
        commit(dir, "a.txt", "topic\n", "topic work");
        let tip = g.rev_parse("topic").unwrap().unwrap();
        git(dir, &["checkout", "-q", "main"]);
        commit(dir, "a.txt", "main\n", "main work");
        git_may_fail(dir, &["rebase", "main", "topic"]);
        tip
    }

    fn write_journal(g: &GitCli, plan: Plan) -> PathBuf {
        let dir = g.git_dir().unwrap().join("sodactl").join("journal");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.json", plan.id));
        let j = Journal {
            verb: plan.verb.clone(),
            started_at_utc: "20260724T000000Z".into(),
            backups: vec![],
            completed_steps: 0,
            plan,
        };
        std::fs::write(&path, serde_json::to_string_pretty(&j).unwrap()).unwrap();
        path
    }

    fn mutation(refname: &str, expected: &str) -> RefMutation {
        RefMutation {
            repo: RepoSide::Wine,
            refname: refname.into(),
            expected_before: Some(expected.into()),
            after: PlannedValue::Unknown { reason: "test".into() },
        }
    }

    /// A one-step plan so `completed_steps: 0` reads as incomplete.
    fn one_step_plan(mutations: Vec<RefMutation>) -> Plan {
        use crate::plan::{Action, Step};
        Plan::new(
            "restack",
            vec![Step::new("the wedged rebase", RepoSide::Wine, Action::RebaseOnto {
                newbase: "x".into(),
                upstream: "y".into(),
                branch: "topic".into(),
            })],
            mutations,
            vec![],
            vec![],
        )
    }

    #[test]
    fn a_clean_tree_refuses_there_is_nothing_to_continue() {
        let (_s, g) = scratch("clean");
        let err = check_resume(&g).unwrap_err();
        let r = err.downcast_ref::<ResumeRefusal>().expect("typed refusal");
        assert!(matches!(r, ResumeRefusal::NotInFlight));
        assert!(r.to_string().starts_with("refused: "), "{r}");
    }

    #[test]
    fn without_a_journal_it_warns_by_name_and_proceeds() {
        let (s, g) = scratch("nojournal");
        wedge(&s.0, &g);
        let rep = check_resume(&g).unwrap();
        assert!(rep.journal.is_none());
        assert!(
            rep.warnings.iter().any(|w| w.contains("no journal") && w.contains("could not")),
            "the warning must name what was not verified: {:?}",
            rep.warnings
        );
        assert_eq!(rep.replaying.as_deref(), Some("refs/heads/topic"));
        git_may_fail(&s.0, &["rebase", "--abort"]);
    }

    #[test]
    fn a_matching_journal_verifies_and_a_moved_ref_refuses() {
        let (s, g) = scratch("journal");
        let tip = wedge(&s.0, &g);
        let main = g.rev_parse_raw("refs/heads/main").unwrap().unwrap();

        // The journalled expectations both still hold: the rebase has not
        // moved `topic` yet (it moves the ref only at the end), and `main` sat
        // still. Every mutation must come back as verified.
        write_journal(
            &g,
            one_step_plan(vec![
                mutation("refs/heads/topic", &tip),
                mutation("refs/heads/main", &main),
            ]),
        );
        let rep = check_resume(&g).unwrap();
        assert_eq!(rep.verified.len(), 2, "{:?}", rep.warnings);
        assert_eq!(rep.journal_verb.as_deref(), Some("restack"));

        // Now the journal claims main was somewhere it is not -- an external
        // writer moved a planned ref mid-recovery. Newer stamp so it wins the
        // newest-first pick; continuing must refuse.
        let contradicted = {
            let dir = g.git_dir().unwrap().join("sodactl").join("journal");
            let plan = one_step_plan(vec![mutation(
                "refs/heads/main",
                "1111111111111111111111111111111111111111",
            )]);
            let path = dir.join(format!("{}.json", plan.id));
            let j = Journal {
                verb: plan.verb.clone(),
                started_at_utc: "20260724T000001Z".into(),
                backups: vec![],
                completed_steps: 0,
                plan,
            };
            std::fs::write(&path, serde_json::to_string_pretty(&j).unwrap()).unwrap();
            path
        };
        let err = check_resume(&g).unwrap_err();
        match err.downcast_ref::<ResumeRefusal>().expect("typed refusal") {
            ResumeRefusal::JournalContradicted { refname, journal, .. } => {
                assert_eq!(refname, "refs/heads/main");
                assert_eq!(journal, &contradicted);
            }
            other => panic!("wrong refusal: {other:?}"),
        }
        git_may_fail(&s.0, &["rebase", "--abort"]);
    }

    #[test]
    fn after_resolving_rebase_continue_finishes_the_replay() {
        let (s, g) = scratch("continue");
        wedge(&s.0, &g);
        // Resolve the conflict the way a human would, then continue through
        // the same GitMut verb the CLI uses.
        std::fs::write(s.0.join("a.txt"), "resolved\n").unwrap();
        git(&s.0, &["add", "--", "a.txt"]);
        GitMut::new(&s.0).rebase_continue().unwrap();
        assert!(treestate::in_flight(&g.git_dir().unwrap()).is_none(), "the replay must be done");
        assert!(g.is_ancestor("main", "topic").unwrap());
    }
}
