//! Tag-family checks.
//!
//! Scheme: `<component>-<winebase>-<version>`. Everything here filters through
//! `crate::tags::TagName` first, and everything `Foreign` is dropped before a
//! single comparison runs -- see that module for why that is the load-bearing
//! requirement rather than a nicety.

use super::{own_limit, GitRead, Policy, SkewPolicy};
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;
use crate::tags::TagName;
use std::collections::BTreeMap;

pub fn check(m: &Manifest, git: &dyn GitRead, p: &Policy) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();
    let home = &m.repo.home_base.commit;

    // One process for 6600 tags. Peeling annotated tags one `rev-parse` at a
    // time costs ~10s of wall clock here, ~99% of the tool's runtime.
    let all = git.tags_with_targets()?;
    let ours: Vec<(String, String, TagName)> = all
        .into_iter()
        .map(|(n, o)| {
            let p = TagName::parse(&n, &p.tag_aliases);
            (n, o, p)
        })
        .filter(|(_, _, p)| !matches!(p, TagName::Foreign))
        .collect();

    // ---- C1: two family tags of the same component naming one commit. ----
    //
    // Hygiene, not Blocker. BRANCHING.md documents the live instance as
    // INTENDED: "Same commit as `spritzvalve-11.0-1` -- the family rolled, this
    // glass did not need re-pouring." Regeneration and tagging are separate
    // decisions, so a consumer that needs no re-pour legitimately keeps its
    // commit. Shipping this at Blocker would make the tool's very first run a
    // false alarm, which is how a tool gets ignored.
    let mut by_oid: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for (name, oid, parsed) in &ours {
        if let TagName::Family { component, .. } = parsed {
            by_oid.entry((component.as_str(), oid.as_str())).or_default().push(name);
        }
    }
    for ((component, oid), names) in &by_oid {
        if names.len() < 2 || declared_equivalent(p, names) {
            continue;
        }
        f.push(Finding::new(
            "TAG_COLLISION",
            Severity::Hygiene,
            *component,
            format!(
                "`{}` all name {} -- either the family rolled without a re-pour (intended) or a version shipped the previous version's bits",
                names.join("`, `"),
                short(oid)
            ),
            format!(
                "declare the equivalence in sodactl.toml if intended, otherwise re-pour (tools/regenerate.sh) and re-cut the tag: git -C wine tag -f <{component}-<base>-<n>>"
            ),
        ));
    }

    // ---- C2: a family tag naming a soda tree the tiers have moved past. ----
    //
    // Patch-id, not ancestry: the tagged object is a stitch, so tier commits
    // are never its ancestors. Same-base cherry-pick, so patch-id is exact.
    //
    // Restricted to the NEWEST tag of each family, deliberately. A shipped tag
    // is a historical record and is *supposed* to fall behind once the tiers
    // move -- checking all of them means one permanent Drift row per release
    // forever, and a finding that can never be cleared trains the reader to
    // skip the whole severity. Only the newest tag is the one someone is about
    // to ship or re-cut from.
    let newest_soda: Option<(&str, &str, &str)> = ours
        .iter()
        .filter_map(|(n, o, p)| match p {
            // Consumer tags are cross-base replays, where patch-id is not
            // evidence of staleness (see consumers.rs); only soda tags name a
            // same-base stitch and can be compared this way.
            TagName::Family { component, .. } if component == "soda" => {
                // Ordered by (base, version), not version alone -- see
                // `TagName::release_key` for the inversion that causes.
                Some((p.release_key()?, n.as_str(), o.as_str(), component.as_str()))
            }
            _ => None,
        })
        .max_by_key(|(k, _, _, _)| *k)
        .map(|(_, n, o, c)| (n, o, c));

    if let Some((name, oid, component)) = newest_soda {
        let (name, oid) = (&name.to_string(), &oid.to_string());
        let mut stale: Vec<String> = Vec::new();
        // Deduplicated ACROSS tiers. One commit is reachable from every
        // descendant tier, so a per-tier count reports it once per tier:
        // soda-core's single commit rendered as "soda-core (+1),
        // soda-glorious (+1), soda-ntdll (+1), soda-fsync (+1),
        // soda-branding (+1)", which reads as five tiers having moved when
        // exactly one had.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for t in &m.tier {
            if git.rev_parse(&t.name)?.is_none() {
                continue;
            }
            // Only tiers the stitch is supposed to carry.
            if !m.stitch.iter().any(|s| s.base_tier == t.name || s.order.contains(&t.name)) {
                continue;
            }
            // A tier's OWN commits. Limiting at the home base is the mistake
            // `consumers.rs` found and fixed with `own_limit`; this call site
            // never got it, and the inherited commits are exactly what produced
            // the five-for-one count above.
            //
            // The `git cherry` precondition is that `limit` be an ancestor of
            // `head`, so that the head range means something. It does NOT
            // require `limit` to be an ancestor of `upstream` -- verified
            // against the live repo, where `soda-core` is not an ancestor of
            // `soda-11.12-3` and the comparison is still exact -- which is why
            // this can compare against a tag naming a stitch at all. A tier
            // failing the precondition is already reported by
            // `TIER_CHILD_STALE`, so skipping is deferring to a check that
            // states the fact, not swallowing an answer.
            let Some(limit) = own_limit(m, &t.name, home) else { continue };
            if git.rev_parse(&limit)?.is_none() || !git.is_ancestor(&limit, &t.name)? {
                continue;
            }
            let missing: Vec<String> = git
                .missing_by_patch_id(oid, &t.name, &limit)?
                .into_iter()
                .filter(|o| seen.insert(o.clone()))
                .collect();
            if !missing.is_empty() {
                stale.push(format!("{} (+{})", t.name, missing.len()));
            }
        }
        if !stale.is_empty() {
            f.push(Finding::new(
                "TAG_ON_STALE_TARGET",
                Severity::Drift,
                name,
                format!(
                    "`{name}` ({}) is missing {} commit(s), from {} -- it was stitched and tagged before those tiers moved",
                    short(oid),
                    seen.len(),
                    stale.join(", ")
                ),
                format!("sodactl stitch soda-full --apply && sodactl tag {component} <n+1> --apply   # tag the NEW stitch; never move `{name}`"),
            ));
        }
    }

    // ---- C3: a consumer branch ahead of its newest family tag. ----
    //
    // BRANCHING.md declares this the NORMAL state ("a pour that has not passed
    // the gate yet"), so it is informational only. Its real job is to make the
    // pin findings legible -- and it deliberately stays silent when the tip is
    // untagged AND pinned, because `pin.rs` already blocks on exactly that and
    // two rows for one fact reads as two problems.
    for c in &m.consumer {
        let Some(tip) = git.rev_parse(&c.name)? else { continue };
        let newest = ours
            .iter()
            .filter_map(|(n, o, p)| match p {
                // Same (base, version) ordering as above: version alone picks
                // the older base's tag once a second base exists.
                TagName::Family { component, .. } if component == &c.name => {
                    Some((p.release_key()?, n.as_str(), o.as_str()))
                }
                _ => None,
            })
            .max_by_key(|(k, _, _)| *k);
        let Some((_, tag, tag_oid)) = newest else { continue };
        if tag_oid == tip || c.pinned_by_superproject {
            continue;
        }
        f.push(Finding::new(
            "CONSUMER_AHEAD_OF_TAG",
            Severity::Hygiene,
            &c.name,
            format!(
                "`{}` is at {} while its newest tag `{tag}` names {} -- normal for a pour that has not passed the gate",
                c.name,
                short(&tip),
                short(tag_oid)
            ),
            format!("no action, or cut a release: git -C wine tag <{}-<base>-<n>> {}", c.name, c.name),
        ));
    }

    // ---- C4: family skew. ----
    //
    // Severity is a POLICY input, not a guess. If the trailing integer means
    // "everything carrying -3 was cut together and is meant to be used
    // together", skew is a Blocker; if a component may legitimately lag between
    // cuts, it is noise during every partial release. This check fires on every
    // partial release, so a wrong severity is one you meet constantly.
    if p.family_skew != SkewPolicy::Off {
        let mut max: BTreeMap<&str, u32> = BTreeMap::new();
        for (_, _, parsed) in &ours {
            if let TagName::Family { component, version, .. } = parsed {
                let e = max.entry(component.as_str()).or_insert(0);
                *e = (*e).max(*version);
            }
        }
        // A GAP in a family's history is not skew -- only the max matters.
        if max.values().collect::<std::collections::BTreeSet<_>>().len() > 1 {
            let listed: Vec<String> = max.iter().map(|(c, v)| format!("{c}={v}")).collect();
            f.push(Finding::new(
                "TAG_FAMILY_SKEW",
                match p.family_skew {
                    SkewPolicy::Blocker => Severity::Blocker,
                    _ => Severity::Drift,
                },
                "tags",
                format!(
                    "family versions disagree: {} -- Blocker if the trailing integer means \"cut together, use together\", noise if a component may lag between cuts. NOTE: `prater-*` lives in a third repository this tool cannot see, so this comparison is structurally incomplete.",
                    listed.join(", ")
                ),
                "record the answer once: set [policy] family_skew in sodactl.toml, then cut the lagging component or accept the lag".to_string(),
            ));
        }
    }

    Ok(f)
}

fn declared_equivalent(p: &Policy, names: &[&str]) -> bool {
    p.tag_equivalences
        .iter()
        .any(|set| names.iter().all(|n| set.iter().any(|s| s == n)))
}

fn short(o: &str) -> String {
    o.chars().take(11).collect()
}
