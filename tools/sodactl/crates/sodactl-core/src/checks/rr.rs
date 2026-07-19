//! rr-cache health.
//!
//! The rerere cache is the only reason regeneration is not a manual conflict
//! marathon, and every one of these failures is silent: the replay just
//! re-prompts, and the human assumes that is normal.

use super::{Policy, Workspace};
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;
use crate::rrcache::{shadowed_by, RrSurvey};

pub fn check(m: &Manifest, ws: &Workspace, p: &Policy) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();
    let tracked_root = ws.superproject_path.join(&p.rr_cache);
    let tracked = RrSurvey::read(&tracked_root)?;

    if tracked.missing {
        // Nothing to check, and nothing to claim. Silence beats a clean bill.
        return Ok(f);
    }

    // Only UNSCOPED rules can shadow an entry. Dropping the `consumers` scope
    // here was a live false positive whose remedy destroyed data.
    //
    // A `[[binary_resolution]]` scoped to one consumer decides that path for
    // THAT pour only. The live `configure` rule is `consumers = ["spritzvalve"]`
    // because Valve's proton_11.0 does not track the generated `configure`, so
    // that replay is a modify/delete -- which produces no rerere preimage at
    // all. Upstream wine DOES track it, so a *content* preimage of `configure`
    // can only have come from a spritzwine pour, where no rule applies and
    // rerere both can and must be consulted. The check nonetheless declared two
    // such entries dead ("it can never be consulted") and offered to prune them:
    // a confident false boolean whose remedy deletes the resolution it
    // misdeclared. That is exactly the failure `rrcache::shadowed_by` documents
    // itself as built to avoid -- the attribution heuristic was sound, the
    // scoping around it was missing.
    let resolution_paths = shadowing_paths(m);

    for e in tracked.entries.iter().filter(|e| e.unresolved()) {
        // B2 first: an entry an unscoped `[[binary_resolution]]` rule already
        // decides can never be consulted -- `regen` applies the rule
        // pre-emptively. It is dead weight, and worse, a human triaging the
        // unresolved list would burn time re-resolving a settled conflict.
        if let Some(path) = shadowed_by(e, &resolution_paths) {
            f.push(Finding::new(
                "RR_ENTRY_SHADOWED_BY_RULE",
                Severity::Hygiene,
                &e.id,
                format!(
                    // Hedged deliberately. rerere never stores the path, so this
                    // is a content heuristic (`rrcache::shadowed_by`), and
                    // rendering a heuristic as a flat fact is how a wrong
                    // attribution gets acted on. `RR_NO_POSTIMAGE` below quotes
                    // its evidence for the same reason.
                    "rr-cache entry `{}` looks (by preimage content) like a conflict in `{path}`, which an unscoped [[binary_resolution]] rule already decides -- if so it can never be consulted",
                    e.id
                ),
                "confirm against the preimage, then prune by hand: rm -r <rr-cache>/<id>   # `sodactl rr gc` is NOT implemented (v2)".to_string(),
            ));
            continue;
        }

        // B1 -- a preimage with no postimage is a conflict rerere RECORDED but
        // whose resolution was never captured. It does not merely fail to help:
        // it re-prompts on every replay, forever, while looking like a cache
        // hit that did not work.
        //
        // The hint is the preimage's first lines verbatim. rerere stores
        // content, never the path, so this tool genuinely cannot name the file
        // -- offering the evidence beats inventing an attribution.
        let hint = e.preimage_head.first().map(|l| format!(" (preimage begins {l:?})")).unwrap_or_default();
        f.push(Finding::new(
            "RR_NO_POSTIMAGE",
            Severity::Drift,
            &e.id,
            format!("rr-cache entry `{}` has a preimage but no postimage -- the conflict is recorded, the resolution is not{hint}", e.id),
            "reproduce the conflict and re-resolve so a postimage is captured, or prune by hand: rm -r <rr-cache>/<id>   # `sodactl rr gc` is NOT implemented (v2)".to_string(),
        ));
    }

    // B3 -- `thisimage` is rerere's transient state for an IN-PROGRESS merge.
    // Committing it means a conflict was mid-resolution when the cache was
    // captured; it is meaningless to every other clone and is a tell that a
    // regen was interrupted.
    let stray: Vec<&str> = tracked.entries.iter().filter(|e| e.has_thisimage).map(|e| e.id.as_str()).collect();
    if !stray.is_empty() {
        f.push(Finding::new(
            "RR_STRAY_THISIMAGE",
            Severity::Hygiene,
            &p.rr_cache,
            format!("{} rr-cache entr(y/ies) carry a stray `thisimage` ({}) -- an interrupted regen left transient state behind", stray.len(), stray.join(", ")),
            format!("git -C . rm --cached {}/*/thisimage && rm {}/*/thisimage", p.rr_cache, p.rr_cache),
        ));
    }

    // B4 -- `regenerate.sh` only WARNS that it copied newly-learned resolutions
    // back out; committing them is a manual step. A resolution living in one
    // person's working tree is a resolution the CI runner and every future
    // clone rediscovers by hand.
    let dirty: Vec<String> = ws
        .superproject
        .status(true)?
        .into_iter()
        .filter(|e| e.path.starts_with(&p.rr_cache))
        .map(|e| e.path)
        .collect();
    if !dirty.is_empty() {
        f.push(Finding::new(
            "RR_UNCOMMITTED",
            Severity::Drift,
            &p.rr_cache,
            format!("{} uncommitted path(s) under `{}` -- newly learned resolutions exist only in this working tree", dirty.len(), p.rr_cache),
            format!("git add {} && git commit -m \"rr-cache: capture {} resolution(s)\"", p.rr_cache, dirty.len()),
        ));
    }

    // B5 -- cheap, and it catches a whole class of "why did it re-prompt".
    //
    // The live cache is NOT `wine/.git/rr-cache`: `wine/.git` is a file holding
    // `gitdir: ../.git/modules/wine`. Resolving it is the difference between a
    // real check and one that silently passes forever.
    let live_root = ws.wine.git_dir()?.join("rr-cache");
    let live = RrSurvey::read(&live_root)?;
    // Set membership, not cardinality. rr-cache entries are content-addressed by
    // directory name, so the exact question -- "which tracked resolutions is the
    // live cache missing?" -- is trivially available, and comparing counts
    // instead reads two DISJOINT sets of equal size as "seeded". That is the
    // counts-as-evidence rule the manifest bans, in the one place where being
    // wrong means a replay silently re-prompts for conflicts the repo knows.
    let live_ids: std::collections::BTreeSet<&str> =
        live.entries.iter().map(|e| e.id.as_str()).collect();
    let absent: Vec<&str> = tracked
        .entries
        .iter()
        .map(|e| e.id.as_str())
        .filter(|id| !live_ids.contains(id))
        .collect();
    if live.missing || !absent.is_empty() {
        f.push(Finding::new(
            "RR_CACHE_UNSEEDED",
            Severity::Hygiene,
            &ws.wine_path,
            format!(
                "the live rerere cache at {} is missing {} of the {} resolution(s) tracked in `{}` ({}{}) -- a replay will re-prompt for conflicts the repo already knows",
                live_root.display(),
                if live.missing { tracked.entries.len() } else { absent.len() },
                tracked.entries.len(),
                p.rr_cache,
                if live.missing {
                    "the live cache does not exist".to_string()
                } else {
                    absent.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                },
                if !live.missing && absent.len() > 3 { ", …" } else { "" }
            ),
            format!("mkdir -p {} && cp -r {}/* {}/", live_root.display(), tracked_root.display(), live_root.display()),
        ));
    }

    Ok(f)
}

