#!/usr/bin/env python3
"""Why a window grows by four and not by two, and whether the guard notices.

REVIEW-M7 binding item 2: the ruling for `PIVOT_GROWTH = 4` was written down as
a measured seek ratio, and no committed script varied the constant —
`pivot_run_size.py` varies `PIVOT_RUN`. So the number in the doc comment was an
assertion wearing a measurement's clothes. This script is the missing
measurement, and it does two jobs.

**One.** It builds a `PIVOT_GROWTH = 2` variant and reads the same corpora with
it and with the tip, cold, one reader. The files are large enough that the two
ramps genuinely differ: at 256 MiB a window of `SLOW_CHUNK` growing by four
reaches the end in three windows and one growing by two takes five. Groups and
`bytes_read` must be identical — the growth factor may only move head
movements — and the seeks are the answer.

**Two.** It runs `compare::tests` in that same `PIVOT_GROWTH = 2` tree and
requires the window-boundary guards to FAIL there. REVIEW-M7 binding item 3 was
a boundary test that had gone on passing after the ramp changed under it,
because its offsets were written down rather than derived. A boundary test that
passes at both growth factors is not testing boundaries, so this asserts that
it does not.

Both variants are built from a copy of the tree with a CARGO_TARGET_DIR of
their own. No variant binary is ever produced in the working checkout, and
nothing under `/Volumes` is touched.

Run from the repository root:

    python3 rust/evidence/scripts/pivot_growth.py <path-to-main-dedup-rs>
"""

import json
import os
import random
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
BRANCH = os.path.join(REPO, "rust", "target", "release", "dedup-rs")
BASE = os.path.join(REPO, "rust", "target", "m7", "growth")

MIB = 1024 * 1024
WRITTEN = "pub const PIVOT_GROWTH: usize = 4;"
MUTATED = "pub const PIVOT_GROWTH: usize = 2;"

# Files far enough past SLOW_CHUNK that the two ramps take different numbers of
# windows. Kept to what the free space on this machine allows.
CASES = [(3, 256 * MIB), (2, 512 * MIB), (4, 64 * MIB)]

# The guards that must notice the ramp moving. Named rather than counted, so a
# renamed test is a failure here rather than a silent pass.
BOUNDARY_GUARDS = [
    "the_windows_are_where_the_growth_factor_puts_them",
]


def build_corpus(root: str, width: int, size: int) -> None:
    os.makedirs(root)
    rng = random.Random(size)
    body = bytes(rng.getrandbits(8) for _ in range(4096)) * (size // 4096)
    for index in range(width):
        with open(os.path.join(root, "c{:02d}.bin".format(index)), "wb") as fh:
            fh.write(body)


def scan(binary: str, root: str, report: str) -> dict:
    proc = subprocess.run(
        [binary, root, "--report-json", report, "--min-size", "1", "--no-cache",
         "--profile", "spinning", "-j", "1"],
        capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("dedup-rs exited {}: {}".format(proc.returncode, proc.stderr))
    with open(report) as fh:
        return json.load(fh)


def growth_two_tree() -> str:
    """A copy of the tree with the growth factor put back to two."""
    tree = os.path.join(BASE, "rust")
    shutil.copytree(os.path.join(REPO, "rust"), tree,
                    ignore=shutil.ignore_patterns("target", "evidence"))
    source = os.path.join(tree, "src", "consts.rs")
    with open(source) as fh:
        text = fh.read()
    if text.count(WRITTEN) != 1:
        raise SystemExit("the anchor appears {} times, not once — this script "
                         "is stale".format(text.count(WRITTEN)))
    with open(source, "w") as fh:
        fh.write(text.replace(WRITTEN, MUTATED))
    return tree


def failing_tests(output: str):
    names = []
    for line in output.splitlines():
        line = line.strip()
        if line.startswith("test ") and line.endswith(" ... FAILED"):
            names.append(line[len("test "):-len(" ... FAILED")].split("::")[-1])
    return sorted(names)


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    main_binary = os.path.abspath(sys.argv[1])
    for binary in (main_binary, BRANCH):
        if not os.path.exists(binary):
            raise SystemExit("build it first: " + binary)

    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    tree = growth_two_tree()

    # ---------------------------------------------- one: does the ramp pay?
    target = os.path.join(BASE, "target")
    environment = dict(os.environ, CARGO_TARGET_DIR=target)
    built = subprocess.run(["cargo", "build", "--release"], cwd=tree,
                           capture_output=True, text=True, env=environment)
    if built.returncode != 0:
        raise SystemExit("the growth-2 variant did not build:\n" + built.stderr[-2000:])
    two = os.path.join(target, "release", "dedup-rs")

    report = os.path.join(BASE, "report.json")
    print("One group of N identical files, --profile spinning, one reader (-j 1).")
    print("Whole-scan counts; stages 1-4 are the same either way.\n")
    print("{:<18} {:>8} {:>8} {:>10} {:>12} {:>12}".format(
        "shape", "growth2", "growth4", "fewer at 4", "bytes equal", "groups"))
    wrong = 0
    ratios = []
    for width, size in CASES:
        root = os.path.join(BASE, "w{}s{}".format(width, size))
        build_corpus(root, width, size)
        at_two = scan(two, root, report)
        at_four = scan(BRANCH, root, report)
        same_bytes = at_two["bytes_read"] == at_four["bytes_read"]
        same_groups = (sorted(map(sorted, at_two["groups"]))
                       == sorted(map(sorted, at_four["groups"])))
        ratio = at_two["seeks"] / max(at_four["seeks"], 1)
        ratios.append(ratio)
        print("{:<18} {:>8} {:>8} {:>9.2f}x {:>12} {:>12}".format(
            "{} x {} MiB".format(width, size // MIB),
            at_two["seeks"], at_four["seeks"], ratio,
            str(same_bytes), len(at_four["groups"])))
        if not same_bytes:
            print("    BYTES MOVED: the growth factor may only move head movements")
            wrong += 1
        if not same_groups:
            print("    THE GROUPS MOVED")
            wrong += 1
        shutil.rmtree(root, ignore_errors=True)
    print("\ngrowth four issues {:.2f}x to {:.2f}x fewer head movements than growth "
          "two,\nreading exactly the same bytes for them".format(
              min(ratios), max(ratios)))

    # -------------------------- two: does the boundary guard see the change?
    print("\nAnd the guard that has to notice the ramp moving. `cargo test "
          "compare::` in the\nsame growth-2 tree — these must FAIL there, or "
          "they are not testing boundaries:")
    result = subprocess.run(["cargo", "test", "--", "compare::tests"], cwd=tree,
                            capture_output=True, text=True, env=environment)
    broke = failing_tests(result.stdout)
    for name in BOUNDARY_GUARDS:
        state = "FAILED at growth 2, as it must" if name in broke else \
                "PASSED at growth 2 — IT IS NOT TESTING BOUNDARIES"
        print("  {:<52} {}".format(name, state))
        if name not in broke:
            wrong += 1
    others = [name for name in broke if name not in BOUNDARY_GUARDS]
    if others:
        print("  (also failing at growth 2, which is fine: {})".format(
            ", ".join(others)))

    shutil.rmtree(BASE, ignore_errors=True)
    if wrong:
        print("\n{} problem(s)".format(wrong))
        return 1
    if min(ratios) <= 1.0:
        print("\ngrowth four did not pay on every shape")
        return 1
    print("\nfour is the better factor, and the guard fails when it changes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
