#!/usr/bin/env python3
"""Count the seeks each profile issues on a synthetic wide-group corpus.

SPIN.md forbids claiming an HDD speedup without an HDD, and there is no
spinning disk an agent may touch. What can be measured on flash is the access
PATTERN — how often the read stream jumps instead of carrying on — which is the
thing a spinning disk charges for and the thing this profile changes.

Every run is at `-j 1`. Concurrent readers interleave, and their interleaving is
the scheduler's, not the algorithm's; at one reader the count is a property of
the pipeline alone and repeats exactly. The default-jobs numbers are printed
underneath for scale, and are not claimed to be stable.

The corpus is one group of N identical files per tree, each file longer than
both the sampling threshold and one comparison round, so every stage the
profile changes is exercised: the tail probe, the eight sample windows, and the
round-robin of a group wider than the cohort cap.

Run from the repository root:

    python3 rust/evidence/scripts/spinning_seeks.py
"""

import json
import os
import random
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
CANDIDATE = os.path.join(REPO, "rust", "target", "release", "dedup-rs")
SCRATCH = ("/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/"
           "620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad")
BASE = os.path.join(SCRATCH, "feat-spin-seeks")

MIB = 1024 * 1024
# Every size is past SAMPLE_SPAN (8 MiB) and past SLOW_CHUNK (16 MiB), so the
# sample stage and a multi-round comparison both happen. The cohort cap is 8:
# the first three widths sit either side of it, and the last case is the shape
# a USB disk full of video actually has — few files, each of them long.
CASES = [(4, 20 * MIB), (8, 20 * MIB), (16, 20 * MIB), (32, 20 * MIB),
         (6, 100 * MIB)]


def build(root: str, width: int, size: int) -> None:
    os.makedirs(root)
    rng = random.Random(99)
    page = bytes(rng.getrandbits(8) for _ in range(4096))
    content = page * (size // 4096)
    for position in range(width):
        with open(os.path.join(root, "copy{:02d}.bin".format(position)), "wb") as fh:
            fh.write(content)


def scan(root: str, report: str, profile: str, jobs):
    command = [CANDIDATE, root, "--report-json", report,
               "--min-size", "1", "--no-cache", "--profile", profile]
    if jobs is not None:
        command += ["-j", str(jobs)]
    proc = subprocess.run(command, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("dedup-rs exited {}: {}".format(proc.returncode, proc.stderr))
    with open(report) as fh:
        return json.load(fh)


def main() -> int:
    if not os.path.exists(CANDIDATE):
        raise SystemExit("build it first: cargo build --release")
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    print("One group of N identical files, one reader (-j 1).\n")
    print("files    MiB   flash seeks   spinning seeks   ratio      flash MiB   "
          "spinning MiB   groups")
    rows = []
    for width, size in CASES:
        root = os.path.join(BASE, "w{}s{}".format(width, size))
        build(root, width, size)
        report = os.path.join(BASE, "report.json")
        runs = {}
        for profile in ("flash", "spinning"):
            first = scan(root, report, profile, 1)
            again = scan(root, report, profile, 1)
            if first["seeks"] != again["seeks"]:
                raise SystemExit(
                    "{} at -j 1 is not deterministic on width {}: {} then {}".format(
                        profile, width, first["seeks"], again["seeks"]))
            runs[profile] = first
        flash, spin = runs["flash"], runs["spinning"]
        if len(flash["groups"]) != 1 or len(flash["groups"][0]) != width:
            raise SystemExit("the corpus did not form one group of {}".format(width))
        if sorted(map(sorted, flash["groups"])) != sorted(map(sorted, spin["groups"])):
            raise SystemExit("the profiles disagree about the groups at width "
                             + str(width))
        rows.append((width, size, flash, spin))
        print("{:>5}  {:>5}   {:>11}   {:>14}   {:>5.1f}x   {:>9.1f}   {:>12.1f}   {:>6}"
              .format(width, size // MIB, flash["seeks"], spin["seeks"],
                      flash["seeks"] / max(spin["seeks"], 1),
                      flash["bytes_read"] / MIB, spin["bytes_read"] / MIB,
                      len(spin["groups"])))
        shutil.rmtree(root, ignore_errors=True)

    print("\nSame corpora at the default reader count, for scale only "
          "(threads interleave, so these move between runs):")
    print("files    MiB   flash seeks   spinning seeks")
    for width, size, _, _ in rows:
        root = os.path.join(BASE, "w{}s{}".format(width, size))
        build(root, width, size)
        report = os.path.join(BASE, "report.json")
        flash = scan(root, report, "flash", None)
        spin = scan(root, report, "spinning", None)
        print("{:>5}  {:>5}   {:>11}   {:>14}".format(
            width, size // MIB, flash["seeks"], spin["seeks"]))
        shutil.rmtree(root, ignore_errors=True)

    shutil.rmtree(BASE, ignore_errors=True)
    worst = min(f["seeks"] / max(s["seeks"], 1) for _, _, f, s in rows)
    best = max(f["seeks"] / max(s["seeks"], 1) for _, _, f, s in rows)
    print("\nspinning issues fewer seeks on every corpus: between {:.1f}x and "
          "{:.1f}x fewer".format(worst, best))
    print("HDD wall-clock unvalidated: no rotational drive is available here.")
    return 0 if worst > 1 else 1


if __name__ == "__main__":
    sys.exit(main())
