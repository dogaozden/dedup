#!/usr/bin/env python3
"""Deterministic corpora for the dedup benchmark, and the truth about them.

Each scenario writes a tree and records, as it writes, which inodes hold
identical bytes. The expected duplicate groups fall out of that record rather
than out of a hand-written list, so the answer key cannot drift away from the
corpus it describes — if a builder changes what it lays down, the manifest
changes with it.

Content comes from random.Random(seed).randbytes: incompressible, so nothing
here accidentally measures a compressor, and reproducible, so the same
(seed, scale, GENERATOR_VERSION) always produces the same bytes.

The manifest lives beside the tree, never inside it. The tool under test is
given the tree and must find the answer for itself.
"""
from __future__ import annotations

import hashlib
import json
import os
import random
import unicodedata
from collections import defaultdict

# Bump this whenever a builder changes what it writes. Corpora stamped with an
# older version are regenerated rather than trusted.
GENERATOR_VERSION = 2

# randbytes(n) builds one n*8-bit integer, so it is asked for the corpus in
# pieces. The chunk size is part of the corpus definition: change it and every
# file changes with it.
RAND_CHUNK = 4 << 20

SCALES = {"default": 1.0, "smoke": 0.1}

# dedup's stage-4 constants, mirrored so `bigheader` can place a difference in
# the gap between two sample windows. They are only used to aim a scenario; if
# dedup changes them the scenario stops being interesting rather than wrong.
SAMPLE_BYTES = 1 << 20
SAMPLE_POINTS = 4

# HEAD_BYTES here is 64 KiB because that is what `photos` and `samesize` were
# built against, and it is deliberately NOT dedup's HEAD_BYTES any more —
# dedup now reads its head in two tiers, 4 KiB then out to 64 KiB, and the
# name stayed with the number rather than with the tier. Do not "fix" this to
# match dedup: these two constants decide how many bytes those scenarios
# write, so changing either one silently rewrites their corpora and every
# number ever recorded against them stops being comparable. `prefixtrap` aims
# at both tiers and names them explicitly instead.
HEAD_BYTES = 64 << 10
DEEP_HEAD = 64 << 10             # dedup's tier-2 target
FIRST_PAGE = 4 << 10             # ...and its tier-1 first pass


def blind_tail(size: int) -> int:
    """How many bytes at the end of a file stage 4 never samples.

    Its last window starts at 3 * ((size - 1 MiB) // 3), so integer division
    leaves the final (size - 1 MiB) % 3 bytes — nought, one or two of them —
    outside every window. A pair that differs only in that tail therefore
    survives sampling and can only be told apart by reading, which is the
    most expensive thing this tool ever has to do. Scenarios that want that
    case pick their sizes through here rather than trusting to luck.
    """
    step = (size - SAMPLE_BYTES) // (SAMPLE_POINTS - 1)
    return size - (step * (SAMPLE_POINTS - 1) + SAMPLE_BYTES)


def blind_tail_size(size: int) -> int:
    while blind_tail(size) == 0:
        size += 1
    return size


def rand(rng: random.Random, n: int) -> bytes:
    if n <= RAND_CHUNK:
        return rng.randbytes(n)
    return b"".join(rng.randbytes(min(RAND_CHUNK, n - i))
                    for i in range(0, n, RAND_CHUNK))


def nfc(text: str) -> str:
    """HFS+ stores names decomposed, so a name written as NFC comes back as
    NFD. Both sides of every comparison are folded to NFC first."""
    return unicodedata.normalize("NFC", text)


# ─────────────────────────────────────────────────────────────────────────────
# writing a tree and remembering it
# ─────────────────────────────────────────────────────────────────────────────

