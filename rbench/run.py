#!/usr/bin/env python3
"""RBENCH — the wall-clock benchmark for the Rust tool.

Time is the score, bytes are a constraint, correctness is a gate. A candidate
that gets a wrong answer has no score at all — not a bad one.

What is measured, and why it can be trusted:

  seconds  the child process from spawn to exit, taken by this process with a
           monotonic clock. The tool's own opinion of how long it took is
           never asked for and would not be believed. Both sides are built at
           --release from staged clean checkouts and run in strict alternation,
           and the per-scenario figure is the median of the within-round
           ratios, so drift that moves both sides moves neither's ratio.

  bytes    self-reported by the candidate, because it is a foreign binary and
           there is no honest way to wrap its reads from out here. That number
           is therefore penned in from both directions: it may never exceed
           what the frozen Python oracle read on the same corpus (the ceiling,
           measured once at generation time with the oracle's open() wrapped),
           and it may never fall below what proving the planted groups
           actually costs (the floor, derived from the corpus manifest). A
           third check holds it against the kernel's physical-read counter.

  answers  never the tool's word against itself. The generators know every
           duplicate group they planted; the truth is derived from the corpus
           manifest, and the oracle is made to agree with it before a single
           timing is taken.

Corpora live on a RAM disk so that no run competes with another for a real
device. The manifest sits beside the scanned tree, never inside it.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.util
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
import unicodedata
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
STATE = os.path.join(HERE, "state")
LOCK = os.path.join(HERE, "baseline.lock")
ORACLE_DRIVER = os.path.join(REPO, "conformance", "oracle_driver.py")
DEFAULT_ROOT = "/Volumes/dedupbench"

sys.path.insert(0, os.path.join(REPO, "bench"))
import corpus                                          # noqa: E402

# ── the scored set ──────────────────────────────────────────────────────────
#
# The six inherited from the Python bench, plus `rescan`. The old bench froze
# its six and refused to let a later scenario join them, because a score is
# only worth something if today's number compares with one from months ago.
# RBENCH is a new score against a new baseline, so the freeze starts here
# instead: this list is fixed from the day it was written, and a scenario
# added later earns its keep as a gate, not as a term.
SCORE_SCENARIOS = ("needle", "typical", "samesize", "widegroup", "photos",
                   "bigheader", "rescan")

# Order the table is printed in. Anything here that is not scored still runs
# and still gates.
ORDER = list(SCORE_SCENARIOS)

# `rescan` is the one scenario that runs with the cache switched on: its
# cold/warm split is the whole point of it. Every other scenario passes
# --no-cache, so no score can ever be bought by remembering the answer to a
# corpus the harness is about to ask about nine more times.
CACHE_ON = ("rescan",)

# `rescan` always builds at smoke scale whatever --scale asked for. It is a
# warm-cache measurement — everything it reports is a ratio — and its 430 MB
# at default scale is what tips the corpora past what the RAM disk holds.
GATE_SCALE = {"rescan": "smoke"}

# A warm scan of a corpus nobody has touched should read essentially nothing.
# Five percent is a ceiling with room in it; the real figure is zero.
WARM_CEILING = 0.05

# Interleaved rounds per side. The spec's floor is 9 and so is this.
MIN_ROUNDS = 9

# Physical reads may exceed logical reads — readahead, metadata, the binary's
# own pages — but not by a factor like this. On a RAM disk the physical
# counter reads ~0 and this check is nearly vacuous; see RGOAL.md.
PHYS_TOLERANCE = 4.0
PHYS_SLACK = 64 << 20

# Process start to first real work, measured as a whole run against an empty
# directory. Inherited from the old bench; see the note in setup_seconds().
SETUP_CEILING = 2.0

SCAN_TIMEOUT = 1800
BUILD_TIMEOUT = 900

# One harness run at a time per volume.
LOCK_NAME = ".bench.lock"
LOCK_WAIT = 1800.0
LOCK_POLL = 2.0
LOCK_GRACE = 5.0

# Other harnesses that must not be scanning while this one times anything.
BUSY_PATTERN = r"bench/run\.py|_measure\.py|rbench|dedup-rs|oracle_driver"


class Fatal(Exception):
    """Something about the machine or the harness is wrong — exit 2."""


class Void(Exception):
    """A gate failed. There is no score, and no timings are printed."""


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
    """The fence. Exactly one volume is in bounds and it is named here."""
    root = os.path.abspath(root)
    if root.startswith("/Volumes/") and not (
            root == DEFAULT_ROOT or root.startswith(DEFAULT_ROOT + os.sep)):
        raise Fatal("refusing to touch {}: {} is the only volume this harness "
                    "may use".format(root, DEFAULT_ROOT))
    return root


def ram_disk_mb() -> tuple:
    """(megabytes, halve_the_corpus) — a 16 GB machine can spare 2816 MB."""
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
        os.makedirs(root, exist_ok=True)
        if not os.access(root, os.W_OK):
            raise Fatal("{} is not writable".format(root))
        if os.stat(root).st_dev == os.stat("/").st_dev:
            return "boot disk — gates are valid here, timings are not"
        return "existing"

    attach = run(["hdiutil", "attach", "-nomount",
                  "ram://{}".format(megabytes * 1024 * 1024 // 512)])
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
    """Corpora are regenerable, so one this run does not want goes first.

    Only ever under `corpora/`, and only ever while the volume lock is held.
    Everything else on the volume belongs to somebody else.
    """
    if shutil.disk_usage(volume).free >= need:
        return
    junk = os.path.join(volume, ".fseventsd")
    if os.path.isdir(junk):
        # Regenerable, and it has been large enough on its own to fail a run.
        shutil.rmtree(junk, ignore_errors=True)
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


def other_harnesses() -> list:
    """Anything else on this machine that might be scanning.

    A second harness on the same volume does not merely slow this one down,
    it evicts corpora underneath it, and the wreckage arrives looking like a
    gate failure in the candidate. Our own process group is excluded: this
    file's path contains "rbench" and would otherwise match itself.
    """
    out = run(["ps", "-Ao", "pid=,pgid=,command="])
    mine = os.getpid()
    try:
        my_group = os.getpgid(mine)
    except OSError:
        my_group = None
    found = []
    for line in out.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        try:
            pid, pgid = int(parts[0]), int(parts[1])
        except ValueError:
            continue
        command = parts[2]
        if pid == mine or (my_group is not None and pgid == my_group):
            continue
        if command.startswith("ps ") or " ps -Ao" in command:
            continue
        if re.search(BUSY_PATTERN, command):
            found.append("{} {}".format(pid, command[:90]))
    return found


# ─────────────────────────────────────────────────────────────────────────────
# one run at a time
# ─────────────────────────────────────────────────────────────────────────────

class VolumeLock:
    """Serialize harness runs that share a volume.

    Advisory, and deliberately so — a file with a pid in it, not a kernel
    lock. Anything that bypasses it is on its own. But every path in this file
    that writes to the volume is behind it.
    """

    def __init__(self, volume: str, wait: float = LOCK_WAIT):
        self.path = os.path.join(volume, LOCK_NAME)
        self.wait = wait
        self.pid = os.getpid()
        self.held = False

    def _payload(self) -> dict:
        return {"pid": self.pid, "host": platform.node(), "who": "rbench",
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
            # Either a lock caught between its create and its write, or a file
            # that will never parse. Age tells them apart, and a lock nobody
            # can read must not wedge the volume forever.
            if time.time() - written < LOCK_GRACE:
                return "a lock still being written", True
            return "an unreadable lock file", False
        text = "pid {} ({}) on {}, since {}".format(
            data.get("pid"), data.get("who", "?"), data.get("host"),
            data.get("started", "?"))
        pid = data.get("pid")
        if data.get("host") != platform.node() or not isinstance(pid, int):
            return text, True
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return text, False
        except PermissionError:
            pass
        return text, True

    def _claim_stale(self) -> bool:
        """Take over a lock whose owner is gone, and say whether we won."""
        temp = "{}.{}".format(self.path, self.pid)
        with open(temp, "w") as fh:
            json.dump(self._payload(), fh)
        os.replace(temp, self.path)
        time.sleep(0.2)
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
                time.sleep(0.05)
                continue
            text, alive = holder
            last = text
            if not alive:
                print("rbench: clearing a stale lock ({})".format(text))
                if self._claim_stale():
                    self.held = True
                    return self
                continue
            if not announced:
                print("rbench: {} is locked by {} — waiting up to {:.0f}s"
                      .format(self.path, text, self.wait))
                announced = True
            time.sleep(LOCK_POLL)

    def release(self) -> None:
        if not self.held:
            return
        self.held = False
        data = self._read()
        if data and data.get("pid") != self.pid:
            return
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


def stage_from_git(sha: str) -> str:
    """A clean `rust/` at `sha`, extracted from git, never from a worktree.

    Only `rust/` is taken. A candidate branch's edits to `rbench/` — to this
    file, to the corpus generators, to the lock — cannot reach the thing being
    measured, because the thing being measured is a directory the harness
    extracts by name. That is the referee protocol, mechanized.
    """
    full = git("rev-parse", sha).strip()
    stage = os.path.join(STATE, "src", full)
    marker = os.path.join(stage, ".staged")
    if os.path.exists(marker):
        return stage
    shutil.rmtree(stage, ignore_errors=True)
    os.makedirs(stage)
    archive = subprocess.Popen(["git", "-C", REPO, "archive", full, "rust"],
                               stdout=subprocess.PIPE)
    untar = subprocess.Popen(["tar", "-x", "-C", stage], stdin=archive.stdout)
    archive.stdout.close()
    untar.communicate()
    if untar.returncode != 0 or not os.path.isdir(os.path.join(stage, "rust")):
        shutil.rmtree(stage, ignore_errors=True)
        raise Fatal("could not extract rust/ at {}".format(sha))
    with open(marker, "w") as fh:
        fh.write(full)
    return stage


def stage_from_path(path: str) -> str:
    """A clean copy of a `rust/` source tree, with build output left behind."""
    source = os.path.abspath(path)
    if os.path.isdir(os.path.join(source, "rust")):
        source = os.path.join(source, "rust")
    if not os.path.isfile(os.path.join(source, "Cargo.toml")):
        raise Fatal("no Cargo.toml under {}".format(source))
    stage = os.path.join(STATE, "src", "path-" + re.sub(r"\W+", "-", source).strip("-"))
    shutil.rmtree(stage, ignore_errors=True)
    os.makedirs(stage)
    shutil.copytree(source, os.path.join(stage, "rust"),
                    ignore=shutil.ignore_patterns("target", ".git"))
    return stage


def build_side(stage: str, label: str) -> str:
    """cargo build --release, into a target directory the harness owns."""
    manifest = os.path.join(stage, "rust", "Cargo.toml")
    target = os.path.join(stage, "target")
    binary = os.path.join(target, "release", "dedup-rs")
    if os.path.isfile(binary):
        return binary
    print("rbench: building {} ...".format(label), end=" ", flush=True)
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = target
    started = time.monotonic()
    out = run(["cargo", "build", "--release", "--manifest-path", manifest],
              env=env, timeout=BUILD_TIMEOUT)
    if out.returncode != 0 or not os.path.isfile(binary):
        print("FAILED")
        raise Fatal("cargo build failed for {}:\n{}".format(
            label, (out.stderr or out.stdout).strip()[-1500:]))
    print("{:.1f}s".format(time.monotonic() - started))
    return binary


def resolve_side(spec, label: str) -> tuple:
    """(identity, binary) for a sha, a path, or the repo's own rust/."""
    if spec is None:
        stage = stage_from_path(os.path.join(REPO, "rust"))
        head = git("rev-parse", "HEAD").strip()[:12]
        dirty = git("status", "--porcelain", "--", "rust").strip()
        return ("worktree {}{}".format(head, "+dirty" if dirty else ""),
                build_side(stage, label))
    if os.path.exists(spec):
        return ("path:" + os.path.abspath(spec), build_side(stage_from_path(spec), label))
    try:
        full = git("rev-parse", "--verify", "{}^{{commit}}".format(spec)).strip()
    except Fatal:
        raise Fatal("{} is neither a path nor a commit".format(spec))
    return (full[:12], build_side(stage_from_git(full), label))


