#!/usr/bin/env bash
#
# Re-stamp, rebuild and verify the CI build image -- in the one order that works.
#
# The stamp (ci/build-env.sha256) is baked INTO the image as its last layer, so it has to
# be regenerated BEFORE the build or the image carries a stamp describing the previous
# Dockerfile and every CI job fails its image assert. Doing it by hand in the right order
# every time is exactly the kind of thing scripts are for.
#
# Run it on the runner host after pulling, and on a dev box after editing the Dockerfile:
#     ./ci/rebuild.sh
set -euo pipefail
cd "$(dirname "$0")/.."

TAG=spritzwine-build:fedora44
STAMP=ci/build-env.sha256

old=$(cat "$STAMP" 2>/dev/null || echo "(none)")
./ci/stamp.sh > "$STAMP"
new=$(cat "$STAMP")
if [ "$old" != "$new" ]; then
    echo "stamp: $old -> $new"
    echo "NOTE: the Dockerfile changed relative to the recorded stamp. Commit $STAMP"
    echo "      TOGETHER with the Dockerfile change, or lint.yml goes red on the mismatch."
else
    echo "stamp unchanged: $new"
fi

docker build -t "$TAG" -f ci/build-env.Dockerfile ci/

# Trust nothing: read the stamp back out of the image we just built.
baked=$(docker run --rm "$TAG" cat /etc/spritzwine/build-env.sha256)
if [ "$baked" != "$new" ]; then
    echo "BUG: the image carries stamp $baked but the repo says $new -- the bake is broken." >&2
    exit 1
fi
echo "ok: $TAG carries stamp $baked"

# Each rebuild orphans the previous image's unique layers as dangling <none> images; a few
# rebuilds quietly eat the host's disk. Prune ONLY dangling images -- tagged ones (this tag,
# the Prater image, unrelated tags) are never touched.
docker image prune -f >/dev/null
echo "pruned dangling layers."
