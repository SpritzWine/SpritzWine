# SpritzWine 0.2.0

The release where the Wine underneath changed. SpritzWine now mixes its Rust into
**Valve's** Wine — the tree Proton is built on — instead of vanilla WineHQ, which is the
difference between an anti-tamper title crashing at thirteen seconds and one you can play.
The fork is no longer hand-maintained either: it is *generated*, by replaying a stack of
`soda-*` tiers onto that base. And for the first time there is CI that can tell a green
build from a working one.

## New

- **A Valve base, and the crash that forced it.** The shipped Wine is
  `spritzvalve-11.0-1` — `valve-wine/proton_11.0` plus three tiers, and nothing else.
  The original fork rode vanilla wine 11.12, and **Avatar: Frontiers of Pandora died at
  ~13 seconds** on it: Ubisoft's anti-tamper layer against Wine's copy-on-write semantics.
  The missing piece was Valve's `WINE_SIMULATE_WRITECOPY` (it is in `proton_11.0`'s
  `dlls/ntdll/unix/loader.c`; it does not exist in upstream Wine, at any version). Rebasing
  onto Valve's tree inherits the whole patchset instead of chasing it one hack at a time.
- **The boot gate.** Wine's conformance tests are necessary and not sufficient, so the gate
  is: *does the game boot?* Currently answering **Avatar: Frontiers of Pandora**, **Risk of
  Rain 2** and **Watch Dogs: Legion**. Avatar plays at ~67 fps on the author's machine —
  one machine, no benchmark suite, and explicitly **no claim of a speedup over stock
  Proton**. Just the difference between a game that runs and a game that doesn't.
- **The branch is generated, not written.** The changes are authored once as thematic tiers
  (`soda-build` → `soda-core` → `soda-branding` / `soda-ntdll` / `soda-glorious`), and a
  consumer branch is *replayed* onto a Wine base by `tools/regenerate.sh` — with a tracked
  `git rerere` cache (`tools/rr-cache/`, 9 resolutions so far) so the same cross-base stitch
  is resolved once by a human and identically by everyone else. The tiers are authored on
  wine 11.12 and land on Valve's 11.0; where the two disagree, **the base wins**. The point
  is that the fork can be peeled off the base again.
- **What is actually Rust in the Wine you get.** Four modules, and you can check it yourself
  with `grep -l lwine_ dlls/*/Makefile.in`:
  `hidparse.sys` (the HID **report-descriptor parser** — a `no_std` PE staticlib with a
  kernel-pool allocator, since a driver imports only ntoskrnl), `hid` (the `HidP_*` **value
  engine**: caps walk, usage/data reads, report writes, every one bounds-checked against the
  report length), `avrt` (**MMCSS** — task name → scheduling category → NT thread priority,
  with a generation-tagged handle table), and `comdlg32` (the **XDG desktop portal** file
  chooser, routed through a WoW64 unix-call thunk so 32-bit apps get it too). Against the
  Valve base that is **−980 / +127** lines in `dlls/hid/hidp.c` and **−529 / +11** in
  `dlls/hidparse.sys/main.c`: two byte-blob parsers, deleted and replaced.
- **What is deliberately *not* shipping.** `soda-ntdll` (the NT threadpool, the
  `RtlVirtualUnwind2` opcode interpreter, SEH scope-table and i386 chain hardening, LZNT1,
  battery reporting) and `soda-glorious` (DualSense/DualShock 4 rumble, dinput, winexinput,
  audio-haptics endpoint pairing) are **written, building and tested — and in no published
  consumer.** They ship when they pass the boot gate, not when they compile: an untested
  consumer sitting on a remote is an invitation for somebody to pin it, and then you own it.
- **Something you can install.** `./packaging/package.sh` produces a relocatable
  `bin/ lib/ share/` tree — the same shape as wine-ge or lutris-wine — that runs from any
  path: `./spritzwine <program.exe>`. It carries a graphical installer (`spritzwine-setup`,
  libcosmic) and a headless one (`spritzwine-cli-install`), which deploy it as a **Steam
  Play compatibility tool**, a **Lutris runner**, into a custom location, or into a fresh
  `WINEPREFIX`. (The Proton-shaped distribution is built elsewhere, from Prater.)
- **beisl comes with it.** The [performance tracer](https://codeberg.org/SpritzWine/beisl)
  (v0.1.0) is bundled in the tarball and offered as a default-on installer step. It is
  deliberately **Wine-agnostic** — it traces stock Proton exactly as happily as it traces
  ours, because a performance claim you cannot reproduce against the thing you claim to beat
  is not a claim.
- **CI that gates on behaviour, not on a green build.** Three Forgejo Actions lanes on the
  org's own runner, replacing a `.woodpecker.yml` that could never have gone green (a ~30-min
  Wine build on a hosted runner capped at 10 minutes). The dependency set is pinned in a
  build image that **bakes its own stamp**, so a stale runner image fails in five seconds
  with the command to fix it instead of hours later in whatever way the missing package
  dictates. `ci/smoke.sh` then checks the four things a compiler cannot: what `configure`
  **decided** (X11, freetype, fontconfig and vulkan required — Wine silently disables any
  backend whose headers are absent, and the old CI package list had no `libX11-devel` at
  all, so it would have published a Wine with **no display driver** and said nothing); the
  **new-WoW64 shape** of the packaged tree (a 64-bit unix side, *zero* native 32-bit unix
  `.so`, both PE arches); that the **Rust libs are really in the tarball** (if the crates
  quietly stopped being linked, every other check would still pass); a fresh-prefix
  `wineboot -u`; and a real PE in **both** bitnesses, judged by exit code. On green, it asks
  Prater whether this SpritzWine still works as Proton.

