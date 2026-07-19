#!/usr/bin/env bash
# Build a micro-repo carrying the whole SpritzWine release topology in ~20
# commits: a tier DAG with one deliberately stale child, a stitch built by
# cherry-pick, a second stitch that is genuinely behind, two consumers built by
# `rebase --onto` (one pinned-and-tagged, one pinned-and-not), an unmodelled
# branch, and a tier declared in the manifest with no branch behind it.
#
# WHY this exists alongside the FakeGit unit tests: a fake can encode a wrong
# mental model just as easily as the code can. The signature bug of this domain
# -- reasoning about a replayed artifact with ancestry -- is only *proven* absent
# against real cherry-picks and real rebases, where the commits genuinely have
# fresh oids and genuinely are nobody's ancestor. This script is what makes that
# claim checkable; `reconcile_real.rs` is what checks it.
#
# Cheap enough (<1s) to rebuild before every run. Deterministic: fixed identity
# and timestamps, so oids are stable and snapshot tests stay possible.
#
# Usage: mkfixture.sh [dir]     (default: tests/fixtures/micro, wiped first)
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
dir=${1:-$here/fixtures/micro}

# Refuse to wipe anything that isn't ours. This script runs `rm -rf` and lives
# next to a 6.5 GB repo that must never be touched.
case $(basename -- "$dir") in
  ''|.|..|/) echo "mkfixture: refusing to build in '$dir'" >&2; exit 1 ;;
esac
if [ -e "$dir" ] && [ ! -e "$dir/.sodactl-fixture" ]; then
  echo "mkfixture: '$dir' exists and is not a fixture dir; refusing to wipe" >&2
  exit 1
fi

rm -rf -- "$dir"
mkdir -p -- "$dir"
touch "$dir/.sodactl-fixture"

wine=$dir/wine
remote=$dir/remote.git

export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@invalid
export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@invalid
export GIT_AUTHOR_DATE='2026-01-01T00:00:00+0000'
export GIT_COMMITTER_DATE='2026-01-01T00:00:00+0000'

g() { git -C "$wine" "$@"; }

# One commit touching one file. Tiers touch disjoint files on purpose: every
# replay in this fixture must apply cleanly, because conflict handling is a
# separate concern with its own tests and a wedged rebase here would look like
# a reconciler failure.
commit() { printf '%s\n' "$2" >>"$wine/$1"; g add -- "$1"; g commit -qm "$2"; }

# `cherry-pick` has no -q; it is chatty on success and must stay silent here.
pick() { g cherry-pick --no-edit "$1" >/dev/null; }

git init -q -b master "$wine"
git -C "$wine" config commit.gpgsign false

# ---- the wine bases -------------------------------------------------------
# W0 is the fork point: the soda tiers are authored on it, and it is the
# exclusive start of every replayed range. That is `home_base` in the manifest
# -- the one constant that genuinely cannot be derived.
commit wine.c 'wine: fork point'
home_base=$(g rev-parse HEAD)

commit wine.c 'wine: upstream moves on'
commit wine.c 'wine: upstream moves on again'
# `master` and `proton_11.0` are named to match what the reconciler skips as
# upstream/vendor lines, so they do not pollute the unmodelled-branch check.
g branch proton_11.0 "$home_base"
g checkout -q proton_11.0
commit valve.c 'valve: proton delta'

# ---- tiers ----------------------------------------------------------------
g checkout -q -b soda-build "$home_base"
commit build.sh 'soda-build: build plumbing'

g checkout -q -b soda-core soda-build
commit core.c 'soda-core: first'
old_core=$(g rev-parse HEAD)          # captured BEFORE core moves
commit core.c 'soda-core: second'

# THE stale child: authored on the old core tip and never restacked. This is
# the live defect in the real repo (four children behind 79e58cb8a70).
g checkout -q -b soda-glorious "$old_core"
commit glorious.c 'soda-glorious: only commit'

# A correctly restacked sibling, as the control.
g checkout -q -b soda-ntdll soda-core
commit ntdll.c 'soda-ntdll: only commit'

# Not in any stitch; exists purely to carry the unpushed-commit case without
# perturbing stitch freshness.
g checkout -q -b soda-demo soda-build
commit demo.c 'soda-demo: first'

# ---- stitches: built by CHERRY-PICK, so nothing here is an ancestor -------
g checkout -q -B soda-full soda-core
pick soda-glorious
pick soda-ntdll

# Stitched before soda-ntdll existed. Genuinely behind -- and the only way to
# tell it apart from soda-full is patch-id, since neither one descends from
# either tier.
g checkout -q -B soda-stale-full soda-core
pick soda-glorious

