# The SpritzWine branch model

*This document lives in the **superproject**, next to the `tools/regenerate.sh` it describes.
It used to live in the wine repo — on the `spritzwine` branch, which is a **generated
consumer**, force-pushed on every regeneration. It would have deleted itself the first time
anyone followed its own instructions.*

*Corrected 2026-07-19, twice. First: the upstream glass has been poured — everything this
document used to call "not yet" is now a tag you can check out; re-read "The consumers" and
"Three bases". Second: a tier is no longer one squashed commit, and regenerating a consumer no
longer implies tagging it; re-read "The tiers" and "Tags". If you are working from an older copy,
those four sections are the ones that lie to you.*

If you have just run `git branch -a` on the wine repo, you are looking at `soda-build`,
`soda-core`, `soda-glorious`, `soda-ntdll`, `soda-branding`, `soda-fsync`, `soda-demo`,
`soda-full`, `spritzvalve`, `spritzwine`, plus upstream's `master`/`stable` and Valve's
`proton_*`. That looks like a mess. It is not; there are exactly **two kinds of branch** here,
and once you know which is which, the rest follows.

**Authored branches — `soda-*`.** Hand-written feature tiers. These are the source of
truth. You edit these.

**Generated branches — `spritzwine` and friends.** Produced mechanically by *replaying*
the soda tiers onto a wine base. Never hand-edited, force-pushed on every regeneration.
Same deal as Valve's own `proton_11.0` branch: a rebased result, not a place you commit to.

The name is the whole idea. **Soda is the syrup; a consumer is soda plus a base.** The
tiers carry no wine of their own — they are the flavour you pour onto whichever wine tree
you want to sip.

---

## The picture

```
                    wine-11.0                     wine-11.12-79-g1bb0da91a5e
                        │                         (the "soda home base";
                        │                          git rev-parse 6260d89930c^)
       Valve's patchset │                                     │
                        ▼                                     │   AUTHORED — you edit here
              valve-wine/proton_11.0                          ▼
                  (2f70bfd4d0f)                       ┌── soda-build ───┬── soda-demo
                        │                             │       │         │
                        │                             │   soda-core ────┼── soda-glorious
                        │                             │       │         │
                        │                             │       │         ├── soda-ntdll
                        │                             │       │         │
                        │                             │       │         ├── soda-fsync
                        │                             │       │         │
                        │                             │       │         └── soda-branding
                        │                             │
                        │        tools/regenerate.sh  │
                        └───────────────◄─────────────┘
                                        │
                                 replay (rebase --onto, git rerere)
                                        │
       GENERATED — never edit           ▼
       ┌──────────────────────────────────────────────────────────────────┐
       │ spritzwine    = wine-11.13 + the FULL mix                        │  THE ENDEAVOUR
       │                 (build + core + glorious + ntdll + fsync +       │  poured, pinned,
       │                  branding)                                       │  what we build
       │                                                                  │
       │ spritzvalve   = proton_11.0 + build + core + branding            │  the necessity
       │                                                                  │  the boot gate
       └──────────────────────────────────────────────────────────────────┘

       One is the point of the project. The other is how we prove the first one is safe.
```

Answers up front:

* **Which branch do I edit?** A `soda-*` one. Always.
* **Which do I never edit?** Any consumer — they get thrown away and rebuilt. Both of them:
  `spritzwine` and `spritzvalve`.
* **Which one is the project actually about?** `spritzwine` — the upstream one, with the full
  mix. It is poured, it is tagged `spritzwine-11.13-3`, and the superproject's `.gitmodules`
  pins it: that is the tree you build today. `spritzvalve` is the Valve-based glass that proves
  real games still boot, because games only boot on Valve's tree and a game booting is the gate.
  Both matter; only one is the point.
* **How do I make a new consumer?** `tools/regenerate.sh <out-branch> <wine-base> <soda-tip>`
  in the SpritzWine superproject. Tag it when you cut a release or pin it — not just because you
  regenerated it. See "Tags".
* **Why three bases (11.0, 11.12 *and* 11.13)?** See "Three bases" below. Short version: the
  tiers are authored on wine 11.12 and never move; Valve is frozen on wine 11.0; upstream has
  moved on to 11.13. Nobody is going to fix that asymmetry — the replay exists precisely so it
  doesn't have to be fixed.