# ─────────────────────────────────────────────────────────────────────────────
# the truth, and the ceiling
# ─────────────────────────────────────────────────────────────────────────────

def canonical_name(names: list) -> str:
    """The one name that stands for a record: shortest, then lexicographic.

    A hardlinked inode wears several names and the walk meets them in whatever
    order the directory hands them over. This is the rule the oracle uses and
    the rule the port implements, so it is the rule the answer key uses too.
    """
    return min(names, key=lambda n: (len(n), n))


def truth_groups(manifest: dict) -> list:
    """The planted duplicate groups, in the shape a report states them in.

    Derived from corpus construction — the generator recorded every group it
    laid down — and not from any tool's opinion of the tree.
    """
    out = []
    for group in manifest["groups"]:
        members = sorted((canonical_name(m["names"]) for m in group["members"]),
                         key=lambda n: (len(n), n))
        out.append((group["size"], members))
    out.sort(key=lambda sm: (-sm[0], os.path.basename(sm[1][0]), sm[1][0]))
    return [members for _size, members in out]


def group_set(groups: list) -> frozenset:
    """Groups as a set of sets of NFC names.

    Order is deliberately not compared here. Which files are copies of which
    is what a benchmark can be fooled about; the order they are printed in is
    the conformance suite's business, and voiding a timing run over a
    tie-break would be a false alarm dressed as rigour.
    """
    return frozenset(frozenset(unicodedata.normalize("NFC", n) for n in g)
                     for g in groups)


