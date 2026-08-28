#!/bin/bash
# Capture M9's evidence files, the way every other one here is captured: by
# redirecting the script that produced it, never by retyping what it printed.
#
# ORDER MATTERS. Four scripts here edit `src/`, rebuild, and restore:
# `m9_dataless_walk.py`, `m9_mutants.py`, `m9_policy_guard.py` and
# `m9_rate_window.py`. Each proves its own restore against the bytes it read at
# entry, but the binary they leave behind is only correct if nothing else runs
# between the restore and the next build — so `cargo build --release` runs
# immediately after them, before any script that measures the binary. The
# stale-mutant rule has burned two people on this project already.
#
# Three evidence files are NOT produced here, because each has its own script
# and its own redirect:
#   rust/evidence/scripts/m9_freeze_gate.sh  > rust/evidence/m9-freeze.txt
#   rust/evidence/scripts/m9_fence_teeth.sh  > rust/evidence/m9-fence-teeth.txt
#   rust/evidence/scripts/m9_gate.sh         > rust/evidence/m9-gate.txt
# ...and the standards battery, which needs the base checkout as an argument:
#   (cd rust && evidence/scripts/m9_standards.sh <base-checkout>/rust \
#       > evidence/m9-standards.txt)
#
# Usage, from the repository root:
#   rust/evidence/scripts/m9_capture.sh
set -u

BIN=rust/target/release/dedup-rs
SCRATCH=${SCRATCH:-/tmp/m9-capture}

run() {
    local name="$1"; shift
    local out="rust/evidence/${name}"
    {
        echo "\$ $*"
        echo
        "$@" 2>&1
        echo
        echo "   exit $?"
    } > "${out}"
    echo "${out}"
    tail -3 "${out}"
    echo
}

rm -rf "${SCRATCH}"
mkdir -p "${SCRATCH}"

# The two that edit src/. First, and nothing between them and the rebuild.
run m9-dataless-walk.txt python3 rust/evidence/scripts/m9_dataless_walk.py "${SCRATCH}/walk"
run m9-mutants.txt       python3 rust/evidence/scripts/m9_mutants.py       "${SCRATCH}/mutants"
# REVIEW-M9 items 1 and 2, each running the reviewer's own verification recipe.
run m9-policy-guard.txt  python3 rust/evidence/scripts/m9_policy_guard.py
run m9-rate-window.txt   python3 rust/evidence/scripts/m9_rate_window.py

echo '$ cargo build --release   (the stale-mutant rule)'
(cd rust && cargo build --release 2>&1 | tail -1)
shasum -a 256 "${BIN}"
echo

# ...and now the ones that measure the binary.
run m9-pulse.txt python3 rust/evidence/scripts/m9_pulse.py "${BIN}" "${SCRATCH}/pulse"
