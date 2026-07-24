//! `reconcile(declared, observed) -> Vec<Finding>`.
//!
//! This is the whole product. Everything else is transport. It is a pure
//! function of a manifest and a `GitRead`, which means it is testable against a
//! fake with no repository on disk.

use crate::finding::{ConsumerState, Finding, Report, Severity, TierState};
use crate::manifest::Manifest;
use sodactl_git::{GitRead, Pin, Reapply};

pub fn reconcile<G: GitRead>(m: &Manifest, git: &G) -> anyhow::Result<Report> {
    let mut r = Report::default();

    check_tiers(m, git, &mut r)?;
    check_stitches(m, git, &mut r)?;
    check_consumers(m, git, &mut r)?;
    check_unmodelled(m, git, &mut r)?;

    r.sort();
    Ok(r)
}

/// `reconcile()` plus every check in `crate::checks`.
///
/// Kept as a separate entry point rather than folded into `reconcile()` for one
/// reason: `reconcile()` is a pure function of a manifest and a single
/// `GitRead`, and that is what makes it testable against a fake with no
/// repository on disk. The `checks` module needs BOTH repositories at once (the
/// pin lives in the superproject, the refs it names live in the submodule) and
/// needs the filesystem (rr-cache). Widening `reconcile()`'s signature to carry
/// all of that would cost every existing fake test its ability to answer
/// honestly, so the two compose instead.
///
/// The report is re-sorted after the merge: severity order is a property of the
/// whole finding set, and appending to an already-sorted vec quietly breaks it.
pub fn reconcile_workspace<G: GitRead>(
    m: &Manifest,
    git: &G,
    ws: &crate::checks::Workspace,
    policy: &crate::checks::Policy,
) -> anyhow::Result<Report> {
    let mut r = reconcile(m, git)?;
    let extra = crate::checks::run_all(m, ws, policy)?;

    // The two halves genuinely overlap. `checks::tiers` re-derives the remote
    // comparison `check_tiers` already did (`TIER_DIVERGED`,
    // `TIER_BEHIND_REMOTE`), and `checks::pin` re-reads the same gitlink
    // `check_pin` reads (`PIN_STAGED_UNCOMMITTED` verbatim, `PIN_UNTAGGED` as a
    // restatement of `PINNED_CONSUMER_UNTAGGED`). None of it is wrong -- it is
    // one fact each, rendered twice, and a report that lists one problem twice
    // teaches the reader to discount the severity column.
    //
    // A finding's identity is (code, subject), not code alone: four tiers each
    // emit their own `TIER_DIVERGED` and all four must survive. Deduplicating on
    // the pair drops only the true restatements.
    //
    // reconcile's versions win on the one aliased pair, because their SUBJECT is
    // the consumer at risk rather than the bare submodule path, and the consumer
    // is what the reader has to act on. Note this is conditional on reconcile
    // having actually spoken -- when no consumer declares
    // `pinned_by_superproject` it stays silent and `checks::pin` is the only
    // guard there is. Filtering HERE rather than inside `checks::pin` is what
    // keeps that module unconditionally complete, so its own regression tests
    // (the pair holding down the wrong-subject bug) still reach every code it
    // can emit.
    // `spritzvalve-full` is BRANCHING.md's standing example of a deliberately
    // unpushed local mix ("Consumers you don't see"). Telling the user to add it
    // to the manifest or delete it contradicts the model's own documentation,
    // which is the worst kind of finding: correct-looking, and wrong per the
    // spec it claims to enforce. The declared ignore list is policy, so it is
    // applied here rather than hardcoded in the check.
    r.findings.retain(|f| {
        !(f.code == "UNMODELLED_BRANCH" && policy.ignore_branches.contains(&f.subject))
    });

    let seen: std::collections::BTreeSet<(&str, &str)> =
        r.findings.iter().map(|f| (f.code, f.subject.as_str())).collect();
    // Two pin findings alias across DIFFERENT subjects, so the pair key above
    // cannot see them: reconcile names the consumer (`spritzwine`) where
    // `checks::pin` names the submodule path (`wine`). Same gitlink, same fact.
    //
    // The alias must agree about the OID, not merely about the code. Both
    // emitters render the short gitlink into their summary, and reconcile's
    // `check_pin` reads `staged.or(committed)` exactly as `checks::pin` does --
    // so when they are talking about the same pin, that string appears in both.
    // Keying on code presence alone would suppress a genuine second fact the
    // moment the two ever diverge (a superproject pinning two submodules, say),
    // and a suppressed Blocker is the one failure mode this whole module exists
    // to avoid. If they disagree, both are printed and the reader arbitrates.
    let mentions_same_pin = |a: &Finding, code: &str| {
        r.findings.iter().filter(|g| g.code == code).any(|g| {
            a.summary
                .split_whitespace()
                .any(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()) && g.summary.contains(w))
        })
    };
    let aliased = |f: &Finding| match f.code {
        "PIN_UNTAGGED" => mentions_same_pin(f, "PINNED_CONSUMER_UNTAGGED"),
        "PIN_STAGED_UNCOMMITTED" => mentions_same_pin(f, "PIN_STAGED_UNCOMMITTED"),
        _ => false,
    };
    let extra: Vec<Finding> = extra
        .into_iter()
        .filter(|f| !seen.contains(&(f.code, f.subject.as_str())) && !aliased(f))
        .collect();
    r.findings.extend(extra);

    r.sort();
    Ok(r)
}

