# Packaging SpritzWine

`package.sh` turns an already-built tree into a **relocatable** tarball that runs
from any path on a compatible host — the same shape as other portable Wine builds
(`wine-ge`, `lutris-wine`, …): a self-contained `bin/ lib/ share/` tree plus a
launcher, linked against the host's own system libraries.

## Build a package

```bash
./build.sh                 # build SpritzWine (new-WoW64: runs 32- and 64-bit apps)
./packaging/package.sh      # -> dist/spritzwine-<version>-x86_64.tar.xz
```

## Use it

```bash
tar -xf dist/spritzwine-*-x86_64.tar.xz
spritzwine-*/spritzwine --version
spritzwine-*/spritzwine path/to/program.exe       # 32- or 64-bit Windows program
WINEPREFIX=~/games/foo spritzwine-*/spritzwine setup.exe
```

Or point any Wine prefix manager at the extracted tree's `bin/wine`.

## How it stays relocatable

Wine's loader locates `lib/wine` and `share/wine` relative to `bin/wine`, so the
tree runs from anywhere. The only non-relocatable part is the Rust unix libraries
(`libwine_ntdll.so`, `libwine_comdlg32.so`): their consumers (`ntdll.so`,
`comdlg32.so`) bake an absolute build-tree `RUNPATH`. `package.sh` bundles those
libraries next to their consumers, and the `spritzwine` launcher puts that directory
on `LD_LIBRARY_PATH` — which the dynamic loader searches *before* `RUNPATH`, so the
bundled copies win and no rpath rewriting is needed. (If `patchelf` happens to be
installed, `package.sh` additionally rewrites the `RUNPATH` to `$ORIGIN` so the libs
self-locate even when invoked without the launcher.)

## Continuous builds

`.woodpecker.yml` builds and packages on Codeberg CI, and publishes the tarball as a
release artifact on tags.
