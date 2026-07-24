//! `regen` -- re-pour a consumer: replay the soda tiers onto the wine base.
//!
//! This is `tools/regenerate.sh` grown up, mirroring its mechanics exactly
//! (rerere enabled + autoupdate, the tracked `tools/rr-cache` seeded into the
//! live gitdir cache before the replay and persisted back after -- even on a
//! stop, so partial progress is shared; `checkout -B <consumer> <soda_tip>`
//! then `rebase --onto <base> <home_base> <consumer>`, never rebasing the soda
//! ref itself) and improving it exactly where the plan doc says:
//!
//! 1. the base's remote is FETCHED first, so the pour lands on the base as it
//!    is, not as this clone last saw it -- which is also why the rebase step
//!    names the base by REF, not by a pinned oid: an oid pinned before the
//!    fetch would silently undo the fetch's whole point;
//! 2. conflicts are driven by a loop instead of `exit 1`: a conflicted path
//!    covered by a `[[binary_resolution]]` rule (respecting its `consumers`
//!    scope) is resolved non-interactively and the rebase continued. Rule
//!    COVERAGE decides application, not binary-ness -- binary content is the
//!    motivating class (rerere can never learn it), but the live `configure`
//!    rule resolves a TEXT modify/delete, and gating on the classifier would
//!    quietly stop exactly the pour the rule was declared for. Classification
//!    (`GitCli::conflict_is_binary`) decides only which GUIDANCE an UNCOVERED
//!    stop prints;
//! 3. on success the rr-cache is not merely persisted but COMMITTED in the
//!    superproject (rr-cache paths only, sweep-proof via `commit -- <path>`),
//!    and the pour is RECORDED: a `[ledger.<consumer>]` entry written into the
//!    manifest (comment-preserving) plus an annotated provenance tag
//!    `gen/<consumer>/<utc>` carrying the same record as JSON -- belt and
//!    braces, so neither the file nor the tag is the sole authority.
//!
//! A conflicted stop the rules do not cover is a legitimate TERMINAL state
//! (`RegenStop`), never a prompt: the guidance prints the exact commands and
//! the tool exits. `stitch` shares this stance and points here for the rules.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::manifest::{self, Generation, Manifest};
use crate::plan::{
    self, Action, ApplyCtx, ApplyError, Plan, PlannedValue, RefMutation, RepoSide, Step,
};
use crate::treestate;
use anyhow::Context as _;
use sodactl_git::{GitCli, GitRead, Side, StatusKind};

/// The reviewable pour: the `Plan` plus the derivation facts.
#[derive(Debug, serde::Serialize)]
pub struct RegenPlan {
    pub plan: Plan,
    pub consumer: String,
    /// The declared base REF (what the rebase step names -- see the module
    /// header for why it is not pinned to an oid).
    pub base: String,
    /// What the base resolves to at plan time; `None` when it only exists
    /// after the fetch step runs.
    pub base_tip: Option<String>,
    pub soda_tip_ref: String,
    /// The soda tip PINNED at plan time -- it is local, no fetch moves it, and
    /// pinning it is what the plan id's drift refusal covers.
    pub soda_tip: String,
    pub home_base: String,
    /// Commits in `home_base..soda_tip` -- what the replay will attempt.
    pub replay_count: u32,
    /// The `[[binary_resolution]]` rules in scope for this consumer, as
    /// `(path, strategy)` -- the conflict loop's whole authority.
    pub rules: Vec<(String, String)>,
    pub backup: Option<(String, String)>,
}

/// Everything `apply_regen` needs beyond the shared `ApplyCtx`. A struct so
/// the CLI call site names what it is wiring instead of passing five
/// positional paths.
pub struct RegenDeps<'a> {
    /// Binary-backed handle for the conflict classifier
    /// (`conflict_is_binary` is inherent on `GitCli`).
    pub wine_cli: &'a GitCli,
    /// The manifest FILE, for the ledger write-back.
    pub manifest_path: &'a Path,
    /// The tracked rr-cache directory (absolute).
    pub tracked_cache: PathBuf,
    /// The same directory relative to the superproject root
    /// (`policy.rr_cache`) -- the commit pathspec.
    pub rr_cache_rel: &'a str,
}

