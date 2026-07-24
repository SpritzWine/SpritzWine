# sodactl

An auditor and plan-first release driver for the SpritzWine branch model.

SpritzWine's release model is a stack of hand-authored **tier** branches that get
replayed onto upstream wine bases to produce **consumer** branches, which the
superproject then pins. Nothing in that pipeline is checked by git: a tier can be
restacked without its children noticing, a stitch can be built before a tier
moved, a pinned consumer SHA can be force-pushed out of existence. All of those
failures are silent until a fresh clone or a CI runner hits them, which is the
worst possible moment.

`sodactl status`/`check`/`rr-health`/`explain`, the TUI and the MCP surface read
the two repositories and report what is inconsistent — those paths never write.
The **write verbs** (`restack`, `stitch`, `regen`, `tag`, `pin`, `rr-gc`,
`resume`, `undo`) DO rewrite branches and commit — behind a plan/apply split:
the default invocation prints a reviewable plan and changes nothing; `--apply`
re-derives the plan, refuses on drift, writes a journal and never-pruned backup
refs, then executes with CAS protection where git provides it. **There is no
push verb and there will not be one** — `sodactl push` prints the derived
commands for Florian to run.

See `docs/BRANCHING.md` in the superproject for the model itself; this tool
encodes that document and is not a substitute for it.

## The governing principle

> **git is the only authority on where refs are. The manifest declares policy and
> topology edges — never geometry.**

`tools/sodactl.toml` says which tiers exist, which tier is whose parent, which
tiers a stitch linearizes, and which base a consumer is poured onto. It does
**not** say how many commits anything has, or which commit anything is at. Those
are observations, and every one of them is derived live.

The reason is not tidiness. A hand-written count is a lie waiting to happen while
a destructive operation depends on it — and the write verbs (below) are
destructive by nature: `rebase --onto`, `checkout -B`, `push --force-with-lease`.

### The bug class this tool exists to avoid

A **stitch** is built by `cherry-pick`. A **consumer** is built by
`rebase --onto`. In both, the resulting commits are *copies*: new SHAs, and not
descendants of the originals.

So any check that reasons about a replayed artifact using **ancestry**, **SHA
equality**, or **commit counts** is wrong, and wrong in the confident direction.
The first version of the stitch check asked `git merge-base --is-ancestor` and
produced four false positives with total confidence. The right primitive is
**patch-id equivalence** (`git cherry <upstream> <head> <limit>`).

Patch-id has its own limit, and the code is careful about it in both directions:

* **Sound for a stitch** — a same-base cherry-pick leaves an unconflicted
  commit's diff byte-identical, so the patch-id survives.
* **Insufficient for a consumer** — a cross-base rebase legitimately changes
  diffs (conflict resolution, upstream context, `binary_resolution` deletions).
  Measured on the live repo, 3 of 7 commits of a correctly-poured consumer are
  absent by patch-id. So patch-id equality proves **presence** and never
  **currency**, and `consumers.rs` only ever infers in that one direction.

`git cherry` additionally requires `limit` to be an ancestor of `head`, or the
range silently truncates and **under**-reports. That precondition is established,
not assumed.

Two further rules, now enforced in one place — `sodactl-git`:

* **A check that cannot determine its answer emits nothing.** Not a benign
  default. `GitRead` distinguishes git's exit 1 (*the answer is no*) from any
  other failure (*there is no answer*), and the latter reaches the caller as an
  error.
* **Counts are not evidence of identity.** Comparing set sizes where the question
  is set membership reads two disjoint sets of equal size as "equal".

## Usage

```sh
cargo build --release

sodactl status                       # the graph as it stands
sodactl check                        # only what is wrong
sodactl check --severity-min blocker # tighten the CI gate
sodactl rr-health                    # the rerere cache, in detail
sodactl status --json                # machine-readable
```

`--manifest` defaults to `tools/sodactl.toml`, resolved relative to the current
directory; the superproject root is taken to be its grandparent.

