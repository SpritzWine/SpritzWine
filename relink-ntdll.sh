#!/usr/bin/env bash
#
# Rebuild the PE-side Rust ntdll staticlib and force-relink ntdll against it.
#
# Why this exists: libwine_ntdll_pe.a is linked into ntdll via EXTRADLLFLAGS,
# which Wine's makedep does NOT track as a prerequisite of ntdll.dll. So after a
# Rust-only edit under rust/src_dll/ntdll_pe, a bare `make` sees no changed .o and
# silently keeps the STALE ntdll.dll -- you end up testing the old code. This does
# the correct sequence: rebuild the staticlib, drop the stale ntdll.dll, relink.
#
# Use it after editing the PE Rust crate; then run your test against wine/ as usual.
# (build.sh does the equivalent stamp-check for full builds.)
set -euo pipefail

cd "$(dirname "$0")"
ROOT="$(pwd)"
JOBS="${JOBS:-$(nproc)}"

echo "==> Rebuilding the PE Rust staticlib (libwine_ntdll_pe.a, both arches)"
cd "$ROOT/rust"
for pe_target in i686-pc-windows-gnullvm x86_64-pc-windows-gnullvm; do
    cargo build --release --manifest-path src_dll/ntdll_pe/Cargo.toml \
        --target-dir target --target "$pe_target"
done

echo "==> Force-relinking ntdll against the fresh staticlib"
cd "$ROOT/wine"
for arch in i386 x86_64; do
    dll="dlls/ntdll/$arch-windows/ntdll.dll"
    rm -f "$dll"
    make -j"$JOBS" "$dll"
done

echo "✅ ntdll relinked against the current Rust staticlib."
