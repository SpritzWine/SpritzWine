# SpritzWine installer

A small installer that deploys a built, **relocatable** SpritzWine tree (see
[`../packaging/`](../packaging)) to the places you actually launch Wine builds from:

- **Steam** — as a Steam Play compatibility tool you can pick under a game's
  *Properties → Compatibility → Force the use of a specific Steam Play compatibility tool*.
- **Lutris** — as a Wine runner you can select in a game's runner options.
- a **custom location** — a self-contained tree you run with `./spritzwine program.exe`.
- and it can **initialise a `WINEPREFIX`** with this build.

It ships in two forms, both bundled into the release tarball:

| binary | what it is |
| --- | --- |
| `spritzwine-setup` | graphical installer (libcosmic / the COSMIC look) |
| `spritzwine-cli-install` | headless equivalent for servers, scripts and CI |

## Layout

Two **standalone** crates (intentionally *not* part of the `rust/` Wine workspace, and not
sharing a lockfile):

- [`core/`](core) — `spritzwine-install-core`: all the deploy logic (detect Steam/Lutris,
  copy the tree, write the Steam descriptors, init a prefix) plus the headless
  `spritzwine-cli-install` binary. **No GUI dependencies**, so it builds and tests with a plain
  toolchain (`cargo test` in `core/`).
- [`gui/`](gui) — `spritzwine-setup`: a four-page wizard (Welcome → Targets → Progress →
  Done) over `core`. libcosmic is a pinned git dependency; installs run on a background
  thread that the UI polls on a timer.

## Build

```bash
cargo build --release --manifest-path installer/core/Cargo.toml   # spritzwine-cli-install
cargo build --release --manifest-path installer/gui/Cargo.toml    # spritzwine-setup (libcosmic)
```

`packaging/package.sh` does both and drops the binaries into the tarball root; set
`WITH_INSTALLER=no` to skip the GUI (the headless `spritzwine-cli-install` is always bundled).

### GUI build dependencies (Fedora)

The headless CLI needs nothing beyond the Rust toolchain. The libcosmic GUI needs:

```
gcc-c++ libxkbcommon-devel libxkbcommon-x11-devel wayland-devel \
mesa-libgbm-devel mesa-libGL-devel mesa-libEGL-devel vulkan-loader-devel \
fontconfig-devel expat-devel pkgconf-pkg-config
```

(These are already in `.devcontainer/Dockerfile` and `.woodpecker.yml`.)

## Use

Shipped inside the tarball, the installer treats the surrounding tree as the build to
install, so the end-user flow is just:

```bash
tar -xf spritzwine-<ver>-x86_64.tar.xz
spritzwine-<ver>/spritzwine-setup           # GUI
# or
spritzwine-<ver>/spritzwine-cli-install --list  # show detected Steam/Lutris targets
spritzwine-<ver>/spritzwine-cli-install --steam --lutris
```

During development, run from a checkout by pointing at a built tree:

```bash
cargo run --manifest-path installer/gui/Cargo.toml -- --payload dist/spritzwine-<ver>
```

## Steam wrapper, and why the generic build stays generic

A bare Wine build is not itself a Steam Play tool — Steam only invokes tools that speak
Proton's protocol. When (and only when) you choose the Steam target, the installer writes a
small Proton-style shim next to the copied tree (`compatibilitytool.vdf`,
`toolmanifest.vdf`, and a `spritzwine-proton` script that maps Steam's verbs and
`STEAM_COMPAT_DATA_PATH` onto the bundled `bin/wine`). The generic tarball itself never
contains any of that — the Steam glue is generated on demand. The wrapper runs host-native
(no Steam Linux Runtime container), matching the host-linked relocatable tree.

The entry point is named `spritzwine-proton`, **not** `proton`, on purpose: Lutris (and
similar) auto-import every Steam compatibility tool whose directory contains a file literally
named `proton`, treat it as a Proton build, and then fail looking for its wine at the Proton
layout `files/bin/wine`. Steam runs whatever `toolmanifest.vdf`'s `commandline` points at
(non-Proton tools like Boxtron/Luxtorpeda do the same), so renaming keeps Steam working while
letting a separate plain-Wine Lutris runner coexist without a name collision.
