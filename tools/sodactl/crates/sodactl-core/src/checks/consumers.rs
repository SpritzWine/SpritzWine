//! Consumer checks -- and the module where "I cannot tell" is most of the job.
//!
//! Read this before adding anything here.
//!
//! A consumer is produced by `rebase --onto <wine-base> <home-base> <consumer>`.
//! Ancestry is useless against it (new SHAs). Patch-id is the right primitive
//! for a **stitch** -- a cherry-pick onto the SAME base, where an unconflicted
//! commit's diff is byte-identical and its patch-id survives. It is **not**
//! reliable for a consumer, because a CROSS-BASE replay legitimately alters
//! diffs: conflict resolutions, context lines upstream changed, and
//! `binary_resolution` rules that delete a path outright.
//!
//! Measured on the live repo: `git cherry <spritzwine> soda-full <home-base>`
//! reports 3 of 7 commits as absent by patch-id, on a consumer that is by every
//! other measure freshly and correctly poured. A naive `missing > 0 => stale`
//! rule would call it stale with total confidence -- the same false-positive
//! shape as the original `merge-base --is-ancestor` bug, one level down.
//!
//! So: patch-id equality is **sufficient for presence** and **not necessary for
//! currency**. Every inference below runs in that one direction only.

use super::{own_limit, GitRead, Policy};
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

