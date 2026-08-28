#!/bin/bash
# M8's absolute fence, restated for M9, from a clean base, in one command.
#
# M9-CLOUD.md: on any tree containing NO dataless files, every output —
# interactive and not — is byte-identical to main, counters included. That is
# M8's fence with one clause added, and it is the clause M9 has to earn: this
# milestone rewrites every rate on every progress display, adds a field to the
# walk's getattrlistbulk request, and adds a key to --report-json. Any one of
# those could move a number the referee grades.
#
# The harness is `m8_freeze.py`, unchanged and deliberately not forked. The
# fence did not change, so neither did the thing that measures it; only the
# base commit is new. Re-running the previous milestone's own gate — written
# before any of this code existed, by somebody with no stake in it passing —
# is a stronger claim than running one written alongside the code it grades.
#
# This builds main@644c66f from a fresh `git archive` — not from anything left
# lying around in a worktree — captures every non-interactive surface from both
# binaries over six corpora, and diffs them. 84 cases.
#
# The gate is falsifiable and was falsified before it was trusted: M8 recorded
# that leaking the live path's deferred stage 5 onto the frozen path makes 56
# of the 84 cases differ. For M9 the corresponding proof is in
# `m9_fence_teeth.sh`, which emits the skip count unconditionally instead of
# only when nonzero and requires this gate to break.
#
# Nothing here touches /Volumes. Corpora and both checkouts live under /tmp.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m9_freeze_gate.sh > rust/evidence/m9-freeze.txt
set -u

BASE_REF=${BASE_REF:-644c66f}
BASE_CO=/tmp/m9-freeze-base-checkout
SCRATCH=/tmp/m9-freeze-scratch
CAP_BASE=/tmp/m9-freeze-cap-base
CAP_OURS=/tmp/m9-freeze-cap-ours

echo "=== the base this branch is measured against ==="
echo "\$ git merge-base main HEAD"
git merge-base main HEAD
echo "\$ git log --oneline -1 ${BASE_REF}"
git log --oneline -1 "${BASE_REF}"
echo

echo "\$ rm -rf ${BASE_CO} && git archive ${BASE_REF} | tar -x -C ${BASE_CO}"
rm -rf "${BASE_CO}"; mkdir -p "${BASE_CO}"
git archive "${BASE_REF}" | tar -x -C "${BASE_CO}"
echo "\$ (cd ${BASE_CO}/rust && cargo build --release)"
(cd "${BASE_CO}/rust" && cargo build --release 2>&1 | tail -1)
echo

echo '$ cargo build --release   (the branch)'
(cd rust && cargo build --release 2>&1 | tail -1)
echo

# Hashes are compared only WITHIN one build directory: a release build embeds
# its own absolute path, so the two binaries differ for reasons that have
# nothing to do with behaviour. What is worth proving is that neither is stale.
echo '$ shasum -a 256 (both binaries, for the record)'
shasum -a 256 "${BASE_CO}/rust/target/release/dedup-rs"
shasum -a 256 rust/target/release/dedup-rs
echo

rm -rf "${SCRATCH}" "${CAP_BASE}" "${CAP_OURS}"
echo "\$ m8_freeze.py capture ${BASE_CO}/rust/target/release/dedup-rs ${CAP_BASE}"
python3 rust/evidence/scripts/m8_freeze.py capture \
    "${BASE_CO}/rust/target/release/dedup-rs" "${CAP_BASE}" --scratch "${SCRATCH}"
rm -rf "${SCRATCH}"
echo
echo '$ m8_freeze.py capture rust/target/release/dedup-rs '"${CAP_OURS}"
python3 rust/evidence/scripts/m8_freeze.py capture \
    rust/target/release/dedup-rs "${CAP_OURS}" --scratch "${SCRATCH}"
echo

echo "\$ m8_freeze.py compare ${CAP_BASE} ${CAP_OURS}"
python3 rust/evidence/scripts/m8_freeze.py compare "${CAP_BASE}" "${CAP_OURS}"
VERDICT=$?

echo
echo '$ ls /Volumes   (nothing here should have touched it)'
ls /Volumes
exit "${VERDICT}"