## Fixed

- **`make dlls` built nothing — and always had.** Wine's phony targets are per-module;
  `dlls` is merely a directory that exists, so step 4 of `build.sh` printed *"Nothing to be
  done"* and compiled exactly zero DLLs, for as long as the script had existed. A warm tree
  never noticed (the next step found the `ntdll.so` an earlier `make` had left lying
  around); a **cold** one — CI, or a new contributor — died linking the Rust `wine` binary
  with an undefined `__wine_dbg_output`. Only a machine that had never built this project
  could see it, and until now, no such machine ran it.
- **A cached "not found" that poisoned every later build.** Cargo replays a build script's
  output while its declared inputs look unchanged — and the Wine tree lives outside the
  package, so it was never an input. One run that probed for `ntdll.so` *before* Wine was
  built cached the miss, and that result (minus its link arguments) replayed forever after:
  the Rust `wine` binary failed on undefined symbols while the freshly linked `ntdll.so` sat
  next to it. Dev boxes never saw it, because there the file existed the first time cargo
  ever ran. `cargo:rerun-if-changed` on each probed path fixes it.
- **udev without SDL2 does not link `winebus`.** `bus_udev.c` is compiled whenever libudev
  is present and calls `is_sdl_ignored_device()`, which exists only under
  `SONAME_LIBSDL2` — so a build with udev and without SDL2 fails to link `winebus.so`, while
  `configure` cheerfully reports SDL2 as *optional*. Keep both or drop both. (It took three
  CI runs to pin down; the build image now carries the pair.)
- **The configure gate could not see the environment change.** `[ Makefile -nt configure ]`
  is tree-internal: add a package to the build image and a warm tree keeps a `config.h`
  describing the old one — skipping configure forever, no matter how often the image is
  rebuilt. That is exactly how the SDL2 breakage survived. `build.sh` now fingerprints the
  environment into `wine/.build-env-stamp` and forces a reconfigure when it changes (cheap
  when nothing effective changed: `config.status` leaves an identical `config.h` untouched).
- **GDI handle exhaustion from a bounded-on-Windows cache Wine left unbounded.**
  `NtUserGetDCEx` minted a fresh DCE on every call for an app that leaks display DCs, until
  the 0x10000-entry GDI handle table ran out and the process died. Saints Row's launcher
  does exactly this during shader compilation. The common-DC cache now recycles its
  least-recently-used entry past a 256 cap — far above any legitimate concurrent use, far
  below the handle-table ceiling. Under a 2000-leak stress probe: 2010 → 266 DC allocations,
  no crash.
- `wineusb.sys` implements `BusQueryContainerID` (it was a FIXME, fourteen times per Watch
  Dogs: Legion log), and `.gitmodules` moved from `ssh://` to `https://` — the old URLs were
  unclonable by CI and by any drive-by contributor, and only ever worked for the author.

## Retracted

- **There was never an ntsync deadlock.** Earlier documentation here described a hang in
  32-bit CEF launchers under ntsync, and a per-game opt-out shipped for it. It was a
  misdiagnosis: under ntsync *every* wait is an ioctl, so threads parked in `/dev/ntsync`
  ioctls are what a **healthy** process looks like. Both Ubisoft titles boot with ntsync on.
  The claim and the opt-out are gone. A bug you only ever confirm is not a bug you have
  investigated.

## Heads-up

- **new-WoW64 is a standing bet, and it is ours to pay for.** There is no 32-bit unix side
  in this Wine at all: 32-bit Windows programs run as real 32-bit PE (clang + lld, no mingw)
  over a single 64-bit unix layer. Upstream Wine built that mechanism and it is where Wine is
  heading — but Valve's shipping Proton still carries a classic native 32-bit unix side, so
  this is a path that upstream *builds* and nobody *ships*. The upside is real: **no 32-bit
  host libraries, ever**, on distros busy dropping them. So is the bill: we find its bugs
  first, and — as the retraction above records — we are also first to blame it for bugs it
  never had.

---

# SpritzWine 0.1.0

The first thing that could leave the build tree: a Wine with Rust in it, packaged as a
tarball you can extract anywhere and run.

## New

- **The new-WoW64 migration.** The Rust integration moved to a 64-bit host build with no
  32-bit unix side — the architectural bet everything since has been built on. The Unix-side
  crates target `x86_64-unknown-linux-gnu`; the PE-side ones are compiled for
  `{i686,x86_64}-pc-windows-gnullvm` and linked straight into the PE image.
- **The relocatable tarball.** A self-contained `bin/ lib/ share/` tree plus a launcher, in
  the same shape as other portable Wine builds: `./spritzwine <program.exe>`.
- **The comdlg32 XDG-portal file chooser**, in Rust and on the Unix side: the legacy dialog
  *and* the Vista `IFileDialog`, multi-select, folder picking, filter-index round-tripping,
  and a WoW64 unix-call thunk so a 32-bit app gets the native chooser too.
- **The first PE-side Rust**, as a `no_std`, `panic = "abort"` staticlib linked into
  `ntdll.dll` behind a single link anchor (LZNT1 `RtlCompressBuffer`/`RtlDecompressBuffer`),
  plus `NtPowerInformation`/`SystemBatteryState` on the Unix side. Both of those live in
  `soda-ntdll` today and are *not* in the shipping consumer — see 0.2.0.
- **`wine_api`**, the bindgen/cbindgen FFI layer over Wine's C APIs, and the `-mwinerust`
  winegcc flag.
