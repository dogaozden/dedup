#!/bin/bash
# The conformance gate, with no profile flag anywhere in it — which is what
# proves the pivot-window sweep changed nothing the referee grades.
#
# `ls /Volumes` is recorded either side of the run. The only /Volumes path this
# touches is the 80 MB APFS image the `m4-trashes` check creates, attaches at
# /Volumes/dedupconf and detaches itself; the listings are here so a reader can
# see that nothing that was already mounted was read or written.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m7_gate.sh > rust/evidence/m7-gate.txt
set -u

echo '$ ls /Volumes   (before)'
ls /Volumes
echo
echo '$ python3 conformance/run.py --gate all \'
echo "    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'"
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}' 2>&1
echo "   exit ${PIPESTATUS[0]}"
echo
echo '$ ls /Volumes   (after)'
ls /Volumes
echo
echo '$ hdiutil info | grep -c dedupconf   (0 = the check detached its image)'
hdiutil info | grep -c dedupconf || true
