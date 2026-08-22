#!/usr/bin/env python3
"""The dedup benchmark harness.

Correctness is a gate, not a score. Every scan is checked against a manifest
that was written when the corpus was laid down, and a single mismatch fails
the whole run with no timings printed at all — there is no way to trade a
wrong answer for a faster one.

What is measured, and why it can be trusted:

  bytes    counted by wrapping open() in the measuring process, not by asking
           the tool. The tool's own hashed_bytes is read too, and a gap of
           more than 20% between the two is itself a gate failure.
  seconds  the scan only, in a fresh process, `--rounds` times per side with
           the two sides interleaved, reporting the minimum. This Mac's load
           drifts; the minimum of an interleaved series is the part of the
           measurement that drift cannot inflate.
  rss      peak resident size of the measuring process.

Corpora live on a RAM disk, so no run competes with another for a real
device and the page cache never has to be dropped. The manifest sits beside
the scanned tree, never inside it, and reading it is a gate failure.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import resource
import shutil
import subprocess
import sys
import time
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
STATE = os.path.join(HERE, "state")
MEASURE = os.path.join(HERE, "_measure.py")
LOCK = os.path.join(HERE, "baseline.lock")
DEFAULT_ROOT = "/Volumes/dedupbench"

sys.path.insert(0, HERE)
import corpus                                        # noqa: E402

# A gap this wide between the bytes we counted and the bytes the tool claims
# means one of the two is not describing the scan that happened.
BYTE_TOLERANCE = 0.20

# Loading dedup and asking diskutil about the volume takes a fifth of a second.
# Anything near this ceiling is a scan that has been moved out of the timer.
SETUP_CEILING = 2.0

SCAN_TIMEOUT = 1800
ADVERSARIAL_TIMEOUT = 60
DELETE_TIMEOUT = 900

ORDER = ["typical", "photos", "bigheader", "needle", "samesize", "widegroup",
         "prefixtrap", "adversarial", "deletion", "rescan"]

# Every scenario but one is measured with the re-scan cache switched off, so
# every score stays comparable with every score taken before the cache existed
# — including the pinned baseline, which has no cache at all — and the byte
# floor keeps its full force. `rescan` is the one that turns it on, and it is
# not timed and not scored.
CACHE_FREE = [n for n in ORDER if n != "rescan"]

# A warm scan of a tree nobody has touched should read essentially nothing.
# Five percent is a ceiling with room in it, not a target: the real figure on
# an unchanged corpus is zero.
WARM_CEILING = 0.05

# The scenarios the score is the geometric mean of. This list is FIXED. A
# scenario added after it was written — `prefixtrap` is the first — does not
# join it, whatever it measures, because a score is only worth anything if
# today's number can be compared with one recorded months ago. New scenarios
# earn their keep as gates and as vetoes (see score()), not as terms.
SCORE_SCENARIOS = ("typical", "photos", "bigheader", "needle", "samesize",
                   "widegroup")

# Bytes are what the machines this tool exists for actually pay in, so they
# carry most of the weight; time keeps a change that trades a whole second
# for a byte from looking like progress.
SCORE_BYTES_WEIGHT = 0.7
SCORE_TIME_WEIGHT = 0.3

# photos, bigheader and widegroup finish in tens of milliseconds. Three rounds
# of min-of-N is too thin to put in a score that gets quoted.
SCORE_MIN_ROUNDS = 9

# One harness run at a time per volume.
LOCK_NAME = ".bench.lock"
LOCK_WAIT = 1800.0
LOCK_POLL = 2.0
LOCK_GRACE = 5.0        # a lockfile younger than this that will not parse is
                        # assumed to be one that is mid-write, not junk


class Fatal(Exception):
    """Something about the machine or the harness is wrong — exit 2."""


def human(n) -> str:
    if n is None:
        return "-"
    n = float(n)
    if n < 1024:
        return "{:.0f} B".format(n)
    for unit in ("KB", "MB", "GB"):
        n /= 1024.0
        if n < 1024:
            return "{:.1f} {}".format(n, unit)
    return "{:.1f} TB".format(n / 1024.0)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def git(*args) -> str:
    out = run(["git", "-C", REPO] + list(args))
    if out.returncode != 0:
        raise Fatal("git {} failed: {}".format(" ".join(args), out.stderr.strip()))
    return out.stdout


# ─────────────────────────────────────────────────────────────────────────────
# the volume
# ─────────────────────────────────────────────────────────────────────────────

def check_root(root: str) -> str:
    root = os.path.abspath(root)
    if "my new passport" in root.lower():
        raise Fatal("refusing to touch {}: that volume is off limits".format(root))
    return root


def check_descriptors() -> None:
    """dedup opens every member of a group at once, and `samesize` is a group
    of over a thousand. Short of descriptors those opens fail, the files land
    in stats.unreadable, and the gate fails for a reason that has nothing to
    do with the tool. Better to say so up front."""
    soft = resource.getrlimit(resource.RLIMIT_NOFILE)[0]
    if soft < 2048:
        raise Fatal("open file limit is {}; the wide-group scenarios need at "
                    "least 2048 (try `ulimit -n 4096`)".format(soft))


def ram_disk_mb() -> tuple:
    """(megabytes, halve_the_corpus). A 16 GB machine can spare 2816 MB; a
    smaller one gets half of that and half the corpus to go in it."""
    out = run(["sysctl", "-n", "hw.memsize"])
    try:
        memsize = int(out.stdout.strip())
    except ValueError:
        memsize = 0
    return (2816, False) if memsize >= 16 * 1000 ** 3 else (1408, True)


def ensure_volume(root: str, megabytes: int) -> str:
    """Attach the RAM disk if it is not already there. Reuse it if it is."""
    if os.path.ismount(root):
        return "reused"
    if root != DEFAULT_ROOT:
        # An explicit --root is taken at its word. A plain directory is a
        # perfectly good place to check that the gates fire; it is just not a
        # place to take a timing from, and the note says so on every run.
        os.makedirs(root, exist_ok=True)
        if not os.access(root, os.W_OK):
            raise Fatal("{} is not writable".format(root))
        if os.stat(root).st_dev == os.stat("/").st_dev:
            return "boot disk — gates are valid here, timings are not"
        return "existing"

    attach = run(["hdiutil", "attach", "-nomount", "ram://{}"
                  .format(megabytes * 1024 * 1024 // 512)])
    if attach.returncode != 0:
        raise Fatal("hdiutil attach failed: {}".format(attach.stderr.strip()))
    device = attach.stdout.split()[0]
    erase = run(["diskutil", "erasevolume", "HFS+", "dedupbench", device])
    if erase.returncode != 0:
        run(["hdiutil", "detach", device])
        raise Fatal("diskutil erasevolume failed: {}".format(erase.stderr.strip()))
    if not os.path.ismount(root):
        raise Fatal("{} did not appear after erasevolume".format(root))
    return "created {} MB at {}".format(megabytes, device)


def ensure_space(volume: str, need: int, keep: set) -> None:
    """Corpora are regenerable, so an old one that this run does not want is
    the first thing to go when the volume is tight."""
    if shutil.disk_usage(volume).free >= need:
        return
    corpora = os.path.join(volume, "corpora")
    for entry in sorted(os.listdir(corpora)) if os.path.isdir(corpora) else []:
        path = os.path.join(corpora, entry)
        if path in keep:
            continue
        shutil.rmtree(path, ignore_errors=True)
        if shutil.disk_usage(volume).free >= need:
            return
    free = shutil.disk_usage(volume).free
    raise Fatal("{} needs {} free, has {}".format(volume, human(need), human(free)))


# ─────────────────────────────────────────────────────────────────────────────
# one run at a time
# ─────────────────────────────────────────────────────────────────────────────

class VolumeLock:
    """Serialize harness runs that share a volume.

    Two runs on one RAM disk do not merely go slowly, they corrupt each
    other's results: `ensure_space` evicts a corpus the other run is halfway
    through scanning, and the deletion gate empties `.Trashes` underneath it.
    Both show up afterwards as a mystery gate failure rather than as a
    collision, which is the worst kind of bug to be handed.

    Advisory, and deliberately so — it is a file with a pid in it, not a
    kernel lock. Anything that bypasses it is on its own. But every path in
    this file that writes to the volume is behind it.
    """

    def __init__(self, volume: str, wait: float = LOCK_WAIT):
        self.path = os.path.join(volume, LOCK_NAME)
        self.wait = wait
        self.pid = os.getpid()
        self.held = False

    def _payload(self) -> dict:
        return {"pid": self.pid, "host": platform.node(),
                "started": datetime.now().astimezone().isoformat(timespec="seconds")}

    def _read(self):
        try:
            with open(self.path) as fh:
                return json.load(fh)
        except (OSError, ValueError):
            return None

    def _holder(self):
        """(description, alive) for whoever holds it, or None if it is gone."""
        try:
            written = os.stat(self.path).st_mtime
        except OSError:
            return None
        data = self._read()
        if data is None:
            # Either a lock caught between its create and its write — a window
            # of microseconds — or a file that is never going to parse. Age is
            # what tells them apart, and a lock nobody can read must not be
            # allowed to wedge the volume forever.
            if time.time() - written < LOCK_GRACE:
                return "a lock still being written", True
            return "an unreadable lock file", False
        text = "pid {} on {}, since {}".format(
            data.get("pid"), data.get("host"), data.get("started", "?"))
        pid = data.get("pid")
        if data.get("host") != platform.node() or not isinstance(pid, int):
            return text, True          # not ours to check: assume it is alive
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return text, False
        except PermissionError:
            pass                       # alive, just not ours to signal
        return text, True

    def _claim_stale(self) -> bool:
        """Take over a lock whose owner is gone, and say whether we won.

        Two runs can decide the same lock is stale at the same moment. Both
        write, `os.replace` picks a winner atomically, and both then read
        back: only one finds its own pid, and the loser goes back to waiting.
        Unlink-then-create would leave both of them thinking they had it.
        """
        temp = "{}.{}".format(self.path, self.pid)
        with open(temp, "w") as fh:
            json.dump(self._payload(), fh)
        os.replace(temp, self.path)
        time.sleep(0.2)                # let any rival's replace land too
        data = self._read()
        return bool(data) and data.get("pid") == self.pid

    def acquire(self) -> "VolumeLock":
        deadline = time.monotonic() + self.wait
        announced = last = None
        while True:
            if last is not None and time.monotonic() >= deadline:
                raise Fatal("gave up waiting for {}: still held by {}"
                            .format(self.path, last))
            try:
                fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            except FileExistsError:
                pass
            else:
                with os.fdopen(fd, "w") as fh:
                    json.dump(self._payload(), fh)
                self.held = True
                return self

            holder = self._holder()
            if holder is None:
                last = last or "a lock that vanished as we read it"
                time.sleep(0.05)               # released between the two calls
                continue
            text, alive = holder
            last = text
            if not alive:
                print("bench: clearing a stale lock ({})".format(text))
                if self._claim_stale():
                    self.held = True
                    return self
                continue                       # a rival stole it first
            if not announced:
                print("bench: {} is locked by {} — waiting up to {:.0f}s"
                      .format(self.path, text, self.wait))
                announced = True
            time.sleep(LOCK_POLL)

    def release(self) -> None:
        if not self.held:
            return
        self.held = False
        data = self._read()
        if data and data.get("pid") != self.pid:
            return                             # someone took it over; leave it
        try:
            os.unlink(self.path)
        except OSError:
            pass


# ─────────────────────────────────────────────────────────────────────────────
# the two sides
# ─────────────────────────────────────────────────────────────────────────────

def baseline_sha() -> str:
    with open(LOCK) as fh:
        for line in fh:
            line = line.strip()
            if line and not line.startswith("#"):
                return line
    raise Fatal("no sha in {}".format(LOCK))


def materialize_baseline(sha: str) -> str:
    path = os.path.join(STATE, "baseline-{}.py".format(sha))
    if not os.path.exists(path):
        os.makedirs(STATE, exist_ok=True)
        source = git("show", "{}:dedup.py".format(sha))
        with open(path, "w") as fh:
            fh.write(source)
    return path


def current_id(dedup_path: str) -> str:
    if os.path.abspath(dedup_path) != os.path.join(REPO, "dedup.py"):
        return "external:{}".format(os.path.abspath(dedup_path))
    if git("status", "--porcelain", "--", "dedup.py").strip():
        return "dirty"
    return git("rev-parse", "HEAD").strip()


def pytest_gate(dedup_path: str) -> str:
    """Run the suite against the dedup.py actually under test.

    The tests import `dedup`, which would otherwise resolve to whatever sits
    in the repo regardless of --dedup. Copying both into one directory closes
    that gap: passing tests always describe the file being measured.
    """
    work = os.path.join(STATE, "pytest")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    shutil.copy(dedup_path, os.path.join(work, "dedup.py"))
    shutil.copy(os.path.join(REPO, "conftest.py"), work)
    shutil.copytree(os.path.join(REPO, "tests"), os.path.join(work, "tests"))
    out = run([sys.executable, "-m", "pytest", "tests/", "-q"], cwd=work)
    if out.returncode == 0:
        return "pass"
    tail = (out.stdout or out.stderr).strip().splitlines()[-6:]
    return "fail: " + " / ".join(t.strip() for t in tail)


# ─────────────────────────────────────────────────────────────────────────────
# measuring and checking
# ─────────────────────────────────────────────────────────────────────────────

def scan_env(cache_dir=None) -> dict:
    """The environment a scan runs in.

    DEDUP_CACHE_DIR always points somewhere the harness owns, so no
    measurement can reach the user's real cache — belt to the braces of the
    scored scenarios passing no cache directory at all.
    """
    env = dict(os.environ)
    env["DEDUP_CACHE_DIR"] = cache_dir or os.path.join(STATE, "no-cache")
    return env


def measure(dedup_path: str, tree: str, volume: str, min_size: int,
            sabotage=None, timeout: int = SCAN_TIMEOUT, cache_dir=None) -> dict:
    cmd = [sys.executable, MEASURE, "--dedup", dedup_path, "--root", tree,
           "--volume", volume, "--min-size", str(min_size)]
    if sabotage:
        cmd += ["--sabotage", sabotage]
    if cache_dir:
        cmd += ["--cache-dir", cache_dir]
    try:
        out = run(cmd, timeout=timeout, env=scan_env(cache_dir))
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timed out after {}s".format(timeout)}
    try:
        return json.loads(out.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"ok": False,
                "error": "driver produced no result: {}".format(
                    (out.stderr or out.stdout).strip()[-400:])}


def check(manifest: dict, result: dict, byte_floor: bool = True):
    """None when the scan is correct, otherwise why it is not.

    `byte_floor` is off for exactly one thing: the warm half of `rescan`,
    where not reading is the whole point and the floor is replaced by a
    stricter pair of demands — the same answer as the cold scan, for a
    fraction of the bytes. Nothing else may turn it off.
    """
    if not result.get("ok"):
        return "scan failed: {}".format(result.get("error", "?"))
    if result.get("tripwire"):
        return "read outside the scanned tree: {}".format(result["tripwire"][0])

    want = corpus.canonical(manifest["groups"])
    got = corpus.canonical(result["groups"])
    if want != got:
        return "groups differ: {}".format(corpus.describe(want - got, got - want))

    reported = result.get("unreadable", [])
    for rel in manifest["expect_unreadable"]:
        if not any(rel in entry for entry in reported):
            return "expected an unreadable file to be reported: {}".format(rel)
    if len(reported) != len(manifest["expect_unreadable"]):
        return "unexpected unreadable entries: {}".format(reported[:3])

    counted, claimed = result["bytes_instrumented"], result["bytes_reported"]
    widest = max(counted, claimed)
    if widest and abs(counted - claimed) / widest > BYTE_TOLERANCE:
        return ("byte count mismatch: {} read through open(), {} reported"
                .format(human(counted), human(claimed)))

    # dedup's own promise is that nothing is called a duplicate on the strength
    # of a hash, so every member of every group has to have been read from end
    # to end. That is a floor on the byte count that no amount of cleverness
    # can go under, and it is what stops a tool from scoring well by reading
    # nothing at all — through mmap, through os.read, or out of a cache it
    # kept from a previous run.
    if byte_floor:
        floor = sum(g["size"] * len(g["members"]) for g in manifest["groups"])
        if counted < floor:
            return ("read {} — proving these groups takes at least {}, so they "
                    "were not proved".format(human(counted), human(floor)))

    if result.get("setup_seconds", 0) > SETUP_CEILING:
        return ("spent {:.1f}s before the clock started; scan work does not "
                "belong outside the timed call".format(result["setup_seconds"]))
    return None


def byte_gap(result: dict) -> float:
    counted, claimed = result["bytes_instrumented"], result["bytes_reported"]
    widest = max(counted, claimed)
    return abs(counted - claimed) / widest if widest else 0.0


# ─────────────────────────────────────────────────────────────────────────────
# gate-only scenarios
# ─────────────────────────────────────────────────────────────────────────────

def trash_for(tree: str, home: str) -> str:
    """Where dedup will put what it removes from `tree`.

    The same rule trash_dir_for uses, worked out from outside the tool so the
    gate is checking dedup's behaviour rather than agreeing with itself. On a
    corpus volume that is the volume's own .Trashes; on a plain directory on
    the boot disk it is ~/.Trash, which is why the run gets its own HOME.
    """
    path = os.path.realpath(tree)
    try:
        dev = os.stat(path).st_dev
    except OSError:
        return os.path.join(home, ".Trash")
    while path != os.path.dirname(path):
        parent = os.path.dirname(path)
        try:
            if os.stat(parent).st_dev != dev:
                return os.path.join(path, ".Trashes", str(os.getuid()))
        except OSError:
            break
        path = parent
    return os.path.join(home, ".Trash")


def deletion_gate(dedup_path: str, volume: str, base: str, manifest: dict) -> str:
    """Drive the real command line and check what it left behind.

    The scan is only half of what this tool does. This is the other half: one
    survivor per group, its bytes untouched, every victim in the Trash, and
    nothing left for a second pass to find.
    """
    tree = os.path.join(base, "tree")
    # A run rooted on the boot disk would otherwise trash into the user's own
    # home folder. It gets a home of its own instead.
    home = os.path.join(STATE, "home")
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(home)
    trash = trash_for(tree, home)
    shutil.rmtree(trash, ignore_errors=True)
    env = scan_env()
    env["HOME"] = home

    try:
        out = run([sys.executable, dedup_path, tree, "--auto", "shortest", "-y",
                   "--no-cache", "--min-size", str(manifest["min_size"])],
                  timeout=DELETE_TIMEOUT, env=env)
    except subprocess.TimeoutExpired:
        return "fail: dedup --auto timed out"
    if out.returncode != 0:
        return "fail: dedup --auto exited {} ({})".format(
            out.returncode, (out.stderr or out.stdout).strip()[-200:])

    victims = []
    for group in manifest["groups"]:
        names = [m["names"][0] for m in group["members"]]
        keeper = min(names, key=lambda n: (len(os.path.join(tree, n)),
                                           os.path.join(tree, n)))
        path = os.path.join(tree, keeper)
        if not os.path.isfile(path):
            return "fail: survivor missing: {}".format(keeper)
        with open(path, "rb") as fh:
            if hashlib.sha256(fh.read()).hexdigest() != group["sha"]:
                return "fail: survivor was modified: {}".format(keeper)
        for name in names:
            if name == keeper:
                continue
            if os.path.exists(os.path.join(tree, name)):
                return "fail: duplicate still present: {}".format(name)
            victims.append(os.path.basename(name))

    if not os.path.isdir(trash):
        return "fail: nothing arrived in {}".format(trash)
    trashed = set(os.listdir(trash))
    missing = [v for v in victims if v not in trashed]
    if missing:
        return "fail: {} victim(s) never reached the Trash, e.g. {}".format(
            len(missing), missing[0])

    # Everything that was not a duplicate has to still be there. Counting is
    # enough: one over-deletion anywhere in the tree moves this number.
    left = sum(len(files) for _, _, files in os.walk(tree))
    if left != manifest["file_count"] - len(victims):
        return "fail: {} files left, expected {}".format(
            left, manifest["file_count"] - len(victims))

    again = run([sys.executable, dedup_path, tree, "--list", "--no-cache",
                 "--min-size", str(manifest["min_size"])],
                timeout=DELETE_TIMEOUT, env=env)
    if again.returncode != 0 or "No duplicates found" not in again.stdout:
        return "fail: a second pass still found duplicates"

    shutil.rmtree(trash, ignore_errors=True)
    shutil.rmtree(home, ignore_errors=True)
    shutil.rmtree(base, ignore_errors=True)
    return "pass"


# ─────────────────────────────────────────────────────────────────────────────
# the re-scan gate
# ─────────────────────────────────────────────────────────────────────────────
#
# Three things have to be true at once for the cache to be worth having, and
# this scenario refuses to let any one of them be traded for another:
#
#   cold   with nothing remembered, everything is read — the byte floor, in
#          full force, exactly as it applies to every other scenario
#   warm   with everything remembered, the answer is identical and the bytes
#          are a rounding error
#   wrong  when what was remembered is a lie, the deletion path notices before
#          it removes anything
#
# The third is the one that matters. The first two are a performance feature;
# the third is the reason the performance feature is allowed to exist.


def mutable_groups(manifest: dict) -> list:
    """Manifest groups whose members are ordinary single-named files.

    Hardlinked members share an inode, so writing to one name writes to all of
    them, which is not the experiment. Widest groups first, and ties broken by
    name, so the same corpus always gets the same treatment.
    """
    out = [g for g in manifest["groups"]
           if len(g["members"]) >= 2 and all(len(m["names"]) == 1
                                             for m in g["members"])]
    return sorted(out, key=lambda g: (-len(g["members"]),
                                      g["members"][0]["names"][0]))


def without_member(groups: list, target: dict, name: str) -> list:
    """The manifest's groups as they should read once `name` stops matching."""
    out = []
    for g in groups:
        if g is not target:
            out.append(g)
            continue
        left = [m for m in g["members"] if name not in m["names"]]
        if len(left) > 1:
            out.append(dict(g, members=left))
    return out


