#!/bin/bash
# Capture every M8 evidence run to a file, so the claim quotes files rather
# than retyping numbers.
#
# The freeze gate and the conformance gate are NOT here: each takes long enough
# to be worth running on its own, and each has its own script and its own
# evidence file.
#
# ORDER MATTERS. The two mutant scripts edit `src/`, rebuild, restore and
# rebuild again, proving the restore by hash. They run FIRST, and the release
# binary is rebuilt after them before anything else touches it — the
# stale-mutant rule, which has burned two people on this project.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m8_capture.sh
set -u

BIN=rust/target/release/dedup-rs

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

# --- the mutant scripts, and then a clean rebuild ---------------------------
run m8-deletion-mutants.txt python3 rust/evidence/scripts/m8_deletion_mutants.py
run m8-fence-clauses.txt python3 rust/evidence/scripts/m8_fence_clauses.py
run m8-panic-probe.txt   python3 rust/evidence/scripts/m8_panic_probe.py
echo '$ cargo build --release   (after the mutants, before anything runs the binary)'
(cd rust && cargo build --release 2>&1 | tail -1)
echo

# --- everything that drives the binary --------------------------------------
rm -rf /tmp/m8-e2e /tmp/m8-cache /tmp/m8-force /tmp/m8-tty
run m8-live-end-to-end.txt   python3 rust/evidence/scripts/m8_live_end_to_end.py "${BIN}" /tmp/m8-e2e
run m8-live-death.txt        python3 rust/evidence/scripts/m8_live_death.py "${BIN}"
run m8-cache-kill-resume.txt python3 rust/evidence/scripts/m8_cache_kill_resume.py "${BIN}" /tmp/m8-cache
run m8-force.txt             python3 rust/evidence/scripts/m8_force.py "${BIN}" /tmp/m8-force
run m8-tty-matrix.txt        python3 rust/evidence/scripts/m8_tty_matrix.py "${BIN}" /tmp/m8-tty