def describe_groups(want: frozenset, got: frozenset, limit: int = 2) -> str:
    missing = [sorted(g)[:3] for g in list(want - got)[:limit]]
    extra = [sorted(g)[:3] for g in list(got - want)[:limit]]
    bits = []
    if missing:
        bits.append("{} group(s) not reported, e.g. {}".format(len(want - got), missing))
    if extra:
        bits.append("{} group(s) reported that were never planted, e.g. {}"
                    .format(len(got - want), extra))
    return "; ".join(bits) or "same groups, different shape"


def byte_floor(manifest: dict) -> int:
    """What proving these groups costs, at the very least.

    Nothing is a duplicate on the strength of a hash, so every member of every
    group has to be read end to end at least once. A candidate that reports
    fewer bytes than this did not prove what it claimed — or is lying about
    its bytes, which the ceiling alone cannot catch because the ceiling only
    looks upward.
    """
    return sum(g["size"] * len(g["members"]) for g in manifest["groups"])


def run_oracle(tree: str, min_size: int, oracle_py: str) -> dict:
    """The frozen Python oracle, with its reads counted from the outside.

    conformance/oracle_driver.py wraps builtins.open and adds up what read()
    actually returns, so this number is measured rather than claimed. It is
    the ceiling every candidate is held under.
    """
    report = os.path.join(STATE, "oracle-report.json")
    out = run([sys.executable, ORACLE_DRIVER, tree, "--report-json", report,
               "--dedup", oracle_py, "--min-size", str(min_size), "--no-cache"],
              timeout=SCAN_TIMEOUT)
    try:
        with open(report) as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        raise Fatal("the oracle produced no report: {}".format(
            (out.stderr or out.stdout).strip()[-500:]))
    if not data.get("ok"):
        raise Fatal("the oracle failed: {}".format(data.get("error", "?")))
    if data.get("tripwire"):
        raise Fatal("the oracle read outside the tree: {}".format(data["tripwire"][0]))
    return data


