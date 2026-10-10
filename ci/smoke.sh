#!/usr/bin/env bash
#
# Smoke gate for a built + packaged SpritzWine tree.
#
# Why this exists, in one sentence: a green Wine build proves almost nothing.
#
# Wine's configure SILENTLY disables any backend whose dev headers are absent -- no warning,
# no error, just a Wine without that driver. The CI lane this replaced installed no
# libX11-devel at all, so it would have published a release tarball with NO X11 driver and
# said nothing. Meanwhile the author's own machine is missing alsa/pulse and wayland dev
# files, so the Wine built there has no sound and no Wayland driver -- also silently.
#
# So step 1 does not test Wine; it tests what Wine DECIDED, and fails if a load-bearing
# backend went missing. The rest checks the thing that actually ships: the tarball.
#
#   1. backends -- wine/include/config.h: required backends present (else FAIL), optional
#                  ones reported (so a downgrade is at least visible in the log).
#   2. shape    -- new-WoW64 invariants in the packaged tree: a 64-bit unix side, NO native
#                  32-bit unix .so, both PE arches, the Rust unix libs actually bundled.
#   3. boot     -- `wineboot -u` in a fresh prefix, under a timeout.
#   4. bitness  -- run a real PE in BOTH bitnesses (system32 + syswow64 cmd.exe) and check
#                  the exit code. Exit code, not output, on purpose: with std handles on a
#                  regular file this Wine drops a PE process's stdout AND stderr entirely,
#                  and only the exit status survives.
#
# Usage (from the repo root, after ./build.sh && ./packaging/package.sh):
#     ./ci/smoke.sh
#
# Knobs: SMOKE_TIMEOUT=300  KEEP=1 (keep the prefix + the extracted tree)
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
ROOT="$(pwd)"
SMOKE_TIMEOUT="${SMOKE_TIMEOUT:-300}"
FAILED=0

ok()   { printf '  \033[32mok\033[0m    %s\n' "$*"; }
warn() { printf '  \033[33mwarn\033[0m  %s\n' "$*"; }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAILED=$((FAILED + 1)); }
hdr()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# ─────────────────────────────────────────────────────────────────────────────────────
hdr "1. backends (what configure decided)"

CONFIG_H="$ROOT/wine/include/config.h"
if [ ! -f "$CONFIG_H" ]; then
    bad "wine/include/config.h not found -- run ./build.sh first"
else
    # SONAME_LIB* means Wine found the headers and will dlopen the library at runtime.
    # Required: a Wine without these is not the Wine this project ships.
    #   X11        -- the display driver. Its absence is the exact bug this gate exists for.
    #   FREETYPE   -- no font backend => dialogs collapse to a few pixels.
    #   FONTCONFIG -- no font discovery.
    #   VULKAN     -- the whole point downstream (DXVK/vkd3d in Prater, games here).
    for lib in LIBX11 LIBFREETYPE LIBFONTCONFIG LIBVULKAN; do
        if grep -qa "define SONAME_$lib " "$CONFIG_H"; then
            ok "$lib"
        else
            bad "$lib is MISSING -- configure silently disabled it (dev headers absent in the build image?)"
        fi
    done
    # Recommended: not fatal (a headless or minimal host may legitimately lack them).
    # ONLY dlopen'd libs can appear in this list -- wayland/alsa/pulse never have a SONAME_*
    # macro in any build (wine links them directly) and are judged in step 2 by the shipped
    # driver .so instead. SDL2 is reported rather than required only because a build without
    # it does not get this far: winebus.sys calls is_sdl_ignored_device() unconditionally but
    # defines it under #ifdef SONAME_LIBSDL2, so winebus.so fails to link outright.
    for lib in LIBXRANDR LIBXRENDER LIBXI LIBXFIXES LIBGL LIBEGL LIBSDL2; do
        if grep -qa "define SONAME_$lib " "$CONFIG_H"; then ok "$lib"; else warn "$lib absent"; fi
    done
    # Bluetooth has no SONAME on trees without the SDP lookup: wineserver compiles its
    # AF_BTH/RFCOMM support under the header macro alone, so that is what is judged.
    if grep -qa "define HAVE_BLUETOOTH_RFCOMM_H " "$CONFIG_H"; then
        ok "bluetooth (RFCOMM sockets)"
    else
        warn "bluetooth absent -- socket(AF_BTH) will fail (bluez-libs-devel missing from the build image?)"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────────────
