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
# --force: the warm wine tree carries GENERATED files that step 2 regenerates -- `configure`
# (autoreconf), server_protocol.h (make_requests), the vulkan/syscall thunks (make_vulkan /
# make_specfiles). Some the base tracks, some it doesn't. When the wine PIN moves to a
# different base, a plain `submodule update` cannot check out over them ("would be overwritten
# by checkout" -> fatal). checkout -f overwrites exactly those colliding files and leaves
# unrelated untracked build outputs (*.o) alone, so the warm build survives and step 2
# regenerates what it discarded.
git submodule update --init --recursive --force

echo "==> [2/6] Configuring Wine"
cd "$ROOT/wine"
# Proton-based branches (spritzvalve-*) don't track wine's generated files the way
# vanilla WineHQ does — configure, include/wine/vulkan.h, the syscall headers and
# the wineserver protocol are generated at build time (Valve's build system does the
# same). Each step is a silent no-op on an in-sync vanilla-based tree.
[ -x configure ] || autoreconf -f
[ -f include/wine/vulkan.h ] || (cd dlls/winevulkan && python3 ./make_vulkan)
./tools/make_specfiles syscalls
./tools/make_requests
# The Makefile-vs-configure gate below is tree-internal: it catches a changed TREE, not a
# changed ENVIRONMENT. That bit us: SDL2-devel was added to the CI image, and the warm CI
# tree kept a config.h with no SONAME_LIBSDL2 -- winebus.so failed to link while "skipping
# configure" scrolled past, forever, no matter how often the image was rebuilt. So the
# environment is fingerprinted too: the CI image bakes its stamp at /etc/spritzwine (see
# ci/build-env.Dockerfile), the repo's ci/build-env.sha256 is the fallback signal outside
# a container. A forced configure is cheap when nothing effective changed: config.status
# leaves an identical config.h untouched, so no rebuild cascades.
ENV_STAMP=""
for f in /etc/spritzwine/build-env.sha256 "$ROOT/ci/build-env.sha256"; do
    if [ -f "$f" ]; then ENV_STAMP=$(cat "$f"); break; fi
done
if [ -n "$ENV_STAMP" ] && [ "$(cat .build-env-stamp 2>/dev/null || true)" != "$ENV_STAMP" ]; then
    echo "    build environment changed (or is unstamped) -> forcing configure"
    rm -f Makefile
fi
if [ -f Makefile ] && [ Makefile -nt configure ]; then
    echo "    wine/Makefile is up to date; skipping configure."
else
    echo "    Running configure (graphical: X11 + Wayland + FreeType when dev files present)..."
    # shellcheck disable=SC2086
    ./configure $CONFIGURE_FLAGS
fi
# Record which environment this configure ran in (set -e means we only get here on success).
[ -z "$ENV_STAMP" ] || printf '%s\n' "$ENV_STAMP" > .build-env-stamp

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
# Sony gamepad rumble core for winebus.sys's unix side (hidraw DualShock 4 /
# DualSense force feedback); linked into winebus.so in step 4.
cargo build --release -p wine_winebus
# SDP client (raw L2CAP socket, no libbluetooth) that resolves the RFCOMM channel for
# ntdll's AF_BTH connect path; linked into ntdll.so in step 4, so it must exist first.
cargo build --release -p wine_bluetooth

