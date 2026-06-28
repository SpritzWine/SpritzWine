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

# SpritzWine builds Wine in new-WoW64 mode: a 64-bit host (HOST_ARCH=x86_64) that
# also builds 32-bit PE, so it runs both 64- and 32-bit Windows apps. The DLLs are
# real PE (built by clang+lld -- no mingw needed) over a single 64-bit Unix side.
# That is what --enable-archs=i386,x86_64 selects. The Rust crates retarget to match
# (Unix cdylibs -> x86_64 host; PE staticlib -> {i686,x86_64}-pc-windows-gnullvm).
#
# GRAPHICAL by default: configure auto-enables the X11 + Wayland display drivers and
# FreeType font rendering when their (64-bit, host-arch) dev files are present -- which is
# what a build people actually run GUI apps/games with needs. (Without FreeType, Wine has
# no font backend and dialogs collapse to a few pixels.) Under new-WoW64 the drivers are
# host-arch, so the old 32-bit wayland/xkb pkg-config dance is gone. For a lean headless
# build, set CONFIGURE_FLAGS to add --without-x --without-freetype.
CONFIGURE_FLAGS="${CONFIGURE_FLAGS:---enable-archs=i386,x86_64}"

echo "==> [1/6] Fetching the Wine submodule"
git submodule update --init --recursive

echo "==> [2/6] Configuring Wine"
cd "$ROOT/wine"
if [ -f Makefile ] && [ Makefile -nt configure ]; then
    echo "    wine/Makefile is up to date; skipping configure."
else
    echo "    Running configure (graphical: X11 + Wayland + FreeType when dev files present)..."
    # shellcheck disable=SC2086
    ./configure $CONFIGURE_FLAGS
fi

echo "==> [3/6] Building the standalone per-dll Rust crates (libwine_ntdll, …)"
# Zero Wine dependencies, so these build first and get linked into ntdll.
# wine_api is deliberately NOT built here: its bindgen needs the headers
# generated in step 4.
cd "$ROOT/rust"
# Unix-side cdylibs -> the 64-bit unixlibs. .cargo/config.toml pins the host triple
# (x86_64-unknown-linux-gnu) so the output lands where Wine's RUST_TARGET paths look.
cargo build --release -p wine_ntdll
# Portal client for comdlg32's unix side (host file chooser via XDG desktop
# portal); linked into comdlg32.so in step 4, so it must exist first.
cargo build --release -p wine_comdlg32

# The PE-side static libs (no_std, panic=abort) are excluded from the workspace so
# their own profile is honored; each links into its dll's PE image in step 4. Under
# new-WoW64 they must be REAL PE -- one build per arch in PE_ARCHS, for the matching
# clang/lld PE target (*-pc-windows-gnullvm). Auto-discovered by the uniform naming:
# every rust/src_dll/<dll>_pe builds libwine_<dll>_pe.a, linked into dlls/<dll> (e.g.
# ntdll_pe -> ntdll.dll, avrt_pe -> avrt.dll). Add a converted module by creating the
# crate -- no edit here.
for pe_target in i686-pc-windows-gnullvm x86_64-pc-windows-gnullvm; do
    for pe_manifest in src_dll/*_pe/Cargo.toml; do
        cargo build --release --manifest-path "$pe_manifest" \
            --target-dir target --target "$pe_target"
    done
done

echo "==> [4/6] Building Wine's dlls and headers (generates headers; links libwine_ntdll into ntdll)"
cd "$ROOT/wine"
make -j"$JOBS" -C include
# Each PE Rust staticlib is linked into its dll via EXTRADLLFLAGS, which makedep does
# NOT track as a dll prerequisite. Drop a stale dll when its staticlib is newer, so
# `make dlls` relinks instead of silently keeping the old code (the same gotcha
# relink-ntdll.sh handles for the quick dev loop). Auto-discovered from the crate set:
# rust/src_dll/<dll>_pe -> libwine_<dll>_pe.a -> dlls/<dll>/<arch>-windows/<dll>.dll.
for arch in i386 x86_64; do
    pe=i686; [ "$arch" = x86_64 ] && pe=x86_64
    for pe_dir in "$ROOT"/rust/src_dll/*_pe; do
        dll=$(basename "$pe_dir" _pe)
        lib="$ROOT/rust/target/$pe-pc-windows-gnullvm/release/libwine_${dll}_pe.a"
        out="dlls/$dll/$arch-windows/$dll.dll"
        if [ -f "$lib" ] && [ -f "$out" ] && [ "$lib" -nt "$out" ]; then
            echo "    PE staticlib ($pe libwine_${dll}_pe.a) newer than $out -> forcing relink"
            rm -f "$out"
        fi
    done
done
make -j"$JOBS" dlls

echo "==> [5/6] Building the Rust FFI library (wine_api) against the fresh headers"
cd "$ROOT/rust"
# NOTE: under new-WoW64 nothing in Wine links wine_api yet -- its only consumer was
# the -mwinerust PE program programs/ping, which can't link an ELF cdylib once it is
# built as real PE. That example is disabled (see configure.ac) pending a Stage 3
# rework (wine_api as a PE library). We still build wine_api so its bindings keep
# compiling against the fresh headers.
cargo build --release

echo "==> [6/6] Building the rest of Wine (programs + loader)"
cd "$ROOT/wine"
make -j"$JOBS"

cat <<EOF

✅ Build complete (new-WoW64: 64-bit host, runs 32- and 64-bit Windows apps).
   Wine loader : $ROOT/wine/wine
   Rust libs   : $ROOT/rust/target/x86_64-unknown-linux-gnu/release/deps/  (unix: libwine_ntdll.so, libwine_comdlg32.so)
                 $ROOT/rust/target/{i686,x86_64}-pc-windows-gnullvm/release/  (PE: libwine_ntdll_pe.a)

Try it:  $ROOT/wine/wine --version
EOF