def ensure_corpus(volume: str, name: str, scale: str, seed: int, factor: float,
                  oracle_py: str, oracle_sha: str, rebuild: bool = False) -> tuple:
    """(base, manifest, ceiling), generating and measuring what is missing.

    The ceiling is computed once, when the corpus is laid down, and cached
    beside it. Running the oracle is the expensive part of generation and its
    answer cannot change while the corpus does not.
    """
    base = corpus.corpus_dir(volume, name, scale, seed)
    sidecar = os.path.join(base, "rbench.json")
    manifest = None if rebuild else corpus.load(volume, name, scale, seed)
    ceiling = None
    if manifest is not None and manifest.get("factor") == factor:
        try:
            with open(sidecar) as fh:
                ceiling = json.load(fh)
            if (ceiling.get("oracle_sha") != oracle_sha
                    or ceiling.get("generator_version") != corpus.GENERATOR_VERSION):
                ceiling = None
        except (OSError, ValueError):
            ceiling = None
    else:
        manifest = None

    if manifest is None:
        shutil.rmtree(base, ignore_errors=True)
        ensure_space(volume, int(corpus.estimated_bytes(name, factor) * 1.4), {base})
        started = time.monotonic()
        manifest = corpus.build(volume, name, seed, scale, factor)
        print("  built {:<10} {:>7} files  {:>9}  in {:.1f}s".format(
            name, manifest["file_count"], human(manifest["total_bytes"]),
            time.monotonic() - started))
        ceiling = None

    if ceiling is None:
        tree = os.path.join(base, "tree")
        started = time.monotonic()
        oracle = run_oracle(tree, manifest["min_size"], oracle_py)
        truth = truth_groups(manifest)
        # The answer key and the frozen oracle have to agree before anything is
        # measured against either. If they do not, the disagreement is the
        # story, and no timing taken today would mean anything.
        if group_set(truth) != group_set(oracle["groups"]):
            raise Fatal(
                "the oracle disagrees with corpus construction on {}: {}"
                .format(name, describe_groups(group_set(truth),
                                              group_set(oracle["groups"]))))
        ceiling = {
            "oracle_sha": oracle_sha,
            "generator_version": corpus.GENERATOR_VERSION,
            "oracle_bytes_read": oracle["bytes_read"],
            "oracle_opens": oracle["opens"],
            "byte_floor": byte_floor(manifest),
            "min_size": manifest["min_size"],
            "groups": truth,
        }
        with open(sidecar, "w") as fh:
            json.dump(ceiling, fh)
        print("  ceiling {:<9} oracle read {:>9} in {:>3} opens  ({:.1f}s)".format(
            name, human(ceiling["oracle_bytes_read"]), ceiling["oracle_opens"],
            time.monotonic() - started))
    return base, manifest, ceiling


def fingerprint(tree: str) -> dict:
    """Size and mtime of every file under the tree.

    Taken before the scored rounds and again afterwards. A benchmark whose
    corpus moved during the run measured two different things and called them
    one, and a candidate that shrinks its own work by editing the corpus is
    the case this exists for.
    """
    out = {}
    for path, _dirs, files in os.walk(tree):
        for name in files:
            full = os.path.join(path, name)
            try:
                st = os.lstat(full)
            except OSError:
                continue
            out[os.path.relpath(full, tree)] = (st.st_size, st.st_mtime_ns)
    return out


def fingerprint_diff(before: dict, after: dict) -> str:
    for rel, was in sorted(before.items()):
        now = after.get(rel)
        if now is None:
            return "{} disappeared during the run".format(rel)
        if now[0] != was[0]:
            return "{} changed size during the run ({} -> {})".format(
                rel, was[0], now[0])
        if now[1] != was[1]:
            return "{} was modified during the run".format(rel)
    for rel in sorted(after):
        if rel not in before:
            return "{} appeared during the run".format(rel)
    return ""


# ─────────────────────────────────────────────────────────────────────────────
# the kernel's opinion of what was read
# ─────────────────────────────────────────────────────────────────────────────

_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
_libc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int,
                                  ctypes.POINTER(ctypes.c_void_p)]
_libc.proc_pid_rusage.restype = ctypes.c_int
RUSAGE_INFO_V2 = 2
_DISKIO_BYTESREAD = 16          # uint64 index, past the 16-byte uuid


def physical_read(pid: int):
    """Bytes this process has actually pulled off a device, or None.

    proc_pid_rusage only answers about a living process, so this is sampled
    while the child runs and the last sample is the one kept. Anything the
    child reads in its final milliseconds is therefore missed, which biases
    the number downwards — the safe direction for a tripwire, since a low
    reading can only ever fail to fire.
    """
    buf = (ctypes.c_uint8 * 512)()
    rc = _libc.proc_pid_rusage(pid, RUSAGE_INFO_V2,
                               ctypes.cast(ctypes.byref(buf),
                                           ctypes.POINTER(ctypes.c_void_p)))
    if rc != 0:
        return None
    return (ctypes.c_uint64 * 62).from_buffer(buf, 16)[_DISKIO_BYTESREAD]


