//! `restack` -- rebase a stale tier's own commits onto its parent's current tip.
//!
//! THE INVENTION, from the design doc: `restack` never takes `<old>` as an
//! argument. A stale hand-typed `<old>` silently duplicates the parent's
//! commits into the child, and every consumer generated downstream then ships
//! a doubled commit with nothing checking for it. So the old base is COMPUTED:
//!
//! 1. `own` = the child's commits not patch-present in the NEW parent tip
//!    (`git cherry` -- patch-ids survive the parent's rewrite, which is what
//!    makes the derivation possible after the fact);
//! 2. verified: `own` must be exactly the child's last `|own|` first-parent
//!    commits -- anything else means the branch mixes its own work with
//!    something this derivation cannot name, and the only honest move is to
//!    refuse with the evidence rather than rebase from a guess;
//! 3. `old_base = child~|own|`.
//!
//! And the POSTCONDITION is the real guard: after the rebase, the patch-id set
//! of `parent_tip..child` must equal `own` -- both directions, plus a count.
//! On drift the branch is restored from its backup ref automatically and the
//! failure is loud. A restack that cannot prove it preserved the child's work
//! did not happen.

use std::collections::BTreeSet;
use std::fmt;

use crate::graph;
use crate::manifest::Manifest;
use crate::plan::{
    self, Action, ApplyCtx, ApplyError, ApplyOutcome, Plan, PlannedValue, RefMutation, RepoSide,
    Step,
};
use anyhow::Context as _;
use sodactl_git::GitRead;

/// One tier's restack: the reviewable `Plan` plus the derivation facts the
/// postcondition (and the tests, and the human) check it against.
#[derive(Debug, serde::Serialize)]
pub struct RestackPlan {
    pub plan: Plan,
    pub tier: String,
    /// The parent tip the own set was derived AGAINST -- the rebase target.
    pub parent_tip: String,
    /// Computed, never an argument (the whole point of the verb).
    pub old_base: String,
    /// The child's own commits (oids, oldest first as `git cherry` lists
    /// them). The postcondition's reference set.
    pub own: Vec<String>,
    /// `(refname, old tip)` -- created create-only by `apply` before the
    /// rebase, restored by the postcondition on drift and by `sodactl undo`.
    pub backup: (String, String),
    /// True when this plan was derived while an ANCESTOR tier in the same
    /// batch still awaits its restack: the parent tip above is about to move,
    /// so the plan is a preview and the batch driver re-derives (and re-shows)
    /// it after the parent lands. Its id is therefore not drift-checked.
    pub provisional: bool,
}

/// Preconditions the derivation itself can fail. Typed, `refused: `-prefixed
/// and downcastable like `plan::Refusal`, because "nothing was changed, the
/// inputs do not support the operation" must map to exit 1, not 2.
#[derive(Debug)]
pub enum RestackRefusal {
    /// The child's patch-id own-set is not its first-parent tail, so no
    /// `old_base` can be named without guessing -- and a guessed base is the
    /// exact bug class this verb exists to close.
    AmbiguousOldParent {
        tier: String,
        /// The cherry-derived own set (oldest first).
        own: Vec<String>,
        /// The first-parent walk `child~0, child~1, ...` up to and including
        /// the first commit that broke the match.
        first_parent: Vec<String>,
        /// Which `child~i` failed the membership test.
        mismatch_at: usize,
    },
    /// The child's candidate own range contains a merge. `rebase --onto`
    /// linearises merges away, which "silently changes what consumers get"
    /// (the `merges_in` trait doc) -- not a repair this verb may perform.
    NotLinear { tier: String, merges: Vec<String> },
}

impl fmt::Display for RestackRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shorts = |v: &[String]| v.iter().map(|o| short(o)).collect::<Vec<_>>().join(", ");
        match self {
            RestackRefusal::AmbiguousOldParent { tier, own, first_parent, mismatch_at } => write!(
                f,
                "refused: cannot derive `{tier}`'s old base -- `{tier}~{mismatch_at}` ({}) is not in \
                 its patch-id own-set, so the branch mixes its own work with something this \
                 derivation cannot name (a rewritten parent, foreign commits). Rebasing from a \
                 guessed base could duplicate or drop commits; inspect by hand. Evidence: \
                 own-set [{}], first-parent tail [{}]",
                first_parent.get(*mismatch_at).map(|o| short(o)).unwrap_or_else(|| "<unresolvable>".into()),
                shorts(own),
                shorts(first_parent),
            ),
            RestackRefusal::NotLinear { tier, merges } => write!(
                f,
                "refused: `{tier}`'s own range contains merge commit(s) [{}] -- a rebase would \
                 linearise them away and silently change what consumers get; flatten the tier by \
                 hand first",
                shorts(merges),
            ),
        }
    }
}

