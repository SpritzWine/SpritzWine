# `sodactl` — SpritzWine release automation

## What this replaces

Today the release path is prose in `docs/BRANCHING.md` plus hand-typed git. The concrete sequence, in order:

1. **Restack tier children** after a tier moves — `git rebase --onto soda-core <old-soda-core> soda-glorious`, ×4 siblings. No script exists. The `<old>` argument is recoverable only from the reflog or `codeberg/<tier>`.
2. **Stitch `soda-full`** — `git checkout -B soda-full soda-core` + a cherry-pick of four ranges in a load-bearing order (`soda-ntdll` and `soda-fsync` both touch `dlls/ntdll/unix/`). Also unscripted.
3. **Regenerate a consumer** — `tools/regenerate.sh <out> <base> <soda-tip> [<home-base>]`. Does not fetch its bases. Does not commit the rr-cache it captures.
4. **Commit `tools/rr-cache`** — manual; the script only warns.
5. **Tag** — `git tag && git push codeberg <tag>`, `--force` on a re-cut.
6. **Bump the superproject pointer** — `git -C wine checkout <tag>; git add wine`, plus a `.gitmodules branch=` edit that must land in the same commit and that nothing verifies.
7. Then `build.sh` / `package.sh` / `smoke.sh` / tag-push, which are already scripted and already work.

How it goes wrong — all of these are **live defects in the repo as surveyed**, not hypotheticals:

- **Four tier children are stale right now.** `soda-core` moved to `39fe08f1bef`; `soda-glorious`, `soda-ntdll`, `soda-fsync`, `soda-demo` still descend from `79e58cb8a70`. Nothing reports this.
- **`soda-full` was stitched from the dead core and is tagged `soda-11.12-3` anyway** — the tag names a soda tree that no longer matches the tiers. `soda-full` has no remote, so nothing can even detect drift.
- **`spritzvalve-11.0-1` and `-11.0-3` point at the same commit** — "version 3" ships version-1 bits.
- **A stale `<old>` in a restack silently duplicates the parent's commit into the child**, and every downstream consumer then carries a doubled commit. There is no post-condition check anywhere.
- **Nothing records a consumer's generation inputs.** "Is `spritzwine` stale?" is currently answered by eyeballing `git log --oneline` subjects.
- **`.gitmodules branch=` vs the gitlink is unchecked.** With the wine checkout sitting on a tier tip (survey caught `+0f00b8e2373` against pin `8bf9f2c8e83`), a naive `git add wine` commits a tier tip as the consumer pin.
- **The rr-cache rots.** `cp -a` merges and never prunes; `git rerere gc` has never run. 5 of 17 entries have a `preimage` and no `postimage` — recorded-but-unresolved conflicts that re-prompt on every replay forever. 2 carry a stray `thisimage` that should never have been committed.
- **A binary conflict wedges the whole thing.** `dlls/user32/resources/oic_winlogo.ico` is line-unlearnable by rerere; `regenerate.sh` exits 1 leaving the tree mid-rebase and detached. The survey found the repo in exactly that state; by the time the headless proposal was written it had been resolved or aborted by hand, with no record of which.
- **Doc drift**: `BRANCHING.md:135` still says the upstream pour targets `winehq/wine-11.12` while the shipped tag is `spritzwine-11.13-3` on an 11.13 base.

None of this is "tedious but discoverable." Most of it is **underivable** — the facts needed to answer the question do not exist in the repo. That is the gap the tool closes.

## Recommendation

