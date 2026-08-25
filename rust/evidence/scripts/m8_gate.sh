#!/bin/bash
# The conformance gate, with no profile flag anywhere in it — which is what
# proves the live scan changed nothing the referee grades.
#
# M8 adds a second reason to trust it: every invocation below passes
# `--report-json`, and `live_wanted` in main.rs refuses the live path to any
# run that does, so no gate can reach the streaming browser even if one were
# somehow run on a terminal. The gate grades the frozen pipeline because it
# cannot do anything else.
#
# `ls /Volumes` is recorded either side of the run AND compared: REVIEW-M7's
# non-binding note 3 pointed out that printing two listings and concluding
# "unchanged" in prose is not a gate. It is one now — a difference is a
# non-zero exit. The only /Volumes path this touches is the 80 MB APFS image
# the `m4-trashes` check creates, attaches at /Volumes/dedupconf and detaches
# itself; the listings prove nothing that was already mounted was disturbed.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m8_gate.sh > rust/evidence/m8-gate.txt
set -u

BEFORE=$(mktemp)
AFTER=$(mktemp)
trap 'rm -f "$BEFORE" "$AFTER"' EXIT

ls /Volumes > "$BEFORE" 2>&1
echo '$ ls /Volumes   (before)'
cat "$BEFORE"
echo
echo '$ python3 conformance/run.py --gate all \'
echo "    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'"
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}' 2>&1
GATE=${PIPESTATUS[0]}
echo "   exit ${GATE}"
echo

ls /Volumes > "$AFTER" 2>&1
echo '$ ls /Volumes   (after)'
cat "$AFTER"
echo
echo '$ diff <(ls /Volumes before) <(ls /Volumes after)'
if diff "$BEFORE" "$AFTER" > /dev/null; then
    echo '(no output: the mounted volumes are the ones that were there before)'
    FENCE=0
else
    echo 'MOUNT TABLE CHANGED:'
    diff "$BEFORE" "$AFTER"
    FENCE=1
fi
echo
echo '$ hdiutil info | grep -c dedupconf   (0 = the check detached its image)'
LEFTOVER=$(hdiutil info | grep -c dedupconf)
echo "$LEFTOVER"

echo
if [ "$GATE" -eq 0 ] && [ "$FENCE" -eq 0 ] && [ "$LEFTOVER" -eq 0 ]; then
    echo 'gate green, mount table unchanged, no image left attached'
    exit 0
fi
echo "FAILED: gate exit ${GATE}, mount table changed=${FENCE}, images left=${LEFTOVER}"
exit 1
