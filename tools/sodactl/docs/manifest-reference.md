# `sodactl.toml` — manifest reference

The manifest lives at `tools/sodactl.toml` in the superproject. `sodactl` resolves
the superproject root as the manifest path's **grandparent**, so the default
invocation only works from the superproject root or with an absolute `--manifest`.

See `docs/BRANCHING.md` for the model this file encodes. This sheet is the schema,
derived from `crates/sodactl-core/src/manifest.rs` and the checks that consume it.

---

## The one rule

**The manifest declares POLICY and TOPOLOGY EDGES. Git is the sole authority on
where refs are. Geometry — commit counts, ahead/behind, staleness, "this tier has
N commits" — is always DERIVED and must never appear as a key.**

The reason is not tidiness. A hand-written count is a lie waiting to happen while a
destructive operation depends on it: `restack`, `regen` and `tag` all rewrite
history, and a stale `commits = N` sitting in a config file is what turns a wrong
number into a duplicated commit in every downstream consumer. Everything the tool
cannot recover from git — which tier is whose parent, which tiers a stitch
linearizes, which base a consumer is poured onto, and the judgement calls in
`[policy]` — is here; everything else is read live, every invocation.

The one declared geometry in the whole file is `[[tier]] parent`. That is the only
thing the manifest can state *wrongly* about the graph, which is why
`TIER_PARENT_EDGE_WRONG` exists.

---

## Quick reference — every key

| Section | Key | Type | Req? | Default | Meaning |
|---|---|---|---|---|---|
| *(top level)* | `schema` | integer | **required** | — | Schema version. Parsed, currently not validated against any value. |
| *(top level)* | `build_artifacts` | array of glob strings | optional | `[]` | Paths the build generates. Silent in the dirty-tree guard, tracked **and** untracked. |
| `[repo]` | `superproject` | string | **required** | — | Superproject root. Parsed but **never read** — the CLI derives the root from the manifest path. |
| `[repo]` | `wine` | string | **required** | — | The wine submodule, relative to the superproject root. Used as the gitlink path. |
| `[repo.home_base]` | `commit` | string | **required** | — | The commit the tiers are authored on; the exclusive start of every replayed range. |
| `[repo.home_base]` | `describes` | string | **required** | — | `git describe` of that commit, printed in remedies so a bump is legible. |
| `[repo.home_base]` | `derivation` | string | **required** | — | How the commit was obtained. Documentation for the reviewer; never read by the tool. |
| `[remotes]` | *(any name)* | string | optional | `{}` | Remote name → URL. Parsed but **never read**; documentation only. |
| `[[tier]]` | `name` | string | **required** | — | The branch name. Also the subject prefix (`<name>:`) checks scan for. |
| `[[tier]]` | `parent` | string | **required** | — | The declared parent edge, or the literal `$home_base`. The only declared geometry. |
| `[[tier]]` | `remote` | string | optional | `None` | Remote name for the tracking ref `<remote>/<name>`. Omit and no drift check runs. |
| `[[stitch]]` | `name` | string | **required** | — | The stitch branch name. |
| `[[stitch]]` | `base_tier` | string | **required** | — | The tier the stitch is built from, and the `git cherry` limit. |
| `[[stitch]]` | `order` | array of strings | **required** | — | The tiers cherry-picked onto `base_tier`, in order. Order is load-bearing. |
| `[[stitch]]` | `local_only` | bool | optional | `false` | Declares the stitch deliberately unpushed. |
| `[[consumer]]` | `name` | string | **required** | — | The generated branch name. |
| `[[consumer]]` | `base` | string | **required** | — | The wine base replayed onto. A **tag or a branch** — see Gotchas. |
| `[[consumer]]` | `soda_tip` | string | **required** | — | The tier or stitch replayed onto the base. |
| `[[consumer]]` | `pinned_by_superproject` | bool | optional | `false` | The superproject's gitlink pins this consumer. Enables the pin cluster. |
| `[[consumer]]` | `exclude` | array of strings | optional | `[]` | Tiers deliberately kept OUT of this consumer's replay. |
| `[policy]` | *(section)* | table | optional | `Policy::default()` | **Presence is the switch.** Absent → the compiled-in defaults, wholesale. |
| `[policy]` | `tag_aliases` | array of strings | optional | `[]` *(when the section is present)* | Base-free tag names that are ours. Undeclared → the tag parses `Foreign` and vanishes. |
| `[policy]` | `ignore_branches` | array of strings | optional | `[]` *(when present)* | Local branches that are documented artifacts, not debris. |
| `[policy]` | `family_skew` | `"blocker"` \| `"drift"` \| `"off"` | optional | `"drift"` | Severity for `TAG_FAMILY_SKEW`. An unrecognised value is a parse **error**. |
| `[policy]` | `rr_cache` | string | optional | `"tools/rr-cache"` | Where the tracked rerere cache lives, relative to the superproject. |
| `[[policy.tag_equivalence]]` | `tags` | array of strings | **required** | — | Tags intended to name one commit. Suppresses `TAG_COLLISION` for that exact set. |
| `[[policy.tag_equivalence]]` | `reason` | string | **required** | — | Why. Mandatory: an unexplained suppression is indistinguishable from a mute. |
| `[[binary_resolution]]` | `path` | string | **required** | — | The path rerere structurally cannot learn. |
| `[[binary_resolution]]` | `strategy` | string | **required** | — | `"ours"` \| `"base"` \| `"delete"`. A free string today — **not validated, not yet consumed**. |
| `[[binary_resolution]]` | `reason` | string | **required** | — | Why this resolution is correct. Never read by the tool; read by you. |
| `[[binary_resolution]]` | `consumers` | array of strings | optional | `[]` | Narrows the rule. **Empty means universal**, and only universal rules shadow rr-cache entries. |