hdr "2. shape (the packaged tree)"

# package.sh leaves both the tree and the tarball in dist/. The tree we gate MUST be the
# one this run packaged: dist/ accumulates version-named trees in a warm work tree
# (package.sh removes only its own), and an arbitrary pick can smoke last week's green
# tarball while today's is broken. So derive the name exactly the way package.sh does
# (VERSION, or git describe), and only fall back to the newest tree by mtime -- loudly.
EXTRACTED=""
VERSION="${VERSION:-$(git -C "$ROOT" describe --tags --always 2>/dev/null || true)}"
if [ -n "$VERSION" ] && [ -d "$ROOT/dist/spritzwine-$VERSION" ]; then
    TREE="$ROOT/dist/spritzwine-$VERSION"
else
    TREE="$(find "$ROOT/dist" -maxdepth 1 -type d -name 'spritzwine-*' -printf '%T@ %p\n' 2>/dev/null \
            | sort -rn | head -1 | cut -d' ' -f2-)"
    [ -z "$TREE" ] || warn "no dist tree matches this checkout (spritzwine-${VERSION:-?}); gating the NEWEST instead: $TREE"
fi
if [ -z "$TREE" ]; then
    TARBALL="$(find "$ROOT/dist" -maxdepth 1 -name 'spritzwine-*.tar.xz' -printf '%T@ %p\n' 2>/dev/null \
               | sort -rn | head -1 | cut -d' ' -f2-)"
    if [ -n "$TARBALL" ]; then
        EXTRACTED="$(mktemp -d)"
        tar -C "$EXTRACTED" -xf "$TARBALL"
        TREE="$(find "$EXTRACTED" -maxdepth 1 -type d -name 'spritzwine-*' | head -1)"
    fi
fi

if [ -z "$TREE" ] || [ ! -d "$TREE" ]; then
    bad "no packaged tree or tarball in dist/ -- run ./packaging/package.sh"
    echo; echo "smoke: $FAILED failure(s)"; exit 1
fi
echo "  tree: $TREE"

if [ -x "$TREE/bin/wine" ]; then ok "bin/wine"; else bad "bin/wine missing or not executable"; fi

# new-WoW64's defining invariant: ONE 64-bit unix side, and no native 32-bit unix side at
# all. 32-bit Windows code runs as real PE on top of the 64-bit unix layer. If a native
# i386-unix .so ever appears here, the build silently fell back to a classic WoW64 layout
# and the project's standing architectural bet has quietly been lost.
if [ -d "$TREE/lib/wine/i386-unix" ]; then
    n_so=$(find "$TREE/lib/wine/i386-unix" -name '*.so' | wc -l)
    if [ "$n_so" -eq 0 ]; then
        ok "no native 32-bit unix .so (new-WoW64)"
    else
        bad "$n_so native 32-bit unix .so present -- this is NOT new-WoW64"
    fi
else
    ok "no i386-unix dir at all (new-WoW64)"
fi
if [ -n "$(find "$TREE/lib/wine/x86_64-unix" -name '*.so' 2>/dev/null | head -1)" ]; then
    ok "64-bit unix side present"
else
    bad "no x86_64-unix .so -- where is the unix side?"
fi

for arch in i386-windows x86_64-windows; do
    if [ -n "$(find "$TREE/lib/wine/$arch" -name '*.dll' 2>/dev/null | head -1)" ]; then
        ok "PE side: $arch"
    else
        bad "PE side missing: $arch"
    fi
done

# The project's whole thesis is Rust inside Wine. If the crates silently stopped being
# linked, everything above would still pass -- so check they are actually in the tarball.
for so in libwine_ntdll.so libwine_comdlg32.so; do
    if [ -f "$TREE/lib/wine/x86_64-unix/$so" ]; then
        ok "rust: $so bundled"
    else
        bad "rust: $so NOT bundled -- did the crates get linked at all?"
    fi
