//! CLI driving of the mutating verbs: rendering, confirmation, exit codes.
//!
//! Core plans and applies; this module is the only place that prints or asks.
//! The contract every verb arm follows:
//!
//! * DEFAULT is plan-only -- print and exit 0, nothing changed.
//! * `--apply` re-derives the plan and compares ids before executing, so what
//!   runs is provably what was reviewed (or a refusal showing both ids).
//! * The destructive subset (restack, undo, tag --force, pin, rr-gc)
//!   additionally asks `y/N` on a tty -- one stdin line, no prompt crate --
//!   skippable with `--yes`.
//! * The build lanes (build/package/smoke) are the one documented exception
//!   to plan-default: they mutate no ref, so they exec directly with the
//!   child's exit code passed through; `--json` is their review form (see
//!   `lane_cmd`).

use anyhow::Result;
use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::plan::{self, ApplyCtx, ApplyError, Plan, PlannedValue, Refusal, RepoSide};
use sodactl_core::verbs::{
    buildwrap, pin, pushprint, regen, release, restack, resume, rrgc, stitch, tagverb, undo,
};
use sodactl_core::Manifest;
use sodactl_git::{GitCli, GitRead};
use std::io::{IsTerminal, Write};
use std::path::Path;

/// Exit code for a failed verb, per the documented split: refusals and
/// terminal states the USER must act on are the gate (1); "the tool could not
/// answer" stays 2. Typed downcasts first (anyhow walks the context chain);
/// the stable `refused: ` display prefix is honoured as a fallback so a
/// refusal minted as a plain message still gates.
pub fn exit_code_for(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<Refusal>().is_some()
        || e.downcast_ref::<restack::RestackRefusal>().is_some()
        || e.downcast_ref::<restack::PostconditionDrift>().is_some()
        || e.downcast_ref::<resume::ResumeRefusal>().is_some()
        || e.downcast_ref::<stitch::StitchPostconditionDrift>().is_some()
        || e.downcast_ref::<tagverb::TagRefusal>().is_some()
        || e.downcast_ref::<pin::PinRefusal>().is_some()
        // A regen stop is a terminal state the USER must act on (resolve /
        // declare a rule / decide a skip), same standing as a stopped step.
        || e.downcast_ref::<regen::RegenStop>().is_some()
        || e.downcast_ref::<regen::RegenRefusal>().is_some()
        || e.downcast_ref::<rrgc::RrGcRefusal>().is_some()
        // The build-lane pin gate: nothing ran, the user picks checkout-or-
        // --anyway. (The lane's own exit code, once it runs, bypasses this
        // mapping entirely -- see `lanes.rs`.)
        || e.downcast_ref::<buildwrap::LaneRefusal>().is_some()
    {
        return crate::EXIT_GATE;
    }
    match e.downcast_ref::<ApplyError>() {
        // A moved ref refuses with nothing changed; a stopped step is a
        // terminal state whose next move (resolve/continue/abort/undo) is in
        // the message. Both are the human's gate, not a broken tool.
        Some(ApplyError::RefusedExpectedMoved { .. } | ApplyError::Step { .. }) => crate::EXIT_GATE,
        Some(_) => crate::EXIT_ERROR,
        None if e.chain().any(|c| c.to_string().starts_with("refused: ")) => crate::EXIT_GATE,
        None => crate::EXIT_ERROR,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch<G: GitRead>(
    cmd: &crate::Cmd,
    m: &Manifest,
    ws: &Workspace,
    git: &G,
    wine_cli: &GitCli,
    super_cli: &GitCli,
    policy: &Policy,
    actx: &ApplyCtx,
    manifest_path: &Path,
    json: bool,
) -> Result<()> {
    match cmd {
        crate::Cmd::Restack { tier, all_descendants, apply, yes } => {
            restack_cmd(m, ws, git, actx, json, tier, *all_descendants, *apply, *yes)
        }
        crate::Cmd::Undo { backup, apply, yes } => {
            undo_cmd(ws, wine_cli, actx, json, backup.as_deref(), *apply, *yes)
        }
        crate::Cmd::Resume { apply } => resume_cmd(ws, actx, json, *apply),
        crate::Cmd::Stitch { name, apply } => stitch_cmd(m, ws, git, actx, json, name, *apply),
        crate::Cmd::Tag { family, n, force, apply, yes } => {
            tag_cmd(m, ws, git, policy, actx, json, family, *n, *force, *apply, *yes)
        }
        crate::Cmd::Pin { tag, offline, apply, yes } => {
            pin_cmd(m, ws, wine_cli, actx, json, tag, *offline, *apply, *yes)
        }
        crate::Cmd::Regen { consumer, apply } => {
            regen_cmd(m, ws, git, wine_cli, policy, actx, manifest_path, json, consumer, *apply)
        }
        crate::Cmd::RrGc { older_than, apply, yes } => {
            rrgc_cmd(m, ws, policy, actx, json, *older_than, *apply, *yes)
        }
        crate::Cmd::Push => push_cmd(m, policy, wine_cli, super_cli, &actx.wine_path, json),
        crate::Cmd::Release { family, n } => {
            release_cmd(m, policy, wine_cli, super_cli, &actx.wine_path, family, *n, json)
        }
        crate::Cmd::Build { anyway } => lane_cmd(buildwrap::plan_build(ws, *anyway), ws, json),
        crate::Cmd::Package { version } => lane_cmd(buildwrap::plan_package(ws, version), ws, json),
        crate::Cmd::Smoke => lane_cmd(buildwrap::plan_smoke(ws), ws, json),
        _ => unreachable!("dispatch called for a non-verb command"),
    }
}

#[allow(clippy::too_many_arguments)]
fn restack_cmd<G: GitRead>(
    m: &Manifest,
    ws: &Workspace,
    git: &G,
    actx: &ApplyCtx,
    json: bool,
    tier: &str,
    all: bool,
    apply: bool,
    yes: bool,
) -> Result<()> {
    plan::preflight(m, ws)?;
    let plans = restack::plan_restack(m, git, tier, all)?;
    if plans.is_empty() {
        println!("nothing to restack: `{tier}` already sits on its parent's tip.");
        return Ok(());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&plans)?);
    } else {
        for (i, rp) in plans.iter().enumerate() {
            if i > 0 {
                println!();
            }
            render_plan(&rp.plan);
        }
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    let names: Vec<&str> = plans.iter().map(|p| p.tier.as_str()).collect();
    confirm(&format!("restack rewrites {} branch(es): {}", names.len(), names.join(", ")), yes)?;

    // Re-derive before EVERY execution. The first plan must match the review
    // byte-for-byte (the id comparison -- the TOCTOU close-out). Later plans
    // in a batch CANNOT match their previews by construction: the preview was
    // derived against a parent tip the batch just moved, which is exactly why
    // they were marked provisional. They are re-shown before running instead
    // of id-compared -- the human confirmed the batch, and the engine still
    // verifies every `expected_before` and CASes every ref move regardless.
    let reviewed = plans[0].plan.id.clone();
    let cap = plans.len();
    let mut done = 0usize;
    loop {
        let cur = restack::plan_restack(m, git, tier, all)?;
        let Some(rp) = cur.first() else { break };
        if done == 0 {
            if rp.plan.id != reviewed {
                anyhow::bail!(
                    "refused: the plan drifted between review and apply (reviewed id {reviewed}, \
                     live id {}) -- state moved underneath; re-run to review the fresh plan",
                    rp.plan.id
                );
            }
        } else {
            println!("\nre-derived after the previous restack:");
            render_plan(&rp.plan);
        }
        let out = restack::apply_restack(rp, actx)?;
        done += 1;
        println!(
            "restacked `{}` -> {} -- postcondition ok ({} own commit(s), patch-ids unchanged)",
            rp.tier,
            short(&out.new_tip),
            rp.own.len()
        );
        println!("  backup: {} -> {}", rp.backup.0, rp.backup.1);
        println!("  journal: {}", out.outcome.journal.display());
        if !all {
            break;
        }
        if done > cap {
            anyhow::bail!(
                "restack did not converge after {cap} applied plan(s) -- stopping; run `sodactl status`"
            );
        }
    }
    Ok(())
}

fn undo_cmd(
    ws: &Workspace,
    wine_cli: &GitCli,
    actx: &ApplyCtx,
    json: bool,
    backup: Option<&str>,
    apply: bool,
    yes: bool,
) -> Result<()> {
    let Some(backup) = backup else {
        // No argument: list. Read-only, so no gate of any kind.
        let l = undo::list_backups(wine_cli)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&l)?);
            return Ok(());
        }
        if l.is_empty() {
            println!("no backups under refs/sodactl/backup/ -- nothing has been rewritten by sodactl yet.");
            return Ok(());
        }
        println!("BACKUPS  (newest first; restore with `sodactl undo <utc>/<branch> --apply`)");
        for b in &l {
            let state = match (&b.branch_tip, b.moved) {
                (None, _) => "branch no longer exists".to_string(),
                (Some(_), false) => "branch still at this tip".to_string(),
                (Some(t), true) => format!("branch since moved (now {})", short(t)),
            };
            println!("  {}  {:<16} {}  {}", b.utc, b.branch, short(&b.tip), state);
        }
        return Ok(());
    };

    // In-flight refusal only, deliberately NOT the full preflight: `undo`
    // moves a ref and never touches the worktree, so tracked dirt is not in
    // danger from it -- but restoring a branch while a replay is half-done
    // would fight the sequencer over the same ref.
    let gd = ws.wine.git_dir()?;
    if let Some(fl) = sodactl_core::treestate::in_flight(&gd) {
        return Err(anyhow::Error::new(Refusal::InFlight {
            markers: fl.markers,
            narrative: fl.narrative,
        }));
    }

    let p = undo::plan_undo(wine_cli, backup)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&p)?);
    } else {
        render_plan(&p);
    }
    if p.steps.is_empty() {
        // The honest no-op -- the plan's note already said why.
        return Ok(());
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    let mu = &p.mutations[0];
    confirm(
        &format!(
            "undo moves {} from {} back to {}",
            mu.refname,
            mu.expected_before.as_deref().map(short).unwrap_or_else(|| "<absent>".into()),
            match &mu.after {
                PlannedValue::Oid { oid } => short(oid),
                PlannedValue::Unknown { .. } => unreachable!("undo always plans a concrete oid"),
            }
        ),
        yes,
    )?;
    let p2 = undo::plan_undo(wine_cli, backup)?;
    if p2.id != p.id {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (reviewed id {}, live id {}) -- \
             state moved underneath; re-run to review the fresh plan",
            p.id,
            p2.id
        );
    }
    let out = plan::apply(&p2, actx).map_err(anyhow::Error::new)?;
    println!("restored.");
    println!("  journal: {}", out.journal.display());
    Ok(())
}