---

## The tiers

A tier is a **dev branch that accumulates one concise commit per feature**, stacked on its
parent tier. Not a topic branch you merge and not a single squashed blob either — a linear chain
of commits, each of which says what it did. Check it yourself:

```
$ BASE=$(git rev-parse 6260d89930c^)     # wine-11.12-79-g1bb0da91a5e
$ git log --oneline $BASE..soda-core
aba05bcd46e soda-core: route SHBrowseForFolder to the host folder portal
79e58cb8a70 soda-core: HID/HidP + comdlg32 portal + avrt + win32u (lands clean on Valve)
c69c111182f soda-build: Rust PE-staticlib build integration (configure/makedep/winegcc)
```

Read that bottom-up and it is `soda-build`'s one commit, then `soda-core`'s two: the original
carve-out, then a feature landed on top of it. Do not take the count as a promise — it climbs
every time a feature lands. The shape is what is promised: a straight line, no merges, and the
first commit after `$BASE` belongs to the parent tier.

It used to say *exactly one squashed commit*, and for a while that was literally true — that is
simply how the tiers were carved out of the old hand-authored fork, one squash per tier, in one
sitting. Keeping it true afterwards meant squashing every new feature into a commit written
months earlier, which costs you the per-feature history — what changed, when, and why — and buys
nothing, because the replay does not care. `rebase --onto` walks whatever commits are in the
range; three cost the same as one. What the replay *does* care about is that the chain is linear
and that each tier's **parent edge** is right, so a feature lands on the tier that owns the code
it touches. Get those two right and the commit count is your business.

| Tier | Parent | Contents |
|---|---|---|
| `soda-build` | fork base | Rust PE-staticlib build integration: `configure.ac`, the generated `configure`, `tools/makedep.c`, `tools/winegcc/winegcc.c`. **Everything else depends on it.** |
| `soda-core` | `soda-build` | HidP value engine, comdlg32 XDG-portal file chooser, avrt MMCSS, win32u DC-cache bound, hidparse, wineusb container ID. Lands clean on the Valve base. |
| `soda-glorious` | `soda-core` | DualSense/controller Rust: winebus.sys rumble, dinput, mmdevapi endpoint pairing, winexinput. |
| `soda-ntdll` | `soda-core` | ntdll Rust: threadpool, `RtlVirtualUnwind2`, SEH hardening, battery/LZNT1, plus the wow64 power thunk. |
| `soda-branding` | `soda-core` | winecfg strings and icons. Per-consumer. |
| `soda-fsync` | `soda-core` | Valve's futex inproc-sync backend, vendored onto upstream's inproc-sync framework (ntsync first, `WINEFSYNC=1` to engage). **Upstream bases only** — `proton_11.0` already has fsync. |
| `soda-demo` | `soda-build` | Rust `ping` + integrationtest harness. Nobody ships this. |

### Why tiers exist at all

A tier is justified by exactly one of two things:

1. **A distinct consumer set.** `soda-branding` is separate because a differently-branded
   consumer swaps it out. `soda-demo` is separate because no shipping consumer wants it.
2. **Isolating conflict-heavy code.** `soda-ntdll` is separate because ntdll is where a
   cross-base replay hurts most (see "Stitch notes"), and you do not want that landmine
   sitting under a change to comdlg32.

If a change needs neither, **it becomes a new commit on an existing tier** — the one that owns
the code it touches. Not a new tier, and not a squash into the tier's existing commits either:
write it up as its own feature commit and leave it standing. More tiers is not more structure;
it is more rebase surface.

`soda-fsync` is rule 1 in its purest form: Valve's base *already has* fsync, so pouring it into
`spritzvalve` would be a conflict looking for a place to happen — while an upstream base has
ntsync but no fsync, and wants it. A tier that exactly one consumer takes.

---

## The consumers

Two glasses, and it is easy to get their order backwards — because the one that boots the games
is not the one that is the point.

### `spritzwine` — the endeavour (poured)

Soda on **upstream WineHQ wine**. No Valve patchset, no Proton, no Prater.