done

# Directly-linked backends: wine builds each driver's unix .so exactly when configure found
# its backend, so the shipped file is the ground truth (config.h has no SONAME_* for these).
for so in winex11.so winevulkan.so winewayland.so; do
    if [ -f "$TREE/lib/wine/x86_64-unix/$so" ]; then
        ok "driver: $so"
    else
        bad "driver $so NOT shipped -- configure did not find its backend (image regression?)"
    fi
done
alsa=""; pulse=""
[ -f "$TREE/lib/wine/x86_64-unix/winealsa.so" ]  && alsa=1
[ -f "$TREE/lib/wine/x86_64-unix/winepulse.so" ] && pulse=1
if [ -n "$alsa" ] && [ -n "$pulse" ]; then
    ok "audio: winealsa.so + winepulse.so"
elif [ -n "$alsa$pulse" ]; then
    warn "audio: only one of winealsa/winepulse shipped"
else
    bad "NO audio driver shipped (neither winealsa.so nor winepulse.so)"
fi

for f in spritzwine spritzwine-cli-install; do
    if [ -x "$TREE/$f" ]; then ok "$f"; else bad "$f missing from the tarball"; fi
done

# ─────────────────────────────────────────────────────────────────────────────────────
hdr "3. boot (fresh prefix)"

PFX="$(mktemp -d)/pfx"
export WINEPREFIX="$PFX"
export WINEDEBUG="${WINEDEBUG:--all}"
# Do not let a missing mono/gecko pop a downloader dialog and hang a headless job.
export WINEDLLOVERRIDES="mscoree,mshtml="

# Invoked indirectly, by the trap below. Different shellcheck versions report this
# differently -- SC2329 ("never invoked") on the function, SC2317 ("unreachable") on its
# body lines -- so silence both, or the lane goes red on whichever version it runs.
# shellcheck disable=SC2329,SC2317
cleanup() {
    "$TREE/bin/wineserver" -k 2>/dev/null || true
    if [ "${KEEP:-0}" = "1" ]; then
        echo "  KEEP=1: prefix left at $PFX"
    else
        rm -rf "$(dirname "$PFX")"
        if [ -n "$EXTRACTED" ]; then rm -rf "$EXTRACTED"; fi
    fi
}
trap cleanup EXIT

if timeout "$SMOKE_TIMEOUT" "$TREE/bin/wine" wineboot -u >/dev/null 2>&1; then
    ok "wineboot -u"
else
    rc=$?
    if [ $rc -eq 124 ]; then
        bad "wineboot TIMED OUT after ${SMOKE_TIMEOUT}s"
    else
        bad "wineboot failed (exit $rc)"
    fi
fi
"$TREE/bin/wineserver" -w 2>/dev/null || true

# ─────────────────────────────────────────────────────────────────────────────────────
hdr "4. bitness (a real PE in each)"

# cmd.exe is a builtin PE and exists in both trees, so this needs no fixture. `exit N` with
# a distinctive N proves the process really ran and really returned -- and the exit code is
# the ONE thing that survives Wine's std-handle handling for PE processes.
run_bitness() {
    local label="$1" exe="$2" want="$3"
    timeout "$SMOKE_TIMEOUT" "$TREE/bin/wine" "$exe" /c "exit $want" >/dev/null 2>&1
    local rc=$?
    if [ "$rc" -eq "$want" ]; then
        ok "$label: cmd.exe ran and returned $want"
    elif [ "$rc" -eq 124 ]; then
        bad "$label: TIMED OUT after ${SMOKE_TIMEOUT}s -- deadlock?"
    else
        bad "$label: expected exit $want, got $rc"
    fi
}
run_bitness "64-bit PE" 'C:\windows\system32\cmd.exe' 42
run_bitness "32-bit PE" 'C:\windows\syswow64\cmd.exe' 43

# ─────────────────────────────────────────────────────────────────────────────────────
echo
if [ "$FAILED" -eq 0 ]; then
    printf '\033[32msmoke: PASS\033[0m\n'
    exit 0
fi
printf '\033[31msmoke: %d failure(s)\033[0m\n' "$FAILED"
exit 1
