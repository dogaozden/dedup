#!/bin/bash
# M8's absolute fence, from a clean base, in one command.
#
# M8-LIVE.md: `--list`, `--auto`, `--report-json`, piped runs, and every
# conformance/bench invocation keep TODAY'S pipeline and TODAY'S bytes, byte
# for byte. This builds main@a362ffa from a fresh `git archive` — not from
# anything left lying around in a worktree — captures every non-interactive
# surface from both binaries over six corpora, and diffs them.
#
# The gate is falsifiable, and was falsified before it was trusted: letting the
# live path's deferred stage 5 leak onto the frozen path makes 56 of the 84
# cases differ, including `wide/decoy.bin` being reported inside a group it
# does not belong to. A gate that cannot fail is not a gate.
#
# Nothing here touches /Volumes. Corpora and both checkouts live under /tmp.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m8_freeze_gate.sh > rust/evidence/m8-freeze.txt
set -u

BASE_REF=a362ffa
BASE_CO=/tmp/m8-freeze-base-checkout
SCRATCH=/tmp/m8-freeze-scratch
CAP_BASE=/tmp/m8-freeze-cap-base
CAP_OURS=/tmp/m8-freeze-cap-ours

echo "\$ git rev-parse HEAD   (the branch under test)"
git rev-parse HEAD
echo
echo "\$ git rev-parse ${BASE_REF}   (the base it must match)"
git rev-parse "${BASE_REF}"
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

rm -rf "${SCRATCH}" "${CAP_BASE}" "${CAP_OURS}"
echo "\$ m8_freeze.py capture ${BASE_CO}/rust/target/release/dedup-rs ${CAP_BASE}"
python3 rust/evidence/scripts/m8_freeze.py capture \
    "${BASE_CO}/rust/target/release/dedup-rs" "${CAP_BASE}"
rm -rf "${SCRATCH}"
echo
echo '$ m8_freeze.py capture rust/target/release/dedup-rs '"${CAP_OURS}"
python3 rust/evidence/scripts/m8_freeze.py capture \
    rust/target/release/dedup-rs "${CAP_OURS}"
echo

echo "\$ m8_freeze.py compare ${CAP_BASE} ${CAP_OURS}"
python3 rust/evidence/scripts/m8_freeze.py compare "${CAP_BASE}" "${CAP_OURS}"
GATE=$?
echo
if [ "${GATE}" -eq 0 ]; then
    echo 'the fence held'
else
    echo 'THE FENCE IS BROKEN'
fi
exit "${GATE}"
