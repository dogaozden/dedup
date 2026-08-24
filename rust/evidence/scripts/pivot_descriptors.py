#!/usr/bin/env python3
"""What the held-descriptor table is worth to the spinning comparison.

M7-PIVOT.md requires the descriptor carry-through to compose with the new sweep
or be bypassed with a stated reason. It composes: `compare_stage`'s spinning
branch lifts a group's carried descriptors the way the flash branch always has,
and the sweep opens each member once and keeps it for every window. The capped
comparison this replaces never took them, and re-opened its pivot in every
cohort besides.

This is a gate, not a print-out: it fails if `opens` does not fall, or if a
single byte of reading moves. REVIEW-M5_2 note 1 is why it exists as a file at
all — the numbers it prints used to be a terminal probe pasted into a claim.

Nothing under `/Volumes` is touched; the tree is built under `rust/target/`.

Run from the repository root:

    python3 rust/evidence/scripts/pivot_descriptors.py <path-to-main-dedup-rs>
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
BASE = os.path.join(REPO, "rust", "target", "m7", "descriptors")

MIB = 1024 * 1024


def build(root: str) -> None:
    os.makedirs(root)
    rng = random.Random(7)
    body = bytes(rng.getrandbits(8) for _ in range(4096)) * (6 * MIB // 4096)
    for name in ("a.bin", "b.bin", "c.bin"):
        with open(os.path.join(root, name), "wb") as fh:
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


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    main_binary = os.path.abspath(sys.argv[1])
    for binary in (main_binary, BRANCH):
        if not os.path.exists(binary):
            raise SystemExit("build it first: " + binary)

    shutil.rmtree(BASE, ignore_errors=True)
    tree = os.path.join(BASE, "tree")
    build(tree)
    report = os.path.join(BASE, "report.json")
    old = scan(main_binary, tree, report)
    new = scan(BRANCH, tree, report)
    shutil.rmtree(BASE, ignore_errors=True)

    print("three 6 MiB copies, --profile spinning, one reader:")
    for label, said in (("main@cc654c4", old), ("this branch", new)):
        print('{:<15} "bytes_read": {}, "opens": {}, "seeks": {}'.format(
            label, said["bytes_read"], said["opens"], said["seeks"]))

    wrong = []
    if sorted(map(sorted, old["groups"])) != sorted(map(sorted, new["groups"])):
        wrong.append("the groups moved")
    if old["bytes_read"] != new["bytes_read"]:
        wrong.append("bytes_read moved: nothing here should change what is read")
    if new["opens"] >= old["opens"]:
        wrong.append("opens did not fall: the descriptors are not being carried")
    if new["seeks"] >= old["seeks"]:
        wrong.append("seeks did not fall")
    if wrong:
        print("\n" + "; ".join(wrong))
        return 1
    print("\none open per member instead of two, the same bytes, fewer seeks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
