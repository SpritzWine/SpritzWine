//! `tag` -- cut `<family>-<winebase>-<n>` on the family's component tip.
//!
//! Everything about the name is DERIVED, never declared: the target comes from
//! the manifest (a consumer family tags its consumer branch; the `soda` family
//! tags the stitch the consumers pour from), and the `<winebase>` field comes
//! from the target's own base ref via `tags::wine_base_of` -- the plan doc's
//! rule, and the mechanism that catches a doc saying 11.12 while the release
//! ships 11.13.
//!
//! The refusals are the OPERATION protecting itself, not the push gate: each
//! one names a state in which the tag being cut would immediately be a lie
//! (a name that already exists, a version that ships the previous version's
//! bits, a target the tiers have moved past). `--force` overrides them all,
//! because the human sometimes knows better -- and a declared
//! `[[policy.tag_equivalence]]` silences the collision arm without force,
//! because BRANCHING.md documents same-commit families as an intended state.

use std::fmt;

use crate::checks::Policy;
use crate::manifest::Manifest;
use crate::plan::{self, Action, Plan, PlannedValue, RefMutation, RepoSide, Step};
use crate::tags::{self, TagName};
use anyhow::Context as _;
use sodactl_git::{GitRead, Reapply};

/// Where a family's tag lands, and where its `<winebase>` came from.
#[derive(Debug, serde::Serialize)]
pub struct FamilyTarget {
    /// The ref the tag names: a consumer branch, or the soda stitch.
    pub target_ref: String,
    /// Derived via `tags::wine_base_of`; `None` when the source names no wine
    /// version, in which case no tag name can be honestly constructed.
    pub wine_base: Option<String>,
    /// Human words for where `wine_base` was read from.
    pub base_source: String,
}

/// Resolve a family name to its component tip, from the manifest alone.
///
/// * A consumer family tags its consumer branch; the base field the consumer
///   declares is what carries the wine version.
/// * The `soda` family tags the stitch the consumers actually pour from --
///   derived as the unique stitch named by a consumer's `soda_tip`, NOT
///   hardcoded to `soda-full`: the manifest may declare scratch stitches
///   (`soda-stale-full` in the fixture) that nothing consumes, and tagging one
///   of those would name a tree no release is built from. Its wine version is
///   the home base's -- a stitch sits on the soda side of the fork point.
/// * `prater` lives in a third repository this tool cannot see; saying so
///   beats resolving to nothing.
pub fn family_target(m: &Manifest, family: &str) -> anyhow::Result<FamilyTarget> {
    if let Some(c) = m.consumer.iter().find(|c| c.name == family) {
        return Ok(FamilyTarget {
            target_ref: c.name.clone(),
            wine_base: tags::wine_base_of(&c.base),
            base_source: format!("consumer base `{}`", c.base),
        });
    }
    if family == "soda" {
        let mut tips: Vec<&str> = m
            .consumer
            .iter()
            .map(|c| c.soda_tip.as_str())
            .filter(|t| m.stitch.iter().any(|s| s.name == *t))
            .collect();
        tips.sort_unstable();
        tips.dedup();
        return match tips.as_slice() {
            [one] => Ok(FamilyTarget {
                target_ref: one.to_string(),
                wine_base: tags::wine_base_of(&m.repo.home_base.describes),
                base_source: format!("home_base describes `{}`", m.repo.home_base.describes),
            }),
            [] => anyhow::bail!(
                "no stitch is any consumer's soda_tip -- there is no canonical soda tree to tag"
            ),
            many => anyhow::bail!(
                "multiple stitches serve as soda_tips ({}) -- the soda family's target is \
                 ambiguous; tag by hand",
                many.join(", ")
            ),
        };
    }
    if family == "prater" {
        anyhow::bail!(
            "`prater` lives in a third repository this tool cannot see -- tag it there"
        );
    }
    let mut known: Vec<String> = m.consumer.iter().map(|c| c.name.clone()).collect();
    known.push("soda".into());
    anyhow::bail!("unknown tag family `{family}` (known: {})", known.join(", "))
}

/// The reviewable tag cut: the `Plan` plus the derivation facts.
#[derive(Debug, serde::Serialize)]
pub struct TagPlan {
    pub plan: Plan,
    pub tag: String,
    pub family: String,
    pub target_ref: String,
    /// The commit the tag will name -- the target ref peeled to `^{commit}`.
    pub commit: String,
    /// The existing tag OBJECT oid when `--force` re-points a name; recorded
    /// in the plan's `expected_before` (and thus the journal) AND preserved
    /// under `refs/sodactl/backup/<stamp>/tags/<tag>` -- tag refs have no
    /// reflog by default, and without a ref the old annotated object is one
    /// `gc.pruneExpire` from gone, leaving the journalled oid a string that
    /// names nothing.
    pub previous: Option<String>,
}

