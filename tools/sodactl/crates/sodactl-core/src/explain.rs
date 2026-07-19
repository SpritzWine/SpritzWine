//! The finding-code catalogue: `docs/BRANCHING.md` as a queryable structure.
//!
//! A `Finding` carries a summary and a remedy, which is enough to act but not
//! enough to *decide*. This adds the third thing: why the finding matters -- the
//! failure it predicts. That is the part that lives in a 450-line prose document
//! today, which means in practice it lives nowhere, because nobody re-reads a
//! document to interpret a lint code.
//!
//! Cheapest surface in the whole tool and plausibly the most used, because it is
//! the one a model can consult before proposing a destructive fix.
//!
//! Deliberately static data with no git access: an explanation that varies with
//! repository state is a status report wearing a costume.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CodeDoc {
    pub code: &'static str,
    /// Matches `Finding::severity` for the same code. Duplicated on purpose:
    /// `explain` must answer without a repo, so it cannot read it off a finding.
    pub severity: &'static str,
    /// One line. What the tool observed.
    pub means: &'static str,
    /// The failure this predicts if ignored. The reason the code exists.
    pub why_it_matters: &'static str,
    /// How to clear it, and what NOT to do -- several of these have a plausible
    /// wrong fix that corrupts history.
    pub remedy: &'static str,
}

