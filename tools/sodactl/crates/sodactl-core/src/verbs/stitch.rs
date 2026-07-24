//! `stitch` -- rebuild a declared stitch branch from scratch: `checkout -B
//! <name> <base_tier>`, then cherry-pick each ordered tier's OWN commits in
//! the manifest's declared order.
//!
//! Two decisions carry the verb:
//!
//! * The per-tier range is derived with `restack`'s own-set discipline
//!   (`derive_own_set`), never typed: the same stale-`<old>` trap that
//!   duplicates commits into a restacked tier duplicates them into a stitch,
//!   and a stitch feeds every full-mix consumer downstream.
//! * The POSTCONDITION is patch-id, not ancestry: a stitch is built by
//!   cherry-pick, so the tiers' commits are never its ancestors -- after the
//!   build, every ordered tier's own commits must be patch-present in the
//!   stitch and the commit count must be exactly the sum of the own sets.
//!   Cherry-picking onto the SAME base preserves patch-ids for unconflicted
//!   picks (the reconcile module's rationale), and a conflicted pick never
//!   reaches the postcondition -- it stops the apply as a terminal state.
//!
//! Conflict handling is STOP WITH GUIDANCE, deliberately: the binary-rule
//! classifier (`diff --numstat` + `[[binary_resolution]]`) belongs to `regen`,
//! which owns the replay where those rules are declared to apply. When regen
//! lands, its classifier is the seam to share here -- until then a conflicted
//! stitch prints the resolve/continue commands and exits, exactly like a
//! conflicted restack.

use std::fmt;

use crate::manifest::Manifest;
use crate::plan::{
    self, Action, ApplyCtx, ApplyError, ApplyOutcome, Plan, PlannedValue, RefMutation, RepoSide,
    Step,
};
use crate::verbs::restack::derive_own_set;
use anyhow::Context as _;
use sodactl_git::GitRead;

/// One ordered tier's contribution to the stitch, derived at plan time. The
/// postcondition (and the tests, and the human) check the result against
/// exactly these facts.
#[derive(Debug, serde::Serialize)]
pub struct StitchPick {
    pub tier: String,
    /// The tier's tip at derivation time -- the head of the pick range.
    pub tip: String,
    /// Computed via the own-set derivation; the exclusive start of the range.
    pub old_base: String,
    /// The tier's own commits (oids, oldest first).
    pub own: Vec<String>,
    /// True when the tier does not contain its parent's current tip: the
    /// stitch still builds, but it linearises stale inputs. A note, not a
    /// refusal -- the gate lives on push, and re-stitching after a restack is
    /// exactly the repair path this warning points at.
    pub stale: bool,
}

/// The reviewable stitch: the `Plan` plus the derivation facts.
#[derive(Debug, serde::Serialize)]
pub struct StitchPlan {
    pub plan: Plan,
    pub name: String,
    pub base_tier: String,
    /// The base tier's tip -- the `checkout -B` start point.
    pub base_tip: String,
    pub picks: Vec<StitchPick>,
    /// `(refname, old tip)` when the stitch already existed; a first-ever
    /// stitch has nothing to back up.
    pub backup: Option<(String, String)>,
}

/// Postcondition failure: the picks ran but the result does not carry the
/// tiers' patch-ids (or carries the wrong count). Where a backup exists the
/// branch has been restored from it by the time this error exists; a
/// first-ever stitch is left in place for inspection instead -- there is no
/// earlier state to restore, and deleting the evidence would leave nothing to
/// diagnose. Not `refused: ` -- a mutation happened, which is a different fact.
#[derive(Debug)]
pub struct StitchPostconditionDrift {
    pub name: String,
    /// Sum of the ordered tiers' own sets.
    pub expected: usize,
    /// `base_tip..<name>` after the build.
    pub count: u32,
    /// `(tier, commits whose patch-id is absent from the stitch)`.
    pub lost: Vec<(String, Vec<String>)>,
    pub backup: Option<String>,
    /// `Some(detail)` when the automatic restore itself failed.
    pub restore_failed: Option<String>,
}

