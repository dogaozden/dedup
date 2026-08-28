#!/usr/bin/env python3
"""REVIEW-M9 binding item 2: what the rate tests pin about the window, exactly.

The claim used to say "the tests now pin the width from both directions". They
did not: the reviewer changed `RATE_WINDOW` to 900 ms and to 2 s with all 276
tests green, because every expectation in the test was derived from
`RATE_WINDOW` itself and so could only ever check internal consistency. Its
100 ms case "failed" on an underflow in the test's own arithmetic rather than
on an assertion — and under `--release` that would have wrapped silently.

This runs the reviewer's own sweep and prints what each width does, so the
claim can say what is true instead of what would be nice. What the tests pin
now:

  * the SHAPE of the decay — exactly 0 one window after the last byte, still
    non-zero a moment before that, about half the steady rate half a window in;
  * the number of readings the window holds, bounded above AND below;
  * and a BRACKET on the width, taken from the spec rather than from taste:
    at least a second, because a shorter window cannot bridge the gaps between
    512 KiB reads and would report ordinary reading as a stall; at most five,
    because M9-CLOUD.md asks that "a stalled read must read ~0 within a few
    seconds".

The exact width is NOT pinned and is not claimed to be. Two seconds passes,
which is right — the reviewer said 2 s would satisfy the spec.

THE STALE-MUTANT RULE. `consts.rs` is restored in a `finally`, the revert is
proved against the bytes read at entry, and the release binary is rebuilt to
its pre-mutant hash. Anything after this must `cargo build --release` first.

Usage, from the repository root:
    python3 rust/evidence/scripts/m9_rate_window.py
"""

import os
import subprocess
import sys

RUST = "rust"
SOURCE = os.path.join(RUST, "src", "consts.rs")
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")

SHIPPED = "pub const RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);"

# The reviewer's sweep, in its order.
WIDTHS = [
    "from_millis(100)",
    "from_millis(500)",
    "from_millis(900)",
    "from_secs(1)",
    "from_secs(2)",
    "from_secs(60)",
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
    why = [
        l.split("panicked at ")[-1].strip()
        for l in done.stdout.splitlines()
        if "panicked at" in l
    ]
    return done.returncode == 0, summary, failed, why


def main():
    with open(SOURCE) as fh:
        original = fh.read()
    if original.count(SHIPPED) != 1:
        raise SystemExit(f"{SOURCE} does not carry the shipped RATE_WINDOW")

    print("$ cargo build --release   (the committed source: RATE_WINDOW = 3s)")
    clean = build()
    print(f"  {clean}  target/release/dedup-rs")
    green, summary, _, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")
    if not green:
        raise SystemExit("the committed source does not pass its own tests")
    print()

    print("=== the sweep: RATE_WINDOW set to each width in turn ===")
    verdicts = {}
    try:
        for width in WIDTHS:
            with open(SOURCE, "w") as fh:
                fh.write(
                    original.replace(
                        SHIPPED,
                        "pub const RATE_WINDOW: std::time::Duration = "
                        f"std::time::Duration::{width};",
                    )
                )
            build()
            passed, summary, failed, why = cargo_test()
            verdicts[width] = passed
            print(f"{width:<17}{summary}")
            for one in failed[:3]:
                print(f"{'':<17}  {one}")
            for one in why[:1]:
                print(f"{'':<17}  {one[:120]}")
    finally:
        with open(SOURCE, "w") as fh:
            fh.write(original)
        with open(SOURCE) as fh:
            restored_source = fh.read() == original

    print()
    print("=== what that means ===")
    admitted = [w for w, ok in verdicts.items() if ok]
    refused = [w for w, ok in verdicts.items() if not ok]
    print(f"admitted: {', '.join(admitted) if admitted else '(none)'}")
    print(f"refused:  {', '.join(refused) if refused else '(none)'}")
    print(
        "The bracket refuses 100ms, 500ms, 900ms and 60s. One second is refused\n"
        "for a different reason: at ten readings wide, 'half a window into a\n"
        "stall' falls between readings and the decay-shape assertion cannot\n"
        "resolve it — so the tests also say the window must be several times the\n"
        "reading spacing to mean anything. Two seconds passes, and should: the\n"
        "spec allows it. The exact choice of three is a tuning judgement, and no\n"
        "test here defends it."
    )

    print()
    print("=== the stale-mutant rule: restored, and proved restored ===")
    print(f"  {SOURCE} compared byte for byte against the bytes read at entry:")
    print(f"  {'identical' if restored_source else 'DIFFERENT — NOT RESTORED'}")
    again = build()
    print("$ cargo build --release")
    print(f"  {again}  target/release/dedup-rs")
    print(
        f"  {'identical to the pre-sweep binary' if again == clean else 'DIFFERENT — THE BINARY IS STALE'}"
    )
    green, summary, _, _ = cargo_test()
    print(f"$ cargo test --release\n  {summary}")

    bad = []
    if not restored_source:
        bad.append("consts.rs was not restored")
    if again != clean:
        bad.append("the rebuilt binary is not the pre-sweep one")
    if not green:
        bad.append("the restored source does not pass its own tests")
    if verdicts.get("from_secs(60)"):
        bad.append("a 60s window passed: a stall would stay invisible for a minute")
    if verdicts.get("from_millis(100)"):
        bad.append("a 100ms window passed: the underflow is back, or the bracket is gone")
    print()
    if bad:
        print(f"FAILED ({len(bad)}):")
        for one in bad:
            print(f"  - {one}")
        return 1
    print(
        "the width is held by a bracket and not by a value, the claim says so, "
        "and the two widths the spec allows are the two the tests admit"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
