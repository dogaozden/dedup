#!/usr/bin/env python3
"""REVIEW-M9 binding item 1: prove the I/O-policy guard bites.

`refuse_materialization` sets the policy and then asks `materialization_refused`
whether it took — but both spell the scope and the value with the SAME symbols,
so before this guard existed any wrong value was self-consistent and invisible.
The reviewer demonstrated it: two one-character mutants, all 276 tests green,
and every behavioural case of `m9_dataless_walk.py` still passing, while the
process was either shielding nobody or asking the kernel to download.

This runs the reviewer's own verification recipe, so that the guard's teeth are
a captured fact here rather than something the reviewer has to discover again:

  1. the three IOPOL constants, each mutated in turn, each required to fail;
  2. and the one that matters most, mutated the way somebody TIDYING UP would
     actually do it — the scope changed AND its literal pin edited to agree, so
     that only the behavioural half of the guard can catch it. That is the case
     that says the scope is a tested property rather than a remembered one,
     because pinning a literal can always be "fixed" by editing the literal.

THE STALE-MUTANT RULE. Each mutant is reverted in a `finally`, the revert is
proved by comparing the file's bytes against the bytes read at entry, and the
release binary is rebuilt to its pre-mutant hash at the end. Anything that runs
after this must still `cargo build --release` first.

Usage, from the repository root:
    python3 rust/evidence/scripts/m9_policy_guard.py
"""

import os
import subprocess
import sys

RUST = "rust"
SOURCE = os.path.join(RUST, "src", "cloud.rs")
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")

GUARD = "the_policy_constants_are_the_headers_and_the_shield_covers_workers"

# (name, [(before, after), ...], what it does to a running scan)
MUTANTS = [
    (
        "scope -> THREAD (the false shield)",
        [("IOPOL_SCOPE_PROCESS: libc::c_int = 0;", "IOPOL_SCOPE_PROCESS: libc::c_int = 1;")],
        "the policy binds the calling thread only, and every byte is read on "
        "one of rayon's workers — a shield over nobody",
    ),
    (
        "OFF -> 2 (ask the kernel to download)",
        [
            (
                "IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;",
                "IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 2;",
            )
        ],
        "2 is ..._FILES_ON: the process asks the kernel to materialize, and "
        "reports success",
    ),
    (
        "type -> 6 (the neighbour the comment warns about)",
        [
            (
                "IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;",
                "IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 6;",
            )
        ],
        "6 is IOPOL_TYPE_VFS_IGNORE_CONTENT_PROTECTION — a different policy "
        "entirely, set on a value that means nothing to it",
    ),
    (
        "scope -> THREAD, with its literal pin edited to agree",
        [
            ("IOPOL_SCOPE_PROCESS: libc::c_int = 0;", "IOPOL_SCOPE_PROCESS: libc::c_int = 1;"),
            ("assert_eq!(IOPOL_SCOPE_PROCESS, 0);", "assert_eq!(IOPOL_SCOPE_PROCESS, 1);"),
        ],
        "what somebody tidying up a failing assertion would really do. Only "
        "the spawned-thread assertion can catch this, and it must",
    ),
]


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
    done = run(["cargo", "test", "--release"], cwd=RUST)
    summary = next(
        (l for l in done.stdout.splitlines() if l.startswith("test result:")),
        "(no summary)",
    )
    failed = [
        l.strip()
        for l in done.stdout.splitlines()
        if l.startswith("    ") and "::" in l and " " not in l.strip()
    ]
    why = [l for l in done.stdout.splitlines() if "panicked at" in l or "assertion" in l]
    return done.returncode == 0, summary, failed, why


def main():
    with open(SOURCE) as fh:
        original = fh.read()

    print("$ cargo build --release   (the committed source, before any mutant)")
    clean = build()
    print(f"  {clean}  target/release/dedup-rs")
    green, summary, _, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")
    if not green:
        raise SystemExit("the committed source does not pass its own tests")
    print()

    survived = []
    for name, edits, effect in MUTANTS:
        print(f"=== {name} ===")
        print(f"    {effect}")
        try:
            text = original
            ok = True
            for before, after in edits:
                if text.count(before) != 1:
                    survived.append(f"{name}: anchor {before!r} not found exactly once")
                    print("    ANCHOR NOT FOUND — not applied\n")
                    ok = False
                    break
                print(f"    -   {before.strip()}")
                print(f"    +   {after.strip()}")
                text = text.replace(before, after)
            if not ok:
                continue
            with open(SOURCE, "w") as fh:
                fh.write(text)
            build()
            caught, summary, failed, why = cargo_test()
            print(f"    $ cargo test --release\n      {summary}")
            for one in failed[:4]:
                print(f"      {one}")
            for one in why[:2]:
                print(f"      {one.strip()[:150]}")
            if caught:
                survived.append(f"{name}: all tests still passed")
                print("    SURVIVED\n")
                continue
            # A failure is not enough: it has to be the policy guard that
            # failed, not something incidental that happens to break too.
            if not any(GUARD in one for one in failed):
                survived.append(
                    f"{name}: tests failed, but not {GUARD} — caught by "
                    "something incidental"
                )
                print(f"    caught, but NOT by {GUARD}\n")
                continue
            print(f"    caught by {GUARD}\n")
        finally:
            with open(SOURCE, "w") as fh:
                fh.write(original)
            with open(SOURCE) as fh:
                if fh.read() != original:
                    survived.append(f"{name}: {SOURCE} was NOT restored")

    print("=== the stale-mutant rule: restored, and proved restored ===")
    print(f"  {SOURCE} was compared byte for byte against the bytes read at")
    print("  entry, immediately after each revert.")
    restored = build()
    print("$ cargo build --release")
    print(f"  {restored}  target/release/dedup-rs")
    print(
        f"  {'identical to the pre-mutant binary' if restored == clean else 'DIFFERENT — THE BINARY IS STALE'}"
    )
    if restored != clean:
        survived.append("the rebuilt binary is not the pre-mutant binary")
    green, summary, _, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")
    if not green:
        survived.append("the restored source does not pass its own tests")

    print()
    if survived:
        print(f"MUTANTS THAT GOT AWAY ({len(survived)}):")
        for one in survived:
            print(f"  - {one}")
        return 1
    print(
        "every wrong constant is caught, and the scope is caught even when its "
        "literal pin has been edited to agree with it — which is the half that "
        "makes it a tested property rather than a remembered one"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
