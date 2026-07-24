# sodactl TUI: reuse decision

## Verdict

Build sodactl's TUI directly on **ratatui 0.30.2 with `default-features = false, features = ["crossterm"]`**, plus exactly three widget-layer dependencies — **`tui-tree-widget` 0.24.0** (both tree panes), **`ansi-to-tui` 8.0.1** (git's own coloured output, including the real merge-lane graph), and **`crokey` 1.4.0**` (keybindings + the help pane rendered from the same map) — and write the app shell, focus model, and detail pane yourself. No framework (`tui-realm`, `rat-salsa`, `ratatui-kit` all rejected), no second git implementation (`asyncgit`, `git2-hooks`, `gleisbau`, `git-branchless-lib`, `jj-lib` all rejected on the standing constraint), and no vendored graph-layout algorithm — the tier manifest is a **tree**, not a DAG, and the one place a real DAG appears is rendered for free by `git log --graph --color=always` piped through `ansi-to-tui`. Total cost measured empirically: **~11 crates over a 91-crate ratatui-crossterm baseline, with zero git2 / gix / libgit2 / libssh2 / OpenSSL in the lock file.**

The separately important verdict, which has nothing to do with the TUI: **replace patch-id-only equivalence with StGit's reapply-emptiness as a slow path.** That is the highest-value finding in the entire investigation.

---

## What we reuse

| Crate | Version (date) | What it gives us | Cost | Maintenance risk |
|---|---|---|---|---|
| **ratatui** | 0.30.2 (2026-06-19) | Everything. `ratatui::run(\|t\| app.run(t))` deletes terminal setup/teardown/panic-restore outright (`init.rs:397` — `set_panic_hook` + raw mode + alt screen) | **91 crates** with `default-features = false, features = ["crossterm"]`; **182** with defaults (0.30 made `termwiz` 0.23.3 and `termina` 0.3.3 default backends). No git anything | Unavoidable, and the only real churn axis. 4 breaking releases in ~2 years; 0.30 split into `ratatui-core 0.1` / `ratatui-widgets 0.3`, renamed `Alignment` → `HorizontalAlignment`, MSRV 1.88. 14.5M recent downloads |
| **tui-tree-widget** | 0.24.0 (2026-01-09) | Findings pane (severity groups → multi-line finding rows) **and** tier pane (tier → commits). Selection is an **identifier path**, not an index — `Node::{SeverityGroup, Finding, Tier, Commit}` carries domain identity | **+1 crate** (`unicode-width`). Depends on `ratatui-core ^0.1` / `ratatui-widgets ^0.3`, not the facade — pulls no backend. No git anything | Single maintainer (EdJoPaTo), ~annual releases with CI-only commits between (repo active 2026-07-01) — *finished and tended*, not abandoned. 225k recent / 833k total downloads. Had a `usize` → `Identifier` break at 0.14. **1,084 LOC — vendorable if it stalls.** Docs 43.75%; `examples/example.rs` is the real doc |
| **ansi-to-tui** | 8.0.1 (2026-01-10) | `git log --graph --color=always` → `ratatui::text::Text` with real merge lanes, git's own colours, BOLD/DIM preserved. Verified at cell level against the real repo: `buf[(3,1)].fg == Color::Yellow` | **+4 crates** (`nom 8`, `simdutf8`, `smallvec`, `thiserror 2`). No git anything | Maintained by the **ratatui org**. 1.35M recent / 3.76M total. **Frozen problem domain** — ANSI SGR has not changed since the 1980s; an unmaintained ANSI parser is a finished one. Only churn vector is `ratatui-core`'s `Text`/`Style` types |
| **crokey** | 1.4.0 (2026-02-01) | `HashMap<KeyCombination, Action>` deserializes straight out of the existing `toml` dep; `key!(ctrl-c)` compile-checked match arms; `KeyCombinationFormat` renders the help pane **from the same map** | **+4 crates** (`crokey-proc_macros`, `once_cell`, `strict`). Re-exports `crossterm 0.29` — the exact version ratatui 0.30.2 uses, single copy in the lock. **Do not add crossterm directly.** No git anything | Canop (broot). 1.43M recent / 4.18M total. 1.x semver. **Structurally the safest of the three: it sits on the crossterm axis, not the ratatui axis**, so a ratatui 0.31 cannot block it |
| **insta** (dev) | 1.48 (2026-06-11) | Snapshot review flow over `TestBackend` buffers | dev-dependency only | 20.8M recent. Trivial risk |

**Rejected, with the specific reason:**

- **`tui-widget-list` 0.15.3** — the probe reproduced a **process abort**, not a mis-render: `index out of bounds: the len is 2 but the index is 9`. `ListView::render` hands a stale `state.selected` into your builder closure without clamping to `item_count`. A control test confirms **ratatui's own `List` clamps silently** (`Some(9)` on a 2-item list → `Some(1)`). Adding the clamp still leaves a stale scroll offset (item 0 scrolled off the top). It aborts on the interaction the findings pane performs most — *change the severity filter*. Also redundant once findings live in the tree.
- **`asyncgit`, `git2-hooks`, `git2-testing`, `gleisbau`, `git-graph`, `git-branchless-lib`, `jj-lib`, `esl01-dag`** — second git implementation. `esl01-dag` is additionally a **dead crates.io mirror**: max version 0.3.0, 2023-01-27.
- **`tui-realm` 4.1.0 / `rat-salsa` 4.0.3 / `ratatui-kit` 0.10.3** — frameworks for three read-only panes. tui-realm is alive (28k recent) but its *smallest* example is 243 lines for a label and a progress bar, it does not save the layout code, and its `Attribute`/`AttrValue` prop bag would flatten an already-typed `Finding` into stringly-typed soup — the same category of error as forking gitui. `rat-salsa` is the best conceptual fit in the ecosystem and has **997 recent downloads** on a seven-month-old rewrite; you cannot bet a tool that force-pushes generated branches on that. `ratatui-kit`: 807 recent downloads, four releases in nine days.
- **The ratatui component template as a project** — never `cargo generate` it. Its `[build-dependencies]` carry **`vergen-gix 10.0.1`**: gix arrives silently, for a version string, in direct contradiction of the standing constraint. Its `errors.rs` also forces `color-eyre` on an `anyhow` workspace, and its `app.rs` (`Vec<Box<dyn Component>>`, every component handed every action and the full frame area) has the same flaw gitui was rejected for.
- **`filetreelist`, `scopetime`, `tui-scrollview`, `tui-popup`, `throbber-widgets-tui`, `tui-logger`, `ratatui-textarea`, `tui-nodes`, `scm-record`** — none load-bearing here. Add a throbber later if git latency actually reads as a hang; `scm-record` is high quality but hunk selection is explicitly out of scope.

---

## What we lift with attribution

All MIT. Copied, not depended on; MIT headers retained in-file with the upstream path and commit.

| Source | What | Obligation |
|---|---|---|
| `gitu/src/cmd_log.rs` (altsem/gitu, MIT, ~110 LOC) | The **design**, not the code: `Vec<Arc<RwLock<CmdLogEntry>>>` with `push_cmd(&Command)` / `push_cmd_with_output`, formatted to `ratatui::text::Text`. This is exactly the recording `GitRead` decorator the provenance requirement needs. Rewrite against sodactl's own types | Credit in a comment; no copied lines means no header needed, but credit anyway |
| `ratatui/templates` @ `cd2b97b`, `src/tui.rs` (MIT, ~30 relevant lines of 234) | `exit()` + `signal_hook::low_level::raise(SIGTSTP)` + `EnterAlternateScreen` on resume. **Only if** the detail pane lets the user actually run the git command it shows. `ratatui::init()` does *not* handle suspend | MIT header retained on the copied block |
| `gitui/src/ui/mod.rs` (MIT) | `calc_scroll_top` (8 LOC), `centered_rect` (~25 LOC) | MIT attribution comment |
| serie (`lusingander/serie`, MIT) | The **invocation idiom only**: `git log --date-order --pretty=<fmt> --date=iso-strict -z` — NUL-delimited, robust to arbitrary commit summaries. Copy the argv, not the code | Comment credit |
| git-machete (VirtusLab) | **Vocabulary**, free of charge: `slide-out`, per-edge `rebase=no` qualifiers, per-**edge** red/green sync rendering | None |

**Explicitly not vendored: serie's `graph/calc.rs` (~650 LOC).** The git-tuis survey recommended it; the probe overturns that. `[[tier]]` declares a **scalar `parent`** (`sodactl-mcp/src/graph.rs:90` derives `Edge { parent, child }` from that one field), so crossing edges are structurally impossible and lane assignment has nothing to assign. Where a real DAG *does* exist — the consumer/wine-base history — `git log --graph` + `ansi-to-tui` renders it correctly and can never drift from what git thinks the history is. Vendoring 650 lines of lane math you own forever, to solve a problem the manifest cannot pose, is the worst trade on the board.

---

## What we write ourselves, and why that is not NIH

Each item, with the specific reason no crate covers it:

1. **Event loop** (~80–120 LOC) — `std::thread` + `mpsc` + `event::poll(100ms)` + a generation counter to discard stale reconcile results. **Not tokio**, despite `sodactl-cli` already depending on it for the MCP subcommand; a 0.79s synchronous reconcile does not need an async runtime, and the template's `tokio::select!` loop exists to serve features (bracketed paste, tick/render split) we do not have. *Non-NIH reason:* the only packaged alternatives are frameworks whose event loops come bundled with a component model we rejected on independent grounds.
2. **Three-pane focus model** (~60–150 LOC) — an enum and a `match`. *Non-NIH reason, stated honestly:* this is cheap **because N=3**, not because it is architecturally superior. It is structurally the same hardwiring gitui's `app.rs` was rejected for, at 1/10th the scale. If the pane count grows, we inherit that problem. Owning that now is the price of not adopting a framework.
3. **Findings pane construction** (~150 LOC) — building `TreeItem`s from a severity-sorted `Report`. The widget supplies navigation; the domain mapping is ours by definition.
4. **Tier pane recursion** (~20 LOC measured in the probe) — recursion over `Option<&str>` parents. No library can be smaller than 20 lines.
5. **Detail pane** (~60–80 LOC) — most of the prose already exists (see item 8).
6. **Severity → `Style`** (~15 LOC) — the CLI has *no* colour dependency today, so this is genuinely new, and genuinely trivial.
7. **Adapter modules `ui::tree` / `ui::ansi` / `ui::keys`** (~60 LOC total) — the hedge that turns "the dependency died or blocked a ratatui bump" into a one-file change, and vendoring into a `cp -r` plus a path dependency. Write these on day one, not after the first breakage.
8. **Finding provenance in `sodactl-core`** (200–400 LOC, 1–2 days) — `Finding { code, severity, subject, summary, remedy }` has **no provenance field**, there are **48 `Finding::new` call sites**, and `GitCli` builds argv internally and discards it. A recording `GitRead` decorator is real design work because attributing several git calls to one finding is not structural. **This is core work, framework-independent, and the item most likely to be missed in any estimate.** It is required by requirement (3) under every option considered.
9. **Presentation extraction into shared code** — `render_pin`'s `STAGED` divergence logic (`sodactl-cli/src/main.rs:326-341`), the remote-status mapping (`main.rs:271-276`, whose comment explains why ahead-only rendering is dangerous), the `contains_parent_tip` tri-state strings. Currently trapped in `println!`. A TUI reimplementing pin divergence by hand creates a **second source of truth for a Blocker-adjacent fact the tool exists to get right.** Worth doing whether or not the TUI ships.
10. **Move `explain.rs`'s `CATALOGUE` from `sodactl-mcp` to `sodactl-core`** (~1 hour) plus the `catalogue_covers_emitted_codes` test. It is most of the detail pane's prose already written — but reaching for it where it lives drags `rmcp 2.2.0` + tokio into the TUI. Must be planned, not discovered.
11. **Two one-line chores** — `#[derive(Hash)]` on `sodactl_core::Severity` (`finding.rs:14` derives `PartialOrd, Ord, Serialize` but not `Hash`; `tui-tree-widget` requires `Identifier: Clone + PartialEq + Eq + Hash`), and pinning ratatui with `default-features = false`.

