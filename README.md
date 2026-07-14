# SpritzWine

**Sanely adding value to Wine, leveraging Rust.**

A *G'spritzter* is wine cut with soda — same wine, just easier to get along with. SpritzWine is
that, for the other Wine: take the Wine your games already run on, mix in Rust exactly where the
C has been quietly ruining afternoons — parsers, descriptors, unwinders — and leave the rest in
peace. Nobody needs "Wine rewritten in Rust", and nobody would live to see it finished.

The Rust that ships today is modest and real: the HID report-descriptor parser, the `HidP_*`
value engine, MMCSS, the portal file chooser. The threadpool, the SEH unwinder and the controller
rumble exist too — written, building, and deliberately *not* shipped, because they haven't passed
the gate yet.

The gate is not the test suite. The gate is: **does the game boot?** Currently answering:
Avatar: Frontiers of Pandora, Risk of Rain 2 and Watch Dogs: Legion — on top of Valve's patched
Wine (the tree underneath Proton), a base that holds still. When something breaks on a base that
holds still, we know exactly whose Rust it was.

That base is the *proving ground*, not the destination. The endeavour is Rust in **upstream**
Wine; Valve's tree is simply the only one anti-tamper titles boot on, so it is what ships first.
See [`docs/VISION.md`](docs/VISION.md) and [`docs/BRANCHING.md`](docs/BRANCHING.md).

Think big, act small: Rust goes in where it earns its keep — safety, performance, conformance —
one shipped and proven piece at a time. Converting things is not the goal; it's the tab we pay
for fewer bugs. When a corner stops yielding value, we leave it in whatever mix of C and Rust it
honestly is, and go where the bugs actually live.

Nobody upstream asked for any of this, and nobody's inbox is being campaigned at. The code sits
here — LGPL, working, played on — and if a piece proves itself and somebody wants it, it's
theirs, no permission needed. Demonstrating beats lobbying.

**→ [`docs/VISION.md`](docs/VISION.md) — why this exists, where it's going, and what the names
mean.** The rest of this file is the machinery. Come for the Spritzer, stay for the frametimes.

---

> **„Man bringe den Spritzwein!"**
> *"One hands the Spritzwine!"* — "Bring forth the spritz wine!"
> — Michael Häupl, Mayor & Governor of Vienna, 1994–2018

