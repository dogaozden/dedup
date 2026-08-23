#!/usr/bin/env python3
"""What `--profile` does when it meets the other flags.

REVIEW-M4's first finding was a mode that changed what a group meant, on a tool
that deletes files, in a combination no gate ever ran. `--profile` is such a
mode. It changes which bytes a digest covers, and it changes how a group is
compared. So the combinations get run here rather than reasoned about:

1. The cache, in both orders and mixed. The `spinning` end probe reads the head
   alone and the `flash` one reads both ends, so the two digests cover
   different bytes. If they ever shared a cache column, a scan that found one
   file's digest remembered and had to read another's would be comparing a
   head against a head-and-tail, and two identical files would land in
   different buckets: a duplicate silently lost.
2. Deletion. `--auto` under `spinning` must remove exactly the files it removes
   under `flash`, on the same tree.
3. `--stage size`, which produces groups nothing has read. The profile must not
   have opened a way past that guard.

Run from the repository root:

    python3 rust/evidence/scripts/spinning_meets_the_rest.py
"""

import json
import os
import random
import shutil
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
# A mutant build can be pointed at instead, so `spinning_mutants.py` can prove
# these checks fail when the thing they guard is removed.
CANDIDATE = os.environ.get("DEDUP_RS_CANDIDATE",
                           os.path.join(REPO, "rust", "target", "release", "dedup-rs"))
ORACLE = os.path.join(REPO, "conformance", "oracle_driver.py")
SCRATCH = ("/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/"
           "620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad")
BASE = os.path.join(SCRATCH, "feat-spin-combinations")

TAIL_BYTES = 4 * 1024
SIZE = 128 * 1024  # long enough that the flash probe reads a tail page