Thirty rows, and the ones that surprise people are the last four: `strategy` and
`reason` are stored and never consulted, and an *empty* `consumers` is the
stronger statement, not the weaker one.

---

## `schema` and `build_artifacts` — top level

```toml
schema = 1

build_artifacts = [
    ".build-env-stamp",
    "configure",
    "dlls/msxml3/xslpattern.tab.c",
]
```

Both **must precede the first table header** — see Gotchas.

`build_artifacts` globs support `*` (does not cross `/`), `**` (crosses `/`) and
`?`. Matching is in-process (`checks::glob_match`), not `git check-ignore`, which
answers a different question — `.gitignore` membership, not our allowlist.

The design constraint is that this project's build tree is *always warm*. A
dirty-tree guard that fires on generated files gets disabled within a day, and then
the guard protecting real uncommitted work is gone too. So a declared artifact is
silent whatever its status; an *undeclared* tracked modification is still an
unconditional Blocker.

**Drives:** `SUBMODULE_DIRTY` (suppresses), `SUBMODULE_UNTRACKED` (suppresses).

---

## `[repo]`

```toml
[repo]
superproject = "."
wine = "wine"
```

`wine` is the submodule path relative to the superproject root, and it is the key
the entire pin cluster hangs off: it is the gitlink path (`HEAD:wine`), the
`ls-files -s` target, and the `.gitmodules` key (`submodule.wine.branch`). Get it
wrong and every pin finding disappears rather than firing.

`superproject` is required by the schema and read by nobody. The root comes from
the manifest path's grandparent.

**Drives:** everything in `checks/pin.rs` — `PIN_UNTAGGED`, `PIN_NOT_ON_REMOTE`,
`PIN_STAGED_UNCOMMITTED`, `PIN_ABSENT`, `PIN_UNVERIFIABLE`,
`GITLINK_NOT_A_CONSUMER`, `GITMODULES_BRANCH_MISSING`,
`GITMODULES_BRANCH_UNRESOLVABLE`, `GITMODULES_BRANCH_MISMATCH`,
`WINE_HEAD_OFF_PIN` — plus `SUBMODULE_DIRTY`, `SUBMODULE_UNTRACKED`,
`WINE_HEAD_DETACHED`, `REBASE_IN_FLIGHT`, `RR_CACHE_UNSEEDED`.