def rewrite(path: str, filler: bytes, keep_mtime: bool) -> None:
    """Replace a file's contents with something else of exactly the same length.

    Same length on purpose: a file that changed size would be dropped by the
    size bucketing before any cached digest could be consulted, which would
    prove nothing about the cache at all.
    """
    st = os.stat(path)
    body = (filler * (st.st_size // len(filler) + 1))[:st.st_size]
    with open(path, "r+b") as fh:
        fh.write(body)
        fh.flush()
        os.fsync(fh.fileno())
    if keep_mtime:
        os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns))
    after = os.stat(path)
    if after.st_size != st.st_size:
        raise Fatal("rewrite changed the size of {}".format(path))
    if keep_mtime and after.st_mtime_ns != st.st_mtime_ns:
        raise Fatal("could not restore mtime_ns on {}".format(path))


def rescan_gate(dedup_path: str, volume: str, base: str, manifest: dict) -> tuple:
    """-> (verdict, cold_bytes, warm_bytes)."""
    tree = os.path.join(base, "tree")
    cache = os.path.join(STATE, "rescan-cache")
    shutil.rmtree(cache, ignore_errors=True)
    min_size = manifest["min_size"]

    def fail(what, why):
        return ("fail: {} — {}".format(what, why), None, None)

    groups = mutable_groups(manifest)
    if len(groups) < 2:
        return fail("setup", "corpus has fewer than two mutable groups")

    # ── phase 1: cold ────────────────────────────────────────────────────
    cold = measure(dedup_path, tree, volume, min_size, cache_dir=cache)
    reason = check(manifest, cold)
    if reason:
        return fail("cold scan", reason)
    if any(g.get("attested") for g in cold["groups"]):
        return fail("cold scan", "a group was attested with an empty cache")
    cold_bytes = cold["bytes_instrumented"]
    if not cold_bytes:
        return fail("cold scan", "read nothing at all")

    # ── phase 2: warm ────────────────────────────────────────────────────
    warm = measure(dedup_path, tree, volume, min_size, cache_dir=cache)
    reason = check(manifest, warm, byte_floor=False)
    if reason:
        return fail("warm scan", reason)
    warm_bytes = warm["bytes_instrumented"]
    ratio = warm_bytes / cold_bytes
    if ratio >= WARM_CEILING:
        return ("fail: warm scan — read {} of the cold scan's {} ({:.1%}), "
                "which is not a re-scan cache".format(
                    human(warm_bytes), human(cold_bytes), ratio), None, None)
    if not any(g.get("attested") for g in warm["groups"]):
        return fail("warm scan", "nothing was attested, so nothing was cached")

    # ── phase 3a: an ordinary edit ───────────────────────────────────────
    edited = groups[0]
    edited_name = edited["members"][0]["names"][0]
    rewrite(os.path.join(tree, edited_name), b"\x5a\xa5", keep_mtime=False)
    expect = without_member(manifest["groups"], edited, edited_name)

    after = measure(dedup_path, tree, volume, min_size, cache_dir=cache)
    reason = check(dict(manifest, groups=expect), after, byte_floor=False)
    if reason:
        return fail("after an edit", reason)
    if not after["bytes_instrumented"]:
        return fail("after an edit", "the edited file was never re-read")

    # ── phase 3b: a forged mtime, and then the deletion path ─────────────
    #
    # The cache cannot see this one and is not expected to: the key it
    # remembers still matches. The warm scan will group these files wrongly,
    # by design. Everything now rests on the deletion path reading the bytes.
    poisoned = groups[1]
    poison_names = [m["names"][0] for m in poisoned["members"]]
    victim = poison_names[0]
    rewrite(os.path.join(tree, victim), b"\xde\xad\xbe\xef", keep_mtime=True)

    lied = measure(dedup_path, tree, volume, min_size, cache_dir=cache)
    if not lied.get("ok"):
        return fail("poisoned scan", lied.get("error", "?"))
    still = [g for g in lied["groups"]
             if g.get("attested") and any(victim in m["names"] for m in g["members"])]
    if not still:
        return fail("poisoned scan",
                    "the poisoned group was not reported, so the refusal below "
                    "would prove nothing")

    before = {n: os.path.getsize(os.path.join(tree, n)) for n in poison_names}
    env = scan_env(cache)
    env["HOME"] = os.path.join(STATE, "home")
    try:
        out = run([sys.executable, dedup_path, tree, "--auto", "shortest", "-y",
                   "--hard", "--min-size", str(min_size)],
                  timeout=DELETE_TIMEOUT, env=env)
    except subprocess.TimeoutExpired:
        return fail("deletion", "dedup --auto timed out")
    said = out.stdout + out.stderr
    if out.returncode != 0:
        return fail("deletion", "dedup --auto exited {} ({})".format(
            out.returncode, said.strip()[-200:]))

    for name in poison_names:
        path = os.path.join(tree, name)
        if not os.path.isfile(path):
            return fail("deletion",
                        "deleted {} out of a group whose copies do not "
                        "match".format(name))
        if os.path.getsize(path) != before[name]:
            return fail("deletion", "{} was modified".format(name))
    if "Refusing" not in said:
        return fail("deletion",
                    "kept the poisoned files but never said why; a refusal "
                    "nobody is told about is a bug waiting to be reported")

    return ("pass", cold_bytes, warm_bytes)