**SpritzWine** integrates **Rust into [Wine](https://www.winehq.org/)**: parts of Wine are
built in Rust and linked back into the real Wine build, so Windows programs running under Wine
end up calling Rust code.

This repository is the **superproject** — the place where the Wine fork and the Rust workspace
are developed together, and where a release pins one against the other. It is *not* what you
install to play a game: that is **SpritzWine-Prater**, a separate repo (a Proton distribution
built from the pins made here). If you landed here looking for something to download, go there.

It started as exploratory work — a clean, maintainable path for writing Wine internals in
Rust — and has grown a production-ish distribution side along the way.

The default branch is **`rust`**. There is no `main`.

## The name — „Man bringe den Spritzwein!"

A small love letter to Vienna. **Michael Häupl** — Mayor and Governor of Vienna for 24 years
(1994–2018), and one of Austria's most quotable politicians — is remembered as much for his dry
Viennese wit as for his politics. His mock-regal call to *bring forth the spritz wine* became
shorthand for a thoroughly Viennese attitude: take the work seriously, take yourself a good deal
less so, and there's always time for a *G'spritzter*.
([the quote, in context](https://www.profil.at/oesterreich/man-bringe-den-spritzwein-michael-haeupl-in-zitaten/400905476))

*Spritzwein* — white wine topped up with sparkling water — is the unofficial house drink of
Vienna. So **SpritzWine** is the obvious toast: Wine (the compatibility layer) with a generous
splash of Rust, served in the same easy spirit. *Prost!* 🥂

## The pieces, and how they fit

| Piece | Where | What it is |
| --- | --- | --- |
| **superproject** | this repo (`rust` branch) | the Rust workspace + the pins + the build/regeneration tooling |
| **`wine/`** | submodule → tag `spritzvalve-11.0-1` | the Wine fork, a *generated* branch (see below) |
| **`rust/`** | a plain directory *in this repo* | the Rust crates the Wine fork links against |
| **`beisl/`** | submodule → tag `v0.1.0` | the performance tracer — an independent tool |
| **SpritzWine-Prater** | [separate repo](https://codeberg.org/SpritzWine/SpritzWine-Prater) | the Proton distribution people actually install |

The split between `wine/` and `rust/` is the load-bearing one: **the Wine tree carries only
build glue and the C-side bridges; the Rust code itself lives outside it**, in `rust/`. Wine's
`configure.ac` finds the crates via `$SPRITZWINE_RUST_DIR` (default: `$ac_pwd/../rust`, i.e. the
in-tree sibling that `build.sh` uses). An out-of-tree build such as Prater — where Wine is
configured in a separate object directory — pre-builds the crates and points that variable at
them. That is also why `rust/` is *not* a submodule: it is this repo's own content, and Wine is
merely a consumer of it.

### `beisl/` — the tracer

[Beisl](https://codeberg.org/SpritzWine/beisl) wraps a game and produces a symbolicated CPU trace
(`perf` samples) with MangoHud's frametime log as a side channel, so you can see who ate the frame.
It is deliberately **wine-agnostic** — it works against stock Proton, GE, Lutris-wine or system
wine just as well as against ours, which is the whole point when you want a SpritzWine-vs-Proton
comparison. It ships bundled with SpritzWine but keeps its own release cadence and its own version
numbers (`v0.1.0`), outside the family scheme below.

## Wine, and what's mine

[Wine](https://www.winehq.org/) is its own project, © its authors, released under the GNU
**LGPL-2.1-or-later**. None of that is mine. The `wine/` submodule is a Wine checkout carrying my
modifications and the hooks that link the Rust crates in — everything I built, broke, and fixed
about Wine lives there.

Nothing there is aimed at upstream as such; if a piece is useful to Wine, cherry-pick it freely —
the LGPL is there for exactly that. The Rust integration under `rust/` follows the same license.

### The branch you get is generated, not written

The pinned `wine/` branch is **not** hand-maintained. The changes are authored once as a stack of
thematic **`soda-*` tiers** (`soda-build` → `soda-core` → `soda-ntdll` / `soda-glorious` /
`soda-branding` / `soda-demo`), and a **consumer** branch is *generated* by replaying those tiers
onto a Wine base. `spritzvalve-11.0-1` — the current pin — is
`valve-wine/proton_11.0` + `soda-build` + `soda-core` + `soda-branding`.

The same tiers pour into a second glass, and that one is the point: **`spritzwine`**, the
consumer on an *upstream WineHQ* base. It carries the **full mix** (`soda-ntdll`, `soda-glorious`)
and the work that has nothing to do with games — battery reporting included. `spritzvalve` holds
still so that games can prove the Rust safe; `spritzwine` is what that proof is *for*.

So: **spritzvalve is the vehicle, spritzwine is the destination** — and shipping first is not the
same as being the point. Today only `spritzvalve` exists as a generated consumer; the branch named
`spritzwine` is still the legacy hand-authored fork, awaiting its first pour. See
[`docs/BRANCHING.md`](docs/BRANCHING.md).

Because a consumer is regenerated (and force-updated) whenever the tiers or the base move, a
consumer's history is not stable. The full story — tiers, bases, what lands where — is in
**[`docs/BRANCHING.md`](docs/BRANCHING.md)**.

Two consequences you should know about before touching anything:

**Never author on a consumer branch.** Your commit will be gone at the next regeneration. Author in
the `soda-*` tier the change belongs to.

**The `branch =` hints in `.gitmodules` name tags, not branches.** That looks like a bug and isn't:

```ini
[submodule "wine"]
	branch = spritzvalve-11.0-1     # a TAG
[submodule "beisl"]
	branch = v0.1.0                 # a TAG
```

Git resolves that key as a *remote-tracking branch* for `git submodule update --remote` — so
pointing it at a tag gives that command nothing to advance to, and the pin stays put. For a
force-pushed generated branch that is exactly what we want: a release must never drift onto a
history that was rewritten under it. **Releases are chosen, not tracked.** Moving a pin is an
explicit edit of `.gitmodules` plus a commit of the new gitlink.

### Regenerating a consumer — `tools/regenerate.sh`

```bash
tools/regenerate.sh <output-branch> <wine-base-ref> <soda-tip-ref> [<soda-home-base>]
tools/regenerate.sh spritzvalve-v1 valve-wine/proton_11.0 soda-branding
```

It checks the output branch out at the soda tip and rebases *that* branch `--onto` the Wine base
(never the soda ref itself — tiers stay authored on their home base). Two details matter:

- **`<soda-home-base>` is a parameter, not a `merge-base`.** For a cross-base regeneration — the
  normal case, 11.12-authored tiers onto `proton_11.0` — the merge base of the two refs is some
  common Wine ancestor, and replaying from there would drag in thousands of upstream commits. The
  default is the true fork point (parent of the first commit of the fork).
- **`tools/rr-cache/` is the shared conflict memory.** The script turns on `git rerere`, seeds it
  from that tracked directory, and copies newly learned resolutions back out. So the same stitch
  is resolved once, by a human, and then replayed identically by everyone else and by CI. If a
  regeneration stops on a conflict rerere has never seen, you resolve it, `git rebase --continue`,
  re-run the script — and the resolution is captured. Commit `tools/rr-cache/` when it changes.

## Version scheme

`<component>-<wine base>-<version>`:

| Tag | Component |
| --- | --- |
| `spritzwine-11.0-0.2.0` | this superproject |
| `prater-11.0-0.2.0` | the SpritzWine-Prater distribution |
| `spritzvalve-11.0-1` | the generated wine consumer |
| `soda-11.12-1` | the tier set |

The middle field is the **Wine base**, and the asymmetry in `soda-11.12-1` is the point, not a
typo: the tiers are *authored* on a wine-11.12 fork base and *replayed* onto `proton_11.0`. The
tier set's own version tracks where it is written; a consumer's tracks where it landed. `beisl`
sits outside all of this on its own `v0.1.0`.

## What's in the Rust workspace

Under `rust/`, three trees, one Cargo workspace (the `*_pe` crates are `exclude`d from it so their
own `panic = "abort"` profile is honored):

- `src_dll/<dll>/` — **Unix-side** crates, built as cdylibs (`libwine_<dll>.so`) and linked into
  the matching Wine DLL's `.so`:
  - `ntdll/` — `NtPowerInformation` (`SystemPowerInformation`), called from `dlls/ntdll/unix/system.c`.
  - `comdlg32/` — the native open/save dialog via the **XDG desktop portal**, routed through a
    WoW64 unix-call thunk so it works from 32- *and* 64-bit apps.
- `src_dll/<dll>_pe/`, `src_sys/<mod>_pe/` — **real-PE** crates (`no_std`, allocation-aware but
  self-contained), built for `{i686,x86_64}-pc-windows-gnullvm` and linked straight into the PE
  image: `ntdll_pe` (LZNT1 `RtlCompressBuffer`/`RtlDecompressBuffer`, threadpool, unwind),
  `avrt_pe` (MMCSS), `hid_pe` (the `HidP_*` value engine), `hidparse_pe` (the HID
  report-descriptor parser — a driver, hence `src_sys`, kernel-pool backed).
- `src_sys/winebus/` — Unix-side Sony gamepad (DualShock 4 / DualSense) rumble core for
  `winebus.sys`.
- `src_lib/` (`wine_api`) — bindgen/cbindgen FFI bindings to Wine's C APIs, plus a `-mwinerust`
  winegcc flag for PE-side programs (see `rust/README_INTEGRATION.md`).

Inside a crate, modules mirror Wine's source files — `ntdll/src/system.rs` ↔
`dlls/ntdll/unix/system.c`. If you're after the Rust side of a Wine function, that's where to look.

**Not every crate is wired into every consumer.** The tier decides. The current pin
(`spritzvalve-11.0-1` = build + core + branding) links Rust into **avrt, comdlg32, hid and
hidparse.sys**; the ntdll work lives in `soda-ntdll` and the controller work in `soda-glorious`,
neither of which is part of that consumer. `grep -l lwine_ dlls/*/Makefile.in` in the wine tree
answers the question for whatever you have checked out.

## Repository layout

```
.
├── build.sh              # one-shot bootstrap build (see Quick start)
├── wine/                 # Wine (git submodule, tag spritzvalve-11.0-1) — generated; see above
├── rust/                 # Rust workspace (a plain directory, NOT a submodule)
│   ├── src_lib/          #   wine_api: FFI bindings to Wine's C APIs (bindgen/cbindgen)
│   ├── src_dll/          #   Rust implementations of Wine DLL internals…
│   │   ├── ntdll/        #     …libwine_ntdll      (Unix side; mirrors dlls/ntdll/unix)
│   │   ├── ntdll_pe/     #     …PE staticlib linked into ntdll.dll (LZNT1, threadpool)
│   │   ├── avrt_pe/      #     …PE staticlib linked into avrt.dll (MMCSS)
│   │   ├── hid_pe/       #     …PE staticlib linked into hid.dll (HidP_* value engine)
│   │   └── comdlg32/     #     …libwine_comdlg32   (portal file chooser)
│   ├── src_sys/          #   drivers (kernel-pool backed)
│   │   ├── winebus/      #     …libwine_winebus    (DualShock 4 / DualSense rumble)
│   │   └── hidparse_pe/  #     …PE staticlib linked into hidparse.sys
│   └── src_programs/     #   example PE programs that call into Rust
├── beisl/                # performance tracer (git submodule, tag v0.1.0)
├── tools/                # regenerate.sh + rr-cache (consumer-branch generation)
├── packaging/            # package.sh -> relocatable tarball + launcher
└── installer/            # graphical + headless installer (Steam / Lutris / prefix)
```

## Quick start

The supported environment is the Fedora dev container under `.devcontainer/` (system deps, the
Rust toolchain, the PE targets, the libcosmic GUI deps, and `LIBCLANG_PATH` are preconfigured).
Open the repo in that container, then:

```bash
./build.sh                 # build Wine (new-WoW64, graphical) + the Rust crates
./packaging/package.sh     # -> dist/spritzwine-<ver>-x86_64.tar.xz  (run the installer inside)
```

That produces the standalone, relocatable build — a self-contained `bin/ lib/ share/` tree in the
same shape as wine-ge / lutris-wine — with an **installer** (`installer/`) that deploys it to
**Steam** (as a Steam Play compatibility tool), **Lutris** (as a Wine runner), a **custom
location**, or a fresh **`WINEPREFIX`**: a libcosmic GUI (`spritzwine-setup`) or a headless CLI
(`spritzwine-cli-install`). See [`installer/README.md`](installer/README.md). (The Proton-shaped
distribution is built elsewhere, from Prater.)

Building outside the container, you need: a Rust toolchain with the **`i686-pc-windows-gnullvm`**
and **`x86_64-pc-windows-gnullvm`** targets (`rustup target add …` — the PE-side staticlibs),
`clang` + `lld` (Wine's PE cross-compiler — no mingw needed), `libclang` (for bindgen), Wine's
build dependencies including the X11 + Wayland + FreeType dev files, and — for the GUI installer —
the libcosmic deps (listed in [`installer/README.md`](installer/README.md)). `.woodpecker.yml` has
a known-good package list. See `rust/README.md` and `rust/LIBCLANG_INSTALLATION_GUIDE.md`.

> **Note — new-WoW64:** Wine is built with `--enable-archs=i386,x86_64`: a 64-bit host that also
> builds 32-bit PE, so it runs both 64- and 32-bit Windows apps. The DLLs are real PE (clang+lld)
> over a single 64-bit Unix side. The Rust crates retarget to match: the Unix-side cdylibs to
> `x86_64-unknown-linux-gnu` (pinned in `rust/.cargo/config.toml`), and the PE-side staticlibs to
> `{i686,x86_64}-pc-windows-gnullvm`. For a lean headless build add `--without-x --without-freetype`
> to `CONFIGURE_FLAGS`; override parallelism with `JOBS=…`.

## How the build fits together

### Wine's generated files are not in the tree

On a Proton-based branch (`spritzvalve-*`), Valve ships the **generators, not their outputs** —
unlike vanilla WineHQ, which commits some of them. So before configuring anything, `build.sh`
recreates what is missing: `autoreconf` (there is no committed `configure`), `dlls/winevulkan/make_vulkan`
(→ `include/wine/vulkan.h`), `tools/make_specfiles syscalls` and `tools/make_requests` (the
wineserver protocol headers). Each step is a no-op on an in-sync vanilla-based tree. If you
configure such a checkout by hand and it explodes on a missing `configure` or a missing
`vulkan.h`, this is why — and it is also why `git status` in `wine/` shows a pile of untracked
and modified generated files after a build. That is expected.

### The ordering is a two-way coupling

`build.sh` runs six steps, and the order is forced:

1. **Fetch the submodules**, then **configure Wine** (after the regeneration above).
2. **The Rust crates first.** They have *no* Wine dependencies, so they build standalone. The
   Unix cdylibs (`wine_ntdll`, `wine_comdlg32`, `wine_winebus`) go to the host triple; then every
   `*_pe` crate is built once per PE arch. Both must exist before Wine links them.
3. **`make dlls` next.** Building Wine's DLLs runs `widl`, which generates the public headers
   (`include/*.h` from `*.idl`), and links the Rust libraries into the modules.
4. **`wine_api` after that.** Its bindgen step parses those generated headers, so it can only run
   once step 3 has produced them.
5. **`make` (programs + loader) last.**

Two gotchas worth knowing:

- A PE staticlib is linked via `EXTRADLLFLAGS`, which **makedep does not track as a prerequisite**.
  A rebuilt crate would therefore *not* relink the DLL, and you'd silently keep running the old
  code. `build.sh` works around it by deleting a PE image whose staticlib is newer (the same thing
  `relink-ntdll.sh` does for the quick dev loop).
- The `-mwinerust` demo (`programs/ping`, a PE program linking `wine_api`) is **disabled** under
  new-WoW64: a real-PE program cannot link the ELF `wine_api` cdylib. Reworking `wine_api` as a PE
  library is a follow-up. `wine_api` is still built, so its bindings keep compiling against the
  fresh headers.

The Unix-side crates are wired in via a shared `WINE_RUST_LDFLAGS` (rpath + library search path,
set in Wine's `configure.ac`); each DLL's `Makefile.in` appends its own `-Wl,-lwine_<dll>` to
`UNIX_LIBS`.

### Adding a Rust implementation for another DLL

1. Create `rust/src_dll/<dll>/` (crate `wine_<dll>`, `crate-type = ["cdylib"]`) and add it to the
   `members` list in `rust/Cargo.toml`. Use `rust/src_dll/ntdll/` as a template.
2. Export functions with `#[unsafe(no_mangle)] pub extern "C"`.
3. In `wine/dlls/<dll>/Makefile.in`, append `$(WINE_RUST_LDFLAGS) -Wl,-lwine_<dll>` to `UNIX_LIBS`
   (or the relevant link line).
4. Call the function from the C source (a forward declaration avoids an include dependency on the
   generated header).
5. Commit the Wine-side half **on the right `soda-*` tier**, not on the consumer branch — see
   [The branch you get is generated](#the-branch-you-get-is-generated-not-written).

For a PE-side crate the naming does the wiring: a crate `<name>_pe` in `src_dll/` (user DLL) or
`src_sys/` (driver) builds `libwine_<name>_pe.a` and is discovered by `build.sh` automatically —
no edit to the build script needed.

## License

SpritzWine — the Wine patches and the Rust integration alike — is **GNU LGPL-2.1-or-later**; see
[`LICENSE`](https://codeberg.org/SpritzWine/wine/src/branch/spritzwine/LICENSE). The original Wine work is © its authors; see
[Wine, and what's mine](#wine-and-whats-mine).
