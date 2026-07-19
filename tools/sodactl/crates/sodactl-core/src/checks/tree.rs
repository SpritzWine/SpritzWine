//! Working-tree state: is it safe to replay onto, and is a replay already
//! half-done?
//!
//! The design constraint that shapes this file: **this project's build tree is
//! always warm.** A dirty-tree guard that fires on generated files gets
//! disabled within a day, and then the guard that protects real uncommitted
//! work is gone too. So the check splits by kind -- tracked modifications are a
//! Blocker, unknown untracked files are Hygiene, and allowlisted build products
//! are silent, not even counted.

use super::{glob_match, Workspace};
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;

pub fn check(m: &Manifest, ws: &Workspace) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();

    // D2 -- the non-negotiable one: no mutating verb may run while a replay is
    // half-done. `regenerate.sh` exits 1 leaving the tree mid-rebase AND
    // detached.
    //
    // The gitdir must be RESOLVED, not guessed: `wine/.git` is a file holding
    // `gitdir: ../.git/modules/wine`, so a check that stats `wine/.git/rebase-merge`
    // finds nothing and passes forever.
    let gd = ws.wine.git_dir()?;
    let in_flight: Vec<&str> = ["rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD", "MERGE_RR", "sequencer"]
        .into_iter()
        .filter(|n| gd.join(n).exists())
        .collect();
    if !in_flight.is_empty() {
        // Report richly: which rebase, how far in. A bare boolean tells you to
        // stop without telling you what you are in the middle of.
        let onto = read_trimmed(&gd.join("rebase-merge/onto"));
        let head_name = read_trimmed(&gd.join("rebase-merge/head-name"));
        let progress = match (read_trimmed(&gd.join("rebase-merge/msgnum")), read_trimmed(&gd.join("rebase-merge/end"))) {
            (Some(n), Some(e)) => format!(", at step {n}/{e}"),
            _ => String::new(),
        };
        let what = match (head_name, onto) {
            (Some(h), Some(o)) => format!(" replaying {h} onto {o}{progress}"),
            _ => progress,
        };
        f.push(Finding::new(
            "REBASE_IN_FLIGHT",
            Severity::Blocker,
            &ws.wine_path,
            format!("a replay is in progress in the wine submodule ({}){what} -- no operation may touch refs until it is finished or aborted", in_flight.join(", ")),
            "git -C wine rebase --continue, or git -C wine rebase --abort".to_string(),
        ));
    }

    // D1 -- split by kind.
    let mut tracked = Vec::new();
    let mut unknown_untracked = Vec::new();
    for e in ws.wine.status(true)? {
        // The allowlist applies to BOTH kinds, and restricting it to untracked
        // paths made it inert exactly where the manifest says it is needed.
        // `configure` is the allowlist's first non-stamp entry AND is tracked in
        // upstream wine (the manifest's own `binary_resolution` note says so),
        // so the moment the build runs autoreconf it becomes ` M configure`,
        // takes the tracked branch, and raises the SUBMODULE_DIRTY Blocker the
        // allowlist exists to prevent. Silent today only because `configure`
        // happens to be unmodified right now.
        //
        // This does not weaken the guard: the whole design here is that a
        // DECLARED build product is silent whatever its status, and an
        // undeclared tracked modification is still an unconditional Blocker.
        if m.build_artifacts.iter().any(|g| glob_match(g, &e.path)) {
            continue;
        }
        if e.is_untracked() {
            unknown_untracked.push(e.path);
        } else {
            tracked.push(e.path);
        }
    }
    if !tracked.is_empty() {
        f.push(Finding::new(
            "SUBMODULE_DIRTY",
            Severity::Blocker,
            &ws.wine_path,
            format!(
                "{} tracked path(s) modified in the wine submodule ({}{}) -- a rebase or `checkout -B` would destroy them",
                tracked.len(),
                tracked.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                if tracked.len() > 4 { ", …" } else { "" }
            ),
            "git -C wine stash, or commit them to a tier -- never pour over uncommitted work".to_string(),
        ));
    }
    if !unknown_untracked.is_empty() {
        f.push(Finding::new(
            "SUBMODULE_UNTRACKED",
            Severity::Hygiene,
            &ws.wine_path,
            format!(
                "{} untracked path(s) not covered by `build_artifacts` ({}{})",
                unknown_untracked.len(),
                unknown_untracked.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                if unknown_untracked.len() > 4 { ", …" } else { "" }
            ),
            "delete them, or add them to `build_artifacts` in sodactl.toml if the build generates them".to_string(),
        ));
    }

    // D3 -- detached HEAD is NORMAL after a pin and abnormal after a failed
    // regen, so it is only legible next to D2. Reported plainly either way.
    if ws.wine.head_branch()?.is_none() {
        f.push(Finding::new(
            "WINE_HEAD_DETACHED",
            Severity::Hygiene,
            &ws.wine_path,
            "the wine submodule is on a detached HEAD -- the correct resting state for a pinned build, and also what an aborted pour leaves behind".to_string(),
            // Deliberately does NOT say "checkout <consumer-or-tier>". It used
            // to, and that is advice this same tool contradicts: landing on a
            // consumer trips CHECKOUT_ON_GENERATED_BRANCH at a HIGHER severity,
            // because a commit made there is force-pushed away by the next pour
            // with nothing to recover from. A checklist that recommends the
            // thing it then reports spends exactly the trust it needs in order
            // to be believed when something is genuinely wrong.
            "nothing to do if you pinned deliberately; if a pour aborted, `git -C wine rebase --abort` or re-run it. Attach to a soda-* tier only to AUTHOR -- never to a consumer".to_string(),
        ));
    }

    // D4 -- only matters as a precondition for verbs that COMMIT in the
    // superproject (`pin`, and `regen` capturing the rr-cache). Hygiene, and
    // reported as a count so a permanently-noisy tree does not bury the rest.
    let sp = ws.superproject.status(false)?;
    if !sp.is_empty() {
        f.push(Finding::new(
            "SUPERPROJECT_DIRTY",
            Severity::Hygiene,
            ".",
            format!(
                "{} tracked path(s) modified in the superproject ({}{}) -- `pin` and `regen` commit here",
                sp.len(),
                sp.iter().take(4).map(|e| e.path.clone()).collect::<Vec<_>>().join(", "),
                if sp.len() > 4 { ", …" } else { "" }
            ),
            "commit or stash before running a verb that writes the superproject".to_string(),
        ));
    }

    Ok(f)
}

fn read_trimmed(p: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