# ─────────────────────────────────────────────────────────────────────────────
# the table
# ─────────────────────────────────────────────────────────────────────────────

def ratio(current, base) -> str:
    if current is None or not base:
        return "-"
    return "{:.2f}x".format(current / base)


def render(results: dict) -> str:
    header = ("scenario", "gate", "bytes cur", "bytes base", "cur/base",
              "sec cur", "sec base", "cur/base", "rss cur", "rss base")
    rows = [header]
    for name in ORDER:
        entry = results.get(name)
        if entry is None:
            continue
        b, s, m = entry["bytes"], entry["seconds"], entry["maxrss_mb"]
        rows.append((
            # rescan has no baseline to compare against — the baseline dedup.py
            # has no cache at all. Its two byte columns are its own cold and
            # warm scans, which makes the ratio column the number this whole
            # scenario exists to report.
            "rescan warm/cold" if name == "rescan" else name,
            entry["gate"] if entry["gate"] in ("pass", "skipped") else "FAIL",
            human(b["current"]), human(b["baseline"]), ratio(b["current"], b["baseline"]),
            "-" if s["current"] is None else "{:.3f}".format(s["current"]),
            "-" if s["baseline"] is None else "{:.3f}".format(s["baseline"]),
            ratio(s["current"], s["baseline"]),
            "-" if m["current"] is None else "{:.0f}".format(m["current"]),
            "-" if m["baseline"] is None else "{:.0f}".format(m["baseline"]),
        ))
    widths = [max(len(r[i]) for r in rows) for i in range(len(header))]
    lines = []
    for i, row in enumerate(rows):
        lines.append("  ".join(cell.ljust(widths[j]) for j, cell in enumerate(row)).rstrip())
        if i == 0:
            lines.append("─" * len(lines[0]))
    return "\n".join(lines)


