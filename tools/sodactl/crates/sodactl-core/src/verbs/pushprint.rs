//! `push --print` -- the ordered, copy-pasteable push list. PRINTS ONLY.
//!
//! Florian pushes (the project's push-workflow rule: no ssh-agent, Codeberg
//! only, submodule first); there is no push verb and there will not be one.
//! This module therefore takes NO mutating handle -- its signature is
//! `GitRead`/`GitCli` only, and the source-scan test below pins that the
//! identifier of the mutating type never appears here.
//!
//! Derivation rules:
//! * Only refs that actually differ from their remote appear. Branches are
//!   compared against their local tracking refs (offline, same as `status`);
//!   tags -- which have no tracking refs -- are probed with `ls-remote`, and
//!   an unreachable remote degrades to a NOTE naming what could not be
//!   verified rather than a silent omission.
//! * `--force-with-lease` for tiers and consumers (both get rewritten; the
//!   lease is the CAS discipline applied to the remote), plain push for tags
//!   and the superproject branch (which only ever gain).
//! * Wine first, then the superproject -- the submodule-first ordering that
//!   keeps the superproject's pin from ever advertising a commit its remote
//!   cannot serve.

use crate::checks::Policy;
use crate::manifest::Manifest;
use crate::plan::RepoSide;
use crate::tags::TagName;
use sodactl_git::{GitCli, GitRead};
use std::collections::BTreeMap;

/// One command to copy-paste, with the derivation that put it there.
#[derive(Debug, serde::Serialize)]
pub struct PushItem {
    pub repo: RepoSide,
    pub command: String,
    pub reason: String,
}