### Exit codes

| code | meaning |
|---|---|
| 0 | nothing at or above the gate |
| 1 | the gate tripped — findings exist at `--severity-min` or worse |
| 2 | **the tool could not answer** — bad manifest, unreadable repo, failed git |

1 and 2 are distinct on purpose. A CI job that cannot tell "four blockers" from
"I could not read a single ref" will eventually be "fixed" by loosening the gate,
and then the tool is silently not running at all.

### Severities

* **Blocker** — a destructive operation would fail, or would destroy work.
* **Drift** — the graph is inconsistent; a regeneration would produce stale
  output.
* **Hygiene** — true, worth knowing, nothing is broken.

Severity is calibrated against a specific failure mode: a finding that can never
be cleared trains the reader to ignore the whole severity column, taking the real
blockers with it. So documented-normal states (a consumer tip ahead of its tag, a
local-only stitch) are Hygiene, and states that merely *would* break if you
committed them are Drift.

## The TUI

```sh
sodactl --manifest tools/sodactl.toml tui
```

A dashboard for the **failure path**: the same reconcile the CLI runs, arranged
so you can see where to do what next. The TUI itself never writes — a
source-grep guardrail test pins every subprocess in the crate to two audited
read-only constructors (`git log` for the chain, `git`/`tig show` for
inspection), so a third spawn site anywhere fails the build.

### Panes

```
┌ findings (35%) ──┬ [graph] / log (tabs, right top) ──┐
│ ▼ DRIFT (14)     │ tier tree  /  commit chain        │
│   [ ] PIN_…      ├ detail (right bottom) ────────────┤
│   [x] TIER_…     │ summary / remedy / catalogue      │
├──────────────────┴───────────────────────────────────┤
│ counts | reconciling ⠋ / refreshed Ns ago / ?  help  │
```

* **findings** — the report grouped by severity, worst first, worst finding
  pre-selected on startup.