/// The `[[binary_resolution]]` paths that can shadow an rr-cache entry.
///
/// Only UNSCOPED rules qualify, and this is the whole content of a fixed false
/// positive -- see the comment at the call site. Split out as a function purely
/// so the scoping rule is testable without a filesystem.
fn shadowing_paths(m: &Manifest) -> Vec<String> {
    m.binary_resolution
        .iter()
        .filter(|b| b.consumers.is_empty())
        .map(|b| b.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "abc"
describes = "d"
derivation = "x"

[[binary_resolution]]
path = "configure"
strategy = "delete"
reason = "valve does not track it"
consumers = ["spritzvalve"]

[[binary_resolution]]
path = "dlls/user32/resources/oic_winlogo.ico"
strategy = "ours"
reason = "the tier owns the glass icon unconditionally"
"#;

    /// THE regression. A rule scoped to one consumer decides that path for that
    /// pour only, so it cannot declare a cached resolution universally dead.
    /// Ignoring the scope made the tool report two `configure` entries -- the
    /// ones a *spritzwine* pour genuinely needs, since upstream wine tracks
    /// `configure` and only Valve's base does not -- as prunable dead weight.
    #[test]
    fn a_consumer_scoped_rule_does_not_shadow() {
        let m = Manifest::parse(MANIFEST).unwrap();
        let paths = shadowing_paths(&m);
        assert!(!paths.contains(&"configure".to_string()), "scoped rule must not shadow: {paths:?}");
    }

    /// ...and the converse, so the fix is a scoping rule rather than a mute
    /// button: an unscoped rule applies to every pour and still shadows.
    #[test]
    fn an_unscoped_rule_still_shadows() {
        let m = Manifest::parse(MANIFEST).unwrap();
        assert!(shadowing_paths(&m).contains(&"dlls/user32/resources/oic_winlogo.ico".to_string()));
    }
}