---

## `[repo.home_base]`

```toml
[repo.home_base]
commit = "1bb0da91a5e"
describes = "wine-11.12-79-g1bb0da91a5e"
derivation = "6260d89930c^"
```

The one genuinely unrecoverable constant. It is the commit the soda tiers are
authored on and the exclusive start of every replayed range.

It is emphatically **not** `git merge-base <wine-base> <soda-tip>`. For a
cross-base replay that is the common wine ancestor down around 11.0, and using it
replays thousands of upstream commits onto Valve's tree — BRANCHING.md's Trap 1.

`describes` and `derivation` exist so a reviewer reading a diff can tell a
deliberate bump from a typo. `describes` reaches the output: a tier whose parent is
`$home_base` gets prose reading `$home_base (wine-11.12-79-g1bb0da91a5e = 1bb0da91a5e)`
while the *command* in the same remedy gets the resolved commit. `derivation` is
never read.

**Drives:** the `limit` in every `own_limit()` range, so indirectly
`CONSUMER_EXCLUDE_VIOLATION`, `CONSUMER_MISSING_TIER`, `TAG_ON_STALE_TARGET`,
`TIER_NOT_LINEAR`, `TIER_CHILD_STALE`.

---

## `[remotes]`

```toml
[remotes]
codeberg = "https://codeberg.org/SpritzWine/wine.git"
winehq = "https://gitlab.winehq.org/wine/wine.git"
valve-wine = "/media/Data/valve-wine-bare.git"
```

Free-form `name = url`. **No check reads this table.** It documents what the names
used in `[[tier]] remote` and in a `[[consumer]] base` refer to; the tool takes
those names straight from where they are used and hands them to git. Adding a
remote here does not make it exist, and using a `remote` name absent from here is
not an error.

**Drives:** nothing.

---

## `[[tier]]`

```toml
[[tier]]
name = "soda-core"
parent = "soda-build"
remote = "codeberg"

[[tier]]
name = "soda-build"
parent = "$home_base"
remote = "codeberg"
```

Tiers are hand-authored, never replayed — which makes this the one part of the
model where **ancestry is the correct primitive**. `TIER_CHILD_STALE` really does
ask `merge-base --is-ancestor`, and it is not an instance of the replay bug class
that governs stitches and consumers.

`parent` accepts a tier name or the literal placeholder `$home_base`.

`remote` is the *remote name only*; the tracking ref is built as
`<remote>/<name>`. Omit it and the tier simply gets no drift comparison — no
finding, no complaint.

`name` doubles as a commit-subject convention: `TIER_PARENT_EDGE_WRONG` and both
consumer tier checks scan for the prefix `<name>:`. That convention is not
enforced anywhere, but two checks lose their second signal without it.

**Drives:** `TIER_MISSING`, `TIER_CHILD_STALE`, `TIER_NOT_LINEAR`,
`TIER_PARENT_EDGE_WRONG`, `TIER_UNPUSHED`, `TIER_DIVERGED`, `TIER_BEHIND_REMOTE`,
`TIER_NO_REMOTE`, plus membership in `UNMODELLED_BRANCH`.

---

## `[[stitch]]`

```toml
[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll", "soda-fsync", "soda-branding"]
local_only = true
```

A stitch is a **linearization** of a tier subset, built by cherry-pick. The tiers'
own commits are never ancestors of it, so staleness is asked by patch-id
(`git cherry`), never by ancestry.