---

## Was the previous "build our own" conclusion wrong?

**The conclusion survives. The reasoning that produced it does not.** Florian's suspicion of bias is correct, and it was correct in three specific, nameable places.

**Where it was biased:**

1. **It rejected at the largest granularity and never re-examined at the smaller one.** "No framework fits" was treated as if it settled "no components fit." The probe refutes that empirically: four candidates compiled first try and added 11 crates total, none touching git2/gix/OpenSSL. The architectural constraint that killed the gitui fork **does not bind at the widget layer at all**. Rejecting `asyncgit` was right and says nothing whatsoever about `tui-tree-widget`.
2. **It set the bar at "would I write this?" instead of "would I write this *correctly*?"** The strongest datum in the whole investigation is an accident: `tui-widget-list` 0.15.3 — a real, maintained crate with 71k quarterly downloads — **panics** on the filter-shrink transition. A dedicated widget author, working on exactly this problem, shipped the bug. The probability a bespoke sodactl findings list gets it right first try is correspondingly low. `tui-tree-widget` is *structurally immune* because selection is an identifier path, not an index — proven by `tree_selection_survives_filtering`, which selects `Finding("WORKTREE_DIRTY")`, re-renders with a blockers-only list, and simply works. That is a bug class deleted by data model, not a convenience.
3. **It missed `ansi-to-tui` entirely.** The correct reuse target for graph rendering was never a Rust lane-layout crate — it was the `git` binary sodactl is already required to shell out to. That is tig's whole architecture, and it is philosophically identical to sodactl's read-only constraint. ~650 LOC avoided, with zero drift risk.