/// Every code `reconcile` and the rr-cache reader can emit.
///
/// Keeping this beside the emitters rather than inside them is a deliberate
/// trade: it can drift, so `catalogue_covers_emitted_codes` in `lib.rs` fails
/// the build if a code is documented here that no emitter uses. The reverse
/// direction cannot be checked without parsing source, and is the reason a new
/// finding code should land in the same commit as its entry here.
pub const CATALOGUE: &[CodeDoc] = &[
    CodeDoc {
        code: "TIER_MISSING",
        severity: "blocker",
        means: "The manifest declares a tier branch that does not exist in the wine repo.",
        why_it_matters: "Every derived answer about that tier -- its commit set, its children's \
staleness, whether a stitch carries it -- is silently unanswerable. Worse, a stitch or consumer \
naming it will fail mid-replay, leaving the tree detached and mid-rebase.",
        remedy: "Either the branch was deleted and should be restored from `refs/sodactl/backup/` \
or the reflog, or the tier is genuinely retired and the `[[tier]]` entry should be removed. Do not \
recreate it from a consumer -- consumers are rebased copies, so their commits have different SHAs \
and re-authoring from one loses the tier's real history.",
    },
    CodeDoc {
        code: "TIER_CHILD_STALE",
        severity: "drift",
        means: "A tier does not contain the current tip of its declared parent tier -- it was \
authored on an older version of that parent.",
        why_it_matters: "This is the central defect of a stacked-tier model and it is completely \
silent. The child still builds, still stitches, still pours. It simply carries the parent as it \
was, so every consumer generated from it ships without whatever landed on the parent since. The \
tiers are stacked, so one stale child hides an arbitrary amount of missing work downstream.",
        remedy: "Restack the child onto the parent's new tip. Critically, the OLD parent tip must \
be computed from the child's own commit set, never typed from memory or the reflog: a wrong `<old>` \
duplicates the parent's commits into the child, and every consumer then carries a doubled commit \
with nothing checking for it. The post-condition that catches this is patch-id set equality of the \
child's own commits before and after.",
    },
    CodeDoc {
        code: "TIER_UNPUSHED",
        severity: "hygiene",
        means: "A tier has commits not present on its tracking remote.",
        why_it_matters: "Tiers are the authored source of truth -- everything else in the repo is \
derived and regenerable. Unpushed tier commits are the only genuinely unrecoverable state in the \
system. They also mean a CI runner or a fresh clone regenerates a consumer that differs from the \
one on this machine, with no signal that it did.",
        remedy: "Push, submodule first. The tool never pushes: this project's workflow is that \
Florian pushes by hand (no ssh-agent, Codeberg only), so `sodactl` prints the ordered command list \
and stops. Prefer `--force-with-lease` over `--force` wherever a force is needed.",
    },
    CodeDoc {
        code: "TIER_NO_REMOTE",
        severity: "hygiene",
        means: "No tracking ref exists for a tier the manifest says has a remote.",
        why_it_matters: "Drift against the remote becomes undetectable rather than zero. The \
absence of a finding then reads as 'in sync', which is the failure mode this code exists to \
prevent.",
        remedy: "Push the branch to create the tracking ref, or drop `remote` from the tier's \
manifest entry to declare it local-only and make the silence deliberate.",
    },
    CodeDoc {
        code: "STITCH_MISSING",
        severity: "drift",
        means: "A declared stitch branch does not exist.",
        why_it_matters: "A stitch is a linearization of sibling tiers, and the sibling tiers have \
no single ref whose ancestry contains all of them. Without the stitch, the full-mix consumer cannot \
be poured at all.",
        remedy: "Rebuild it by cherry-picking the manifest's declared order onto the base tier. The \
order is load-bearing where tiers touch the same files -- `soda-ntdll` and `soda-fsync` both live \
in `dlls/ntdll/unix/` -- so take it from the manifest rather than reconstructing it.",
    },
    CodeDoc {
        code: "STITCH_STALE",
        severity: "drift",
        means: "A stitch is missing commits from one of its constituent tiers, compared by \
patch-id.",
        why_it_matters: "The stitch was built before that tier moved, so the full-mix consumer \
poured from it is missing work that is sitting finished on the tier. Note the comparison: a stitch \
is built by cherry-pick, so the tier's original commits are NEVER ancestors of it. Asking this \
question with `merge-base --is-ancestor` returns a confident wrong answer -- it did, and it \
produced four false 'carried' verdicts. Patch-id equivalence is the only correct primitive for any \
replayed or cherry-picked artifact, and the same applies to consumers.",
        remedy: "Re-stitch from scratch. It is a derived scratch branch rebuilt in seconds; there \
is nothing in it worth salvaging. Restack any stale tier children first, or the fresh stitch \
linearizes stale inputs.",
    },
    CodeDoc {
        code: "STITCH_LOCAL_ONLY",
        severity: "hygiene",
        means: "The stitch is declared local-only, so it has no remote to compare against.",
        why_it_matters: "Informational, and the expected state. A stitch is a derived artifact; \
pushing it would create a fifth ref people could mistake for a source of truth. The cost is that \
nothing can tell you remotely that it is stale -- which is exactly what STITCH_STALE covers \
locally.",
        remedy: "None. This is the declared policy, surfaced so the absence of drift detection is \
visible rather than assumed.",
    },
    CodeDoc {
        code: "PINNED_CONSUMER_UNTAGGED",
        severity: "blocker",
        means: "The superproject pins this consumer, but the consumer's tip carries no tag.",
        why_it_matters: "The single highest-consequence finding in the tool. Consumers are \
force-pushed on every regeneration. A gitlink pinning a raw consumer SHA breaks the moment the \
branch is re-poured: the old commit falls out of the branch's history, the remote stops advertising \
it, and every clone and every CI run dies with `Fetched in submodule path 'wine', but it did not \
contain <sha>`. The repository is already unbuildable at that point, and the commit that caused it \
looks innocent.",
        remedy: "Tag the consumer tip on the `<component>-<winebase>-<version>` scheme, push the \
tag, then pin the TAG -- both the gitlink and `.gitmodules branch=`, in the same commit. This is \
the one place tagging is mandatory outside a release cut: routine pours earn no tag, but a pour \
something pins does, even on a day nothing is being released.",
    },
    CodeDoc {
        code: "UNMODELLED_BRANCH",
        severity: "hygiene",
        means: "A local branch exists that the manifest does not model as a tier, stitch or \
consumer.",
        why_it_matters: "Reported rather than ignored on principle: an unmodelled branch is either \
dead and should go, or live and undocumented, and those need opposite actions. Silently filtering \
them is how a real consumer ends up outside every consistency check the tool performs.",
        remedy: "Add it to the manifest or delete it. Leaving it to generate a permanent finding is \
the worst of the three -- a lint everyone has learned to skip is a lint that is off.",
    },
    CodeDoc {
        code: "RR_NO_POSTIMAGE",
        severity: "hygiene",
        means: "A tracked rerere cache entry has a `preimage` but no `postimage`: a conflict that \
was recorded but never resolved.",
        why_it_matters: "rerere replays a resolution only when it has a postimage. An entry without \
one is dead weight that guarantees the same conflict stops every future replay, forever, on every \
machine -- while the cache's mere presence implies the conflict is handled.",
        remedy: "Either reproduce the conflict and resolve it so a postimage is recorded and \
committed, or prune the entry. Pruning is safe and loses only the preimage record. Decide once, \
per entry, rather than rediscovering the choice mid-replay.",
    },
    CodeDoc {
        code: "RR_STRAY_THISIMAGE",
        severity: "hygiene",
        means: "A tracked rerere cache entry carries a `thisimage` file.",
        why_it_matters: "`thisimage` is transient state from an in-progress merge. Its presence in \
a committed cache means someone copied `.git/rr-cache` out mid-conflict. Harmless to replay, but it \
is evidence the cache was captured from an unclean tree, which puts the neighbouring entries under \
suspicion too.",
        remedy: "Delete the `thisimage` files and commit. Capture the cache after a replay \
completes, not during one.",
    },
    CodeDoc {
        code: "RR_CACHE_MISSING",
        severity: "drift",
        means: "The superproject has no tracked `tools/rr-cache` directory.",
        why_it_matters: "Every conflict resolution ever made by hand is then re-made by hand, on \
every replay, by every person. Two of this project's recurring conflicts (`configure` modify/delete \
and the branding icon) are additionally of a class rerere cannot learn at all, so an absent cache \
is not merely slower -- it is what turns a routine pour into a manual merge session.",
        remedy: "Restore it from git history, or accept that this checkout cannot regenerate a \
consumer reproducibly and do the pour where the cache is.",
    },
];

pub fn lookup(code: &str) -> Option<&'static CodeDoc> {
    // Case-insensitive: a model will reasonably ask about `tier_child_stale`.
    CATALOGUE.iter().find(|d| d.code.eq_ignore_ascii_case(code))
}