# ─────────────────────────────────────────────────────────────────────────────
# one scan
# ─────────────────────────────────────────────────────────────────────────────

def scan(binary: str, tree: str, min_size: int, *, cache_dir: str, tag: str,
         no_cache: bool = True, sample: bool = False,
         timeout: int = SCAN_TIMEOUT) -> dict:
    """Spawn the tool once and time the whole process.

    The report goes to a path this run owns, and DEDUP_CACHE_DIR always points
    somewhere the harness owns: the tool's defaults are one fixed filename and
    one shared cache directory per machine, and either would let two runs on
    this box read each other's leavings.
    """
    report = os.path.join(STATE, "reports", "{}.json".format(tag))
    os.makedirs(os.path.dirname(report), exist_ok=True)
    try:
        os.unlink(report)                  # never mistake a stale report for output
    except OSError:
        pass
    env = dict(os.environ)
    env["DEDUP_CACHE_DIR"] = cache_dir
    cmd = [binary, tree, "--report-json", report, "--min-size", str(min_size)]
    if no_cache:
        cmd.append("--no-cache")

    phys = None
    started = time.perf_counter()
    try:
        if sample:
            proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, env=env, text=True)
            while proc.poll() is None:
                got = physical_read(proc.pid)
                if got is not None:
                    phys = got
                time.sleep(0.002)
            err = proc.communicate(timeout=timeout)[1]
            code = proc.returncode
        else:
            out = subprocess.run(cmd, capture_output=True, text=True, env=env,
                                 timeout=timeout)
            err, code = out.stderr, out.returncode
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timed out after {}s".format(timeout)}
    seconds = time.perf_counter() - started

    if code != 0:
        return {"ok": False, "error": "exited {}: {}".format(code, err.strip()[-300:])}
    try:
        with open(report) as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return {"ok": False, "error": "wrote no readable report"}
    data["ok"] = True
    data["seconds"] = seconds
    data["physical_read"] = phys
    return data


def setup_seconds(binary: str, cache_dir: str) -> float:
    """How long the tool takes to do nothing at all.

    The old bench needed this because it timed the scan call rather than the
    process, so work moved into startup left the clock. Here the clock covers
    spawn to exit and there is nowhere to hide, which leaves this as what it
    says: a ceiling on fixed overhead, so a candidate cannot pay for a fast
    scan with a second of loading.
    """
    empty = os.path.join(STATE, "empty")
    shutil.rmtree(empty, ignore_errors=True)
    os.makedirs(empty)
    times = []
    for i in range(3):
        got = scan(binary, empty, 1, cache_dir=cache_dir, tag="setup-{}".format(i))
        if not got.get("ok"):
            raise Void("the tool could not scan an empty directory: {}"
                       .format(got.get("error", "?")))
        times.append(got["seconds"])
    return min(times)


def check_run(result: dict, ceiling: dict, *, floor: bool, side: str,
              scenario: str) -> str:
    """Empty when this scan is admissible, otherwise why it is not."""
    if not result.get("ok"):
        return "{}: scan failed: {}".format(side, result.get("error", "?"))

    want = group_set(ceiling["groups"])
    got = group_set(result.get("groups", []))
    if want != got:
        return "{}: wrong answer on {} — {}".format(
            side, scenario, describe_groups(want, got))

    read = result.get("bytes_read", 0)
    if read > ceiling["oracle_bytes_read"]:
        return ("{}: read {} where the oracle read {} — reading more than the "
                "oracle is a regression".format(
                    side, human(read), human(ceiling["oracle_bytes_read"])))
    if floor and read < ceiling["byte_floor"]:
        return ("{}: read {} — proving these groups takes at least {}, so they "
                "were not proved".format(side, human(read),
                                         human(ceiling["byte_floor"])))

    phys = result.get("physical_read")
    if phys is not None and phys > read * PHYS_TOLERANCE + PHYS_SLACK:
        return ("{}: the kernel says it pulled {} off a device while it reported "
                "reading {}".format(side, human(phys), human(read)))
    return ""


# ─────────────────────────────────────────────────────────────────────────────
# one scenario
# ─────────────────────────────────────────────────────────────────────────────

def next_tick() -> None:
    """Block until the wall clock crosses a second boundary.

    HFS+ records mtime to the second and the tool refuses to trust a digest
    cached in the same second as its file's mtime. Without this the populating
    scan can start in the tick that built the corpus, every row it writes is
    racily clean, and the warm scan re-reads all of it — a correct answer to a
    real ambiguity, but not the thing being measured.
    """
    edge = int(time.time())
    while int(time.time()) == edge:
        time.sleep(0.02)