* **graph** — the modelled topology: home base → tiers by parent edge, with
  stitches and consumers as sibling groups (replayed artifacts, not
  descendants). Stale markers distinguish the direct offender (`NO -- stale`)
  from inheritance (`STALE (inherited)`), and a consumer's STAGED pin
  divergence is visible in-tree. Orphan tiers (typo'd parent, cycle) render at
  top level with the reason in words rather than vanishing.
* **log** — the commit chain via `git log --graph`, context-scoped: Overview is
  every modelled ref since the home base; selecting a tier narrows to its own
  commits (`own_limit..tier` — own commits, **not** ancestry), a stitch to
  `base_tier..name`, a consumer to `base..name`. Enter on a finding whose
  subject is a modelled ref jumps the log to that context.
* **detail** — follows the focused pane: finding → summary, the remedy as a
  set-off copy-paste line, and the catalogue prose (a missing catalogue entry
  renders an explicit "no catalogue entry" line, never a silent omission);
  tier/consumer → the same cells the CLI tables print, plus the restack order
  verbatim; commit → oid and what Enter will open.

### Keys

One static table drives both the event loop and the `?` overlay, so help
structurally cannot drift from the bindings. `↑/k` `↓/j` move, `←/→`
close/open groups, `Space` ticks, `Enter` jumps/inspects, `Tab` cycles focus,
`r` refreshes, `Ctrl-Z` suspends (`fg` resumes), `?` help, `q`/`Ctrl-C` quit.

### Refresh model

Refresh is **manual (`r`) plus terminal focus-gain** — no polling. A worker
thread re-reads the manifest and re-reconciles per refresh (git stays the live
authority; nothing is cached across refreshes but the previous snapshot);
outcomes carry a generation counter and stale ones are discarded. A **failed**
refresh keeps the last good snapshot on screen and says so in words in the
footer — the screen never blanks and never silently looks fresh. Mashing `r`
coalesces to one reconcile, not N.

### Ticks

`Space` marks a finding as dealt-with for this session: dimmed but still
visible, session-local, keyed on `(severity, code, subject)`. Ticks survive a
refresh — but a finding that vanishes and returns, or **escalates in
severity**, comes back unticked: state that changed twice is new evidence, and
a pre-dimmed Blocker is the one thing the pane must never show.

### Shell-out inspection

`Enter` on a commit in the log suspends the TUI and opens the commit on the
real screen: `tig show <oid>` if `tig` is on `PATH` (probed once at startup),
else `git -C <wine> show <oid>` driving its own pager. The detail pane names
which one you will get before you press it. A failed inspection reports cause
first, command line last, in the footer — worded as an inspect failure, never
as stale data — and clears on the next keypress.

### Deliberately v2

* **Provenance capture** — remedy + catalogue prose serve the failure path
  today; hand-written provenance strings would drift, and drift here is a
  silent breakdown. The worker seam makes the recording decorator a one-site
  change later.
* **Persisted ticks** — session-local is the honest scope until a tick's
  identity can survive a reconcile that renames subjects.
* **Rebindable keys** — the single static table is the feature; a config file
  would reintroduce the drift the table exists to prevent.

## Finding codes

### Tiers

| code | meaning |
|---|---|
| `TIER_MISSING` | the manifest declares a tier with no branch |
| `TIER_CHILD_STALE` | a tier no longer contains its parent's tip — it was authored on an older parent, and every consumer built from it ships stale. Ancestry is the *right* primitive here: tiers are authored, never replayed |
| `TIER_PARENT_EDGE_WRONG` | the observed parent edge contradicts the manifest |
| `TIER_NOT_LINEAR` | a merge in a tier; `rebase --onto` silently changes what consumers get |
| `TIER_UNPUSHED` | ahead of its remote, fast-forwardable |
| `TIER_DIVERGED` | ahead **and** behind — restacked locally; a plain push is rejected |
| `TIER_BEHIND_REMOTE` | behind with nothing of its own: someone else's work, and every pour from here drops it. **Not** a force-push situation |
| `TIER_NO_REMOTE` | no tracking ref, so drift cannot be detected at all |

### Stitches

| code | meaning |
|---|---|
| `STITCH_MISSING` | declared, does not exist |
| `STITCH_REBASED` | no longer descends from its base tier, so the `git cherry` precondition fails and staleness was **not** checked — reported rather than guessed |
| `STITCH_STALE` | missing a tier's commits by patch-id: stitched before that tier moved |
| `STITCH_LOCAL_ONLY` | declared local-only; no remote drift detection is possible |

### Consumers

| code | meaning |
|---|---|
| `CONSUMER_BASE_UNRESOLVABLE` | the declared base does not resolve. A pour would fail *after* `checkout -B` has already moved the branch. Note wine releases are **tags**, so `refs/remotes/<remote>/wine-N.M` never exists |
| `CONSUMER_NOT_ON_BASE` | does not descend from its declared base — poured onto something else. Ancestry is valid here: a replay's base really is an ancestor of the result |
| `CONSUMER_EXCLUDE_VIOLATION` | carries commits from a tier excluded from this chain |
| `CONSUMER_MISSING_TIER` | carries nothing from a tier it should: absent by patch-id **and** no commit bears the tier's subject prefix. The conjunction is deliberate — either signal alone false-positives |
| `CONSUMER_CURRENCY_UNKNOWN` | there is no generation ledger, so "is this current?" is genuinely unanswerable. An honest non-answer, at Hygiene |
| `CONSUMER_AHEAD_OF_TAG` | tip ahead of its newest tag — documented as normal |

### The superproject pin

Consumers are force-pushed, so an untagged pinned SHA falls out of its branch on
the next pour and `git submodule update` dies with *"Fetched in submodule path
'wine', but it did not contain \<sha\>"*. This cluster exists for that.

There are **three** pins and they are three separate facts: `HEAD:wine` (what is
committed), the index gitlink (what is *staged*), and wine's own HEAD (what is
checked out). Every finding names which one it fired on. When the staged pin
differs from the committed one, the cluster is reported at **Drift** and phrased
as what committing *would* do — the committed state may be perfectly clean, and
CI (which checks out HEAD) can never observe the staged one.

