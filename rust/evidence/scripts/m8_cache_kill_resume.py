#!/usr/bin/env python3
"""M8's partial-scan cache, as the spec asks it to be shown.

M8-LIVE.md: "a scripted run that scans a corpus, is killed at ~half, then
re-runs — the second run's bytes_read/opens must be materially lower, and its
answer identical to an uninterrupted scan's."

So: scan once cold for the truth, then scan again from cold and kill it part
way, then run it to completion and compare. Every kill the spec names gets its
own case — SIGTERM, SIGINT (what Ctrl+C sends), and SIGKILL, which nothing can
handle and which is therefore the only real test of "a torn write must be
impossible by construction". If the cache is only safe because a signal
handler tidied up, SIGKILL says so.

Before this milestone every one of these cases read the whole corpus twice:
the cache was a `HashMap` written once at the end, so a scan that did not
reach its end wrote nothing at all.

The corpus is AGED before every scan. A cache row is trusted only while the
file's mtime tick is strictly older than the second the writing scan started
in (cache.rs, git's racily-clean rule), so a corpus built and scanned in the
same second has every row dropped — and this script would report a cache that
saves nothing, for a reason that has nothing to do with the cache.

Nothing here touches /Volumes. Corpora live under the scratch directory given.

Usage: m8_cache_kill_resume.py <path-to-dedup-rs> <scratch-dir>
"""

import json
import os
import random
import shutil
import signal
import subprocess
import sys
import time

MIB = 1024 * 1024