> **CORRECTED 2026-07-19 — the UI is a requirement, not an option.** The paragraph
> below deferred the TUI on frequency grounds ("how often will he run this"). That
> was the wrong axis, and it silently overrode a stated requirement. The UI's job is
> **not** the happy path — it is triage **when something is broken**: pinpointing
> directories and open issues, acting as a visual checklist, showing where to do what
> next, tracking the commit chain, and keeping a broken commit from being pushed.
>
> Consequences, because this is not just "add a renderer":
> - Findings need a **location** (worktree path, conflicted files), not just a subject ref.
> - "Where to do what next" is a **dependency-ordered action queue**, not a flat finding
>   list — restack → stitch → pour → pin → tag, where four are blocked on the first.
> - The **commit chain** (tier → stitch → consumer → pin → tag) becomes a first-class
>   object with per-link status. `ConsumerState.current` is honestly `None` today
>   because nothing records generation inputs; the UI is what makes that gap *visible*
>   rather than invisible, and it is the argument for pulling the ledger earlier.
> - A **checklist** implies persistent per-item state across invocations.
>
> **ratatui, and it stays in scope.** The rejection below ("ends by shelling out to
> `git mergetool` anyway") does not survive the reframing: a triage UI does not need to
> *be* the merge tool: showing which files conflicted, in which replay, with the
> declared `[[binary_resolution]]` rule for each, is the feature. libcosmic stays ruled
> out — that reasoning (dependency churn, remote operation over SSH) is unaffected.
>
> **Build evidence — decided 2026-07-19.** The gate checks structure *and* build
> state. Required: a `build` action in the UI/CLI, a `smoke` action, and freshness
> checks on the built artifacts.
>
> Evidence must be NEWER than what it attests to. That ordering is the whole model,
> and it inverts one of the comparisons as originally phrased — the binary being
> *older* than the last boot is the healthy case (you booted that binary); the
> unhealthy case is evidence that predates what it claims to cover:
>
> ```
> sources  →  binary  →  boot/smoke evidence  →  tag
>          ↑ binary must be newer than sources        (else BUILD_STALE)
>                   ↑ evidence newer than binary      (else BUILD_UNVERIFIED)
>                                    ↑ tag points at a tree that has evidence
>                                                     (else TAG_UNVERIFIED)
> ```
>
> Finding codes: `BUILD_MISSING` (no artifacts at all), `BUILD_STALE` (artifacts
> predate the source tree), `BUILD_UNVERIFIED` (no boot/smoke record covering this
> build), `TAG_UNVERIFIED` (a tag was cut over a tree with no passing evidence).
>
> **mtime is a heuristic, not the record.** `git checkout` rewrites mtimes, and a
> branch switch between identical trees leaves them untouched — both happened during
> the 2026-07-19 session. So mtime is the cheap first signal only; the authority is a
> stamped record keyed by **tree oid**: `{tree_oid, built_at, build_result,
> smoke_result, wine_version}`. That is the same ledger consumer-currency needs, which
> is the second argument for pulling it forward rather than deferring to v3.
>
> **Smoke is opt-out, not omitted.** `ci/smoke.sh` takes minutes, so it must be
> skippable without disabling the whole gate: `--skip-smoke` and `SODACTL_SKIP_SMOKE=1`,
> honoured by both the CLI and the `pre-push` hook. Skipping is *recorded* — a push
> gated without smoke evidence is marked as such in the ledger rather than silently
> counting as verified.

> **The push guarantee needs a mechanism, not a dashboard.** "No commit can be pushed
> broken while the tool is used" cannot be delivered by a UI, because pushes happen by
> hand from a separate shell — a screen you *could* have looked at is not a gate. Use a
> `pre-push` hook running `sodactl check --severity-min blocker` in both the
> superproject and `wine/` (the real gate; needs an install step since hooks are not
> cloned), plus `sodactl` printing the push command **only when green** (the ergonomic
> path, and it respects the rule that Florian pushes). Open question: `check` currently
> means *structurally sound*, not *compiles* — a hook that runs `build.sh` would take
> minutes and get disabled. Deciding what evidence counts as "not broken" (a recorded
> successful build for this exact tree oid? a passing `smoke.sh`?) is the same missing
> ledger, and the hook should not imply a guarantee it does not make.

Build **`sodactl`**: a pure `sodactl-core` library, a `sodactl` CLI, and a `sodactl mcp` stdio server behind the same binary — and **defer the UI entirely**, with ratatui pre-selected if and only if a month of real use proves `watch sodactl status` is a thing Florian actually does. libcosmic is ruled out now and permanently: 616 lock packages behind a SHA-on-master with no crates.io release, no usable tags, no MSRV statement and ~89 commits/month of undocumented churn, for a tool run a few times a month — and half the operations live on the self-hosted runner host reached over SSH, where a GUI is useless. The in-TUI conflict screen with terminal suspend/restore is dropped too: it is ~10 days that `TestBackend` cannot test, that will wedge a terminal at least once, and that ends by shelling out to `git mergetool` anyway; detect the conflict, classify it, apply declared binary rules, print the exact command, exit. The spine is the headless proposal because it scored highest on the axes that matter here (domain automation, first-usefulness ratio, Claude-Code fit) and because the value is a `reconcile(declared, observed) -> Vec<Finding>` function plus a generation ledger — both frontend-agnostic, both unit-testable without a repo, and both worth more than every orchestration verb combined. Grafted in from the other two: the `Finding`-with-`severity()`-and-`remedy()` shape as the product surface, `[[binary_resolution]]` rules as reviewable policy for the one conflict class rerere structurally cannot learn, provenance mirrored into an annotated tag object so the ledger is not the sole authority, and backup refs + compare-and-swap before every rewrite. Dropped from the headless spine: hand-authored `commits = N` tier geometry — the manifest holds **policy, never geometry**.

## Domain model

Three sources, strictly ranked:

1. **Git is the only authority on where refs are.** Every SHA is read live, every invocation. No SHA is cached as truth.
2. **The manifest declares what git cannot know** — topology edges, policy, magic constants. Reviewed like code.
3. **The ledger records what happened** — generation inputs, written back on success, mirrored into a tag object. Treated as a *claim to be verified*, never as a fact.

### Geometry is derived, not declared

The judge is right: `commits = N` is rot with a destructive operation depending on it. Commit counts come from `git rev-list --count $(git merge-base <parent> <child>)..<child>`. `soda-core` having 2 commits is an observation, not a manifest line. The manifest declares only the **parent edge**; count, ahead/behind, ancestry and staleness all derive.

Corollary: `check` reports **unmodelled branches** rather than pretending they don't exist. The repo has 14 branches; the tier DAG models 8. `spritzvalve-full`, `spritzwine-valve`, `spritzwine-pre-1112-rebase`, `spritzwine-backup-20260703` get a `UNMODELLED_BRANCH` finding at `Hygiene` severity — visible, triageable, not silently ignored.

### Manifest schema — `tools/sodactl.toml`

```toml
schema = 1

[repo]
superproject = "."
wine         = "wine"
# The one genuinely unrecoverable constant (regenerate.sh:30).
# SHA + describe + derivation, so a reviewer can tell a bump from a typo.
home_base = { commit = "1bb0da91a5e",
              describes = "wine-11.12-79-g1bb0da91a5e",
              derivation = "6260d89930c^" }

[remotes]
codeberg   = "https://codeberg.org/SpritzWine/wine.git"
winehq     = "https://gitlab.winehq.org/wine/wine.git"
valve-wine = "/media/Data/valve-wine-bare.git"

# Build products that must not trip the dirty-tree guard.
build_artifacts = ["dlls/msxml3/xslpattern.tab.c", ".build-env-stamp", "**/*.o"]

# ---- tier DAG: `parent` is the ONLY declared geometry. Counts derive. ----
[[tier]] name = "soda-build"    parent = "$home_base"
[[tier]] name = "soda-core"     parent = "soda-build"
[[tier]] name = "soda-glorious" parent = "soda-core"
[[tier]] name = "soda-ntdll"    parent = "soda-core"
[[tier]] name = "soda-fsync"    parent = "soda-core"
[[tier]] name = "soda-branding" parent = "soda-core"
[[tier]] name = "soda-demo"     parent = "soda-build"

# Advisory metadata: drives conflict prediction and invariant assertions.
[tier.soda-ntdll.assert]
deletes         = ["dlls/ntdll/threadpool.c"]
exports_match   = { glob = "Tp*", across = ["winehq/wine-11.13", "valve-wine/proton_11.0"] }

# ---- stitches: a linearization of a tier subset, in a load-bearing order ----
[[stitch]]
name  = "soda-full"
base  = "soda-core"
order = ["soda-glorious", "soda-ntdll", "soda-fsync", "soda-branding"]

# ---- consumers: generated, force-pushed ----
[[consumer]]
name       = "spritzwine"
base       = { remote = "winehq", ref = "wine-11.13" }
soda_tip   = "soda-full"
tag_family = "spritzwine"          # wine_base derived from the base ref -> "11.13"

[[consumer]]
name       = "spritzvalve"
base       = { remote = "valve-wine", ref = "proton_11.0" }
soda_tip   = "soda-branding"
tag_family = "spritzvalve"
exclude    = ["soda-fsync"]        # BRANCHING.md:230-231, asserted not remembered

# ---- binary conflicts: rerere is line-based and can NEVER learn these ----
[[binary_resolution]]
path      = "dlls/user32/resources/oic_winlogo.ico"
strategy  = "ours"                 # soda-branding owns the glass
consumers = ["spritzwine", "spritzvalve"]
note      = "SpritzWine glass replaces the Wine glass; see rebrand-spritzwine"

# ---- the ledger. Written by `sodactl regen` on success. Never hand-edited. ----
[consumer.spritzwine.last_generated]
at            = "2026-07-18T21:14:02Z"
base_commit   = "…"
soda_commit   = "da673812a8b"
home_base     = "1bb0da91a5e"
result        = "8bf9f2c8e83"
rr_cache_tree = "…"
sodactl       = "0.3.1"
```

`toml_edit`, not `toml` — the ledger writes back in place and must preserve comments and ordering.

### Provenance, belt and braces

`regen` writes the same JSON block into an **annotated tag object** `gen/<consumer>/<utc>`. Tags don't change the consumer's SHA (so pins and family tags stay valid), survive force-push, and travel with the repo. If the TOML ledger and the tag object disagree, `check` emits `LEDGER_CONTRADICTS_TAG` and trusts neither.

### Core types

```rust
pub struct TierState {
    pub tier: String,
    pub tip: Option<Oid>,               // None = branch missing
    pub remote_tip: Option<Oid>,        // None = no remote (soda-full!)
    pub ahead: u32, pub behind: u32,
    pub parent_tip: Option<Oid>,
    pub own_commits: Vec<Oid>,          // merge-base(parent,child)..child — DERIVED
    pub rebased_on_parent: bool,        // is_ancestor(parent_tip, tip)
}

pub struct Finding {
    pub code: FindingCode,              // stable string: STALE_CHILD, TAG_COLLISION, …
    pub severity: Severity,             // Blocker | Drift | Hygiene
    pub message: String,
    pub evidence: Evidence,             // {ref, oid} | {file, line} | {path}
    pub remedy: Option<Remedy>,         // verb + args that would fix it
}

pub enum FindingCode {
    StaleChild, StaleStitch, StaleConsumer, LedgerMissing, LedgerContradictsTag,
    TierUnpushed, TierDiverged, TierNoRemote, UnmodelledBranch,
    TagCollision, TagOnStaleTarget, TagFamilySkew, PinUntagged, PinNotOnCodeberg,
    GitmodulesBranchMismatch, SubmoduleDirty,
    RrNoPostimage, RrStrayThisimage, RrUncommitted,
    EnvStampDrift, RebaseInFlight, DocDrift, ThreadpoolExportMismatch,
}
```

`Severity::Blocker` on `RebaseInFlight` is universal: **no mutating verb runs while `.git/modules/wine/rebase-merge/` exists.**

### Tags as a parsed family

```rust
pub struct TagName { component: String, wine_base: Version, family_version: u32 }
// soda-11.12-3 · spritzvalve-11.0-3 · spritzwine-11.13-3 · prater-11.0-3
// Legacy SemVer (spritzwine-11.0-0.2.0, v0.1.0) parses to TagName::Legacy.
```

`wine_base` is **derived from the consumer's base ref**, never declared — which is exactly how the tool catches `BRANCHING.md:135` saying 11.12 while the shipped tag says 11.13. Two family versions on one commit is `TAG_COLLISION`, `Blocker`.

## Architecture

```
tools/sodactl/
  Cargo.toml              # own workspace, own lockfile — NOT the wine build's
  sodactl.toml            # the manifest (lives at tools/sodactl.toml)
  crates/
    sodactl-core/         # model, reconcile, plan. no clap, no rmcp, no terminal, no stdout.
    sodactl-git/          # gix for reads; std::process::Command("git") for writes
    sodactl-cli/          # bin `sodactl`; `sodactl mcp` is a subcommand
    sodactl-mcp/          # rmcp handler; depends on core only. rmcp confined here.
  tests/fixtures/         # tarballed micro-repos with a synthetic tier DAG
```

This copies the split the project already got right (`installer/core`, deps = `anyhow` only, own workspace, vs `installer/gui`, 616 packages). Nothing here enters the Wine build's dependency surface; `build.sh` never touches it.

```toml
# sodactl-core
gix        = { version = "0.66", default-features = false,
               features = ["max-performance-safe", "revision"] }
serde      = { version = "1", features = ["derive"] }
toml_edit  = "0.22"      # ledger round-trip preserving comments
serde_json = "1"
schemars   = "1"         # ONE schema: CLI --json == MCP structuredContent
thiserror  = "2"
camino     = "1"
time       = { version = "0.3", features = ["serde", "formatting"] }
tracing    = "0.1"

# sodactl-cli
clap               = { version = "4", features = ["derive", "env"] }
tracing-subscriber = "0.3"     # STDERR ONLY, enforced by a test
anyhow             = "1"

# sodactl-mcp
rmcp  = { version = "=2.2.0", features = ["server","macros","schemars","transport-io","elicitation"] }
tokio = { version = "1", features = ["rt-multi-thread","macros","io-std","process","io-util","sync"] }

# dev
insta = "1"
```

**`gix` reads, `git` binary writes.** Non-negotiable. `index.lock` is the real mutex and Florian has a terminal, an editor's git integration, and `build.sh:44`'s `submodule update --force` all writing the same 6.5 GB tree. Rebase, cherry-pick, checkout, update-ref go through `Command`. Ahead/behind, ancestry, ref and tag enumeration, tree oids go through `gix`, because they're called constantly and `status` over 6.5 GB already dominates `regenerate.sh`.

**How the surfaces share the core.** `StatusReport`, `Finding`, `Plan` and `Event` are `Serialize + JsonSchema` types in `sodactl-core`. The CLI renders them as a human table or `--json`. MCP returns the identical values as `structuredContent`. A future ratatui frontend renders them as widgets. One schema, N surfaces, zero drift — and it means the deferred UI is ~600 lines against a proven core rather than a design exercise against a domain model that doesn't exist yet.

**`rmcp` is quarantined in one crate** and pinned exactly. MCP `2026-07-28` lands in nine days with an rmcp 3.x behind it; stdio transport insulates us from essentially all of the announced break (sessions, `Mcp-Method` routing, auth), and confining rmcp to one crate makes the migration a contained diff.

## Operations

| Op | Verb | Inputs | Preconditions | Long? | Confirm? | Idempotent / resumable |
|---|---|---|---|---|---|---|
| State report | `status [--json]` | — | none | no (<1s warm) | no | yes / n.a. |
| Lint | `check [--json]` | — | none | no | no | yes / n.a. |
| Explain an op | `explain <op>` | op name | none | no | no | yes |
| Restack children | `restack <tier> [--all-descendants]` | tier name; **old parent tip is computed, never an argument** | clean tree, no rebase in flight, child is exactly its own derived commit set on the old parent | s–min | **yes** | yes (re-run is a no-op) / via `--continue` |
| Stitch | `stitch <name>` | manifest order | all children restacked; rr-cache seeded | s–min | no (`-B` from scratch) | yes by construction |
| Regenerate | `regen <consumer>` | manifest base + soda tip + home_base | clean tree, no rebase in flight, base fetched (verb does it) | s–min | no | yes by construction / re-run captures the resolution |
| rr-cache lint | `rr lint` | — | none | no | no | yes |
| rr-cache gc | `rr gc [--older-than 30d]` | — | none | no | yes | yes |
| Tag | `tag <family> <n> [--force]` | family, version | branch pushed; no family collision; target not stale | no | **yes on `--force`** | yes with `--force` |
| Bump pointer | `pin <consumer-tag>` | a **tag name, never a SHA** | tag exists, names a consumer tip, reachable on Codeberg | s–min (6.5 GB checkout) | yes | yes |
| Compile | `build` | — | pointer bumped; env stamp matches | **~30 min cold** | no | yes (warm/incremental) |
| Package | `package --version <tag>` | tag | build done | min | no | yes |
| Smoke | `smoke` | — | packaged | ~1 min | no | yes |
| Release plan | `release <family> <n> --plan` | family, version | — | no | **yes (composite)** | plan only, never executes |
| Prater dispatch | `dispatch prater --sha <s>` | sha | token present | no | yes | no (fire-and-forget POST) |
| Push | `push --print` | — | none | no | — | **prints commands, never runs them** |
| Resume | `resume` | — | rebase in flight, nothing else moved | no | yes | n.a. |
| Undo | `undo [<backup-ref>]` | backup ref name | — | no | yes | yes |

`build` / `package` / `smoke` are **thin wrappers, deliberately**. `build.sh` is 169 lines of hard-won phase ordering; reimplementing it loses a week and gains a bug. The wrapper's entire value is two pre-flight refusals — unbumped pointer (`build.sh:44` reverts wine to the recorded pointer and silently builds the wrong thing, the single most-cited gotcha in MEMORY.md) and env-stamp mismatch — plus structured log tails. `image rebuild` is not a verb at all: it is runner-host-only and `check` reports the drift with an explanation that only the runner-host operator can clear it.

### The `restack` invention

`restack` never takes `<old>` as an argument, which eliminates the entire class of "stale `<old>` → parent's commit silently duplicated into the child → every consumer gains a doubled commit."

```rust
let own      = rev_list(merge_base(parent_tip_old_guess, child)..child)?;
let old_base = child.ancestor(own.len())?;
ensure!(is_plausible_tier_tip(old_base), Bail::AmbiguousOldParent);
let before   = patch_ids(&own);

backup_ref(child)?;                                    // refs/sodactl/backup/<utc>/<child>
git(["rebase", "--onto", parent_tip, &old_base, child])?;

// POST-CONDITION — the real guard:
let after = patch_ids(&rev_list(parent_tip..child)?);
ensure!(after == before, Bail::PatchIdDrift);          // auto-restores from backup
```

Patch-id set equality before and after is what catches a duplicated parent commit: the count becomes `n+1` and the set gains a member. This needs no interface at all, which is precisely why the UI is deferred.

## Conflict handling

`regenerate.sh` exits 1 leaving the tree mid-rebase and detached. That is a legitimate terminal state for the tool; it is not a state the tool tries to drive out of interactively.

**1. Detect and refuse.** Every verb begins with `InFlight::detect()` — `rebase-merge/`, `rebase-apply/`, `CHERRY_PICK_HEAD`, `MERGE_RR`, `sequencer/`. If present, every mutating verb refuses with `REBASE_IN_FLIGHT` (`Blocker`). `status` reports it richly: `head-name`, `onto`, `msgnum/end`, remaining todo, and the conflicted paths **classified**.

**2. Classify.** `git diff --numstat` reporting `-\t-` means binary.

- **Binary, and covered by `[[binary_resolution]]`** — resolved non-interactively *before the replay ever stops*: `git checkout --ours -- <path> && git add <path>`, recorded in the run log and in the ledger. This is the permanent fix for `oic_winlogo.ico`, which rerere will otherwise re-prompt on for every replay until the end of time. It is the highest-value-per-line item in the whole plan.
- **Binary, not covered** — the tool says so explicitly: *rerere is line-based and can never learn this; it will re-prompt on every replay.* It prints the two blob paths and offers `--binary-strategy ours|theirs`, and on a choice, **offers to append the rule to `sodactl.toml`** — converting a recurring manual stop into a one-time reviewed policy decision. If you want to look at the two icons, `xdg-open` on the extracted stage-2/stage-3 blobs is ten lines in the CLI. That, not a 616-package toolkit, is the correct amount of engineering for that problem.
- **Text** — the tool prints the paths and the exact next commands, and stops:
  ```
  Resolve, then:
    git -C wine add <paths>
    git -C wine rebase --continue
    sodactl regen spritzwine        # re-run to capture the resolution as an rr-cache postimage
  ```
  This is deliberately not more than `regenerate.sh` gives you. The human already has a shell and an `$EDITOR`; becoming a shell costs ~10 days, is untestable with `TestBackend`, and ends by exec'ing `git mergetool` anyway.

**3. Never auto-resume.** `sodactl resume` re-verifies nothing else moved and runs `git rebase --continue`. Nothing more.

**4. Capture, then commit.** `regen` does what `regenerate.sh` doesn't: it **fetches the base first**, and after `persist_cache` it **commits `tools/rr-cache`** in the superproject instead of only warning. A resolution that lives in one person's `.git` is a resolution that gets rediscovered from scratch.

**5. Lint the cache.** `rr lint` reports the 5 postimage-less entries (recorded-but-unresolved; guaranteed re-prompts) and the 2 stray `thisimage`s (transient state that should never have been committed). `rr gc` runs the `git rerere gc` that has never been run and prunes postimage-less entries older than N days.

## Safety guardrails

1. **Plan/apply split, always.** Every mutating verb has `--plan` (default under MCP) rendering the literal argv plus predicted ref mutations. `--apply` **re-derives the plan from live state** and refuses if it differs from what was shown. No TOCTOU window between review and execution.
2. **Compare-and-swap on every ref.** Each `RefMutation` carries `expected_before`, applied via `git update-ref <ref> <new> <old>` — atomic, fails loudly. This is the only guardrail that also covers external writers (your other terminal, your editor, `build.sh`'s forced submodule update), which no daemon or lock server could.
3. **Backup refs, unconditionally, before every history rewrite.** `refs/sodactl/backup/<utc>/<branch> = <old-tip>`, recorded in a journal at `.git/sodactl/journal/<plan-id>.json` written *before the first mutation*. Never auto-pruned; `sodactl undo` lists and restores by name; expiry only via explicit `sodactl gc --older-than 30d`. This is what permanently retires "the old tip is unrecoverable once the reflog expires." Known limitation, stated plainly: the backups live inside the same submodule `.git` they protect — they survive a bad rebase, not `rm -rf wine`.
4. **Dirty-tree refusal that doesn't cry wolf.** Tracked modifications block. Untracked files matching the manifest's `build_artifacts` globs are ignored (the wine tree legitimately carries `dlls/msxml3/xslpattern.tab.c` and `.build-env-stamp`). Other untracked files warn. A guardrail that fires on every invocation gets disabled within a day.
5. **Trap guards from `BRANCHING.md`, encoded as assertions.** Trap 1: `regen` always passes `home_base` from the manifest; a plan that would replay more than the tier chain's commit count is refused with the count shown. Trap 2: refuse to rebase any `soda-*` branch onto a wine base — that is the move that invalidates every rr-cache entry. Plus `soda-ntdll`'s invariant: before generating any chain containing it, assert the base does not patch `dlls/ntdll/threadpool.c` and that the `Tp*` export sets match across both bases.
6. **`pin` takes a tag, not a SHA.** It verifies the tag exists, names a consumer branch tip from the manifest, probes Codeberg with the same `GET /repos/SpritzWine/wine/git/commits/<pin>` that `lint.yml:60-75` uses (locally, *before* the commit rather than in CI after), checks the tag out itself, and edits `.gitmodules branch=` in the **same commit**. `gitlink_tags.is_empty()` is a hard error — it is the `Fetched in submodule path 'wine', but it did not contain <sha>` failure, pre-detected.
7. **Florian does the pushing. There is no push verb.** This is the project's `push-workflow` rule (no ssh-agent, submodule-first, Codeberg only) and it is treated as a hard constraint. `sodactl push --print` emits the ordered, copy-pasteable list with submodule-first ordering and `--force-with-lease` (strictly better than the bare `--force` currently prescribed — it is the CAS discipline applied to the remote):
   ```
   git -C wine push --force-with-lease codeberg soda-core soda-glorious soda-ntdll soda-fsync
   git -C wine push --force-with-lease codeberg spritzwine
   git -C wine push codeberg spritzwine-11.13-4
   git push codeberg rust
   git push codeberg spritzwine-11.13-4
   ```
   The documented vault-repo exception (Claude pushes those directly) does not apply — no vault repo is in this graph.
8. **Elicitation on the destructive subset, MCP-side.** A tool-permission prompt says "may call this tool"; elicitation says "here is the specific branch about to be rewritten and the backup ref that will hold its old tip."
9. **No daemon, no lock server.** Serializing our own two frontends while N external writers touch the tree is a false sense of safety. `index.lock` + CAS + backup refs covers strictly more. If real isolation is ever needed, the answer is `git worktree add`, not IPC.
10. **Stdout discipline as a test, not a convention.** A `#[test]` greps `sodactl-core` for `println!|print!|std::io::stdout` and fails. On stdio, stdout *is* the JSON-RPC channel, and one stray print corrupts the protocol in a way that surfaces as an unrelated-looking client error three tool calls later.

## MCP surface

`sodactl mcp` — same binary, stdio transport, `Arc<Core>` handler. Every tool returns `Json<T>` with a `schemars`-derived `output_schema`; the same `T` serialises `sodactl --json`. `tracing-subscriber` to stderr only.

**Read tools** (no permission friction, and where most of the value is):

| Tool | Arguments | Returns |
|---|---|---|
| `sodactl_status` | `{ refresh?: bool }` | `Json<StatusReport>` — tiers with ahead/behind and derived commit sets, stitch freshness, consumers with input drift, pin consistency, tag ledger + collisions, in-flight op, rr-cache health, env stamps, secrets presence |
| `sodactl_check` | `{ severity_min?: "blocker"\|"drift"\|"hygiene" }` | `Json<Vec<Finding>>` — `{code, severity, message, evidence, remedy}` with stable codes so a model can reason and CI can allowlist |
| `sodactl_explain` | `{ op: string }` | `Json<OpDoc>` — commands, preconditions, failure modes, idempotence, duration. **`docs/BRANCHING.md` as a queryable structure.** Cheapest tool here, plausibly the most used |
| `sodactl_provenance` | `{ consumer: string }` | `Json<Provenance>` — recorded ledger vs tag object vs live refs; the answer to "is this stale?" |
| `sodactl_tier_graph` | `{}` | `Json<Dag>` — nodes, edges, staleness, restack order |
| `sodactl_rr_health` | `{}` | `Json<Vec<RrEntry>>` |
| `sodactl_plan` | `{ verb: string, args: object }` | `Json<Plan>` — steps with literal argv, preconditions, predicted ref mutations, backup refs. **Cannot mutate.** |
| `sodactl_log_tail` | `{ job: string, lines?: u32, grep?: string }` | bounded tail: ring of 2000 lines + every `error_patterns` match + last 200 unconditionally |

**Mutating tools** (each separately declared so Claude Code's permission system gates them individually):

| Tool | Arguments | Notes |
|---|---|---|
| `sodactl_apply` | `{ plan_id: string, expected_heads: {ref: oid}[] }` | **One mutating entry point**, so there is exactly one place enforcing confirmation, backup refs and CAS. Re-derives the plan, refuses on drift. Elicits before `restack`, `tag --force`, and anything moving a tag a superproject commit pins. Progress notifications on the optional `progressToken`, degrading silently when absent |
| `sodactl_resume` | `{ }` | `git rebase --continue` after re-verifying nothing else moved |
| `sodactl_record_resolutions` | `{ }` | persist + commit `tools/rr-cache` |
| `sodactl_build_start` / `sodactl_build_status` | `{}` / `{ job: string }` | The only long-running pair. Hand-rolled job handles, ~60 lines — **not** rmcp's `task_manager`, which is the module most exposed to the 2026-07-28 Tasks reshape |
| `sodactl_push_script` | `{}` | Returns the command list as **text**. Explicitly does not execute |

Not present: any push tool, `package`/`smoke`/`image rebuild`, sampling (deprecated 07-28), Tasks API.

`#[tool_handler(instructions = ...)]` carries the three rules a model most needs and cannot infer: *never author on a `spritz*` branch; never rebase a tier onto a wine base; the user performs all pushes.*

## Phased plan

**v0 — manifest + read-only `status` / `check`. 5 days.**
Core model, `sodactl.toml` hand-written for the current graph (7 tiers, 1 stitch, 2 consumers, 1 binary rule), `gix` read layer, derived commit counts, tag-family parsing, pin consistency, rr-cache health, `--json`, ~15 finding codes. Fixture micro-repos and insta snapshots from day one.

**This ships alone and has zero blast radius.** On the surveyed state it immediately reports: four stale tier children; `soda-full` stitched from a dead core and tagged `soda-11.12-3`; `soda-core` unpushed; `soda-branding` diverged 1/2; `soda-full` with no remote; `spritzvalve-11.0-1 ≡ -11.0-3`; `.gitmodules` vs gitlink; submodule dirty; 5 postimage-less rr entries; 2 stray `thisimage`s; four unmodelled branches; the 11.12-vs-11.13 doc drift. **Twelve real defects, three of which are actively shipping wrong bits.** Nothing else in this plan has that ratio. (The headless proposal said 3 days; the judge said 5. The judge is right — a hand-written manifest plus fixture repos plus getting `TagName` parsing right over legacy SemVer tags is a week.)

Also in v0, near-free: wire `sodactl check --severity-min blocker` into `lint.yml`, which already runs on every push to `rust` and takes seconds. That converts "nobody looked" into "CI is red" — the partial substitute for the ambient awareness a deferred UI costs.

**v1 — MCP read surface. 2 days.**
`sodactl mcp`, `_status` / `_check` / `_explain` / `_provenance` / `_tier_graph` / `_rr_health`, stdio, `.mcp.json` entry alongside the existing `beisl` server, the stdout-discipline test. Reuses v0's types verbatim. After this, every Claude Code session in this repo starts already knowing the graph state instead of running six git commands to reconstruct it — which, given how many MEMORY.md entries are exactly that reconstruction, is the second-best value-per-day here.

**v2 — plan/apply for `restack` + `stitch`. 5 days.**
`Plan`/`Step`/`Event`, backup refs, journal, `update-ref` CAS, dirty-tree policy with `build_artifacts`, trap guards, the computed-old-parent and patch-id post-condition, `undo`. Two verbs only — the ones with no script at all and the nastiest silent-corruption mode. Restacking the four currently-stale children is the acceptance test.

**v3 — `regen`, rr-cache, `tag`, `pin`, `push --print`. 5 days.**
`regen` wrapping `regenerate.sh`'s logic properly: fetch bases first, apply `[[binary_resolution]]` rules pre-emptively, write the ledger and the provenance tag, commit `tools/rr-cache`. `rr lint` / `rr gc`. `tag` with family-collision and stale-target refusal. `pin` taking a tag and editing `.gitmodules` atomically. **End of v3 the tool covers the entire authored-history half of the inventory**, and the ledger exists — which retroactively upgrades v0's consumer staleness from `Unknown` to real.

**v4 — build-lane wrappers + `release --plan` + Prater dispatch. 3 days.**
Thin. Value is the pre-flight refusals and structured log tails, not reimplementation.

**v5 — decide about a UI. 0 days of code.**
Re-read `status --json` after a month of real use. If the answer is "I keep running `watch sodactl status`," build a **read-only** ratatui dashboard (`ratatui 0.30` umbrella, `crossterm 0.29` via re-export, `tui-logger`, `throbber-widgets-tui`, `Table` + `Scrollbar`, `insta` + `TestBackend` snapshots, a `Runner` trait faked in tests) — ~1 week against a proven core. It renders `Finding`s and tails jobs; it does **not** resolve conflicts and does **not** suspend the terminal. If the answer is "Claude runs `sodactl_check` and tells me," the UI was correctly never built. libcosmic is ruled out now, permanently, for the reasons in the Recommendation.

**Total to a genuinely complete tool: ~20 working days. To something that catches twelve live defects: five.**

## Decisions — 2026-07-19

**Tag collisions are normal, not exceptional.** Multiple tags naming one revision is
expected: a component that did not need re-pouring keeps its commit across a family
release, and some tiers (branding especially) will barely change for long stretches.
So `TAG_COLLISION` must not require a declared `[[policy.tag_equivalence]]` to stay
quiet — requiring one means writing a new entry every release, which is a treadmill,
and a treadmill gets muted. Equivalences stay supported for the cases worth *narrating*,
but silence is the default. By the same reasoning `family_skew = "off"`: a component
legitimately sits at N-1 between cuts.

**The gate is on PUSH only. Everything else stays unblocked.** Conforming to every
check is required to *push*. Building, pouring, stitching and vetting must remain
available while checks are failing — that is the entire point of a development aid.
A tool that refuses to build because the graph is untidy is an obstacle, and the
first thing anyone does with an obstacle is route around it. Concretely: `build`,
`smoke`, `regen`, `stitch`, `restack` never consult `check`; only the `pre-push` hook
and CI do. A destructive verb may still refuse on its *own* preconditions (a dirty
tree, a rebase in progress) — that is not the gate, that is the operation protecting
itself.

**rr-cache: prune these five, and key future resolutions by PATH, not by revision.**

The proposal of a `fixes/` directory of resolution advisories is right in substance and
wrong in one detail, and the detail is the whole lesson. Keying an advisory by revision
or conflict-id reproduces *exactly* the failure that created this mess: an rr-cache
entry id is a hash of the conflict text, so any drift in the surrounding code yields a
new id and the old entry becomes unmatchable garbage. That is demonstrably what happened
here — the recorded conflicts are between `WINE_RUST_PE_LDFLAGS_*` and an intermediate
generalization, while every current tier uses `WINE_RUST_PE_DIR_*`, a third variant that
postdates both. The text on either side of those conflicts survives on no tier, no
stitch and no consumer (only on the stale `feature/rust-unwind-limits` branch), so they
can never match again. **They are orphans of a superseded development state: prune.**

The durable key is `path` + `strategy`, which is what `[[binary_resolution]]` already
is. Extend that rather than building a parallel mechanism:

- `strategy = "ours" | "base" | "delete" | "regenerate"` — a *strategy* survives code
  drift; a literal resolved blob does not, and goes stale silently.
- `"regenerate"` is the missing one and it covers the two 25k-line `configure` entries
  outright: `configure` is generated, so hand-merging it is never correct on any base.
  `build.sh`'s `autoreconf` already does the right thing; the rule just makes it
  declared instead of incidental.
- An optional `conflict_id` may *narrow* a rule to one exact conflict, but must never
  be the only key — otherwise it rots the moment the file moves.

## State digest and receipts — proving the checks actually ran

The problem this solves is not a hostile one. It is "I turned the hook off during a
messy afternoon and never turned it back on", and its cousin "the checks passed, then
I rebased, and the green tick is now about a repository that no longer exists".

**The digest covers what `check` consulted, not what is convenient to hash.** Hashing
the tag set — the obvious first idea — is close to the worst available input: tags move
rarely and reflect almost nothing that breaks. The digest would sit unchanged while the
pin, the branch tips and the working tree all drifted, which is a signature that reads
"verified" long after it stopped being true. The inputs are:

```
state_digest = H(
  manifest bytes                                  # policy changed -> re-verify
  tool version                                    # check logic changed -> re-verify
  for each modelled ref, sorted:  name : oid      # tiers, stitches, consumers, bases
  superproject gitlink (committed and staged)     # the pin is the thing that orphans
  wine HEAD + attached branch
  tracked rr-cache entry ids, sorted
  sorted list of dirty paths, build_artifacts applied
)
```

Sorted and canonical, so it is reproducible on another machine and in CI.

**A receipt is a freshness record, not a credential.**

```
{ state_digest, tool_version, severity_min, findings: {blocker, drift, hygiene},
  build: {tree_oid, result, at}, smoke: {result | "skipped", at}, at }
```

The rule is one line: **a receipt is valid only while the recomputed digest equals the
recorded one.** Rebase, restack, stage a pin, dirty a file — the digest moves and the
receipt is void without anyone having to remember to invalidate it. That is the same
mechanism the build-evidence section needs, so it is one ledger, not two.

**Be honest about what this does and does not defend against.** A local receipt file is
*not* a security control. Anyone who can pass `--no-verify` can also write a receipt.
It defends against forgetting and against drift — which is the actual threat model for
one developer and their own repository — and it must not be described as more than that.

**So CI recomputes rather than trusts.** The strong guarantee costs nothing extra and
needs no signing key: the pipeline runs `sodactl check` itself and compares its own
digest against the pushed state. A receipt is then a fast local path ("this exact state
already passed, don't re-derive"), and CI is the authority. Reaching for a signed
receipt instead would be strictly weaker and more machinery — the key would live on the
same disk as the thing it attests to.

**Where it earns its keep as a development aid:** `sodactl build` records
`{tree_oid, result}` into the receipt, so the pre-push hook can answer "was this exact
tree built, and did smoke pass?" in milliseconds rather than by rebuilding. Skipping
smoke is recorded as `"skipped"` rather than omitted, so a push gated without smoke
evidence is visibly not the same as one gated with it.

## Worked example — the first issue the model caught (2026-07-19)

Recorded because it is both an **open item** and the **proof the build-evidence checks
are worth building**: it was found by hand-applying the freshness model above, before a
line of it was implemented.

`dist/` holds `spritzwine-spritzwine-11.13-3-x86_64.tar.xz` — 257 MB, named for the
pinned tag, packaged from something else:

| | |
|---|---|
| tarball claims | `spritzwine-11.13-3` |
| that tag derefs to | `8bf9f2c8e83` — `wine-11.13-6` |
| what is actually inside | `e3a386db584` — `wine-11.13-7-g56df0cbc56f` |

`package.sh` takes the artifact name from the pinned tag but packages whatever was
built. So the tarball asserts a release identity it does not contain — including an
unreleased feature. **Status: not shipping, Florian is mid-work on this tree.** No
action needed beyond not mistaking it for a release; it exists as the reference case
for `PACKAGE_MISLABELED`.

Two things this pins down for the implementation:

- The check is `package name -> tag -> commit` versus `packaged tree -> commit`. Both
  sides must be derived, and neither is mtime.
- **Deref annotated tags.** `git rev-parse spritzwine-11.13-3` returns `c648e824600`,
  the *tag object*, not the commit. Any comparison must use `^{commit}` or it compares
  tag-object oids against commit oids and silently never matches. (`GitCli::rev_parse`
  already appends it; new tag code must too.)

## Open questions for Florian

1. **The four stale children — restack or accept?** `soda-glorious`, `soda-ntdll`, `soda-fsync`, `soda-demo` all descend from `79e58cb8a70`, but `soda-core` is at `39fe08f1bef`. Is that a genuine backlog, or was the second core commit intentionally not propagated yet? This determines whether v0 ships with those as `Blocker` or `Drift`.
2. **`spritzvalve-11.0-1 ≡ -11.0-3` — which is correct?** Either version 3 was never actually regenerated (and Prater is pinning version-1 bits under a version-3 name), or the two versions are genuinely identical for the Valve consumer. The remedy differs completely. I need the answer before `check` can name it.
3. **Is the `oic_winlogo.ico` resolution always "ours"?** I've written `strategy = "ours"` into the manifest on the assumption that `soda-branding` owns the glass unconditionally, per the rebrand work. If there is any base whose icon should win, the rule needs a `consumers` narrowing or removal.
4. **`soda-full` has no remote at all.** Should `sodactl` push it (well — print a push line for it), making drift detectable, or is it deliberately local-only as a derived artifact? Currently nothing anywhere can tell you it's stale.
5. **The five postimage-less rr-cache entries — prune or resolve?** They're recorded-but-unresolved conflicts that re-prompt on every replay. Pruning them is safe but loses the preimage record; resolving them requires reproducing each conflict. `rr gc` policy hangs on this.
6. **What is the family version actually meant to gate?** The scheme implies a single integer shared across `soda-11.12-N` / `spritzvalve-11.0-N` / `spritzwine-11.13-N` / superproject / `prater-11.0-N`. Is skew always a defect, or is it legitimate for one component to sit at N-1 between cuts? `TAG_FAMILY_SKEW`'s severity depends entirely on this.
7. **The four unmodelled branches** (`spritzvalve-full`, `spritzwine-valve`, `spritzwine-pre-1112-rebase`, `spritzwine-backup-20260703`) — dead, or live-but-undocumented? I'd like to either add them to the manifest or delete them; leaving them to generate a permanent `UNMODELLED_BRANCH` finding is the worst of both.
8. **`docs/BRANCHING.md:135` says 11.12, the tag says 11.13.** Should `sodactl check` treat doc drift as a real finding (implying the doc becomes a lint target with a machine-readable marker), or is the manifest simply taking over as the authority and the doc becoming prose?
9. **rmcp timing.** MCP `2026-07-28` lands in nine days. Ship v1 on `=2.2.0` now and migrate, or hold v1 for two weeks and start on 3.x? I'd ship now — stdio is nearly untouched by the break and v1 is two days — but it's your maintenance burden.
10. **Does `sodactl check` block CI?** Wiring it into `lint.yml` is nearly free and is the main mitigation for deferring the UI. But with twelve findings live today, it goes red immediately. Gate it at `Blocker` only, or land it as advisory-non-blocking first?