fn resume_cmd(ws: &Workspace, actx: &ApplyCtx, json: bool, apply: bool) -> Result<()> {
    let rep = resume::check_resume(ws.wine)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rep)?);
    } else {
        println!("in flight: {}{}", rep.markers.join(", "), rep.narrative);
        if let Some(r) = &rep.replaying {
            println!("  rewriting {r}");
        }
        if let Some(j) = &rep.journal {
            println!("  journal: {}", j.display());
        }
        for v in &rep.verified {
            println!("  verified: {v}");
        }
        for w in &rep.warnings {
            println!("  WARNING: {w}");
        }
        println!("  will run: git -C {} rebase --continue", actx.wine_path);
    }
    if !apply {
        println!(
            "\nplan only -- nothing was run. Resolve the conflicts, `git -C {} add` them, then \
             re-run with --apply.",
            actx.wine_path
        );
        return Ok(());
    }
    // Re-verify at execution time: the check is cheap and the answer can have
    // changed while the report sat on screen. (Deliberately no y/N here --
    // resume is not in the destructive subset; it finishes work the human
    // already started.)
    let rep = resume::check_resume(ws.wine)?;
    match actx.wine.rebase_continue() {
        Ok(()) => {
            println!("the replay continued to completion.");
            let subject = rep.replaying.as_deref().and_then(|r| r.strip_prefix("refs/heads/"));
            match (rep.journal_verb.as_deref(), subject) {
                (Some("restack"), Some(t)) => println!(
                    "this replay was a `restack` -- its postcondition never ran; re-run \
                     `sodactl restack {t}` to verify (a no-op when the tier is current)."
                ),
                // Finishing a pour through the generic resume skips ALL of
                // regen's recording: no ledger entry (currency stays
                // undecidable), no rr-cache persist+commit (every fresh clone
                // re-prompts on the conflict just resolved), no provenance
                // tag, no postcondition. Re-running regen replays cleanly --
                // rerere knows the resolution now -- and does the recording.
                (Some("regen"), Some(c)) => println!(
                    "this replay was a `regen` -- the pour's bookkeeping (rr-cache \
                     persist+commit, ledger entry, provenance tag, postcondition) never ran; \
                     re-run `sodactl regen {c} --apply` to record it (rerere replays the \
                     resolution you just made, so it drives straight through)."
                ),
                _ => {}
            }
            Ok(())
        }
        Err(e) => {
            // Another conflict, or unstaged resolutions: the same legitimate
            // terminal state a conflicted apply is. The CLI owns the exit.
            eprintln!("sodactl: the replay stopped again ({e})");
            eprintln!(
                "  resolve the remaining conflicts, `git -C {wp} add <paths>`, then re-run \
                 `sodactl resume --apply`; to abandon, `git -C {wp} rebase --abort`",
                wp = actx.wine_path
            );
            std::process::exit(crate::EXIT_GATE);
        }
    }
}

