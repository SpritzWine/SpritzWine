# SpritzWine

> **„Man bringe den Spritzwein!"**
> *"One hands the Spritzwine!"* — "Bring forth the spritz wine!"
> — Michael Häupl, Mayor & Governor of Vienna, 1994–2018

**SpritzWine** integrates **Rust into [Wine](https://www.winehq.org/)**: parts of Wine are
built in Rust and linked back into the real Wine build, so Windows programs running under Wine
end up calling Rust code. On top of that it ships a **relocatable, graphical build** and a small
**installer**, so you can actually run it from Steam, Lutris, or a Wine prefix.

It started as exploratory work — a clean, maintainable path for writing Wine internals in
Rust — and has grown a production-ish distribution side along the way.

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

## Wine, and what's mine

[Wine](https://www.winehq.org/) is its own project, © its authors, released under the GNU
**LGPL-2.1-or-later**. None of that is mine. The `wine/` submodule here is a Wine checkout on a
**`spritzwine`** branch carrying my modifications and the hooks that link the Rust crates in —
everything I built, broke, and fixed about Wine lives there.

Nothing on that branch is aimed at upstream as such; if a piece is useful to Wine, cherry-pick it
freely — the LGPL is there for exactly that. The Rust integration under `rust/` follows the same
license. See the [`LICENSE`](https://codeberg.org/SpritzWine/wine/src/branch/spritzwine/LICENSE) for the details.

## What works today

**Rust inside Wine**

- A reusable build path: per-DLL Rust crates under `rust/src_dll/<dll>/`, each producing
  `libwine_<dll>.so`, linked into the matching Wine DLL.
- **`NtPowerInformation`** (`SystemPowerInformation`) implemented in Rust
  (`rust/src_dll/ntdll/`), called from `dlls/ntdll/unix/system.c`.
- **comdlg32 portal file chooser** (`rust/src_dll/comdlg32/`) — the native open/save dialog via
  the XDG desktop portal, routed through a WoW64 unix-call thunk so it works from 32- *and*
  64-bit apps.
- **LZNT1 compression** (`RtlCompressBuffer` / `RtlDecompressBuffer`) in a self-contained,
  allocation-free **real-PE** crate (`rust/src_dll/ntdll_pe/`), linked into `ntdll.dll` for both
  arches.
- `rust/src_lib` (`wine_api`): bindgen/cbindgen FFI bindings to Wine's C APIs, plus a `-mwinerust`
  winegcc flag for PE-side programs (see `rust/README_INTEGRATION.md`).

**Running it like a real Wine build**

- A **graphical, relocatable** new-WoW64 build (X11 + Wayland + FreeType): `build.sh` then
  `packaging/package.sh` produce a self-contained `bin/ lib/ share/` tarball — the same shape as
  wine-ge / lutris-wine.
- An **installer** (`installer/`) that deploys that build to **Steam** (as a Steam Play
  compatibility tool), **Lutris** (as a Wine runner), a **custom location**, or a fresh
  **`WINEPREFIX`** — as a libcosmic GUI (`spritzwine-setup`) or a headless CLI
  (`spritzwine-cli-install`). See [`installer/README.md`](installer/README.md).

## Repository layout

```
.
├── build.sh              # one-shot bootstrap build (see Quick start)
├── wine/                 # Wine (git submodule, branch `spritzwine`) — see "Wine, and what's mine"
├── rust/                 # Rust workspace
│   ├── src_lib/          #   wine_api: FFI bindings to Wine's C APIs (bindgen/cbindgen)
│   ├── src_dll/          #   Rust implementations of Wine DLL internals…
│   │   ├── ntdll/        #     …libwine_ntdll      (Unix side; mirrors dlls/ntdll/unix)
│   │   ├── ntdll_pe/     #     …real-PE staticlib linked into ntdll.dll (LZNT1)
│   │   └── comdlg32/     #     …libwine_comdlg32   (portal file chooser)
│   └── src_programs/     #   example PE programs that call into Rust
├── packaging/            # package.sh -> relocatable tarball + launcher
└── installer/            # graphical + headless installer (Steam / Lutris / prefix)
```

Inside `rust/src_dll/<dll>/`, modules mirror Wine's source files — e.g. `ntdll/src/system.rs` ↔
`dlls/ntdll/unix/system.c`. If you're after the Rust side of a Wine function, that's where to look.

## Quick start

The supported environment is the Fedora dev container under `.devcontainer/` (system deps, the
Rust toolchain, the PE targets, the libcosmic GUI deps, and `LIBCLANG_PATH` are preconfigured).
Open the repo in that container, then:

```bash
./build.sh                 # build Wine (new-WoW64, graphical) + the Rust crates
./packaging/package.sh     # -> dist/spritzwine-<ver>-x86_64.tar.xz  (run the installer inside)
```

Building outside the container, you need: a Rust toolchain with the **`i686-pc-windows-gnullvm`**
and **`x86_64-pc-windows-gnullvm`** targets (`rustup target add …` — the PE-side staticlib),
`clang` + `lld` (Wine's PE cross-compiler — no mingw needed), `libclang` (for bindgen), Wine's
build dependencies including the X11 + Wayland + FreeType dev files, and — for the GUI installer —
the libcosmic deps (listed in [`installer/README.md`](installer/README.md)). See `rust/README.md`
and `rust/LIBCLANG_INSTALLATION_GUIDE.md` for specifics.

> **Note — new-WoW64:** Wine is built with `--enable-archs=i386,x86_64`: a 64-bit host that also
> builds 32-bit PE, so it runs both 64- and 32-bit Windows apps. The DLLs are real PE (clang+lld)
> over a single 64-bit Unix side. The Rust crates retarget to match: the Unix-side cdylibs to
> `x86_64-unknown-linux-gnu` (pinned in `rust/.cargo/config.toml`), and the PE-side staticlib to
> `{i686,x86_64}-pc-windows-gnullvm`. For a lean headless build add `--without-x --without-freetype`
> to `CONFIGURE_FLAGS`; override parallelism with `JOBS=…`.

## How the build fits together

There's a two-way coupling between Wine and the Rust crates, which is why the order in `build.sh`
matters:

1. **`libwine_ntdll` first.** The per-DLL crates under `rust/src_dll/` have *no* Wine
   dependencies, so they build standalone and can be linked into ntdll.
2. **`make dlls` next.** Building Wine's DLLs runs `widl`, which generates the public headers
   (`include/*.h` from `*.idl`). ntdll links `libwine_ntdll` here.
3. **`wine_api` after that.** Its bindgen step parses those generated headers, so it can only run
   once step 2 has produced them.
4. **`make` (programs + loader) last.** (The `ping` `-mwinerust` example that linked `wine_api` is
   disabled under new-WoW64 — a real-PE program can't link the ELF `wine_api` cdylib; reworking
   that path is a follow-up.)

The DLL crates are wired in via a shared `WINE_RUST_LDFLAGS` (rpath + library search path, set in
Wine's `configure.ac`); each DLL's `Makefile.in` appends its own `-Wl,-lwine_<dll>` to `UNIX_LIBS`.

### Adding a Rust implementation for another DLL

1. Create `rust/src_dll/<dll>/` (crate `wine_<dll>`, `crate-type = ["cdylib"]`) and add it to the
   `members` list in `rust/Cargo.toml`. Use `rust/src_dll/ntdll/` as a template.
2. Export functions with `#[unsafe(no_mangle)] pub extern "C"`.
3. In `wine/dlls/<dll>/Makefile.in`, append `$(WINE_RUST_LDFLAGS) -Wl,-lwine_<dll>` to `UNIX_LIBS`
   (or the relevant link line).
4. Call the function from the C source (a forward declaration avoids an include dependency on the
   generated header).

## License

SpritzWine — the Wine patches and the Rust integration alike — is **GNU LGPL-2.1-or-later**; see
[`LICENSE`](https://codeberg.org/SpritzWine/wine/src/branch/spritzwine/LICENSE). The original Wine work is © its authors; see
[Wine, and what's mine](#wine-and-whats-mine).