# ─────────────────────────────────────────────────────────────────────────────
# the score
# ─────────────────────────────────────────────────────────────────────────────

def score(gates: dict, scenarios: dict):
    """(value, void_reason) — one of the two is always None.

    The geometric mean over SCORE_SCENARIOS of

        (bytes_cur / bytes_base) ** 0.7  ×  (sec_cur / sec_base) ** 0.3

    so lower is better and 1.0000 is the pinned baseline exactly. A geometric
    mean because these are ratios: halving one scenario and doubling another
    has to come out where it started, and an arithmetic mean does not.

    The score can also be void, which is not the same as being bad. A void
    score has no number at all, because the run it would have summarised is
    not a run whose numbers mean anything.
    """
    for name, verdict in sorted(gates.items()):
        if verdict.startswith("fail"):
            return None, "the {} gate failed".format(name)

    # Rule 2 of GOAL.md §4, mechanized. Bytes are deterministic, so a
    # regression is never noise, and no time win buys one off. Every scenario
    # that reports bytes is checked — not just the six that are scored —
    # which is how a scenario added later can veto a change without ever
    # touching the number.
    for name in ORDER:
        entry = scenarios.get(name)
        if entry is None:
            continue
        cur, base = entry["bytes"]["current"], entry["bytes"]["baseline"]
        if cur is None or base is None:
            continue
        if cur > base:
            return None, "bytes regressed on {} ({} vs {})".format(
                name, human(cur), human(base))

    product = 1.0
    for name in SCORE_SCENARIOS:
        entry = scenarios.get(name)
        if entry is None:
            return None, "{} did not run, and the score is all six or none".format(name)
        b, s = entry["bytes"], entry["seconds"]
        if not b["baseline"] or not s["baseline"] or b["current"] is None \
                or s["current"] is None:
            return None, "{} has no baseline to be scored against".format(name)
        product *= ((b["current"] / b["baseline"]) ** SCORE_BYTES_WEIGHT *
                    (s["current"] / s["baseline"]) ** SCORE_TIME_WEIGHT)
    return product ** (1.0 / len(SCORE_SCENARIOS)), None