`base_tier` is both the branch the stitch is built from and the `limit` argument to
`git cherry`. That limit has a precondition — it must be an ancestor of the stitch —
and the check *establishes* it rather than assuming it: if the stitch no longer
descends from `base_tier`, you get `STITCH_REBASED` and staleness is explicitly
**not** checked, because a truncated `git cherry` under-reports at exit 0.

`order` is load-bearing. `soda-ntdll` and `soda-fsync` both live in
`dlls/ntdll/unix/`, so the sequence decides which conflict you resolve and
therefore which rr-cache entries match.

`local_only = true` declares the stitch deliberately unpushed — BRANCHING.md's
position that `soda-full` is a derived artifact and pushing it would create a fifth
thing people mistake for a source of truth. It produces a Hygiene row saying so,
rather than silence, so the missing drift detection stays visible.

**Drives:** `STITCH_MISSING`, `STITCH_REBASED`, `STITCH_STALE`,
`STITCH_LOCAL_ONLY`. Also expands `soda_tip` for `CONSUMER_MISSING_TIER`, and
gates which tiers `TAG_ON_STALE_TARGET` measures (a tier in no stitch is skipped).

---

## `[[consumer]]`

```toml
[[consumer]]
name = "spritzwine"
base = "wine-11.13"
soda_tip = "soda-full"
pinned_by_superproject = true

[[consumer]]
name = "spritzvalve"
base = "valve-wine/proton_11.0"
soda_tip = "soda-branding"
pinned_by_superproject = false
exclude = ["soda-fsync", "soda-glorious"]
```

A consumer is GENERATED — `rebase --onto <base> <home_base> <name>` — force-pushed,
never hand-edited.

`base` is handed straight to `rebase --onto` as a literal string, which is why an
unresolvable one is a Blocker rather than a warning: regen fails *partway*, after
`checkout -B` has already repointed the branch. It may be a tag or a branch; see
Gotchas.

`soda_tip` names a tier or a stitch. A stitch expands to `base_tier` plus `order`;
a tier stands for itself; anything unrecognised expands to **nothing**, so a typo
produces silence rather than invented tiers.

`pinned_by_superproject` switches on `reconcile`'s pin check for this consumer, and
suppresses `CONSUMER_AHEAD_OF_TAG` (because `pin.rs` already blocks on exactly that
fact, and two rows for one fact reads as two problems).

`exclude` lists tiers deliberately kept out. This is per-consumer **by design** —
see Gotchas.

**Drives:** `CONSUMER_BASE_UNRESOLVABLE`, `CONSUMER_NOT_ON_BASE`,
`CONSUMER_MISSING_TIER`, `CONSUMER_EXCLUDE_VIOLATION`, `CONSUMER_CURRENCY_UNKNOWN`,
`CONSUMER_AHEAD_OF_TAG`, `PINNED_CONSUMER_UNTAGGED`, `PIN_ABSENT`,
`PIN_UNVERIFIABLE`, `GITLINK_NOT_A_CONSUMER`, plus membership in
`UNMODELLED_BRANCH`.

---

## `[policy]`

```toml
[policy]
tag_aliases = ["soda-all", "spritzwine-archive"]
ignore_branches = ["spritzvalve-full"]
rr_cache = "tools/rr-cache"
family_skew = "drift"
```

These were compiled-in Rust defaults, which contradicted the manifest's own
governing rule by deciding policy in code where no reviewer sees it. They are
decisions, not observations, so they belong where a diff records who changed them.

**`tag_aliases`** — base-free tag names that are ours. The tag grammar is
`<component>-<base>-<version>`, and anything failing to parse is dropped as
`Foreign` *before any comparison runs*. So a missing alias does not raise a wrong
finding — it makes the tag **invisible**. A silent hole is the worse failure, and
it is the reason this is a declared list and not a pattern: guessing at a base-free
shape would swallow arbitrary upstream names out of the repo's ~6600 tags.

