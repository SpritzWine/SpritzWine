# SpritzWine

A first attempt at integrating **Rust into [Wine](https://www.winehq.org/)** — building
parts of Wine in Rust and linking them back into the real Wine build, so Windows
programs running under Wine end up calling Rust code.

This is exploratory work, not a production fork. The goal is to prove out a clean,
maintainable path for writing Wine internals in Rust.

## What works today

- A reusable build path: per-DLL Rust crates live under `rust/src_dll/<dll>/`, each
  producing `libwine_<dll>.so`, linked into the corresponding Wine DLL.
- The first real example: **`NtPowerInformation`'s `SystemPowerInformation` case is
  implemented in Rust** (`rust/src_dll/ntdll/src/system.rs`) and called from Wine's
  `dlls/ntdll/unix/system.c`.
- `rust/src_lib` (`wine_api`): bindgen/cbindgen-generated FFI bindings to Wine's C
  APIs, plus a `-mwinerust` winegcc flag for PE-side programs (see `rust/README_INTEGRATION.md`).

## Repository layout

```
.
├── build.sh              # one-shot bootstrap build (see below)
├── wine/                 # patched Wine (git submodule, branch `spritzwine`)
└── rust/                 # Rust workspace
    ├── src_lib/          # wine_api: FFI bindings to Wine's C APIs (bindgen/cbindgen)
    ├── src_dll/          # Rust implementations of Wine DLL internals…
    │   └── ntdll/        #   …e.g. libwine_ntdll (mirrors dlls/ntdll source files)
    └── src_programs/     # example PE programs that call into Rust
```

Inside `rust/src_dll/<dll>/`, modules mirror Wine's source files — e.g.
`ntdll/src/system.rs` corresponds to `dlls/ntdll/unix/system.c`. If you're looking
for the Rust side of a Wine function, that's where to find it.

## Quick start

The supported environment is the Fedora dev container under `.devcontainer/`
(all system deps, the Rust toolchain, the i686 target, and `LIBCLANG_PATH` are
preconfigured). Open the repo in that container, then:

```bash
./build.sh
```

That runs the whole thing end-to-end. To build manually or outside the container,
you need: a Rust toolchain with the **`i686-unknown-linux-gnu`** target
(`rustup target add i686-unknown-linux-gnu`), `libclang` (for bindgen), Wine's
build dependencies, and 32-bit dev libraries. See `rust/README.md` and
`rust/LIBCLANG_INSTALLATION_GUIDE.md` for specifics.

> **Note:** Wine is built **32-bit (i386)** so its `HOST_ARCH` matches the i686
> Rust target pinned in `rust/.cargo/config.toml`. `build.sh` configures with
> `--disable-win64` for this reason. Override with `CONFIGURE_FLAGS=…` and
> parallelism with `JOBS=…`.

## How the build fits together

There's a two-way coupling between Wine and the Rust crates, which is why the order
in `build.sh` matters:

1. **`libwine_ntdll` first.** The per-DLL crates under `rust/src_dll/` have *no* Wine
   dependencies, so they build standalone and can be linked into ntdll.
2. **`make dlls` next.** Building Wine's DLLs runs `widl`, which generates the public
   headers (`include/*.h` from `*.idl`). ntdll links `libwine_ntdll` here.
3. **`wine_api` after that.** Its bindgen step parses those generated headers, so it
   can only run once step 2 has produced them.
4. **`make` (programs + loader) last.** Wine's `ping` example links `wine_api`.

The DLL crates are wired in via a shared `WINE_RUST_LDFLAGS` (rpath + library search
path, set in Wine's `configure.ac`); each DLL's `Makefile.in` appends its own
`-Wl,-lwine_<dll>` to `UNIX_LIBS`.

### Adding a Rust implementation for another DLL

1. Create `rust/src_dll/<dll>/` (crate `wine_<dll>`, `crate-type = ["cdylib"]`) and add
   it to the `members` list in `rust/Cargo.toml`. Use `rust/src_dll/ntdll/` as a template.
2. Export functions with `#[unsafe(no_mangle)] pub extern "C"`.
3. In `wine/dlls/<dll>/Makefile.in`, append `$(WINE_RUST_LDFLAGS) -Wl,-lwine_<dll>`
   to `UNIX_LIBS` (or the relevant link line).
4. Call the function from the C source (a forward declaration avoids an include
   dependency on the generated header).

## License

Wine is LGPL-2.1-or-later; the Rust integration follows the same license.