# ─────────────────────────────────────────────────────────────────────────────
# selftest
# ─────────────────────────────────────────────────────────────────────────────

SOURCE_SABOTAGE = {
    # Take the cache's word for it and delete without comparing. This is the
    # single mistake that would turn the re-scan cache from a speed feature
    # into a way of losing files, so the rescan gate has to catch it.
    "skip-attested-verify":
        "def verify_attested(groups, solid=None, cache=None):\n"
        "    return list(groups), []\n",
}


def sabotaged_dedup(dedup_path: str, mode: str) -> str:
    """Write a copy of dedup.py with one thing deliberately broken.

    The deletion path runs as a real command line, so a sabotage that only
    lives in the measuring process cannot reach it. This one goes into the
    source, just above the entry point, where it overrides the real thing.
    """
    with open(dedup_path) as fh:
        source = fh.read()
    anchor = '\nif __name__ == "__main__":'
    if anchor not in source:
        raise Fatal("cannot sabotage {}: no entry point to patch above"
                    .format(dedup_path))
    os.makedirs(STATE, exist_ok=True)
    out = os.path.join(STATE, "sabotaged-{}.py".format(mode))
    with open(out, "w") as fh:
        fh.write(source.replace(anchor, "\n\n" + SOURCE_SABOTAGE[mode] + anchor, 1))
    return out


