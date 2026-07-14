# The SpritzWine branch model

*This document lives in the **superproject**, next to the `tools/regenerate.sh` it describes.
It used to live in the wine repo — on the `spritzwine` branch, which is a **generated
consumer**, force-pushed on every regeneration. It would have deleted itself the first time
anyone followed its own instructions.*

If you have just run `git branch -a` on the wine repo, you are looking at `soda-build`,
`soda-core`, `soda-glorious`, `soda-ntdll`, `soda-branding`, `soda-demo`, `spritzvalve`,
`spritzwine`, plus upstream's `master`/`stable` and Valve's `proton_*`. That looks like a
mess. It is not; there are exactly **two kinds of branch** here, and once you know which is
which, the rest follows.

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
                        │                             │       │         └── soda-branding
                        │                             │
                        │        tools/regenerate.sh  │
                        └───────────────◄─────────────┘
                                        │
                                 replay (rebase --onto, git rerere)
                                        │
       GENERATED — never edit           ▼
       ┌──────────────────────────────────────────────────────────────────┐
       │ spritzwine    = upstream wine + the FULL mix                     │  THE ENDEAVOUR
       │                 (build + core + glorious + ntdll + branding)     │  not poured yet
       │                                                                  │
       │ spritzvalve   = proton_11.0  + build + core + branding           │  the necessity
       │                                                                  │  ships today
       └──────────────────────────────────────────────────────────────────┘

       One is the point of the project. The other is how we prove the first one is safe.
```

Answers up front:

* **Which branch do I edit?** A `soda-*` one. Always.
* **Which do I never edit?** Any consumer — they get thrown away and rebuilt. Today that
  means `spritzvalve`. (`spritzwine` joins the set once it's regenerated from the tiers;
  today it is still the legacy hand-authored fork, see below.)
* **Which one is the project actually about?** `spritzwine` — the upstream one, with the full
  mix. `spritzvalve` is what ships, because games only boot on Valve's tree, and a game booting
  is the gate. Shipping first is not the same as being the point.
* **How do I make a new consumer?** `tools/regenerate.sh <out-branch> <wine-base> <soda-tip>`
  in the SpritzWine superproject, then tag it.
* **Why two bases (11.0 *and* 11.12)?** See "Two bases" below. Short version: the tiers were
  authored on wine 11.12, and Valve ships on wine 11.0. Nobody is going to fix that
  asymmetry — the replay exists precisely so it doesn't have to be fixed.

---

## The tiers

Each tier is **exactly one squashed commit** on top of its parent tier. Not a topic branch
with history — one commit. Check it yourself:

```
$ BASE=$(git rev-parse 6260d89930c^)     # wine-11.12-79-g1bb0da91a5e
$ git log --oneline $BASE..soda-core
79e58cb8a70 soda-core: HID/HidP + comdlg32 portal + avrt + win32u (lands clean on Valve)
c69c111182f soda-build: Rust PE-staticlib build integration (configure/makedep/winegcc)
```

Two commits, because `soda-core` sits on `soda-build`. That is the entire history.

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

If a change needs neither, **squash it into an existing tier**. More tiers is not more
structure; it is more rebase surface.

`soda-fsync` is rule 1 in its purest form: Valve's base *already has* fsync, so pouring it into
`spritzvalve` would be a conflict looking for a place to happen — while an upstream base has
ntsync but no fsync, and wants it. A tier that exactly one consumer takes.

---

## The consumers

Two glasses, and it is easy to get their order backwards — because the one that ships is not
the one that is the point.

### `spritzwine` — the endeavour (not poured yet)

Soda on **upstream WineHQ wine**. No Valve patchset, no Proton, no Prater.

This is what the project is *for*: Rust in Wine, tracking what Wine is becoming rather than
what Valve froze. It carries the **full mix** — `soda-build` + `soda-core` + `soda-glorious`
+ `soda-ntdll` + `soda-branding` + `soda-fsync` — and it is the home of everything that has nothing to do
with games (`soda-ntdll`'s own commit subject says "(spritzwine only)"; battery reporting
exists because Windows programs ask how much charge is left and Wine had no good answer — no
frametime was harmed or improved). `regenerate.sh`'s usage example is literally `spritzwine-gen`
onto `winehq/wine-11.12`.

**It does not exist in generated form yet.** The branch carrying that name today (HEAD
`4815a57385c`) is the *original hand-authored pre-Valve fork* on vanilla wine 11.12 — the tree
the soda tiers were carved out of, not a tier-generated consumer. It predates the model this
document describes. Do not build from it in that form. Once regenerated from the tiers it joins
the never-edit set like every other consumer.

### `spritzvalve` — the necessity (what ships today)

`proton_11.0` + `soda-build` + `soda-core` + `soda-branding`. Tagged **`spritzvalve-11.0-1`**
(commit `8c00a2018`). This is what SpritzWine-Prater is built from; the superproject's `wine`
submodule is pinned to that tag. Boots Avatar: Frontiers of Pandora, Risk of Rain 2, Watch
Dogs: Legion.

It carries the **thin mix** on purpose, and it exists for one reason: the project's real gate is
*does the game boot?*, and games only boot on Valve's tree (see "Why a Valve base at all",
below). So this is the glass we can actually hand to someone, and the one that proves the Rust
does not break real software.

That makes it the vehicle, not the destination. **spritzvalve is how we prove SpritzWine is
safe; SpritzWine is the thing being proven.** Shipping first is not the same as being the point
— and if you only read the branch list, you would conclude the opposite, which is why this
paragraph exists.

(It was called `spritzvalve-v1` during the proof of concept — you will still see that name in
older superproject commits and in `regenerate.sh`'s usage comment. Same thing, renamed.)

### Consumers you don't see

Richer mixes (adding `soda-glorious`, `soda-ntdll`) get stitched and vetted in local clones and
are **deliberately not pushed**. An untested consumer sitting on the remote is an invitation for
someone to pin it, and then you own it. A consumer appears here when it passes the boot gate,
not when it compiles.

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

## Two bases (11.0 and 11.12) — the asymmetry is real

* The soda tiers are authored **on wine 11.12** (`wine-11.12-79-g1bb0da91a5e`), because that
  is where the fork was when the work was done.
* Every Valve wine branch — `proton_11.0`, `experimental_11.0`, `bleeding-edge` — sits **on
  wine 11.0**. `git describe` puts `proton_11.0` at `wine-11.0-1190-g2f70bfd4d0f`. There is
  no newer wine base inside the Proton 11 cycle. Valve is not going to hand you one.

So the shipping consumer is *11.12-authored tiers replayed backwards onto an 11.0 tree*. That
is not an accident to be cleaned up; it is the steady state, and it is why the replay is a
script with a tracked rerere cache rather than a one-off rebase somebody did once.

---

## Pouring the upstream glass (the open work)

`regenerate.sh` replays a **linear range** (`<home-base>..<soda-tip>`) onto a wine base. That is
why `spritzvalve` is easy: `soda-branding` *is* a straight line (build → core → branding), so
there is a single tip to point at.

The full mix is not a straight line. `soda-glorious`, `soda-ntdll`, `soda-branding` and
`soda-fsync` are **siblings** on `soda-core` — there is no single ref whose ancestry contains all
of them. So before the upstream consumer can be poured, the tiers it wants must be **stitched into
a chain**, and that chain is what `regenerate.sh` gets as its soda tip:

```sh
# 1. stitch the full mix into a linear tip (once; re-do when a tier moves)
git checkout -B soda-full soda-core
git cherry-pick soda-core..soda-glorious \
                soda-core..soda-ntdll   \
                soda-core..soda-fsync   \
                soda-core..soda-branding