impl std::error::Error for RestackRefusal {}

/// Postcondition failure: the rebase ran but the replayed commits do not carry
/// the same patch-ids as the own set. The branch has already been RESTORED
/// from its backup by the time this error exists (or `restore_failed` says
/// exactly what to do by hand). Not `refused: ` -- a mutation happened and was
/// rolled back, which the human must know is a different fact.
#[derive(Debug)]
pub struct PostconditionDrift {
    pub tier: String,
    pub expected: usize,
    pub count: u32,
    /// Own commits whose patch-id vanished from the rebased branch.
    pub lost: Vec<String>,
    /// Rebased commits whose patch-id was never in the own set.
    pub alien: Vec<String>,
    pub backup: String,
    /// `Some(detail)` when the automatic restore itself failed.
    pub restore_failed: Option<String>,
}

impl fmt::Display for PostconditionDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shorts = |v: &[String]| v.iter().map(|o| short(o)).collect::<Vec<_>>().join(", ");
        write!(
            f,
            "restack postcondition FAILED for `{}`: expected its {} own commit(s) with unchanged \
             patch-ids on the new base, observed count={}, lost=[{}], unexpected=[{}].",
            self.tier,
            self.expected,
            self.count,
            shorts(&self.lost),
            shorts(&self.alien),
        )?;
        match &self.restore_failed {
            None => write!(f, " The branch was restored from {}.", self.backup),
            Some(d) => write!(
                f,
                " AUTOMATIC RESTORE FAILED ({d}) -- restore by hand: \
                 `git -C wine update-ref refs/heads/{} {}`",
                self.tier, self.backup
            ),
        }
    }
}

