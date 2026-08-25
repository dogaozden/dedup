#!/usr/bin/env python3
"""Every clause of the fence's own guard must be guarded by a test.

REVIEW-M8 item 3. `live_wanted` is where the M8 claim says the absolute fence
"is enforced in code rather than promised in a comment" — but every case in the
freeze proof is a run with no terminal, so `has_terminal()` short-circuits and
the clauses behind it are never evaluated. The reviewer deleted two of them and
`cargo test` stayed at 250 passed: a gate that cannot fail proves nothing about
the clause it does not reach.

The flag half is now `live_flags`, decidable without a terminal. This script is
the proof that it is decidable AND decided: it deletes each clause in turn,
runs `cargo test`, and requires a failure every time.

After the last mutant the source is restored and rebuilt, and the restore is
proved by comparing the release binary's hash with the one it started from —
the stale-mutant rule, checked rather than asserted.

Nothing here touches /Volumes and nothing is written outside `rust/src`.

usage: m8_fence_clauses.py
"""
import hashlib
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RUST = os.path.dirname(os.path.dirname(HERE))
SOURCE = os.path.join(RUST, "src", "main.rs")
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")

# (name, what to find, what to put in its place). Each removes exactly one
# reason `live_flags` can say no.
MUTANTS = [
    ("live_flags loses the --report-json clause",
     " && options.report_json.is_none()", ""),
    ("live_flags loses the --stage clause",
     " && options.stage.is_empty()", ""),
    ("live_flags loses browsing_flags entirely",
     "browsing_flags(options) && options.report_json.is_none()",
     "options.report_json.is_none()"),
    ("browsing_flags stops excluding --auto",
     "options.auto.is_none() && (options.interactive || !options.list)",
     "(options.interactive || !options.list)"),
    ("browsing_flags stops excluding --list",
     "options.auto.is_none() && (options.interactive || !options.list)",
     "options.auto.is_none()"),
]


def sha(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=RUST, capture_output=True,
                          text=True)


def failing_tests(output):
    return [line.split()[1] for line in output.splitlines()
            if line.startswith("test ") and line.rstrip().endswith("FAILED")]


def main():
    original = open(SOURCE).read()
    # Build FIRST, so the hash compared at the end is what the committed
    # source produces rather than whatever happened to be lying in target/.
    # An earlier run of this script reported a mismatch that was only ever a
    # stale binary from before the source was last edited — which is the
    # stale-mutant rule catching the script rather than the code, and worth
    # keeping in mind rather than tuning away.
    cargo("build", "--release")
    before = sha(BINARY)
    problems = []
    try:
        for name, find, replace in MUTANTS:
            if find not in original:
                problems.append("%s: the text to mutate is not there any more "
                                "— update this script" % name)
                continue
            open(SOURCE, "w").write(original.replace(find, replace, 1))
            done = cargo("test", "--release")
            caught = failing_tests(done.stdout)
            print("%-46s %s" % (name, "caught" if caught else "SURVIVED"))
            for test in caught:
                print("%-46s   by %s" % ("", test))
            if not caught:
                problems.append("%s: nothing failed — this clause of the "
                                "fence is guarded by nothing" % name)
    finally:
        open(SOURCE, "w").write(original)
        cargo("build", "--release")

    print()
    green = cargo("test", "--release")
    line = [ln for ln in green.stdout.splitlines() if ln.startswith("test result")]
    print("$ cargo test   (source restored)")
    print("   %s" % (line[0] if line else "no result line"))
    if "0 failed" not in (line[0] if line else ""):
        problems.append("the suite is not green again after the mutants")

    after = sha(BINARY)
    print("$ shasum -a 256 target/release/dedup-rs   (the stale-mutant rule)")
    print("   before: %s" % (before or "(none)"))
    print("   after:  %s" % after)
    if before is not None and before != after:
        problems.append("the restored binary is not the one we started with")

    print()
    for problem in problems:
        print("FAIL  %s" % problem)
    if not problems:
        print("pass  every clause of live_flags is guarded by a test that "
              "fails when it goes")
    return len(problems)


if __name__ == "__main__":
    sys.exit(main())