**Also missed by everyone, including the surveys:** `ratatui = "0.30"` with default features costs **182 crates**, not 91, because 0.30 made `termwiz` and `termina` default backends. Four independent surveys did not flag it. On a tool whose entire premise is a quarantined, minimal dependency surface, that is exactly the kind of silent doubling the earlier analysis was supposed to be guarding against.

**On `filetreelist` specifically — dismissed too fast, but the verdict holds for a better reason.** It *is* a genuine scroll/collapse/selection state machine (`filetree.rs:144 move_selection(MoveSelection) -> bool`, `window_height: Cell<Option<usize>>`, `visual_selection()`, `iterate()`), with `thiserror` as its only dependency, 19k recent downloads, updated 2026-03-24. The earlier dismissal ("we have no file tree") was cosmetic and wrong. The correct rejection is that it is **welded to paths at the identity level**: the only constructor is `FileTree::new(list: &[&Path], collapsed: &BTreeSet<&String>)` with no payload slot; `indent = path.ancestors().count() - 2` (`item.rs:120`); `Ord`/`PartialEq` are by path (`item.rs:186-202`), so ordering is lexicographic and severity-sorted findings are unreachable; and `FileTreeItems::new` calls `fold_paths` **unconditionally** with no off switch (`filetreeitems.rs:26`), so a tier with one commit silently folds into a single `soda-core/abc1234` row — wrong output, not a cosmetic quirk. Any `/` in a branch name corrupts the hierarchy. Reject on identity/ordering/folding semantics.

