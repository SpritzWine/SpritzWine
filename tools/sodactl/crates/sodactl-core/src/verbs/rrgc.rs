//! `rr gc` -- prune the rerere cache of entries that can never help again.
//!
//! Three kinds of dead weight, each with a different death certificate:
//!
//! * **No postimage, older than `--older-than` (default 30 days):** a conflict
//!   rerere recorded whose resolution was never captured. It re-prompts on
//!   every replay forever while looking like a cache hit that did not work
//!   (`RR_NO_POSTIMAGE`). The age gate exists because a FRESH preimage is a
//!   conflict someone is probably mid-way through resolving -- pruning it
//!   would delete the recording just before the postimage arrives.
//! * **Shadowed by an UNSCOPED `[[binary_resolution]]` rule** (the
//!   `rrcache::shadowed_by` attribution, with its consumer-scoping lesson):
//!   `regen` applies the rule before rerere is ever consulted, so the entry is
//!   unreachable at any age.
//! * **Stray `thisimage` files:** rerere's transient mid-merge state,
//!   meaningless to every other clone; the FILE is removed, the entry kept.
//!
//! Pruning removes an entry from BOTH caches -- the tracked `tools/rr-cache`
//! and the live gitdir one. Tracked-only pruning was the obvious first cut
//! and it does not stick: the next regen's persist step (`cp live/. tracked/`)
//! would faithfully copy the pruned entry right back.
//!
//! The filesystem removals cannot be plan `Step`s (steps are git actions), so
//! the `RrGcPlan` carries them as typed data beside the `Plan` -- and the
//! CLI's drift comparison covers BOTH (`plan.id` alone would miss a prune set
//! that changed between review and apply). They execute as the engine's
//! PRELUDE (`plan::apply_with_prelude`): after the `expected_before`
//! pre-verify -- whose refusal promises "nothing was changed", which must be
//! true of the cache files too -- and before the git steps, whose `add` must
//! see the deletions to stage them.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::checks::Policy;
use crate::manifest::Manifest;
use crate::plan::{Action, ApplyCtx, Plan, PlannedValue, RefMutation, RepoSide, Step};
use crate::rrcache::{shadowed_by, RrSurvey};
use anyhow::Context as _;
use sodactl_git::GitRead;

/// Why an entry is being pruned. Carried verbatim into the plan rendering --
/// a prune without a stated reason is indistinguishable from someone deleting
/// an inconvenient resolution.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PruneReason {
    /// Preimage with no postimage, and old enough that nobody is plausibly
    /// mid-resolution.
    NoPostimage { age_days: u64 },
    /// An unscoped rule decides the path this entry's content belongs to.
    Shadowed { path: String },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Prune {
    pub id: String,
    pub reason: PruneReason,
}

/// The reviewable gc: the git `Plan` plus the filesystem work beside it.
#[derive(Debug, serde::Serialize)]
pub struct RrGcPlan {
    pub plan: Plan,
    /// Entries removed from both caches.
    pub prune: Vec<Prune>,
    /// Entry ids whose stray `thisimage` FILE is removed (entries kept).
    pub strays: Vec<String>,
    /// The tracked cache root (absolute).
    pub tracked_root: PathBuf,
    /// The live cache root (resolved gitdir), when the gitdir resolved.
    pub live_root: PathBuf,
}

/// Post-apply summary.
#[derive(Debug)]
pub struct RrGcOutcome {
    pub pruned: usize,
    pub strays_removed: usize,
    /// The superproject commit recording the prune, when one was planned.
    pub committed: Option<String>,
}

/// A gc-time precondition failure. Typed, `refused: `-prefixed, downcastable
/// -- exit 1.
#[derive(Debug)]
pub enum RrGcRefusal {
    /// The prune commit would be made on a detached superproject HEAD.
    DetachedSuperproject,
}

impl fmt::Display for RrGcRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RrGcRefusal::DetachedSuperproject => write!(
                f,
                "refused: the superproject HEAD is detached -- the prune commit would dangle; \
                 check a branch out first"
            ),
        }
    }
}

impl std::error::Error for RrGcRefusal {}