def ensure_corpus(volume: str, name: str, scale: str, seed: int, factor: float,
                  rebuild: bool = False) -> tuple:
    """(base directory, manifest), building the corpus if it is not usable."""
    base = corpus.corpus_dir(volume, name, scale, seed)
    manifest = None if rebuild else corpus.load(volume, name, scale, seed)
    if manifest is None or manifest.get("factor") != factor:
        shutil.rmtree(base, ignore_errors=True)
        ensure_space(volume, int(corpus.estimated_bytes(name, factor) * 1.4), {base})
        manifest = corpus.build(volume, name, seed, scale, factor)
    return base, manifest


def selftest(dedup_path: str, volume: str, seed: int) -> int:
    """Break the tool on purpose and insist the gates notice.

    A gate that has never fired is a gate nobody has tested. `samesize` is the
    scenario used because it contains twins that differ only in their final
    byte, which is precisely what a truncated comparison cannot see.
    """
    name, scale = "samesize", "smoke"
    factor = corpus.SCALES[scale]
    base, manifest = ensure_corpus(volume, name, scale, seed, factor)
    tree = os.path.join(base, "tree")

    print("selftest: scenario {} at {} scale\n".format(name, scale))
    passed = True

    control = measure(dedup_path, tree, volume, manifest["min_size"])
    reason = check(manifest, control)
    ok = reason is None
    print("  control, no sabotage        {}".format(
        "clean" if ok else "UNEXPECTED FAILURE: {}".format(reason)))
    print("      byte gap {:.1%}, {} group(s) found".format(
        byte_gap(control) if control.get("ok") else 0.0,
        len(control.get("groups", []))))
    passed &= ok

    bad = measure(dedup_path, tree, volume, manifest["min_size"],
                  sabotage="truncate-compare")
    reason = check(manifest, bad)
    caught = reason is not None
    print("\n  truncate-compare            {}".format("caught" if caught else "MISSED"))
    print("      {}".format(reason or "the correctness gate did not fire"))
    passed &= caught

    lying = measure(dedup_path, tree, volume, manifest["min_size"],
                    sabotage="underreport-bytes")
    gap = byte_gap(lying) if lying.get("ok") else 0.0
    caught = gap > BYTE_TOLERANCE and check(manifest, lying) is not None
    print("\n  underreport-bytes           {}".format("caught" if caught else "MISSED"))
    print("      byte gap {:.1%} against a {:.0%} tolerance".format(gap, BYTE_TOLERANCE))
    passed &= caught

    # The re-scan gate, twice: once honest, to show the gate can be satisfied,
    # and once with the deletion-time comparison removed, to show it cannot be
    # satisfied without it. A corpus is rebuilt for each, because the gate
    # deletes from what it scans.
    print("\nselftest: scenario rescan at smoke scale\n")
    rfactor = corpus.SCALES["smoke"]
    base, manifest = ensure_corpus(volume, "rescan", "smoke", seed, rfactor,
                                   rebuild=True)
    verdict, cold, warm = rescan_gate(dedup_path, volume, base, manifest)
    ok = verdict == "pass"
    print("  control, no sabotage        {}".format(
        "clean" if ok else "UNEXPECTED FAILURE: {}".format(verdict)))
    if ok:
        print("      warm read {} against a cold {} ({:.2%})".format(
            human(warm), human(cold), warm / cold if cold else 0))
    passed &= ok

    base, manifest = ensure_corpus(volume, "rescan", "smoke", seed, rfactor,
                                   rebuild=True)
    verdict, _, _ = rescan_gate(sabotaged_dedup(dedup_path, "skip-attested-verify"),
                                volume, base, manifest)
    caught = verdict.startswith("fail")
    print("\n  skip-attested-verify        {}".format("caught" if caught else "MISSED"))
    print("      {}".format(verdict if caught else
                            "deletion skipped the comparison and nothing fired"))
    passed &= caught

    print("\nselftest {}".format("passed" if passed else "FAILED"))
    return 0 if passed else 1