fn stitch_cmd<G: GitRead>(
    m: &Manifest,
    ws: &Workspace,
    git: &G,
    actx: &ApplyCtx,
    json: bool,
    name: &str,
    apply: bool,
) -> Result<()> {
    // Full preflight: `checkout -B` moves the worktree, so tracked dirt is in
    // exactly the same danger a rebase puts it in.
    plan::preflight(m, ws)?;
    let sp = stitch::plan_stitch(m, git, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&sp)?);
    } else {
        render_plan(&sp.plan);
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    // Re-derive + id-compare, no y/N: the design table's call -- a stitch is
    // `-B` from scratch over a derived branch, so the backup ref plus the
    // review is the protection, and there is nothing irreplaceable to warn
    // about.
    let sp2 = stitch::plan_stitch(m, git, name)?;
    if sp2.plan.id != sp.plan.id {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (reviewed id {}, live id {}) -- \
             state moved underneath; re-run to review the fresh plan",
            sp.plan.id,
            sp2.plan.id
        );
    }
    let out = stitch::apply_stitch(&sp2, actx)?;
    println!(
        "stitched `{}` -> {} -- postcondition ok ({} commit(s) on `{}`, every tier's patch-ids present)",
        sp2.name,
        short(&out.new_tip),
        sp2.picks.iter().map(|p| p.own.len()).sum::<usize>(),
        sp2.base_tier
    );
    if let Some((backup, tip)) = &sp2.backup {
        println!("  backup: {backup} -> {tip}");
    }
    println!("  journal: {}", out.outcome.journal.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn tag_cmd<G: GitRead>(
    m: &Manifest,
    ws: &Workspace,
    git: &G,
    policy: &Policy,
    actx: &ApplyCtx,
    json: bool,
    family: &str,
    n: u32,
    force: bool,
    apply: bool,
    yes: bool,
) -> Result<()> {
    // In-flight refusal only, deliberately not the full preflight: a tag
    // moves no branch and touches no worktree, but mid-replay the "component
    // tip" is a moving target and tagging it would memorialise a half-done
    // rewrite.
    let gd = ws.wine.git_dir()?;
    if let Some(fl) = sodactl_core::treestate::in_flight(&gd) {
        return Err(anyhow::Error::new(Refusal::InFlight {
            markers: fl.markers,
            narrative: fl.narrative,
        }));
    }
    let tp = tagverb::plan_tag(m, git, policy, family, n, force)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&tp)?);
    } else {
        render_plan(&tp.plan);
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    if force {
        // Only --force confirms: re-pointing a name (and overruling the
        // tag-time refusals) is the destructive subset; a plain cut is
        // create-only and CAS-protected.
        confirm(
            &format!(
                "tag --force re-points/overrules for `{}` (previous: {})",
                tp.tag,
                tp.previous.as_deref().map(short).unwrap_or_else(|| "<none>".into())
            ),
            yes,
        )?;
    }
    let tp2 = tagverb::plan_tag(m, git, policy, family, n, force)?;
    if tp2.plan.id != tp.plan.id {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (reviewed id {}, live id {}) -- \
             state moved underneath; re-run to review the fresh plan",
            tp.plan.id,
            tp2.plan.id
        );
    }
    let out = tagverb::apply_tag(&tp2, actx)?;
    println!("tagged `{}` -> {} (`{}`'s tip)", tp2.tag, short(&tp2.commit), tp2.target_ref);
    println!("  journal: {}", out.journal.display());
    // Derived, never invented (pushprint's rule): the hint names a remote
    // only when the tiers agree on one.
    match pushprint::tiers_single_remote(m) {
        Some(r) => println!(
            "  push it: git -C {} push {r} {}   # Florian pushes",
            actx.wine_path, tp2.tag
        ),
        None => println!(
            "  push it yourself -- the tiers name no single remote; `sodactl push` derives the \
             command"
        ),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pin_cmd(
    m: &Manifest,
    ws: &Workspace,
    wine_cli: &GitCli,
    actx: &ApplyCtx,
    json: bool,
    tag: &str,
    offline: bool,
    apply: bool,
    yes: bool,
) -> Result<()> {
    // Full preflight: step one checks the wine worktree out at the tag.
    plan::preflight(m, ws)?;
    let pp = pin::plan_pin(m, wine_cli, ws.superproject, &ws.wine_path, tag, offline)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&pp)?);
    } else {
        render_plan(&pp.plan);
    }
    if pp.plan.steps.is_empty() {
        // The honest no-op -- the plan's note already said why.
        return Ok(());
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    confirm(
        &format!("pin commits the superproject pointer to `{}` ({})", pp.tag, short(&pp.commit)),
        yes,
    )?;
    let pp2 = pin::plan_pin(m, wine_cli, ws.superproject, &ws.wine_path, tag, offline)?;
    if pp2.plan.id != pp.plan.id {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (reviewed id {}, live id {}) -- \
             state moved underneath; re-run to review the fresh plan",
            pp.plan.id,
            pp2.plan.id
        );
    }
    let out = pin::apply_pin(&pp2, actx, &ws.wine_path)?;
    println!("pinned `{}` ({}) -- gitlink and .gitmodules moved in one commit", pp2.tag, short(&pp2.commit));
    println!("  journal: {}", out.journal.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn regen_cmd<G: GitRead>(
    m: &Manifest,
    ws: &Workspace,
    git: &G,
    wine_cli: &GitCli,
    policy: &Policy,
    actx: &ApplyCtx,
    manifest_path: &Path,
    json: bool,
    consumer: &str,
    apply: bool,
) -> Result<()> {
    // Full preflight: `checkout -B` plus a cross-base rebase move the
    // worktree, the same exposure stitch has.
    plan::preflight(m, ws)?;
    let rp = regen::plan_regen(m, git, consumer)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rp)?);
    } else {
        render_plan(&rp.plan);
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    // Re-derive + id-compare, no y/N: the stitch rationale verbatim -- a
    // consumer is a derived, force-pushed branch rebuilt from scratch, so the
    // backup ref plus the review is the protection and there is nothing
    // irreplaceable to warn about.
    let rp2 = regen::plan_regen(m, git, consumer)?;
    if rp2.plan.id != rp.plan.id {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (reviewed id {}, live id {}) -- \
             state moved underneath; re-run to review the fresh plan",
            rp.plan.id,
            rp2.plan.id
        );
    }
    let deps = regen::RegenDeps {
        wine_cli,
        manifest_path,
        tracked_cache: ws.superproject_path.join(&policy.rr_cache),
        rr_cache_rel: &policy.rr_cache,
    };
    let out = regen::apply_regen(&rp2, actx, &deps)?;
    println!(
        "regenerated `{}` -> {} on `{}` -- postcondition ok (descends from the base)",
        rp2.consumer,
        short(&out.new_tip),
        rp2.base
    );
    for (p, s) in &out.auto_resolved {
        println!("  auto-resolved `{p}` by rule (strategy \"{s}\")");
    }
    println!(
        "  ledger: [ledger.{}] written into {} (UNCOMMITTED -- review and commit it, or a fresh \
         clone reads CONSUMER_CURRENCY_UNKNOWN)",
        rp2.consumer,
        manifest_path.display()
    );
    println!("  provenance: {} (annotated, carries the same record as JSON)", out.gen_tag);
    match &out.rr_commit {
        Some(c) => println!("  rr-cache: new resolutions committed ({})", short(c)),
        None => println!("  rr-cache: no new resolutions to commit"),
    }
    if let Some((backup, tip)) = &rp2.backup {
        println!("  backup: {backup} -> {tip}");
    }
    println!("  journal: {}", out.journal.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rrgc_cmd(
    m: &Manifest,
    ws: &Workspace,
    policy: &Policy,
    actx: &ApplyCtx,
    json: bool,
    older_than: u64,
    apply: bool,
    yes: bool,
) -> Result<()> {
    // In-flight refusal only, deliberately not the full preflight: the gc
    // touches no worktree and moves no branch, but `git rerere gc` mid-replay
    // would collect exactly the transient state the sequencer still needs.
    let gd = ws.wine.git_dir()?;
    if let Some(fl) = sodactl_core::treestate::in_flight(&gd) {
        return Err(anyhow::Error::new(Refusal::InFlight {
            markers: fl.markers,
            narrative: fl.narrative,
        }));
    }
    let gp = rrgc::plan_rrgc(m, policy, ws.superproject, ws.wine, &ws.superproject_path, older_than)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&gp)?);
    } else {
        render_plan(&gp.plan);
    }
    if !apply {
        println!("\nplan only -- nothing was changed. Re-run with --apply to execute.");
        return Ok(());
    }
    // The destructive subset: pruning deletes recorded resolutions.
    confirm(
        &format!(
            "rr-gc deletes {} cache entr(y/ies) and {} stray thisimage file(s) from both caches",
            gp.prune.len(),
            gp.strays.len()
        ),
        yes,
    )?;
    let gp2 = rrgc::plan_rrgc(m, policy, ws.superproject, ws.wine, &ws.superproject_path, older_than)?;
    // The id covers the git steps; the prune/stray sets live BESIDE the plan
    // (filesystem work cannot be steps), so drift in them must be compared
    // explicitly or a cache that changed between review and apply would be
    // pruned by the stale list.
    if gp2.plan.id != gp.plan.id || gp2.prune != gp.prune || gp2.strays != gp.strays {
        anyhow::bail!(
            "refused: the plan drifted between review and apply (the cache or the repos moved \
             underneath) -- re-run to review the fresh plan"
        );
    }
    let out = rrgc::apply_rrgc(&gp2, actx)?;
    println!(
        "rr-gc done: {} entr(y/ies) pruned, {} stray thisimage file(s) removed",
        out.pruned, out.strays_removed
    );
    match &out.committed {
        Some(c) => println!("  committed: {} (rr-cache paths only)", short(c)),
        None => println!("  nothing to commit"),
    }
    Ok(())
}