impl std::error::Error for PostconditionDrift {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Plan the restack of `tier` -- and, with `all_descendants`, of every
/// descendant tier that needs one, in `Dag::restack_order` (parents strictly
/// before children; the order is the whole value, see `graph.rs`).
///
/// An empty vec is the honest "nothing to restack": the tier already sits on
/// its parent's tip (re-running a completed restack lands here, which is what
/// makes the verb idempotent).
///
/// Plans after the first in a batch are `provisional`: derived against a
/// parent tip the batch itself is about to move. The apply driver re-derives
/// each one after its predecessor lands -- deriving them here anyway is what
/// lets the human review the SHAPE of the whole batch before confirming it.
pub fn plan_restack<G: GitRead>(
    m: &Manifest,
    git: &G,
    tier: &str,
    all_descendants: bool,
) -> anyhow::Result<Vec<RestackPlan>> {
    if m.tier(tier).is_none() {
        anyhow::bail!("no [[tier]] named `{tier}` in the manifest");
    }
    let dag = graph::tier_graph(m, git)?;

    let targets: Vec<String> = if all_descendants {
        // The subtree rooted at `tier`, restricted to what actually needs a
        // restack, in the DAG's parent-first order.
        let mut family: BTreeSet<&str> = BTreeSet::from([tier]);
        // Transitive closure over the manifest's parent edges. Bounded passes,
        // same cycle discipline as `propagate_staleness`.
        for _ in 0..m.tier.len() {
            let before = family.len();
            for t in &m.tier {
                if family.contains(t.parent.as_str()) {
                    family.insert(&t.name);
                }
            }
            if family.len() == before {
                break;
            }
        }
        dag.restack_order.iter().filter(|n| family.contains(n.as_str())).cloned().collect()
    } else {
        let node = dag
            .nodes
            .iter()
            .find(|n| n.name == tier)
            .expect("tier_graph emits a node per manifest tier");
        match node.on_parent_tip {
            // Directly stale: the one case a single-tier restack repairs.
            Some(false) => vec![tier.to_string()],
            // On its parent's tip already -- possibly inherited-stale, but
            // restacking IT alone would be a no-op; that is the batch driver's
            // job (`--all-descendants` from the stale ancestor).
            Some(true) => vec![],
            None => anyhow::bail!(
                "cannot tell whether `{tier}` needs a restack -- the branch or its parent does \
                 not resolve (see `sodactl check`)"
            ),
        }
    };

    let mut plans = Vec::new();
    for (i, name) in targets.iter().enumerate() {
        // Provisional iff an ancestor in THIS batch precedes it: its parent
        // tip is scheduled to move under it.
        let parent =
            m.tier(name).map(|t| t.parent.clone()).unwrap_or_default();
        let provisional = i > 0 && targets[..i].iter().any(|earlier| *earlier == parent);
        plans.push(plan_one(m, git, name, provisional)?);
    }
    Ok(plans)
}

/// One tier's derived own-commit facts: the whole "never take `<old>` as an
/// argument" machinery, shared verbatim by `restack` (which rebases the tier)
/// and `stitch` (which cherry-picks exactly this range) so the two verbs
/// cannot disagree about what a tier's own work is.
#[derive(Debug)]
pub(crate) struct OwnSet {
    pub child_tip: String,
    /// The tip of the tier's manifest parent -- what the own set was derived
    /// AGAINST.
    pub parent_tip: String,
    /// Computed: `child~|own|`, verified below.
    pub old_base: String,
    /// The tier's own commits (oids, oldest first as `git cherry` lists them).
    pub own: Vec<String>,
}

/// Derive a tier's own commits and its old base, as documented in the module
/// header: cherry against the parent's CURRENT tip, verify the own set is the
/// child's first-parent tail, refuse (`AmbiguousOldParent` / `NotLinear`)
/// rather than guess.
pub(crate) fn derive_own_set<G: GitRead>(
    m: &Manifest,
    git: &G,
    name: &str,
) -> anyhow::Result<OwnSet> {
    let t = m
        .tier(name)
        .with_context(|| format!("no [[tier]] named `{name}` in the manifest"))?;
    let home = m.repo.home_base.commit.as_str();
    let parent_ref = if t.parent == "$home_base" { home.to_string() } else { t.parent.clone() };

    let child_tip = git
        .rev_parse(name)?
        .with_context(|| format!("tier branch `{name}` does not exist (TIER_MISSING)"))?;
    let parent_tip = git
        .rev_parse(&parent_ref)?
        .with_context(|| format!("`{name}`'s parent `{parent_ref}` does not resolve"))?;

    // `git cherry`'s limit is only meaningful while it is an ancestor of the
    // head side (the STITCH_REBASED lesson: violated, it silently truncates
    // and UNDER-reports). Establish it rather than assume it.
    if !git.is_ancestor(home, name)? {
        anyhow::bail!(
            "`{name}` does not descend from the home base {} -- the topology is broken beyond \
             what this derivation can repair; inspect by hand",
            short(home)
        );
    }

    // The own set, limited at the HOME BASE -- deliberately not
    // `checks::own_limit`'s parent limit, and this is not the repeated
    // home-base mistake that function warns about: the parent tip is exactly
    // what MOVED, so it is not an ancestor of the stale child and cannot be a
    // limit at all. The wide range does over-include the old parent's commits
    // when the parent was rewritten (not merely advanced) -- and the
    // first-parent verification below is what makes that over-inclusion safe:
    // any commit in `own` that is not the child's own work breaks the tail
    // match and turns into a refusal instead of a wrong `old_base`.
    let own = git.missing_by_patch_id(&parent_tip, name, home)?;
    let own_set: BTreeSet<&str> = own.iter().map(String::as_str).collect();
    let n = own.len();

    // `own` must be exactly the last n first-parent commits, or old_base is
    // a guess.
    let mut first_parent = Vec::new();
    for i in 0..n {
        let fp = git.rev_parse(&format!("{name}~{i}"))?;
        match fp {
            Some(oid) if own_set.contains(oid.as_str()) => first_parent.push(oid),
            other => {
                if let Some(oid) = other {
                    first_parent.push(oid);
                }
                return Err(anyhow::Error::new(RestackRefusal::AmbiguousOldParent {
                    tier: name.to_string(),
                    own,
                    first_parent,
                    mismatch_at: i,
                }));
            }
        }
    }
    let old_base = git
        .rev_parse(&format!("{name}~{n}"))?
        .with_context(|| format!("`{name}~{n}` (the computed old base) does not resolve"))?;

    // With the tail verified, the range is first-parent-linear in its own
    // commits -- but a merge WITHIN the tail would still replay wrong.
    let merges = git.merges_in(&format!("{old_base}..{name}"))?;
    if !merges.is_empty() {
        return Err(anyhow::Error::new(RestackRefusal::NotLinear {
            tier: name.to_string(),
            merges,
        }));
    }

    Ok(OwnSet { child_tip, parent_tip, old_base, own })
}

/// The derivation for one tier, as documented in the module header.
fn plan_one<G: GitRead>(
    m: &Manifest,
    git: &G,
    name: &str,
    provisional: bool,
) -> anyhow::Result<RestackPlan> {
    let t = m.tier(name).expect("caller verified the tier exists");
    let OwnSet { child_tip, parent_tip, old_base, own } = derive_own_set(m, git, name)?;
    let n = own.len();

    let backup = (plan::backup_refname(&plan::backup_stamp(), name), child_tip.clone());

    let parent_words = if t.parent == "$home_base" {
        format!("$home_base ({})", m.repo.home_base.describes)
    } else {
        format!("`{}`'s tip", t.parent)
    };
    let steps = vec![Step::new(
        format!(
            "rebase `{name}`: replay its {n} own commit(s) onto {} ({parent_words})",
            short(&parent_tip)
        ),
        RepoSide::Wine,
        Action::RebaseOnto {
            newbase: parent_tip.clone(),
            upstream: old_base.clone(),
            branch: name.to_string(),
        },
    )];
    let mutations = vec![RefMutation {
        repo: RepoSide::Wine,
        refname: format!("refs/heads/{name}"),
        expected_before: Some(child_tip),
        after: PlannedValue::Unknown {
            reason: "the rebase mints new commits; the tip is unknowable until it runs".into(),
        },
    }];

    let mut notes = Vec::new();
    notes.push(format!("old base (computed, never an argument): {}", short(&old_base)));
    for (oid, subject) in git.log_subjects(&format!("{old_base}..{name}"))? {
        notes.push(format!("own: {} {subject}", short(&oid)));
    }
    if provisional {
        notes.push(
            "provisional: derived while an ancestor tier in this batch still awaits its restack; \
             re-derived (and re-shown) after the ancestor moves"
                .to_string(),
        );
    }

    let plan = Plan::new("restack", steps, mutations, vec![backup.clone()], notes);
    Ok(RestackPlan {
        plan,
        tier: name.to_string(),
        parent_tip,
        old_base,
        own,
        backup,
        provisional,
    })
}

#[derive(Debug)]
pub struct RestackOutcome {
    pub outcome: ApplyOutcome,
    /// The rebased tip that passed the postcondition.
    pub new_tip: String,
}

/// Run one restack plan through the shared engine, then enforce the
/// postcondition. On patch-id drift the branch is restored from its backup
/// (detach -> CAS -> re-attach; each step is a `GitMut` verb, so the restore
/// is as reviewable in the source as the rebase was in the plan) and the
/// failure is returned loudly as `PostconditionDrift`.
pub fn apply_restack(rp: &RestackPlan, ctx: &ApplyCtx) -> anyhow::Result<RestackOutcome> {
    let outcome = match plan::apply(&rp.plan, ctx) {
        Ok(o) => o,
        Err(e) => {
            // A conflicted rebase is a legitimate TERMINAL state (the design
            // doc's "conflict handling"): the engine's error already names the
            // journal, the backup and the resolve/--continue/--abort commands;
            // the one restack-specific addition is that the postcondition
            // never ran.
            let extra = matches!(e, ApplyError::Step { .. }).then(|| {
                format!(
                    "after the rebase completes (or aborts), re-run `sodactl restack {}` -- it \
                     is a no-op when the tier is current, and re-verifies it",
                    rp.tier
                )
            });
            let err = anyhow::Error::new(e);
            return Err(match extra {
                Some(x) => err.context(x),
                None => err,
            });
        }
    };

    let refname = format!("refs/heads/{}", rp.tier);
    let new_tip = ctx
        .wine_read
        .rev_parse_raw(&refname)?
        .with_context(|| format!("{refname} vanished after the rebase"))?;

    // The postcondition: patch-id set equality of the child's own commits,
    // before and after, plus a count. Both `git cherry` limits satisfy the
    // ancestor invariant BY CONSTRUCTION here: `old_base` is `backup~n`, and
    // `parent_tip` is what the branch was just rebased onto.
    let n = rp.own.len();
    let count = ctx.wine_read.count_commits(&format!("{}..{}", rp.parent_tip, rp.tier))?;
    let lost = ctx.wine_read.missing_by_patch_id(&rp.tier, &rp.backup.0, &rp.old_base)?;
    let alien = ctx.wine_read.missing_by_patch_id(&rp.backup.0, &rp.tier, &rp.parent_tip)?;
    if count as usize == n && lost.is_empty() && alien.is_empty() {
        return Ok(RestackOutcome { outcome, new_tip });
    }

    // Drift: restore before reporting. Three explicit steps because the
    // rebase left HEAD attached to the branch: detach first so the worktree
    // moves cleanly, CAS the ref back (expected = the drifted tip we just
    // observed, so an external writer still loses to the ref lock), then
    // re-attach -- leaving the repo as if the rebase had merely checked the
    // branch out and changed nothing.
    let restore_failed = (|| -> Result<(), String> {
        ctx.wine.checkout_detach(&rp.backup.1).map_err(|e| e.to_string())?;
        ctx.wine
            .update_ref_cas(&refname, &rp.backup.1, Some(&new_tip))
            .map_err(|e| e.to_string())?;
        ctx.wine.checkout_branch_reset(&rp.tier, &rp.backup.1).map_err(|e| e.to_string())
    })()
    .err();

    Err(anyhow::Error::new(PostconditionDrift {
        tier: rp.tier.clone(),
        expected: n,
        count,
        lost,
        alien,
        backup: rp.backup.0.clone(),
        restore_failed,
    }))
}
