#!/usr/bin/env python3
"""Why a run is `MIN_CHUNK` and not the comparison chunk.

A window is what a head movement buys; a run is how much of it is read at a
time. Runs inside a window carry straight on from each other, so their length
costs no head movement — but it decides how closely a parting is placed, and so
how many bytes a member reads past one and how many it reads a second time when
its class is compared again.

That is an argument, and an argument is not a measurement. So the same corpus is
read three ways: by `main`'s capped cohort comparison, by this branch, and by
this branch built with `PIVOT_RUN = SLOW_CHUNK` — a run as long as the chunk,
which is the obvious implementation and the one this constant exists to reject.

The variant is built from a copy of the tree in scratch, with its own
CARGO_TARGET_DIR, so no variant binary is ever produced in the working checkout.

Nothing under `/Volumes` is touched.

Run from the repository root:

    python3 rust/evidence/scripts/pivot_run_size.py <path-to-main-dedup-rs>
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
BASE = os.path.join(REPO, "rust", "target", "m7", "run-size")

MIB = 1024 * 1024
WRITTEN = "pub const PIVOT_RUN: usize = MIN_CHUNK;"
MUTATED = "pub const PIVOT_RUN: usize = SLOW_CHUNK;"


def build_corpus(root: str) -> str:
    """`cohort-boundary`'s shape: several content classes in one size, each
    file shorter than one comparison chunk, parting in the last kilobyte."""
    os.makedirs(root)
    rng = random.Random(60)
    shared = bytes(rng.getrandbits(8) for _ in range(4096)) * (2 * MIB // 4096)
    for band in range(4):
        body = shared[:-1024] + bytes([65 + band]) * 1024
        for index in range(4):
            with open(os.path.join(root, "b{}c{:02d}.bin".format(band, index)), "wb") as fh:
                fh.write(body)
    return "16 x 2 MiB in 4 classes, parting in the last kilobyte"


def scan(binary: str, root: str, report: str) -> dict:
    proc = subprocess.run(
        [binary, root, "--report-json", report, "--min-size", "1", "--no-cache",
         "--profile", "spinning", "-j", "1"],
        capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("dedup-rs exited {}: {}".format(proc.returncode, proc.stderr))
    with open(report) as fh:
        return json.load(fh)


def variant_binary() -> str:
    """This branch, built with a run as long as the chunk."""
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
    target = os.path.join(BASE, "target")
    environment = dict(os.environ, CARGO_TARGET_DIR=target)
    built = subprocess.run(["cargo", "build", "--release"], cwd=tree,
                           capture_output=True, text=True, env=environment)
    if built.returncode != 0:
        raise SystemExit("the variant did not build:\n" + built.stderr[-2000:])
    return os.path.join(target, "release", "dedup-rs")


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    main_binary = os.path.abspath(sys.argv[1])
    if not os.path.exists(main_binary) or not os.path.exists(BRANCH):
        raise SystemExit("build both first")

    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    root = os.path.join(BASE, "tree")
    print(build_corpus(root) + ", --profile spinning, one reader.\n")
    long_run = variant_binary()
    report = os.path.join(BASE, "report.json")

    sides = [
        ("main, capped cohorts", main_binary),
        ("PIVOT_RUN = SLOW_CHUNK", long_run),
        ("PIVOT_RUN = MIN_CHUNK", BRANCH),
    ]
    print("{:<24} {:>12} {:>9} {:>7}".format("read as", "bytes_read", "MiB", "seeks"))
    said = {}
    for label, binary in sides:
        got = scan(binary, root, report)
        said[label] = got
        print("{:<24} {:>12} {:>9.1f} {:>7}".format(
            label, got["bytes_read"], got["bytes_read"] / MIB, got["seeks"]))

    shape = sorted(map(sorted, said[sides[0][0]]["groups"]))
    wrong = [label for label, _ in sides
             if sorted(map(sorted, said[label]["groups"])) != shape]
    shutil.rmtree(BASE, ignore_errors=True)
    if wrong:
        print("\nTHE GROUPS MOVED: " + ", ".join(wrong))
        return 1
    print("\nall three report the same groups")
    chunky = said["PIVOT_RUN = SLOW_CHUNK"]["bytes_read"]
    ours = said["PIVOT_RUN = MIN_CHUNK"]["bytes_read"]
    capped = said["main, capped cohorts"]["bytes_read"]
    print("a run as long as the chunk reads {:.2f}x what this branch reads, and "
          "{:.2f}x what\nmain's capped comparison reads; at MIN_CHUNK it is "
          "{:.2f}x main".format(chunky / ours, chunky / capped, ours / capped))
    if ours >= chunky or ours >= capped:
        print("the short run did not pay")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
