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

import fcntl
import json
import os
import pty
import random
import shutil
import struct
import subprocess
import sys
import termios
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


def pty_drive(root: str, home: str, keys, extra):
    """Run the browser under a real terminal and send it `keys`.

    The same shape as `m5_end_to_end.py`'s driver, which is where the details
    (the window size a pty starts without, and answering the cursor-position
    query nothing else on this pty will answer) were worked out.
    """
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    environment = dict(os.environ, HOME=home, TERM="xterm-256color")
    environment["DEDUP_CACHE_DIR"] = os.path.join(home, "cache")
    process = subprocess.Popen([CANDIDATE, root] + extra, stdin=slave,
                               stdout=slave, stderr=slave, env=environment,
                               start_new_session=True)
    os.close(slave)
    os.set_blocking(master, False)

    def drain():
        try:
            chunk = os.read(master, 262144)
        except (BlockingIOError, OSError):
            return
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")

    # The menu comes first now: one Enter accepts it, then the scan runs.
    for _ in range(8):
        time.sleep(0.1)
        drain()
    for key in [b"\r"] + list(keys):
        if process.poll() is not None:
            break
        try:
            os.write(master, key)
        except OSError:
            break
        for _ in range(5):
            time.sleep(0.1)
            drain()
    try:
        code = process.wait(timeout=30)
    except subprocess.TimeoutExpired:
        process.kill()
        code = process.wait()
    drain()
    os.close(master)
    return code, ""


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


def case_browser_deletion_parity(binary_pty) -> str:
    """The browser deletes the same files under either profile.

    `--profile` reaches the browser too — `main::browse` hands the scan's
    profile to the `Compare` the browser deletes through, and a wide attested
    group re-proved at deletion time takes the capped comparison. So the same
    marks under the two profiles have to remove the same files.
    """
    outcomes = {}
    for profile in ("flash", "spinning"):
        root = os.path.join(BASE, "browse-" + profile)
        home = os.path.join(BASE, "browse-home-" + profile)
        os.makedirs(root)
        os.makedirs(home)
        build(root)
        put(root, "nested/a-copy.bin", 1, 0xAA)
        before = survivors(root)
        # `a` marks every copy but the highlighted one in this group, ENTER
        # asks for the confirmation, `y` accepts it, `q` leaves.
        code, _ = binary_pty(root, home,
                             [b"a", b"\r", b"y", b"q"],
                             ["-i", "--min-size", "1", "--no-cache",
                              "--profile", profile,
                              "--report-json", os.path.join(home, "report.json")])
        if code != 0:
            return "FAIL: the browser under {} exited {}".format(profile, code)
        after = survivors(root)
        # Two runs that both deleted nothing agree about everything, which is
        # not the thing being tested.
        if after == before:
            return ("FAIL: the browser under {} removed nothing, so this "
                    "proves nothing".format(profile))
        outcomes[profile] = (before, after)
    if outcomes["flash"] != outcomes["spinning"]:
        return "FAIL: flash kept {}, spinning kept {}".format(
            outcomes["flash"][1], outcomes["spinning"][1])
    gone = sorted(set(outcomes["flash"][0]) - set(outcomes["flash"][1]))
    return "ok (both removed {} and kept {})".format(
        ", ".join(gone), ", ".join(outcomes["flash"][1]))


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
        ("the browser under either profile",
         lambda: case_browser_deletion_parity(pty_drive)),
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
