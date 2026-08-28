#!/usr/bin/env python3
"""Break each thing M9 claims to guard, and require the right harness to notice.

A gate that cannot fail is not a gate. Every mutant below is a plausible edit —
the kind somebody makes while tidying — and each one is paired with the harness
that is supposed to catch it. A mutant that survives its harness is a failure of
this script, and is reported as one.

The pairing matters as much as the failure. `cargo test` cannot reach the
walk-time exclusion at all: no test can make a dataless file, so deleting the
skip leaves the whole suite green. That mutant is caught only by
`m9_dataless_walk.py`, and saying so here is the point — M8 learned the same
thing about its deletion-honesty proof, which lives in the pty battery and not
in `cargo test`.

THE STALE-MUTANT RULE. Every mutant is applied to the working tree, reverted in
a `finally`, and the revert is proved rather than asserted: each file's bytes
are compared against the exact bytes read before the mutant went in, and the
rebuilt release binary's hash is compared against the one taken before any
mutant ran. Content, not `git diff` — an earlier draft used `git diff --stat`
and reported a failure the moment the branch had any other uncommitted work in
`rust/src`, which is a check on the wrong thing. `git diff --stat` is still
printed, for information, so a reader can see what else was in the tree.
Anything that runs after this must still `cargo build --release` first.

Usage, from the repository root:
    python3 rust/evidence/scripts/m9_mutants.py <scratch-dir>
"""

import os
import subprocess
import sys

RUST = "rust"
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")


