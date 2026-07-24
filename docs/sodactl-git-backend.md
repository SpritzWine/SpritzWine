# sodactl git access: gix vs. the binary — decision

## Verdict

**Keep the `git` binary. Adopt no gitoxide crate. Do the machine-readable-modes fix, and fold `GitRead` + `GitQ` into one trait.** The measurement does not vindicate the standing constraint's reasoning — gix reached 19 of 20 methods, matched the binary byte-for-byte on merged tree OIDs, and the one ABSENT method (`missing_by_patch_id`) was *rebuilt in ~130 LOC and reproduced git's equivalence relation on 410 real commits, including the binary-`.ico` `soda-branding` class that two earlier analyses called the hard stop*. The constraint was right by accident. It is overturned as an argument and sustained as a conclusion, on four numbers that were never measured before: **trait bodies come out 210 LOC gix vs. 210 LOC binary** (helpers 69 vs. 51, *plus* the 130 LOC of patch-id you would own forever), **dependencies go 102 → 229 crates**, **gix's `status` disagrees with git on this repo today** (10 items vs. 8) in the exact module where the known parser bugs live, and gix's only decisive perf win — `tags_with_targets`, 133 ms → 18 ms — sits behind **one call site** on a 200 ms run. What changes: nine methods get `-z` / `--porcelain=v2` / `--format`, `ahead_behind` stops classifying errors by German-locale-sensitive stderr text, `reapply_verdict` gets `GIT_OBJECT_DIRECTORY` (which buys gix's `with_object_memory()` advantage for one environment variable), the duplicate `GitExec` impl is deleted, and the false `lib.rs` doc claim about a 6.5 GB `status` bottleneck is corrected. Net ≈ **+5 LOC across 9 methods, −120 LOC from the fold, 3 new tests, ~1 day.**

## The measurements

gix **0.85.0** (released 2026-06-22, lockstep with `gix-ref` 0.65.0, `gix-diff` 0.65.0, `gix-merge` 0.18.0, `gix-status` 0.32.0, `gix-config` 0.58.0, `gix-index` 0.53.0); git **2.55.0**; rustc 1.96.0. Probe compiled and run read-only against `/media/SSD/Projekte/SpritzWine{,/wine}` — **12,434 tracked files, 6,697 refs, 6,616 tags** (all three re-verified by me just now).

| method | gix coverage | LOC gix / bin | agrees with binary | timing gix / bin |
|---|---|---|---|---|
| `rev_parse` | COMPLETE (`rev_parse_single("r^{commit}")`) | 3 / 4 | SAME | — |
| `rev_parse_raw` | COMPLETE (no auto-peel; gitlink native) | 3 / 8 | SAME | — |
| `tree_oid` | COMPLETE | 3 / 4 | SAME | — |
| `git_dir` (submodule `.git` FILE) | COMPLETE | 3 / 5 | SAME | — |
| `head_branch` (detached → `None`) | COMPLETE | 3 / 8 | SAME | — |
| `submodule_index_oid` | COMPLETE (typed `Stage`, `is_submodule()`) | 12 / 27 | SAME | — |
| `is_ancestor` | PARTIAL — derived from `merge_base` | 11 / 3 | SAME | 0.35 ms |
| `ahead_behind` | PARTIAL — `merge_base` + 2 hidden walks | 10 / 13 | SAME | 1.2 ms |
| `count_commits` | PARTIAL — manual range destructure + peel | 8 / 7 | SAME | — |
| `log_subjects` | PARTIAL — walk + `summary()` | 13 / 8 | SAME | 1.5 ms |
| `merges_in` | PARTIAL — `parent_ids().count()>1` | 13 / 8 | SAME | 1.1 ms |
| `local_branches` | COMPLETE | 8 / 7 | SAME (23) | 0.1 / 1.5 ms |
| `tags` | COMPLETE | 8 / 3 | SAME (6616) | 2.4 / 4.0 ms |
| `tags_with_targets` | COMPLETE (`.peeled()`) | 8 / 18 | SAME, **0 diff lines** vs `for-each-ref` | **17.6 / 132.8 ms** |
| `tags_pointing_at` | COMPLETE | 8 / 8 | SAME | 16.5 / **9.7 ms** ← bin wins |
| `branches_containing` | PARTIAL — `merge_base` per ref | 18 / 11 | SAME | 20.5 / **3.6 ms** ← bin wins |
| `config_file_get` | COMPLETE (real git-config parser) | 5 / 8 | SAME | — |
| `reapply_verdict` | COMPLETE (`merge_trees` + `has_unresolved_conflicts`) | 27 / 35 | **SAME 12/12**, all 3 verdicts, **byte-identical tree OIDs** | 1–5 ms both |
| `status` | **DIVERGES** | 40 / 17 | **10 items vs 8** | 10.1 ms in-proc / 20–30 ms wall |
| `missing_by_patch_id` | **ABSENT** — rebuilt | 6+130 / 8 | equivalence relation SAME on 410 commits; byte-identity **0/410** | — |
| **totals** | 12 complete, 6 partial, 1 divergent, 1 absent | **210 / 210** | — | full workload **44–47 / 198–210 ms** |

Re-measured by me on the live wine submodule, warm, three runs each: `git status --porcelain -z` = **0.03 / 0.02 / 0.02 s wall**; `git for-each-ref … refs/tags` = **0.12 / 0.12 / 0.12 s**. So the "git status is 5 ms" figure in the ecosystem report was in-process and excludes ~1.4 ms/spawn plus process teardown; end-to-end git is ~25 ms against gix's ~10 ms. **gix is modestly faster on `status`, not 9× slower** — that report's headline was measured backwards, in gix's disfavour, and I am correcting it rather than banking it. It does not change the verdict: 15 ms on a 200 ms tool nobody watches.

Other verified costs: **+127 crates (102 → 229, 52 of them `gix-*`)**, binary 5.78 → 8.04 MB, cold release build 25.2 s wall / 87.7 s CPU. **Zero C toolchain** — no `cc`, no `pkg-config`, no `bindgen`, no `*-sys` with native code; compression is `zlib-rs`, hashing pure Rust; license MIT OR Apache-2.0; MSRV 1.85 against your pinned 1.96. That property is real and is the single best thing about gitoxide. Against it: every crate sodactl needs (`gix`, `gix-merge`, `gix-status`, `gix-index`, `gix-diff`, `gix-revwalk`) is in gitoxide's own **"Initial Development"** stability tier, ~12 lockstep `0.x` bumps/year (of which only ~1–2 carry real API breaks — the churn argument is routinely overstated), bus factor **1** (Byron 13,120 commits vs. next contributor 862), ~$20.5k/yr funding.

## What gitoxide cannot do

**ABSENT (1): `missing_by_patch_id`.** `grep -rniE "patch[_-]id"` over all 48 resolved `gix-*` crate sources returns **zero hits**. No `gix-patch` on crates.io. Nothing on the roadmap. Also absent as primitives: `is_ancestor`, `ahead_behind`, `--contains`, `--merges`, range-spec walking, and `rerere` (zero hits; irrelevant — `rrcache.rs:1` is pure filesystem by design and sodactl never merges).

What it implies is **not** "the binary stays" — that claim, load-bearing in the mapping report, is false and the empirical build disproved it. What it implies is worse and more specific: **patch-id is reconstructible but its failure mode is silent false-clean.** The build took three iterations; the first two returned `Ok` and agreed with git on most commits while being wrong. (a) `diff_tree_to_tree` emits intermediate *tree* entries, so byte-identical diffs hashed differently. (b) A `-U0` diff merged `641dac59` ("unlink frame elements") with `7ebf2d34` ("unlink **i**frame elements") — same one-line change, different functions — into one equivalence class, i.e. a **suppressed `STITCH_STALE`**: tool says clean, tier is stale, consumer regenerates wrong. Only a class-level cross-check caught either. Adjacent evidence: gix's diff hunks match git on **98.91%** of files (181/183 over 60 wine commits), and both divergences landed on `soda-ntdll` and `soda-core` — the authored tier commits the checks exist to verify.

**DIVERGENT (1): `status`.** gix 10 items, git 8. `.brv` (7 directories, **0 files**) is emitted by gix's dirwalk and by git under *no* `--untracked-files` setting, because git has no concept of an empty directory. And `wine` double-counts as `TreeIndex` + `IndexWorktree` where git emits one record — `checks/rr.rs` reports `dirty.len()`, so a naive port inflates it. These are policy differences you would own, not bugs.

## Was the "no second git implementation" constraint justified?

**As written, no — it is overturned. As a decision, yes — it is vindicated, on evidence it never had.** Say both plainly.

Overturned as reasoning: the constraint would have rejected `merge_trees`, which I have verified produces **byte-identical merged tree OIDs** to `git merge-tree --write-tree` across every tested case and is *cleaner* about side effects (`with_object_memory()` wrote 0 loose objects; the binary moved the wine odb 61→67). It would also have rejected `gix-config`, which implements git's documented source precedence and the full `includeIf` set, and `gix-ref`, which peeled all 6,616 tags with **zero diff lines** against `for-each-ref`. Linking a library that reads git's *formats* is not a second implementation of anything. Two prior analyses in this project reached "build our own / keep what we have," one demonstrably biased; the constraint is the same instinct in policy form, and it was asserted rather than measured. It deserved this scrutiny.

Sustained as a conclusion, for one reason the constraint's author did not state: **the boundary that matters is formats vs. policies.** gix reads git's on-disk formats correctly — refs, index, trees, config, commits — and every measured agreement is on that side. Where git's answer is defined by an undocumented, versioned, heuristic *policy* — patch-id normalization, diff hunk splitting, dirwalk/ignore semantics — gix either lacks it or diverges, and every measured disagreement is on that side. That is not a coincidence and it is not fixable by gitoxide maturing on a schedule you control.

**Rewrite the constraint as: *do not re-implement git's policies — patch-id, merge resolution, dirwalk/ignore, refname resolution — where git's own machine-readable output is available.*** Under that rule gix loses today on measurements, not on principle, and the door is explicitly open.

I also reject the hybrid, and not because the seam is fuzzy — the format/policy line is crisp and predicts the results rather than being fitted to them. I reject it because **the region it encloses is empty of value**: gix's sole clear win (`tags_with_targets`, 115 ms saved) has one call site, while `tags_pointing_at` and `branches_containing`, where gix measured *slower*, have five between them. Buying 115 ms with +127 crates, a second absence doctrine (`downcast_ref::<gix::refs::file::find::existing::Error>()` alongside exit-code discipline, neither exhaustive-checked), a two-snapshot TOCTOU hazard (gix holds an opened packed-refs/index view; `build.sh`'s `git submodule update --force` rewrites the gitlink mid-run), and a doubled conformance-test surface is a bad trade. A crisp seam around nothing is still a seam.