# ─────────────────────────────────────────────────────────────────────────────
# the run
# ─────────────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="bench/run.py",
        description="Benchmark dedup against a fixed corpus, with correctness as a gate.")
    p.add_argument("--scenario", action="append", metavar="NAME",
                   help="run only this scenario (repeatable)")
    p.add_argument("--seed", type=int, default=1, help="corpus seed (default: 1)")
    p.add_argument("--scale", choices=sorted(corpus.SCALES), default="default")
    p.add_argument("--rounds", type=int, default=3,
                   help="interleaved rounds per side; the minimum is reported")
    p.add_argument("--no-tests", action="store_true", help="skip the pytest gate")
    p.add_argument("--no-baseline", action="store_true",
                   help="measure the current dedup.py only; gates still run")
    p.add_argument("--dedup", default=os.path.join(REPO, "dedup.py"),
                   help="the dedup.py under test")
    p.add_argument("--selftest", action="store_true",
                   help="sabotage the tool and check that the gates catch it")
    p.add_argument("--score", action="store_true",
                   help="print one number for the whole run; implies --rounds {}"
                        .format(SCORE_MIN_ROUNDS))
    p.add_argument("--root", default=DEFAULT_ROOT, help="corpus volume")
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return _run(args)
    except Fatal as e:
        print("bench: {}".format(e), file=sys.stderr)
        return 2


def _run(args) -> int:
    root = check_root(args.root)
    dedup_path = os.path.abspath(args.dedup)
    if not os.path.isfile(dedup_path):
        raise Fatal("no such file: {}".format(dedup_path))
    if args.rounds < 1:
        raise Fatal("--rounds must be at least 1")
    if args.score:
        if args.no_tests or args.no_baseline:
            raise Fatal("--score cannot be combined with --no-tests or "
                        "--no-baseline: a score with a gate unrun, or with "
                        "nothing to compare against, is not a score")
        args.rounds = max(args.rounds, SCORE_MIN_ROUNDS)
    check_descriptors()

    megabytes, halve = ram_disk_mb()
    volume_note = ensure_volume(root, megabytes)
    os.makedirs(STATE, exist_ok=True)

    lock = VolumeLock(root).acquire()
    try:
        return _locked(args, root, dedup_path, volume_note, halve)
    finally:
        lock.release()


