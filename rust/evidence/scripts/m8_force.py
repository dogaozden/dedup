#!/usr/bin/env python3
"""`--force` against `--no-cache`, told apart by the counters.

M8-LIVE.md: `--force` ignores everything the cache knows for this scan — reads
no verdicts from it — but WRITES fresh learnings as the scan proves things.
`--no-cache` neither reads nor writes.

Three flags, four runs, and the counters have to distinguish all of them:

  * a warm run reads almost nothing, because the cache answers for it;
  * `--force` reads everything a cold run reads, because it believes nothing;
  * `--no-cache` reads everything too — and, unlike `--force`, LEAVES THE
    CACHE ALONE. That is the whole difference between the two flags, and it
    is invisible in bytes_read: it only shows in what the database says
    afterwards. So the database is read directly.

The corpus is aged before every scan: a cache row is trusted only while the
file's mtime tick is strictly older than the second the writing scan started
in (cache.rs, git's racily-clean rule), so a corpus built and scanned inside
one second has every row dropped and every run here would look identical for
a reason that has nothing to do with the flags.

Nothing here touches /Volumes. Corpora live under the scratch directory given.

Usage: m8_force.py <path-to-dedup-rs> <scratch-dir>
"""

import json
import os
import random
import shutil
import sqlite3
import subprocess
import sys
import time

MIB = 1024 * 1024


def build(root, pairs=8, mib=4):
    """Identical pairs big enough to reach the byte comparison, which is the
    only stage that learns a whole-file digest — and so the only one whose
    work a warm run can skip entirely."""
    rng = random.Random(20260824)
    page = bytes(rng.getrandbits(8) for _ in range(MIB))
    for pair in range(pairs):
        body = bytes([pair]) * MIB + page * (mib - 1)
        for copy in ("a", "b"):
            with open(os.path.join(root, "p%02d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(body)
    when = time.time() - 10
    for name in os.listdir(root):
        os.utime(os.path.join(root, name), (when, when))


def run(binary, root, cache, tag, scratch, flags=()):
    report = os.path.join(scratch, "report-%s.json" % tag)
    subprocess.run(
        [os.path.abspath(binary), os.path.abspath(root), "--list",
         "--min-size", "1", "--report-json", report, *flags],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        env=dict(os.environ, DEDUP_CACHE_DIR=cache), check=False)
    with open(report) as fh:
        return json.load(fh)


def digests(cache):
    """(rows, rows carrying a whole-file digest) across every database."""
    if not os.path.isdir(cache):
        return 0, 0
    rows = full = 0
    for name in os.listdir(cache):
        if not name.endswith(".db"):
            continue
        connection = sqlite3.connect(os.path.join(cache, name))
        rows += connection.execute("SELECT count(*) FROM files").fetchone()[0]
        full += connection.execute(
            "SELECT count(*) FROM files WHERE full IS NOT NULL").fetchone()[0]
        connection.close()
    return rows, full


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary, scratch = sys.argv[1], sys.argv[2]
    root = os.path.join(scratch, "tree")
    cache = os.path.join(scratch, "cache")
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(root)
    build(root)

    cold = run(binary, root, cache, "cold", scratch)
    warm = run(binary, root, cache, "warm", scratch)
    forced = run(binary, root, cache, "forced", scratch, ["--force"])
    after_force = digests(cache)

    # --no-cache gets its own directory AND is the only run to touch it. An
    # earlier draft warmed this directory with an ordinary run first and then
    # blamed the sixteen rows it found on `--no-cache`, which had not written
    # one of them.
    nocache_dir = os.path.join(scratch, "cache-nocache")
    nowarm = run(binary, root, nocache_dir, "nc", scratch, ["--no-cache"])
    after_nocache = digests(nocache_dir)

    rows = [
        ("cold (nothing remembered)", cold),
        ("warm (the cache answers)", warm),
        ("--force (believes nothing)", forced),
        ("--no-cache (reads and writes nothing)", nowarm),
    ]
    print("%-32s %14s %8s %8s" % ("run", "bytes_read", "opens", "groups"))
    for name, body in rows:
        print("%-32s %14d %8d %8d"
              % (name, body["bytes_read"], body["opens"], len(body["groups"])))
    print()
    print("cache after the --force run:    %d rows, %d with a whole-file digest"
          % after_force)
    print("cache after the --no-cache run: %d rows, %d with a whole-file digest"
          % after_nocache)
    print()

    problems = []
    if warm["bytes_read"] >= cold["bytes_read"]:
        problems.append("the warm run read no less than the cold one — the "
                        "cache is not working, so nothing below means anything")
    if forced["bytes_read"] != cold["bytes_read"]:
        problems.append("--force read %d bytes where a cold run read %d: it is "
                        "believing something" % (forced["bytes_read"],
                                                 cold["bytes_read"]))
    if forced["opens"] != cold["opens"]:
        problems.append("--force opened %d files where a cold run opened %d"
                        % (forced["opens"], cold["opens"]))
    if forced["groups"] != cold["groups"]:
        problems.append("--force found different groups")
    # The distinction that is the reason for two flags: --force WRITES.
    if after_force[1] == 0:
        problems.append("--force wrote no whole-file digests: it is behaving "
                        "like --no-cache")
    if after_nocache[0] != 0:
        problems.append("--no-cache left %d row(s) behind" % after_nocache[0])
    if nowarm["bytes_read"] != cold["bytes_read"]:
        problems.append("--no-cache read %d where a cold run read %d"
                        % (nowarm["bytes_read"], cold["bytes_read"]))

    for problem in problems:
        print("FAIL  %s" % problem)
    if not problems:
        print("pass  --force reads like a cold run and writes like a warm one;")
        print("      --no-cache reads like a cold run and writes nothing.")
    return len(problems)


if __name__ == "__main__":
    sys.exit(main())
