#!/bin/bash
# The three project standards, and the count the claim quotes, captured the way
# every other evidence file here is: by redirecting this script, never by
# retyping what it printed.
#
# The build directory is cleaned first so clippy really compiles the crate
# rather than replaying a warm cache and printing "Finished" over yesterday's
# verdict.
#
# `main` is built from a `git archive` of the base commit into a target
# directory of its own, so the before-count is re-derived here rather than
# copied out of the previous claim, and so the clippy verdict on the base can
# be read beside this branch's.
#
# Usage, from rust/:  evidence/scripts/m7_standards.sh <base-checkout>/rust \
#                       > evidence/m7-standards.txt
set -u
BASE="${1:?the base checkout's rust/ directory}"
HERE="$PWD"

echo "=== main@cc654c4, for the before-count and the clippy verdict that predates this branch ==="
cd "$BASE" || exit 2
echo '$ cargo test'
cargo test 2>&1 | tail -3
echo
echo '$ cargo clippy --all-targets -- -D warnings'
cargo clippy --all-targets -- -D warnings 2>&1 | tail -12
echo "   exit ${PIPESTATUS[0]}"
cd "$HERE" || exit 2

echo
echo "=== this branch ==="
cargo clean -p dedup-rs 2>/dev/null

echo '$ cargo test'
cargo test 2>&1 | tail -3
echo
echo '$ cargo clippy --all-targets -- -D warnings'
cargo clippy --all-targets -- -D warnings 2>&1 | tail -3
echo "   exit ${PIPESTATUS[0]}"
echo
echo '$ cargo fmt --check'
cargo fmt --check && echo '(no output: clean)'