def _locked(args, root, dedup_path, volume_note, halve) -> int:
    if args.selftest:
        return selftest(dedup_path, root, args.seed)

    factor = corpus.SCALES[args.scale] * (0.5 if halve else 1.0)
    names = args.scenario or ORDER
    for name in names:
        if name not in corpus.SCENARIOS:
            raise Fatal("unknown scenario: {}".format(name))
    names = [n for n in ORDER if n in set(names)]

    print("bench: volume {} ({}), scale {} factor {:g}, seed {}, rounds {}"
          .format(root, volume_note, args.scale, factor, args.seed, args.rounds))
    print("bench: dedup {}".format(dedup_path))

    keep = {corpus.corpus_dir(root, n, args.scale, args.seed) for n in names}
    manifests = {}
    for name in names:
        base = corpus.corpus_dir(root, name, args.scale, args.seed)
        manifest = corpus.load(root, name, args.scale, args.seed)
        # deletion and rescan both destroy what they scan, so neither is reused.
        if (manifest is None or manifest.get("factor") != factor
                or name in ("deletion", "rescan")):
            # Out with the old first: the space it is holding is the space the
            # replacement needs.
            shutil.rmtree(base, ignore_errors=True)
            ensure_space(root, int(corpus.estimated_bytes(name, factor) * 1.4), keep)
            started = time.monotonic()
            manifest = corpus.build(root, name, args.seed, args.scale, factor)
            print("  built {:<12} {:>7} files  {:>9}  in {:.1f}s".format(
                name, manifest["file_count"], human(manifest["total_bytes"]),
                time.monotonic() - started))
        manifests[name] = manifest

    gates = {"pytest": "skipped", "correctness": "pass",
             "deletion": "skipped", "adversarial": "skipped",
             "rescan": "skipped"}

    if not args.no_tests:
        print("\nbench: pytest gate ...", end=" ", flush=True)
        gates["pytest"] = pytest_gate(dedup_path)
        print(gates["pytest"] if gates["pytest"] == "pass" else "FAILED")

    sha = baseline_sha()
    baseline_path = None if args.no_baseline else materialize_baseline(sha)

    scenarios = {}
    profile = None
    for name in names:
        manifest = manifests[name]
        base = corpus.corpus_dir(root, name, args.scale, args.seed)
        tree = os.path.join(base, "tree")
        if name == "deletion":
            print("\nbench: deletion gate ...", end=" ", flush=True)
            gates["deletion"] = deletion_gate(dedup_path, root, base, manifest)
            print(gates["deletion"] if gates["deletion"] == "pass" else "FAILED")
            scenarios[name] = blank_entry(gates["deletion"])
            continue

        if name not in CACHE_FREE:
            print("\nbench: rescan gate ...", end=" ", flush=True)
            verdict, cold, warm = rescan_gate(dedup_path, root, base, manifest)
            gates["rescan"] = verdict
            print("pass" if verdict == "pass" else "FAILED")
            if verdict != "pass":
                print("    {}".format(verdict))
            entry = blank_entry(verdict)
            entry["bytes"] = {"current": warm, "baseline": cold}
            scenarios[name] = entry
            continue

        timed = manifest["timed"]
        timeout = SCAN_TIMEOUT if timed else ADVERSARIAL_TIMEOUT
        rounds = args.rounds if timed else 1
        sides = {"current": [], "baseline": []}
        print("\nbench: {} ...".format(name), end=" ", flush=True)
        for _ in range(rounds):
            if baseline_path:
                sides["baseline"].append(
                    measure(baseline_path, tree, root, manifest["min_size"],
                            timeout=timeout))
            sides["current"].append(
                measure(dedup_path, tree, root, manifest["min_size"], timeout=timeout))

        problem = None
        for side in ("baseline", "current"):
            for result in sides[side]:
                reason = check(manifest, result)
                if reason and problem is None:
                    problem = "{}: {}".format(side, reason)
            counted = {r["bytes_instrumented"] for r in sides[side] if r.get("ok")}
            if len(counted) > 1 and problem is None:
                problem = ("{} read a different number of bytes from one round to "
                           "the next: {}".format(side, sorted(counted)))
        print("pass" if problem is None else "FAILED")
        if problem is not None:
            print("    {}".format(problem))
            key = "correctness" if timed else "adversarial"
            if not gates[key].startswith("fail"):
                gates[key] = "fail: {} — {}".format(name, problem)
            scenarios[name] = blank_entry("fail: {}".format(problem))
            continue

        if sides["current"] and sides["current"][0].get("ok"):
            profile = {"jobs": sides["current"][0]["jobs"],
                       "solid_state": sides["current"][0]["solid_state"]}
        if not timed:
            gates["adversarial"] = "pass"
            # One round is too thin to time, but bytes are deterministic and
            # worth having: it puts this scenario behind the same bytes veto
            # as every other, which is the only thing a score can do with it.
            entry = blank_entry("pass")
            for side, runs in sides.items():
                if runs and runs[0].get("ok"):
                    entry["bytes"][side] = runs[0]["bytes_instrumented"]
            scenarios[name] = entry
            continue
        scenarios[name] = summarize(sides)

    verdict = "pass"
    for value in gates.values():
        if value.startswith("fail"):
            verdict = "fail"

    score_value, void_reason = score(gates, scenarios) if args.score else (None, None)

    results = {
        "meta": {
            "seed": args.seed, "scale": args.scale, "factor": factor,
            "rounds": args.rounds, "baseline_sha": None if args.no_baseline else sha,
            "current_sha_or_dirty": current_id(dedup_path),
            "dedup_path": dedup_path, "host": platform.node(),
            "started_iso": datetime.now().astimezone().isoformat(timespec="seconds"),
            "halved_for_memory": halve,
            "reader_profile": profile,
        },
        "gates": gates,
        "scenarios": scenarios,
        "verdict": verdict,
        "score": None if score_value is None else round(score_value, 4),
        "score_void_reason": void_reason,
    }
    with open(os.path.join(STATE, "results.json"), "w") as fh:
        json.dump(results, fh, indent=1)

    print()
    if verdict == "fail":
        print("GATE FAILURE — no timings are reported for a run that got the "
              "wrong answer.")
        for key, value in gates.items():
            if value.startswith("fail"):
                print("  {}: {}".format(key, value))
    else:
        print(render(scenarios))
        entry = scenarios.get("rescan")
        if entry and entry["bytes"]["baseline"]:
            warm, cold = entry["bytes"]["current"], entry["bytes"]["baseline"]
            print("\nre-scan: a warm scan of an unchanged corpus read {} where "
                  "the cold scan read {} ({:.2%}).".format(
                      human(warm), human(cold), warm / cold))
            print("         Every group it reported from memory is still read "
                  "and compared before anything is deleted.")
    print("\nverdict: {}   ({})".format(verdict, os.path.join(STATE, "results.json")))

    # Last line, always, so a referee can read the score off the tail of the
    # output without parsing anything above it.
    if args.score:
        if score_value is None:
            print("SCORE: void ({})".format(void_reason))
            return 1
        print("SCORE: {:.4f}".format(score_value))
    return 0 if verdict == "pass" else 1


def blank_entry(gate: str) -> dict:
    nulls = {"current": None, "baseline": None}
    return {"gate": gate, "bytes": dict(nulls), "seconds": dict(nulls),
            "maxrss_mb": dict(nulls)}


def summarize(sides: dict) -> dict:
    entry = blank_entry("pass")
    for side, runs in sides.items():
        if not runs:
            continue
        entry["bytes"][side] = runs[0]["bytes_instrumented"]
        entry["seconds"][side] = min(r["seconds"] for r in runs)
        entry["maxrss_mb"][side] = min(r["maxrss_bytes"] for r in runs) / (1 << 20)
    return entry


if __name__ == "__main__":
    sys.exit(main())
