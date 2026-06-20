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
# Override parallelism with JOBS=… and the configure flags with CONFIGURE_FLAGS=…
set -euo pipefail

cd "$(dirname "$0")"
ROOT="$(pwd)"
JOBS="${JOBS:-$(nproc)}"

# Wine must be a 32-bit (i386) build so its HOST_ARCH matches the i686 Rust
# target pinned in rust/.cargo/config.toml -- hence --disable-win64. The other
# flags just keep the build lean/headless; adjust them for your environment.
CONFIGURE_FLAGS="${CONFIGURE_FLAGS:---disable-win64 --without-x --without-freetype}"

echo "==> [1/6] Fetching the Wine submodule"
git submodule update --init --recursive

echo "==> [2/6] Configuring Wine"
cd "$ROOT/wine"
if [ -f Makefile ] && [ Makefile -nt configure ]; then
    echo "    wine/Makefile is up to date; skipping configure."
else
    echo "    wine/Makefile is stale or missing; running configure..."
    # shellcheck disable=SC2086
    ./configure $CONFIGURE_FLAGS
fi

echo "==> [3/6] Building the standalone per-dll Rust crates (libwine_ntdll, …)"
# Zero Wine dependencies, so these build first and get linked into ntdll.
# wine_api is deliberately NOT built here: its bindgen needs the headers
# generated in step 4.
cd "$ROOT/rust"
cargo build --release -p wine_ntdll

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