class Corpus:
    """Writes files under `tree` and keeps one record per inode.

    Hardlinks are several names for one inode, which is exactly how dedup
    treats them, so names accumulate on the record rather than making a new
    one. Unreadable files are recorded too, but excluded from the expected
    groups: a file the tool cannot open cannot be proved a duplicate.
    """

    def __init__(self, tree: str, min_size: int):
        self.tree = tree
        self.min_size = min_size
        self.records: dict = {}          # inode -> {names, size, sha, readable}
        self.unreadable: list = []       # paths the tool is expected to fail on
        self.total_bytes = 0
        self._dirs = set()

    def _abs(self, rel: str) -> str:
        path = os.path.join(self.tree, rel)
        parent = os.path.dirname(path)
        if parent not in self._dirs:
            os.makedirs(parent, exist_ok=True)
            self._dirs.add(parent)
        return path

    def mkdir(self, rel: str) -> str:
        path = os.path.join(self.tree, rel)
        os.makedirs(path, exist_ok=True)
        self._dirs.add(path)
        return path

    def write(self, rel: str, *parts: bytes, readable: bool = True) -> str:
        """Write one file from a sequence of byte blocks.

        Taking parts rather than one buffer lets two 56 MB files share a 55 MB
        prefix object instead of holding two copies of it in memory.
        """
        path = self._abs(rel)
        h = hashlib.sha256()
        size = 0
        with open(path, "wb") as fh:
            for part in parts:
                fh.write(part)
                h.update(part)
                size += len(part)
        if not readable:
            os.chmod(path, 0o000)
            self.unreadable.append(rel)
        self.records[os.stat(path).st_ino] = {
            "names": [rel], "size": size, "sha": h.hexdigest(), "readable": readable,
        }
        self.total_bytes += size
        return path

    def link(self, existing_rel: str, rel: str) -> None:
        """Another name for a file already in the tree."""
        path = self._abs(rel)
        os.link(os.path.join(self.tree, existing_rel), path)
        self.records[os.stat(path).st_ino]["names"].append(rel)

    def link_outside(self, existing_rel: str, abs_path: str) -> None:
        """A name for a tree file that the scan will never see.

        The inode's link count goes up but the name does not, which is dedup's
        definition of a file it must not offer to delete.
        """
        os.makedirs(os.path.dirname(abs_path), exist_ok=True)
        os.link(os.path.join(self.tree, existing_rel), abs_path)

    def symlink(self, rel: str, target: str) -> None:
        os.symlink(target, self._abs(rel))

    def groups(self) -> list:
        """Every set of readable inodes at or above min_size sharing content."""
        by_content = defaultdict(list)
        for ino, rec in self.records.items():
            if not rec["readable"] or rec["size"] < self.min_size:
                continue
            by_content[(rec["size"], rec["sha"])].append(rec)

        out = []
        for (size, _sha), recs in by_content.items():
            if len(recs) < 2:
                continue
            members = []
            for rec in recs:
                nlink = os.stat(os.path.join(self.tree, rec["names"][0])).st_nlink
                members.append({
                    "names": sorted(rec["names"]),
                    # dedup only frees space when it can delete every name the
                    # inode has, so a link reaching outside the tree pins it.
                    "deletable": len(rec["names"]) >= nlink,
                })
            members.sort(key=lambda m: m["names"])
            out.append({"size": size, "sha": _sha, "members": members})
        out.sort(key=lambda g: (-g["size"], g["members"][0]["names"][0]))
        return out


# ─────────────────────────────────────────────────────────────────────────────
# scenarios
# ─────────────────────────────────────────────────────────────────────────────

def _depth(rel: str) -> int:
    return 0 if not rel else rel.count("/") + 1


def _dir_tree(rng: random.Random, count: int, max_depth: int) -> list:
    dirs = [""]
    for i in range(1, count):
        parent = dirs[rng.randrange(len(dirs))]
        if _depth(parent) + 1 > max_depth:
            parent = ""
        dirs.append("{}/d{:04d}".format(parent, i) if parent else "d{:04d}".format(i))
    return dirs


def _namer(rng: random.Random, dirs: list, stem: str = "f"):
    counter = [0]

    def name() -> str:
        counter[0] += 1
        d = dirs[rng.randrange(len(dirs))]
        leaf = "{}{:06d}.bin".format(stem, counter[0])
        return "{}/{}".format(d, leaf) if d else leaf

    return name