**`ignore_branches`** — local branches that are documented artifacts rather than
debris. Applied at the merge point in `reconcile_workspace`, filtering
`UNMODELLED_BRANCH` by subject. `spritzvalve-full` is BRANCHING.md's standing
example of a consumer you deliberately do not see.

**`rr_cache`** — where the tracked cache lives, relative to the superproject. It is
committed, and `regenerate.sh` seeds `wine/.git/rr-cache` from it; the live copy is
one developer's scratch state and is never the authority.

**`family_skew`** — `"blocker"`, `"drift"` or `"off"`. Parsed by serde, so an
unrecognised value is a manifest **parse error naming the key**, not a silent
fallback. `"off"` skips the check entirely; `"blocker"` and `"drift"` set the
severity.

**Drives:** `tag_aliases` → every tag check's input set (`TAG_COLLISION`,
`TAG_ON_STALE_TARGET`, `TAG_FAMILY_SKEW`, `CONSUMER_AHEAD_OF_TAG`).
`ignore_branches` → `UNMODELLED_BRANCH`. `rr_cache` → `RR_NO_POSTIMAGE`,
`RR_ENTRY_SHADOWED_BY_RULE`, `RR_STRAY_THISIMAGE`, `RR_UNCOMMITTED`,
`RR_CACHE_UNSEEDED`. `family_skew` → `TAG_FAMILY_SKEW`.

### Fallback semantics — section presence is the switch

This is subtle and worth getting exactly right.

**No `[policy]` section at all** → `Policy::default()` **wholesale**. That is the
frozen pre-refactor behaviour, byte-identical: aliases `["soda-all",
"spritzwine-archive"]`, ignore list `["spritzvalve-full"]`, `family_skew = drift`,
`rr_cache = "tools/rr-cache"`, and the two documented tag equivalences. Moving
policy into the manifest could not change a single existing finding for a manifest
written before the section existed.

**A `[policy]` section that IS present is authoritative for its lists.** An omitted
list means **empty**, never "and also whatever the binary believes". Anything else
makes "we recognise no tag aliases" inexpressible, and re-creates in a subtler form
the exact problem the section exists to remove.

**The two scalars keep their defaults when omitted** — `family_skew = "drift"`,
`rr_cache = "tools/rr-cache"` — because neither has a meaningful empty value.

So `[policy]` with nothing but `rr_cache` in it yields: no aliases, no ignored
branches, no equivalences, `drift`, and that `rr_cache`. That is deliberate, and
`tests/policy_manifest.rs` pins all three halves of the contract.

**One exception, and it is not in `[policy]` at all:** `consumer_excludes` is
always derived from the `[[consumer]] exclude` keys, whether or not `[policy]`
exists. It has no global home to be defaulted from — an exclusion now requires a
consumer that declares it. A consumer declaring nothing contributes no entry at
all, rather than an empty list that later reads as "checked".

---

## `[[policy.tag_equivalence]]`

```toml
[[policy.tag_equivalence]]
tags = ["spritzvalve-11.0-1", "spritzvalve-11.0-3"]
reason = """
BRANCHING.md, verbatim: "the family rolled, this glass did not need re-pouring." \
Regeneration and tagging are separate decisions. -11.0-1 is kept because things \
pinned it.\
"""
```

Tags *intended* to name one commit. `reason` is mandatory for the same reason
`[[binary_resolution]] reason` is: an equivalence suppresses a finding, and a
suppression with no recorded justification is indistinguishable from someone having
muted an inconvenient result.

Matching is **subset containment**: a collision is suppressed when *every* colliding
tag name appears in *one* declared set. A set that covers two of three colliding
tags suppresses nothing.

**Drives:** `TAG_COLLISION` (suppresses).

---

## `[[binary_resolution]]`