| code | meaning |
|---|---|
| `PIN_UNTAGGED` / `PINNED_CONSUMER_UNTAGGED` | the pinned commit carries no tag |
| `PIN_NOT_ON_REMOTE` | on no remote-tracking branch. Offline check, so a stale tracking ref shows the same — the message says so |
| `PIN_STAGED_UNCOMMITTED` | the gitlink is staged but not committed |
| `PIN_ABSENT` / `PIN_UNVERIFIABLE` | no gitlink at all / the gitlink could not be read. Never rendered as a clean pin |
| `GITLINK_NOT_A_CONSUMER` | the pin is in no consumer branch and carries no consumer tag — typically a **tier**, which is soda without wine |
| `GITMODULES_BRANCH_MISSING` | `.gitmodules` declares no `branch=` for the submodule |
| `GITMODULES_BRANCH_UNRESOLVABLE` | `.gitmodules branch=` names a ref that does not exist — a typo |
| `GITMODULES_BRANCH_MISMATCH` | `.gitmodules branch=` and the gitlink name different commits, so `submodule update --remote` would fetch a different tree than the pin |
| `WINE_HEAD_OFF_PIN` | the checkout sits off the pin; `build.sh`'s `submodule update --force` will yank it back |

### Tags

Scheme: `<component>-<winebase>-<version>`. The repo carries ~6600 tags and all
but a handful are upstream's or Valve's, so anything unrecognised parses as
`Foreign` and produces **no finding whatsoever**.

| code | meaning |
|---|---|
| `TAG_COLLISION` | two family tags of one component naming one commit — intended when a family rolled without a re-pour, so Hygiene |
| `TAG_ON_STALE_TARGET` | the newest family tag is missing tier commits by patch-id. Restricted to the newest tag: a shipped tag is *supposed* to fall behind |
| `TAG_FAMILY_SKEW` | family versions disagree. **Undecidable from the repo** and it fires on every partial release — see Open questions |

"Newest" is ordered by `(wine_base, version)`, not by the trailing integer alone.

### Working tree and rerere

| code | meaning |
|---|---|
| `REBASE_IN_FLIGHT` | a replay is half-done; no verb may touch refs |
| `SUBMODULE_DIRTY` | tracked modifications a rebase would destroy |
| `SUBMODULE_UNTRACKED` | untracked files not covered by `build_artifacts` |
| `SUPERPROJECT_DIRTY` | the superproject has modifications, and `pin`/`regen` commit there |
| `WINE_HEAD_DETACHED` | normal after pinning a tag, a symptom of an aborted pour otherwise |
| `RR_NO_POSTIMAGE` | a conflict rerere recorded but whose resolution was never captured: it re-prompts forever while looking like a cache hit |
| `RR_ENTRY_SHADOWED_BY_RULE` | an entry an **unscoped** `[[binary_resolution]]` already decides. Hedged, because rerere never stores the path — the attribution is a content heuristic |
| `RR_STRAY_THISIMAGE` | transient in-progress state committed; a tell that a regen was interrupted |
| `RR_UNCOMMITTED` | newly learned resolutions exist only in this working tree |
| `RR_CACHE_UNSEEDED` | the live cache is missing tracked resolutions (compared by **id**, not count) |
| `UNMODELLED_BRANCH` | a branch the manifest does not describe — dead, or live but undocumented? |

The build tree here is always warm, so the dirty-tree guard splits by kind:
declared `build_artifacts` are silent whether tracked or untracked, undeclared
tracked modifications are a Blocker, unknown untracked files are Hygiene. A guard
that fires on generated files gets disabled within a day, and then the guard
protecting real uncommitted work is gone too.

