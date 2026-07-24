//! Working-tree state: is it safe to replay onto, and is a replay already
//! half-done?
//!
//! The design constraint that shapes this file: **this project's build tree is
//! always warm.** A dirty-tree guard that fires on generated files gets
//! disabled within a day, and then the guard that protects real uncommitted
//! work is gone too. So the check splits by kind -- tracked modifications are a
//! Blocker, unknown untracked files are Hygiene, and allowlisted build products
//! are silent, not even counted.

use super::Workspace;
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;
use crate::treestate;

pub fn check(m: &Manifest, ws: &Workspace) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();

    // D2 -- the non-negotiable one: no mutating verb may run while a replay is
    // half-done. `regenerate.sh` exits 1 leaving the tree mid-rebase AND
    // detached.
    //
    // The probe itself lives in `treestate` because `plan::preflight` refuses
    // on the same observation -- a probe fixed here but not there lets a verb
    // rebase over exactly this state. The resolved-gitdir requirement and the
    // rich narrative are documented on the helper.
    let gd = ws.wine.git_dir()?;
    if let Some(fl) = treestate::in_flight(&gd) {
        f.push(Finding::new(
            "REBASE_IN_FLIGHT",
            Severity::Blocker,
            &ws.wine_path,
            format!("a replay is in progress in the wine submodule ({}){} -- no operation may touch refs until it is finished or aborted", fl.markers.join(", "), fl.narrative),
            "git -C wine rebase --continue, or git -C wine rebase --abort".to_string(),
        ));
    }

    // D1 -- split by kind. The split (and the reason the allowlist applies to
    // BOTH kinds -- the tracked-`configure` story) lives in
    // `treestate::split_dirty`, shared with the verbs' preflight refusal.
    let treestate::DirtySplit { tracked, unknown_untracked } =
        treestate::split_dirty(ws.wine.status(true)?, &m.build_artifacts);
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
