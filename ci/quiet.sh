#!/usr/bin/env bash
#
# Run a long build quietly.
#
# Wine's build prints one compiler invocation per file -- thousands of lines -- which buries
# the only thing anyone reads a CI log for: what broke. So the full output goes to a log file
# and stdout gets three things instead:
#
#   * a heartbeat (elapsed time + the build's own "==> [n/6]" phase), because a 30-minute
#     step that prints nothing is indistinguishable from a hung one;
#   * the phase markers, on completion;
#   * on FAILURE, the tail of the log -- where the error actually is.
#
# The full log stays on the runner's cache disk, so it can be read after the fact.
#
# Usage:  ci/quiet.sh <logfile> <command> [args...]
set -uo pipefail

log="${1:?usage: quiet.sh <logfile> <command> [args...]}"
shift
mkdir -p "$(dirname "$log")"
: > "$log"

"$@" >"$log" 2>&1 &
pid=$!

# Heartbeat. Report the build's own phase marker, so the line means something.
#
# Poll often but PRINT rarely: sleeping the whole heartbeat interval would keep waiting long
# after the build had already finished (and would add that dead time to every fast build).
start=$SECONDS
tick=0
while kill -0 "$pid" 2>/dev/null; do
    sleep 2
    tick=$(( tick + 2 ))
    [ $(( tick % 60 )) -eq 0 ] || continue
    kill -0 "$pid" 2>/dev/null || break
    phase=$(grep -E '^==> ' "$log" | tail -1)
    printf '    ... %3dm  %-52s  (%s lines)\n' \
        "$(( (SECONDS - start) / 60 ))" "${phase:-starting}" "$(wc -l < "$log")"
done

wait "$pid"
rc=$?

echo
echo "── phases ──"
grep -E '^==> ' "$log" || echo "    (none reached)"

if [ "$rc" -ne 0 ]; then
    echo
    # The tail alone is usually useless: a parallel make keeps printing compile lines after
    # the failing one, so the error scrolls away. Pull the error lines out by name first, and
    # keep the tail only as context.
    echo "── FAILED (exit $rc) — error lines ──"
    grep -nEi 'error:|error [0-9]+|undefined reference|collect2:|cannot find|no such file|fatal|\*\*\* ' "$log" \
        | head -n 40 \
        || echo "    (nothing matched the usual error patterns — see the tail below)"
    echo
    echo "── last 60 lines (context) ──"
    tail -n 60 "$log"
else
    printf '\n✅ ok in %dm (%s lines)\n' "$(( (SECONDS - start) / 60 ))" "$(wc -l < "$log")"
fi

# Archive. The run's own output IS the archive: Forgejo keeps it, it is searchable in the web
# UI, and it survives the next build overwriting $log on the cache disk. Folded into a
# collapsed group so it stays out of the way -- one click, not a wall.
#
# Deliberately NOT an uploaded artifact: actions/upload-artifact is a Node action, and these
# job containers have no Node (which is also why the checkout steps are hand-rolled git).
# Adding Node to both build images just to file a text file is a poor trade.
#
# QUIET_ARCHIVE=0 turns it off, for the day a build produces an absurd log.
if [ "${QUIET_ARCHIVE:-1}" = "1" ]; then
    printf '::group::full build log (%s lines)\n' "$(wc -l < "$log")"
    cat "$log"
    echo '::endgroup::'
fi

echo "── full log also on the runner host: $log ──"
exit "$rc"