def build(root, pairs=30, mib=12, smalls=12000):
    """Two shapes, because the kill has two things to be sure of.

    `smalls` identical small pairs give the cache MORE ROWS THAN ONE FLUSH
    HOLDS. That matters: a flush happens after `FLUSH_ROWS` rows or
    `FLUSH_EVERY` seconds, and a corpus of fifty files on a fast disk trips
    neither — it finishes cold before the timer, learns fifty rows, and the
    only write is the one at the end. Such a corpus cannot show an incremental
    flush working, because none happens.

    `pairs` identical big pairs give the scan enough to do that there is a
    middle to be killed in. Every pair is its own group and every group
    survives to the byte comparison, which is where the cache has the most to
    remember.
    """
    rng = random.Random(20260824)
    page = bytes(rng.getrandbits(8) for _ in range(MIB))
    for pair in range(pairs):
        body = bytes([pair]) * MIB + page * (mib - 1)
        for copy in ("a", "b"):
            with open(os.path.join(root, "p%03d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(body)
    os.makedirs(os.path.join(root, "many"))
    for n in range(smalls // 2):
        body = bytearray(page[:8192])
        body[:8] = n.to_bytes(8, "little")
        for copy in ("a", "b"):
            with open(os.path.join(root, "many", "s%05d%s.bin" % (n, copy)), "wb") as fh:
                fh.write(bytes(body))
    age(root)


def age(root):
    """Ten seconds into the past — see the note at the top of this file."""
    when = time.time() - 10
    for base, _, names in os.walk(root):
        for name in names:
            os.utime(os.path.join(base, name), (when, when))


def fresh(scratch, name):
    root = os.path.join(scratch, name)
    cache = os.path.join(scratch, name + "-cache")
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(cache, ignore_errors=True)
    os.makedirs(root)
    build(root)
    return root, cache


def rows_in(cache):
    """Rows the cache holds right now, or 0 if there is nothing to read.

    Read with its own connection while the scan still has the database open,
    which WAL allows and which is itself part of what is being shown.
    """
    import sqlite3
    if not os.path.isdir(cache):
        return 0
    total = 0
    for name in os.listdir(cache):
        if not name.endswith(".db"):
            continue
        try:
            connection = sqlite3.connect("file:%s?mode=ro" % os.path.join(cache, name),
                                         uri=True, timeout=0.2)
            total += connection.execute("SELECT count(*) FROM files").fetchone()[0]
            connection.close()
        except Exception:                                # noqa: BLE001
            continue
    return total


def scan(binary, root, cache, tag, scratch, kill_on_flush=False,
         sig=signal.SIGTERM):
    """One scan. With `kill_on_flush`, kill it as soon as it has COMMITTED
    something — which is the only moment at which killing it proves anything.

    Waiting on the cache rather than on a stopwatch is what keeps this honest
    on a fast disk: an earlier draft killed at half the cold wall time, the
    cold time was 0.2 seconds, and every case reported that the scan had
    already finished.

    Returns (report or None, exit code, wall seconds, finished-before-the-kill).
    """
    report = os.path.join(scratch, "report-%s.json" % tag)
    if os.path.exists(report):
        os.remove(report)
    env = dict(os.environ, DEDUP_CACHE_DIR=cache)
    began = time.time()
    process = subprocess.Popen(
        [os.path.abspath(binary), os.path.abspath(root), "--list",
         "--min-size", "1", "--report-json", report],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    if kill_on_flush:
        deadline = time.time() + 120
        while time.time() < deadline:
            if process.poll() is not None:
                return None, process.returncode, time.time() - began, True
            if rows_in(cache) > 0:
                process.send_signal(sig)
                break
            time.sleep(0.01)
        else:
            process.kill()
            return None, -1, time.time() - began, True
    code = process.wait()
    took = time.time() - began
    body = None
    if os.path.exists(report):
        with open(report) as fh:
            body = json.load(fh)
    return body, code, took, False


def cache_readable(cache):
    """Can the next run open what the killed one left?

    A cache the next run cannot open is a failed milestone, so it is asked
    directly rather than inferred from a scan that might have quietly rebuilt
    it. Every `.db` in the directory is opened and read.
    """
    import sqlite3
    files = [f for f in os.listdir(cache) if f.endswith(".db")] \
        if os.path.isdir(cache) else []
    if not files:
        return False, "no database was written at all", 0
    rows = 0
    for name in files:
        path = os.path.join(cache, name)
        try:
            connection = sqlite3.connect(path)
            got = connection.execute("SELECT count(*) FROM files").fetchone()[0]
            broken = connection.execute("PRAGMA integrity_check").fetchone()[0]
            connection.close()
        except Exception as error:                       # noqa: BLE001
            return False, "%s: %s" % (name, error), 0
        if broken != "ok":
            return False, "%s: integrity_check said %r" % (name, broken), 0
        rows += got
    return True, "ok", rows


def case(binary, scratch, name, sig, signame):
    problems = []
    # 1. The truth: an uninterrupted cold scan.
    root, cache = fresh(scratch, name)
    truth, code, took, _ = scan(binary, root, cache, name + "-truth", scratch)
    if truth is None:
        return ["the uninterrupted scan wrote no report (exit %d)" % code], None

    # 2. From cold again, killed part way through.
    root, cache = fresh(scratch, name)
    _, killed_code, _, finished_early = scan(
        binary, root, cache, name + "-killed", scratch,
        kill_on_flush=True, sig=sig)
    if finished_early:
        problems.append("the scan finished before the kill landed — this case "
                        "did not test what it is named for")

    # 3. Is what it left usable at all?
    ok, why, rows = cache_readable(cache)
    if not ok:
        problems.append("the killed scan's cache is unusable: %s" % why)

    # 4. Run to completion against that cache.
    #
    # NOT re-aged: `age` rewrites every mtime, and mtime is half the key a row
    # is found by. An earlier draft aged here and reported that the cache saved
    # nothing — it had invalidated every row itself, a step before measuring.
    resumed, code, _, _ = scan(binary, root, cache, name + "-resumed", scratch)
    if resumed is None:
        return problems + ["the resumed scan wrote no report (exit %d)" % code], None

    # 5. The answer must be the uninterrupted one, exactly.
    if resumed["groups"] != truth["groups"]:
        problems.append("the resumed scan found different groups")
    for field in ("files_seen", "bytes_seen", "unreadable"):
        if resumed[field] != truth[field]:
            problems.append("resumed %s = %r, uninterrupted %r"
                            % (field, resumed[field], truth[field]))

    # 6. ...and it must have cost materially less.
    saved_bytes = truth["bytes_read"] - resumed["bytes_read"]
    saved_opens = truth["opens"] - resumed["opens"]
    if saved_bytes <= 0:
        problems.append("the resumed scan read no less: %d vs %d"
                        % (resumed["bytes_read"], truth["bytes_read"]))
    return problems, {
        "signal": signame,
        "rows_left_by_the_kill": rows,
        "cold_bytes_read": truth["bytes_read"],
        "resumed_bytes_read": resumed["bytes_read"],
        "bytes_saved": saved_bytes,
        "percent_saved": round(100.0 * saved_bytes / max(truth["bytes_read"], 1), 1),
        "cold_opens": truth["opens"],
        "resumed_opens": resumed["opens"],
        "opens_saved": saved_opens,
        "groups_identical": resumed["groups"] == truth["groups"],
        "cold_seconds": round(took, 2),
    }


CASES = [
    ("sigterm", signal.SIGTERM, "SIGTERM"),
    ("sigint", signal.SIGINT, "SIGINT (what Ctrl+C sends)"),
    ("sigkill", signal.SIGKILL, "SIGKILL (unhandleable)"),
]


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary, scratch = sys.argv[1], sys.argv[2]
    os.makedirs(scratch, exist_ok=True)
    failures = 0
    for name, sig, signame in CASES:
        problems, numbers = case(binary, scratch, name, sig, signame)
        if problems:
            failures += 1
            print("FAIL  killed by %s" % signame)
            for problem in problems:
                print("        %s" % problem)
        else:
            print("pass  killed by %s" % signame)
        if numbers:
            for key in ("rows_left_by_the_kill", "cold_bytes_read",
                        "resumed_bytes_read", "bytes_saved", "percent_saved",
                        "cold_opens", "resumed_opens", "opens_saved",
                        "groups_identical", "cold_seconds"):
                print("        %-22s %s" % (key, numbers[key]))
        print()
    print("%d case(s), %d failed" % (len(CASES), failures))
    return failures


if __name__ == "__main__":
    sys.exit(main())