def _typical(c: Corpus, rng: random.Random, f: float) -> None:
    """A photo-library-shaped tree: lots of small files, a long thin tail of
    big ones, a tenth of them duplicated."""
    n = max(300, round(16000 * f))
    dirs = _dir_tree(rng, max(20, n // 40), 7)
    name = _namer(rng, dirs)

    def size() -> int:
        if rng.random() < 0.003:
            return rng.randint(256 << 10, 2 << 20)
        return 4096 + int(rng.lognormvariate(8.699, 1.1))   # ln(6000)

    written = 0
    for _ in range(max(3, round(0.04 * n))):
        data = rand(rng, size())
        for _ in range(2 if rng.random() < 0.6 else 3):
            c.write(name(), data)
            written += 1
    for _ in range(n - written):
        c.write(name(), rand(rng, size()))


def _photos(c: Corpus, rng: random.Random, f: float) -> None:
    """Few files, all large. Most of the work is stage 5 reading them."""
    total = max(12, round(100 * f))
    n_pairs = max(2, round(17 * f))
    n_header = max(1, round(4 * f))

    def size() -> int:
        return rng.randint(2 << 20, 6 << 20)

    # one group of five, because a wide group of large files behaves nothing
    # like a pair
    data = rand(rng, size())
    for i in range(5):
        c.write("album/set/five-{}.jpg".format(i), data)

    for i in range(n_pairs):
        data = rand(rng, size())
        c.write("album/pair{:03d}/a.jpg".format(i), data)
        c.write("album/copies/pair{:03d}.jpg".format(i), data)

    # Same size, different first 64 KiB: the stage-3 head hash exists for
    # exactly this, and it must not have to read past it.
    for i in range(n_header):
        tail = rand(rng, size() - HEAD_BYTES)
        c.write("album/variants/{:03d}-a.jpg".format(i), rand(rng, HEAD_BYTES), tail)
        c.write("album/variants/{:03d}-b.jpg".format(i), rand(rng, HEAD_BYTES), tail)

    used = 5 + 2 * n_pairs + 2 * n_header
    for i in range(max(2, total - used)):
        c.write("album/loose/{:03d}.jpg".format(i), rand(rng, size()))


def _bigheader(c: Corpus, rng: random.Random, f: float) -> None:
    """Four pairs of large files, each aimed at a different stage.

    Sizes stay distinct so every pair gets its own size bucket, and stay above
    dedup's 8 MB sampling threshold even at smoke scale, or stage 4 would not
    run at all and the scenario would test nothing.
    """
    sizes = []
    for i, mb in enumerate((56, 57, 52, 60)):
        size = max(12 + i, round(mb * f)) << 20
        while size in sizes:            # halving the scale can collapse two
            size += 1 << 20             # bases onto one size; keep them apart
        sizes.append(size)
    sizes[3] = blind_tail_size(sizes[3])

    # a real duplicate: the only pair here that stage 5 has to read to the end
    body = rand(rng, sizes[0])
    c.write("big/true-a.dat", body)
    c.write("big/true-b.dat", body)
    del body

    # One byte apart, in the gap between stage 4's first and second sample
    # windows. Every window agrees, so sampling settles nothing — but the
    # difference is early, so stage 5 stops almost at once. This is the case
    # that says whether the early exit works on files this size.
    step = (sizes[1] - SAMPLE_BYTES) // (SAMPLE_POINTS - 1)
    at = (SAMPLE_BYTES + step) // 2
    head = rand(rng, at)
    tail = rand(rng, sizes[1] - at - 1)
    c.write("big/blind-a.dat", head, b"\x00", tail)
    c.write("big/blind-b.dat", head, b"\xff", tail)
    del head, tail

    # A long shared prefix and then nothing in common: stage 4's later windows
    # land in the divergent part and settle it without a full read.
    prefix = rand(rng, (sizes[2] * 3) // 4)
    c.write("big/prefix-a.dat", prefix, rand(rng, sizes[2] - len(prefix)))
    c.write("big/prefix-b.dat", prefix, rand(rng, sizes[2] - len(prefix)))
    del prefix

    # Identical except the very last byte, at a size whose final bytes fall
    # outside every sample window. Nothing before stage 5 can separate these,
    # and stage 5 has to read both of them to the end to do it: the most work
    # a pair of files can cost.
    body = rand(rng, sizes[3] - 1)
    c.write("big/lastbyte-a.dat", body, b"\x00")
    c.write("big/lastbyte-b.dat", body, b"\xff")


def _needle(c: Corpus, rng: random.Random, f: float) -> None:
    """Almost nothing to find, spread over a great many files. This is the
    walk and the startup cost, and very little else."""
    n = max(2000, round(100000 * f))
    pairs = max(10, round(50 * f))
    dirs = _dir_tree(rng, max(50, n // 200), 4)
    name = _namer(rng, dirs)

    for _ in range(pairs):
        data = rand(rng, rng.randint(1024, 4096))
        c.write(name(), data)
        c.write(name(), data)
    for _ in range(n - 2 * pairs):
        c.write(name(), rand(rng, rng.randint(1024, 4096)))


SAMESIZE_BYTES = 256 << 10


def _samesize(c: Corpus, rng: random.Random, f: float) -> None:
    """Every file the same length with the same first 64 KiB, so stages 2 and
    3 separate nothing and the whole population arrives at stage 5 in one
    bucket."""
    n = max(140, round(1400 * f))
    n_groups = max(4, round(40 * f))
    n_lastbyte = max(4, round(10 * f))
    head = rand(rng, HEAD_BYTES)
    rest = SAMESIZE_BYTES - HEAD_BYTES

    written = 0
    for i in range(n_groups):
        body = rand(rng, rest)
        for j in range(2 if rng.random() < 0.7 else 3):
            c.write("same/g{:03d}-{}.bin".format(i, j), head, body)
            written += 1

    # A twin that only parts company on its final byte: nothing but reading
    # every byte can tell these apart.
    for i in range(n_lastbyte):
        body = rand(rng, rest - 1)
        c.write("same/last{:03d}-a.bin".format(i), head, body, b"\x00")
        c.write("same/last{:03d}-b.bin".format(i), head, body, b"\xff")
        written += 2

    for i in range(max(0, n - written)):
        c.write("same/u{:05d}.bin".format(i), head, rand(rng, rest))

    # The same trick at a size big enough to reach stage 4, chosen so its
    # final bytes land outside every sample window and sampling cannot help.
    big = blind_tail_size(max(12 << 20, round((32 << 20) * f)))
    body = rand(rng, big - 1)
    c.write("same/bigtwin-a.bin", body, b"\x00")
    c.write("same/bigtwin-b.bin", body, b"\xff")


PREFIXTRAP_BYTES = 2 << 20

# One band under dedup's first page, three between it and the deep head. The
# longest stops well short of DEEP_HEAD — 48 KiB against 64 — so the band is
# separated by a comfortable margin rather than by the last kilobyte of it.
PREFIXTRAP_PREFIXES = (2 << 10, 8 << 10, 24 << 10, 48 << 10)


def _prefixtrap(c: Corpus, rng: random.Random, f: float) -> None:
    """Same-size files that share a short prefix and then nothing at all.

    This scenario exists to punish one specific idea: reading less of the
    head. Every other scenario makes a shorter head look free — it reads
    fewer bytes in stage 3 and separates just as much — because nowhere else
    do same-size files agree for a few kilobytes and then part company. Real
    trees are full of that shape: camera raws from one body share a header,
    and so do exports from one encoder.

    Four bands, one shared prefix each, divergence on the byte after it, all
    at one size so stage 2 hands the whole population to stage 3 together.

    Stage 3 reads its head in two tiers, and there is a band aimed at each:

      2 KiB   under FIRST_PAGE, so tier 1 settles this band on its own for
              one page a file. This band is a marker, not a trap: shrinking
              tier 1 below 2 KiB drops it through to tier 2 and gives the
              saving back, which shows in the table as bytes rising, but it
              cannot trip the veto. Losing tier-1 separation costs at most
              the whole deep head, and paying the whole head is exactly what
              the pinned baseline already did — so the number returns to
              1.00x and stops there. Measured: tier 1 at 1 KiB reads 18.0 MB
              against a baseline of 18.0 MB.
      8, 24,
      48 KiB  over FIRST_PAGE and under DEEP_HEAD, so tier 1 leaves them
              crowded and tier 2 separates them. This is the trap. Pull
              DEEP_HEAD in below a band's prefix and that band is not
              separated at all: it arrives at stage 5 as a single bucket and
              is read far past the head, which is far past anything the
              baseline paid. Measured at default scale: DEEP_HEAD at 4 KiB
              reads 147.2 MB against a baseline of 44.2 MB, a 3.33x
              regression that voids the score.

    The scenario says nothing about making the head *longer* — that is still
    a live idea, and this corpus is indifferent to it.

    The files sit below SAMPLE_SPAN on purpose. Above it stage 4 would sample
    the divergent part and separate a band anyway, which would let a
    shortened head off with a warning instead of a bill.
    """
    n = max(24, round(180 * f))
    n_pairs = max(4, round(8 * f))
    bands = len(PREFIXTRAP_PREFIXES)

    for band, shared in enumerate(PREFIXTRAP_PREFIXES):
        stem = "raw/shared{}k".format(shared >> 10)
        prefix = rand(rng, shared)
        rest = PREFIXTRAP_BYTES - shared
        for i in range(n // bands + (1 if band < n % bands else 0)):
            c.write("{}/u{:04d}.raw".format(stem, i), prefix, rand(rng, rest))
        # A real duplicate pair in every band, so the scenario has groups to
        # prove and stage 5 has honest work to do however the head is read.
        for i in range(n_pairs // bands + (1 if band < n_pairs % bands else 0)):
            body = rand(rng, rest)
            c.write("{}/dup{:03d}-a.raw".format(stem, i), prefix, body)
            c.write("{}/dup{:03d}-b.raw".format(stem, i), prefix, body)
        del prefix


def _widegroup(c: Corpus, rng: random.Random, f: float) -> None:
    """One enormous group, and the hardlink cases.

    dedup collapses every name of an inode onto a single record before it
    counts a group, so a hardlink cluster is one member wearing several names
    — not several duplicates. Three shapes are laid down here because they
    have three different answers.
    """
    wide = max(50, round(500 * f))
    data = rand(rng, SAMESIZE_BYTES)
    for i in range(wide):
        c.write("wide/copy{:04d}.bin".format(i), data)
    del data

    for i in range(5):
        data = rand(rng, 64 << 10)
        for j in range(3):
            c.write("trio/g{}-{}.bin".format(i, j), data)

    # Four names for one inode plus a separate file holding the same bytes:
    # two members, both safe to delete down to one.
    linked = rand(rng, 100 << 10)
    c.write("links/cluster/a.bin", linked)
    for n in ("b.bin", "c.bin", "d.bin"):
        c.link("links/cluster/a.bin", "links/cluster/" + n)
    c.write("links/twin-of-cluster.bin", linked)

    # Two names inside the tree and one outside it. Deleting what the scan can
    # see would free nothing, so the member is reported undeletable.
    pinned = rand(rng, 101 << 10)
    c.write("links/pinned/a.bin", pinned)
    c.link("links/pinned/a.bin", "links/pinned/b.bin")
    c.link_outside("links/pinned/a.bin",
                   os.path.join(os.path.dirname(c.tree), "outside", "pinned.bin"))
    c.write("links/twin-of-pinned.bin", pinned)

    # Four names, one inode, nothing else like it: hardlinks on their own are
    # not duplicates and this must not be reported as a group.
    c.write("links/solo/a.bin", rand(rng, 102 << 10))
    for n in ("b.bin", "c.bin", "d.bin"):
        c.link("links/solo/a.bin", "links/solo/" + n)


def _adversarial(c: Corpus, rng: random.Random, f: float) -> None:
    """Everything a filesystem can do that a scanner might not survive."""
    real = rand(rng, 200 << 10)
    c.write("dup/one.bin", real)
    c.write("dup/two.bin", real)

    # Symlinks are never followed, so none of these may appear in a group and
    # none of them may cost a read.
    c.symlink("link-to-dup.bin", os.path.join(c.tree, "dup", "one.bin"))
    c.symlink("broken-link", os.path.join(c.tree, "does-not-exist"))
    c.mkdir("loop")
    c.symlink("loop/up", c.tree)          # a directory symlink back to the root

    # Unreadable, and sharing a size with a readable file so that it survives
    # stage 2 and actually reaches the point where it fails to open. Given a
    # unique size it would be dropped before anything tried to read it, and
    # would never reach stats.unreadable at all.
    c.write("locked.bin", rand(rng, 300 << 10), readable=False)
    c.write("size-twin-of-locked.bin", rand(rng, 300 << 10))

    # Names that have to survive a round trip through JSON, the shell and an
    # HFS+ volume that stores them decomposed.
    odd = rand(rng, 150 << 10)
    c.write("odd names/a file with spaces.bin", odd)
    c.write("odd names/emoji \U0001F419 name.bin", odd)
    c.write("odd names/quote'and\"double.bin", rand(rng, 150 << 10))
    c.write("odd names/ünïcode-ñame.bin", rand(rng, 150 << 10))
    c.write("odd names/new\nline.bin", rand(rng, 150 << 10))

    # Empty files are below any min_size of 1 and must be dropped, not grouped
    # with each other on the strength of having the same (zero) contents.
    for i in range(3):
        c.write("empty/zero{}".format(i), b"")

    # Exactly at the floor: min_size is inclusive, so these two are a group.
    c.write("boundary/tiny-a", b"A")
    c.write("boundary/tiny-b", b"A")
    c.write("boundary/tiny-c", b"B")

    filler = max(4, round(40 * f))
    for i in range(filler):
        c.write("filler/{:03d}.bin".format(i), rand(rng, 200 << 10))
    pair = rand(rng, 200 << 10)
    c.write("filler/dup-a.bin", pair)
    c.write("filler/dup-b.bin", pair)


def _deletion(c: Corpus, rng: random.Random, f: float) -> None:
    """A small flat tree for the real command line to eat.

    Flat and same-length names on purpose: `--auto shortest` keeps the file
    with the shortest absolute path and breaks ties alphabetically, so with
    every path the same length the survivor is decided by the name alone and
    the expectation is exact. Unique basenames keep the check on the Trash
    unambiguous.
    """
    total = max(30, round(240 * f))
    n_groups = max(6, round(40 * f))
    size = 256 << 10

    idx = 0
    for _ in range(n_groups):
        data = rand(rng, size)
        for _ in range(2 if rng.random() < 0.7 else 3):
            c.write("f{:04d}.bin".format(idx), data)
            idx += 1
    while idx < total:
        c.write("f{:04d}.bin".format(idx), rand(rng, size))
        idx += 1


SCENARIOS = {
    #                 min_size,    timed, builder
    "typical":     (1,             True,  _typical),
    "photos":      (1 << 20,       True,  _photos),
    "bigheader":   (10 << 20,      True,  _bigheader),
    "needle":      (1,             True,  _needle),
    "samesize":    (1,             True,  _samesize),
    "widegroup":   (1,             True,  _widegroup),
    "prefixtrap":  (1 << 20,       True,  _prefixtrap),
    "adversarial": (1,             False, _adversarial),
    "deletion":    (1,             False, _deletion),
    # The re-scan scenario has no shape of its own on purpose: it is the
    # `photos` corpus, scanned twice and then interfered with. Its own name
    # only buys it its own directory, so mutating it cannot disturb the
    # scored run of the same profile.
    "rescan":      (1 << 20,       False, _photos),
}

# Rough on-disk cost, used only to refuse to start a corpus that will not fit.
# The floor is what the smoke scale still has to write for a scenario to keep
# its point.
ESTIMATE = {
    "typical":     (8e6, 330e6),
    "photos":      (60e6, 430e6),
    "bigheader":   (115e6, 470e6),
    "needle":      (12e6, 430e6),
    "samesize":    (70e6, 440e6),
    "widegroup":   (20e6, 145e6),
    "prefixtrap":  (68e6, 412e6),
    "adversarial": (14e6, 14e6),
    "deletion":    (10e6, 70e6),
    "rescan":      (60e6, 430e6),
}


def estimated_bytes(scenario: str, factor: float) -> int:
    floor, full = ESTIMATE[scenario]
    return int(max(floor, full * factor))


# ─────────────────────────────────────────────────────────────────────────────
# building and reading back
# ─────────────────────────────────────────────────────────────────────────────

def corpus_dir(root: str, scenario: str, scale: str, seed: int) -> str:
    return os.path.join(root, "corpora", "{}-{}-seed{}".format(scenario, scale, seed))


def build(root: str, scenario: str, seed: int, scale: str, factor: float) -> dict:
    """Lay down one corpus and write its manifest beside the tree."""
    import shutil

    min_size, timed, builder = SCENARIOS[scenario]
    base = corpus_dir(root, scenario, scale, seed)
    shutil.rmtree(base, ignore_errors=True)
    tree = os.path.join(base, "tree")
    os.makedirs(tree)

    c = Corpus(tree, min_size)
    builder(c, random.Random(seed), factor)

    manifest = {
        "scenario": scenario,
        "seed": seed,
        "scale": scale,
        "factor": factor,
        "min_size": min_size,
        "timed": timed,
        "generator_version": GENERATOR_VERSION,
        "file_count": len(c.records),
        "total_bytes": c.total_bytes,
        "expect_unreadable": sorted(c.unreadable),
        "groups": c.groups(),
    }
    with open(os.path.join(base, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=1)
    return manifest


def load(root: str, scenario: str, scale: str, seed: int):
    """The manifest for an existing corpus, or None if it is missing, stale or
    unreadable. Anything doubtful is regenerated rather than trusted."""
    base = corpus_dir(root, scenario, scale, seed)
    try:
        with open(os.path.join(base, "manifest.json")) as fh:
            m = json.load(fh)
    except (OSError, ValueError):
        return None
    if not os.path.isdir(os.path.join(base, "tree")):
        return None
    if m.get("generator_version") != GENERATOR_VERSION:
        return None
    if (m.get("scenario"), m.get("seed"), m.get("scale")) != (scenario, seed, scale):
        return None
    return m


# ─────────────────────────────────────────────────────────────────────────────
# comparing an answer to the key
# ─────────────────────────────────────────────────────────────────────────────

def canonical(groups: list) -> frozenset:
    """Reduce a manifest's groups or a scan's groups to the same shape.

    A member is the set of names one inode wears inside the tree, because that
    is what dedup reports: hardlinks travel together on one record. Whether
    the record can actually be deleted rides along with it, so a link reaching
    outside the tree stays distinguishable from one that does not.
    """
    out = set()
    for g in groups:
        members = frozenset(
            (frozenset(nfc(n) for n in m["names"]), bool(m["deletable"]))
            for m in g["members"]
        )
        out.add((int(g["size"]), members))
    return frozenset(out)


def describe(missing: set, extra: set, limit: int = 3) -> str:
    """A short, readable account of how two answers differ."""
    def show(groups):
        lines = []
        for size, members in sorted(groups, key=lambda g: -g[0])[:limit]:
            names = sorted(sorted(n)[0] for n, _ in members)
            lines.append("{}B [{}]".format(size, ", ".join(names[:4])))
        if len(groups) > limit:
            lines.append("... and {} more".format(len(groups) - limit))
        return "; ".join(lines)

    parts = []
    if missing:
        parts.append("{} expected group(s) not found: {}".format(len(missing), show(missing)))
    if extra:
        parts.append("{} unexpected group(s): {}".format(len(extra), show(extra)))
    return " | ".join(parts) or "no difference"
