#!/usr/bin/env bash
#
# The build-env stamp: a hash of the Dockerfile's FUNCTIONAL content.
#
# Comments and blank lines are stripped before hashing, so a documentation edit does not
# invalidate a multi-GB image on every host that carries it -- two of the three image
# invalidations on this CI's first day were comment-only, and each one turned every lane
# red until a human rebuilt the runner image.
#
# This script is the ONLY definition of the stamp: rebuild.sh writes it, lint.yml checks
# it, the Dockerfile bakes its output. Change the rule here and nowhere else.
#
# Caveat: a line whose first non-blank character is '#' is treated as a comment. Do not
# introduce RUN heredocs whose body contains such lines without revisiting this rule.
set -euo pipefail
cd "$(dirname "$0")/.."
grep -vE '^[[:space:]]*#|^[[:space:]]*$' ci/build-env.Dockerfile | sha256sum | cut -d' ' -f1