```toml
[[binary_resolution]]
path = "configure"
strategy = "delete"
reason = """
Upstream wine tracks the generated `configure`; Valve's proton_11.0 does not. \
The base wins: configure.ac merges cleanly and build.sh's autoreconf regenerates it.\
"""
consumers = ["spritzvalve"]

[[binary_resolution]]
path = "dlls/user32/resources/oic_winlogo.ico"
strategy = "ours"
reason = "The tier owns the glass icon unconditionally — blob 2f56ce03d19."
```

Conflicts rerere is **structurally** incapable of learning. rerere records
line-level text resolutions; it cannot record a modify/delete and it cannot record a
binary blob. Both recur on every regeneration and were being resolved by hand every
single time.

`strategy` documents the intent — `"ours"` (the tier's blob wins), `"base"` (the
wine base wins), `"delete"` (the path stays deleted). It is a free string: serde
does not validate it, and **no check reads it yet**. The verb that would apply it
(`regen`) is not implemented.

`consumers` narrows the rule. The scoping is load-bearing in the one place the
manifest is consumed today: **only unscoped rules shadow an rr-cache entry.** A rule
scoped to `spritzvalve` decides `configure` for that pour only — Valve's base does
not track it, so that replay is a modify/delete producing no rerere preimage at all.
Upstream wine *does* track it, so a content preimage of `configure` can only have
come from a spritzwine pour, where no rule applies and rerere both can and must be
consulted. Ignoring the scope declared two live entries dead and offered to prune
them: a confident false boolean whose remedy deletes the resolution it misdeclared.

**Drives:** `RR_ENTRY_SHADOWED_BY_RULE` (unscoped rules only, via a content
heuristic on the preimage — the finding is hedged because rerere never stores the
path).

---

## Gotchas

Each of these cost real debugging time. Symptom first.

**TOML scoping — top-level keys must precede the first table header.**
*Symptom:* `invalid type: sequence, expected a string`. Put `schema` or
`build_artifacts` after `[repo]` and TOML makes them members of `[repo]`, so
`repo.superproject` goes missing and `repo.build_artifacts` appears as a stray
array. The error names the wrong thing entirely. Both keys go at the very top of
the file, above every `[table]`.

**A consumer `base` may be a TAG or a BRANCH.**
*Symptom:* `CONSUMER_BASE_UNRESOLVABLE`, and a `git fetch` that never fixes it.
`wine-11.13` is a **tag** — upstream wine releases are tags, and
`remote.winehq.fetch` is `+refs/heads/*:refs/remotes/winehq/*`, so no amount of
fetching ever creates `refs/remotes/winehq/wine-11.13`. Writing
`winehq/wine-11.13` does not resolve. `valve-wine/proton_11.0`, by contrast, really
is a remote-tracking **branch** and is correct as written. The finding's remedy says
so, because a remedy you can run forever without progress is worse than none.

**Annotated tags need `^{commit}`.**
*Symptom:* a comparison that silently never matches. `git rev-parse <tag>` on an
annotated tag returns the **tag object**, not the commit — `spritzwine-11.13-3`
derefs to `c648e824600` (the tag) and `8bf9f2c8e83` (the commit). Ours are
annotated, Valve's are not. `GitQ::rev_parse` appends `^{commit}` for you and
`tags_with_targets` prefers `%(*objectname)` over `%(objectname)`; any new tag code
must do the same or it compares tag-object oids against commit oids forever.

**`$home_base` is a placeholder, not a ref.**
It is valid **only** as a `[[tier]] parent`, and it never reaches a printed command.
*Symptom if that slips:* the remedy `git rebase --onto $home_base <old-$home_base-tip>
soda-build`, which a shell expands to `git rebase --onto "" "" soda-build` — run
against the one branch the docs say must never move. Commands get the resolved
commit; prose gets commit plus `describes`.

**`consumer.exclude` is per-consumer by design.**
*Symptom of the old form:* a `CONSUMER_EXCLUDE_VIOLATION` nobody asked for. Excludes
used to be a global list keyed by consumer name, so the test fixture's
coincidentally-named `spritzvalve` — named for the topology it mirrors, not for the
live repo's policy — silently inherited the live repo's exclusions and produced a
finding on a fixture that declared nothing. It happened to be the finding the test
wanted, which is exactly the hazard. On the consumer entry, an exclusion cannot
exist without a consumer to attach to and cannot attach to one the manifest does not
define.