pub fn check(m: &Manifest, git: &dyn GitRead, p: &Policy) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();
    let home = &m.repo.home_base.commit;

    // The gen/<consumer>/ provenance tags, peeled, read ONCE -- and only when
    // a ledger exists at all. With no ledger this check must stay
    // byte-identical (and call-identical) to its pre-ledger self: that is the
    // hard gate that lets the ledger land without re-blessing anything.
    let gen_tags: Vec<(String, String)> =
        if m.ledger.is_empty() { Vec::new() } else { git.tags_with_targets()? };

    for c in &m.consumer {
        let head = git.rev_parse(&c.name)?;

        // F4 -- the manifest declares bases as literal strings that `regen`
        // hands straight to `rebase --onto`. If one does not resolve, regen
        // fails PARTWAY -- after `checkout -B` has already repointed the
        // consumer branch. Catching it read-only is the whole value.
        let base = git.rev_parse(&c.base)?;
        if base.is_none() {
            f.push(Finding::new(
                "CONSUMER_BASE_UNRESOLVABLE",
                Severity::Blocker,
                &c.name,
                format!(
                    "`{}` declares base `{}`, which does not resolve in this clone -- a pour would fail after `checkout -B` has already moved the branch",
                    c.name, c.base
                ),
                // NOT a bare `git fetch <remote>`. That was the remedy, and it
                // can never fix the live instance: wine releases are TAGS, and
                // `remote.winehq.fetch` is `+refs/heads/*:refs/remotes/winehq/*`,
                // so no amount of fetching ever creates
                // `refs/remotes/winehq/wine-11.13`. `spritzwine-11.13-3` was in
                // fact poured onto the tag `wine-11.13`. A remedy you can run
                // forever without progress is worse than none.
                format!(
                    "the base may be a TAG, not a branch: check `git -C wine rev-parse {b}` vs `{tail}`, then either fix `base =` in sodactl.toml or run `git -C wine fetch {r} --tags`",
                    b = c.base,
                    tail = c.base.rsplit('/').next().unwrap_or(&c.base),
                    r = c.base.split('/').next().unwrap_or("<remote>")
                ),
            ));
        }

        // F1 -- currency. With no ledger entry it is not derivable, and the
        // message has to say WHY or the next reader will "fix" it with a
        // patch-id comparison and reintroduce the false positive above. With
        // an entry, the recorded inputs and result make it DECIDABLE: the
        // upgrade the ledger exists for.
        match m.ledger.get(&c.name) {
            // HYGIENE, not Drift. Drift is defined as "the graph is
            // inconsistent; a regeneration would produce stale output" -- an
            // unknown is neither. It fires unconditionally, once per
            // ledger-less consumer; at Drift it made `check` unable to ever
            // exit 0 on this repo. A gate that is permanently red is a gate
            // everyone learns to ignore, which would take the real Blockers
            // down with it.
            None => f.push(Finding::new(
                "CONSUMER_CURRENCY_UNKNOWN",
                Severity::Hygiene,
                &c.name,
                format!(
                    "no generation ledger for `{}` -- a consumer is a cross-base replay, so patch-id equivalence against `{}` is not evidence of staleness and currency is not answerable",
                    c.name, c.soda_tip
                ),
                format!("sodactl regen {} --apply   # the pour writes the [ledger.{}] entry that makes currency decidable", c.name, c.name),
            )),
            Some(g) => {
                // Belt and braces first: the provenance tag mirrors the ledger
                // entry, and if the newest one names a different result, one
                // of the two records was moved or hand-edited -- trust
                // NEITHER, and say so before any inference built on `g`.
                let prefix = format!("gen/{}/", c.name);
                let newest = gen_tags
                    .iter()
                    .filter(|(n, _)| n.starts_with(&prefix))
                    // The UTC stamp orders lexicographically by construction.
                    .max_by(|a, b| a.0.cmp(&b.0));
                if let Some((tag, target)) = newest {
                    if target != &g.result {
                        f.push(Finding::new(
                            "LEDGER_CONTRADICTS_TAG",
                            Severity::Drift,
                            &c.name,
                            format!(
                                "the ledger records `{}` poured to {} but the newest provenance tag `{tag}` names {} -- the two records of the same pour disagree, so neither can be trusted",
                                c.name, short(&g.result), short(target)
                            ),
                            format!("verify the branch by hand (git -C wine log {}), then `sodactl regen {} --apply` to re-establish a consistent ledger + tag pair", c.name, c.name),
                        ));
                    }
                }

                match &head {
                    // A ledger for a branch that does not resolve: the claim
                    // and the repo disagree at the coarsest possible level.
                    None => f.push(Finding::new(
                        "LEDGER_CONTRADICTS_TIP",
                        Severity::Drift,
                        &c.name,
                        format!(
                            "the ledger records `{}` poured to {} but the branch does not resolve at all",
                            c.name, short(&g.result)
                        ),
                        format!("sodactl regen {} --apply   # re-pour (recreates the branch and rewrites the ledger)", c.name),
                    )),
                    Some(tip) if tip != &g.result => {
                        // Report WHICH WAY the disagreement runs -- the two
                        // causes want opposite repairs, and a message that
                        // cannot tell them apart sends someone to the wrong
                        // one.
                        let way = match git.rev_parse(&g.result)? {
                            None => "and the recorded result commit no longer exists in this clone",
                            Some(_) if git.is_ancestor(&g.result, &c.name)? =>
                                "-- commits were made on the consumer after the recorded pour",
                            Some(_) =>
                                "-- the branch was re-poured or force-moved without a ledger write",
                        };
                        f.push(Finding::new(
                            "LEDGER_CONTRADICTS_TIP",
                            Severity::Drift,
                            &c.name,
                            format!(
                                "the ledger records `{}` poured to {} but the tip is {} {way}",
                                c.name, short(&g.result), short(tip)
                            ),
                            format!("if the extra commits are wanted, move them to a soda tier and re-pour; either way `sodactl regen {} --apply` restores the generated-never-authored invariant", c.name),
                        ));
                    }
                    Some(_) => {
                        // Result matches the tip, so the recorded INPUTS are
                        // meaningful: any that moved since the pour means the
                        // consumer no longer reflects what it would be poured
                        // from today. This is the one staleness verdict a
                        // cross-base replay supports (module header) -- and it
                        // needs no patch-id at all.
                        let mut moved = Vec::new();
                        if let Some(b) = &base {
                            if b != &g.base_commit {
                                moved.push(format!(
                                    "base `{}` ({} -> {})",
                                    c.base,
                                    short(&g.base_commit),
                                    short(b)
                                ));
                            }
                        }
                        if let Some(s) = git.rev_parse(&c.soda_tip)? {
                            if s != g.soda_commit {
                                moved.push(format!(
                                    "soda tip `{}` ({} -> {})",
                                    c.soda_tip,
                                    short(&g.soda_commit),
                                    short(&s)
                                ));
                            }
                        }
                        if home != &g.home_base {
                            moved.push(format!(
                                "home base ({} -> {})",
                                short(&g.home_base),
                                short(home)
                            ));
                        }
                        if !moved.is_empty() {
                            f.push(Finding::new(
                                "CONSUMER_STALE_BY_LEDGER",
                                Severity::Drift,
                                &c.name,
                                format!(
                                    "`{}` was poured from inputs that have since moved: {}",
                                    c.name,
                                    moved.join(", ")
                                ),
                                format!("sodactl regen {} --apply   # re-pour from the current inputs (rewrites the ledger entry)", c.name),
                            ));
                        }
                    }
                }
            }
        }

        let (Some(_head), Some(_base)) = (&head, &base) else { continue };

        // F5 -- ancestry IS valid for this one question: the replay's base is a
        // genuine ancestor of the result even though the replayed commits are
        // not.
        if !git.is_ancestor(&c.base, &c.name)? {
            f.push(Finding::new(
                "CONSUMER_NOT_ON_BASE",
                Severity::Blocker,
                &c.name,
                format!(
                    "`{}` does not descend from its declared base `{}` -- it was poured onto something else",
                    c.name, c.base
                ),
                format!("sodactl regen {} --apply   # re-pour onto the declared base", c.name),
            ));
        }

        // F3 -- BRANCHING.md: soda-fsync in a Valve-based chain is "a conflict
        // looking for a place to happen" because proton_11.0 already carries
        // fsync. Two signals, for the reason stated at the top of this file:
        // patch-id proves presence, and a subject-prefix scan catches a commit
        // whose diff the replay legitimately changed.
        let excluded = p
            .consumer_excludes
            .iter()
            .find(|(n, _)| *n == c.name)
            .map(|(_, v)| v.as_slice())
            .unwrap_or_default();
        let subjects = git.log_subjects(&format!("{}..{}", c.base, c.name))?;

        for tier in excluded {
            if git.rev_parse(tier)?.is_none() {
                continue;
            }
            // A tier's OWN commits, not everything it inherits.
            //
            // Measured wrong first: limiting at the home base makes the range
            // for `soda-fsync` include soda-build's and soda-core's commits,
            // which every consumer legitimately carries -- so "some commit
            // matched by patch-id" fired an exclusion Blocker on a correctly
            // poured spritzvalve. The question is only ever about the commits
            // the excluded tier itself introduces.
            let Some(limit) = own_limit(m, tier, home) else { continue };
            let own = own_commits(git, &limit, tier)?;
            if own.is_empty() {
                continue;
            }
            // Set membership, not `missing.len() < total`. Two separate ways the
            // count comparison fabricated this Blocker:
            //
            //   * `total` came from `log_subjects` (which counts merges) while
            //     `missing` comes from `git cherry` (which is `--no-merges`), so
            //     a single merge anywhere in the tier made `missing.len() <
            //     total` unconditionally true. `TIER_NOT_LINEAR` reports merges;
            //     it does not stop this comparison from running.
            //   * even without merges, a count says nothing about WHICH commit
            //     matched. The upstream patch-id set here is the whole
            //     `limit..<consumer>` range -- 1194 unrelated Valve commits for
            //     spritzvalve -- so one coincidence flipped a Blocker on.
            //     Sharpest for `soda-fsync`, whose own description is "vendor
            //     Valve's futex inproc-sync backend": it is derived from the very
            //     history it is compared against.
            let missing = git.missing_by_patch_id(&c.name, tier, &limit)?;
            let present_by_patch = own.iter().any(|o| !missing.contains(o));
            let prefix = format!("{tier}:");
            let present_by_subject = subjects.iter().any(|(_, s)| s.starts_with(&prefix));

            if present_by_patch || present_by_subject {
                f.push(Finding::new(
                    "CONSUMER_EXCLUDE_VIOLATION",
                    Severity::Blocker,
                    &c.name,
                    format!(
                        "`{}` carries commits from `{tier}`, which is excluded from this chain (present by {})",
                        c.name,
                        match (present_by_patch, present_by_subject) {
                            (true, true) => "patch-id and subject",
                            (true, false) => "patch-id",
                            _ => "subject",
                        }
                    ),
                    format!("sodactl regen {} --apply   # re-pour without `{tier}`", c.name),
                ));
            }
        }

        // F2 -- the ONE sound staleness inference available without a ledger,
        // and it is deliberately hard to trigger. `missing > 0` alone means
        // nothing (see the module header). Only when a tier's commits are
        // absent by patch-id AND no commit in the replay range carries that
        // tier's subject prefix is the tier genuinely not there -- that
        // combination is very difficult to produce by legitimate conflict
        // resolution, which changes diffs but not subjects.
        for tier in tiers_of(m, &c.soda_tip) {
            if excluded.contains(&tier.to_string()) {
                continue;
            }
            if git.rev_parse(tier)?.is_none() {
                continue;
            }
            // Same range discipline as the exclusion check above: a tier's own
            // commits. Inherited ones prove nothing about this tier.
            let Some(limit) = own_limit(m, tier, home) else { continue };
            let own = own_commits(git, &limit, tier)?;
            if own.is_empty() {
                continue;
            }
            // Same set-membership discipline as the exclusion check above, for
            // the same two reasons. Here the count comparison was biased toward
            // SILENCE rather than a false Blocker, so it hid missing tiers
            // instead of inventing them -- still wrong, just quietly.
            let missing = git.missing_by_patch_id(&c.name, tier, &limit)?;
            if own.iter().any(|o| !missing.contains(o)) {
                continue; // something matched by patch-id: present, high confidence.
            }
            let prefix = format!("{tier}:");
            if subjects.iter().any(|(_, s)| s.starts_with(&prefix)) {
                continue; // inconclusive-but-present; NOT a finding.
            }
            f.push(Finding::new(
                "CONSUMER_MISSING_TIER",
                Severity::Drift,
                &c.name,
                format!(
                    "`{}` carries nothing from `{tier}`: no commit matches by patch-id and none carries the `{tier}:` subject prefix",
                    c.name
                ),
                format!("sodactl regen {} --apply   # re-pour from `{}`", c.name, c.soda_tip),
            ));
        }
    }
    Ok(f)
}