fn push_cmd(
    m: &Manifest,
    policy: &Policy,
    wine_cli: &GitCli,
    super_cli: &GitCli,
    wine_path: &str,
    json: bool,
) -> Result<()> {
    let pr = pushprint::plan_push(m, policy, wine_cli, super_cli, wine_path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&pr)?);
        return Ok(());
    }
    if pr.items.is_empty() {
        println!("nothing to push: every derived ref matches its remote.");
    } else {
        println!("PUSH COMMANDS  (in order, wine first -- run them yourself; sodactl never pushes)");
        for i in &pr.items {
            println!("  {}", i.command);
            println!("      # {}", i.reason);
        }
    }
    for n in &pr.notes {
        println!("  note: {n}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn release_cmd(
    m: &Manifest,
    policy: &Policy,
    wine_cli: &GitCli,
    super_cli: &GitCli,
    wine_path: &str,
    family: &str,
    n: u32,
    json: bool,
) -> Result<()> {
    let rr = release::plan_release(m, policy, wine_cli, super_cli, wine_path, family, n)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rr)?);
        return Ok(());
    }
    println!(
        "RELEASE {} {}  ({})  -- checklist only; nothing executes",
        rr.family,
        rr.version,
        rr.tag.as_deref().unwrap_or("<no derivable tag name>")
    );
    for item in &rr.items {
        let label = match item.status {
            release::ItemStatus::Done => "done   ",
            release::ItemStatus::Pending => "PENDING",
            release::ItemStatus::Unknown => "unknown",
        };
        println!("  [{label}] {:<8} {}", item.name, item.detail);
        for c in &item.commands {
            println!("           $ {c}");
        }
    }
    Ok(())
}

