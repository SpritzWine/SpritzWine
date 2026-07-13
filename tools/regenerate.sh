#!/usr/bin/env bash
# Regenerate a spritz-* wine consumer branch by replaying soda tiers onto a wine base.
# Consumers are GENERATED (force-updated), never hand-edited — author only in soda-*.
#
# Idempotent + CI-friendly: seeds git-rerere from the repo-tracked cache next to this
# script (tools/rr-cache in the SpritzWine root repo), so a runner reuses every stitch
# resolution already made, and writes new ones back.
#
#   tools/regenerate.sh spritzvalve     valve-wine/proton_11.0   soda-branding
#   tools/regenerate.sh spritzwine-gen  winehq/wine-11.12        soda-ntdll
#
# Args: <output-branch> <wine-base-ref> <soda-tip-ref> [<soda-home-base>]
#
# <soda-home-base> is the commit the soda tiers are authored on (exclusive start of
# the replayed range). It is NOT derivable via `git merge-base <wine-base> <soda-tip>`:
# for a cross-base regenerate (e.g. 11.12-authored tiers onto proton_11.0) that
# merge-base is the common wine ancestor, and using it would replay thousands of
# upstream commits. Default = the true fork base, parent of the first fschaupp commit.
set -euo pipefail

OUT_BRANCH="${1:?output branch, e.g. spritzvalve}"
BASE_REF="${2:?wine base ref, e.g. valve-wine/proton_11.0}"
SODA_REF="${3:?soda tip ref, e.g. soda-branding}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TRACKED_CACHE="$SCRIPT_DIR/rr-cache"          # committed to the SpritzWine root repo
WINE_DIR="$SCRIPT_DIR/../wine"
cd "$WINE_DIR"

SODA_HOME_BASE="${4:-$(git rev-parse 6260d89930c^)}"   # wine-11.12-79-g1bb0da91a5e
GIT_CACHE="$(git rev-parse --git-path rr-cache)"

# 1. Turn rerere on and seed it from the shared, tracked cache.
git config rerere.enabled true
git config rerere.autoupdate true              # stage replayed resolutions automatically
mkdir -p "$TRACKED_CACHE" "$GIT_CACHE"
cp -a "$TRACKED_CACHE/." "$GIT_CACHE/" 2>/dev/null || true

persist_cache() { cp -a "$GIT_CACHE/." "$TRACKED_CACHE/" 2>/dev/null || true; }

# 2. Point the output branch at the soda tip, then rebase THAT branch onto the wine
#    base. Never rebase the soda ref itself — tiers stay authored on their home base.
git checkout -B "$OUT_BRANCH" "$SODA_REF"
if ! git rebase --onto "$BASE_REF" "$SODA_HOME_BASE" "$OUT_BRANCH"; then
    echo
    echo ">> Unresolved conflict rerere did not know. Resolve it, 'git rebase --continue',"
    echo ">> then re-run this script to capture the new resolution into $TRACKED_CACHE."
    persist_cache   # share partial progress even on a stop
    exit 1
fi

# 3. Success: copy any newly-learned resolutions back into the tracked cache.
persist_cache
if ! git -C "$SCRIPT_DIR/.." diff --quiet -- tools/rr-cache 2>/dev/null; then
    echo ">> New rerere resolutions captured in tools/rr-cache — commit them to share with CI."
fi
echo ">> Regenerated $OUT_BRANCH = $BASE_REF + ($SODA_HOME_BASE..$SODA_REF)"