## The write verbs

**These rewrite branches and commit.** All of them are plan-by-default: run
without `--apply` and they print numbered steps with literal argv, predicted
ref mutations (`ref: old -> new`), backup refs and notes, then exit 0 having
changed nothing. `--apply` re-derives the plan and refuses if it drifted from
what was reviewed; the destructive subset (`restack`, `tag --force`, `pin`,
`undo`, `rr-gc`) additionally asks `y/N` (or `--yes`). Every history rewrite
first writes a journal (`.git/sodactl/journal/`) and a create-only, never-pruned
backup ref under `refs/sodactl/backup/`, restorable via `sodactl undo`.

| verb | does |
|---|---|
| `restack <tier>` | rebase a tier's own commits onto its parent's new tip — `<old>` is computed from the patch-id own-set, never an argument; patch-id postcondition, auto-restore on drift |
| `stitch <name>` | rebuild a stitch: `checkout -B` from the base tier, cherry-pick the declared order, verify every tier's patch-ids landed |
| `regen <consumer>` | pour a consumer: fetch the base's remote, seed rerere from `tools/rr-cache`, `rebase --onto`, drive conflicts through `[[binary_resolution]]` rules, then persist+commit the cache, write the `[ledger.<name>]` entry and cut a `gen/<name>/<utc>` provenance tag |
| `tag <family> <n>` | cut `<family>-<winebase>-<n>` on the family's derived component tip; refusals protect the cut itself (`--force` overrides, preserving a re-pointed tag's old object as a backup ref) |
| `pin <tag>` | move the wine gitlink and `.gitmodules branch=` to a TAG in one commit, after verifying the `.gitmodules` URL serves it |
| `rr-gc` | prune postimage-less/shadowed rr-cache entries from BOTH caches, commit the prune |
| `resume` / `undo` | continue a conflicted replay after journal verification / restore a backup ref, CAS-guarded |
| `build` / `package` / `smoke` | exec the superproject's own lane scripts with the exit code passed through — the one documented exception to plan-default (they move no ref; `--json` is their review form). `build` gates on checkout-vs-pin (`--anyway` overrides) |
| `push` | **prints** the derived push commands; never executes them |
| `release <family> <n>` | the composite checklist (tag → pin → build → package → smoke → push), statuses derived live, nothing executed |

The **generation ledger** `regen` writes (`[ledger.<name>]` in the manifest,
comment-preserving) is what makes consumer currency decidable: with an entry,
`check` compares the recorded inputs against live state
(`CONSUMER_STALE_BY_LEDGER`, `LEDGER_CONTRADICTS_TIP`, `LEDGER_CONTRADICTS_TAG`);
without one, `CONSUMER_CURRENCY_UNKNOWN` stays the honest answer.

### Other gaps

* ~~**`Policy` is hardcoded defaults.**~~ **Closed.** Policy is declared: a
  `[policy]` section (`tag_aliases`, `ignore_branches`, `family_skew`,
  `rr_cache`, `[[policy.tag_equivalence]]`) plus `exclude` on each
  `[[consumer]]`. Excludes are per-consumer rather than a global list keyed by
  name, because a keyed list matches by string and so can miss silently — or hit
  the wrong consumer, which is what it did to the test fixture. Omitting the
  whole section falls back to the values that used to be compiled in, so a
  pre-`[policy]` manifest is unaffected; omitting a single *key* inside a
  present section means empty, so "we recognise no aliases" stays expressible.
  `checks::Policy` survives as the resolved shape the checks consume, built by
  `Policy::from_manifest`.
* **`prater-*` lives in a third repository** this tool cannot see, so the tag
  family comparison is structurally incomplete. `TAG_FAMILY_SKEW` says so in its
  own message rather than reporting a clean check that examined two thirds of the
  family.