/// The whole list plus everything that could NOT be derived, named.
#[derive(Debug, serde::Serialize)]
pub struct PushReport {
    pub items: Vec<PushItem>,
    pub notes: Vec<String>,
}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// The one remote the tiers agree on, when they do -- the push target for
/// refs that declare none themselves (consumers, tags). `None` when the tiers
/// disagree or none declares a remote, and callers must then degrade to words:
/// inventing a remote name would print a command that pushes the release
/// somewhere nobody fetches from. Public because every push-shaped HINT in
/// the tool (`tag`'s post-cut line, `pin`'s unpushed-tag refusal, the release
/// checklist) must derive from the same rule or they drift the day the
/// remote is not called `codeberg`.
pub fn tiers_single_remote(m: &Manifest) -> Option<&str> {
    let mut remotes: Vec<&str> = m.tier.iter().filter_map(|t| t.remote.as_deref()).collect();
    remotes.sort_unstable();
    remotes.dedup();
    match remotes.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// Derive the push list. Read-only by signature.
pub fn plan_push(
    m: &Manifest,
    p: &Policy,
    wine: &GitCli,
    superproject: &GitCli,
    wine_path: &str,
) -> anyhow::Result<PushReport> {
    let mut items = Vec::new();
    let mut notes = Vec::new();

    // The push remote for things that do not declare one (consumers, tags):
    // the remote the tiers agree on (`tiers_single_remote` -- the shared
    // derive-never-invent rule). Disagreement or absence degrades to a note.
    let default_remote = tiers_single_remote(m);

    // ---- wine branches: tiers then consumers, grouped per remote. ----------
    //
    // `(remote, force) -> [branch]`; tiers and consumers are kept as separate
    // commands purely for the reader (the doc's example does the same).
    let push_branch = |name: &str,
                           remote: Option<&str>,
                           bucket: &mut BTreeMap<String, Vec<String>>,
                           notes: &mut Vec<String>|
     -> anyhow::Result<()> {
        if wine.rev_parse(name)?.is_none() {
            notes.push(format!("`{name}` does not exist -- nothing to push (see `sodactl check`)"));
            return Ok(());
        }
        let Some(r) = remote else {
            notes.push(format!(
                "`{name}` declares no remote and the tiers name no single one -- push by hand if \
                 it should publish"
            ));
            return Ok(());
        };
        let tracking = format!("{r}/{name}");
        match wine.ahead_behind(&tracking, name)? {
            Some(ab) if ab.ahead == 0 && ab.behind == 0 => {}
            Some(ab) if ab.ahead == 0 => notes.push(format!(
                "`{name}` is {} behind {tracking} with nothing local -- fetch, don't push",
                ab.behind
            )),
            Some(ab) => bucket.entry(r.to_string()).or_default().push(format!(
                "{name} ({} ahead{})",
                ab.ahead,
                if ab.behind > 0 { format!(", {} behind: rewritten", ab.behind) } else { String::new() }
            )),
            // No tracking ref: never pushed here (or the ref was pruned).
            // Either way the remote lacks it, which is exactly "differs".
            None => bucket
                .entry(r.to_string())
                .or_default()
                .push(format!("{name} (no tracking ref -- first push)")),
        }
        Ok(())
    };

    let mut tiers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in &m.tier {
        push_branch(&t.name, t.remote.as_deref(), &mut tiers, &mut notes)?;
    }
    let mut consumers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in &m.consumer {
        // A consumer declares no remote; the tracking ref that exists is the
        // evidence of where it publishes. Falling back to the tiers' remote
        // covers the never-pushed consumer.
        let found = if wine.rev_parse(&c.name)?.is_none() {
            None
        } else {
            let mut candidates: Vec<&str> = default_remote.into_iter().collect();
            candidates.extend(m.remotes.keys().map(String::as_str));
            candidates.iter().find(|r| {
                matches!(wine.rev_parse(&format!("{r}/{}", c.name)), Ok(Some(_)))
            }).copied().or(default_remote)
        };
        push_branch(&c.name, found, &mut consumers, &mut notes)?;
    }
    let mut emit = |bucket: BTreeMap<String, Vec<String>>| {
        for (r, entries) in bucket {
            let names: Vec<&str> =
                entries.iter().map(|e| e.split(' ').next().unwrap_or(e)).collect();
            items.push(PushItem {
                repo: RepoSide::Wine,
                command: format!(
                    "git -C {wine_path} push --force-with-lease {r} {}",
                    names.join(" ")
                ),
                reason: entries.join("; "),
            });
        }
    };
    emit(tiers);
    emit(consumers);

    // ---- wine tags. --------------------------------------------------------
    //
    // Candidates: OUR tags naming a consumer tip or the superproject pin --
    // the ones whose absence on the remote breaks a fetch or a release.
    // Probed with ls-remote because tags have no tracking refs; a differing
    // remote tag is a NOTE, never a push command -- a published tag is never
    // moved from here.
    let mut consumer_tips = Vec::new();
    for c in &m.consumer {
        if let Some(tip) = wine.rev_parse(&c.name)? {
            consumer_tips.push(tip);
        }
    }
    let pin = superproject
        .rev_parse_raw(&format!("HEAD:{wine_path}"))
        .ok()
        .flatten();
    let candidates: Vec<(String, String)> = wine
        .tags_with_targets()?
        .into_iter()
        .filter(|(n, o)| {
            !matches!(TagName::parse(n, &p.tag_aliases), TagName::Foreign)
                && (consumer_tips.iter().any(|t| t == o) || pin.as_deref() == Some(o.as_str()))
        })
        .collect();
    match default_remote {
        None if !candidates.is_empty() => notes.push(
            "tags could not be checked: the tiers name no single remote to probe".to_string(),
        ),
        _ => {
            for (tag, local) in &candidates {
                let Some(r) = default_remote else { break };
                match wine.ls_remote_tag(r, tag) {
                    Ok(None) => items.push(PushItem {
                        repo: RepoSide::Wine,
                        command: format!("git -C {wine_path} push {r} {tag}"),
                        reason: format!("`{tag}` ({}) is not on {r}", short(local)),
                    }),
                    Ok(Some(remote)) if &remote != local => notes.push(format!(
                        "`{tag}` differs on {r} ({} there, {} here) -- never move a published \
                         tag; sort out which side is right by hand",
                        short(&remote),
                        short(local)
                    )),
                    Ok(Some(_)) => {}
                    Err(e) => notes.push(format!(
                        "`{tag}`: {r} could not be asked ({e}) -- presence unverified, push it \
                         by hand if it is new"
                    )),
                }
            }
        }
    }

    // ---- superproject: the branch, then release tags. ----------------------
    match superproject.head_branch()? {
        None => notes.push("the superproject HEAD is detached -- nothing derivable to push".into()),
        Some(branch) => {
            // The remote name comes from the tracking refs that exist -- the
            // superproject's remotes are its own, not the manifest's (those
            // name WINE remotes).
            let tracking: Vec<(String, String)> = superproject
                .refs_under("refs/remotes/")?
                .into_iter()
                .filter_map(|(name, oid)| {
                    name.strip_prefix("refs/remotes/")
                        .and_then(|r| r.strip_suffix(&format!("/{branch}")))
                        .map(|r| (r.to_string(), oid))
                })
                .collect();
            match tracking.first() {
                None => notes.push(format!(
                    "superproject branch `{branch}` has no tracking ref -- push by hand if it \
                     should publish"
                )),
                Some((r, _)) => {
                    match superproject.ahead_behind(&format!("{r}/{branch}"), &branch)? {
                        Some(ab) if ab.ahead > 0 && ab.behind == 0 => items.push(PushItem {
                            repo: RepoSide::Superproject,
                            command: format!("git push {r} {branch}"),
                            reason: format!("{} commit(s) ahead of {r}/{branch}", ab.ahead),
                        }),
                        Some(ab) if ab.ahead > 0 => notes.push(format!(
                            "superproject `{branch}` diverged from {r}/{branch} ({} ahead, {} \
                             behind) -- the superproject is never force-pushed from a printed \
                             list; reconcile by hand",
                            ab.ahead, ab.behind
                        )),
                        _ => {}
                    }
                    // Release tags: the newest Family tag per component (older
                    // ones shipped long ago; probing all of history is noise).
                    let mut newest: BTreeMap<String, ((u32, u32, u32), String, String)> =
                        BTreeMap::new();
                    for (name, oid) in superproject.tags_with_targets()? {
                        let parsed = TagName::parse(&name, &p.tag_aliases);
                        if let (Some(c), Some(k)) = (parsed.component(), parsed.release_key()) {
                            let e = newest.entry(c.to_string()).or_insert((k, name.clone(), oid.clone()));
                            if k > e.0 {
                                *e = (k, name, oid);
                            }
                        }
                    }
                    for (_, (_, tag, local)) in newest {
                        match superproject.ls_remote_tag(r, &tag) {
                            Ok(None) => items.push(PushItem {
                                repo: RepoSide::Superproject,
                                command: format!("git push {r} {tag}"),
                                reason: format!("`{tag}` ({}) is not on {r}", short(&local)),
                            }),
                            Ok(Some(remote)) if remote != local => notes.push(format!(
                                "superproject tag `{tag}` differs on {r} -- never move a \
                                 published tag; sort it out by hand"
                            )),
                            Ok(Some(_)) => {}
                            Err(e) => notes.push(format!(
                                "superproject tag `{tag}`: {r} could not be asked ({e}) -- \
                                 presence unverified"
                            )),
                        }
                    }
                }
            }
        }
    }

    Ok(PushReport { items, notes })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// The no-push promise, this module's half: `plan_push` PRINTS and its
    /// signature carries only read handles -- the mutating type's identifier
    /// must never appear in this file's production code. Fragment-built so the
    /// test does not match its own source (the shared guardrail rationale).
    #[test]
    fn pushprint_never_touches_the_mutating_handle() {
        let ident = "Git".to_string() + "Mut";
        let cfg_test = "#[cfg(te".to_string() + "st)]";
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/verbs/pushprint.rs"),
        )
        .unwrap();
        assert!(
            text.lines().any(|l| l.trim() == cfg_test),
            "the test-code cutoff line vanished -- the scan below just became a no-op"
        );
        let offenders: Vec<usize> = text
            .lines()
            .enumerate()
            .take_while(|(_, l)| l.trim() != cfg_test)
            .filter(|(_, l)| !l.trim_start().starts_with("//") && l.contains(&ident))
            .map(|(n, _)| n + 1)
            .collect();
        assert!(
            offenders.is_empty(),
            "`push --print` must stay print-only; the mutating handle appears at lines \
             {offenders:?}"
        );
    }
}
