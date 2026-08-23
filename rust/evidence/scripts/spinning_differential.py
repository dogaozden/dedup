#!/usr/bin/env python3
"""Groups are profile-independent: prove it against the oracle, tree by tree.

SPIN.md's first law is that `--profile spinning` may change access patterns and
nothing else. No spinning disk is available to an agent, so what CAN be proved
is proved here: on scratch SSD trees shaped to attack the profile's changes,
the oracle, `--profile flash`, `--profile spinning` and a flagless run all
report byte-identical groups.

The trees are chosen for what the profile removes. `spinning` does not read the
tail page, so a tree whose only differences are in the tail page is the first
one. It does not sample, so a tree whose only differences are in the middle is
the second. It compares wide groups a cohort at a time, so a tree whose equal
members fall on either side of a cohort boundary is the third.

Run from the repository root:

    python3 rust/evidence/scripts/spinning_differential.py
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
ORACLE = os.path.join(REPO, "conformance", "oracle_driver.py")
SCRATCH = ("/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/"
           "620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad")
BASE = os.path.join(SCRATCH, "feat-spin-differential")

MIB = 1024 * 1024
# The three thresholds the profile moves around, from rust/src/consts.rs.
HEAD_BYTES = 4 * 1024
DEEP_HEAD = 64 * 1024
TAIL_BYTES = 4 * 1024
SAMPLE_SPAN = 8 * MIB
SLOW_CHUNK = 16 * MIB


def body(seed: int, size: int) -> bytearray:
    """Deterministic incompressible-enough content of exactly `size` bytes."""
    rng = random.Random(seed)
    return bytearray(rng.getrandbits(8) for _ in range(min(size, 4096))) * \
        (size // 4096) + bytearray(rng.getrandbits(8) for _ in range(size % 4096))


def make(size: int, seed: int, *, tail=None, middle=None, last=None) -> bytes:
    """One file's content, with the named region overwritten.

    `tail` rewrites the final TAIL_BYTES (what the flash end probe reads and
    the spinning one does not). `middle` rewrites a page halfway through (what
    the sample stage would catch). `last` rewrites the final kilobyte (which
    only a comparison that reads to EOF can catch).
    """
    content = body(seed, size)
    if tail is not None:
        content[size - TAIL_BYTES:] = bytes([tail]) * TAIL_BYTES
    if middle is not None:
        at = size // 2
        content[at:at + 4096] = bytes([middle]) * 4096
    if last is not None:
        content[size - 1024:] = bytes([last]) * 1024
    return bytes(content)


def write(root: str, name: str, content: bytes) -> None:
    path = os.path.join(root, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(content)


# ---------------------------------------------------------------- the trees

def tree_tails(root: str) -> str:
    """Same head, different last page — the bytes `spinning` never probes."""
    size = DEEP_HEAD + TAIL_BYTES + 8192  # long enough that flash reads a tail
    write(root, "a.bin", make(size, 1, tail=0xAA))
    write(root, "b.bin", make(size, 1, tail=0xAA))
    write(root, "c.bin", make(size, 1, tail=0xBB))
    write(root, "d.bin", make(size, 1, tail=0xBB))
    write(root, "e.bin", make(size, 1, tail=0xCC))
    return "identical but for the tail page: two pairs and a singleton"


def tree_middles(root: str) -> str:
    """Same ends, different middle — the bytes the sample stage exists for."""
    size = SAMPLE_SPAN + 2 * MIB
    write(root, "p.bin", make(size, 2, middle=0x11))
    write(root, "q.bin", make(size, 2, middle=0x11))
    write(root, "r.bin", make(size, 2, middle=0x22))
    return "same ends, different middles, past the sampling threshold"


def tree_last_chunk(root: str) -> str:
    """Differences in the final kilobyte of a file that takes several rounds."""
    size = SLOW_CHUNK + 2 * MIB
    for name, mark in (("one.bin", 0x01), ("two.bin", 0x01), ("six.bin", 0x06)):
        write(root, name, make(size, 3, last=mark))
    return "same until the last kilobyte, longer than one compare round"


def tree_cohort_boundary(root: str) -> str:
    """Equal members either side of a cohort boundary at the interleave cap.

    Sixteen same-size files in four content classes, laid out so the first
    round's pivot separates nobody and two members of one class land in
    different cohorts of that round. They can only be found equal by a later
    pass over what the earlier ones left behind.
    """
    size = 2 * MIB
    classes = ["A"] + ["C"] * 6 + ["B", "B"] + ["D"] * 6 + ["A"]
    marks = {"A": 0xA0, "B": 0xB0, "C": 0xC0, "D": 0xD0}
    for position, letter in enumerate(classes):
        write(root, "m{:02d}.bin".format(position),
              make(size, 4, last=marks[letter]))
    return "16 files, 4 content classes, equal members split across cohorts"


def tree_wide(root: str) -> str:
    """One group far wider than the interleave cap."""
    size = SAMPLE_SPAN + MIB
    content = make(size, 5)
    for position in range(24):
        write(root, "w{:02d}.bin".format(position), content)
    return "one group of 24 identical files, three times the cohort width"


def tree_links(root: str) -> str:
    """Hardlink clusters beside ordinary duplicates."""
    size = DEEP_HEAD * 4
    write(root, "keep/base.bin", make(size, 6))
    os.link(os.path.join(root, "keep/base.bin"), os.path.join(root, "link-one.bin"))
    os.makedirs(os.path.join(root, "deep"), exist_ok=True)
    os.link(os.path.join(root, "keep/base.bin"), os.path.join(root, "deep/link-two.bin"))
    write(root, "copy.bin", make(size, 6))
    write(root, "other.bin", make(size, 7))
    write(root, "other-copy.bin", make(size, 7))
    return "a three-name hardlink cluster, a true copy of it, and a pair"


def tree_unreadable(root: str) -> str:
    """Files inside a group that cannot be opened."""
    size = SAMPLE_SPAN + MIB
    content = make(size, 8)
    for name in ("readable-one.bin", "readable-two.bin", "locked.bin",
                 "locked-two.bin", "readable-three.bin"):
        write(root, name, content)
    for name in ("locked.bin", "locked-two.bin"):
        os.chmod(os.path.join(root, name), 0)
    return "a five-file group with two members that cannot be opened"


def tree_mixed(root: str) -> str:
    """A seeded jumble across every size class the pipeline splits on."""
    rng = random.Random(1729)
    sizes = [0, 1, HEAD_BYTES - 1, HEAD_BYTES, HEAD_BYTES + 1,
             DEEP_HEAD - 1, DEEP_HEAD, DEEP_HEAD + 1,
             DEEP_HEAD + TAIL_BYTES, DEEP_HEAD + TAIL_BYTES + 1,
             MIB, SAMPLE_SPAN, SAMPLE_SPAN + 1, SLOW_CHUNK + 7]
    count = 0
    for size in sizes:
        for copy in range(rng.choice([1, 2, 3, 5])):
            seed = rng.choice([100, 200, 300])
            variant = rng.choice([None, 0x5A]) if size > TAIL_BYTES else None
            write(root, "d{}/f{:03d}.bin".format(size % 7, count),
                  make(size, seed, tail=variant))
            count += 1
    return "{} files over every size class, seeded".format(count)


TREES = [
    ("tails", tree_tails),
    ("middles", tree_middles),
    ("last-chunk", tree_last_chunk),
    ("cohort-boundary", tree_cohort_boundary),
    ("wide", tree_wide),
    ("links", tree_links),
    ("unreadable", tree_unreadable),
    ("mixed", tree_mixed),
]


# ------------------------------------------------------------------ running

def unlock(root: str) -> None:
    for directory, _, names in os.walk(root):
        for name in names:
            try:
                os.chmod(os.path.join(directory, name), 0o644)
            except OSError:
                pass


def groups_of(report: str):
    with open(report) as fh:
        obj = json.load(fh)
    return sorted(tuple(sorted(group)) for group in obj["groups"]), obj


def run_oracle(root: str, report: str):
    subprocess.run([sys.executable, ORACLE, root, "--report-json", report,
                    "--min-size", "1", "--no-cache", "-q"],
                   check=True, capture_output=True)
    return groups_of(report)


def run_candidate(root: str, report: str, flags):
    proc = subprocess.run([CANDIDATE, root, "--report-json", report,
                           "--min-size", "1", "--no-cache"] + flags,
                          capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit("dedup-rs {} exited {}: {}".format(
            flags, proc.returncode, proc.stderr))
    return groups_of(report)


def main() -> int:
    if not os.path.exists(CANDIDATE):
        raise SystemExit("build it first: cargo build --release")
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    failures = 0
    print("tree                  files  groups   flash seeks  spinning seeks"
          "   flash bytes  spinning bytes  verdict")
    for name, build in TREES:
        root = os.path.join(BASE, name)
        os.makedirs(root)
        shape = build(root)
        report = os.path.join(BASE, name + ".json")
        try:
            oracle, oracle_obj = run_oracle(root, report)
            flash, flash_obj = run_candidate(root, report, ["--profile", "flash"])
            spin, spin_obj = run_candidate(root, report, ["--profile", "spinning"])
            plain, _ = run_candidate(root, report, [])
        finally:
            unlock(root)

        same = oracle == flash == spin == plain
        failures += 0 if same else 1
        print("{:<20}  {:>5}  {:>6}  {:>12}  {:>14}  {:>12}  {:>14}  {}".format(
            name, oracle_obj["files_seen"], len(oracle),
            flash_obj["seeks"], spin_obj["seeks"],
            flash_obj["bytes_read"], spin_obj["bytes_read"],
            "same" if same else "DIFFER"))
        if not same:
            print("    shape: " + shape)
            for label, got in (("oracle", oracle), ("flash", flash),
                               ("spinning", spin), ("no flag", plain)):
                print("    {:<9} {}".format(label, got))
        shutil.rmtree(root, ignore_errors=True)

    shutil.rmtree(BASE, ignore_errors=True)
    print()
    if failures:
        print("{} tree(s) differ".format(failures))
        return 1
    print("{} trees, every profile identical to the oracle".format(len(TREES)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