/// Plan the gc. `older_than_days` gates only the no-postimage class; shadowed
/// entries are unreachable at any age (module header).
pub fn plan_rrgc(
    m: &Manifest,
    p: &Policy,
    superproject: &dyn GitRead,
    wine: &dyn GitRead,
    superproject_path: &Path,
    older_than_days: u64,
) -> anyhow::Result<RrGcPlan> {
    let tracked_root = superproject_path.join(&p.rr_cache);
    let tracked = RrSurvey::read(&tracked_root)?;
    let live_root = wine.git_dir()?.join("rr-cache");

    // The same scoping rule as checks/rr.rs, via the same helper: only an
    // UNSCOPED rule shadows (the consumer-scoped `configure` false positive
    // whose "remedy" would have deleted two live resolutions).
    let shadowing: Vec<String> = m
        .binary_resolution
        .iter()
        .filter(|b| b.consumers.is_empty())
        .map(|b| b.path.clone())
        .collect();

    let now = SystemTime::now();
    let mut prune = Vec::new();
    let mut strays = Vec::new();
    for e in &tracked.entries {
        if e.has_thisimage {
            strays.push(e.id.clone());
        }
        if !e.unresolved() {
            continue;
        }
        if let Some(path) = shadowed_by(e, &shadowing) {
            prune.push(Prune { id: e.id.clone(), reason: PruneReason::Shadowed { path } });
            continue;
        }
        // Age = the preimage's mtime, the same clock `git rerere gc` reads.
        // An unreadable mtime keeps the entry: pruning on missing evidence is
        // the counts-as-evidence mistake with a delete attached.
        let Some(age_days) = std::fs::metadata(e.dir.join("preimage"))
            .and_then(|md| md.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|d| d.as_secs() / 86_400)
        else {
            continue;
        };
        if age_days >= older_than_days {
            prune.push(Prune { id: e.id.clone(), reason: PruneReason::NoPostimage { age_days } });
        }
    }
    // A stray on a pruned entry disappears with the entry; listing it twice
    // would double-count the work.
    strays.retain(|id| !prune.iter().any(|pr| &pr.id == id));

    let mut steps = vec![Step::new(
        "run git's own rerere gc in the wine repo (prunes the live cache by its age policy)",
        RepoSide::Wine,
        Action::RerereGc,
    )];
    let mut mutations = Vec::new();
    let mut notes = Vec::new();

    if !prune.is_empty() || !strays.is_empty() {
        let head_branch = superproject
            .head_branch()?
            .ok_or_else(|| anyhow::Error::new(RrGcRefusal::DetachedSuperproject))?;
        let head_oid = superproject
            .rev_parse_raw(&format!("refs/heads/{head_branch}"))?
            .with_context(|| format!("refs/heads/{head_branch} does not resolve"))?;
        let message = format!(
            "sodactl: rr gc -- prune {} entr{} from {}",
            prune.len(),
            if prune.len() == 1 { "y" } else { "ies" },
            p.rr_cache
        );
        steps.push(Step::new(
            "stage the pruned cache",
            RepoSide::Superproject,
            Action::Add { paths: vec![p.rr_cache.clone()] },
        ));
        steps.push(Step::new(
            "commit the prune (rr-cache paths only, sweep-proof)",
            RepoSide::Superproject,
            Action::CommitPaths { message, paths: vec![p.rr_cache.clone()] },
        ));
        mutations.push(RefMutation {
            repo: RepoSide::Superproject,
            refname: format!("refs/heads/{head_branch}"),
            expected_before: Some(head_oid),
            after: PlannedValue::Unknown {
                reason: "the prune commit mints a new oid; unknowable until it exists".into(),
            },
        });
        notes.push(
            "any OTHER uncommitted change under the rr-cache (newly learned resolutions) rides \
             along in the prune commit -- it is scoped to the cache directory, not to the pruned \
             entries"
                .to_string(),
        );
    } else {
        notes.push("nothing to prune -- only git's own rerere gc runs".to_string());
    }
    for pr in &prune {
        notes.push(match &pr.reason {
            PruneReason::NoPostimage { age_days } => format!(
                "prune {}: preimage without postimage, {age_days} day(s) old -- re-prompts forever",
                pr.id
            ),
            PruneReason::Shadowed { path } => format!(
                "prune {}: content attributed to `{path}`, which an unscoped [[binary_resolution]] \
                 rule decides -- unreachable at any age",
                pr.id
            ),
        });
    }
    for id in &strays {
        notes.push(format!("remove stray thisimage from {id} (transient mid-merge state; entry kept)"));
    }
    notes.push("entries are pruned from BOTH caches, or the next regen's persist step re-imports them".to_string());

    let plan = Plan::new("rr-gc", steps, mutations, vec![], notes);
    Ok(RrGcPlan { plan, prune, strays, tracked_root, live_root })
}