* **`sodactl-mcp` compiles but has no entry point.** `serve_stdio()` is
  unreferenced and its `Opaque<T>` output-schema shim is pending `JsonSchema`
  derives on the core types.
* **`RR_STRAY_THISIMAGE` only surveys the tracked cache**, per its spec. The live
  cache under `.git/modules/wine/rr-cache` can carry stray `thisimage` files that
  `check` stays silent about; `rr-health` shows them. Live `thisimage` is
  transient local state, and firing Drift on it would be noise — but a pile of
  them means a `regen` was interrupted.

### Open questions for the maintainer

* **What does the trailing integer mean across components?** If everything
  carrying `-3` was cut together and is meant to be used together, family skew is
  a Blocker; if a component may legitimately lag between cuts, it is noise on
  every partial release. It ships at Drift with its reasoning printed because
  guessing either way is worse than asking. Still open — but the guess is now
  *visible*: `[policy] family_skew` in `tools/sodactl.toml` carries the value
  with a comment saying plainly that it encodes an assumption, not an answer.
* **`spritzwine`'s declared base is `winehq/wine-11.13`, which does not
  resolve** — wine releases are tags, and `spritzwine-11.13-3` was in fact poured
  onto the tag. Either the manifest should say `wine-11.13`, or the fetch
  refspec should bring tags in.

## The git backend: hybrid by capability

All git **reads** go through one trait, `GitRead` (writes go through `GitMut`
in `mutate.rs` — the git binary permanently, because `index.lock` is the real
mutex), with **three** implementations
whose roles are named:

* **`GitGix`** — gitoxide (`gix 0.85`, `default-features = false`), owning the
  18 methods it answers *correctly*. Typed results are the point: the binary
  wrapper's bug class — a leading space eaten from a filename, error
  classification by matching German stderr text, `ls-files -s` parsed
  positionally, a whole-buffer lossy UTF-8 conversion corrupting a path before
  the splitter saw it — is unwritable against a typed API.
* **`GitCli`** — the git binary, kept **complete** as the *reference backend*:
  the oracle the differential harness compares gix against. Behaviour changes
  land here first and gix must then match.
* **`GitHybrid`** — the production type: pure per-method delegation along the
  seam. 18 methods → gix; exactly two stay on the binary.

### Why two methods stay on the binary — the evidence

* **`missing_by_patch_id`** — gitoxide has **no patch-id** (zero hits for
  `patch[_-]id` across all 48 gix crates, nothing on the roadmap). A ~130 LOC
  hand-rolled reconstruction was tried and produced a **silent false clean**:
  it collapsed "unlink frame elements" and "unlink iframe elements" into one
  equivalence class and suppressed a real `STITCH_STALE` — the tool reports
  clean, the tier is stale, the consumer regenerates wrong. Never reimplement
  this off the binary.
* **`status`** — gix's dirwalk **diverges on this repo today**: it emits
  `.brv` (7 empty directories, 0 files — git has no concept of an empty
  directory under any `--untracked-files` setting) and double-counts the
  `wine` gitlink as TreeIndex + IndexWorktree where git emits one record.
  `checks/rr.rs` reports `dirty.len()`, so a naive port inflates it. Decided
  2026-07-19; revisit only with a measured gix status that matches git here.

