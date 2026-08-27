#!/bin/bash
# Prove the M9 freeze gate can fail.
#
# `m9_freeze_gate.sh` says 84 cases, 0 differing. That sentence is worth
# nothing unless the gate would have said something else had M9 broken the
# fence — and the way M9 was most likely to break it is very specific.
#
# `--report-json` gains a `skipped_cloud` key, and the spec requires it to be
# ABSENT rather than zero on a tree with no cloud files in it, because a new
# key holding `0` is not byte-identical output. That "only when nonzero" is one
# `.then_some()` and it is the single easiest thing in this milestone to write
# the obvious wrong way. So that is the mutant: emit the count always.
#
# Nothing else is touched, and the corpora are the same six the gate uses —
# none of which contains a dataless file, so the count is 0 in every case. A
# gate that passes this mutant is a gate that would not have noticed M9
# breaking the one law it exists to enforce.
#
# THE STALE-MUTANT RULE. The source is restored in a trap and the restore is
# proved by rebuilding to the pre-mutant hash. Anything that runs after this
# must still `cargo build --release` first.
#
# Usage, from the repository root:
#   rust/evidence/scripts/m9_fence_teeth.sh > rust/evidence/m9-fence-teeth.txt
set -u

BASE_REF=${BASE_REF:-644c66f}
BASE_CO=/tmp/m9-freeze-base-checkout
SCRATCH=/tmp/m9-teeth-scratch
CAP_BASE=/tmp/m9-freeze-cap-base
CAP_BROKEN=/tmp/m9-teeth-cap-broken
SOURCE=rust/src/main.rs
KEEP=/tmp/m9-teeth-main.rs.orig

WAS='skipped_cloud: (skipped_cloud > 0).then_some(skipped_cloud),'
NOW='skipped_cloud: Some(skipped_cloud),'

cp "${SOURCE}" "${KEEP}"
restore() {
    cp "${KEEP}" "${SOURCE}"
    echo
    echo '=== the stale-mutant rule: restored, and proved restored ==='
    echo "\$ cmp ${SOURCE} ${KEEP}"
    if cmp "${SOURCE}" "${KEEP}"; then
        echo '  (no output: the file is the one that was read before the mutant)'
    fi
    echo '$ cargo build --release'
    (cd rust && cargo build --release 2>&1 | tail -1)
    echo '$ shasum -a 256 rust/target/release/dedup-rs'
    shasum -a 256 rust/target/release/dedup-rs
    echo "  (compare with the pre-mutant hash printed at the top)"
    rm -f "${KEEP}"
}
trap restore EXIT

echo '$ cargo build --release   (the committed source, before the mutant)'
(cd rust && cargo build --release 2>&1 | tail -1)
echo '$ shasum -a 256 rust/target/release/dedup-rs'
shasum -a 256 rust/target/release/dedup-rs
echo

# The base capture the gate already took, or a fresh one if this is being run
# on its own.
if [ ! -f "${CAP_BASE}/capture.json" ]; then
    echo "\$ (no base capture at ${CAP_BASE}: building ${BASE_REF} and capturing it)"
    rm -rf "${BASE_CO}"; mkdir -p "${BASE_CO}"
    git archive "${BASE_REF}" | tar -x -C "${BASE_CO}"
    (cd "${BASE_CO}/rust" && cargo build --release 2>&1 | tail -1)
    rm -rf "${SCRATCH}"
    python3 rust/evidence/scripts/m8_freeze.py capture \
        "${BASE_CO}/rust/target/release/dedup-rs" "${CAP_BASE}" --scratch "${SCRATCH}"
else
    echo "\$ using the base capture m9_freeze_gate.sh already took: ${CAP_BASE}"
fi
echo

echo '=== the mutant: the skip count is always emitted, even when it is zero ==='
echo "-   ${WAS}"
echo "+   ${NOW}"
python3 - "${SOURCE}" "${WAS}" "${NOW}" <<'PY'
import sys
path, was, now = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
if text.count(was) != 1:
    raise SystemExit("anchor found %d times — refusing to edit" % text.count(was))
open(path, "w").write(text.replace(was, now))
PY
echo '$ cargo build --release'
(cd rust && cargo build --release 2>&1 | tail -1)
echo

rm -rf "${SCRATCH}" "${CAP_BROKEN}"
echo "\$ m8_freeze.py capture rust/target/release/dedup-rs ${CAP_BROKEN}"
python3 rust/evidence/scripts/m8_freeze.py capture \
    rust/target/release/dedup-rs "${CAP_BROKEN}" --scratch "${SCRATCH}"
echo

echo "\$ m8_freeze.py compare ${CAP_BASE} ${CAP_BROKEN}"
python3 rust/evidence/scripts/m8_freeze.py compare "${CAP_BASE}" "${CAP_BROKEN}"
BROKE=$?
echo

if [ "${BROKE}" -ne 0 ]; then
    echo 'THE GATE HAS TEETH: the mutant was caught, so "0 differing" means something'
    VERDICT=0
else
    echo 'THE GATE IS BLIND: a run that emits the key unconditionally passed it'
    VERDICT=1
fi
exit "${VERDICT}"