# ---- consumers: built by rebase --onto, force-pushed, never hand-edited ---
# `regenerate.sh`'s move. The exclusive limit is home_base, NOT
# merge-base(base, soda-tip): the latter would replay the whole upstream delta.
g checkout -q -B spritzwine soda-full
g rebase -q --onto master "$home_base" spritzwine

g checkout -q -B spritzvalve soda-full
g rebase -q --onto proton_11.0 "$home_base" spritzvalve
# Tagged -- the negative control for PINNED_CONSUMER_UNTAGGED. spritzwine is
# deliberately left untagged, and is the one the superproject pins.
g tag spritzvalve-11.0-1 spritzvalve
spritzwine_tip=$(g rev-parse spritzwine)

# An abandoned branch nobody modelled. Dead or live-but-undocumented is exactly
# the question the finding exists to force.
g branch spritzwine-backup-20260703 soda-core

# ---- remote ---------------------------------------------------------------
git init -q --bare "$remote"
g remote add codeberg "$remote"
g push -q codeberg soda-build soda-core soda-glorious soda-ntdll soda-demo
# ...and then one tier gains a commit that never got pushed. soda-ntdll's
# tracking ref is deleted rather than never created, so the fixture covers
# "remote configured but ref absent" and not just "no remote at all".
g update-ref -d refs/remotes/codeberg/soda-ntdll
g checkout -q soda-demo
commit demo.c 'soda-demo: second, unpushed'

g checkout -q master

# ---- the SUPERPROJECT -----------------------------------------------------
# Without this the fixture cannot exercise the pin checks at all: a pin is a
# fact about the superproject's GITLINK, not about any branch in here, so a
# fixture with no superproject makes every pin check answer "unknown" while the
# tests still look like they are asserting something.
#
# The gitlink is written with `update-index --cacheinfo` rather than
# `submodule add`, which would want to clone. A mode-160000 stage-0 entry is
# exactly what a submodule pin IS; the clone machinery is not under test.
#
# Pinned at `spritzwine`, which is deliberately untagged -- the live defect in
# the real repo and the whole reason PINNED_CONSUMER_UNTAGGED exists. There is
# only ever ONE gitlink, so only one consumer can be pinned; the manifest below
# marks spritzvalve unpinned rather than pretending two pointers exist.
git init -q -b main "$dir"
git -C "$dir" config commit.gpgsign false
git -C "$dir" update-index --add --cacheinfo "160000,$spritzwine_tip,wine"
cat >"$dir/.gitmodules" <<GITMODULES
[submodule "wine"]
	path = wine
	url = $remote
	branch = spritzwine
GITMODULES
git -C "$dir" add .gitmodules .sodactl-fixture
git -C "$dir" commit -qm 'fixture: pin wine at the spritzwine pour'

# ---- manifest -------------------------------------------------------------
# Mirrors tools/sodactl.toml in shape. `soda-missing` is declared with nothing
# behind it, because "the manifest names a branch that does not exist" has to be
# a finding rather than a crash.
mkdir -p "$dir/tools"
cat >"$dir/tools/sodactl.toml" <<TOML
# GENERATED by tests/mkfixture.sh -- do not edit, rebuild.
schema = 1
build_artifacts = [".build-env-stamp"]

[repo]
superproject = "."
wine = "wine"

[repo.home_base]
commit = "$home_base"
describes = "fixture-fork-point"
derivation = "tests/mkfixture.sh"

[remotes]
codeberg = "$remote"

[[tier]]
name = "soda-build"
parent = "\$home_base"
remote = "codeberg"

[[tier]]
name = "soda-core"
parent = "soda-build"
remote = "codeberg"

[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-demo"
parent = "soda-build"
remote = "codeberg"

[[tier]]
name = "soda-missing"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll"]
local_only = true

[[stitch]]
name = "soda-stale-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll"]
local_only = true

[[consumer]]
name = "spritzwine"
base = "master"
soda_tip = "soda-full"
pinned_by_superproject = true

[[consumer]]
name = "spritzvalve"
base = "proton_11.0"
soda_tip = "soda-full"
pinned_by_superproject = false
# Declared, where it used to be inherited. While excludes were a global list
# keyed by consumer NAME with a hardcoded default, this fixture's spritzvalve --
# named for the topology it mirrors, not for the live repo's policy -- silently
# picked up the REAL repo's exclusions and raised a CONSUMER_EXCLUDE_VIOLATION
# nothing here had asked for. It happened to be the finding we wanted, which is
# exactly the hazard: the fixture was passing on someone else's policy. Now it
# asks. soda-fsync does not exist in this micro-repo and is skipped;
# soda-glorious is in soda-full and so is genuinely carried, which is the
# finding.
exclude = ["soda-fsync", "soda-glorious"]
TOML

echo "fixture: $dir  (home_base $home_base)"