**The stacked-branch angle was not considered at all, and that is the real gap** — see the next section. It did not change the TUI answer, but it surfaced a live correctness bug in the core.

**Net:** the earlier conclusion was right by luck as much as by analysis. Correcting the bias subtracts roughly **1,000 lines of would-be-hand-rolled code** (tree navigation, ANSI parsing, ANSI-graph lane layout, key parsing) and **fixes one correctness bug** — but it does not find a base to fork. Reuse here lives at the component and algorithm layer, not the application layer. That is a measured result: `git-branchless-smartlog`'s renderer was read at source and is lifetime-bound to a git2 `Repo`, needs `EventReplayer`/`EventLogDb` (SQLite) and `Dag` from a dead crates.io mirror, and emits `cursive_core::theme` strings — not ratatui. 634 recent downloads confirms nobody links it.

---

## The stacked-branch finding

This family contributes **one algorithm and one vocabulary. Zero code.** No Rust crate in it is linkable without importing a second git implementation or an entire VCS.

**How each tool detects a stale child after the parent moves:**

| Tool | Primitive |
|---|---|
| git-branchless | Recorded obsolescence metadata — git hooks write "X rewritten as Y" into a SQLite event log; `git sl` flags descendants of obsoleted commits |
| jj | Durable **change-id** on every commit; descendants auto-rebase on any edit |
| git-machete | Ancestry (`is-ancestor` parent-tip vs branch) + reflog-scanned fork-point; optional squash-merge detection with a fast/exact split |
| git-town | Declared parent in git config (`git-town-branch.<b>.parent`) + ancestry; rebase-onto **only if there are commits to remove** |
| StGit | **Reapply the patch onto the new base; if the result is empty, it was already merged** |
| git-stack | patch-id-ish (could not confirm at source level) |

