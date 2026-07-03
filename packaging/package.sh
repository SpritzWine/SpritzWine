#!/usr/bin/env bash
#
# Package an already-built SpritzWine tree into a relocatable tarball that runs
# from any path on a compatible host -- the same shape as other portable Wine
# builds (a self-contained bin/ lib/ share/ tree plus a launcher).
#
# Prerequisite: run ./build.sh first (this script does not build Wine). It then
# installs Wine into a staging prefix, bundles the Rust unix libraries and the
# launcher, and produces:
#
#     dist/spritzwine-<version>-x86_64.tar.xz
#
# Extract it anywhere and run  <dir>/spritzwine <program.exe>  (or point a Wine
# prefix manager at <dir>/bin/wine).
#
# Override: VERSION=… DIST=…
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
VERSION="${VERSION:-$(git describe --tags --always 2>/dev/null || echo 0.1.0)}"
DIST="${DIST:-$ROOT/dist}"
STAGE="$DIST/stage"
NAME="spritzwine-$VERSION"
TREE="$DIST/$NAME"
RUST_DEPS="$ROOT/rust/target/x86_64-unknown-linux-gnu/release/deps"
UNIXDIR="lib/wine/x86_64-unix"

[ -x "$ROOT/wine/wine" ] || { echo "error: Wine not built -- run ./build.sh first." >&2; exit 1; }
for so in libwine_ntdll.so libwine_comdlg32.so; do
    [ -f "$RUST_DEPS/$so" ] || { echo "error: missing $RUST_DEPS/$so -- run ./build.sh." >&2; exit 1; }
done

echo "==> [1/5] Installing Wine into a staging prefix"
rm -rf "$STAGE" "$TREE"
make -C "$ROOT/wine" install DESTDIR="$STAGE" >/dev/null
mkdir -p "$TREE"
cp -a "$STAGE/usr/local/." "$TREE/"
rm -rf "$TREE/include" "$TREE/share/man"   # runtime needs neither dev headers nor man pages
# new-WoW64 ships only bin/wine (the 64-bit loader runs both 32- and 64-bit apps); some
# launchers/tools still expect bin/wine64, so provide it as a compat symlink.
ln -sf wine "$TREE/bin/wine64"

echo "==> [2/5] Bundling the Rust unix libraries"
install -m644 "$RUST_DEPS/libwine_ntdll.so" "$RUST_DEPS/libwine_comdlg32.so" "$TREE/$UNIXDIR/"
# Optional tidy-up: rewrite the leftover absolute build-tree RUNPATH to $ORIGIN so
# the libs are self-locating even without the launcher. Best-effort -- the
# launcher's LD_LIBRARY_PATH already makes the tree relocatable, so patchelf is
# not required.
if command -v patchelf >/dev/null 2>&1; then
    for so in ntdll comdlg32; do
        patchelf --set-rpath '$ORIGIN' "$TREE/$UNIXDIR/$so.so" 2>/dev/null || true
    done
fi
# Shrink the tree: strip debug info from ELF objects (the loader, libwine, the
# unixlibs). PE files are left untouched. Best-effort.
if command -v strip >/dev/null 2>&1; then
    find "$TREE/bin" "$TREE/lib" -type f 2>/dev/null | while read -r f; do
        case "$(file -b "$f" 2>/dev/null)" in
            ELF*) strip --strip-unneeded "$f" 2>/dev/null || true ;;
        esac
    done
fi

echo "==> [3/5] Adding the launcher"
install -m755 "$ROOT/packaging/spritzwine" "$TREE/spritzwine"

echo "==> [4/5] Building and bundling the installer"
# The headless CLI (installer/core) has no GUI dependencies, so it is always bundled --
# it guarantees a scriptable install path on any host.
( cd "$ROOT/installer/core" && cargo build --release )
install -m755 "$ROOT/installer/core/target/release/spritzwine-cli-install" "$TREE/spritzwine-cli-install"
# The graphical installer (installer/gui, libcosmic) is bundled unless WITH_INSTALLER=no --
# building it needs the libcosmic system deps (see installer/README.md).
if [ "${WITH_INSTALLER:-yes}" != "no" ]; then
    ( cd "$ROOT/installer/gui" && cargo build --release )
    install -m755 "$ROOT/installer/gui/target/release/spritzwine-setup" "$TREE/spritzwine-setup"
else
    echo "    WITH_INSTALLER=no -> skipping the GUI (headless spritzwine-cli-install still bundled)"
fi
if command -v strip >/dev/null 2>&1; then
    for b in spritzwine-cli-install spritzwine-setup; do
        [ -f "$TREE/$b" ] && strip --strip-unneeded "$TREE/$b" 2>/dev/null || true
    done
fi

# Beisl (the perf tracer) is bundled unless WITH_BEISL=no. The CLI wrapper is always
# built; its setup GUI needs the same libcosmic deps as spritzwine-setup, so it follows
# WITH_INSTALLER. After extracting the tree, `beisl install` puts it on PATH.
if [ "${WITH_BEISL:-yes}" != "no" ] && [ -d "$ROOT/beisl" ]; then
    echo "==> [4b/5] Building and bundling Beisl (perf tracer)"
    ( cd "$ROOT/beisl" && cargo build --release -p beisl )
    install -m755 "$ROOT/beisl/target/release/beisl" "$TREE/beisl"
    if [ "${WITH_INSTALLER:-yes}" != "no" ]; then
        ( cd "$ROOT/beisl/crates/beisl-gui" && cargo build --release )
        install -m755 "$ROOT/beisl/crates/beisl-gui/target/release/beisl-setup" "$TREE/beisl-setup"
    fi
    if command -v strip >/dev/null 2>&1; then
        for b in beisl beisl-setup; do
            [ -f "$TREE/$b" ] && strip --strip-unneeded "$TREE/$b" 2>/dev/null || true
        done
    fi
fi

echo "==> [5/5] Creating the tarball"
TARBALL="$DIST/$NAME-x86_64.tar.xz"
tar -C "$DIST" -caf "$TARBALL" "$NAME"

cat <<EOF

✅ Packaged: $TARBALL  ($(du -sh "$TARBALL" | cut -f1))
   Try it:    tar -C /tmp -xf "$TARBALL" && /tmp/$NAME/spritzwine --version
   Install:   /tmp/$NAME/spritzwine-setup           # graphical installer (Steam/Lutris/prefix)
              /tmp/$NAME/spritzwine-cli-install --help   # headless equivalent
   Perf:      /tmp/$NAME/beisl install              # put beisl on PATH -> add 'beisl %command%' to launch options
EOF