def measure_scenario(name: str, tree: str, ceiling: dict, sides: dict,
                     rounds: int) -> dict:
    """Interleaved rounds, one clean answer per run, ratios paired by round.

    Pairing within a round and taking the median of the ratios is what makes
    this survive a machine whose absolute timings drift by a factor of two
    across an afternoon: drift that moves both sides in a round divides out of
    that round's ratio, and the median throws away the rounds where it did not.
    """
    warm = name in CACHE_ON
    min_size = ceiling["min_size"]
    caches = {side: os.path.join(STATE, "cache", "{}-{}".format(name, side))
              for side in sides}
    for path in caches.values():
        shutil.rmtree(path, ignore_errors=True)
        os.makedirs(path)

    before = fingerprint(tree)
    entry = {"rounds": [], "cold_bytes": {}, "warm_bytes": None}

    # ── the untimed run each side gets before the clock starts ───────────────
    #
    # Everywhere but `rescan` this is a page-cache warm-up and nothing more.
    # In `rescan` it is the cold scan that fills the cache, and it is the
    # measurement the warm rounds are a ratio against.
    for side, binary in sides.items():
        if warm:
            next_tick()
        first = scan(binary, tree, min_size, cache_dir=caches[side],
                     tag="{}-{}-prime".format(name, side), no_cache=not warm,
                     sample=True)
        reason = check_run(first, ceiling, floor=True, side=side, scenario=name)
        if reason:
            raise Void(reason)
        if warm:
            entry["cold_bytes"][side] = first["bytes_read"]
            next_tick()

    # ── the scored rounds ────────────────────────────────────────────────────
    for index in range(rounds):
        row = {}
        for side, binary in sides.items():
            got = scan(binary, tree, min_size, cache_dir=caches[side],
                       tag="{}-{}-{}".format(name, side, index),
                       no_cache=not warm)
            reason = check_run(got, ceiling, floor=not warm, side=side,
                               scenario=name)
            if reason:
                raise Void(reason)
            row[side] = {"seconds": got["seconds"], "bytes": got["bytes_read"],
                         "opens": got["opens"]}
        entry["rounds"].append(row)

    moved = fingerprint_diff(before, fingerprint(tree))
    if moved:
        raise Void("the corpus moved under {}: {}".format(name, moved))

    for side in sides:
        counted = {r[side]["bytes"] for r in entry["rounds"]}
        if len(counted) > 1:
            raise Void("{} read a different number of bytes from one round to "
                       "the next on {}: {}".format(side, name, sorted(counted)))

    if warm:
        entry["warm_bytes"] = entry["rounds"][0]["candidate"]["bytes"]
        cold = entry["cold_bytes"].get("candidate", 0)
        if not cold:
            raise Void("the cold scan of {} read nothing, so there is no warm "
                       "ratio to check".format(name))
        ratio = entry["warm_bytes"] / cold
        if ratio >= WARM_CEILING:
            raise Void("warm scan read {} of the cold scan's {} ({:.1%}), which "
                       "is not a re-scan cache".format(
                           human(entry["warm_bytes"]), human(cold), ratio))

    entry["ratios"] = [r["candidate"]["seconds"] / r["baseline"]["seconds"]
                       for r in entry["rounds"]]
    entry["ratio"] = statistics.median(entry["ratios"])
    entry["baseline_seconds"] = statistics.median(
        r["baseline"]["seconds"] for r in entry["rounds"])
    entry["candidate_seconds"] = statistics.median(
        r["candidate"]["seconds"] for r in entry["rounds"])
    entry["bytes"] = entry["rounds"][0]["candidate"]["bytes"]
    entry["baseline_bytes"] = entry["rounds"][0]["baseline"]["bytes"]
    entry["oracle_bytes"] = ceiling["oracle_bytes_read"]
    entry["floor_bytes"] = ceiling["byte_floor"]
    return entry


# ─────────────────────────────────────────────────────────────────────────────
# the table and the score
# ─────────────────────────────────────────────────────────────────────────────

def geomean(values) -> float:
    values = list(values)
    return math.exp(sum(math.log(v) for v in values) / len(values))


def spread(values) -> float:
    """Half the peak-to-peak of a set of ratios, as a fraction."""
    values = list(values)
    return (max(values) - min(values)) / 2.0


def render(scenarios: dict) -> str:
    header = ("scenario", "base s", "cand s", "ratio", "spread", "bytes",
              "ceiling", "floor")
    rows = [header]
    for name in ORDER:
        entry = scenarios.get(name)
        if entry is None:
            continue
        rows.append((
            name,
            "{:.4f}".format(entry["baseline_seconds"]),
            "{:.4f}".format(entry["candidate_seconds"]),
            "{:.4f}".format(entry["ratio"]),
            "±{:.1%}".format(spread(entry["ratios"])),
            human(entry["bytes"]),
            human(entry["oracle_bytes"]),
            human(entry["floor_bytes"]),
        ))
    widths = [max(len(r[i]) for r in rows) for i in range(len(header))]
    lines = []
    for i, row in enumerate(rows):
        lines.append("  ".join(cell.rjust(widths[j]) if j else cell.ljust(widths[j])
                               for j, cell in enumerate(row)).rstrip())
        if i == 0:
            lines.append("─" * len(lines[0]))
    return "\n".join(lines)


# ─────────────────────────────────────────────────────────────────────────────
# selftest: candidates that cheat, and the gates that must notice
# ─────────────────────────────────────────────────────────────────────────────