def content(seed: int, tail: int) -> bytes:
    rng = random.Random(seed)
    page = bytes(rng.getrandbits(8) for _ in range(4096))
    body = bytearray(page * (SIZE // 4096))
    body[SIZE - TAIL_BYTES:] = bytes([tail]) * TAIL_BYTES
    return bytes(body)


def put(root: str, name: str, seed: int, tail: int) -> None:
    """Write a file and backdate it, so a cache row written now outranks it.

    The cache distrusts a row whose file could have changed in the same second
    it was written. Backdating is how a scripted test gets a warm cache without
    sleeping through the tick.
    """
    path = os.path.join(root, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(content(seed, tail))
    old = time.time() - 3600
    os.utime(path, (old, old))


def groups(path: str):
    with open(path) as fh:
        return sorted(tuple(sorted(g)) for g in json.load(fh)["groups"])


def scan(root: str, report: str, extra, cache_dir=None):
    environment = dict(os.environ)
    if cache_dir:
        environment["DEDUP_CACHE_DIR"] = cache_dir
    command = [CANDIDATE, root, "--report-json", report, "--min-size", "1"] + extra
    proc = subprocess.run(command, capture_output=True, text=True, env=environment)
    return proc, groups(report) if os.path.exists(report) else None


def oracle(root: str, report: str):
    subprocess.run([sys.executable, ORACLE, root, "--report-json", report,
                    "--min-size", "1", "--no-cache", "-q"],
                   check=True, capture_output=True)
    return groups(report)


def build(root: str) -> None:
    """Same heads, different tails: only the tail page separates the pairs."""
    put(root, "a.bin", 1, 0xAA)
    put(root, "b.bin", 1, 0xAA)
    put(root, "c.bin", 1, 0xBB)
    put(root, "d.bin", 1, 0xCC)


def survivors(root: str):
    found = []
    for directory, _, names in os.walk(root):
        for name in names:
            found.append(os.path.relpath(os.path.join(directory, name), root))
    return sorted(found)


def case_cache_crossover(first: str, second: str) -> str:
    """Scan under one profile, add a duplicate, scan under the other."""
    root = os.path.join(BASE, "cache-{}-{}".format(first, second))
    cache_dir = os.path.join(BASE, "cache-dir-{}-{}".format(first, second))
    os.makedirs(root)
    os.makedirs(cache_dir)
    report = os.path.join(BASE, "report.json")
    build(root)

    scan(root, report, ["--profile", first], cache_dir)
    # A new copy of an existing file. Its digest has to be read this time while
    # its twin's comes back from the cache — the mixed state where two digests
    # that cover different bytes would be compared against each other.
    put(root, "e.bin", 1, 0xAA)
    _, got = scan(root, report, ["--profile", second], cache_dir)
    want = oracle(root, report)
    if got != want:
        return "FAIL: warm {} then {} reported {}, oracle {}".format(
            first, second, got, want)
    if not any(len(group) == 3 for group in got):
        return "FAIL: the three copies of a.bin did not come back as one group"
    return "ok ({} groups, the new copy joined its twins)".format(len(got))


def case_deletion_parity() -> str:
    """`--auto` must remove the same files under either profile."""
    outcomes = {}
    for profile in ("flash", "spinning"):
        root = os.path.join(BASE, "delete-" + profile)
        os.makedirs(root)
        build(root)
        put(root, "nested/a-copy.bin", 1, 0xAA)
        report = os.path.join(BASE, "report.json")
        proc, _ = scan(root, report,
                       ["--no-cache", "--profile", profile, "--auto", "shortest", "-y"])
        if proc.returncode != 0:
            return "FAIL: --auto under {} exited {}: {}".format(
                profile, proc.returncode, proc.stderr.strip()[:200])
        outcomes[profile] = survivors(root)
    if outcomes["flash"] != outcomes["spinning"]:
        return "FAIL: flash kept {}, spinning kept {}".format(
            outcomes["flash"], outcomes["spinning"])
    return "ok (both kept {})".format(", ".join(outcomes["flash"]))


def case_stage_size_guard() -> str:
    """A size bucket is not a group, whatever the profile says."""
    root = os.path.join(BASE, "stage-size")
    os.makedirs(root)
    build(root)
    before = survivors(root)
    report = os.path.join(BASE, "report.json")
    proc, _ = scan(root, report,
                   ["--no-cache", "--profile", "spinning", "--stage", "size",
                    "--auto", "shortest", "-y"])
    if proc.returncode != 2:
        return "FAIL: expected exit 2, got {}".format(proc.returncode)
    if survivors(root) != before:
        return "FAIL: files were removed from a scan that read nothing"
    return "ok (exit 2, nothing removed): " + proc.stderr.strip().split(".")[0]


def case_bad_value() -> str:
    root = os.path.join(BASE, "bad-value")
    os.makedirs(root)
    build(root)
    proc, _ = scan(root, os.path.join(BASE, "report.json"),
                   ["--no-cache", "--profile", "rotational"])
    if proc.returncode != 2:
        return "FAIL: expected exit 2, got {}".format(proc.returncode)
    return "ok (exit 2): " + proc.stderr.strip()


def main() -> int:
    if not os.path.exists(CANDIDATE):
        raise SystemExit("build it first: cargo build --release")
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    cases = [
        ("cache: flash first, then spinning", lambda: case_cache_crossover("flash", "spinning")),
        ("cache: spinning first, then flash", lambda: case_cache_crossover("spinning", "flash")),
        ("cache: spinning twice", lambda: case_cache_crossover("spinning", "spinning")),
        ("--auto under either profile", case_deletion_parity),
        ("--stage size --auto --profile spinning", case_stage_size_guard),
        ("--profile with a name it does not know", case_bad_value),
    ]
    failed = 0
    for name, run in cases:
        answer = run()
        failed += answer.startswith("FAIL")
        print("{:<42} {}".format(name, answer))
    shutil.rmtree(BASE, ignore_errors=True)
    print()
    if failed:
        print("{} combination(s) wrong".format(failed))
        return 1
    print("every combination behaves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