/// The build lanes, one handler for all three: plan in core, exec here.
///
/// Deliberately NOT plan-default like the git verbs: a lane mutates no ref --
/// its script is the superproject's own, re-runnable build machinery, its
/// preconditions are typed refusals, and "print the plan, re-run with
/// --apply" in front of a half-hour build would train people to add a flag
/// reflexively. The review form still exists: `--json` prints the serialized
/// lane (script, args, env, notes) and stops -- it cannot also exec, because
/// the moment the script runs, stdout belongs to it.
///
/// On success this function does not return: the child's exit code passes
/// through verbatim (`lanes.rs` has the contract story), including 0.
fn lane_cmd(
    plan: anyhow::Result<buildwrap::LanePlan>,
    ws: &Workspace,
    json: bool,
) -> Result<()> {
    let lp = plan?;
    if json {
        println!("{}", serde_json::to_string_pretty(&lp)?);
        return Ok(());
    }
    for l in lane_lines(&lp, &ws.superproject_path) {
        println!("{l}");
    }
    let code = crate::lanes::exec_lane(&lp, &ws.superproject_path)?;
    std::process::exit(code);
}

// ---- rendering and confirmation --------------------------------------------

/// The human form of a `Plan`: numbered steps with their literal argv, the
/// predicted ref mutations, the backup refs, the notes. Everything the design
/// doc says a review needs, nothing derived a second way -- the argv printed
/// IS `Step.argv`, which `plan.rs` derives from the action that executes.
///
/// Pure (a line vector), printed by `render_plan`: the CLI has no snapshot
/// harness, so testability here means the builder returns data.
fn plan_lines(p: &Plan) -> Vec<String> {
    let mut out = vec![format!("PLAN {}  (id {})", p.verb, p.id)];
    if p.steps.is_empty() {
        out.push("  no steps -- nothing would change.".into());
    }
    for (i, s) in p.steps.iter().enumerate() {
        out.push(format!("  {}. [{}] {}", i + 1, side(s.repo), s.description));
        out.push(format!("     $ git {}", shell_join(&s.argv)));
    }
    if !p.mutations.is_empty() {
        out.push("  ref mutations:".into());
        for mu in &p.mutations {
            out.push(format!(
                "    [{}] {}: {} -> {}",
                side(mu.repo),
                mu.refname,
                mu.expected_before.as_deref().unwrap_or("<absent>"),
                planned(&mu.after)
            ));
        }
    }
    if !p.backups.is_empty() {
        out.push("  backups (created create-only before any step):".into());
        for (name, tip) in &p.backups {
            out.push(format!("    {name} -> {tip}"));
        }
    }
    for n in &p.notes {
        out.push(format!("  note: {n}"));
    }
    out
}

