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

echo "==> [1/4] Installing Wine into a staging prefix"
rm -rf "$STAGE" "$TREE"
make -C "$ROOT/wine" install DESTDIR="$STAGE" >/dev/null
mkdir -p "$TREE"
cp -a "$STAGE/usr/local/." "$TREE/"
rm -rf "$TREE/include" "$TREE/share/man"   # runtime needs neither dev headers nor man pages

echo "==> [2/4] Bundling the Rust unix libraries"
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

echo "==> [3/4] Adding the launcher"
install -m755 "$ROOT/packaging/spritzwine" "$TREE/spritzwine"

echo "==> [4/4] Creating the tarball"
TARBALL="$DIST/$NAME-x86_64.tar.xz"
tar -C "$DIST" -caf "$TARBALL" "$NAME"

cat <<EOF

✅ Packaged: $TARBALL  ($(du -sh "$TARBALL" | cut -f1))
   Try it:  tar -C /tmp -xf "$TARBALL" && /tmp/$NAME/spritzwine --version
EOF