**`[[stitch]] order` is load-bearing.**
`soda-ntdll` and `soda-fsync` both touch `dlls/ntdll/unix/` (the unwinder/threadpool
vs `sync.c`). Reordering them changes which conflict you resolve, which changes the
rr-cache entry ids, which means every stored resolution stops matching. Treat the
order as part of the definition, not as a list.

**`build_artifacts` applies to tracked AND untracked paths.**
*Symptom:* a `SUBMODULE_DIRTY` Blocker on `configure` after any build.
`configure` is **tracked** in upstream wine, so the moment `autoreconf` runs it
becomes ` M configure` and takes the tracked branch. An allowlist restricted to
untracked files would be inert exactly where the manifest says it is needed. This
does not weaken the guard — a declared build product is silent whatever its status,
and an undeclared tracked modification is still an unconditional Blocker.

---

## Worked example

The live manifest, trimmed of prose. Read the real
`/media/SSD/Projekte/SpritzWine/tools/sodactl.toml` for the full commentary.

```toml
schema = 1

# Generated files the build leaves behind. Must not trip the dirty-tree guard, or
# every operation refuses on a warm tree. Tracked AND untracked.
# (Top-level keys precede the first table header — TOML scoping.)
build_artifacts = [
    ".build-env-stamp",
    "configure",                          # tracked upstream; autoreconf rewrites it
    "dlls/actxprxy/actxprxy_mshtml.h",
    "dlls/msxml3/xslpattern.tab.c",
    "dlls/msxml3/xslpattern.tab.h",
    "dlls/msxml3/xslpattern.yy.c",
]

[repo]
superproject = "."
wine = "wine"

# The one unrecoverable constant. NOT `git merge-base <wine-base> <soda-tip>` —
# that is the common ancestor down around 11.0 and would replay thousands of
# upstream commits onto Valve's tree.
[repo.home_base]
commit = "1bb0da91a5e"
describes = "wine-11.12-79-g1bb0da91a5e"
derivation = "6260d89930c^"

# Documentation. No check reads this table.
[remotes]
codeberg = "https://codeberg.org/SpritzWine/wine.git"
winehq = "https://gitlab.winehq.org/wine/wine.git"
valve-wine = "/media/Data/valve-wine-bare.git"

# ---- tiers: authored, hand-written. `parent` is the ONLY declared geometry. ----

[[tier]]
name = "soda-build"
parent = "$home_base"       # the placeholder; valid nowhere else
remote = "codeberg"

[[tier]]
name = "soda-core"
parent = "soda-build"
remote = "codeberg"

[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-fsync"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-branding"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-demo"
parent = "soda-build"
remote = "codeberg"

# ---- stitches: a linearization of a tier subset. Order is load-bearing —
# soda-ntdll and soda-fsync both live in dlls/ntdll/unix/. ----

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll", "soda-fsync", "soda-branding"]
local_only = true           # derived scratch branch; deliberately unpushed

# ---- consumers: GENERATED, force-pushed, never hand-edited. ----

[[consumer]]
name = "spritzwine"
base = "wine-11.13"                     # a TAG. `winehq/wine-11.13` does not resolve.
soda_tip = "soda-full"
pinned_by_superproject = true

[[consumer]]
name = "spritzvalve"
base = "valve-wine/proton_11.0"         # a remote-tracking BRANCH.
soda_tip = "soda-branding"
pinned_by_superproject = false
# BRANCHING.md rule 1: Valve's base already has fsync, so pouring soda-fsync in is
# "a conflict looking for a place to happen". Declared HERE, on the consumer.
exclude = ["soda-fsync", "soda-glorious"]

# ---- policy: the judgement calls. ----
# Deleting this whole section is safe and well-defined — the tool falls back to the
# values it used to compile in. Deleting a single KEY is not the same thing: within
# a present [policy] an omitted list means EMPTY.
[policy]

# Undeclared aliases parse Foreign and vanish from every comparison — a silent
# hole, not a loud error. That is why this is a list and not a regex.
tag_aliases = ["soda-all", "spritzwine-archive"]

# Documented artifacts, not debris. BRANCHING.md's "consumers you don't see".
ignore_branches = ["spritzvalve-full"]

rr_cache = "tools/rr-cache"

# "blocker" | "drift" | "off". THIS VALUE ENCODES AN ASSUMPTION, NOT AN ANSWER.
family_skew = "drift"

# Each entry suppresses a TAG_COLLISION, so each carries a reason.
[[policy.tag_equivalence]]
tags = ["spritzvalve-11.0-1", "spritzvalve-11.0-3"]
reason = "The family rolled; this glass did not need re-pouring. -11.0-1 is kept because things pinned it."

[[policy.tag_equivalence]]
tags = ["soda-all", "soda-11.12-1"]
reason = "Two names for one frozen point by construction: soda-all is the base-free union reference."

# ---- conflicts rerere is STRUCTURALLY incapable of learning. ----

[[binary_resolution]]
path = "configure"
strategy = "delete"
reason = "Valve's proton_11.0 does not track the generated configure; the base wins and autoreconf regenerates it."
consumers = ["spritzvalve"]     # SCOPED — so it does not shadow spritzwine's rr entries

[[binary_resolution]]
path = "dlls/user32/resources/oic_winlogo.ico"
strategy = "ours"
reason = "The tier owns the glass icon unconditionally; blob 2f56ce03d19 is byte-identical in every shipped consumer tag."
                                # UNSCOPED — universal, and therefore shadowing
```