impl fmt::Display for StitchPostconditionDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stitch postcondition FAILED for `{}`: expected {} commit(s) on the base with every \
             tier's patch-ids present, observed count={}",
            self.name, self.expected, self.count,
        )?;
        for (tier, oids) in &self.lost {
            let shorts = oids.iter().map(|o| short(o)).collect::<Vec<_>>().join(", ");
            write!(f, ", `{tier}` lost [{shorts}]")?;
        }
        write!(f, ".")?;
        match (&self.backup, &self.restore_failed) {
            (Some(b), None) => write!(f, " The branch was restored from {b}."),
            (Some(b), Some(d)) => write!(
                f,
                " AUTOMATIC RESTORE FAILED ({d}) -- restore by hand: \
                 `git -C wine update-ref refs/heads/{} {b}`",
                self.name
            ),
            (None, _) => write!(
                f,
                " No earlier stitch existed to restore; the failed build was left in place for \
                 inspection (`git -C wine log {}`); delete it with `git -C wine branch -D {}`",
                self.name, self.name
            ),
        }
    }
}

impl std::error::Error for StitchPostconditionDrift {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Plan the rebuild of one declared stitch. Always from scratch (`-B` from the
/// base tier), which is what makes the verb idempotent by construction: the
/// same inputs replay to the same tree, and a re-run after a wedged attempt
/// starts clean instead of continuing from half a linearization.
pub fn plan_stitch<G: GitRead>(m: &Manifest, git: &G, name: &str) -> anyhow::Result<StitchPlan> {
    let s = m
        .stitch
        .iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no [[stitch]] named `{name}` in the manifest"))?;

    let base_tip = git
        .rev_parse(&s.base_tier)?
        .with_context(|| format!("`{name}`'s base tier `{}` does not resolve", s.base_tier))?;
    // Raw: the CAS pre-verify compares the ref AS STORED.
    let existing = git.rev_parse_raw(&format!("refs/heads/{name}"))?;

    let mut picks = Vec::new();
    for tier in &s.order {
        let os = derive_own_set(m, git, tier)?;
        // Staleness is a fact worth showing in the review, not a gate: the
        // verb-never-consults-check rule, and the stitch of a stale tier is
        // sometimes exactly what a human wants to compare against.
        let stale = !git.is_ancestor(&os.parent_tip, tier)?;
        picks.push(StitchPick {
            tier: tier.clone(),
            tip: os.child_tip,
            old_base: os.old_base,
            own: os.own,
            stale,
        });
    }

    let mut steps = vec![Step::new(
        format!("rebuild `{name}` from scratch on `{}` ({})", s.base_tier, short(&base_tip)),
        RepoSide::Wine,
        Action::CheckoutBranchReset { branch: name.to_string(), start: base_tip.clone() },
    )];
    for p in picks.iter().filter(|p| !p.own.is_empty()) {
        steps.push(Step::new(
            format!("cherry-pick `{}`'s {} own commit(s)", p.tier, p.own.len()),
            RepoSide::Wine,
            // Oids, not ref names: the range in the reviewed plan is the range
            // that runs, whatever moves in between (and the id then covers it).
            Action::CherryPick { range: format!("{}..{}", p.old_base, p.tip) },
        ));
    }

    let mutations = vec![RefMutation {
        repo: RepoSide::Wine,
        refname: format!("refs/heads/{name}"),
        expected_before: existing.clone(),
        after: PlannedValue::Unknown {
            reason: "cherry-picks mint new commits; the tip is unknowable until they run".into(),
        },
    }];

    let backup = existing
        .as_ref()
        .map(|tip| (plan::backup_refname(&plan::backup_stamp(), name), tip.clone()));

    let mut notes = Vec::new();
    notes.push(format!("the checkout moves to `{name}` (checkout -B) and stays there"));
    if existing.is_none() {
        notes.push(format!("`{name}` does not exist yet -- this build creates it"));
    }
    for p in &picks {
        if p.own.is_empty() {
            notes.push(format!("`{}` contributes no own commits -- nothing to pick", p.tier));
        } else {
            notes.push(format!(
                "`{}`: {} own commit(s), old base {} (computed, never an argument)",
                p.tier,
                p.own.len(),
                short(&p.old_base)
            ));
        }
        if p.stale {
            notes.push(format!(
                "warning: `{}` does not contain its parent's tip -- the stitch will linearise \
                 stale inputs; `sodactl restack {}` first, then re-stitch",
                p.tier, p.tier
            ));
        }
    }

    let plan = Plan::new(
        "stitch",
        steps,
        mutations,
        backup.clone().into_iter().collect(),
        notes,
    );
    Ok(StitchPlan {
        plan,
        name: name.to_string(),
        base_tier: s.base_tier.clone(),
        base_tip,
        picks,
        backup,
    })
}

#[derive(Debug)]
pub struct StitchOutcome {
    pub outcome: ApplyOutcome,
    /// The rebuilt tip that passed the postcondition.
    pub new_tip: String,
}

/// Run one stitch plan through the shared engine, then enforce the
/// postcondition (patch-id presence per tier + count equality). On drift the
/// branch is restored from its backup where one exists; see
/// `StitchPostconditionDrift` for the first-ever-stitch case.
pub fn apply_stitch(sp: &StitchPlan, ctx: &ApplyCtx) -> anyhow::Result<StitchOutcome> {
    let outcome = match plan::apply(&sp.plan, ctx) {
        Ok(o) => o,
        Err(e) => {
            // A conflicted pick is a legitimate terminal state; the engine's
            // error already names the journal, the backup and the
            // resolve/--continue/--abort commands. The stitch-specific
            // additions: binary rules are regen's to apply (the seam noted in
            // the module header), and the re-run is a full rebuild.
            let extra = matches!(e, ApplyError::Step { .. }).then(|| {
                format!(
                    "a `[[binary_resolution]]` rule is applied automatically only by `sodactl \
                     regen`; for a stitch, resolve by hand. Afterwards re-run `sodactl stitch {}` \
                     -- it rebuilds from scratch and re-verifies",
                    sp.name
                )
            });
            let err = anyhow::Error::new(e);
            return Err(match extra {
                Some(x) => err.context(x),
                None => err,
            });
        }
    };

    let refname = format!("refs/heads/{}", sp.name);
    let new_tip = ctx
        .wine_read
        .rev_parse_raw(&refname)?
        .with_context(|| format!("{refname} vanished after the build"))?;

    // The postcondition. Both cherry limits satisfy the ancestor invariant by
    // construction: each `old_base` is `tier~n`, and `base_tip` is what the
    // stitch was just built on. Count equality is what rules out EXTRA
    // commits, which the per-tier presence check cannot see.
    let expected: usize = sp.picks.iter().map(|p| p.own.len()).sum();
    let count = ctx.wine_read.count_commits(&format!("{}..{}", sp.base_tip, sp.name))?;
    let mut lost = Vec::new();
    for p in &sp.picks {
        if p.own.is_empty() {
            continue;
        }
        let missing = ctx.wine_read.missing_by_patch_id(&sp.name, &p.tip, &p.old_base)?;
        if !missing.is_empty() {
            lost.push((p.tier.clone(), missing));
        }
    }
    if count as usize == expected && lost.is_empty() {
        return Ok(StitchOutcome { outcome, new_tip });
    }

    // Drift. Same three-step restore as restack's, for the same reason: the
    // build left HEAD attached to the branch, so detach, CAS the ref back
    // (expected = the drifted tip just observed), re-attach.
    let restore_failed = match &sp.backup {
        None => None,
        Some((_, old_tip)) => (|| -> Result<(), String> {
            ctx.wine.checkout_detach(old_tip).map_err(|e| e.to_string())?;
            ctx.wine.update_ref_cas(&refname, old_tip, Some(&new_tip)).map_err(|e| e.to_string())?;
            ctx.wine.checkout_branch_reset(&sp.name, old_tip).map_err(|e| e.to_string())
        })()
        .err(),
    };

    Err(anyhow::Error::new(StitchPostconditionDrift {
        name: sp.name.clone(),
        expected,
        count,
        lost,
        backup: sp.backup.as_ref().map(|(r, _)| r.clone()),
        restore_failed,
    }))
}
