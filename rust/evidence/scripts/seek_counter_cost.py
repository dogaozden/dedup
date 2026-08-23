#!/usr/bin/env python3
"""What the seek counter costs the profile that is actually scored.

The counter takes a lock on every read block, and `read_block` is the hot path
of the default profile — the one RBENCH scores and the one the conformance gate
grades. Two questions follow, and reasoning about them is not the standard this
project holds:

1. Does anything the harness measures move? `bytes_read`, `opens` and the
   groups themselves are compared against a build of `main` on the same tree.
   These must be identical, not close.
2. Does it cost time? Medians of several runs at one reader and at eight, page
   cache warm.

A wall-clock number from a shared machine is indicative, not a gate — another
agent's I/O lands in it, and one run of this script under load reported 0.18x
and 1.29x on the two halves of the same comparison, which is noise and nothing
else. So the spread of the samples and the machine's load average are printed
beside the median, and no conclusion is drawn from the timing alone. The
identity check in (1) is the part that does not care who else is running.

Usage: seek_counter_cost.py <path-to-main-dedup-rs> <path-to-this-dedup-rs> <scratch-dir>
"""

import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import time

RUNS = 7
WARMUP = 2


def build(corpus: str) -> str:
    """Many small files and a few long ones — both shapes of read_block use."""
    os.makedirs(corpus, exist_ok=True)
    rng = random.Random(3)
    page = bytes(rng.getrandbits(8) for _ in range(4096))
    for index in range(4000):
        with open(os.path.join(corpus, "s{:05d}.bin".format(index)), "wb") as fh:
            fh.write(page[: 2048 + (index % 8) * 256] if index % 3 else page)
    for index in range(12):
        with open(os.path.join(corpus, "big{:02d}.bin".format(index)), "wb") as fh:
            fh.write(page * (12 * 1024 // 4))
    total = sum(os.path.getsize(os.path.join(corpus, name))
                for name in os.listdir(corpus))
    return "{} MiB, {} files".format(total // (1 << 20), len(os.listdir(corpus)))


def scan(binary: str, corpus: str, report: str, jobs: int):
    start = time.perf_counter()
    subprocess.run([binary, corpus, "--report-json", report, "--min-size", "1",
                    "--no-cache", "-j", str(jobs)], check=True, capture_output=True)
    seconds = time.perf_counter() - start
    with open(report) as fh:
        return seconds, json.load(fh)


def main() -> int:
    if len(sys.argv) != 4:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    before, after, base = (os.path.abspath(p) for p in sys.argv[1:])
    base = os.path.join(base, "feat-seek-cost")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    corpus = os.path.join(base, "corpus")
    print("corpus: " + build(corpus))
    print("load average: {:.2f} {:.2f} {:.2f}  (this machine is shared; a "
          "figure above about 1 makes the timings below indicative only)"
          .format(*os.getloadavg()))
    report = os.path.join(base, "report.json")
    binaries = [("main", before), ("this branch", after)]

    wrong = 0
    for jobs in (1, 8):
        print("\n-j {}   ({} runs each, page cache warm, medians)".format(jobs, RUNS))
        seen = {}
        for name, binary in binaries:
            for _ in range(WARMUP):
                scan(binary, corpus, report, jobs)
            times = []
            for _ in range(RUNS):
                seconds, said = scan(binary, corpus, report, jobs)
                times.append(seconds)
            seen[name] = (statistics.median(times), said)
            print("  {:<12} {:7.1f} ms  (min {:.1f}, max {:.1f})   bytes_read {}  "
                  "opens {}  groups {}".format(
                      name, statistics.median(times) * 1000, min(times) * 1000,
                      max(times) * 1000, said["bytes_read"], said["opens"],
                      len(said["groups"])))
        old, new = seen["main"], seen["this branch"]
        for field in ("bytes_read", "opens", "files_seen", "bytes_seen"):
            if old[1][field] != new[1][field]:
                print("  MOVED: {} was {}, is {}".format(field, old[1][field], new[1][field]))
                wrong += 1
        if sorted(map(sorted, old[1]["groups"])) != sorted(map(sorted, new[1]["groups"])):
            print("  MOVED: the groups are not the same groups")
            wrong += 1
        print("  -> this branch / main = {:.3f}x wall clock (indicative); "
              "bytes_read, opens, files_seen, bytes_seen and the groups all "
              "identical (exact)".format(new[0] / old[0]))

    shutil.rmtree(base, ignore_errors=True)
    print()
    if wrong:
        print("{} counter(s) moved: the default profile is not untouched".format(wrong))
        return 1
    print("nothing the harness measures moved")
    return 0


if __name__ == "__main__":
    sys.exit(main())