---

## Pending — decided, NOT implemented

Everything above describes the tool as it behaves **today**. The following are
decisions from `docs/sodactl-plan.md` ("Decisions — 2026-07-19") that no code
implements yet. Do not write manifests against them.

**PENDING — `family_skew` becomes `"off"`.** Multiple tags on one revision is
normal, and a component legitimately sits at N-1 between cuts. The value ships as
`"drift"` purely because that is what the tool did before the key existed, and a
refactor is the wrong place to decide it. `"off"` parses and works today; the
manifest has not been changed to it. Note the comparison is structurally incomplete
either way — `prater-*` lives in a third repository this tool cannot see.

**PENDING — `TAG_COLLISION` stops requiring a declared equivalence.** Silence
becomes the default: requiring an entry per release is a treadmill, and a treadmill
gets muted. `[[policy.tag_equivalence]]` stays supported for the cases worth
*narrating*, but it will no longer be the thing standing between you and a finding.
Today it still is.

**PENDING — `[[binary_resolution]]` gains `strategy = "regenerate"` and an optional
`conflict_id`.** `"regenerate"` covers the two 25k-line `configure` entries
outright: `configure` is generated, so hand-merging it is never correct on any base,
and `build.sh`'s `autoreconf` already does the right thing — the rule just makes it
declared instead of incidental. `conflict_id` may *narrow* a rule to one exact
conflict but must never be the only key, because an rr-cache id is a hash of the
conflict text and rots the moment the surrounding code moves. Neither field parses
today; `strategy` is still a free string that nothing reads.

**PENDING — five orphaned rr-cache entries to be pruned.** The recorded conflicts
sit between `WINE_RUST_PE_LDFLAGS_*` and an intermediate generalization, while every
current tier uses `WINE_RUST_PE_DIR_*`, a third variant postdating both. The text on
either side survives on no tier, no stitch and no consumer — they can never match
again. They are orphans of a superseded development state. `sodactl rr gc` is not
implemented; pruning is by hand.