And the hybrid's last argument — typed-API safety — is retired by the binary itself. Verified on this repo just now:

```
$ git status --porcelain=v2 --untracked-files=all | grep wine
1 MM S..U 160000 160000 160000 8bf9f2c8e838…a9ceff26a e3a386db5849…e71141acd wine
```

**One** record (no double-count), explicit `160000` modes (gitlink-vs-blob unrepresentable), **and both the HEAD gitlink and the index gitlink in the same record** — that is `rev_parse_raw("HEAD:wine")` *and* `submodule_index_oid("wine")` for free, from the one method gix gets wrong. Likewise `git cat-file --batch-check='%(objectname) %(objecttype)'` returns the structured token `missing` at exit 0 for an absent ref and `submodule` for a gitlink; `git ls-files --format='%(stage)'` makes stage-1/2/3 an explicit field; `%(objecttype)` distinguishes `tag` from `commit`, closing the silent annotated-peel trap that bit two probes. Every structural-safety property claimed for gix is available from the binary in a machine-readable mode.

## The porcelain fix

Worth doing regardless of the gix decision. All invocations verified against git 2.55.0 on this host.

| site | change |
|---|---|
| `sodactl-core/src/checks/mod.rs:331` `status` | `--porcelain=v2 -z`; replace `split_at(2)` + `trim_start()` with `rec.get(3..).ok_or_else(…)`; malformed record → `Err`, not `continue`. **Live bug**: `" leading-space.txt"` currently returns `leading-space.txt` — the separator and the filename's own space are both eaten. `lib.rs` uses `rec.get(3..)` and is correct. |
| `checks/mod.rs:235` `ahead_behind` | **Highest-value single change.** Delete `stderr.contains("unknown revision")`. Pre-verify each endpoint with `git rev-parse --verify --quiet '<rev>^{commit}'` (exit 1 absent / 0 present, no stdout noise) or the `cat-file --batch-check` `missing` token; then treat *any* nonzero from `rev-list` as `Err`. This host's git speaks German (`Fehler:`, `Verwendung:` in raw output) — only `LC_ALL=C` is holding that line up, and no flag or exit code fixes it. |
| `sodactl-git/src/lib.rs:332` `reapply_verdict` | Route through `self.exec` — it currently builds its own `Command` with **no `LC_ALL=C`**, which is how the two impls drifted. Add `-z`. Set `GIT_OBJECT_DIRECTORY=<scratch>` with `<scratch>/info/alternates` → real odb. Measured: 53 objects to scratch, **real loose count 67 → 67**. gix's `with_object_memory()` advantage for one env var. |
| `git_dir` | `git rev-parse --absolute-git-dir` (drops the manual join; note it exits 128 in bare repos). |
| `submodule_index_oid` (`checks/mod.rs:354`, `lib.rs:504`) | `git ls-files -z --format='%(objectmode) %(objectname) %(stage) %(path)'`; require mode `160000`, stage `0`. |
| ref queries | `git for-each-ref --format='%(objecttype)%00%(refname:short)%00%(objectname)%00%(*objectname)' refs/tags` — **`for-each-ref` has no `-z`** (verified: `Fehler: Unbekannter Schalter 'z'`); `%00` in the format works, and line-splitting is safe because refnames categorically forbid space and newline (`check-ref-format` verified). Error when `%(objecttype)` is `tag` but `%(*objectname)` is empty — the false-Blocker direction `lib.rs:179` warns about, now checkable. Keep `git branch --contains` (3.6 ms). |
| `log_subjects` / `merges_in` | `git log -z --format='%H%x09%P%x09%s'`, records on `\0`, fields on `\t`, **with an arity check** (serie's `load_commits_format()` idiom — one place defines the format so writer and parser cannot drift). |
| `lib.rs:237`, `checks/mod.rs:155` | **Stop `String::from_utf8_lossy`-ing the whole buffer.** Return `Vec<u8>`, split on `b'\0'`, decode per field. A Latin-1 filename is U+FFFD before the splitter sees it and then silently fails a glob — same signature as the four named bugs, immune to every flag. This is the one defect `-z` alone does not fix. |
| **do not change** | `git cherry` output is `+ <sha>`, fixed-width, no path — `split_whitespace().nth(1)` is categorically safe. `%s` folds multi-line subjects to one line, so `log_subjects`' line-splitting is already correct; "fixing" it breaks it. `\x1f` is **not** escaped inside `%s` — serie's unit-separator idiom is a heuristic; only fixed-width fields (`%H` = 40 bytes) or `%x09`-with-arity-check are safe. |

`-z` is categorical for the path-bearing class — NUL is the one byte a pathname cannot contain, and all four named bugs are in that class. It is *not* categorical for error classification, in-value separators, or encoding; those three need the structural fixes above.

## Recommendation and build order

1. **Fold `GitRead` + `GitQ`; delete `GitExec`.** ~−120 LOC. **~3 h.** Do this *first*. The disease is not human-readable output — it is two impls of one trait that drifted, and only `sodactl-git` got the hardening. Fixing parsers without deleting the duplicate leaves the mechanism intact.
2. **`ahead_behind` error classification** (`rev-parse --verify --quiet` pre-check). **~1 h**, 1 test. Highest correctness value in the plan; live hazard on this locale.
3. **`status` → `--porcelain=v2 -z`, both call paths.** **~1.5 h**, 2 tests (leading-space path verbatim; malformed → `Err`).
4. **Byte-level exec + per-field decode.** **~1 h.** Blocks nothing but unblocks everything's correctness.
5. **`-z`/`--format` propagation** to `submodule_index_oid`, `config_file_get`, `log_subjects`, `merges_in`, ref queries. **~2 h.**
6. **`reapply_verdict`: `self.exec` + `GIT_OBJECT_DIRECTORY`.** **~1 h.** Stops writing unreachable loose objects into Florian's repo.
7. **Correct `lib.rs:3-5`.** **~5 min.** It asserts a `status` bottleneck over 6.5 GB; the tree is 12,434 tracked files, git does it in 20–30 ms, and the 6.5 GB is pack data. Leaving that stale premise in place is what guarantees this gets re-litigated on a number nobody measured. **Do not skip this — it is the cheapest item and the one that protects the decision.**

**Total ≈ 1 day.** No `Cargo.toml` change: 102 packages, no `gix*`, no `git2`.

**Re-check in ~18 months, or immediately if:** gitoxide ships patch-id; `gix-merge`/`gix-status` leave "Initial Development"; or sodactl must run where a `git` binary cannot be assumed. That last one is the only scenario measured today that genuinely flips the calculus — the C-toolchain-free property is a real asset there and nowhere else.

Disclosure: the probe runs left ~19 unreachable loose objects in `.git/modules/wine/objects` from `git merge-tree --write-tree`. Harmless, unreachable, collected by any future gc. No refs, index, worktree or config were touched; HEAD is `f74f1c74`, 6,697 refs unchanged.

## Open questions for Florian

1. **Does `reconcile.rs:93` get a ticket?** It parses sodactl's *own prose findings* with `split_whitespace()` hunting for hex words, to alias two checks together. That is more fragile than anything in the git layer, no git flag or library touches it, and it is out of scope for this work — but it is the next parser bug waiting to happen.
2. **`missing_by_patch_id` semantics: keep `git cherry`, or move to `rev-list --cherry-mark --left-right`?** The latter is the plumbing equivalent and drops the limit-argument footgun documented at `lib.rs:152-153`. It is a *semantics* change, not a parsing fix — own commit, own tests. Your call whether it is in this cycle.
3. **`status()` behaviour changes need your sign-off**: paths with leading whitespace are now returned verbatim, so a `build_artifacts` allowlist entry that accidentally matched the trimmed form stops matching; and a malformed record now errors instead of being silently skipped. Both are the correct direction, both can turn a previously-green run red. Acceptable?
4. **Is `.brv` meant to be in `status` output at all?** git omits it (7 directories, 0 files, not ignored). If sodactl should notice empty scaffold directories, that is a filesystem check like `rrcache.rs`, not a git one — and it is the one place where gix's "wrong" answer might actually be the one you want.
5. **`GIT_OBJECT_DIRECTORY` scratch location** — tempdir under `/tmp`, or inside the repo's `.git`? Cross-filesystem hardlinking of alternates is the only wrinkle, and the wine odb is on the SSD.