#!/usr/bin/env bash
#
# SpritzWine bootstrap build.
#
# Builds the Rust pieces and the patched Wine in the right order. The ordering
# matters because of a two-way coupling (see README.md "How the build fits
# together"):
#
#   * ntdll links the standalone Rust crate libwine_ntdll  -> build it first
#   * the Rust FFI lib (wine_api) runs bindgen over Wine's *generated* headers
#     -> those headers only exist after Wine's dlls have been built (widl)
#   * Wine's ping example links wine_api                    -> build it last
#
# Override parallelism with JOBS=…, the configure flags with CONFIGURE_FLAGS=…,
# and disable the optional Wayland driver with WITH_WAYLAND=no.
set -euo pipefail

cd "$(dirname "$0")"
ROOT="$(pwd)"
JOBS="${JOBS:-$(nproc)}"

# Wine must be a 32-bit (i386) build so its HOST_ARCH matches the i686 Rust
# target pinned in rust/.cargo/config.toml -- hence --disable-win64. The build is
# otherwise lean/headless; the optional Wayland display driver is auto-enabled
# when its 32-bit dev files are present (see step [2/6]). Adjust for your env.
CONFIGURE_FLAGS="${CONFIGURE_FLAGS:---disable-win64 --without-x --without-freetype}"

# Auto-enable Wine's Wayland driver when the *32-bit* wayland/xkb dev files are
# available. Their pkg-config files live outside the default search path on
# multilib systems (e.g. Fedora's /usr/lib/pkgconfig), so we locate them and
# point pkg-config there for ./configure. Force off with WITH_WAYLAND=no.
WITH_WAYLAND="${WITH_WAYLAND:-auto}"

detect_wayland_pkgconfig() {
    local dir
    for dir in /usr/lib/pkgconfig /usr/lib32/pkgconfig /usr/lib/i386-linux-gnu/pkgconfig; do
        [ -d "$dir" ] || continue
        # --cflags --libs resolves Requires.private too, so this only succeeds
        # when the whole 32-bit closure (libffi, libxml-2.0, liblzma, zlib, …) is
        # installed. Restricting PKG_CONFIG_LIBDIR mirrors Wine's own configure,
        # so it won't false-positive on 64-bit-only .pc files.
        if PKG_CONFIG_LIBDIR="$dir:/usr/share/pkgconfig" pkg-config --cflags --libs \
               wayland-client wayland-egl wayland-cursor xkbcommon xkbregistry \
               >/dev/null 2>&1; then
            echo "$dir"
            return 0
        fi
    done
    return 1
}

echo "==> [1/6] Fetching the Wine submodule"
git submodule update --init --recursive

echo "==> [2/6] Configuring Wine"
cd "$ROOT/wine"
if [ -f Makefile ] && [ Makefile -nt configure ]; then
    echo "    wine/Makefile is up to date; skipping configure."
else
    # Enable the Wayland driver if its 32-bit dev files are present.
    if [ "$WITH_WAYLAND" != "no" ] && command -v pkg-config >/dev/null 2>&1 \
       && wl_pcdir="$(detect_wayland_pkgconfig)"; then
        echo "    Wayland 32-bit dev files found ($wl_pcdir); enabling winewayland.drv."
        export PKG_CONFIG_PATH="$wl_pcdir${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    else
        echo "    No Wayland 32-bit dev files; configuring headless (no display driver)."
    fi
    echo "    Running configure..."
    # shellcheck disable=SC2086
    ./configure $CONFIGURE_FLAGS
fi

echo "==> [3/6] Building the standalone per-dll Rust crates (libwine_ntdll, …)"
# Zero Wine dependencies, so these build first and get linked into ntdll.
# wine_api is deliberately NOT built here: its bindgen needs the headers
# generated in step 4.
cd "$ROOT/rust"
cargo build --release -p wine_ntdll
# Portal client for comdlg32's unix side (host file chooser via XDG desktop
# portal); linked into comdlg32.so in step 4, so it must exist first.
cargo build --release -p wine_comdlg32

# The PE-side static lib (no_std, panic=abort) is excluded from the workspace so
# its own profile is honored; it gets linked into ntdll's PE image (ntdll.dll.so)
# in step 4, so it must exist first.
cargo build --release --manifest-path src_dll/ntdll_pe/Cargo.toml --target-dir target

echo "==> [4/6] Building Wine's dlls and headers (generates headers; links libwine_ntdll into ntdll)"
cd "$ROOT/wine"
make -j"$JOBS" -C include
make -j"$JOBS" dlls

echo "==> [5/6] Building the Rust FFI library (wine_api) against the fresh headers"
cd "$ROOT/rust"
cargo build --release

echo "==> [6/6] Building the rest of Wine (programs + loader)"
cd "$ROOT/wine"
make -j"$JOBS"

cat <<EOF

✅ Build complete.
   Wine loader : $ROOT/wine/wine
   Rust libs   : $ROOT/rust/target/i686-unknown-linux-gnu/release/deps/
                 (libwine_ntdll.so, libwine_api.so)

Try it:  $ROOT/wine/wine --version
EOF
