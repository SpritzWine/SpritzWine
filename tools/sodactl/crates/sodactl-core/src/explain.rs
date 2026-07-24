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
/// trade: it can drift, so `catalogue_covers_emitted_codes` in the mcp crate
/// fails the build in BOTH directions -- a code documented here that no
/// emitter uses, and a code passed to `Finding::new` that has no entry here.
/// The second direction leans on a style invariant (the code literal sits on
/// the line of the `Finding::new(` call or the next one) that the test asserts
/// rather than assumes, so a formatting drift fails loudly instead of
/// silently skipping call sites.
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
        remedy: "`sodactl restack <tier> --apply` (add `--all-descendants` to repair a whole \
stale subtree in dependency order). Critically, the OLD parent tip must be computed from the \
child's own commit set, never typed from memory or the reflog: a wrong `<old>` duplicates the \
parent's commits into the child, and every consumer then carries a doubled commit with nothing \
checking for it. The verb computes it, backs the branch up under `refs/sodactl/backup/`, and \
enforces the post-condition that catches the trap -- patch-id set equality of the child's own \
commits before and after -- restoring from the backup on drift.",
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
        code: "TIER_DIVERGED",
        severity: "drift",
        means: "A tier is both ahead of and behind its tracking remote -- it was rewritten \
(restacked) locally while the remote still advertises the pre-restack tip.",
        why_it_matters: "This is the expected shape immediately after a restack, not an accident \
-- but it is also exactly the state a plain `git push` cannot resolve: git rejects it as \
non-fast-forward, and the confused recovery that follows is where people reach for `--force` and \
destroy remote commits that were never theirs to drop. The two ahead/behind counts describe ONE \
rewrite, not two divergent lines of work, which is why this is Drift and not a conflict.",
        remedy: "`git -C wine push --force-with-lease codeberg <tier>` -- after confirming the \
remote's extra commits are your own pre-restack tips and nothing more. `--force-with-lease` fails \
if someone moved the remote meanwhile; plain `--force` does not, and that is the entire \
difference. Florian pushes; `sodactl push` prints the ordered command list with the lease flag \
already in place.",
    },
    CodeDoc {
        code: "TIER_BEHIND_REMOTE",
        severity: "blocker",
        means: "A tier is strictly behind its tracking remote with no local commits of its own -- \
the remote carries authored work this clone does not.",
        why_it_matters: "Tiers are the authored source of truth, so every pour from this clone \
silently omits whatever the remote is ahead by. Worse, both reflex remedies destroy it: a restack \
from here rewrites the tier without the missing work, and a force-push then deletes it from the \
remote too. This is the one tier state in which pushing anything is the wrong move.",
        remedy: "Look before touching: `git -C wine fetch <remote> && git -C wine log \
<remote>/<tier> ^<tier>`. If the extra commits are genuinely authored work, fast-forward onto \
them (`git -C wine merge --ff-only <remote>/<tier>`); if this clone is simply stale, that is the \
finding doing its job. Do NOT force-push, and do not restack until the tier is current.",
    },
    CodeDoc {
        code: "TIER_NOT_LINEAR",
        severity: "drift",
        means: "A tier contains merge commits above its declared parent -- it is not the straight \
line BRANCHING.md promises.",
        why_it_matters: "Every consumer is produced by replaying the tier with `rebase --onto`, \
and a rebase linearizes: the merge is flattened away, so what a consumer receives is no longer \
what the tier's history says it carries. The tool's own comparisons are also merge-blind \
(`git cherry` is `--no-merges`), so a merge inside a tier quietly degrades staleness answers \
everywhere downstream -- it once fabricated a CONSUMER_EXCLUDE_VIOLATION all by itself.",
        remedy: "`git -C wine rebase <parent> <tier>` to linearize, then re-stitch \
(`sodactl stitch soda-full --apply`) and re-pour anything downstream. `sodactl restack` is NOT \
the tool here -- the tier may be perfectly current, just non-linear -- so back the branch up \
first if doing it by hand.",
    },
    CodeDoc {
        code: "TIER_PARENT_EDGE_WRONG",
        severity: "hygiene",
        means: "The range between a tier and its declared parent contains a commit carrying \
ANOTHER declared tier's subject prefix -- the declared parent edge appears to sit lower than the \
real one.",
        why_it_matters: "Ancestry cannot catch this: a tier really stacked on `soda-glorious` \
that declares `soda-core` still passes the ancestor check, because soda-core is below \
soda-glorious. But every replay built on the declared edge silently drags the intermediate tier \
along -- including into consumers that explicitly exclude it.",
        remedy: "`git -C wine log --oneline <parent>..<tier>` and read whose commits sit in the \
range; if another tier's do, correct `parent =` in sodactl.toml. Subject prefixes are a \
convention, not a guarantee, which is why this is Hygiene: it is evidence to check, not a \
verdict.",
    },
    CodeDoc {
        code: "STITCH_MISSING",
        severity: "drift",
        means: "A declared stitch branch does not exist.",
        why_it_matters: "A stitch is a linearization of sibling tiers, and the sibling tiers have \
no single ref whose ancestry contains all of them. Without the stitch, the full-mix consumer cannot \
be poured at all.",
        remedy: "`sodactl stitch <name> --apply` rebuilds it: `checkout -B` from the base tier, \
then the manifest's declared cherry-pick order, patch-id-verified afterwards. The order is \
load-bearing where tiers touch the same files -- `soda-ntdll` and `soda-fsync` both live in \
`dlls/ntdll/unix/` -- which is exactly why the verb takes it from the manifest rather than having \
anyone reconstruct it.",
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
        remedy: "Re-stitch from scratch: `sodactl stitch <name> --apply`. It is a derived scratch \
branch rebuilt in seconds; there is nothing in it worth salvaging. Restack any stale tier children \
first (`sodactl restack`), or the fresh stitch linearizes stale inputs -- the verb warns per tier \
when that is about to happen.",
    },
    CodeDoc {
        code: "STITCH_REBASED",
        severity: "drift",
        means: "The stitch no longer descends from its declared base tier, so the patch-id \
staleness comparison was NOT run -- its answer would have been silently wrong.",
        why_it_matters: "`git cherry <upstream> <head> <limit>` is only meaningful while the \
limit is an ancestor of the head; otherwise it truncates the comparison set with exit 0 and no \
warning. Demonstrated on the live repo: the same staleness question answered '1 missing' with \
the broken invariant and '3 missing' without it. Declining to answer and saying why beats \
reporting a confident number that means nothing -- so the check establishes the precondition \
instead of assuming it, and this finding is what it says when the precondition fails.",
        remedy: "`sodactl stitch <name> --apply`. A stitch is a derived scratch branch: \
rebuilding it on the current base tier takes seconds, salvages nothing because there is nothing \
worth salvaging, and restores the invariant every later staleness answer depends on.",
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
        code: "CONSUMER_BASE_UNRESOLVABLE",
        severity: "blocker",
        means: "A consumer declares a base ref that does not resolve in this clone.",
        why_it_matters: "The manifest hands bases verbatim to `rebase --onto`, and a pour fails \
PARTWAY on an unresolvable one -- after `checkout -B` has already repointed the consumer branch, \
leaving the tree detached and mid-rebase. Catching it read-only, before anything moves, is the \
entire value of the check. The live gotcha: wine releases are TAGS, and the winehq refspec only \
fetches branches, so `git fetch` can be run forever without ever making `wine-11.13` resolve.",
        remedy: "First check whether the base is a tag: `git -C wine rev-parse <base>` vs its \
bare tail. Then either fix `base =` in sodactl.toml or `git -C wine fetch <remote> --tags`. A \
bare `git fetch <remote>` is the remedy that can never fix the tag case -- run the diagnosis \
first.",
    },
    CodeDoc {
        code: "CONSUMER_CURRENCY_UNKNOWN",
        severity: "hygiene",
        means: "A consumer has no generation ledger entry, so whether it is current cannot be \
answered at all.",
        why_it_matters: "A consumer is a cross-base replay: ancestry sees only new SHAs and \
patch-ids legitimately change under conflict resolution, so both classical primitives give \
confident WRONG answers here -- measured on the live repo, `git cherry` called 3 of 7 commits \
absent on a freshly, correctly poured consumer. Reporting the unknown as stale would be that \
false positive; reporting nothing would render 'cannot tell' as 'fine'. Hygiene rather than \
Drift on purpose: it fires unconditionally for every ledger-less consumer, and at Drift the \
gate could never be green, which teaches everyone to ignore the severity that carries real \
drift.",
        remedy: "`sodactl regen <consumer> --apply` -- the pour records its inputs and result \
in `[ledger.<consumer>]`, which is what makes currency decidable from then on. Nothing to do \
per-finding beyond that; the code retires itself the first time the verb pours the consumer.",
    },
    CodeDoc {
        code: "CONSUMER_NOT_ON_BASE",
        severity: "blocker",
        means: "A consumer does not descend from its declared base -- it was poured onto \
something else.",
        why_it_matters: "This is the one consumer question ancestry CAN answer: the replayed \
commits get new SHAs, but the base underneath them is a genuine ancestor of the result. Failing \
it means the branch is not what the manifest says it is -- built on the wrong wine, or on a \
base that has since been redefined -- and everything downstream (the pin, the tag, the build) \
inherits the mismatch silently.",
        remedy: "`sodactl regen <consumer> --apply` re-pours onto the declared base. If the \
DECLARATION is what changed -- the consumer was deliberately moved to a new base -- update \
`base =` in sodactl.toml first, so the pour and the record move together.",
    },
    CodeDoc {
        code: "CONSUMER_EXCLUDE_VIOLATION",
        severity: "blocker",
        means: "A consumer carries commits from a tier its manifest entry excludes -- present \
by patch-id, by subject prefix, or both (the finding says which).",
        why_it_matters: "Exclusions encode hard incompatibilities, not preferences: soda-fsync \
in a Valve-based chain is 'a conflict looking for a place to happen' because proton_11.0 \
already carries its own fsync. A violated exclusion ships two implementations of the same \
mechanism in one tree. Two detection signals on purpose -- patch-id proves presence exactly, \
and the subject scan catches a commit whose diff the replay legitimately rewrote.",
        remedy: "`sodactl regen <consumer> --apply` re-pours without the excluded tier. If the \
tier is newly WANTED in this chain, remove it from `exclude` in sodactl.toml instead -- but \
check the incompatibility that put it there first; the manifest records the reason.",
    },
    CodeDoc {
        code: "CONSUMER_MISSING_TIER",
        severity: "drift",
        means: "Nothing of a constituent tier is detectable in the consumer: no commit matches \
by patch-id AND none carries the tier's subject prefix.",
        why_it_matters: "The one sound staleness inference a cross-base replay supports without \
a ledger, and it is deliberately hard to trigger: legitimate conflict resolution changes diffs \
but not subjects, so both signals going dark together means the tier is genuinely absent -- \
the pour predates the tier, or dropped it mid-replay. The consumer still builds; it just \
silently lacks a slice of what it is supposed to carry.",
        remedy: "`sodactl regen <consumer> --apply` re-pours from the current soda tip. If the \
tier is INTENDED to be absent from this chain, say so in the consumer's `exclude` list -- an \
undeclared absence and a declared one are different facts.",
    },
    CodeDoc {
        code: "CONSUMER_AHEAD_OF_TAG",
        severity: "hygiene",
        means: "A consumer branch tip sits past its newest family tag.",
        why_it_matters: "BRANCHING.md declares this the NORMAL state -- 'a pour that has not \
passed the gate yet'. Its job is to make the pin findings legible: seeing the gap between tip \
and tag explains WHY a pin still points at the older tagged commit. It deliberately stays \
silent when the untagged tip is also pinned, because the pin cluster already blocks on that \
and two rows for one fact read as two problems.",
        remedy: "No action while the pour is still being tested. To ship it: `sodactl tag \
<family> <n> --apply`, push the tag (`pin` verifies the remote serves it and refuses an \
unpushed one; `sodactl push` derives the command), then `sodactl pin <tag> --apply`. Do not \
tag reflexively -- routine pours earn no tag; only a pour something pins, or a release cut, \
does.",
    },
    CodeDoc {
        code: "TAG_COLLISION",
        severity: "hygiene",
        means: "Two family tags of the same component name the same commit, and no \
`[[policy.tag_equivalence]]` declares them equivalent.",
        why_it_matters: "The state is ambiguous between an intended fact and a release defect, \
and the tool cannot tell which: either the family rolled without this component needing a \
re-pour (the live instance, documented as intended -- regeneration and tagging are separate \
decisions), or a version was cut that ships the previous version's bits. Silence would bury \
the defect case; a Blocker would false-alarm on the intended one.",
        remedy: "If intended, declare it: add the tags to `[[policy.tag_equivalence]]` in \
sodactl.toml with a reason -- the finding retires and the intent is recorded. If a re-pour \
was missed: `sodactl regen <consumer> --apply`, then re-cut the tag onto the fresh pour \
(`sodactl tag <family> <n> --force --apply`).",
    },
    CodeDoc {
        code: "TAG_ON_STALE_TARGET",
        severity: "drift",
        means: "The NEWEST soda family tag names a stitch that is missing commits which have \
since landed on its constituent tiers (compared by patch-id, deduplicated across tiers).",
        why_it_matters: "Only the newest tag is checked, deliberately: a shipped tag is a \
historical record and is SUPPOSED to fall behind once the tiers move -- checking all of them \
would mint one permanent Drift row per release forever, and a finding that can never clear \
trains the reader to skip the severity. The newest tag is different: it is the one about to \
be pinned, shipped, or re-cut from, and pinning it ships without the work that moved.",
        remedy: "Pour a fresh stitch and tag THAT: `sodactl stitch soda-full --apply && \
sodactl tag soda <n+1> --apply`. Never move the existing tag onto new content -- it is a \
record of what shipped, and rewriting it silently changes what an old pin means.",
    },
    CodeDoc {
        code: "TAG_FAMILY_SKEW",
        severity: "drift",
        means: "The newest version number differs across tag families -- one component's \
releases lag the others.",
        why_it_matters: "Whether that is a problem is a POLICY question, not a derivable fact: \
if the trailing integer means 'cut together, use together', skew is a real release defect; if \
a component may legitimately lag between cuts, this is noise on every partial release. The \
manifest's `family_skew` key records the answer once (the live value is `drift`, and its own \
comment says it encodes an assumption, not an answer). Structurally incomplete either way: \
`prater-*` lives in a third repository this tool cannot see.",
        remedy: "Decide once and record it: set `[policy] family_skew` in sodactl.toml to \
`blocker`, `drift` or `off`. Then either cut the lagging component (`sodactl tag <family> \
<n> --apply`) or accept the lag as the recorded policy allows.",
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
something pins does, even on a day nothing is being released. While the pin bump is only STAGED \
this is demoted to Drift, for PIN_UNTAGGED's reason: it describes what committing would publish, \
and CI cannot observe the index -- the two emitters demote together.",
    },
    CodeDoc {
        code: "PIN_UNVERIFIABLE",
        severity: "drift",
        means: "A consumer is declared pinned_by_superproject, but the superproject's gitlink \
for the wine submodule could not be read; the reason is quoted in the finding.",
        why_it_matters: "Every Blocker in the pin cluster is a statement about the gitlink. When \
the gitlink is unreadable those checks do not pass -- they do not RUN, which is a different \
thing. Rendering that as a clean report is the 'unknown presented as fine' failure: the next \
pour can still orphan the pinned SHA and break every clone, and nothing will have said a word \
beforehand.",
        remedy: "Run sodactl from the superproject checkout so `HEAD:wine` and the index gitlink \
resolve (the default manifest path does this). If it fires even there, the quoted reason names \
what actually failed.",
    },
    CodeDoc {
        code: "PIN_ABSENT",
        severity: "blocker",
        means: "A consumer is declared pinned_by_superproject, but the superproject records no \
gitlink at all -- neither committed nor staged.",
        why_it_matters: "The declaration and the repository disagree at the coarsest level: \
either the submodule was deregistered, or the manifest promises a pin that never existed. Both \
mean the guarantee the pin exists for -- 'this exact wine tree is what was built and tested' -- \
is currently backed by nothing, and `build.sh` has no recorded commit to check out.",
        remedy: "Check out a TAGGED consumer commit in wine, then `git add wine`, committed \
together with the `.gitmodules branch=` line. `sodactl pin <tag> --apply` performs exactly this \
sequence, with the refusals that keep the two edits in one commit.",
    },
    CodeDoc {
        code: "PIN_STAGED_UNCOMMITTED",
        severity: "drift",
        means: "The wine gitlink staged in the index differs from the one HEAD records -- a pin \
bump is midway through.",
        why_it_matters: "Nothing is broken yet; one `git commit` decides. Every other pin finding \
is conditioned on this state: while the bump is only staged they describe what committing WOULD \
publish, demoted from Blocker because CI can never observe the index (it checks out HEAD, where \
index == HEAD). The residual risk is a habitual `git commit -a` publishing a pin that has not \
been tagged or pushed yet -- at which point the demoted findings land for real.",
        remedy: "Finish or back out. Finish: tag the new tip and update `.gitmodules branch=` in \
the SAME commit -- `sodactl pin <tag> --apply` does both. Back out: `git restore --staged wine`. \
This finding's remedy is the only 'unstage' in the tool, which is why it has its own code.",
    },
    CodeDoc {
        code: "PIN_UNTAGGED",
        severity: "blocker",
        means: "The judged gitlink (staged if a bump is midway, committed otherwise) names a \
commit with no tag pointing at it.",
        why_it_matters: "Consumers are force-pushed on every pour. An untagged pinned SHA falls \
out of the branch on the next regeneration, the remote stops advertising it, and every fresh \
clone and CI run dies with `did not contain <sha>` -- discovered at their fetch, long after the \
innocent-looking commit that caused it. Tags are the only refs a pour never moves, which is why \
a pinned commit must carry one. Same failure PINNED_CONSUMER_UNTAGGED reports from the \
consumer's side; this is the gitlink's own view, and it is the one CI reproduces.",
        remedy: "Cut the tag on the family scheme and pin the TAG: `sodactl tag <family> <n> \
--apply` tags the component tip, then push the tag (`pin` verifies the remote serves it and \
refuses an unpushed one; `sodactl push` derives the command), then `sodactl pin <tag> --apply` \
writes the gitlink and `.gitmodules branch=` in one commit. While the bump is only staged this \
is demoted to Drift -- it describes what committing would publish, not what CI can see.",
    },
    CodeDoc {
        code: "PIN_NOT_ON_REMOTE",
        severity: "blocker",
        means: "The judged gitlink names a commit that no remote-tracking branch contains -- as \
far as this clone can tell, the SHA exists only on this disk.",
        why_it_matters: "The pin is what every other clone fetches; a pin the remote cannot serve \
is a broken repository for everyone else, discovered at their fetch rather than your commit. \
This is the same failure CI probes Codeberg for after the commit, caught one step earlier and \
offline -- and because it is offline, a stale tracking ref can produce a false positive, which \
the finding says in its own text rather than claiming more than the data supports.",
        remedy: "Push the TAG naming the pin, not just the branch: consumers are force-pushed, so \
a branch push keeps the SHA fetchable only until the next pour reintroduces the identical \
failure. Same remedy as PIN_UNTAGGED because it is the same mechanism. `sodactl push` prints the \
ordered commands; Florian pushes, submodule first.",
    },
    CodeDoc {
        code: "GITMODULES_BRANCH_MISSING",
        severity: "drift",
        means: "`.gitmodules` declares no `submodule.wine.branch`, so nothing records which \
consumer ref the pin is supposed to track.",
        why_it_matters: "The gitlink pins an exact commit but not its meaning. `submodule update \
--remote` has nothing to follow, and the next person bumping the pin reverse-engineers which \
family the commit belongs to. BRANCHING.md requires the `branch=` edit to land in the same \
commit as every pin bump precisely so the two records cannot drift apart -- absent is the \
furthest possible form of drifted.",
        remedy: "`git config -f .gitmodules submodule.wine.branch <tag-naming-the-gitlink> && \
git add .gitmodules` -- or let `sodactl pin <tag> --apply` write it, which puts it in the same \
commit as the gitlink by construction.",
    },
    CodeDoc {
        code: "GITMODULES_BRANCH_UNRESOLVABLE",
        severity: "blocker",
        means: "`.gitmodules` names a ref for the wine submodule that does not resolve in the \
wine repo at all.",
        why_it_matters: "A typo, or a tag that was deleted. Everything that consults `branch=` -- \
`submodule update --remote`, a fresh clone following the recorded ref -- fails on it, and the \
'ref not found' surfaces far from the one config line that caused it. A separate code from \
GITMODULES_BRANCH_MISMATCH because the repairs differ: a typo wants correcting, a drift wants \
re-pinning, and a message that cannot tell them apart sends someone to the wrong one.",
        remedy: "`git config -f .gitmodules submodule.wine.branch <an-existing-tag> && git add \
.gitmodules`. Re-running the pin with the intended tag also clears it -- `sodactl pin` refuses \
tags that do not exist, so the verb cannot reintroduce this.",
    },
    CodeDoc {
        code: "GITMODULES_BRANCH_MISMATCH",
        severity: "blocker",
        means: "`.gitmodules branch=` and the judged gitlink name different commits.",
        why_it_matters: "The two records of the pin now disagree, so which tree gets built \
depends on which flag a script happens to use: plain `submodule update` checks out the gitlink, \
`--remote` fetches the `branch=` ref. The tree that was built and tested and the tree that \
ships stop being the same thing, with no error anywhere -- both commands succeed.",
        remedy: "Point `branch=` at the tag naming the gitlink and stage BOTH in one commit: \
`git config -f .gitmodules submodule.wine.branch <tag> && git add .gitmodules wine`. The \
same-commit discipline is the point, and `sodactl pin <tag> --apply` enforces it.",
    },
    CodeDoc {
        code: "GITLINK_NOT_A_CONSUMER",
        severity: "blocker",
        means: "The judged gitlink names a commit contained in no declared consumer branch and \
carrying no consumer-family tag. When it is a TIER tip, the finding names which -- that is the \
actionable part.",
        why_it_matters: "The catastrophic-and-silent member of the pin cluster: `git add wine` \
performed while the checkout sat on a soda tier commits soda-without-wine as the build pin. It \
compiles nothing and fails weirdly, several steps away from the `git add` that caused it. \
Ancestry cannot ask this question (consumers are force-pushed, so their history is disposable); \
containment in a consumer branch or a consumer-family tag is the real criterion.",
        remedy: "`git -C wine checkout <consumer-tag> && git add wine` -- pin a consumer, never a \
tier. `sodactl pin <tag> --apply` refuses any tag whose commit is not a pinned consumer's tip, \
which makes this mistake unmakeable through the verb.",
    },
    CodeDoc {
        code: "WINE_HEAD_OFF_PIN",
        severity: "hygiene",
        means: "The wine checkout's HEAD is not the commit the judged pin records.",
        why_it_matters: "`build.sh` runs `git submodule update --force` (line 44), which yanks \
the checkout back to the COMMITTED pin before building -- so the next build compiles the pin, \
not the tree sitting in the working directory, and nothing says so. Normal while deliberately \
standing on a tier to author; a trap the moment you believe the checkout is what builds. \
`sodactl build` refuses on exactly this mismatch, with `--anyway` as the eyes-open override \
that names the consequence.",
        remedy: "Nothing, if the detour is deliberate. Otherwise `git -C wine checkout \
<the-pin>`, or stage/commit the pin bump so the pin and the checkout agree -- the finding is \
judged against the pin about to be PUBLISHED, so completing a correct bump clears it.",
    },
    CodeDoc {
        code: "REBASE_IN_FLIGHT",
        severity: "blocker",
        means: "A replay is half-done in the wine submodule -- rebase, cherry-pick or \
sequencer state exists in the resolved gitdir (the finding names the markers and, for a \
rebase, how far it got).",
        why_it_matters: "The non-negotiable precondition: no operation may touch refs while a \
replay is mid-flight, because the branch being replayed is already detached from its old tip \
and every ref move compounds the wreckage. `regenerate.sh` exits 1 leaving exactly this state \
behind. Every mutating verb refuses on the same probe, so the finding and the refusals cannot \
drift apart -- what `check` reports is precisely what a verb would refuse on.",
        remedy: "Finish or abort, nothing else. Resolve the conflict, then `sodactl resume` \
-- it re-verifies against the pour's journal that nothing else moved before running the \
continue -- or `git -C wine rebase --abort` to stand down. Do not `checkout` your way out; \
that strands the sequencer state for the next probe to find.",
    },
    CodeDoc {
        code: "SUBMODULE_DIRTY",
        severity: "blocker",
        means: "Tracked paths are modified in the wine submodule, beyond the \
`build_artifacts` allowlist.",
        why_it_matters: "A rebase or `checkout -B` -- the first step of every pour -- destroys \
uncommitted tracked changes without asking. The allowlist exists because this project's build \
tree is always warm: a dirty-tree guard that fires on generated files gets disabled within a \
day, and then the guard protecting real uncommitted work is gone too. So allowlisted build \
products are silent, and anything that fires here is by definition work.",
        remedy: "`git -C wine stash`, or commit the changes to a soda tier -- never pour over \
uncommitted work. If a path is actually a build product, add it to `build_artifacts` in \
sodactl.toml so the guard stays sharp for the rest.",
    },
    CodeDoc {
        code: "SUBMODULE_UNTRACKED",
        severity: "hygiene",
        means: "Untracked files exist in the wine submodule that `build_artifacts` does not \
cover.",
        why_it_matters: "A replay does not destroy untracked files, so nothing is at risk -- \
but each one is either abandoned scratch or a build product the allowlist does not know yet, \
and the allowlist only keeps the SUBMODULE_DIRTY guard honest if additions to it are \
deliberate. Unexplained files are also how the next `git add -A` on a tier picks up cruft.",
        remedy: "Delete them, or add them to `build_artifacts` in sodactl.toml if the build \
generates them -- decide per path rather than letting the list grow stale.",
    },
    CodeDoc {
        code: "WINE_HEAD_DETACHED",
        severity: "hygiene",
        means: "The wine submodule is on a detached HEAD.",
        why_it_matters: "Ambiguous on its own, informative in context: detached is the CORRECT \
resting state for a pinned build (`git submodule update` detaches by design), and also exactly \
what an aborted pour leaves behind. It is only legible next to REBASE_IN_FLIGHT -- detached \
plus in-flight is a wreck, detached alone is Tuesday. Reported plainly so the reader makes \
that call with both facts visible.",
        remedy: "Nothing, if the pin or a deliberate detour explains it. After a failed pour: \
`git -C wine rebase --abort` or re-run the pour. To author, attach to a `soda-*` tier only -- \
never to a consumer, which CHECKOUT_ON_GENERATED_BRANCH reports at a higher severity because \
commits made there are force-pushed away.",
    },
    CodeDoc {
        code: "SUPERPROJECT_DIRTY",
        severity: "hygiene",
        means: "Tracked paths are modified in the superproject working tree.",
        why_it_matters: "Only matters as a precondition: `pin` and `regen` COMMIT in the \
superproject (the gitlink bump; the rr-cache capture), and unrelated staged changes would be \
swept into those commits -- which is why `sodactl pin` refuses outright when other changes \
are staged. Reported as a count so a permanently-noisy tree does not bury the rest of the \
report.",
        remedy: "Commit or stash before running a verb that writes the superproject. The \
verbs' own refusals catch the staged case; clearing it here first just saves the round-trip.",
    },
    CodeDoc {
        code: "CHECKOUT_ON_GENERATED_BRANCH",
        severity: "drift",
        means: "The wine checkout is standing ON a generated branch -- a consumer (Drift), or \
a derived stitch (the same code at Hygiene).",
        why_it_matters: "Nothing is wrong yet; this is the guard rail, not the crash report. A \
commit made while HEAD is on a consumer is rejected by nothing: it commits cleanly, builds, \
and disappears without a trace on the next pour, because the branch is force-reset to the \
replay result and the old tip stops being reachable -- no conflict, no warning, no reflog \
entry anywhere the next person will look. A stitch loses commits the same way via re-stitch's \
`checkout -B`, milder only because stitches are rebuilt knowingly.",
        remedy: "`git -C wine checkout --detach` -- same commit, identical tree, a warm build \
survives it untouched. To author, stand on a `soda-*` tier; the safe places are detached or \
on a tier, nothing else.",
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
        code: "RR_ENTRY_SHADOWED_BY_RULE",
        severity: "hygiene",
        means: "An unresolved rr-cache entry looks (by preimage content) like a conflict in a \
path that an UNSCOPED `[[binary_resolution]]` rule already decides.",
        why_it_matters: "`regen` applies unscoped rules pre-emptively, before rerere is ever \
consulted, so such an entry can never fire -- dead weight that a human triaging the \
unresolved list will burn time re-resolving. The hedged wording is load-bearing: rerere \
stores content, never the path, so the attribution is a heuristic. And the scoping is the \
scar of a live false positive -- a rule scoped to one consumer decides that path for THAT \
pour only, and treating it as universal declared two resolutions a spritzwine pour genuinely \
needs to be prunable dead weight.",
        remedy: "Confirm the attribution against the preimage, then `sodactl rr-gc --apply` \
-- shadowed entries are pruned regardless of age. By hand: `rm -r <rr-cache>/<id>`.",
    },
    CodeDoc {
        code: "RR_UNCOMMITTED",
        severity: "drift",
        means: "Uncommitted paths exist under the tracked rr-cache -- newly learned \
resolutions live only in this working tree.",
        why_it_matters: "The tracked cache is the mechanism that makes regeneration \
reproducible ACROSS clones. A resolution sitting uncommitted here is one the CI runner and \
every future checkout rediscover by hand, mid-replay, while this machine sails through -- \
the exact asymmetry the tracked cache exists to remove. `regenerate.sh` only WARNS that it \
copied resolutions out; committing them was always the manual step that got skipped.",
        remedy: "`git add tools/rr-cache && git commit`. Pours run through `sodactl regen` \
commit their own captures (`sodactl: capture rerere resolutions ...`), so this finding mostly \
marks resolutions learned outside the verb.",
    },
    CodeDoc {
        code: "RR_CACHE_UNSEEDED",
        severity: "hygiene",
        means: "The live rerere cache in the wine gitdir is missing resolutions that the \
tracked cache carries (compared by entry id, as sets).",
        why_it_matters: "rerere only replays what its LIVE cache holds; the tracked copy in \
the superproject is inert until seeded across. Unseeded, a replay re-prompts for conflicts \
the repository already knows the answer to -- and the human assumes that is normal, which is \
how the cache silently stops earning its keep. Set membership rather than counts, because \
two disjoint sets of equal size would read as 'seeded'; and the live cache is NOT \
`wine/.git/rr-cache` -- `wine/.git` is a gitdir-pointer file, and resolving it is the \
difference between a real check and one that passes forever.",
        remedy: "Copy the tracked cache in (the finding's remedy carries the exact `cp` for \
the resolved gitdir), or let the verb do it: `sodactl regen` seeds the live cache from \
`tools/rr-cache` before every replay.",
    },
    CodeDoc {
        code: "CONSUMER_STALE_BY_LEDGER",
        severity: "drift",
        means: "A consumer's generation ledger entry matches its tip, but one of the recorded \
inputs (base, soda tip, home base) has moved since the pour.",
        why_it_matters: "A consumer is a cross-base replay, so no ancestry or patch-id comparison \
can decide its currency -- the ledger's recorded inputs are the ONLY sound basis. This finding is \
that basis working: the branch itself is exactly as poured, but pouring it again today would \
produce something different, which is the definition of stale for a generated artifact.",
        remedy: "`sodactl regen <consumer> --apply` re-pours from the current inputs and rewrites \
the ledger entry. The moved input is named in the finding, so if the movement itself is the \
surprise (a base that jumped, a soda tip someone rewrote), chase that first.",
    },
    CodeDoc {
        code: "LEDGER_CONTRADICTS_TIP",
        severity: "drift",
        means: "The ledger records a pour result that is not the consumer branch's current tip \
(or the branch does not resolve at all).",
        why_it_matters: "Consumers are generated, never authored on -- that is the invariant that \
makes force-pushing them safe. A tip that drifted from the recorded result means either someone \
committed directly on the consumer (work the next pour will silently obliterate) or the branch \
was re-poured outside `sodactl regen` (so the ledger vouches for a tree that no longer exists). \
The finding says which direction the disagreement runs, because the two want opposite repairs.",
        remedy: "If commits were added: move them to a soda tier, then re-pour. If the branch was \
re-poured by hand: `sodactl regen <consumer> --apply` re-establishes a truthful ledger. Never \
hand-edit the ledger entry to match -- it is a record of what happened, not a setting.",
    },
    CodeDoc {
        code: "LEDGER_CONTRADICTS_TAG",
        severity: "drift",
        means: "The manifest's ledger entry and the newest `gen/<consumer>/*` provenance tag \
record different results for the same consumer's pour.",
        why_it_matters: "The two records exist as belt and braces: `regen` writes both from the \
same instant, so they can only disagree if one was moved, hand-edited, or written by something \
else. At that point neither is evidence -- trusting either one is a coin flip presented as an \
audit trail.",
        remedy: "Verify the branch by hand (`git -C wine log <consumer>`), then `sodactl regen \
<consumer> --apply` to mint a fresh, consistent ledger entry + provenance tag pair. The stale tag \
stays as history; only the NEWEST tag is compared.",
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