# 2. pour it onto an upstream base
tools/regenerate.sh spritzwine winehq/wine-11.12 soda-full
```

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

# the shipping consumer:
tools/regenerate.sh spritzvalve valve-wine/proton_11.0 soda-branding
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

| Tag | Points at | Means |
|---|---|---|
| `spritzvalve-11.0-1` | `8c00a2018` | Generated consumer, replayed **onto** Proton's wine 11.0 |
| `soda-11.12-1` | `40abbdc4f97` | The tier set, authored **on** wine 11.12 |
| `spritzwine-11.0-0.2.0` | *(superproject)* | The superproject release |
| `prater-11.0-0.2.0` | *(distribution)* | The shipped build |

Note the two prepositions. `-11.0-` on a consumer means "replayed onto that base"; `-11.12-`
on the tier set means "authored on that base". Same field, different relationship, and that
is exactly the asymmetry from the previous section made visible in the name.

`soda-11.12-1` is worth one extra sentence, because it is not a tier tip: it is a single
frozen commit whose tree is the **union** of all six tiers — the lossless-partition
reference. If the tiers are ever re-cut, `soda-build + core + glorious + ntdll + demo +
branding` must still reconstruct that exact tree. It is the regression test for the split
itself.

**The operational fact, and it is the most important one in this document:** consumers are
**force-pushed**. A superproject gitlink pinning a raw consumer SHA will break the moment the
branch is regenerated — the old commit falls out of the branch's history, the remote stops
advertising it, and `git submodule update` dies with

```
Fetched in submodule path 'wine', but it did not contain <sha>.
```

A tag is a ref. A ref keeps the commit reachable and fetchable forever, no matter where the
branch moves. **Tag every consumer you pin, and pin the tag.** `.gitmodules` in the
superproject sets `branch = spritzvalve-11.0-1` for exactly this reason.

---

## Stitch notes

Things that were painful once and should not be re-learned.

**Where the base and a tier disagree, the base wins.** The tiers were authored against wine
11.12; the consumer lands on Valve's 11.0. Where upstream moved on between those two, the
tier's version of the file is *stale*, and the correct resolution is Valve's:

* `dlls/ntdll/Makefile.in` — Valve's 11.0 still imports `$(TOMCRYPT_PE_LIBS)` and links
  `$(UNWIND_LIBS)`; upstream had switched to `$(SYMCRYPT_PE_LIBS)` + `$(ZLIB_PE_LIBS)` and
  dropped libunwind entirely by 11.12. Take Valve's.
* `virtual_unwind()` — in Valve's tree it takes an extra `need_backtrace()` argument
  (`signal_x86_64.c`, `signal_arm64.c`, `signal_arm64ec.c`); the macro does not exist in
  11.12 at all. Take Valve's signature.

These are the conflicts the tracked rr-cache exists to remember. If you resolve one, commit
the cache.

**`soda-ntdll` deletes `dlls/ntdll/threadpool.c` outright** — all 2797 lines; Rust replaces
it. That is safe across the base swap for two independently-checked reasons: Valve never
patched `threadpool.c`, so nothing of theirs is being thrown away, and the `Tp*` export
surface in `ntdll.spec` is **identical** between 11.0 and 11.12 (35 exports on both). If
either of those ever stops being true, this deletion needs re-verifying before the next
`soda-ntdll` consumer ships.