pub fn render_plan(p: &Plan) {
    for l in plan_lines(p) {
        println!("{l}");
    }
}

/// The lane counterpart of `plan_lines`: same review vocabulary (the header,
/// a copy-pasteable `$` line, `note:` rows), but a `$ VERSION=x script` shape
/// -- what a human would literally type -- instead of numbered git steps.
fn lane_lines(p: &buildwrap::LanePlan, root: &std::path::Path) -> Vec<String> {
    let env: String = p
        .env
        .iter()
        .map(|(k, v)| format!("{k}={} ", shell_join(std::slice::from_ref(v))))
        .collect();
    let args = if p.args.is_empty() { String::new() } else { format!(" {}", shell_join(&p.args)) };
    let mut out = vec![
        format!("LANE {}  (exec from {}; the script's exit code passes through)", p.verb, root.display()),
        format!("  $ {env}./{}{args}", p.script),
    ];
    for n in &p.notes {
        out.push(format!("  note: {n}"));
    }
    out
}

/// What `confirm` must do, decided from its two inputs. Pure so the gating
/// table is testable without a terminal; `confirm` owns the I/O.
#[derive(Debug, PartialEq, Eq)]
enum ConfirmGate {
    /// `--yes`: proceed without asking.
    Skip,
    /// A tty is present: ask, one stdin line.
    Ask,
    /// No `--yes` and no tty: refuse. Silently proceeding because nobody
    /// COULD answer is how confirmation prompts stop meaning anything in
    /// scripts.
    RefuseNotATty,
}

fn confirm_gate(yes: bool, tty: bool) -> ConfirmGate {
    match (yes, tty) {
        (true, _) => ConfirmGate::Skip,
        (false, true) => ConfirmGate::Ask,
        (false, false) => ConfirmGate::RefuseNotATty,
    }
}

/// The one affirmative spelling table, shared by nothing else on purpose:
/// anything not explicitly "yes" -- including an empty line, EOF, "Y es" --
/// declines. Declining is the human tripping the gate, not an error.
fn affirmative(line: &str) -> bool {
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// One stdin line on a tty, or `--yes`. Declining is the human tripping the
/// gate: exit 1, nothing changed.
fn confirm(what: &str, yes: bool) -> Result<()> {
    match confirm_gate(yes, std::io::stdin().is_terminal()) {
        ConfirmGate::Skip => Ok(()),
        ConfirmGate::RefuseNotATty => anyhow::bail!(
            "refused: confirmation required ({what}) and stdin is not a terminal -- re-run with --yes"
        ),
        ConfirmGate::Ask => {
            print!("{what} -- proceed? [y/N] ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            if !affirmative(&line) {
                eprintln!("aborted -- nothing was changed.");
                std::process::exit(crate::EXIT_GATE);
            }
            Ok(())
        }
    }
}

fn side(r: RepoSide) -> &'static str {
    match r {
        RepoSide::Wine => "wine",
        RepoSide::Superproject => "superproject",
    }
}

