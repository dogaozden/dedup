#!/bin/bash
# The three project standards, captured the way every other evidence file here
# is: by redirecting this script, never by retyping what it printed.
#
# The build directory is cleaned first so clippy really compiles the crate
# rather than replaying a warm cache and printing "Finished" over yesterday's
# verdict.
#
# Usage, from rust/:  evidence/scripts/m5_standards.sh > evidence/m5-standards.txt
set -u

cargo clean -p dedup-rs 2>/dev/null

echo '$ cargo test'
cargo test 2>&1 | tail -3
echo
echo '$ cargo clippy --all-targets -- -D warnings'
cargo clippy --all-targets -- -D warnings 2>&1 | tail -3
echo
echo '$ cargo fmt --check'
cargo fmt --check && echo '(no output: clean)'