This is what the project is *for*: Rust in Wine, tracking what Wine is becoming rather than
what Valve froze. It carries the **full mix** — `soda-build` + `soda-core` + `soda-glorious`
+ `soda-ntdll` + `soda-fsync` + `soda-branding` — and it is the home of everything that has nothing to do
with games (`soda-ntdll`'s own commit subject says "(spritzwine only)"; battery reporting
exists because Windows programs ask how much charge is left and Wine had no good answer — no
frametime was harmed or improved).

**It exists.** Tagged **`spritzwine-11.13-3`** (commit `8bf9f2c8e83`), the tiers replayed onto
`winehq/wine-11.13` — six commits on top of the release, which at the time was one per tier.
Expect that number to climb as features land; the replay carries whatever the tiers hold. It
built and it booted. The superproject pins it: `.gitmodules` says
`branch = spritzwine-11.13-3` and the `wine` gitlink is that commit. It is a consumer like any
other, so it is force-pushed and you never edit it.

The old hand-authored fork is not lost — it is preserved as the annotated tag
**`spritzwine-archive`** (`746cf095d74`, whose parent is the `4815a57385c` this document used to
point you at). That is the pre-tier-model tree the soda tiers were carved *out of*, kept
reachable so the carve can be audited. Do not build from it, do not branch off it. It is a
museum piece with a ref.

### `spritzvalve` — the necessity (the boot gate)

`proton_11.0` + `soda-build` + `soda-core` + `soda-branding`. Tagged **`spritzvalve-11.0-3`**
(commit `8c00a2018` — the same commit `spritzvalve-11.0-1` points at; the family release rolled,
the consumer did not need re-pouring). This is what SpritzWine-Prater is built from. Boots
Avatar: Frontiers of Pandora, Risk of Rain 2, Watch Dogs: Legion.

The superproject used to pin *this* tag, because it was the only poured consumer. It no longer
does — see above. Prater still does, and that is the correct split: Prater is the Proton
distribution, so it wants the Proton-based glass.

It carries the **thin mix** on purpose, and it exists for one reason: the project's real gate is
*does the game boot?*, and games only boot on Valve's tree (see "Why a Valve base at all",
below). So this is the glass we can actually hand to someone, and the one that proves the Rust
does not break real software.

That makes it the vehicle, not the destination. **spritzvalve is how we prove SpritzWine is
safe; SpritzWine is the thing being proven.** It got poured first, and for a while it was the
only thing pinned — but arriving first is not the same as being the point, and now that the
upstream glass is poured the ordering is finally visible in `.gitmodules` too.