/// Execute: the filesystem removals ride as the engine's prelude (module
/// header: after everything that can refuse, before the `add` that must see
/// the deletions). The removals are the irreversible half, so a CAS refusal
/// from the engine really does mean the cache is untouched.
pub fn apply_rrgc(gp: &RrGcPlan, ctx: &ApplyCtx) -> anyhow::Result<RrGcOutcome> {
    let prune = || {
        for pr in &gp.prune {
            for root in [&gp.tracked_root, &gp.live_root] {
                let dir = root.join(&pr.id);
                if dir.is_dir() {
                    std::fs::remove_dir_all(&dir)
                        .with_context(|| format!("pruning {}", dir.display()))?;
                }
            }
        }
        for id in &gp.strays {
            for root in [&gp.tracked_root, &gp.live_root] {
                let f = root.join(id).join("thisimage");
                if f.is_file() {
                    std::fs::remove_file(&f).with_context(|| format!("removing {}", f.display()))?;
                }
            }
        }
        Ok(())
    };
    crate::plan::apply_with_prelude(&gp.plan, ctx, prune).map_err(anyhow::Error::new)?;
    let committed = if gp.plan.steps.iter().any(|s| matches!(s.action, Action::CommitPaths { .. }))
    {
        ctx.superproject_read.rev_parse("HEAD")?
    } else {
        None
    };
    Ok(RrGcOutcome { pruned: gp.prune.len(), strays_removed: gp.strays.len(), committed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sodactl_git::{GitCli, GitMut};
    use std::fs::{self, File, FileTimes};
    use std::process::Command;
    use std::time::Duration;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out =
            Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A superproject with a committed `tools/rr-cache` plus a bare-bones
    /// "wine" repo for the rerere-gc step; pid-scoped for the usual reason.
    fn scratch(name: &str) -> (Scratch, PathBuf, PathBuf) {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-rrgc-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let wine = dir.join("wine-standalone");
        for d in [&dir, &wine] {
            fs::create_dir_all(d).unwrap();
            git(d, &["init", "-q", "-b", "main"]);
            git(d, &["config", "user.email", "t@t"]);
            git(d, &["config", "user.name", "t"]);
        }
        fs::write(wine.join("w.txt"), "w\n").unwrap();
        git(&wine, &["add", "-A"]);
        git(&wine, &["commit", "-qm", "one"]);
        (Scratch(dir.clone()), dir, wine)
    }

    /// Write one rr-cache entry; ages the preimage `days` back via file times
    /// -- the same mtime clock the planner reads.
    fn entry(root: &Path, id: &str, files: &[(&str, &str)], days_old: u64) {
        let d = root.join(id);
        fs::create_dir_all(&d).unwrap();
        for (name, content) in files {
            fs::write(d.join(name), content).unwrap();
        }
        if days_old > 0 {
            let t = SystemTime::now() - Duration::from_secs(days_old * 86_400);
            let f = File::options().write(true).open(d.join("preimage")).unwrap();
            f.set_times(FileTimes::new().set_modified(t)).unwrap();
        }
    }

    fn manifest(unscoped_configure_rule: bool) -> Manifest {
        let rule = if unscoped_configure_rule {
            "\n[[binary_resolution]]\npath = \"configure\"\nstrategy = \"delete\"\nreason = \"r\"\n"
        } else {
            ""
        };
        Manifest::parse(&format!(
            r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "abc"
describes = "d"
derivation = "x"
{rule}"#
        ))
        .unwrap()
    }

    /// The whole matrix in one cache: fresh-unresolved kept, old-unresolved
    /// pruned, shadowed pruned at ANY age, resolved kept, stray thisimage
    /// removed with its entry kept -- and the prune commits, sweep-proof.
    #[test]
    fn the_pruning_matrix() {
        let (_s, sup, wine) = scratch("matrix");
        let cache = sup.join("tools/rr-cache");
        entry(&cache, "aaaa-fresh-unresolved", &[("preimage", "p")], 1);
        entry(&cache, "bbbb-old-unresolved", &[("preimage", "p")], 45);
        entry(
            &cache,
            "cccc-shadowed",
            &[("preimage", "#! /bin/sh\n# Generated by GNU Autoconf 2.72 for Wine 11.12.\n")],
            1, // fresh on purpose: shadowing must not care about age
        );
        entry(&cache, "dddd-resolved", &[("preimage", "p"), ("postimage", "q")], 90);
        entry(
            &cache,
            "eeee-stray",
            &[("preimage", "p"), ("postimage", "q"), ("thisimage", "t")],
            90,
        );
        git(&sup, &["add", "-A"]);
        git(&sup, &["commit", "-qm", "seed cache"]);
        // The live cache carries a copy of the old entry: pruning must reach
        // it too, or the next regen persist re-imports the corpse.
        let live = GitCli::new(&wine).git_dir().unwrap().join("rr-cache");
        entry(&live, "bbbb-old-unresolved", &[("preimage", "p")], 45);

        let sup_cli = GitCli::new(&sup);
        let wine_cli = GitCli::new(&wine);
        let m = manifest(true);
        let p = Policy::from_manifest(&m);
        let gp = plan_rrgc(&m, &p, &sup_cli, &wine_cli, &sup, 30).unwrap();

        let mut pruned: Vec<&str> = gp.prune.iter().map(|x| x.id.as_str()).collect();
        pruned.sort_unstable();
        assert_eq!(pruned, vec!["bbbb-old-unresolved", "cccc-shadowed"]);
        assert!(matches!(
            gp.prune.iter().find(|x| x.id.starts_with("bbbb")).unwrap().reason,
            PruneReason::NoPostimage { age_days: 45 }
        ));
        assert!(matches!(
            &gp.prune.iter().find(|x| x.id.starts_with("cccc")).unwrap().reason,
            PruneReason::Shadowed { path } if path == "configure"
        ));
        assert_eq!(gp.strays, vec!["eeee-stray"]);

        // Apply: a stranger's staged file proves the commit is scoped.
        fs::write(sup.join("stranger.txt"), "s\n").unwrap();
        git(&sup, &["add", "stranger.txt"]);
        let wine_mut = GitMut::new(&wine);
        let sup_mut = GitMut::new(&sup);
        let ctx = ApplyCtx {
            wine: &wine_mut,
            superproject: &sup_mut,
            wine_read: &wine_cli,
            superproject_read: &sup_cli,
            wine_path: "wine".into(),
        };
        let out = apply_rrgc(&gp, &ctx).unwrap();
        assert_eq!((out.pruned, out.strays_removed), (2, 1));
        assert!(out.committed.is_some());

        assert!(!cache.join("bbbb-old-unresolved").exists());
        assert!(!cache.join("cccc-shadowed").exists());
        assert!(!live.join("bbbb-old-unresolved").exists(), "pruned from BOTH caches");
        assert!(cache.join("aaaa-fresh-unresolved").is_dir(), "fresh stays for its postimage");
        assert!(cache.join("dddd-resolved").is_dir());
        assert!(cache.join("eeee-stray").is_dir() && !cache.join("eeee-stray/thisimage").exists());

        // The commit exists, names the count, and left the stranger staged.
        let subj = sup_cli.log_subjects("HEAD~1..HEAD").unwrap();
        assert!(subj[0].1.contains("prune 2 entries"), "{}", subj[0].1);
        assert!(sup_cli.rev_parse_raw("HEAD:stranger.txt").unwrap().is_none());
        assert!(sup_cli
            .status(false)
            .unwrap()
            .iter()
            .any(|e| e.path == "stranger.txt" && e.xy.starts_with('A')));
    }

    /// A CAS refusal must leave the cache files untouched: the prunes are the
    /// irreversible half, so they run as the engine's prelude AFTER the
    /// `expected_before` pre-verify -- otherwise "refused ... nothing was
    /// changed" prints over already-deleted entries (the live-gitdir copy of
    /// which nothing can restore).
    #[test]
    fn a_refused_apply_leaves_the_cache_untouched() {
        let (_s, sup, wine) = scratch("refused");
        let cache = sup.join("tools/rr-cache");
        entry(&cache, "bbbb-old-unresolved", &[("preimage", "p")], 45);
        git(&sup, &["add", "-A"]);
        git(&sup, &["commit", "-qm", "seed cache"]);
        let live = GitCli::new(&wine).git_dir().unwrap().join("rr-cache");
        entry(&live, "bbbb-old-unresolved", &[("preimage", "p")], 45);

        let sup_cli = GitCli::new(&sup);
        let wine_cli = GitCli::new(&wine);
        let m = manifest(false);
        let p = Policy::from_manifest(&m);
        let gp = plan_rrgc(&m, &p, &sup_cli, &wine_cli, &sup, 30).unwrap();
        assert_eq!(gp.prune.len(), 1);

        // The superproject HEAD moves between plan and apply -- the exact
        // probe the reviewer ran against the pre-fix ordering.
        fs::write(sup.join("mover.txt"), "m\n").unwrap();
        git(&sup, &["add", "mover.txt"]);
        git(&sup, &["commit", "-qm", "someone else commits"]);

        let wine_mut = GitMut::new(&wine);
        let sup_mut = GitMut::new(&sup);
        let ctx = ApplyCtx {
            wine: &wine_mut,
            superproject: &sup_mut,
            wine_read: &wine_cli,
            superproject_read: &sup_cli,
            wine_path: "wine".into(),
        };
        let err = apply_rrgc(&gp, &ctx).unwrap_err();
        assert!(err.to_string().starts_with("refused: "), "{err}");
        assert!(
            cache.join("bbbb-old-unresolved").is_dir(),
            "the tracked entry must survive a refusal"
        );
        assert!(
            live.join("bbbb-old-unresolved").is_dir(),
            "the live entry (unrecoverable if deleted) must survive a refusal"
        );
    }

    /// Nothing to prune: the honest near-no-op -- rerere gc still runs, no
    /// commit is planned, and nothing under the superproject moves.
    #[test]
    fn a_clean_cache_plans_no_commit() {
        let (_s, sup, wine) = scratch("clean");
        let cache = sup.join("tools/rr-cache");
        entry(&cache, "aaaa-resolved", &[("preimage", "p"), ("postimage", "q")], 90);
        git(&sup, &["add", "-A"]);
        git(&sup, &["commit", "-qm", "seed"]);

        let sup_cli = GitCli::new(&sup);
        let wine_cli = GitCli::new(&wine);
        let m = manifest(false);
        let p = Policy::from_manifest(&m);
        let gp = plan_rrgc(&m, &p, &sup_cli, &wine_cli, &sup, 30).unwrap();
        assert!(gp.prune.is_empty() && gp.strays.is_empty());
        assert_eq!(gp.plan.steps.len(), 1, "only the rerere gc step");
        assert!(gp.plan.mutations.is_empty());

        let head = sup_cli.rev_parse("HEAD").unwrap();
        let wine_mut = GitMut::new(&wine);
        let sup_mut = GitMut::new(&sup);
        let ctx = ApplyCtx {
            wine: &wine_mut,
            superproject: &sup_mut,
            wine_read: &wine_cli,
            superproject_read: &sup_cli,
            wine_path: "wine".into(),
        };
        let out = apply_rrgc(&gp, &ctx).unwrap();
        assert_eq!(out.committed, None);
        assert_eq!(sup_cli.rev_parse("HEAD").unwrap(), head, "no commit happened");
        assert!(cache.join("aaaa-resolved").is_dir());
    }
}
