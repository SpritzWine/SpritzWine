//! Tier refinements: divergence, linearity, the declared parent edge.
//!
//! Tiers are AUTHORED, not replayed, so ancestry is the correct primitive here
//! -- this module is the one place in the check set where `is_ancestor` is not
//! the bug. `reconcile()` already owns the parent-tip check; what follows is
//! what it drops on the floor.

use super::GitRead;
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;

pub fn check(m: &Manifest, git: &dyn GitRead) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();
    let home = &m.repo.home_base.commit;

    for t in &m.tier {
        if git.rev_parse(&t.name)?.is_none() {
            continue; // reconcile() owns TIER_MISSING; do not double-report.
        }

        if let Some(remote) = t.remote.as_deref() {
            let tracking = format!("{remote}/{}", t.name);
            if let Some(ab) = git.ahead_behind(&tracking, &t.name)? {
                // `reconcile()` computes `behind` and never reads it, which
                // makes ahead>0 && behind>0 indistinguishable from a plain
                // unpushed branch. They are different situations with
                // different remedies: a diverged tier was REWRITTEN by a
                // restack, so the plain `git push` it prints will simply be
                // rejected as non-fast-forward, and the user goes into a
                // confusing recovery instead of a force-with-lease.
                if ab.ahead > 0 && ab.behind > 0 {
                    f.push(Finding::new(
                        "TIER_DIVERGED",
                        Severity::Drift,
                        &t.name,
                        format!(
                            "`{}` is {} ahead and {} behind {tracking} -- it was rewritten (restacked) and the remote still advertises the pre-restack tip",
                            t.name, ab.ahead, ab.behind
                        ),
                        format!(
                            "git -C wine push --force-with-lease {remote} {}   # rewritten by a restack; a plain push is rejected",
                            t.name
                        ),
                    ));
                } else if ab.ahead == 0 && ab.behind > 0 {
                    // Blocker: either someone else's authored work is about to
                    // be dropped from every pour, or your local tier is the
                    // stale one. Both are answered by looking, not by pushing.
                    f.push(Finding::new(
                        "TIER_BEHIND_REMOTE",
                        Severity::Blocker,
                        &t.name,
                        format!(
                            "`{}` is {} behind {tracking} with nothing of its own -- the remote carries authored work this tier does not, and every pour from here silently drops it",
                            t.name, ab.behind
                        ),
                        format!("git -C wine fetch {remote} && git -C wine log {tracking} ^{}   # inspect before doing anything; do NOT force", t.name),
                    ));
                }
            }
        }

        // BRANCHING.md promises a tier is "a straight line, no merges". A merge
        // inside one breaks `rebase --onto` replay semantics and silently
        // changes what every consumer downstream receives.
        let parent = if t.parent == "$home_base" { home.clone() } else { t.parent.clone() };
        if git.rev_parse(&parent)?.is_none() {
            continue; // reconcile() reports the broken edge; nothing to add.
        }
        let range = format!("{parent}..{}", t.name);

        let merges = git.merges_in(&range)?;
        if !merges.is_empty() {
            f.push(Finding::new(
                "TIER_NOT_LINEAR",
                Severity::Drift,
                &t.name,
                format!(
                    "`{}` contains {} merge commit(s) above `{}` -- a tier must be a straight line or its replay is not well-defined",
                    t.name,
                    merges.len(),
                    t.parent
                ),
                format!("git -C wine rebase {parent} {}   # linearize, then re-stitch and re-pour downstream", t.name),
            ));
        }

        // The manifest's `parent` is the only geometry it declares, so it is
        // the only thing it can declare WRONGLY -- and `is_ancestor` cannot
        // catch the interesting error. If a tier is really stacked on
        // `soda-glorious` but declares `soda-core`, ancestry still holds
        // (soda-core is below soda-glorious) and reconcile() is satisfied. The
        // declared parent is merely too LOW, and every replay that uses it
        // silently drags soda-glorious along.
        //
        // The tell: the commits in `parent..tier` should all be this tier's
        // own. One carrying ANOTHER tier's subject prefix means that tier sits
        // between the two, and naming it is the actionable part.
        //
        // Subject prefixes are a convention, not a guarantee, so this is
        // Hygiene with the offending subject quoted -- and it stays silent
        // unless a commit is prefixed with a *declared tier's* name, which is
        // far too specific to arise by accident.
        if t.parent != "$home_base" {
            let own = git.log_subjects(&range)?;
            let intruder = own.iter().find_map(|(_, s)| {
                m.tier
                    .iter()
                    .find(|x| x.name != t.name && s.starts_with(&format!("{}:", x.name)))
                    .map(|x| (x.name.as_str(), s.as_str()))
            });
            if let Some((other, subject)) = intruder {
                f.push(Finding::new(
                    "TIER_PARENT_EDGE_WRONG",
                    Severity::Hygiene,
                    &t.name,
                    format!(
                        "`{}` declares its parent as `{}`, but the range above it carries {subject:?} -- `{other}` appears to sit between them, so the declared edge is too low and every replay drags `{other}` along",
                        t.name, t.parent
                    ),
                    format!("verify with: git -C wine log --oneline {range}   # then correct `parent` in sodactl.toml"),
                ));
            }
        }
    }
    Ok(f)
}