(It was called `spritzvalve-v1` during the proof of concept — you will still see that name in
older superproject commits and in `regenerate.sh`'s usage comment. Same thing, renamed.)

### Consumers you don't see

Experimental mixes get stitched and vetted in local clones and are **deliberately not pushed**.
`spritzvalve-full` (spritzvalve plus `soda-glorious`) is the standing example, and your local
`spritzvalve` and `spritzwine` branch tips will routinely sit *ahead* of their tags for the same
reason — a pour that has not passed the gate yet. An untested consumer sitting on the remote is
an invitation for someone to pin it, and then you own it. A consumer appears on the remote when
it passes the boot gate, not when it compiles.

This is also why you pin **tags, not branches**. `git ls-remote --tags` is the honest list of
what exists; `git branch -a` in your own clone is a list of what you were in the middle of.

---

## Why a Valve base at all

The original fork rode vanilla wine. Vanilla wine does not carry Proton's patchset, and that
patchset is not cosmetic: **Avatar: Frontiers of Pandora crashed at ~13-14 seconds** on the
vanilla-11.12 fork — Ubisoft's anti-tamper layer, and the missing piece was Valve's
`WINE_SIMULATE_WRITECOPY`. Grep for it: it exists in `valve-wine/proton_11.0`, it does not
exist in wine 11.12.

Rebasing onto `proton_11.0` inherits Valve's *entire* patchset for free, and the same title
now stays alive past two minutes. That is the whole justification for the generated-consumer
machinery. Chasing individual Proton hacks one at a time was the alternative, and it is a
losing game.

---

## Three bases (11.0, 11.12 and 11.13) — the asymmetry is real

Three numbers, and they play three different roles. Confusing them is the classic mistake, so
be exact:

* **11.12 is the tier HOME base.** The soda tiers are authored on `wine-11.12-79-g1bb0da91a5e`,
  because that is where the fork was when the work was done. **This one never moves.** It is the
  exclusive start of every replayed range (see Trap 1 below). It is not anybody's shipping base;
  it is the fixed point the tiers are measured from.
* **11.0 is Valve's base.** Every Valve wine branch — `proton_11.0`, `experimental_11.0`,
  `bleeding-edge` — sits on wine 11.0. `git describe` puts `proton_11.0` at
  `wine-11.0-1190-g2f70bfd4d0f`. There is no newer wine base inside the Proton 11 cycle. Valve
  is not going to hand you one.
* **11.13 is where upstream is.** `spritzwine-11.13-3` is the tiers replayed onto
  `winehq/wine-11.13`. This one *does* move — that is the entire point of the upstream glass.
  Next release it will be 11.14, and the tiers will not have changed.

So `spritzvalve` is *11.12-authored tiers replayed backwards onto an 11.0 tree*, and
`spritzwine` is *the same tiers replayed forwards onto an 11.13 tree*. Same syrup, poured in
two directions. Neither is an accident to be cleaned up; it is the steady state, and it is why
the replay is a script with a tracked rerere cache rather than a one-off rebase somebody did
once. A tier authored once now rides two bases and follows upstream for free — that is the
whole return on the machinery.

The tag naming makes the roles visible: `-11.12-` on the tier set means *authored on*, `-11.0-`
and `-11.13-` on consumers mean *replayed onto*. See the tag table.

---

## Pouring the upstream glass

This used to be the open work. It is now the routine, and you re-run it every time upstream
tags a release.

`regenerate.sh` replays a **linear range** (`<home-base>..<soda-tip>`) onto a wine base. That is
why `spritzvalve` is easy: `soda-branding` *is* a straight line (build → core → branding), so
there is a single tip to point at.

The full mix is not a straight line. `soda-glorious`, `soda-ntdll`, `soda-branding` and
`soda-fsync` are **siblings** on `soda-core` — there is no single ref whose ancestry contains all
of them. So before the upstream consumer can be poured, the tiers it wants must be **stitched into
a chain**, and that chain is what `regenerate.sh` gets as its soda tip:

```sh
# 1. stitch the full mix into a linear tip (re-do whenever a tier moves)
git checkout -B soda-full soda-core
git cherry-pick soda-core..soda-glorious \
                soda-core..soda-ntdll   \
                soda-core..soda-fsync   \
                soda-core..soda-branding

# 2. pour it onto an upstream base
tools/regenerate.sh spritzwine winehq/wine-11.13 soda-full
```

`soda-full` is a **local scratch branch, deliberately not pushed** — it is a derived artifact,
regenerated from the tiers in seconds, and pushing it would create a fifth thing people could
mistake for a source of truth. The stitch *result* on the home base is what gets frozen and
pushed, as `soda-11.12-3`. If you want the stitched chain without re-cherry-picking, check out
that tag; if a tier has moved since, re-run step 1 and you will get a newer one.

Order matters where the tiers touch the same files: `soda-ntdll` and `soda-fsync` both live in
`dlls/ntdll/unix/` (the unwinder/threadpool vs. `sync.c`), so expect the stitch to want a
resolution there — which is precisely what the tracked `tools/rr-cache` exists to remember.

Do **not** put `soda-fsync` in a chain destined for `spritzvalve`: Valve's base already carries
fsync.

---

## Regeneration

`tools/regenerate.sh` lives in the **SpritzWine superproject**, not here:
`/media/SSD/Projekte/SpritzWine/tools/regenerate.sh`.

```
tools/regenerate.sh <output-branch> <wine-base-ref> <soda-tip-ref> [<soda-home-base>]

# the Valve-based consumer (thin mix, straight-line tip):
tools/regenerate.sh spritzvalve valve-wine/proton_11.0 soda-branding

# the upstream consumer (full mix, needs the soda-full stitch first):
tools/regenerate.sh spritzwine winehq/wine-11.13 soda-full
```

What it does, in order:

1. Turns on `rerere` (`rerere.enabled`, `rerere.autoupdate`) and seeds `.git/rr-cache` from
   the repo-tracked `tools/rr-cache` in the superproject. That cache is committed, so a
   fresh clone or a CI runner replays every conflict resolution that has already been made
   by hand. Newly learned resolutions are copied back out at the end — commit them.
2. `git checkout -B <output-branch> <soda-tip>` — the output branch is *pointed at* the soda
   tip.
3. `git rebase --onto <wine-base> <soda-home-base> <output-branch>`.

Two traps are baked into that third line, and a newcomer needs both:

**Trap 1 — the fork point is the soda HOME base, not a merge-base.** The exclusive start of
the replayed range is `git rev-parse 6260d89930c^` (the parent of the first fork commit,
`6260d89930c "feat(configure): Integrate Rust with Wine's build system"`). It is *not*
`git merge-base proton_11.0 soda-branding` — that is the common wine ancestor down around
wine 11.0, and using it would replay the entire 11.0→11.12 upstream range on top of Valve's
tree. Thousands of commits, guaranteed catastrophe. The script defaults to the right one; if
you ever pass the fourth argument, pass it deliberately.

**Trap 2 — rebase the OUTPUT branch, never the soda tier.** `git checkout -B` first, then
rebase *that*. If you rebase `soda-branding` itself, the tiers stop being authored on their
home base, every other consumer's replay silently changes, and the rr-cache entries stop
matching. The tiers never move. Ever.

---

## Tags, and why they are not optional

Scheme: **`<component>-<wine base>-<version>`**.

First, when *not* to tag. **Regenerating a consumer is not a release.** You will re-pour
`spritzwine` while chasing a build break, pour it again after fixing a tier, pour a throwaway
mix to see whether it compiles — none of that earns a tag. Tags are the honest public list of
what exists (see "Consumers you don't see"), and a list that grows every time somebody runs a
script is not honest, it is noise. **Tag when you cut a release**, and give the whole family the
same trailing number. Regeneration and tagging are two separate decisions; do not let the script
make the second one for you.

That rule has exactly one hard exception, and it is not negotiable — read on.

The trailing number is a **family release**: everything carrying `-3` was cut together and is
meant to be used together, whatever base it sits on.

| Tag | Points at | Means |
|---|---|---|
| `spritzwine-11.13-3` | `8bf9f2c8e83` | Generated consumer, replayed **onto** upstream wine 11.13 — the full mix. What the superproject pins. |
| `spritzvalve-11.0-3` | `8c00a2018` | Generated consumer, replayed **onto** Proton's wine 11.0. Same commit as `spritzvalve-11.0-1` — the family rolled, this glass did not need re-pouring. |
| `spritzvalve-11.0-1` | `8c00a2018` | Family release 1 of the same. Kept because things pinned it. |
| `soda-11.12-3` | `da673812a8b` | The tier set stitched into a linear chain, authored **on** wine 11.12 |
| `soda-11.12-1` | `40abbdc4f97` | Family release 1 of the tier set, same base |
| `soda-all` | `40abbdc4f97` | Alias of `soda-11.12-1` — the union reference, see below |
| `spritzwine-archive` | `746cf095d74` | The legacy hand-authored fork, pre-tier-model. Not a consumer. Never build it. |
| `spritzwine-11.0-0.2.0` | *(superproject)* | The superproject release |
| `prater-11.0-0.2.0` | *(distribution)* | The shipped build |

Note the two prepositions. `-11.0-` and `-11.13-` on a consumer mean "replayed onto that base";
`-11.12-` on the tier set means "authored on that base". Same field, different relationship, and
that is exactly the asymmetry from the previous section made visible in the name.

`soda-all` / `soda-11.12-1` is worth one extra sentence, because it is not a tier tip: it is a
frozen point whose tree is the **union** of all six tiers — the lossless-partition reference.
If the tiers are ever re-cut, `soda-build + core + glorious + ntdll + demo + branding` must still
reconstruct that exact tree. It is the regression test for the split itself, which is why it
carries a second, base-free name that will not scroll away as the family number climbs.

**The operational fact, and it is the most important one in this document:** consumers are
**force-pushed**. A superproject gitlink pinning a raw consumer SHA will break the moment the
branch is regenerated — the old commit falls out of the branch's history, the remote stops
advertising it, and `git submodule update` dies with

```
Fetched in submodule path 'wine', but it did not contain <sha>.
```

A tag is a ref. A ref keeps the commit reachable and fetchable forever, no matter where the
branch moves. **Tag every consumer you pin, and pin the tag.** That is the exception promised
above, and it is the one place tagging is mandatory rather than a release decision: the moment a
SHA is written into the superproject's `wine` gitlink, it needs a ref holding it alive, or the
next regeneration turns your pin into the error above. So — routine pours, no tag; a pour that
something pins, tag it before you pin it, even if you are not cutting a release that day.
`.gitmodules` in the
superproject sets `branch = spritzwine-11.13-3` for exactly this reason — and the fact that
that line used to read `spritzvalve-11.0-1`, and the pin survived the switch without a single
unfetchable SHA, is the policy paying for itself.

---

## Stitch notes

Things that were painful once and should not be re-learned.

**Where the base and a tier disagree, the base wins.** This bites hardest on the *backwards*
replay: the tiers were authored against wine 11.12, and `spritzvalve` lands them on Valve's
11.0. Where upstream moved on between those two, the tier's version of the file is *stale*, and
the correct resolution is Valve's:

* `dlls/ntdll/Makefile.in` — Valve's 11.0 still imports `$(TOMCRYPT_PE_LIBS)` and links
  `$(UNWIND_LIBS)`; upstream had switched to `$(SYMCRYPT_PE_LIBS)` + `$(ZLIB_PE_LIBS)` and
  dropped libunwind entirely by 11.12. Take Valve's.
* `virtual_unwind()` — in Valve's tree it takes an extra `need_backtrace()` argument
  (`signal_x86_64.c`, `signal_arm64.c`, `signal_arm64ec.c`); the macro does not exist in
  11.12 at all. Take Valve's signature.

These are the conflicts the tracked rr-cache exists to remember. If you resolve one, commit
the cache.

**Two conflicts rerere cannot remember — you will resolve these by hand every single time.**
rerere records *text-content* resolutions: a hunk it has seen before, resolved the same way
again. Neither of the following is a text hunk, so no amount of committing the cache will make
them stop. This is not the cache failing; it is a category it structurally cannot hold. Expect
them, resolve them, move on.

* **`configure` — modify/delete, on every Valve-base replay.** Upstream wine tracks the
  generated `configure`; `proton_11.0` does not track it at all. So `soda-build`, authored on an
  upstream tree, modifies a file the Valve base has deleted, and git stops with *"deleted in HEAD
  and modified in soda-build"*. **Keep it deleted** — the base wins, same rule as above.
  `configure.ac` merges cleanly and that is the real source; `build.sh` step 2 runs
  `[ -x configure ] || autoreconf -f` and regenerates it. Resolving this the other way commits a
  generated file into a tree whose build system does not want one.
* **`dlls/user32/resources/oic_winlogo.ico` — binary, in the `soda-branding` replay.** Both
  sides changed a binary blob, so there is nothing to merge and rerere has nothing to record.
  **Take the TIER's blob**, not the base's — the tier's *is* the SpritzWine glass icon, which is
  the entire content of `soda-branding`. Take the base's and you have silently un-branded the
  build. The correct blob is `2f56ce03d190c07283f19ae95bd99ccecb955336`; it is identical in
  `soda-branding` and in every consumer tag shipped so far, so you can check your work with
  `git ls-tree <ref> dlls/user32/resources/oic_winlogo.ico`.

**`soda-ntdll` deletes `dlls/ntdll/threadpool.c` outright** — all 2797 lines; Rust replaces
it. That is safe across the base swap for two independently-checked reasons: Valve never
patched `threadpool.c`, so nothing of theirs is being thrown away, and the `Tp*` export
surface in `ntdll.spec` is **identical** between 11.0 and 11.12 (35 exports on both). If
either of those ever stops being true, this deletion needs re-verifying before the next
`soda-ntdll` consumer ships.