/// A pour that stopped mid-replay in a state only a human (or a manifest
/// edit) can advance. Not `refused: ` -- the tree IS mid-rebase, and saying
/// otherwise would send the next command into the sequencer's arms. Partial
/// rerere progress has been persisted to the tracked cache by the time this
/// error exists, exactly like the script's stop path.
#[derive(Debug)]
pub enum RegenStop {
    /// Text conflicts rerere did not know and no rule covers.
    Text {
        consumer: String,
        wine_path: String,
        paths: Vec<String>,
        journal: PathBuf,
    },
    /// A binary conflict no rule covers: rerere is structurally unable to
    /// ever learn it, so the only durable fix is a declared rule.
    UncoveredBinary {
        consumer: String,
        wine_path: String,
        path: String,
        journal: PathBuf,
    },
    /// The replay is in flight, nothing is left to resolve, and `rebase
    /// --continue` still refuses (e.g. a rule emptied a pick and git wants
    /// `--skip`) -- or the loop stopped converging. The detail is git's own
    /// message; deciding to skip a commit is a human call, not a loop's.
    NoProgress {
        consumer: String,
        wine_path: String,
        detail: String,
        journal: PathBuf,
    },
}

impl fmt::Display for RegenStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The plan doc's "Conflict handling" text block, verbatim in
            // shape: paths, then the exact next commands, then stop.
            RegenStop::Text { consumer, wine_path, paths, journal } => write!(
                f,
                "regen stopped: {} text conflict(s) rerere did not know ({}). Resolve, then:\n\
                 \x20 git -C {wp} add {}\n\
                 \x20 git -C {wp} rebase --continue\n\
                 \x20 sodactl regen {consumer} --apply   # re-run to capture the resolution as an rr-cache postimage\n\
                 (partial rerere progress was persisted to the tracked cache; to abandon: `git -C {wp} rebase --abort`)\n\
                 \x20 journal: {}",
                paths.len(),
                paths.join(", "),
                paths.join(" "),
                journal.display(),
                wp = wine_path,
            ),
            RegenStop::UncoveredBinary { consumer, wine_path, path, journal } => write!(
                f,
                "regen stopped: binary conflict in `{path}` and no [[binary_resolution]] rule covers it for \
                 `{consumer}`. rerere is line-based and can never learn this -- it will re-prompt on every \
                 replay until a rule decides it. Inspect the two sides:\n\
                 \x20 git -C {wp} show :2:{path}   # the wine base's blob (strategy \"base\")\n\
                 \x20 git -C {wp} show :3:{path}   # the tier's blob (strategy \"ours\")\n\
                 Then either resolve by hand and `git -C {wp} rebase --continue`, or declare the decision -- \
                 appending a rule is a MANUAL manifest edit, add to sodactl.toml:\n\
                 \n\
                 [[binary_resolution]]\n\
                 path = \"{path}\"\n\
                 strategy = \"ours\"   # \"ours\" = the tier's blob wins; \"base\" = the wine base's; \"delete\"\n\
                 reason = \"<why this is the right call>\"\n\
                 consumers = [\"{consumer}\"]   # drop this line to apply to every consumer\n\
                 \n\
                 and re-run `sodactl regen {consumer} --apply`. (To abandon: `git -C {wp} rebase --abort`)\n\
                 \x20 journal: {}",
                journal.display(),
                wp = wine_path,
            ),
            RegenStop::NoProgress { consumer, wine_path, detail, journal } => write!(
                f,
                "regen stopped: the replay is in flight with nothing left to resolve, and `rebase --continue` \
                 still refuses: {detail}\n\
                 Decide by hand (`git -C {wp} rebase --skip` drops the current pick; `git -C {wp} rebase \
                 --abort` abandons), then re-run `sodactl regen {consumer} --apply`.\n\
                 \x20 journal: {}",
                journal.display(),
                wp = wine_path,
            ),
        }
    }
}