**Is their primitive better than sodactl's patch-id? Yes — StGit's is, and it fixes a live bug.**

`git patch-id` hashes diff hunks with whitespace normalisation, but it remains **context-sensitive**. When a tier is replayed onto a newer Wine base and surrounding lines drifted — or when **git-rerere resolves a conflict during replay**, which is sodactl's central mechanism — the replayed commit's patch-id differs from the original despite being the same change. Patch-id therefore returns false "not yet applied" verdicts on **precisely the commits the rerere pipeline worked hardest on**. That is the highest-consequence failure class in the tool.

StGit does not hash. `stg pull --merged` reapplies each patch and asks whether the result is empty; the man page's note that this *"costs performance"* is the tell that it is doing real merge work rather than hashing. git-machete independently landed on the same two-tier shape (`machete.squashMergeDetection simple` vs a slower exact mode). Two mature, unrelated tools converging is decent evidence the shape is right.

**Adopt: patch-id as the fast path, reapply-emptiness as the slow path for anything patch-id reports as missing.** It costs nothing — `git` already does it — and it is worth more to sodactl's correctness than the entire TUI.

**jj's change-id is the theoretically correct primitive** and would be the green-field answer: a durable identity surviving rebase, cherry-pick and conflict resolution. It exists only because jj owns the commit backend and writes the id at creation. You cannot obtain change-ids for commits produced by `git rebase --onto` and `git cherry-pick`. The ceiling exists; it is not reachable from here.

**Adopt from git-machete as design, not code:** `slide-out` (remove a tier whose content landed upstream in Wine, reparent its children — sodactl will need exactly this, and the semantics are already worked out); per-edge qualifiers (`rebase=no` generalises to stitch/consumer variance without new vocabulary); and **per-edge red/green sync rendering** — "green = child in sync with parent, red = parent has commits the child lacks." That is a per-*edge* fact, not a per-node one, and it is the right visual grammar for the tier pane.

**Do not adopt git-machete's staleness model.** Its edges are single-parent, ancestry-and-reflog based; it assumes branches are rebased, not cherry-pick-linearized, and its fork-point machinery needs local reflogs. A stitch has no ancestry relationship to its sources and a freshly generated consumer has no reflog. Machete would report every one of them as fully out of sync. Same manifest *shape*, incompatible *staleness model*.

Also worth borrowing: git-town's term **"phantom conflicts"** is the industry name for the failure sodactl hit — good `Finding.code` vocabulary.

---

## Revised plan

**Phase 0 — prerequisites (0.5 day).** Not TUI work; all of it is worth doing regardless.
- `#[derive(Hash)]` on `sodactl_core::Severity`. *(5 min)*
- Move `explain.rs`'s `CATALOGUE` + its coverage test from `sodactl-mcp` to `sodactl-core`. *(1 h)*
- Extract `render_pin`, the remote `+a/-b` mapping, and the `contains_parent_tip` tri-state out of `main.rs` into a shared presentation module returning `String`. *(2 h)*

