#!/usr/bin/env python3
"""What the progress line costs a run that never sees it: nothing.

RBENCH and the conformance harness both pipe stderr, so both take the branch
where the pulse is off — no thread, no writes, one null check per file. The
claim is that this is unmeasurable, and the claim is worth only as much as the
measurement behind it.

Two things are compared against a build of `main`, on the same tree, with
stderr piped, alternating so that a warming disk or a busy machine drifts
through both sides equally:

  * the counters the conformance protocol calls honesty fields — opens,
    bytes_read, files_seen, bytes_seen — which must be identical; and
  * wall-clock, reported as a median and a spread, which must be inside the
    noise each build shows against itself.

`seeks` is compared only at one reader. With more, concurrent readers
interleave and count against each other, and `main` disagrees with itself
run to run — a comparison there would be measuring the scheduler.

Usage: m5_2_inertness.py <mine> <main> <scratch-dir> [repeats]
"""

import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import time

MIB = 1024 * 1024
COUNTERS = ("opens", "bytes_read", "files_seen", "bytes_seen")


def build(root, bigs=8, mib=24, smalls=30000):
    """A shape with work in every stage: big pairs the comparison must read
    end to end, same-size files it has to separate, and a crowd of small ones
    the small-file stage settles.

    Deliberately file-heavy. What the pulse adds to a run that cannot see it is
    one branch per file, so files — not bytes — are what a measurement has to
    have plenty of before it means anything.
    """
    rng = random.Random(20260823)
    page = bytes(rng.getrandbits(8) for _ in range(65536))
    os.makedirs(os.path.join(root, "many"))
    tail = page * (mib * MIB // 65536 - 1)
    for pair in range(bigs):
        payload = bytes([pair]) * 65536 + tail
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(payload)
    # Same size, different bytes: they part company in the head stage.
    for n in range(500):
        with open(os.path.join(root, "many", "same-%03d.bin" % n), "wb") as fh:
            fh.write(bytes([n % 256]) * 4096 + page[:60 * 1024])
    for n in range(smalls):
        with open(os.path.join(root, "many", "s%05d.bin" % n), "wb") as fh:
            fh.write(page[: 8192 if n % 3 else 4096])


def once(binary, root, home, jobs):
    where = os.path.join(home, "report.json")
    command = [os.path.abspath(binary), os.path.abspath(root),
               "--min-size", "1", "--no-cache", "--list",
               "-j", str(jobs), "--report-json", where]
    environment = dict(os.environ, HOME=home,
                       DEDUP_CACHE_DIR=os.path.join(home, "cache"))
    started = time.monotonic()
    run = subprocess.run(command, capture_output=True, timeout=600,
                         env=environment)
    elapsed = time.monotonic() - started
    if run.returncode != 0:
        raise SystemExit("{} exited {}".format(binary, run.returncode))
    if run.stderr:
        raise SystemExit("{} wrote to a piped stderr: {!r}".format(
            binary, run.stderr[:200]))
    with open(where) as fh:
        said = json.load(fh)
    return elapsed, said


def trimmed(times):
    """The runs with the slowest tenth dropped.

    The noise floor this gate compares against used to be the full range, and
    on a shared machine one six-second outlier made every difference "inside
    the noise" — a gate that cannot fail is not a gate. Only the slow tail is
    dropped: a run cannot be spuriously fast, so the fast end is real.
    """
    ordered = sorted(times)
    keep = max(2, len(ordered) - max(1, len(ordered) // 10))
    return ordered[:keep]


def spread(times):
    kept = trimmed(times)
    return "{:.3f}s median, {:.3f}–{:.3f} over the fastest {} of {}".format(
        statistics.median(times), min(kept), max(kept), len(kept), len(times))


def main():
    if not 4 <= len(sys.argv) <= 5:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    mine, was, base = sys.argv[1], sys.argv[2], os.path.abspath(sys.argv[3])
    repeats = int(sys.argv[4]) if len(sys.argv) > 4 else 7

    base = os.path.join(base, "m5_2-inertness")
    shutil.rmtree(base, ignore_errors=True)
    root, home = os.path.join(base, "tree"), os.path.join(base, "home")
    os.makedirs(root)
    os.makedirs(home)
    build(root)

    failed = 0
    for jobs in (1, 8):
        print("== stderr piped, -j{} ==".format(jobs))
        # One of each, thrown away: the first read of a tree is the disk
        # warming, and whichever build went first would wear it.
        once(mine, root, home, jobs)
        once(was, root, home, jobs)

        timings = {"m5.2": [], "main": []}
        reports = {}
        for _ in range(repeats):
            for label, binary in (("m5.2", mine), ("main", was)):
                elapsed, said = once(binary, root, home, jobs)
                timings[label].append(elapsed)
                reports.setdefault(label, said)

        fields = COUNTERS + (("seeks",) if jobs == 1 else ())
        for field in fields:
            here, there = reports["m5.2"][field], reports["main"][field]
            same = "same" if here == there else "DIFFERENT"
            print("  {:<12} {:>14} {:>14}   {}".format(
                field, here, there, same))
            failed += here != there
        groups = [sorted(sorted(g) for g in reports[side]["groups"])
                  for side in ("m5.2", "main")]
        print("  {:<12} {:>14} {:>14}   {}".format(
            "groups", len(groups[0]), len(groups[1]),
            "same" if groups[0] == groups[1] else "DIFFERENT"))
        failed += groups[0] != groups[1]
        if jobs != 1:
            print("  seeks        {:>14} {:>14}   not compared: nondeterministic "
                  "above one reader".format(reports["m5.2"]["seeks"],
                                            reports["main"]["seeks"]))

        for label in ("m5.2", "main"):
            print("  {:<12} {}".format(label, spread(timings[label])))
        here = statistics.median(timings["m5.2"])
        there = statistics.median(timings["main"])
        # The noise floor: how far each build's own runs move around their
        # median, with the slowest tenth dropped so one outlier cannot excuse a
        # real difference. A difference smaller than this is not a difference.
        noise = max(max(trimmed(t)) - min(trimmed(t)) for t in timings.values())
        print("  {:<12} {:+.1f}% ({:+.3f}s), against a {:.3f}s trimmed spread "
              "within a build".format("difference", (here - there) / there * 100,
                                      here - there, noise))
        if abs(here - there) > noise:
            print("  the difference is outside the noise")
            failed += 1
        print()

    shutil.rmtree(base, ignore_errors=True)
    if failed:
        print("{} measurement(s) disagree".format(failed))
        return 1
    print("the pulse costs a piped run nothing that can be measured")
    return 0


if __name__ == "__main__":
    sys.exit(main())