impl std::error::Error for RegenStop {}

/// A pour-time precondition failure. Typed, `refused: `-prefixed,
/// downcastable -- exit 1, nothing was changed (checked before even the
/// rr-cache seed copy).
#[derive(Debug)]
pub enum RegenRefusal {
    /// The post-pour bookkeeping commits in the superproject (the rr-cache
    /// capture); on a detached HEAD that commit would dangle -- the same
    /// refusal `rr gc` applies to the same kind of commit, applied here at
    /// apply time because only `apply_regen` holds the superproject handle.
    DetachedSuperproject,
}

impl fmt::Display for RegenRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegenRefusal::DetachedSuperproject => write!(
                f,
                "refused: the superproject HEAD is detached -- the pour's rr-cache capture commit \
                 would dangle; check a branch out first"
            ),
        }
    }
}

impl std::error::Error for RegenRefusal {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Plan the pour of one consumer.
pub fn plan_regen<G: GitRead>(m: &Manifest, git: &G, name: &str) -> anyhow::Result<RegenPlan> {
    let c = m
        .consumer
        .iter()
        .find(|c| c.name == name)
        .with_context(|| format!("no [[consumer]] named `{name}` in the manifest"))?;

    let home = m.repo.home_base.commit.clone();
    let soda_tip = git
        .rev_parse(&c.soda_tip)?
        .with_context(|| format!("`{name}`'s soda tip `{}` does not resolve", c.soda_tip))?;
    // Trap 1 from BRANCHING.md, encoded: the replayed range starts at the
    // manifest's home base, and the tip must actually carry it -- otherwise
    // the range is not "the soda tiers" but an arbitrary slice of history.
    if !git.is_ancestor(&home, &soda_tip)? {
        anyhow::bail!(
            "`{}` ({}) does not descend from the home base {} -- the replay range \
             `{}..{}` would not be the soda tiers; fix the manifest or the branch",
            c.soda_tip,
            short(&soda_tip),
            short(&home),
            short(&home),
            c.soda_tip
        );
    }
    let replay_count = git.count_commits(&format!("{home}..{soda_tip}"))?;

    // The fetch step: derivable only when the base's leading path component
    // names a declared remote (`valve-wine/proton_11.0`). A tag base
    // (`wine-11.13`) names no remote, and guessing one would fetch the wrong
    // repository with a straight face.
    let fetch_remote = c
        .base
        .split_once('/')
        .map(|(r, _)| r)
        .filter(|r| m.remotes.contains_key(*r))
        .map(str::to_string);
    let base_tip = git.rev_parse(&c.base)?;
    if base_tip.is_none() && fetch_remote.is_none() {
        anyhow::bail!(
            "`{name}`'s base `{}` does not resolve and names no declared remote to fetch from -- \
             the base may be a TAG: `git -C wine fetch <remote> --tags`, then re-run",
            c.base
        );
    }

    let existing = git.rev_parse_raw(&format!("refs/heads/{name}"))?;

    let mut steps = Vec::new();
    if let Some(r) = &fetch_remote {
        steps.push(Step::new(
            format!("fetch `{r}` so the pour lands on the base as it IS"),
            RepoSide::Wine,
            Action::Fetch { remote: r.clone(), refspecs: vec![] },
        ));
    }
    // regenerate.sh step 1, verbatim intent: rerere on, replayed resolutions
    // staged automatically. Repo-local, idempotent.
    for (k, v) in [("rerere.enabled", "true"), ("rerere.autoupdate", "true")] {
        steps.push(Step::new(
            format!("enable {k}"),
            RepoSide::Wine,
            Action::ConfigSet { key: k.into(), value: v.into() },
        ));
    }
    steps.push(Step::new(
        format!("point `{name}` at the soda tip `{}` ({})", c.soda_tip, short(&soda_tip)),
        RepoSide::Wine,
        // The pinned oid, like stitch: the tip is local and the id then
        // covers exactly what replays.
        Action::CheckoutBranchReset { branch: name.to_string(), start: soda_tip.clone() },
    ));
    steps.push(Step::new(
        format!(
            "rebase `{name}`: replay {replay_count} commit(s) ({}..{}) onto `{}`",
            short(&home),
            c.soda_tip,
            c.base
        ),
        RepoSide::Wine,
        Action::RebaseOnto {
            // By REF, deliberately (module header): the fetch step above may
            // move it, and that movement is the improvement.
            newbase: c.base.clone(),
            upstream: home.clone(),
            branch: name.to_string(),
        },
    ));

    let mutations = vec![RefMutation {
        repo: RepoSide::Wine,
        refname: format!("refs/heads/{name}"),
        expected_before: existing.clone(),
        after: PlannedValue::Unknown {
            reason: "a cross-base replay mints new commits; the tip is unknowable until it runs"
                .into(),
        },
    }];
    let backup = existing
        .as_ref()
        .map(|tip| (plan::backup_refname(&plan::backup_stamp(), name), tip.clone()));

    let rules: Vec<(String, String)> = m
        .binary_resolution
        .iter()
        .filter(|b| b.consumers.is_empty() || b.consumers.iter().any(|c| c == name))
        .map(|b| (b.path.clone(), b.strategy.clone()))
        .collect();

    let mut notes = Vec::new();
    notes.push("the tracked rr-cache is seeded into the live cache before the replay and persisted back after (even on a stop)".into());
    match &base_tip {
        Some(b) => notes.push(format!("base `{}` currently resolves to {}", c.base, short(b))),
        None => notes.push(format!(
            "base `{}` does not resolve YET -- the fetch step is what makes it exist here",
            c.base
        )),
    }
    if fetch_remote.is_none() {
        notes.push(format!(
            "no fetch step: base `{}` names no declared remote -- if it can be stale here, fetch by hand first",
            c.base
        ));
    }
    if existing.is_none() {
        notes.push(format!("`{name}` does not exist yet -- this pour creates it"));
    }
    for (p, s) in &rules {
        notes.push(format!("conflict rule in scope: `{p}` -> strategy \"{s}\""));
    }
    notes.push(format!(
        "on success: rr-cache persisted + committed (tracked-cache paths only), [ledger.{name}] \
         written into the manifest, provenance tag gen/{name}/<utc> cut on the result -- \
         bookkeeping, not steps: its contents (the poured tip) are unknowable until the replay ran"
    ));
    notes.push(
        "the ledger edit to the manifest stays UNCOMMITTED for review -- a fresh clone reads \
         CONSUMER_CURRENCY_UNKNOWN until it lands; commit it with the pin bump or on its own"
            .to_string(),
    );
    notes.push(format!("the checkout moves to `{name}` (checkout -B) and stays there"));

    let plan = Plan::new("regen", steps, mutations, backup.clone().into_iter().collect(), notes);
    Ok(RegenPlan {
        plan,
        consumer: name.to_string(),
        base: c.base.clone(),
        base_tip,
        soda_tip_ref: c.soda_tip.clone(),
        soda_tip,
        home_base: home,
        replay_count,
        rules,
        backup,
    })
}

#[derive(Debug)]
pub struct RegenOutcome {
    /// The poured tip that passed the postcondition.
    pub new_tip: String,
    pub journal: PathBuf,
    /// `(path, strategy)` per conflict a rule resolved.
    pub auto_resolved: Vec<(String, String)>,
    /// The superproject commit capturing new rr-cache resolutions, when any.
    pub rr_commit: Option<String>,
    pub ledger: Generation,
    pub gen_tag: String,
}

/// Run one regen plan: seed rerere, run the engine, drive the conflict loop,
/// then record the pour (persist+commit rr-cache, ledger, provenance tag).
pub fn apply_regen(
    rp: &RegenPlan,
    ctx: &ApplyCtx,
    deps: &RegenDeps,
) -> anyhow::Result<RegenOutcome> {
    // Before anything -- even the seed copy: the success path commits the
    // rr-cache capture in the superproject, and a detached HEAD would leave
    // that commit dangling (rr gc's refusal, at this verb's earliest gate).
    if ctx.superproject_read.head_branch()?.is_none() {
        return Err(anyhow::Error::new(RegenRefusal::DetachedSuperproject));
    }

    let gitdir = ctx.wine_read.git_dir()?;
    let live_cache = gitdir.join("rr-cache");

    // Seed BEFORE anything runs (regenerate.sh's `cp -a tracked/. live/`):
    // this is what lets a fresh clone replay every stitch resolution already
    // made. A filesystem copy, not a git action -- it cannot appear as a plan
    // step, so the plan notes carry it instead.
    copy_tree(&deps.tracked_cache, &live_cache)
        .with_context(|| "seeding the live rr-cache from the tracked cache")?;

    let (journal, conflicted) = match plan::apply(&rp.plan, ctx) {
        Ok(o) => (o.journal, false),
        Err(ApplyError::Step { journal, detail, .. }) => {
            // Either the rebase stopped on a conflict (the expected, driveable
            // case) or some earlier step genuinely failed. The distinction is
            // structural -- the gitdir, never the message.
            if !treestate::rebase_in_flight(&gitdir) {
                anyhow::bail!("regen step failed with nothing in flight: {detail}");
            }
            (journal, true)
        }
        Err(e) => return Err(anyhow::Error::new(e)),
    };

    // The conflict loop. On a terminal stop, persist partial rerere progress
    // first -- the script's `persist_cache` on the failure path, kept.
    let auto_resolved = match drive_conflicts(rp, ctx, deps, &gitdir, &journal) {
        Ok(v) => v,
        Err(e) => {
            let _ = copy_tree(&live_cache, &deps.tracked_cache);
            return Err(e);
        }
    };
    if conflicted {
        // Git leaves the finished conflict's rerere metadata behind: a
        // `MERGE_RR` (non-empty whenever a rule resolved a recorded conflict,
        // since no postimage was ever captured) plus those dead preimage-only
        // recordings. `rerere clear` removes exactly that set and nothing
        // rerere actually learned (see `GitMut::rerere_clear`); without it
        // every rule-resolving pour bequeaths a wedged preflight and a fresh
        // RR_NO_POSTIMAGE to the next run.
        ctx.wine.rerere_clear()?;
    }

    let refname = format!("refs/heads/{}", rp.consumer);
    let new_tip = ctx
        .wine_read
        .rev_parse_raw(&refname)?
        .with_context(|| format!("{refname} vanished after the pour"))?;

    // The postcondition is CONSUMER_NOT_ON_BASE's question, asked positively:
    // the result must descend from the declared base (resolved NOW, i.e.
    // post-fetch). Ancestry is sound here -- the base genuinely is an
    // ancestor of a `rebase --onto` result even though the replayed commits
    // are nobody's. No auto-restore: a completed `rebase --onto <base>`
    // cannot produce this, so firing means the world moved mid-pour and a
    // blind restore could fight whoever moved it; the backup is named.
    let base_now = ctx
        .wine_read
        .rev_parse(&rp.base)?
        .with_context(|| format!("base `{}` does not resolve after the pour", rp.base))?;
    if !ctx.wine_read.is_ancestor(&base_now, &rp.consumer)? {
        anyhow::bail!(
            "regen postcondition FAILED: `{}` ({}) does not descend from its base `{}` ({}){} -- \
             inspect before trusting the branch",
            rp.consumer,
            short(&new_tip),
            rp.base,
            short(&base_now),
            match &rp.backup {
                Some((r, t)) => format!("; the previous tip survives at {r} ({})", short(t)),
                None => String::new(),
            }
        );
    }

    // Persist (success path) and COMMIT the capture -- the improvement over
    // the script's warning. `commit -- <path>` keeps a stranger's staged work
    // out; see `GitMut::commit_paths`.
    copy_tree(&live_cache, &deps.tracked_cache)
        .with_context(|| "persisting the live rr-cache back to the tracked cache")?;
    // "Anything to commit?" is asked of the INDEX after an `add`, never of the
    // status listing: an untracked parent directory collapses to one `?
    // tools/` entry (measured on the fixture), so a path filter over status
    // reads a brand-new cache as clean and silently skips the capture.
    let rr_commit = if deps.tracked_cache.is_dir() {
        ctx.superproject.add(&[deps.rr_cache_rel])?;
        let staged = ctx
            .superproject_read
            .status(false)?
            .into_iter()
            .any(|e| {
                e.path.starts_with(deps.rr_cache_rel)
                    && e.xy.chars().next().is_some_and(|c| c != '.')
            });
        if staged {
            ctx.superproject.commit_paths(
                &format!("sodactl: capture rerere resolutions from {} regen", rp.consumer),
                &[deps.rr_cache_rel],
            )?;
            ctx.superproject_read.rev_parse("HEAD")?
        } else {
            None
        }
    } else {
        None
    };

    // Record the pour: ledger entry + provenance tag, one instant, same
    // content (belt and braces -- if they ever disagree, LEDGER_CONTRADICTS_TAG
    // trusts neither).
    let now = SystemTime::now();
    let ledger = Generation {
        at: plan::utc_stamp_iso_at(now),
        base_commit: base_now,
        soda_commit: rp.soda_tip.clone(),
        home_base: rp.home_base.clone(),
        result: new_tip.clone(),
        sodactl: env!("CARGO_PKG_VERSION").to_string(),
    };
    manifest::write_ledger_entry(deps.manifest_path, &rp.consumer, &ledger)?;
    let gen_tag = format!("gen/{}/{}", rp.consumer, plan::utc_stamp_at(now));
    let json = serde_json::json!({
        "consumer": rp.consumer,
        "at": ledger.at,
        "base_commit": ledger.base_commit,
        "soda_commit": ledger.soda_commit,
        "home_base": ledger.home_base,
        "result": ledger.result,
        "sodactl": ledger.sodactl,
    });
    ctx.wine.tag_annotated(&gen_tag, &new_tip, &serde_json::to_string_pretty(&json)?, false)?;

    Ok(RegenOutcome { new_tip, journal, auto_resolved, rr_commit, ledger, gen_tag })
}

/// Drive a conflicted rebase to completion or to a typed terminal stop.
///
/// Structure per iteration, all decisions structural (gitdir + index), never
/// stderr prose:
/// * no rebase in flight (`rebase_in_flight`, the NARROW probe -- see its doc
///   for why the broad one outlives a completed rebase) -> finished;
/// * in flight, nothing unmerged -> `rebase --continue` (rerere.autoupdate
///   staged a known resolution; the stop itself is how rebase behaves) -- if
///   even that refuses, `NoProgress`;
/// * any unmerged path NOT covered by a rule -> terminal, guidance chosen by
///   the classifier (binary -> the rule offer; otherwise the resolve block);
/// * every unmerged path covered -> apply each strategy, continue, loop.
///
/// The iteration cap is a convergence guard, not a timeout: every productive
/// iteration either finishes a pick or stops, so `2*picks + 8` iterations
/// without terminating means the loop is walking in place.
fn drive_conflicts(
    rp: &RegenPlan,
    ctx: &ApplyCtx,
    deps: &RegenDeps,
    gitdir: &Path,
    journal: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut auto = Vec::new();
    let cap = rp.replay_count as usize * 2 + 8;
    for _ in 0..cap {
        if !treestate::rebase_in_flight(gitdir) {
            return Ok(auto);
        }
        let unmerged: Vec<String> = ctx
            .wine_read
            .status(false)?
            .into_iter()
            .filter(|e| e.kind == StatusKind::Unmerged)
            .map(|e| e.path)
            .collect();

        if unmerged.is_empty() {
            if let Err(e) = ctx.wine.rebase_continue() {
                // Distinguish "stopped at the NEXT conflict" (loop) from
                // "refused with nothing to resolve" (terminal): after the
                // continue, either new unmerged entries exist or they do not.
                let still_stuck = ctx
                    .wine_read
                    .status(false)?
                    .iter()
                    .all(|s| s.kind != StatusKind::Unmerged);
                if still_stuck && treestate::rebase_in_flight(gitdir) {
                    return Err(anyhow::Error::new(RegenStop::NoProgress {
                        consumer: rp.consumer.clone(),
                        wine_path: ctx.wine_path.clone(),
                        detail: e.to_string(),
                        journal: journal.to_path_buf(),
                    }));
                }
            }
            continue;
        }

        // Coverage first (module header: coverage decides application).
        let rule_for =
            |p: &str| rp.rules.iter().find(|(path, _)| path.as_str() == p).map(|(_, s)| s.clone());
        let mut covered: Vec<(String, String)> = Vec::new();
        let mut uncovered: Vec<String> = Vec::new();
        for p in unmerged {
            match rule_for(&p) {
                Some(s) => covered.push((p, s)),
                None => uncovered.push(p),
            }
        }
        if !uncovered.is_empty() {
            for p in &uncovered {
                if deps.wine_cli.conflict_is_binary(p)? {
                    return Err(anyhow::Error::new(RegenStop::UncoveredBinary {
                        consumer: rp.consumer.clone(),
                        wine_path: ctx.wine_path.clone(),
                        path: p.clone(),
                        journal: journal.to_path_buf(),
                    }));
                }
            }
            return Err(anyhow::Error::new(RegenStop::Text {
                consumer: rp.consumer.clone(),
                wine_path: ctx.wine_path.clone(),
                paths: uncovered,
                journal: journal.to_path_buf(),
            }));
        }
        for (p, strategy) in covered {
            match strategy.as_str() {
                // The manifest speaks in TIER terms; git speaks in rebase
                // terms, and during `rebase --onto <base> ...` they are
                // INVERTED: HEAD is the wine base being rebased onto, so
                // git's --ours is the BASE and --theirs is the replayed tier
                // commit. Transposing this silently ships the wrong blob,
                // which is why the mapping lives in exactly one place.
                "ours" => ctx.wine.resolve_take(&p, Side::Theirs)?,
                "base" => ctx.wine.resolve_take(&p, Side::Ours)?,
                "delete" => ctx.wine.rm_cached_conflict(&p)?,
                other => {
                    return Err(anyhow::anyhow!(
                        "[[binary_resolution]] for `{p}` declares unknown strategy `{other}` \
                         (known: ours | base | delete) -- fix the manifest. The replay is left in \
                         flight; resolve by hand or `git -C {} rebase --abort`",
                        ctx.wine_path
                    ))
                }
            }
            auto.push((p, strategy));
        }
        // An Err here is the ordinary "stopped at the next conflict"; the
        // next iteration classifies whatever it stopped on.
        let _ = ctx.wine.rebase_continue();
    }
    Err(anyhow::Error::new(RegenStop::NoProgress {
        consumer: rp.consumer.clone(),
        wine_path: ctx.wine_path.clone(),
        detail: format!("the conflict loop did not converge within {cap} iterations"),
        journal: journal.to_path_buf(),
    }))
}

/// `cp -a src/. dst/` in std terms: recursive, creating `dst`, overwriting
/// files. A missing `src` is a no-op -- a repo that never recorded a
/// resolution has nothing to seed, and that is not an error.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}