Both holes are **typed**: `GitGix` returns `GitError::Unsupported` for them,
the differential report renders them unconditionally (even at "0 calls — NOT
EXERCISED"), and a tripwire test breaks if anyone "finishes the migration".

No behavioural deviations remain. The last one — `tags_pointing_at` on a
full-hex oid the repo does not contain was `Err` on gix but 0-empty on the
binary — was closed 2026-07-19 by making the binary loud too (`cat-file -e`
pre-check): `Ok([])` for an unreadable gitlink pin would bias toward a false
`PIN_UNTAGGED`, and a clear error beats a silent breakdown. Agreement is
pinned by `tags_pointing_at_a_missing_oid_errs_on_both_backends`.

### The differential harness

`Differential` is itself a `GitRead` that calls **both** backends, records
agreement, and lets the *reference's* answer flow onward — so the entire
production pipeline runs through it unmodified and gix is scored against
production's real argument distribution while the output stays the golden
output by construction. Three ways to run it:

```sh
# hermetic half -- fixture repos, every compared method; runs in plain CI:
cargo test --test differential

# live half -- the real checkout; #[ignore]d so the skip is structural,
# never a green bar that silently did nothing:
SODACTL_LIVE_REPO=/path/to/SpritzWine \
  cargo test --release --test differential -- --ignored

# the full production pipeline plus directed probes, over both live repos:
sodactl --manifest /path/to/tools/sodactl.toml selfcheck --differential
```

`selfcheck` exit codes are its own contract, deliberately not `check`'s:
**0** = backends agree, **2** = disagreements, **1** = the harness could not
run — so a `&&` chain can tell "gix diverged" from "the harness is broken",
and neither reads as `check`'s "gate tripped". Run it (and the live test)
before any `gix` version bump; both are read-only.

## Layout

```
crates/sodactl-git    GitRead; GitCli (binary, the oracle) + GitGix (gitoxide)
                      + GitHybrid (production) + Differential (the harness)
                      + GitMut (mutate.rs -- the ONLY mutating surface, one
                        audited spawn site, grep-provably no push method)
crates/sodactl-core   manifest, findings, reconcile, checks/, tags, rrcache,
                      plan (plan/apply, journal, backups), verbs/ (one module
                      per write verb)
crates/sodactl-cli    clap front end: status, check, rr-health, selfcheck, tui,
                      the write verbs (verbs.rs) and the build lanes (lanes.rs)
crates/sodactl-tui    ratatui dashboard (no tokio; crossterm only via re-export)
crates/sodactl-mcp    MCP surface (compiles; no entry point yet)
```

`GitRead` is the *only* git trait, and that is load-bearing rather than tidy.
There used to be two — `GitRead`/`GitCli` here and `GitQ`/`GitExec` in
`sodactl-core/src/checks/mod.rs` — nominally the same surface, in practice two
implementations that drifted, because hardening only ever landed on one of them.
That is how `reapply_verdict` came to build its own `Command` without `LC_ALL=C`
while every sibling call was locale-pinned, and how one `ahead_behind` matched a
stderr substring the other had already abandoned. Anything that runs `git` goes
through this crate: no production code in `core`, `cli` or `mcp` spawns `git`
itself (the CLI's single audited `Command::new` runs the build-lane *scripts*,
and the remaining spawn sites are test fixtures building scratch repos), and
adding one back reintroduces the mechanism, not just a line.

`reconcile()` is a pure function of a manifest and one `GitRead`, which is what
makes it testable against a fake with no repository on disk. The `checks` module
needs *both* repositories plus the filesystem, so `reconcile_workspace()`
composes the two rather than widening `reconcile()`'s signature — that width is
exactly what would cost the fake tests their ability to answer "unknown"
honestly.

Tests are in three layers: unit tests beside the code, `reconcile_fake.rs`
(synthetic DAGs, one story each, exact finding sets), and `reconcile_real.rs`
(a generated git fixture — a fake can encode a wrong mental model just as easily
as the code can).

Dependencies are kept minimal deliberately: this is a host tool that must never
enter the Wine build's dependency graph. The gix migration is the one large,
measured exception: +119 lockfile packages, all pure Rust on Linux (no cc, no
pkg-config, no `-sys` build steps), permissively licensed throughout. Every gix
feature is either compile-required or explicitly justified in
`crates/sodactl-git/Cargo.toml`. Note the tree's effective MSRV is **1.96.0
exactly** (`kstring 2.0.4`, pulled in via `gix-attributes`), not gix's own
declared 1.85 — fine on the pinned toolchain, brittle anywhere older.