# The PE-side static libs (no_std, panic=abort) are excluded from the workspace so
# their own profile is honored; each links into its module's PE image in step 4. Under
# new-WoW64 they must be REAL PE -- one build per arch in PE_ARCHS, for the matching
# clang/lld PE target (*-pc-windows-gnullvm). Auto-discovered by the uniform naming:
# every <name>_pe crate builds libwine_<name>_pe.a, linked into dlls/<name>. Two trees
# by module kind: src_dll/*_pe -> user DLLs (e.g. ntdll_pe -> ntdll.dll, process-heap
# backed); src_sys/*_pe -> kernel drivers (e.g. hidparse_pe -> hidparse.sys, kernel-pool
# backed). Add a converted module by creating the crate in the right tree -- no edit here.
for pe_target in i686-pc-windows-gnullvm x86_64-pc-windows-gnullvm; do
    for pe_manifest in src_dll/*_pe/Cargo.toml src_sys/*_pe/Cargo.toml; do
        cargo build --release --manifest-path "$pe_manifest" \
            --target-dir target --target "$pe_target"
    done
done

echo "==> [4/6] Building Wine (headers, dlls, programs; links libwine_ntdll into ntdll)"
cd "$ROOT/wine"
make -j"$JOBS" -C include
# Each PE Rust staticlib is linked into its module via EXTRADLLFLAGS, which makedep
# does NOT track as a prerequisite. Drop a stale PE image when its staticlib is newer,
# so the build below relinks instead of silently keeping the old code (the same gotcha
# relink-ntdll.sh handles for the quick dev loop). The crate <name>_pe (from either
# tree) maps to a Wine module by name alone: a user DLL lives in dlls/<name>/ and
# builds <name>.dll, while a driver/exe lives in dlls/<name>.<ext>/ and builds
# <name>.<ext>. Resolving that by filesystem lookup keeps this kind-agnostic, so a
# future .sys/.exe/.drv crate needs no edit here.
for arch in i386 x86_64; do
    pe=i686; [ "$arch" = x86_64 ] && pe=x86_64
    for pe_dir in "$ROOT"/rust/src_dll/*_pe "$ROOT"/rust/src_sys/*_pe; do
        [ -d "$pe_dir" ] || continue
        name=$(basename "$pe_dir" _pe)
        lib="$ROOT/rust/target/$pe-pc-windows-gnullvm/release/libwine_${name}_pe.a"
        [ -f "$lib" ] || continue
        # dlls/<name> is a plain DLL (module file <name>.dll); otherwise the module
        # dir carries the extension (dlls/<name>.sys, dlls/<name>.exe, ...) and the
        # PE file is named exactly after that dir.
        if [ -d "dlls/$name" ]; then
            out="dlls/$name/$arch-windows/$name.dll"
        else
            moddir=""
            for d in "dlls/$name".*; do [ -d "$d" ] && { moddir=${d#dlls/}; break; }; done
            [ -n "$moddir" ] || continue
            out="dlls/$moddir/$arch-windows/$moddir"
        fi
        if [ -f "$out" ] && [ "$lib" -nt "$out" ]; then
            echo "    PE staticlib ($pe libwine_${name}_pe.a) newer than $out -> forcing relink"
            rm -f "$out"
        fi
    done
done
# NOT `make dlls`: Wine's Makefile has no such target. Its phony targets are per-module
# (dlls/ntdll/all, ...), and `dlls` is merely an existing DIRECTORY -- so `make dlls` prints
# "Nothing to be done for 'dlls'." and silently builds nothing. That went unnoticed for as
# long as this script existed, because on a tree that had been built before, step 5 found the
# ntdll.so an earlier `make` had left behind. On a COLD tree (CI, or a new contributor) it
# does not exist, and step 5 dies linking the Rust `wine` bin: undefined __wine_dbg_output.
#
# So build Wine for real here. Nothing in Wine links wine_api (see step 5), so there is no
# circular dependency and the full build is safe this early; step 6 then finishes in seconds.
make -j"$JOBS"

echo "==> [5/6] Building the Rust FFI library (wine_api) against the fresh headers"
cd "$ROOT/rust"
# NOTE: under new-WoW64 nothing in Wine links wine_api yet -- its only consumer was
# the -mwinerust PE program programs/ping, which can't link an ELF cdylib once it is
# built as real PE. That example is disabled (see configure.ac) pending a Stage 3
# rework (wine_api as a PE library). We still build wine_api so its bindings keep
# compiling against the fresh headers.
cargo build --release

echo "==> [6/6] Finishing the Wine build (a no-op unless step 5 changed something)"
cd "$ROOT/wine"
make -j"$JOBS"

cat <<EOF

✅ Build complete (new-WoW64: 64-bit host, runs 32- and 64-bit Windows apps).
   Wine loader : $ROOT/wine/wine
   Rust libs   : $ROOT/rust/target/x86_64-unknown-linux-gnu/release/deps/  (unix: libwine_ntdll.so, libwine_comdlg32.so, libwine_winebus.so, libwine_bluetooth.so)
                 $ROOT/rust/target/{i686,x86_64}-pc-windows-gnullvm/release/  (PE: libwine_ntdll_pe.a)

Try it:  $ROOT/wine/wine --version
EOF