/// The ref a `$home_base` placeholder actually names.
fn resolve_parent(m: &Manifest, parent: &str) -> String {
    if parent == "$home_base" {
        m.repo.home_base.commit.clone()
    } else {
        parent.to_string()
    }
}

/// How to name the parent to a human, given the placeholder is not a ref.
///
/// The old code checked against the RESOLVED parent but formatted the RAW one,
/// so a tier whose parent is `$home_base` produced the remedy
/// `git rebase --onto $home_base <old-$home_base-tip> soda-build`. Pasted into a
/// shell that expands to `git rebase --onto "" "" soda-build`, run against the
/// one tier the docs say must never move. A remedy that is copy-pasteable and
/// wrong is worse than no remedy at all, so the placeholder never reaches the
/// output: commands get the resolved commit, prose gets commit + `describes`.
fn parent_label(m: &Manifest, parent: &str, parent_ref: &str) -> String {
    if parent == "$home_base" {
        format!("$home_base ({} = {})", m.repo.home_base.describes, short(parent_ref))
    } else {
        format!("`{parent}`")
    }
}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

fn check_tiers<G: GitRead>(m: &Manifest, git: &G, r: &mut Report) -> anyhow::Result<()> {
    for t in &m.tier {
        let oid = git.rev_parse(&t.name)?;
        let parent_ref = resolve_parent(m, &t.parent);

        if oid.is_none() {
            r.findings.push(Finding::new(
                "TIER_MISSING",
                Severity::Blocker,
                &t.name,
                format!("manifest declares tier `{}` but no such branch exists", t.name),
                format!("create it, or remove the [[tier]] entry for `{}`", t.name),
            ));
            r.tiers.push(TierState {
                name: t.name.clone(),
                oid: None,
                commits_on_parent: None,
                parent: t.parent.clone(),
                ahead_of_remote: None,
                behind_remote: None,
                contains_parent_tip: None,
            });
            continue;
        }

        // THE central check: does the child still descend from its parent's tip?
        // When a tier moves, every child silently keeps the old base until it is
        // restacked, and every consumer built from those children ships stale.
        //
        // Ancestry is the RIGHT primitive here, unlike in the stitch and
        // consumer checks: tiers are hand-AUTHORED on their parent, never
        // replayed, so the parent's commits really are ancestors. This is not an
        // instance of the cherry-pick bug class.
        let contains = if git.rev_parse(&parent_ref)?.is_some() {
            Some(git.is_ancestor(&parent_ref, &t.name)?)
        } else {
            None
        };

        if contains == Some(false) {
            r.findings.push(Finding::new(
                "TIER_CHILD_STALE",
                Severity::Drift,
                &t.name,
                format!(
                    "`{}` does not contain the tip of its parent {} -- it was authored on an older parent",
                    t.name,
                    parent_label(m, &t.parent, &parent_ref)
                ),
                // The verb, not the raw rebase: `restack` computes `<old>`
                // (the placeholder here used to invite typing it from memory,
                // the exact commit-duplication trap) and patch-id-verifies the
                // result, restoring from a backup ref on drift.
                format!(
                    "sodactl restack {child} --apply   # computes the old base, verifies by patch-id; then re-stitch and re-pour anything downstream",
                    child = t.name
                ),
            ));
        }

        // Geometry is DERIVED. Never declared.
        let commits = git.count_commits(&format!("{parent_ref}..{}", t.name)).ok();

        let (ahead, behind) = match t.remote.as_deref() {
            Some(remote) => {
                let tracking = format!("{remote}/{}", t.name);
                match git.ahead_behind(&tracking, &t.name)? {
                    Some(ab) => {
                        // `behind` used to be computed, stored, and then never
                        // looked at by any check -- so a tier that had been
                        // restacked (the normal ahead>0 AND behind>0 shape) was
                        // reported as plain "N commits not pushed", and the
                        // printed remedy was a plain `git push` that git rejects
                        // as non-fast-forward. Three situations, three remedies.
                        match (ab.ahead, ab.behind) {
                            (0, 0) => {}
                            // Behind with nothing local is NOT divergence, and
                            // it must not reach the catch-all below. `(0, b)`
                            // failed both of the original arms and landed in
                            // `TIER_DIVERGED`, so one tier produced two
                            // contradictory findings: a Drift telling you to
                            // `push --force-with-lease` (destroying b commits
                            // that are not yours) and a Blocker telling you not
                            // to. `TIER_BEHIND_REMOTE` just below is the only
                            // correct reading of this state. `checks/tiers.rs`
                            // already gated on `ahead > 0 && behind > 0`; this
                            // copy of the logic did not.
                            (0, _) => {}
                            (a, 0) => r.findings.push(Finding::new(
                                "TIER_UNPUSHED",
                                Severity::Hygiene,
                                &t.name,
                                format!("{a} commit(s) not on {tracking}"),
                                format!(
                                    "git -C wine push {remote} {}   # fast-forward; Florian pushes, submodule first",
                                    t.name
                                ),
                            )),
                            (a, b) => r.findings.push(Finding::new(
                                "TIER_DIVERGED",
                                Severity::Drift,
                                &t.name,
                                format!(
                                    "`{}` is {a} ahead and {b} behind {tracking} -- it was rewritten (restacked) locally and the remote still advertises the pre-restack tip",
                                    t.name
                                ),
                                format!(
                                    "git -C wine push --force-with-lease {remote} {}   # a plain push WILL be rejected; confirm the {b} remote commit(s) are yours to drop",
                                    t.name
                                ),
                            )),
                        }
                        if ab.ahead == 0 && ab.behind > 0 {
                            // Nothing local to publish and the remote is ahead:
                            // this is someone else's authored work, and every
                            // pour from here silently omits it. Blocker, and
                            // emphatically NOT a force-push situation.
                            r.findings.push(Finding::new(
                                "TIER_BEHIND_REMOTE",
                                Severity::Blocker,
                                &t.name,
                                format!(
                                    "`{}` is {} commit(s) behind {tracking} with nothing of its own -- every pour from here drops that work",
                                    t.name, ab.behind
                                ),
                                format!(
                                    "git -C wine fetch {remote} && git -C wine merge --ff-only {tracking}   # do NOT force-push over this",
                                ),
                            ));
                        }
                        (Some(ab.ahead), Some(ab.behind))
                    }
                    None => {
                        r.findings.push(Finding::new(
                            "TIER_NO_REMOTE",
                            Severity::Hygiene,
                            &t.name,
                            format!("no tracking ref {tracking} -- drift cannot be detected"),
                            "push it, or mark the tier local-only in the manifest".to_string(),
                        ));
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };

        r.tiers.push(TierState {
            name: t.name.clone(),
            oid,
            commits_on_parent: commits,
            parent: t.parent.clone(),
            ahead_of_remote: ahead,
            behind_remote: behind,
            contains_parent_tip: contains,
        });
    }
    Ok(())
}

fn check_stitches<G: GitRead>(m: &Manifest, git: &G, r: &mut Report) -> anyhow::Result<()> {
    for s in &m.stitch {
        if git.rev_parse(&s.name)?.is_none() {
            r.findings.push(Finding::new(
                "STITCH_MISSING",
                Severity::Drift,
                &s.name,
                format!("stitch `{}` does not exist", s.name),
                // The verb, not the raw commands: `stitch` derives each
                // tier's own range (the same stale-`<old>` trap restack
                // closes) and patch-id-verifies the result.
                format!("sodactl stitch {} --apply   # rebuilds it from `{}` in the declared order, patch-id-verified", s.name, s.base_tier),
            ));
            continue;
        }

        // `git cherry <upstream> <head> <limit>` is only meaningful while
        // `limit` is an ancestor of both sides; otherwise it silently truncates
        // the comparison set and UNDER-reports. Demonstrated on the live repo
        // with refs where the invariant does not hold: the same question
        // answered "1 missing" with the limit and "3 missing" without it, exit 0
        // both times. So establish the precondition instead of assuming it --
        // an answer whose meaning depends on an unchecked invariant is not an
        // answer.
        if !git.is_ancestor(&s.base_tier, &s.name)? {
            r.findings.push(Finding::new(
                "STITCH_REBASED",
                Severity::Drift,
                &s.name,
                format!(
                    "`{}` no longer descends from its base tier `{}` -- patch-id comparison against it would silently under-report missing commits, so staleness was NOT checked",
                    s.name, s.base_tier
                ),
                format!("sodactl stitch {} --apply   # rebuilds it on `{}` from scratch (a stitch is derived; nothing in it is worth salvaging)", s.name, s.base_tier),
            ));
            continue;
        }

        // A stitch is a linearization of tiers, built by CHERRY-PICK -- the
        // tier's own commits are never ancestors of it. Ancestry would give a
        // confident wrong answer; patch-id equivalence is the real question.
        //
        // Patch-id IS sound here, unlike for consumers: a stitch cherry-picks
        // onto the SAME base, so an unconflicted commit's diff is byte-identical
        // and its patch-id survives. A consumer replays across bases, where a
        // changed patch-id means the replay did its job.
        // ...but patch-id is still only the FAST path. It stays context-sensitive
        // even same-base: a cherry-pick that rerere touched, or one whose
        // surrounding lines drifted, hashes differently while being the same
        // change. So every commit patch-id calls missing gets a second, exact
        // opinion from an actual reapply (StGit's primitive). Confirming is
        // cheap -- it runs only over the suspected-missing set, which is empty
        // in the healthy case.
        for tier in &s.order {
            let suspected = git.missing_by_patch_id(&s.name, tier, &s.base_tier)?;
            if suspected.is_empty() {
                continue;
            }

            let mut confirmed = Vec::new();
            let mut unconfirmed = 0usize;
            for c in &suspected {
                match git.reapply_verdict(c, &s.name)? {
                    // patch-id was wrong: reapplying is a no-op, so the change
                    // IS there. Suppressing this is the whole point.
                    Reapply::Present => {}
                    Reapply::Missing => confirmed.push(c.clone()),
                    // A conflicted reapply is not evidence of absence. Keep the
                    // patch-id suspicion -- silently dropping it would hide a
                    // real staleness -- but say the confirmation did not land,
                    // rather than dressing a guess up as a measurement.
                    Reapply::Unknown => {
                        confirmed.push(c.clone());
                        unconfirmed += 1;
                    }
                }
            }

            if !confirmed.is_empty() {
                r.findings.push(Finding::new(
                    "STITCH_STALE",
                    Severity::Drift,
                    &s.name,
                    format!(
                        "`{}` is missing {} commit(s) from `{}` -- it was stitched before that tier moved{}",
                        s.name,
                        confirmed.len(),
                        tier,
                        match unconfirmed {
                            0 => " (confirmed by reapply)".to_string(),
                            n if n == confirmed.len() =>
                                format!(" (patch-id only; {n} reapply check(s) conflicted, so this is unconfirmed)"),
                            n => format!(" ({n} of them unconfirmed -- reapply conflicted)"),
                        }
                    ),
                    format!("sodactl stitch {} --apply   # re-stitches in the declared order; restack any stale tiers first or it linearises stale inputs", s.name),
                ));
            }
        }
        if s.local_only {
            r.findings.push(Finding::new(
                "STITCH_LOCAL_ONLY",
                Severity::Hygiene,
                &s.name,
                format!("`{}` is local-only by declaration -- nothing can detect its drift remotely", s.name),
                "expected for a derived scratch branch; no action".to_string(),
            ));
        }
    }
    Ok(())
}

fn check_consumers<G: GitRead>(m: &Manifest, git: &G, r: &mut Report) -> anyhow::Result<()> {
    // One read for all consumers: the gitlink is a property of the submodule,
    // not of any one branch.
    let pin = git.superproject_pin()?;

    for c in &m.consumer {
        let oid = git.rev_parse(&c.name)?;

        // A consumer is a REPLAY: its commits are cherry-picked copies with new
        // SHAs, so ancestry against the soda tip is meaningless. Without a
        // generation ledger recording the inputs, "is this current?" is not
        // answerable -- and `None` is the honest answer rather than a guess.
        let current = None;

        if oid.is_some() && c.pinned_by_superproject {
            check_pin(c, &pin, git, r)?;
        }

        r.consumers.push(ConsumerState {
            name: c.name.clone(),
            oid,
            base: c.base.clone(),
            soda_tip: c.soda_tip.clone(),
            current,
            pinned_by_superproject: c.pinned_by_superproject,
            pin: c.pinned_by_superproject.then(|| pin.clone()),
        });
    }
    Ok(())
}

/// Consumers are force-pushed. A pinned raw SHA falls out of the branch on the
/// next pour and `git submodule update` dies with "did not contain <sha>". This
/// is the one case where a tag is mandatory outside a release.
///
/// THE SUBJECT IS THE GITLINK, NOT THE BRANCH TIP. The original check resolved
/// the consumer's local branch and asked whether a tag pointed at *that*. But
/// BRANCHING.md states as normal operation that local consumer tips "routinely
/// sit ahead of their tags" -- a pour that has not passed the gate yet. So:
///
///   * false positive -- a correctly-tagged pin plus one fresh local pour raises
///     a Blocker whose remedy tells you to tag untested bits and pin them;
///   * false negative -- the gitlink pins an old untagged SHA while the branch
///     tip happens to carry a tag, so it reports clean and the pin breaks anyway.
///
/// On the day it was written the two commits coincided, so the check passed its
/// own review by coincidence. This is a wrong-subject error, not a wrong-
/// primitive one, and no amount of fixing the tag lookup would have found it.
fn check_pin<G: GitRead>(
    c: &crate::manifest::Consumer,
    pin: &Pin,
    git: &G,
    r: &mut Report,
) -> anyhow::Result<()> {
    let (committed, staged) = match pin {
        Pin::Known { committed, staged } => (committed, staged),
        // "I could not read the gitlink" must never render as a clean pin.
        Pin::Unknown { reason } => {
            r.findings.push(Finding::new(
                "PIN_UNVERIFIABLE",
                Severity::Drift,
                &c.name,
                format!(
                    "`{}` is declared pinned but the superproject's gitlink could not be read ({reason}) -- whether the pin is fetchable is UNKNOWN, not fine",
                    c.name
                ),
                "run sodactl from the superproject checkout so the gitlink is readable".to_string(),
            ));
            return Ok(());
        }
    };

    let Some(oid) = staged.as_deref().or(committed.as_deref()) else {
        r.findings.push(Finding::new(
            "PIN_ABSENT",
            Severity::Blocker,
            &c.name,
            format!("`{}` is declared pinned but the superproject records no gitlink at all", c.name),
            "git add wine   # after checking out a tagged consumer commit".to_string(),
        ));
        return Ok(());
    };

    // A staged bump is what a `git commit` right now would publish. Not broken
    // yet, but one keystroke from it -- and its remedy ("unstage") is one no
    // other finding suggests, which is why it gets its own code.
    if pin.is_staged() {
        r.findings.push(Finding::new(
            "PIN_STAGED_UNCOMMITTED",
            Severity::Drift,
            &c.name,
            format!(
                "the wine gitlink is staged at {} but HEAD records {} -- committing publishes the staged one",
                short(oid),
                committed.as_deref().map(short).unwrap_or_else(|| "nothing".into())
            ),
            "git restore --staged wine   # or tag the new tip and update .gitmodules branch= in the SAME commit".to_string(),
        ));
    }

    // One `git tag --points-at` rather than one `rev-parse` per tag. The old
    // scan spawned a process for each of the repo's 6616 tags (measured 9.7s,
    // 99% of total runtime) and swallowed peel errors with `.ok()`, which biased
    // a failed peel toward "no tag matched" -- the direction that fabricates a
    // Blocker.
    if git.tags_pointing_at(oid)?.is_empty() {
        // Demoted while the judged pin is only STAGED, for the reason spelled
        // out in `checks::pin`: the committed gitlink may be entirely clean, and
        // blocking on a state no CI runner can observe (CI checks out HEAD)
        // means blocking a developer midway through a correct pin bump. The two
        // emitters must agree here or the merge-point alias just swaps one
        // severity for the other and the demotion is cosmetic.
        let uncommitted = pin.is_staged() && staged.as_deref() != committed.as_deref();
        r.findings.push(Finding::new(
            "PINNED_CONSUMER_UNTAGGED",
            if uncommitted { Severity::Drift } else { Severity::Blocker },
            &c.name,
            format!(
                "{}`{}` at {} with no tag pointing there -- consumers are force-pushed, so the next pour orphans this SHA and `git submodule update` fails with \"did not contain\"",
                if uncommitted {
                    "committing the staged gitlink would pin "
                } else {
                    "the superproject pins "
                },
                c.name,
                short(oid)
            ),
            format!("git -C wine tag <component>-<base>-<n> {}   # then pin the TAG", short(oid)),
        ));
    }
    Ok(())
}

fn check_unmodelled<G: GitRead>(m: &Manifest, git: &G, r: &mut Report) -> anyhow::Result<()> {
    let modelled = m.modelled_refs();
    for b in git.local_branches()? {
        // Upstream and vendor branches are not ours to model.
        //
        // The `proton_*` arm fires on nothing in the live repo today --
        // `proton_11.0` lives at `valve-wine/proton_11.0`, a remote-tracking ref
        // this `refs/heads` scan never sees. It is kept anyway: a plain
        // `git checkout proton_11.0` MATERIALISES exactly that local branch, and
        // when it does, the branch is vendor-owned and reporting it as
        // "dead, or live but undocumented?" is a false positive. Unreached
        // today, correct the day it is reached, and free either way.
        if matches!(b.as_str(), "master" | "oldstable" | "stable") || b.starts_with("proton_") {
            continue;
        }
        if !modelled.contains(&b.as_str()) {
            r.findings.push(Finding::new(
                "UNMODELLED_BRANCH",
                Severity::Hygiene,
                &b,
                format!("branch `{b}` is not in the manifest -- dead, or live but undocumented?"),
                "add it to sodactl.toml, or delete it".to_string(),
            ));
        }
    }
    Ok(())
}