/// The tag-time preconditions. Typed, `refused: `-prefixed, downcastable --
/// exit 1. Every arm's wording says what would be WRONG about the tag, not
/// what a later push gate would think.
#[derive(Debug)]
pub enum TagRefusal {
    /// The name cannot be constructed: the target's base names no wine
    /// version.
    UnderivableBase { family: String, base_source: String },
    /// The exact name already exists (at any commit).
    Exists { tag: String, at: String },
    /// Another tag of the same family already names the target commit, and no
    /// declared equivalence covers the pair: either the family rolled without
    /// a re-pour (declare it) or this version would ship the previous
    /// version's bits.
    Collision { tag: String, existing: String, commit: String },
    /// A declared equivalence names this tag next to one that exists at a
    /// DIFFERENT commit -- cutting it would make the declaration false.
    EquivalenceContradicted { tag: String, partner: String, partner_at: String, target: String },
    /// The target is a stitch the tiers have moved past (derivable there and
    /// only there; consumer currency needs the ledger).
    StaleTarget { tag: String, target_ref: String, detail: String },
}

impl fmt::Display for TagRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagRefusal::UnderivableBase { family, base_source } => write!(
                f,
                "refused: cannot derive a wine base for family `{family}` from its {base_source} \
                 -- no honest `<family>-<winebase>-<n>` name exists; tag by hand if this is \
                 intentional"
            ),
            TagRefusal::Exists { tag, at } => write!(
                f,
                "refused: tag `{tag}` already exists (at {at}) -- a released name never moves; \
                 cut the next version, or `--force` if this one was cut in error and never pushed"
            ),
            TagRefusal::Collision { tag, existing, commit } => write!(
                f,
                "refused: `{existing}` already names {commit} -- cutting `{tag}` there would \
                 either ship the previous version's bits under a new name or record a roll \
                 nobody declared. If the family rolled without a re-pour, declare a \
                 [[policy.tag_equivalence]] for the pair; otherwise re-stitch/re-pour first \
                 (or `--force` to overrule)"
            ),
            TagRefusal::EquivalenceContradicted { tag, partner, partner_at, target } => write!(
                f,
                "refused: a declared tag_equivalence pairs `{tag}` with `{partner}`, but \
                 `{partner}` is at {partner_at} while this cut would land at {target} -- the \
                 declaration would become false the moment the tag exists; fix the manifest or \
                 the target (or `--force` to overrule)"
            ),
            TagRefusal::StaleTarget { tag, target_ref, detail } => write!(
                f,
                "refused: `{tag}` would name `{target_ref}` while {detail} -- the tag would \
                 memorialise a tree the tiers have moved past; re-stitch first \
                 (`sodactl stitch {target_ref} --apply`), or `--force` to tag it anyway"
            ),
        }
    }
}