SHIM = '''#!/usr/bin/env python3
"""A candidate that cheats, for proving the gates fire."""
import json, os, subprocess, sys

BINARY = {binary!r}
MODE = {mode!r}

code = subprocess.call([BINARY] + sys.argv[1:])
args = sys.argv[1:]
report = args[args.index("--report-json") + 1] if "--report-json" in args else None
root = next((a for a in args if not a.startswith("-")
             and os.path.isdir(a)), None)

if MODE == "touch-corpus" and root:
    for path, _dirs, files in os.walk(root):
        if files:
            victim = os.path.join(path, sorted(files)[0])
            st = os.stat(victim)
            os.utime(victim, ns=(st.st_atime_ns, st.st_mtime_ns - 2_000_000_000))
            break
elif report and os.path.exists(report):
    with open(report) as fh:
        data = json.load(fh)
    if MODE == "drop-group" and data.get("groups"):
        data["groups"] = data["groups"][1:]
    elif MODE == "inflate-bytes":
        data["bytes_read"] = int(data.get("bytes_read", 0)) * 3 + (1 << 20)
    elif MODE == "underreport-bytes":
        data["bytes_read"] = int(data.get("bytes_read", 0)) // 4
    with open(report, "w") as fh:
        json.dump(data, fh)

sys.exit(code)
'''

SABOTAGE = {
    "drop-group": "the correctness gate",
    "inflate-bytes": "the bytes ceiling",
    "underreport-bytes": "the byte floor",
    "touch-corpus": "the manifest tripwire",
}


def sabotaged(binary: str, mode: str) -> str:
    os.makedirs(STATE, exist_ok=True)
    path = os.path.join(STATE, "cheat-{}".format(mode))
    with open(path, "w") as fh:
        fh.write(SHIM.format(binary=binary, mode=mode))
    os.chmod(path, 0o755)
    return path


def selftest(volume: str, seed: int, baseline: str, oracle_py: str,
             oracle_sha: str) -> int:
    """Break the candidate on purpose and insist every gate notices.

    `samesize` at smoke scale is the corpus, because it contains twins that
    differ only in their final byte — precisely what a truncated comparison
    cannot see — and it is small enough to rebuild between cases.
    """
    name, scale = "samesize", "smoke"
    factor = corpus.SCALES[scale]
    print("rbench selftest: {} at {} scale\n".format(name, scale))
    passed = True

    def fresh():
        return ensure_corpus(volume, name, scale, seed, factor, oracle_py,
                             oracle_sha, rebuild=True)

    base, manifest, ceiling = fresh()
    tree = os.path.join(base, "tree")

    try:
        measure_scenario(name, tree, ceiling,
                         {"baseline": baseline, "candidate": baseline}, rounds=1)
        print("  control, no sabotage      clean")
    except Void as e:
        print("  control, no sabotage      UNEXPECTED VOID: {}".format(e))
        passed = False

    for mode, gate in SABOTAGE.items():
        if mode == "touch-corpus":
            base, manifest, ceiling = fresh()
            tree = os.path.join(base, "tree")
        cheat = sabotaged(baseline, mode)
        try:
            measure_scenario(name, tree, ceiling,
                             {"baseline": baseline, "candidate": cheat}, rounds=1)
            print("\n  {:<24}  MISSED — {} did not fire".format(mode, gate))
            passed = False
        except Void as e:
            print("\n  {:<24}  caught by {}".format(mode, gate))
            print("      {}".format(str(e)[:150]))
        if mode == "touch-corpus":
            base, manifest, ceiling = fresh()
            tree = os.path.join(base, "tree")

    print("\nselftest {}".format("passed" if passed else "FAILED"))
    return 0 if passed else 1


# ─────────────────────────────────────────────────────────────────────────────
# the run
# ─────────────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="rbench/run.py",
        description="Wall-clock benchmark for the Rust dedup tool. Time is the "
                    "score, bytes are a constraint, correctness is a gate.")
    p.add_argument("--candidate", default=None, metavar="SHA|PATH",
                   help="what to measure (default: this checkout's rust/)")
    p.add_argument("--scenario", action="append", metavar="NAME",
                   help="run only this scenario (repeatable)")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--scale", choices=sorted(corpus.SCALES), default="default")
    p.add_argument("--rounds", type=int, default=MIN_ROUNDS,
                   help="interleaved rounds per side (minimum {})".format(MIN_ROUNDS))
    p.add_argument("--calibrate", action="store_true",
                   help="time the baseline against itself and report the noise floor")
    p.add_argument("--selftest", action="store_true",
                   help="sabotage the candidate and check that the gates catch it")
    p.add_argument("--root", default=DEFAULT_ROOT)
    p.add_argument("--allow-busy", action="store_true",
                   help="score even though another harness is live (never for a "
                        "number anyone will quote)")
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return _run(args)
    except Fatal as e:
        print("rbench: {}".format(e), file=sys.stderr)
        return 2


def _run(args) -> int:
    root = check_root(args.root)
    if args.rounds < 1:
        raise Fatal("--rounds must be at least 1")
    # A subset run prints no SCORE line, so it may be as thin as it likes. A
    # full run is the thing that gets quoted, and it may not be.
    if not (args.selftest or args.calibrate or args.scenario) \
            and args.rounds < MIN_ROUNDS:
        raise Fatal("--rounds below {} is not a score".format(MIN_ROUNDS))

    busy = other_harnesses()
    if busy and not args.allow_busy:
        raise Fatal("another harness is live; scoring now would measure the "
                    "contention, not the change:\n  " + "\n  ".join(busy))

    os.makedirs(STATE, exist_ok=True)
    megabytes, halve = ram_disk_mb()
    note = ensure_volume(root, megabytes)

    lock = VolumeLock(root).acquire()
    try:
        return _locked(args, root, note, halve, busy)
    finally:
        lock.release()