/// A tier's own NON-MERGE commits, `limit..tier`.
///
/// Merges are excluded because every comparison these feed is against
/// `git cherry`, which is `--no-merges`. A merge commit therefore can never
/// appear in a `missing` set, and including it here would make it read as
/// "present by patch-id" for free.
fn own_commits(git: &dyn GitRead, limit: &str, tier: &str) -> anyhow::Result<Vec<String>> {
    let range = format!("{limit}..{tier}");
    let merges = git.merges_in(&range)?;
    Ok(git
        .log_subjects(&range)?
        .into_iter()
        .map(|(o, _)| o)
        .filter(|o| !merges.contains(o))
        .collect())
}

/// The tiers a soda tip stands for: a stitch expands to its base tier plus its
/// declared order; a tier stands for itself. Anything unknown expands to
/// nothing, so an unrecognised tip produces silence rather than invented tiers.
fn tiers_of<'a>(m: &'a Manifest, tip: &'a str) -> Vec<&'a str> {
    if let Some(s) = m.stitch.iter().find(|s| s.name == tip) {
        let mut v = vec![s.base_tier.as_str()];
        v.extend(s.order.iter().map(String::as_str));
        return v;
    }
    if m.tier.iter().any(|t| t.name == tip) {
        return vec![tip];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{AheadBehind, Oid, StatusEntry};
    use sodactl_git::{GitError, Reapply, Result};
    use std::path::PathBuf;

    const MANIFEST: &str = r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "home"
describes = "d"
derivation = "x"

[[tier]]
name = "soda-core"
parent = "$home_base"

[[tier]]
name = "soda-fsync"
parent = "soda-core"

[[consumer]]
name = "spritzvalve"
base = "valve"
soda_tip = "soda-core"
"#;

    /// Purpose-built rather than an extension of `FakeGit`: these tests need to
    /// state, per range, exactly what `git log` and `git cherry` disagree
    /// about -- which is the whole subject of the regression below.
    struct G {
        /// `range -> [(oid, subject)]`, as `git log` would list it (merges included).
        log: Vec<(String, Vec<(String, String)>)>,
        /// `range -> [oid]` that are merges.
        merges: Vec<(String, Vec<String>)>,
        /// Oids `git cherry` reports as absent upstream.
        missing: Vec<String>,
        /// `(short refname, peeled target)` -- the provenance-tag surface.
        tags: Vec<(String, String)>,
    }

    impl GitRead for G {
        fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
            Ok(Some(rev.to_string()))
        }
        fn log_subjects(&self, range: &str) -> Result<Vec<(Oid, String)>> {
            Ok(self.log.iter().find(|(r, _)| r == range).map(|(_, v)| v.clone()).unwrap_or_default())
        }
        fn merges_in(&self, range: &str) -> Result<Vec<Oid>> {
            Ok(self.merges.iter().find(|(r, _)| r == range).map(|(_, v)| v.clone()).unwrap_or_default())
        }
        fn missing_by_patch_id(&self, _: &str, _: &str, _: &str) -> Result<Vec<Oid>> {
            Ok(self.missing.clone())
        }
        fn is_ancestor(&self, _: &str, _: &str) -> Result<bool> {
            Ok(true)
        }
        fn rev_parse_raw(&self, r: &str) -> Result<Option<Oid>> {
            self.rev_parse(r)
        }
        fn tags_pointing_at(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
            Ok(self.tags.clone())
        }
        fn branches_containing(&self, _: &str, _: bool) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn submodule_index_oid(&self, _: &str) -> Result<Option<Oid>> {
            Ok(None)
        }
        fn config_file_get(&self, _: &str, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        // Not modelled -> an error, never a benign default.
        fn git_dir(&self) -> Result<PathBuf> {
            Err(GitError::Unsupported { method: "git_dir" })
        }
        fn ahead_behind(&self, _: &str, _: &str) -> Result<Option<AheadBehind>> {
            Err(GitError::Unsupported { method: "ahead_behind" })
        }
        fn status(&self, _: bool) -> Result<Vec<StatusEntry>> {
            Err(GitError::Unsupported { method: "status" })
        }
        fn head_branch(&self) -> Result<Option<String>> {
            Err(GitError::Unsupported { method: "head_branch" })
        }
        fn reapply_verdict(&self, _: &str, _: &str) -> Result<Reapply> {
            Err(GitError::Unsupported { method: "reapply_verdict" })
        }
        fn count_commits(&self, _: &str) -> Result<u32> {
            Err(GitError::Unsupported { method: "count_commits" })
        }
        fn local_branches(&self) -> Result<Vec<String>> {
            Err(GitError::Unsupported { method: "local_branches" })
        }
        fn tags(&self) -> Result<Vec<String>> {
            Err(GitError::Unsupported { method: "tags" })
        }
    }

    fn policy() -> Policy {
        Policy {
            consumer_excludes: vec![("spritzvalve".into(), vec!["soda-fsync".into()])],
            ..Policy::default()
        }
    }

    fn empty_g() -> G {
        G { log: vec![], merges: vec![], missing: vec![], tags: vec![] }
    }

    /// The identity fake resolves every rev to itself, so a ledger whose
    /// recorded values ARE the manifest's ref names reads as "nothing moved".
    fn ledger_manifest(base_commit: &str, soda_commit: &str, home: &str, result: &str) -> Manifest {
        Manifest::parse(&format!(
            "{MANIFEST}\n[ledger.spritzvalve]\nat = \"2026-07-24T00:00:00Z\"\nbase_commit = \"{base_commit}\"\nsoda_commit = \"{soda_commit}\"\nhome_base = \"{home}\"\nresult = \"{result}\"\nsodactl = \"0.1.0\"\n"
        ))
        .unwrap()
    }

    fn codes(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|x| x.code).collect()
    }

    /// The hard gate's check-side half: with NO ledger the finding set is
    /// exactly what it was before the ledger existed -- the unknown fires,
    /// none of the ledger codes can.
    #[test]
    fn without_a_ledger_currency_stays_unknown_and_nothing_new_fires() {
        let m = Manifest::parse(MANIFEST).unwrap();
        let f = check(&m, &empty_g(), &policy()).unwrap();
        let c = codes(&f);
        assert!(c.contains(&"CONSUMER_CURRENCY_UNKNOWN"), "{c:?}");
        for new in ["CONSUMER_STALE_BY_LEDGER", "LEDGER_CONTRADICTS_TIP", "LEDGER_CONTRADICTS_TAG"]
        {
            assert!(!c.contains(&new), "{new} must need a ledger: {c:?}");
        }
    }

    /// A ledger whose result is the tip and whose inputs are current: the
    /// question is DECIDED, so neither the unknown nor any drift fires.
    #[test]
    fn a_current_ledger_silences_the_unknown_without_replacing_it() {
        let m = ledger_manifest("valve", "soda-core", "home", "spritzvalve");
        let f = check(&m, &empty_g(), &policy()).unwrap();
        let c = codes(&f);
        for gone in [
            "CONSUMER_CURRENCY_UNKNOWN",
            "CONSUMER_STALE_BY_LEDGER",
            "LEDGER_CONTRADICTS_TIP",
            "LEDGER_CONTRADICTS_TAG",
        ] {
            assert!(!c.contains(&gone), "{gone} must not fire when current: {c:?}");
        }
    }

    /// An input that moved since the pour -- the staleness verdict the whole
    /// ledger exists to make possible, with the moved input NAMED.
    #[test]
    fn a_moved_input_is_stale_by_ledger() {
        let m = ledger_manifest("valve", "old-soda-tip", "home", "spritzvalve");
        let f = check(&m, &empty_g(), &policy()).unwrap();
        let hit = f
            .iter()
            .find(|x| x.code == "CONSUMER_STALE_BY_LEDGER")
            .unwrap_or_else(|| panic!("{:?}", codes(&f)));
        assert_eq!(hit.severity, Severity::Drift);
        assert!(hit.summary.contains("soda tip `soda-core`"), "{}", hit.summary);
        assert!(hit.remedy.contains("sodactl regen spritzvalve --apply"), "{}", hit.remedy);
        assert!(!codes(&f).contains(&"CONSUMER_CURRENCY_UNKNOWN"));
    }

    /// The recorded result is not the tip: the ledger and the repo disagree
    /// about the pour itself -- and the identity fake's is_ancestor answers
    /// true, so the message must take the commits-were-added direction.
    #[test]
    fn a_result_that_is_not_the_tip_contradicts() {
        let m = ledger_manifest("valve", "soda-core", "home", "some-older-pour");
        let f = check(&m, &empty_g(), &policy()).unwrap();
        let hit = f
            .iter()
            .find(|x| x.code == "LEDGER_CONTRADICTS_TIP")
            .unwrap_or_else(|| panic!("{:?}", codes(&f)));
        assert!(hit.summary.contains("commits were made on the consumer"), "{}", hit.summary);
        // A wrong result must not ALSO be measured for input staleness --
        // inferences on top of a contradicted record are noise.
        assert!(!codes(&f).contains(&"CONSUMER_STALE_BY_LEDGER"), "{:?}", codes(&f));
    }

    /// The newest provenance tag names a different result than the ledger:
    /// trust neither. Older tags are history, not contradiction.
    #[test]
    fn a_disagreeing_provenance_tag_condemns_both_records() {
        let mut g = empty_g();
        g.tags = vec![
            // Older tag at some historic pour: fine.
            ("gen/spritzvalve/20260101T000000Z".into(), "ancient".into()),
            // Newest tag disagrees with the ledger's result.
            ("gen/spritzvalve/20260701T000000Z".into(), "elsewhere".into()),
            // A different consumer's tag must not bleed in.
            ("gen/other/20260722T000000Z".into(), "spritzvalve".into()),
        ];
        let m = ledger_manifest("valve", "soda-core", "home", "spritzvalve");
        let f = check(&m, &g, &policy()).unwrap();
        let hit = f
            .iter()
            .find(|x| x.code == "LEDGER_CONTRADICTS_TAG")
            .unwrap_or_else(|| panic!("{:?}", codes(&f)));
        assert!(hit.summary.contains("gen/spritzvalve/20260701T000000Z"), "{}", hit.summary);
        assert!(hit.summary.contains("neither can be trusted"), "{}", hit.summary);

        // And when the newest tag AGREES, silence -- an older stray does not
        // contradict a record it never mirrored.
        g.tags.pop();
        g.tags[1].1 = "spritzvalve".into();
        let f = check(&m, &g, &policy()).unwrap();
        assert!(!codes(&f).contains(&"LEDGER_CONTRADICTS_TAG"), "{:?}", codes(&f));
    }

    /// THE regression. `soda-fsync` has two own commits, one of them a MERGE,
    /// and the consumer carries neither.
    ///
    /// `git cherry` is `--no-merges`, so it can only ever report the one
    /// non-merge commit -- while `log_subjects` counts both. The old
    /// `missing.len() < total` test therefore read 1 < 2 as "something matched
    /// by patch-id" and raised a Blocker saying the consumer carries an
    /// excluded tier, on a consumer that carries nothing of it at all. Any
    /// single merge in a tier made this unconditional.
    #[test]
    fn a_merge_in_an_excluded_tier_does_not_fabricate_a_violation() {
        let g = G {
            log: vec![(
                "soda-core..soda-fsync".into(),
                vec![("c1".into(), "soda-fsync: real".into()), ("m1".into(), "Merge branch".into())],
            )],
            merges: vec![("soda-core..soda-fsync".into(), vec!["m1".into()])],
            // The one non-merge commit is absent from the consumer: nothing of
            // this tier is present.
            missing: vec!["c1".into()],
            tags: vec![],
        };
        let m = Manifest::parse(MANIFEST).unwrap();
        let f = check(&m, &g, &policy()).unwrap();
        let violations: Vec<&str> =
            f.iter().filter(|x| x.code == "CONSUMER_EXCLUDE_VIOLATION").map(|x| x.code).collect();
        assert!(violations.is_empty(), "fabricated a violation from a merge: {f:#?}");
    }

    /// The converse, so the fix is a correctness change and not a mute button:
    /// when a non-merge commit of the excluded tier IS present by patch-id, the
    /// Blocker still fires.
    #[test]
    fn a_genuinely_present_excluded_commit_still_blocks() {
        let g = G {
            log: vec![(
                "soda-core..soda-fsync".into(),
                vec![("c1".into(), "soda-fsync: real".into()), ("c2".into(), "soda-fsync: more".into())],
            )],
            merges: vec![],
            // c2 is NOT missing -> it is present in the consumer.
            missing: vec!["c1".into()],
            tags: vec![],
        };
        let m = Manifest::parse(MANIFEST).unwrap();
        let f = check(&m, &g, &policy()).unwrap();
        assert!(
            f.iter().any(|x| x.code == "CONSUMER_EXCLUDE_VIOLATION" && x.severity == Severity::Blocker),
            "{f:#?}"
        );
    }
}