impl std::error::Error for TagRefusal {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Plan the cut of `<family>-<winebase>-<n>`.
pub fn plan_tag<G: GitRead>(
    m: &Manifest,
    git: &G,
    p: &Policy,
    family: &str,
    n: u32,
    force: bool,
) -> anyhow::Result<TagPlan> {
    let ft = family_target(m, family)?;
    let Some(base) = &ft.wine_base else {
        return Err(anyhow::Error::new(TagRefusal::UnderivableBase {
            family: family.to_string(),
            base_source: ft.base_source,
        }));
    };
    let tag = format!("{family}-{base}-{n}");

    let commit = git
        .rev_parse(&ft.target_ref)?
        .with_context(|| format!("target `{}` does not resolve", ft.target_ref))?;

    // Raw on the full ref: for an annotated tag this is the TAG OBJECT oid,
    // which is exactly what a forced re-point's pre-verify must compare (and
    // what the backup ref below must preserve).
    let previous = git.rev_parse_raw(&format!("refs/tags/{tag}"))?;
    if previous.is_some() && !force {
        let at = git.rev_parse(&tag)?.unwrap_or_else(|| "<unresolvable>".into());
        return Err(anyhow::Error::new(TagRefusal::Exists { tag, at: short(&at) }));
    }

    let mut notes = Vec::new();

    // Family collision: another tag of this component already at the target
    // commit. One `tags_with_targets` pass, the tagcheck discipline (peeled in
    // one process, Foreign dropped before any comparison).
    let equivalent = |a: &str, b: &str| {
        p.tag_equivalences
            .iter()
            .any(|set| set.iter().any(|s| s == a) && set.iter().any(|s| s == b))
    };
    for (name, oid) in git.tags_with_targets()? {
        if name == tag {
            continue;
        }
        match TagName::parse(&name, &p.tag_aliases) {
            TagName::Family { component, .. } if component == family => {}
            _ => continue,
        }
        if oid == commit {
            if equivalent(&tag, &name) {
                notes.push(format!(
                    "`{name}` names the same commit -- declared equivalent, so the roll is \
                     intended and recorded"
                ));
            } else if force {
                notes.push(format!(
                    "OVERRULED by --force: `{name}` already names this commit with no declared \
                     equivalence"
                ));
            } else {
                return Err(anyhow::Error::new(TagRefusal::Collision {
                    tag,
                    existing: name,
                    commit: short(&commit),
                }));
            }
        } else if equivalent(&tag, &name) && !force {
            // The declaration exists BEFORE the cut (that is how same-commit
            // rolls are pre-declared); a partner at a different commit means
            // the cut would falsify it.
            return Err(anyhow::Error::new(TagRefusal::EquivalenceContradicted {
                tag,
                partner: name.clone(),
                partner_at: short(&oid),
                target: short(&commit),
            }));
        }
    }

    // Stale target, where derivable: only a stitch target supports the
    // question (same-base cherry-picks, so patch-id is sound -- consumers are
    // cross-base replays where it is not, and their currency waits for the
    // ledger). Same fast-path-plus-reapply-confirmation as the STITCH_STALE
    // check, so a rerere-touched pick cannot fabricate a refusal.
    if let Some(s) = m.stitch.iter().find(|s| s.name == ft.target_ref) {
        let stale_detail = if !git.is_ancestor(&s.base_tier, &s.name)? {
            Some(format!("it no longer descends from its base tier `{}`", s.base_tier))
        } else {
            let mut behind = Vec::new();
            for tier in &s.order {
                if git.rev_parse(tier)?.is_none() {
                    continue;
                }
                let suspected = git.missing_by_patch_id(&s.name, tier, &s.base_tier)?;
                let mut confirmed = 0usize;
                for c in &suspected {
                    if !matches!(git.reapply_verdict(c, &s.name)?, Reapply::Present) {
                        confirmed += 1;
                    }
                }
                if confirmed > 0 {
                    behind.push(format!("`{tier}` (+{confirmed})"));
                }
            }
            (!behind.is_empty())
                .then(|| format!("it is missing commits from {}", behind.join(", ")))
        };
        if let Some(detail) = stale_detail {
            if force {
                notes.push(format!("OVERRULED by --force: {detail}"));
            } else {
                return Err(anyhow::Error::new(TagRefusal::StaleTarget {
                    tag,
                    target_ref: ft.target_ref,
                    detail,
                }));
            }
        }
    }

    // A re-point is a ref rewrite: it gets the same never-pruned backup a
    // branch rewrite gets, under the `tags/` subject prefix `undo` maps back
    // to `refs/tags/`. The backup ref keeps the old TAG OBJECT alive past any
    // gc, which the journal's recorded oid alone cannot.
    let backups = previous
        .as_ref()
        .map(|prev| (plan::backup_refname(&plan::backup_stamp(), &format!("tags/{tag}")), prev.clone()))
        .into_iter()
        .collect::<Vec<_>>();
    if let Some((backup_ref, prev)) = backups.first() {
        notes.push(format!(
            "--force re-points an existing tag; the previous tag object {} survives at \
             {backup_ref} -- restore with `sodactl undo` (or `git -C wine update-ref \
             refs/tags/{tag} {prev}`)",
            short(prev)
        ));
    }
    notes.push(format!("wine base `{base}` derived from the {}", ft.base_source));

    let message = format!("{tag}: `{}` at {}", ft.target_ref, short(&commit));
    let steps = vec![Step::new(
        format!("annotated tag `{tag}` on `{}`'s tip {}", ft.target_ref, short(&commit)),
        RepoSide::Wine,
        // The peeled COMMIT oid, not the ref name: `rev_parse` already applied
        // `^{commit}`, so the tag lands on the commit whatever sits on the ref
        // when the plan runs -- and the id then covers the exact target.
        Action::TagAnnotated { name: tag.clone(), target: commit.clone(), message, force },
    )];
    let mutations = vec![RefMutation {
        repo: RepoSide::Wine,
        refname: format!("refs/tags/{tag}"),
        expected_before: previous.clone(),
        after: PlannedValue::Unknown {
            reason: "an annotated tag mints a new tag object; its oid is unknowable until then"
                .into(),
        },
    }];

    let plan = Plan::new("tag", steps, mutations, backups, notes);
    Ok(TagPlan { plan, tag, family: family.to_string(), target_ref: ft.target_ref, commit, previous })
}

/// Run one tag plan through the shared engine, then verify the name derefs to
/// the planned commit. No auto-restore -- a wrong tag is one `update-ref` from
/// gone, and the error names it.
pub fn apply_tag(tp: &TagPlan, ctx: &crate::plan::ApplyCtx) -> anyhow::Result<crate::plan::ApplyOutcome> {
    let outcome = crate::plan::apply(&tp.plan, ctx).map_err(anyhow::Error::new)?;
    let landed = ctx
        .wine_read
        .rev_parse(&tp.tag)?
        .with_context(|| format!("`{}` does not resolve after the cut", tp.tag))?;
    if landed != tp.commit {
        anyhow::bail!(
            "tag postcondition FAILED: `{}` derefs to {} but the plan targeted {} -- delete it \
             (`git -C wine tag -d {}`) and re-run",
            tp.tag,
            short(&landed),
            short(&tp.commit),
            tp.tag
        );
    }
    Ok(outcome)
}
