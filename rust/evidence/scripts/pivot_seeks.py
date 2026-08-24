#!/usr/bin/env python3
"""What the pivot-window comparison costs, against the sweep it replaces.

M7-PIVOT.md asks for one number and its price: on a generated multi-group
corpus read with `--profile spinning`, the seeks this branch issues against the
seeks `main` issues, with `bytes_read` and `opens` beside them. Seeks are what a
spinning disk charges for; bytes are what SPIN.md says the profile is allowed to
trade for them, so a fall in seeks bought with a rise in bytes has to be shown
as such rather than reported alone.

Every run is at `-j 1`. Concurrent readers interleave and their interleaving
belongs to the scheduler, not to the algorithm; at one reader the counts are a
property of the pipeline and repeat exactly, which each shape is checked for.

`--profile flash` is run beside them as the reference the conformance gate
grades, and every shape asserts that all three runs report the same groups. A
seek count is worth nothing if the answer moved.

No wall clock is measured here and none is claimed. SPIN.md forbids an HDD
speed claim without an HDD, and no rotational drive is available to an agent.

Corpora are generated under `rust/target/` in this worktree and removed after
each shape. Nothing under `/Volumes` is read or written.

Run from the repository root:

    python3 rust/evidence/scripts/pivot_seeks.py <path-to-main-dedup-rs>
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
BASE = os.path.join(REPO, "rust", "target", "m7", "seek-corpora")

MIB = 1024 * 1024


def page(seed: int) -> bytes:
    rng = random.Random(seed)
    return bytes(rng.getrandbits(8) for _ in range(4096))


def write(path: str, body: bytes) -> None:
    with open(path, "wb") as fh:
        fh.write(body)


def copies(root: str, stem: str, body: bytes, count: int) -> None:
    for index in range(count):
        write(os.path.join(root, "{}{:02d}.bin".format(stem, index)), body)


def one_wide_group(root: str) -> str:
    """Twelve copies of one long file — the shape the profile exists for."""
    copies(root, "wide", page(1) * (64 * MIB // 4096), 12)
    return "one group of 12 x 64 MiB"


def several_groups(root: str) -> str:
    """Three groups, each wider than the cohort cap the sweep replaces."""
    for group in range(3):
        copies(root, "g{}c".format(group), page(10 + group) * (48 * MIB // 4096), 10)
    return "3 groups of 10 x 48 MiB"


def few_and_long(root: str) -> str:
    """A USB disk full of video: not many files, each of them enormous."""
    for group in range(2):
        copies(root, "film{}t".format(group), page(20 + group) * (256 * MIB // 4096), 3)
    return "2 groups of 3 x 256 MiB"


def parting_groups(root: str) -> str:
    """Groups that share a long head and then part.

    The residue is the part of the sweep that reads bytes a second time, so a
    corpus without one flatters it. Here every file in a band is identical for
    its first 40 MiB and the bands diverge after that, which is the shape stage
    3's head probe cannot separate and stage 4 does not run under `spinning`.
    """
    shared = page(30) * (40 * MIB // 4096)
    for band in range(3):
        body = shared + page(40 + band) * (24 * MIB // 4096)
        copies(root, "band{}m".format(band), body, 6)
    write(os.path.join(root, "lonely.bin"), shared + page(99) * (24 * MIB // 4096))
    return "3 bands of 6 x 64 MiB sharing 40 MiB, plus one unique"


def small_and_many(root: str) -> str:
    """The shape M6 recorded the profile losing slightly on, kept for honesty."""
    for group in range(4):
        copies(root, "s{}c".format(group), page(50 + group) * (20 * MIB // 4096), 4)
    return "4 groups of 4 x 20 MiB"


def one_long_pair(root: str) -> str:
    """Two copies of one very long file, where the window ceiling is reached.

    The ratio grows with the file: a sweep pays a head movement per member per
    window and lockstep pays one per member per chunk, so the further past
    `PIVOT_WINDOW` a file runs the wider the gap. This is the largest shape the
    free space on this machine allowed.
    """
    copies(root, "half", page(70) * (512 * MIB // 4096), 2)
    return "one group of 2 x 512 MiB"


def classes_in_one_run(root: str) -> str:
    """Four content classes in one size, each file shorter than one chunk.

    The differential calls this shape `cohort-boundary` and it is where a
    pivot sweep costs the most: every class needs a pass of its own, and a file
    that fits inside one comparison chunk gives the residue nothing to skip.
    It is here because a corpus of long files alone would flatter the sweep.
    """
    shared = page(60) * (2 * MIB // 4096)
    for band in range(4):
        body = bytes(shared[:-1024]) + bytes([65 + band]) * 1024
        copies(root, "band{}c".format(band), body, 4)
    return "16 x 2 MiB in 4 classes, parting in the last kilobyte"


SHAPES = [
    ("one-wide-group", one_wide_group),
    ("several-groups", several_groups),
    ("few-and-long", few_and_long),
    ("one-long-pair", one_long_pair),
    ("parting-groups", parting_groups),
    ("small-and-many", small_and_many),
    ("classes-in-one-run", classes_in_one_run),
]


def scan(binary: str, root: str, report: str, profile: str) -> dict:
    command = [binary, root, "--report-json", report, "--min-size", "1",
               "--no-cache", "--profile", profile, "-j", "1"]
    proc = subprocess.run(command, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("dedup-rs exited {}: {}".format(proc.returncode, proc.stderr))
    with open(report) as fh:
        return json.load(fh)


def twice(binary: str, root: str, report: str, profile: str, what: str) -> dict:
    first = scan(binary, root, report, profile)
    again = scan(binary, root, report, profile)
    for field in ("seeks", "bytes_read", "opens"):
        if first[field] != again[field]:
            raise SystemExit("{} is not deterministic at -j 1 on {}: {} then {}"
                             .format(what, root, first[field], again[field]))
    return first


def groups_of(said: dict) -> list:
    return sorted(sorted(group) for group in said["groups"])


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    main_binary = os.path.abspath(sys.argv[1])
    for binary in (main_binary, BRANCH):
        if not os.path.exists(binary):
            raise SystemExit("build it first: " + binary)

    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    report = os.path.join(BASE, "report.json")
    print("Seeks, one reader (-j 1). main is the capped cohort sweep under "
          "--profile spinning;\n`ours` is this branch under the same flag. "
          "`flash` is the profile the gate grades,\nfor comparison with M6, "
          "whose table read flash against spinning.\n")
    print("Whole-scan counts: stages 1-4 are untouched by this branch and cost "
          "a head movement\nper file between them, so the stage-5 ratio is "
          "higher than the column below.\n")
    print("{:<19} {:>6} {:>6} {:>6} {:>6} {:>7} {:>7} {:>9}".format(
        "shape", "groups", "flash", "main", "ours", "vs main", "vs flash",
        "opens"))
    rows = []
    wrong = 0
    for name, build in SHAPES:
        root = os.path.join(BASE, name)
        os.makedirs(root)
        described = build(root)
        old = twice(main_binary, root, report, "spinning", "main --profile spinning")
        new = twice(BRANCH, root, report, "spinning", "this branch --profile spinning")
        flash = twice(BRANCH, root, report, "flash", "this branch --profile flash")
        if not (groups_of(old) == groups_of(new) == groups_of(flash)):
            print("  {}: THE GROUPS MOVED".format(name))
            wrong += 1
        if new["opens"] != old["opens"]:
            opens = "{}->{}".format(old["opens"], new["opens"])
        else:
            opens = str(new["opens"])
        print("{:<19} {:>6} {:>6} {:>6} {:>6} {:>6.1f}x {:>7.1f}x {:>9}".format(
            name, len(new["groups"]), flash["seeks"], old["seeks"], new["seeks"],
            old["seeks"] / max(new["seeks"], 1),
            flash["seeks"] / max(new["seeks"], 1), opens))
        rows.append((name, described, old, new, flash))
        shutil.rmtree(root, ignore_errors=True)

    print("\nshape                what it is")
    for name, described, _, _, _ in rows:
        print("  {:<18} {}".format(name, described))

    print("\nbytes. SPIN.md lets `spinning` read MORE than the profile the gate "
          "grades — it trades bytes for seeks by design — so what matters here "
          "is this branch against main, which is the sweep it replaces:")
    print("  {:<18} {:>13} {:>13} {:>13} {:>9}".format(
        "shape", "flash MiB", "main MiB", "our MiB", "ours/main"))
    for name, _, old, new, flash in rows:
        print("  {:<18} {:>13.1f} {:>13.1f} {:>13.1f} {:>8.2f}x".format(
            name, flash["bytes_read"] / MIB, old["bytes_read"] / MIB,
            new["bytes_read"] / MIB,
            new["bytes_read"] / max(old["bytes_read"], 1)))

    shutil.rmtree(BASE, ignore_errors=True)
    worst = min(o["seeks"] / max(n["seeks"], 1) for _, _, o, n, _ in rows)
    best = max(o["seeks"] / max(n["seeks"], 1) for _, _, o, n, _ in rows)
    print("\nseeks: between {:.1f}x and {:.1f}x fewer than main on every shape "
          "above".format(worst, best))
    if wrong:
        print("{} shape(s) wrong".format(wrong))
        return 1
    if worst <= 1.0:
        print("a shape did not improve")
        return 1
    print("groups identical across main, this branch, and --profile flash, "
          "on every shape")
    print("HDD wall-clock unvalidated: no rotational drive is available here.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