fn planned(v: &PlannedValue) -> String {
    match v {
        PlannedValue::Oid { oid } => oid.clone(),
        PlannedValue::Unknown { reason } => format!("(unknown: {reason})"),
    }
}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Copy-pasteable quoting for the `$ git ...` display line: anything shell-
/// meaningful gets single quotes (with the standard `'\''` escape). Display
/// only -- execution never round-trips through a shell.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.chars().any(|c| c.is_whitespace() || "'\"\\$`".contains(c)) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- tests ------------------------------------------------------------------
//
// The CLI's testable surface is deliberately the PURE part: the line builders,
// the confirmation gating table, the exit-code mapping, and the spawn audit.
// Verb LOGIC tests live in sodactl-core against real fixture repos; spawning
// the built binary from here to re-test them would be a second, slower copy of
// the same coverage with a tty dependency on top.
#[cfg(test)]
mod tests {
    use super::*;
    use sodactl_core::plan::{Action, RefMutation, Step};
    use sodactl_core::verbs::buildwrap::{LanePlan, LaneRefusal};
    use std::path::{Path, PathBuf};

    // -- rendering ------------------------------------------------------------

    fn sample_plan() -> Plan {
        Plan::new(
            "demo",
            vec![Step::new(
                "move the branch",
                RepoSide::Wine,
                Action::UpdateRefCas {
                    refname: "refs/heads/soda-demo".into(),
                    new: "1111111111111111111111111111111111111111".into(),
                    expected_old: Some("2222222222222222222222222222222222222222".into()),
                },
            )],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/soda-demo".into(),
                expected_before: Some("2222222222222222222222222222222222222222".into()),
                after: PlannedValue::Oid { oid: "1111111111111111111111111111111111111111".into() },
            }],
            vec![("refs/sodactl/backup/20260724T000000Z/soda-demo".into(), "2222".into())],
            vec!["a note".into()],
        )
    }

    #[test]
    fn plan_lines_carry_every_review_ingredient_in_order() {
        let p = sample_plan();
        let lines = plan_lines(&p);
        assert_eq!(lines[0], format!("PLAN demo  (id {})", p.id));
        assert_eq!(lines[1], "  1. [wine] move the branch");
        // The `$` line is Step.argv verbatim -- derived from the Action that
        // executes, so the review can never show a different command.
        assert_eq!(
            lines[2],
            "     $ git update-ref refs/heads/soda-demo 1111111111111111111111111111111111111111 \
             2222222222222222222222222222222222222222"
        );
        assert_eq!(lines[3], "  ref mutations:");
        assert_eq!(
            lines[4],
            "    [wine] refs/heads/soda-demo: 2222222222222222222222222222222222222222 -> \
             1111111111111111111111111111111111111111"
        );
        assert_eq!(lines[5], "  backups (created create-only before any step):");
        assert_eq!(lines[6], "    refs/sodactl/backup/20260724T000000Z/soda-demo -> 2222");
        assert_eq!(lines[7], "  note: a note");
        assert_eq!(lines.len(), 8);
    }

    #[test]
    fn an_empty_plan_says_so_in_words() {
        let p = Plan::new("noop", vec![], vec![], vec![], vec![]);
        let lines = plan_lines(&p);
        assert_eq!(lines[1], "  no steps -- nothing would change.");
    }

    #[test]
    fn a_create_only_mutation_renders_absent_not_a_zero_oid() {
        let p = Plan::new(
            "tag",
            vec![],
            vec![RefMutation {
                repo: RepoSide::Superproject,
                refname: "refs/tags/x".into(),
                expected_before: None,
                after: PlannedValue::Unknown { reason: "annotated tag object".into() },
            }],
            vec![],
            vec![],
        );
        let lines = plan_lines(&p);
        // lines[1] is the empty-steps sentence; the mutation block follows.
        assert_eq!(
            lines[3],
            "    [superproject] refs/tags/x: <absent> -> (unknown: annotated tag object)"
        );
    }

    #[test]
    fn lane_lines_show_the_literal_invocation_a_human_would_type() {
        let lp = LanePlan {
            verb: "package",
            script: "packaging/package.sh".into(),
            args: vec![],
            env: vec![("VERSION".into(), "spritzvalve-11.0-1".into())],
            notes: vec!["package.sh reads VERSION from the environment".into()],
        };
        let lines = lane_lines(&lp, Path::new("/w"));
        assert_eq!(
            lines[0],
            "LANE package  (exec from /w; the script's exit code passes through)"
        );
        // env rendered shell-style before the script: copy-pasteable as-is.
        assert_eq!(lines[1], "  $ VERSION=spritzvalve-11.0-1 ./packaging/package.sh");
        assert_eq!(lines[2], "  note: package.sh reads VERSION from the environment");
    }

    #[test]
    fn shell_join_quotes_exactly_what_a_shell_would_mangle() {
        let j = |v: &[&str]| shell_join(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(j(&["rebase", "--onto", "a", "b"]), "rebase --onto a b");
        assert_eq!(j(&["commit", "-m", "two words"]), "commit -m 'two words'");
        assert_eq!(j(&["-m", "it's"]), "-m 'it'\\''s'");
        assert_eq!(j(&["a$b"]), "'a$b'");
        assert_eq!(j(&[""]), "''");
    }

    // -- confirmation gating ---------------------------------------------------

    #[test]
    fn the_confirm_gate_table_is_exactly_three_rows() {
        assert_eq!(confirm_gate(true, true), ConfirmGate::Skip);
        // --yes wins even without a tty: that IS the scripting path.
        assert_eq!(confirm_gate(true, false), ConfirmGate::Skip);
        assert_eq!(confirm_gate(false, true), ConfirmGate::Ask);
        assert_eq!(confirm_gate(false, false), ConfirmGate::RefuseNotATty);
    }

    #[test]
    fn only_an_explicit_yes_is_affirmative() {
        for ok in ["y", "Y", "yes", " y\n", "yes\n"] {
            assert!(affirmative(ok), "{ok:?} must proceed");
        }
        // Everything else declines -- including EOF's empty read and the
        // "YES" someone shouts at a prompt that documented lowercase.
        for no in ["", "\n", "n", "N", "no", "YES", "Yes", "y es", "q"] {
            assert!(!affirmative(no), "{no:?} must decline");
        }
    }

    // -- exit codes ------------------------------------------------------------

    #[test]
    fn refusals_gate_and_plain_errors_error() {
        let gate = |e: anyhow::Error| assert_eq!(exit_code_for(&e), crate::EXIT_GATE, "{e:#}");
        let err = |e: anyhow::Error| assert_eq!(exit_code_for(&e), crate::EXIT_ERROR, "{e:#}");

        gate(anyhow::Error::new(Refusal::InFlight {
            markers: vec!["rebase-merge"],
            narrative: String::new(),
        }));
        gate(anyhow::Error::new(LaneRefusal::HeadOffPin {
            head: "1".repeat(40),
            pin: "2".repeat(40),
            pin_source: "index",
        }));
        // A context wrapper must not launder a refusal into exit 2 -- anyhow's
        // downcast walks the chain.
        gate(
            anyhow::Error::new(Refusal::DirtyTracked { paths: vec!["a".into()] })
                .context("while planning"),
        );
        // The display-prefix fallback for a refusal minted as a plain message.
        gate(anyhow::anyhow!("refused: the plan drifted between review and apply"));

        err(anyhow::anyhow!("reading manifest: No such file"));
        err(anyhow::anyhow!("a refusal mentioned mid-sentence: refused: is not a prefix"));
    }

    // -- the spawn audit -------------------------------------------------------

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// The tui guardrail's mould, for the crate that DOES exec: every spawn in
    /// sodactl-cli is the one audited build-lane site in `lanes.rs`. This
    /// crate fronts mutating verbs against Florian's live repos, so a
    /// convenient `Command` added anywhere else -- a "quick" `git` call in a
    /// verb arm, bypassing `GitMut`'s audited runner -- must fail a test, not
    /// a review.
    #[test]
    fn every_spawn_is_the_one_audited_build_lane_site() {
        // Fragments, so this test's own source never matches itself (the mcp
        // rationale: an exemption marker on a guardrail is the thing that
        // eventually gets used on a real offender).
        let spawn = "Command::".to_string() + "new";
        let marker = "AUDITED BUILD-LANE ".to_string() + "CONSTRUCTOR";
        let audited = ["lanes.rs"];

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(files.len() >= 3, "the source walk must actually find this crate");

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let hits: Vec<usize> = text
                .lines()
                .enumerate()
                // Comments mention the pattern (lanes.rs documents its own
                // audit); only code can spawn.
                .filter(|(_, l)| !l.trim_start().starts_with("//"))
                .filter(|(_, l)| l.contains(&spawn))
                .map(|(n, _)| n + 1)
                .collect();
            if audited.contains(&name.as_str()) {
                assert_eq!(
                    hits.len(),
                    1,
                    "{name} must contain exactly its one audited spawn site, found lines {hits:?}"
                );
                assert!(
                    text.contains(&marker),
                    "{name} lost its audit marker -- re-audit before renaming anything"
                );
            } else {
                for line in hits {
                    offenders.push(format!("{}:{line}", f.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "spawn sites outside the audited build-lane constructor -- every git \
             this crate runs must go through sodactl-git's audited runners:\n{}",
            offenders.join("\n")
        );
    }

    /// The no-push guardrail at the CLI layer: the lanes are the only exec,
    /// and none of them is a `git push` in a trenchcoat. The core/git crates
    /// carry the real `no_push` tests; this one pins the CLI's contribution
    /// -- the lane scripts' names never grow a push lane without the
    /// conversation happening here.
    #[test]
    fn the_lane_scripts_are_the_known_three() {
        // CORE's compiled constants, not a local copy: `buildwrap` documents
        // that every `plan_*` builds its `LanePlan.script` from exactly these
        // three, so asserting against the constants pins what the CLI can
        // possibly exec -- a fourth lane, a renamed script, or a push wrapper
        // changes a constant and fails here first. (An earlier version of
        // this test compared two arrays both written in the test; it could
        // not fail.)
        let planned = [buildwrap::BUILD_SCRIPT, buildwrap::PACKAGE_SCRIPT, buildwrap::SMOKE_SCRIPT];
        assert_eq!(planned, ["build.sh", "packaging/package.sh", "ci/smoke.sh"]);
        for script in planned {
            assert!(
                !script.contains("push"),
                "`{script}` smells like a push lane -- that conversation happens here first"
            );
        }
    }
}