def _locked(args, root, note, halve, busy) -> int:
    sha = baseline_sha()
    oracle_sha = git("rev-parse", sha).strip()
    oracle_py = os.path.join(STATE, "oracle-{}.py".format(oracle_sha[:12]))
    if not os.path.exists(oracle_py):
        with open(oracle_py, "w") as fh:
            fh.write(git("show", "{}:dedup.py".format(oracle_sha)))

    baseline_id, baseline = resolve_side(sha, "baseline {}".format(sha[:12]))
    if args.selftest:
        return selftest(root, args.seed, baseline, oracle_py, oracle_sha)

    if args.calibrate:
        candidate_id, candidate = "baseline (calibration)", baseline
    else:
        candidate_id, candidate = resolve_side(args.candidate, "candidate")

    names = args.scenario or ORDER
    for name in names:
        if name not in corpus.SCENARIOS:
            raise Fatal("unknown scenario: {}".format(name))
    names = [n for n in ORDER if n in set(names)]

    load = os.getloadavg()
    print("rbench: volume {} ({}), scale {}, seed {}, rounds {}"
          .format(root, note, args.scale, args.seed, args.rounds))
    print("rbench: baseline {}   candidate {}".format(baseline_id, candidate_id))
    print("rbench: load average {:.2f} {:.2f} {:.2f}".format(*load))
    if busy:
        print("rbench: WARNING — another harness is live; these numbers are not "
              "a record")

    sides = {"baseline": baseline, "candidate": candidate}
    scenarios = {}
    void_reason = None
    setup = None
    try:
        setup = setup_seconds(candidate, os.path.join(STATE, "cache", "setup"))
        if setup > SETUP_CEILING:
            raise Void("spent {:.2f}s scanning an empty directory; fixed "
                       "overhead above {:.1f}s is not a scan".format(
                           setup, SETUP_CEILING))
        for name in names:
            scale = GATE_SCALE.get(name, args.scale)
            factor = corpus.SCALES[scale] * (0.5 if halve else 1.0)
            base, manifest, ceiling = ensure_corpus(
                root, name, scale, args.seed, factor, oracle_py, oracle_sha)
            print("rbench: {} ...".format(name), end=" ", flush=True)
            entry = measure_scenario(name, os.path.join(base, "tree"), ceiling,
                                     sides, args.rounds)
            scenarios[name] = entry
            print("{:.4f}  (±{:.1%})".format(entry["ratio"],
                                             spread(entry["ratios"])))
    except Void as e:
        void_reason = str(e)
        print("VOID")

    scored = [scenarios[n]["ratio"] for n in SCORE_SCENARIOS if n in scenarios]
    complete = len(scored) == len(SCORE_SCENARIOS)
    if void_reason is None and not complete and not args.scenario:
        void_reason = "not every scored scenario ran"
    value = geomean(scored) if void_reason is None and scored else None

    results = {
        "meta": {
            "baseline": baseline_id, "candidate": candidate_id,
            "baseline_sha": sha, "oracle_sha": oracle_sha,
            "seed": args.seed, "scale": args.scale, "rounds": args.rounds,
            "host": platform.node(), "load_average": load,
            "halved_for_memory": halve, "setup_seconds": setup,
            "other_harnesses_live": busy,
            "started_iso": datetime.now().astimezone().isoformat(timespec="seconds"),
        },
        "scenarios": scenarios,
        "score": None if value is None or not complete else round(value, 4),
        "partial_geomean": None if value is None or complete else round(value, 4),
        "void_reason": void_reason,
    }
    out_path = os.path.join(STATE, "results.json")
    with open(out_path, "w") as fh:
        json.dump(results, fh, indent=1)

    print()
    if void_reason is not None:
        print("VOID — no timings are reported for a run that failed a gate.")
        print("  {}".format(void_reason))
        print("\n({})".format(out_path))
        print("SCORE: void ({})".format(void_reason))
        return 1

    print(render(scenarios))
    print("\n({})".format(out_path))

    if args.calibrate:
        deltas = [abs(scenarios[n]["ratio"] - 1.0) for n in scenarios]
        widest = [spread(scenarios[n]["ratios"]) for n in scenarios]
        floor = max(max(deltas), max(widest))
        print("\nNOISE FLOOR: ±{:.1%}  (worst median deviation from 1.00: {:.1%}; "
              "worst within-scenario spread: {:.1%})".format(
                  floor, max(deltas), max(widest)))
        print("A score that moves by less than this has not moved.")
        return 0

    if not complete:
        # A subset of the scored scenarios is a reading, not a score, and
        # printing it on the SCORE line is how a reading gets quoted as one.
        print("\npartial geomean over {}: {:.4f}   (not a score — {} of {} "
              "scored scenarios ran)".format(
                  ", ".join(n for n in SCORE_SCENARIOS if n in scenarios),
                  value, len(scored), len(SCORE_SCENARIOS)))
        return 0
    print("\nSCORE: {:.4f}".format(value))
    return 0


if __name__ == "__main__":
    sys.exit(main())