**Phase 1 — Finding provenance in core (1–2 days, 200–400 LOC).** A recording `GitRead` decorator (design shape from gitu's `cmd_log.rs`) plus a `provenance` field on `Finding`. The wide range is honest uncertainty about whether the decorator can attribute multi-call checks without restructuring them. **Do this before the TUI**, because requirement (3) depends on it and it is framework-independent.

**Phase 2 — shell + adapters (0.5 day).** `ratatui::run()`, the `std::thread` + `mpsc` + generation-counter loop, the three-pane enum, and the `ui::tree` / `ui::ansi` / `ui::keys` adapter modules. Note the refactor that bites: `Workspace` holds `&GitExec` borrows (`main.rs:125-130`), so the worker thread must construct its own `GitExec`/`GitCli`.

**Phase 3 — findings pane (1 day).** `tui-tree-widget`, severity groups, multi-line rows, tick state. **Write the warm-up render guard on day one**: `select_first`, `select_last`, `key_up/down`, `select_relative` are **no-ops before the first `render()`** — they read `last_identifiers`, populated only by `render` (`tree_state.rs:23-35, 174`). This bites on startup ("select the worst finding") and in every headless test. `state.select(vec![path])` works cold. Comment the call site with why.

**Phase 4 — tier pane + detail pane (1 day).** 20-line parent recursion, commits as expandable children, per-edge red/green status. Detail pane pulls `CATALOGUE` prose + provenance; `ansi-to-tui` for the wine-base/consumer history sub-pane. **`ansi-to-tui` panes are pagers, not interactive views** — there is no line→oid mapping; interactive selection over that data needs a parallel `git log -z --pretty=…` pass.

**Phase 5 — keys + help (0.5 day).** `crokey` map in `tools/sodactl.toml`; help overlay rendered via `KeyCombinationFormat` from the same map.

**Phase 6 — snapshot tests (0.5 day).** `TestBackend` + `insta`. Nearly free *if* the TUI is a pure function of `Report` — keep it that way.

**Phase 7 — core, decoupled from all of the above: reapply-emptiness slow path** behind the patch-id fast path. *(0.5–1 day.)* Highest correctness value in this document.

**Totals: TUI ~600–800 LOC / 3–4 days. Core work (phases 0, 1, 7) 2–3.5 days.** Reuse cuts the TUI figure by roughly a third against fully hand-rolled and removes two bug classes outright — index-based selection invalidation, and graph-lane layout.

---

## Open questions for Florian

1. **Does the detail pane let you *run* the git command it shows, or only display it?** This is the biggest fork in the road and it must be answered before phase 2. If yes, we lift the template's SIGTSTP suspend block (~30 lines, MIT) and the event loop must handle terminal handoff. If no, `ratatui::run()` + blocking `event::read()` is genuinely sufficient and we skip that file entirely.
2. **Are the "ticks" on the findings checklist session-local or persisted across runs?** Session-only is +20 LOC. Persisted keyed by `(code, subject)` is +60 LOC **and a genuine design question**, because `subject` is a ref or path that legitimately changes between runs — a tick would silently follow or silently orphan.
3. **Are rebindable keys actually wanted?** I have adopted `crokey` on the strength of the help pane rendering from the same map (no second hardcoded list to drift). If fixed keys are fine and you'd rather have 4 fewer crates, drop it — the loss is a 20-line `match` and a hand-maintained help list. It is purely additive later.
4. **Provenance capture: decorator or hand-written strings at 48 sites?** The decorator is correct but may not attribute cleanly without restructuring checks; hand-written strings are free to drift from what the code actually runs — the same drift risk `explain.rs:33` already documents about itself. This drives the 200 vs 400 LOC spread.
5. **Should reapply-emptiness run in the default reconcile, or behind a flag?** It costs real merge work per candidate commit. The current run is 0.79s; this could move it materially. StGit and git-machete both make it opt-in/slow-path.
6. **Do you want the git-machete vocabulary adopted verbatim** (`slide-out`, `rebase=no`, per-edge red/green), or should sodactl coin its own terms? Borrowing buys familiarity for anyone who has used machete; coining avoids implying machete's ancestry-based semantics, which sodactl deliberately does *not* share.