def run(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def hash_of(path):
    return run(["shasum", "-a", "256", path]).stdout.split()[0]


def build():
    done = run(["cargo", "build", "--release"], cwd=RUST)
    if done.returncode != 0:
        raise SystemExit("build failed:\n" + done.stderr[-2000:])
    return hash_of(BINARY)


def cargo_test():
    """Every test, and the names of the ones that failed."""
    done = run(["cargo", "test", "--release"], cwd=RUST)
    failed = sorted(
        line.split()[0]
        for line in done.stdout.splitlines()
        if line.strip().startswith(("tests::", "pulse::", "cloud::", "effort::"))
        or (line.startswith("    ") and "::" in line and line.strip().count(" ") == 0)
    )
    summary = next(
        (line for line in done.stdout.splitlines() if line.startswith("test result:")),
        "(no summary)",
    )
    return done.returncode == 0, summary, failed


def walk_harness(scratch):
    """The end-to-end dataless proof, run as its own process."""
    done = run(
        [
            "python3",
            "rust/evidence/scripts/m9_dataless_walk.py",
            os.path.join(scratch, "walk"),
        ]
    )
    reasons = [
        line.strip()[2:]
        for line in done.stdout.splitlines()
        if line.startswith("  - ")
    ]
    return done.returncode == 0, reasons


def pty_harness(scratch):
    """The terminal battery, run as its own process.

    Its corpus is built once and reused, so only the first call pays for it.
    """
    done = run(
        [
            "python3",
            "rust/evidence/scripts/m9_pulse.py",
            BINARY,
            os.path.join(scratch, "pulse"),
        ]
    )
    reasons = [
        line.split(": ", 1)[1] if ": " in line else line
        for line in done.stdout.splitlines()
        if "FAIL" in line
    ]
    return done.returncode == 0, reasons


# (name, file, what it was, what it becomes, which harness should catch it, why
#  somebody might plausibly write it)
MUTANTS = [
    (
        "the-walk-stops-skipping",
        "rust/src/walk.rs",
        "if !filters.cloud && crate::cloud::dataless(entry.flags) {",
        "if false && !filters.cloud && crate::cloud::dataless(entry.flags) {",
        "walk",
        "the law itself: a cloud placeholder becomes a record and gets read",
    ),
    (
        "the-flag-cloud-does-nothing",
        "rust/src/walk.rs",
        "if !filters.cloud && crate::cloud::dataless(entry.flags) {",
        "if crate::cloud::dataless(entry.flags) {",
        "walk",
        "--cloud accepted and ignored, so the escape hatch is not one",
    ),
    (
        "the-count-is-not-reported",
        "rust/src/main.rs",
        "skipped_cloud: (skipped_cloud > 0).then_some(skipped_cloud),",
        "skipped_cloud: None,",
        "walk",
        "the skips become silent, which is what the spec forbids",
    ),
    (
        "live-wanted-forgets-two-of-its-clauses",
        "rust/src/main.rs",
        "    terminal && live_flags(options)",
        "    terminal && browsing_flags(options)",
        "test",
        "REVIEW-M8's non-binding note 4, and the reviewer's own mutant: this "
        "left all 258 tests green on main, because `has_terminal()` was asked "
        "for rather than passed in and is false in every test process",
    ),
    (
        "the-detector-is-the-size-heuristic",
        "rust/src/cloud.rs",
        "    flags & SF_DATALESS != 0\n",
        "    flags & (SF_DATALESS | 0x0000_0020) != 0\n",
        "test",
        "UF_COMPRESSED folded in, which drops real local files as 'cloud'",
    ),
    (
        "the-policy-is-installed-backwards",
        "rust/src/cloud.rs",
        "    !wants_cloud && refuse_materialization()",
        "    wants_cloud && refuse_materialization()",
        "test",
        "the no-materialize policy installed only for the runs that asked for "
        "the downloads, and never for the default scan it protects",
    ),
    (
        "the-window-becomes-the-lifetime",
        "rust/src/pulse.rs",
        "        while self.readings.len() >= 2\n"
        "            && now.duration_since(self.readings[1].0) >= crate::consts::RATE_WINDOW\n"
        "        {\n"
        "            self.readings.pop_front();\n"
        "        }\n",
        "",
        "test",
        "the 1/t bug, exactly: nothing ever leaves the window",
    ),
    (
        "the-window-is-one-reading-wide",
        "rust/src/pulse.rs",
        "            && now.duration_since(self.readings[1].0) >= crate::consts::RATE_WINDOW",
        "            && now.duration_since(self.readings[1].0) >= SPACING",
        "test",
        "a rate over the last beat: jumpy, and not what the constant says",
    ),
    (
        "the-name-is-cut-at-the-end",
        "rust/src/pulse.rs",
        "    let left = (room - 1) / 3;\n    let right = room - 1 - left;",
        "    let left = room - 1;\n    let right = 0;",
        "test",
        "the basename thrown away, keeping the part every path shares",
    ),
    (
        "the-progress-row-forgets-the-file",
        "rust/src/pulse.rs",
        "    let painted = line(stage.label, &body, &effort.now_reading(), width);",
        '    let painted = line(stage.label, &body, "", width);',
        "pty",
        "the stderr pulse stops naming the file it is stuck on",
    ),
    (
        "the-comparison-forgets-the-file",
        "rust/src/compare.rs",
        "                    context.effort.reading(records[opened[*slot]].path());\n",
        "",
        "pty",
        "the long stage goes back to naming nothing — the bug this milestone "
        "already made once",
    ),
]


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: m9_mutants.py <scratch-dir>")
    scratch = sys.argv[1]
    os.makedirs(scratch, exist_ok=True)

    print("$ cargo build --release   (the committed source, before any mutant)")
    clean = build()
    print(f"  {clean}  target/release/dedup-rs")
    green, summary, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")
    if not green:
        raise SystemExit("the committed source does not pass its own tests")
    print()

    survived = []
    for name, path, before, after, harness, why in MUTANTS:
        print(f"=== {name} ===")
        print(f"    {why}")
        print(f"    {path}, caught by: {harness}")
        with open(path) as fh:
            original = fh.read()
        if original.count(before) != 1:
            survived.append(f"{name}: the anchor is not in {path} exactly once")
            print("    ANCHOR NOT FOUND — the mutant was not applied\n")
            continue
        try:
            with open(path, "w") as fh:
                fh.write(original.replace(before, after))
            if harness == "test":
                build()
                caught, summary, failed = cargo_test()
                print(f"    $ cargo test --release\n      {summary}")
                for one in failed[:6]:
                    print(f"      {one}")
                if caught:
                    survived.append(f"{name}: all tests still passed")
                    print("    SURVIVED\n")
                else:
                    print("    caught\n")
            elif harness == "walk":
                build()
                held, reasons = walk_harness(scratch)
                print("    $ m9_dataless_walk.py")
                for one in reasons[:6]:
                    print(f"      - {one}")
                # The suite is asked too, so the record shows what it did and
                # did not see. A green suite here is not a defect; it is the
                # reason this harness exists.
                suite_green, suite_summary, _ = cargo_test()
                print(f"    $ cargo test --release\n      {suite_summary}")
                if suite_green:
                    print(
                        "      (green — no test can make a dataless file, which "
                        "is why the harness above is the guard)"
                    )
                if held:
                    survived.append(f"{name}: the dataless harness still passed")
                    print("    SURVIVED\n")
                else:
                    print("    caught\n")
            else:
                build()
                held, reasons = pty_harness(scratch)
                print("    $ m9_pulse.py")
                for one in reasons[:4]:
                    print(f"      {one}")
                # The suite is asked too, so the record shows plainly that it
                # does NOT cover this — `cargo test` has no terminal, so it
                # cannot see anything the terminal is shown.
                suite_green, suite_summary, _ = cargo_test()
                print(f"    $ cargo test --release\n      {suite_summary}")
                if suite_green:
                    print(
                        "      (green — no test has a terminal, which is why "
                        "the battery above is the guard)"
                    )
                if held:
                    survived.append(f"{name}: the pty battery still passed")
                    print("    SURVIVED\n")
                else:
                    print("    caught\n")
        finally:
            with open(path, "w") as fh:
                fh.write(original)
            with open(path) as fh:
                if fh.read() != original:
                    survived.append(f"{name}: {path} was NOT restored")

    print("=== the stale-mutant rule: restored, and proved restored ===")
    print("  every mutated file was compared byte for byte against the bytes")
    print("  read before its mutant went in, immediately after each revert.")
    print("$ git diff --stat -- rust/src   (for information: what else is in the tree)")
    for line in run(["git", "diff", "--stat", "--", "rust/src"]).stdout.splitlines():
        print(f"  {line}")
    restored = build()
    print("$ cargo build --release")
    print(f"  {restored}  target/release/dedup-rs")
    print(
        f"  {'identical to the pre-mutant binary' if restored == clean else 'DIFFERENT — THE BINARY IS STALE'}"
    )
    if restored != clean:
        survived.append("the rebuilt binary is not the pre-mutant binary")
    green, summary, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")
    if not green:
        survived.append("the restored source does not pass its own tests")

    print()
    if survived:
        print(f"MUTANTS THAT GOT AWAY ({len(survived)}):")
        for one in survived:
            print(f"  - {one}")
        return 1
    # Counted rather than written down: the numbers drifted the first time a
    # mutant was added to the table, which is the same class of mistake as a
    # comment that describes code it no longer matches.
    tally = {}
    for _, _, _, _, harness, _ in MUTANTS:
        tally[harness] = tally.get(harness, 0) + 1
    blind = tally.get("walk", 0) + tally.get("pty", 0)
    print(
        "every mutant was caught by the harness it was paired with: "
        f"{tally.get('test', 0)} by `cargo test`, {tally.get('walk', 0)} by "
        f"the dataless harness and {tally.get('pty', 0)} by the pty battery "
        f"— and the record above shows the suite staying green for all "
        f"{blind} of the ones it cannot reach"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
