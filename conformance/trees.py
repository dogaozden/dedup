#!/usr/bin/env python3
"""The trees the referee judges on, and the answer key for each.

Every tree is written from a seeded random source, so the same (seed,
GENERATOR_VERSION) lays down the same bytes on any machine. Each builder
records, as it writes, which inode holds which bytes; the expected duplicate
groups fall out of that record rather than out of a hand-written list, so the
answer key cannot drift away from the tree it describes.

These are deliberately independent of bench/corpora. The benchmark's trees are
shaped to be timed and grow to gigabytes; these are shaped to be *argued with*
— every construction here exists because some plausible implementation gets it
wrong — and the whole set fits in a few hundred megabytes so a gate run costs
seconds.

The manifest lives beside the tree, never inside it: a scan that reads the
answer key is not scanning.
"""
from __future__ import annotations

import fnmatch
import hashlib
import json
import os
import random
import shutil
import stat

# Bump whenever a builder changes what it writes. Trees stamped with an older
# version are regenerated rather than trusted.
GENERATOR_VERSION = 1

# randbytes(n) builds one n*8-bit integer, so large files are asked for in
# pieces. The chunk size is part of the tree definition: change it and every
# file changes with it.
RAND_CHUNK = 4 << 20

# The oracle's stage boundaries, mirrored here only so the builders can aim at
# them. dedup.py owns the real values; these exist to place a divergence on the
# far side of a threshold, never to assert what the threshold is.
HEAD_BYTES = 4 * 1024
DEEP_HEAD = 64 * 1024
TAIL_BYTES = 4 * 1024
SAMPLE_SPAN = 8 * 1024 * 1024


def rand(rng: random.Random, n: int) -> bytes:
    if n <= RAND_CHUNK:
        return rng.randbytes(n)
    return b"".join(rng.randbytes(min(RAND_CHUNK, n - i))
                    for i in range(0, n, RAND_CHUNK))


def canonical(names) -> str:
    """The one name that stands for a record in a report.

    A hardlinked inode wears several names and the walk meets them in whatever
    order the directory hands them over, so the first name seen is not stable
    across runs. Shortest-then-lexicographic is, and it is the same rule the
    oracle uses to pick a keeper, so the name in the report is the name that
    survives a deletion.
    """
    return min(names, key=lambda n: (len(n), n))


class Tree:
    """A tree under construction, and the record of what went into it."""

    def __init__(self, root: str, min_size: int):
        self.root = root
        self.min_size = min_size
        self.records: dict = {}          # inode -> {names, size, sha, readable}
        self.unreadable: set = set()
        self.total_bytes = 0

    def _abs(self, rel: str) -> str:
        return os.path.join(self.root, rel)

    def mkdir(self, rel: str) -> None:
        os.makedirs(self._abs(rel), exist_ok=True)

    def write(self, rel: str, *chunks: bytes, readable: bool = True) -> str:
        path = self._abs(rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        h = hashlib.sha256()
        size = 0
        with open(path, "wb") as fh:
            for chunk in chunks:
                fh.write(chunk)
                h.update(chunk)
                size += len(chunk)
        if not readable:
            os.chmod(path, 0o000)
            self.unreadable.add(rel)
        self.total_bytes += size
        self.records[os.stat(path).st_ino] = {
            "names": [rel], "size": size, "sha": h.hexdigest(),
            "readable": readable,
        }
        return rel

    def link(self, existing_rel: str, rel: str) -> None:
        """Another name in the tree for a file already written."""
        path = self._abs(rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        os.link(self._abs(existing_rel), path)
        self.records[os.stat(path).st_ino]["names"].append(rel)

    def link_outside(self, existing_rel: str, abs_path: str) -> None:
        """A name for a tree file the scan will never see.

        The inode's link count goes up but the name does not, which is the
        oracle's definition of a file it must not offer to delete.
        """
        os.makedirs(os.path.dirname(abs_path), exist_ok=True)
        os.link(self._abs(existing_rel), abs_path)

    def symlink(self, rel: str, target: str) -> None:
        path = self._abs(rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        os.symlink(target, path)

    def entries(self) -> list:
        """Every record, as the manifest stores it."""
        out = []
        for rec in self.records.values():
            names = sorted(rec["names"])
            out.append({
                "names": names,
                "size": rec["size"],
                "sha": rec["sha"],
                "readable": rec["readable"],
                # Deleting every name the scan can see frees the inode only if
                # the scan can see them all.
                "deletable": len(names) >= os.stat(
                    self._abs(names[0])).st_nlink,
            })
        out.sort(key=lambda e: canonical(e["names"]))
        return out


# ── builders ─────────────────────────────────────────────────────────────────
#
# Signature is the same throughout: (tree, rng) -> None.


def _mixed(t: Tree, rng: random.Random) -> None:
    """An ordinary tree. Nothing here is a trap; it is the control."""
    for i in range(12):
        data = rand(rng, rng.randint(200 << 10, 900 << 10))
        t.write("photos/{:02d}/original.jpg".format(i), data)
        t.write("photos/backup/copy-{:02d}.jpg".format(i), data)

    trio = rand(rng, 512 << 10)
    for name in ("docs/report.bin", "docs/old/report.bin", "archive/report.bin"):
        t.write(name, trio)

    # Extensions exist so the --ext / --skip-ext / -x cases have something to
    # bite on: two of these are duplicates of each other and of nothing else.
    scratch = rand(rng, 300 << 10)
    t.write("build/a.tmp", scratch)
    t.write("build/b.tmp", scratch)
    t.write("notes/keep.txt", rand(rng, 90 << 10))
    t.write("notes/also.txt", rand(rng, 90 << 10))

    for i in range(20):
        t.write("loose/{:03d}.bin".format(i), rand(rng, rng.randint(64 << 10, 400 << 10)))


def _smallzoo(t: Tree, rng: random.Random) -> None:
    """Files small enough that the oracle reads them whole in one open and
    decides on the retained bytes. Everything that can go wrong in that path
    is here: sizes either side of both thresholds, twins parting on their last
    byte, and same-size strangers that must not be merged."""
    for i, size in enumerate((1, 2, 100, 1 << 10, 7 << 10, 40 << 10, 63 << 10)):
        data = rand(rng, size)
        t.write("pairs/{:02d}-a.bin".format(i), data)
        t.write("pairs/{:02d}-b.bin".format(i), data)

    # A page and a block, each side of the boundary and exactly on it. If a
    # reader is off by one about how much a small file needs, one of these six
    # pairs stops matching.
    for size in (4095, 4096, 4097, 65535, 65536, 65537):
        data = rand(rng, size)
        t.write("boundary/{}-a.bin".format(size), data)
        t.write("boundary/{}-b.bin".format(size), data)
        # A stranger of exactly the same length, so the size bucket cannot
        # settle it and something has to look at the bytes.
        t.write("boundary/{}-x.bin".format(size), rand(rng, size))

    # Same length, same everything but the final byte: nothing short of reading
    # to the end tells these apart.
    for i, size in enumerate((17, 4096, 32 << 10, 64 << 10)):
        body = rand(rng, size - 1)
        t.write("lastbyte/{:02d}-a.bin".format(i), body, b"\x00")
        t.write("lastbyte/{:02d}-b.bin".format(i), body, b"\xff")

    # Zero length. Below any min_size of 1, so they must be dropped — not
    # grouped with one another on the strength of having the same no bytes.
    for i in range(4):
        t.write("empty/zero{}".format(i), b"")

    for i in range(60):
        t.write("many/{:03d}.bin".format(i), rand(rng, rng.randint(1, 60 << 10)))


def _prefix(t: Tree, rng: random.Random) -> None:
    """Same-size files that agree for a long way and then stop.

    This tree exists to punish one specific idea: deciding on a prefix. Each
    family shares more of its front than the last, and one family shares both
    ends and differs only in the middle, which is the shape a head-and-tail
    probe cannot see at all.
    """
    size = 256 << 10

    # Agree for one page, part company immediately after.
    head = rand(rng, HEAD_BYTES)
    for i in range(6):
        t.write("head4k/{:02d}-a.bin".format(i), head, rand(rng, size - HEAD_BYTES))
        t.write("head4k/{:02d}-b.bin".format(i), head, rand(rng, size - HEAD_BYTES))

    # Agree for the whole deep head.
    deep = rand(rng, DEEP_HEAD)
    for i in range(6):
        t.write("head64k/{:02d}-a.bin".format(i), deep, rand(rng, size - DEEP_HEAD))
        t.write("head64k/{:02d}-b.bin".format(i), deep, rand(rng, size - DEEP_HEAD))

    # Agree at both ends, differ in the middle. A probe that reads the front
    # and the back and nothing else calls these identical.
    for i in range(6):
        front = rand(rng, DEEP_HEAD)
        back = rand(rng, TAIL_BYTES)
        middle = size - DEEP_HEAD - TAIL_BYTES
        t.write("bothends/{:02d}-a.bin".format(i), front, rand(rng, middle), back)
        t.write("bothends/{:02d}-b.bin".format(i), front, rand(rng, middle), back)

    # Genuine duplicates wearing the same shared head, so a reader that gives
    # up on the family altogether is caught too.
    for i in range(4):
        data = head + rand(rng, size - HEAD_BYTES)
        t.write("real/{:02d}-a.bin".format(i), data)
        t.write("real/{:02d}-b.bin".format(i), data)
        t.write("real/{:02d}-c.bin".format(i), data)


def _bighead(t: Tree, rng: random.Random) -> None:
    """Files past the sampling threshold, where a digest of scattered windows
    decides who reaches the byte comparison. Every pair here agrees on both
    ends, so the ends probe forwards all of them, and the differences are
    placed where sampling is least likely to be looking."""
    size = 12 << 20
    assert size > SAMPLE_SPAN

    data = rand(rng, size)
    t.write("big/twin-a.iso", data)
    t.write("big/twin-b.iso", data)

    trio = rand(rng, size)
    for name in ("big/trio-a.iso", "big/set/trio-b.iso", "big/set/deep/trio-c.iso"):
        t.write(name, trio)
    del trio

    # One byte apart, in the middle. Sampling may or may not land on it; the
    # byte comparison has no choice but to find it.
    body = rand(rng, size)
    mid = size // 2
    t.write("big/mid-a.iso", body)
    t.write("big/mid-b.iso", body[:mid], bytes([body[mid] ^ 0xFF]), body[mid + 1:])
    del body

    # One byte apart, at the very end, inside the tail page both files share
    # right up to it.
    body = rand(rng, size - 1)
    t.write("big/end-a.iso", body, b"\x00")
    t.write("big/end-b.iso", body, b"\xff")
    del body

    for i in range(2):
        t.write("big/loose-{}.iso".format(i), rand(rng, size))


def _links(t: Tree, rng: random.Random) -> None:
    """Hardlinks, which are one file wearing several names.

    Three shapes, because they have three different answers: a cluster with a
    separate twin, a cluster with no twin at all, and a cluster with a name
    outside the tree.
    """
    linked = rand(rng, 220 << 10)
    t.write("cluster/a.bin", linked)
    for name in ("b.bin", "c.bin", "d.bin"):
        t.link("cluster/a.bin", "cluster/" + name)
    t.write("cluster-twin.bin", linked)

    # Four names, one inode, nothing else like it. Hardlinks on their own are
    # not duplicates and this must not be reported as a group.
    t.write("solo/a.bin", rand(rng, 221 << 10))
    for name in ("b.bin", "c.bin", "d.bin"):
        t.link("solo/a.bin", "solo/" + name)

    # Two names inside the tree and one outside it. Deleting what the scan can
    # see would free nothing, so the member is reported undeletable.
    pinned = rand(rng, 222 << 10)
    t.write("pinned/a.bin", pinned)
    t.link("pinned/a.bin", "pinned/b.bin")
    t.link_outside("pinned/a.bin",
                   os.path.join(os.path.dirname(t.root), "outside", "pinned.bin"))
    t.write("pinned-twin.bin", pinned)

    for i in range(6):
        t.write("filler/{}.bin".format(i), rand(rng, 100 << 10))


def _adversarial(t: Tree, rng: random.Random) -> None:
    """Everything a filesystem can do that a scanner might not survive."""
    real = rand(rng, 180 << 10)
    t.write("dup/one.bin", real)
    t.write("dup/two.bin", real)

    # Symlinks are never followed, so none of these may appear in a group and
    # none of them may cost a read.
    t.symlink("link-to-dup.bin", os.path.join(t.root, "dup", "one.bin"))
    t.symlink("broken-link", os.path.join(t.root, "does-not-exist"))
    t.mkdir("loop")
    t.symlink("loop/up", t.root)          # a directory symlink back to the root

    # Unreadable, and sharing a size with a readable file so it survives the
    # size bucket and actually reaches the point where it fails to open. Given
    # a unique size it would be dropped before anything tried to read it.
    t.write("locked.bin", rand(rng, 260 << 10), readable=False)
    t.write("size-twin-of-locked.bin", rand(rng, 260 << 10))

    # Two unreadable files holding identical bytes. Neither can be proven, so
    # they are not a group however tempting their sizes look.
    twin = rand(rng, 261 << 10)
    t.write("locked-pair/a.bin", twin, readable=False)
    t.write("locked-pair/b.bin", twin, readable=False)

    # Names that have to survive a round trip through JSON, a shell, and a
    # volume that stores them decomposed.
    odd = rand(rng, 140 << 10)
    t.write("odd names/a file with spaces.bin", odd)
    t.write("odd names/emoji \U0001F419 name.bin", odd)
    t.write("odd names/quote'and\"double.bin", rand(rng, 140 << 10))
    t.write("odd names/ünïcode-ñame.bin", rand(rng, 140 << 10))
    t.write("odd names/new\nline.bin", rand(rng, 140 << 10))

    # A duplicate pair whose names are only awkward, so the awkwardness has to
    # survive being reported *and* being grouped.
    weird = rand(rng, 141 << 10)
    t.write("odd names/tab\there-a.bin", weird)
    t.write("odd names/\U0001F419\U0001F419-b.bin", weird)

    for i in range(3):
        t.write("empty/zero{}".format(i), b"")

    # Exactly at the floor: min_size is inclusive, so these two are a group.
    t.write("boundary/tiny-a", b"A")
    t.write("boundary/tiny-b", b"A")
    t.write("boundary/tiny-c", b"B")

    for i in range(8):
        t.write("filler/{:02d}.bin".format(i), rand(rng, 150 << 10))


def _deletion(t: Tree, rng: random.Random) -> None:
    """Shaped for the deletion gate: every group has an unambiguous keeper.

    Names are chosen so the shortest path is strictly shortest — no ties — and
    the survivor is therefore predictable without asking the tool. The pinned
    hardlink is here because the one thing worse than missing a duplicate is
    deleting a name that frees nothing.
    """
    for i in range(8):
        data = rand(rng, rng.randint(120 << 10, 400 << 10))
        t.write("g{:02d}.bin".format(i), data)                  # the keeper
        t.write("copies/g{:02d}-copy1.bin".format(i), data)
        if i % 2 == 0:
            t.write("copies/deeper/g{:02d}-copy2.bin".format(i), data)

    # Two names in the tree, one outside it: undeletable, so both names must
    # still be there afterwards even though the twin is a real duplicate.
    pinned = rand(rng, 310 << 10)
    t.write("pinned/aa.bin", pinned)
    t.link("pinned/aa.bin", "pinned/bb.bin")
    t.link_outside("pinned/aa.bin",
                   os.path.join(os.path.dirname(t.root), "outside", "pinned.bin"))
    t.write("p.bin", pinned)

    # Nothing about these is duplicated; every one must survive untouched.
    for i in range(10):
        t.write("keep/{:02d}.bin".format(i), rand(rng, rng.randint(90 << 10, 300 << 10)))


BUILDERS = {
    "mixed": _mixed,
    "smallzoo": _smallzoo,
    "prefix": _prefix,
    "bighead": _bighead,
    "links": _links,
    "adversarial": _adversarial,
    "deletion": _deletion,
}

# Every tree is scanned with min_size 1 unless a case says otherwise: the point
# is to see the small files, not to skip them the way a day-to-day run does.
MIN_SIZE = 1


# ── build, load, and the answer key ──────────────────────────────────────────


def tree_dir(base: str, name: str, seed: int) -> str:
    # Absolute throughout: the adversarial tree plants a symlink loop pointing
    # at its own root, and a relative target would aim it at wherever the
    # harness happened to be started from.
    return os.path.join(os.path.abspath(base), "{}-seed{}".format(name, seed))


def build(base: str, name: str, seed: int) -> dict:
    """Write one tree and its manifest. Always destructive."""
    if name not in BUILDERS:
        raise KeyError(name)
    home = tree_dir(base, name, seed)
    unlock(home)
    shutil.rmtree(home, ignore_errors=True)
    root = os.path.join(home, "tree")
    os.makedirs(root)

    t = Tree(root, MIN_SIZE)
    BUILDERS[name](t, random.Random(seed))

    manifest = {
        "tree": name,
        "seed": seed,
        "generator_version": GENERATOR_VERSION,
        "min_size": MIN_SIZE,
        "file_count": len(t.records),
        "total_bytes": t.total_bytes,
        "unreadable": sorted(t.unreadable),
        "entries": t.entries(),
    }
    with open(os.path.join(home, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=1, sort_keys=True)
    return manifest


def load(base: str, name: str, seed: int):
    """The manifest for an existing tree, or None if it is missing, stale or
    unreadable. Anything doubtful is regenerated rather than trusted."""
    home = tree_dir(base, name, seed)
    try:
        with open(os.path.join(home, "manifest.json")) as fh:
            m = json.load(fh)
    except (OSError, ValueError):
        return None
    if not os.path.isdir(os.path.join(home, "tree")):
        return None
    if m.get("generator_version") != GENERATOR_VERSION:
        return None
    if (m.get("tree"), m.get("seed")) != (name, seed):
        return None
    return m


def ensure(base: str, name: str, seed: int) -> dict:
    return load(base, name, seed) or build(base, name, seed)


def unlock(path: str) -> None:
    """Give the 0o000 files their permissions back so the tree can be removed."""
    for dirpath, _, filenames in os.walk(path):
        for fn in filenames:
            full = os.path.join(dirpath, fn)
            try:
                if not os.path.islink(full) and not os.stat(full).st_mode & stat.S_IRUSR:
                    os.chmod(full, 0o644)
            except OSError:
                pass


def excluded(rel: str, patterns) -> bool:
    """The walk matches a glob against each entry's own name and prunes whole
    directories that match, so a path is out if any component matches."""
    return any(fnmatch.fnmatch(part, pat)
               for part in rel.split(os.sep) for pat in patterns)


def file_ext(name: str) -> str:
    stem, dot, ext = os.path.basename(name).rpartition(".")
    if not dot or not stem:
        return ""
    return ext.lower()


def ext_allowed(rel: str, include_ext, exclude_ext) -> bool:
    ext = file_ext(rel)
    if include_ext is not None and ext not in include_ext:
        return False
    return ext not in exclude_ext


def expected(manifest: dict, min_size: int = None, include_ext=None,
             exclude_ext=(), excludes=()) -> dict:
    """The answer key for one set of scan flags, derived from construction.

    Nothing here looks at the tree. The manifest recorded what every inode
    holds as it was written, so the groups a correct scan must report are a
    pure function of that record and the flags — which is what makes this an
    independent check on the oracle rather than an echo of it.
    """
    min_size = manifest["min_size"] if min_size is None else min_size
    exclude_ext = frozenset(exclude_ext)
    files_seen = 0
    bytes_seen = 0
    survivors = []

    for entry in manifest["entries"]:
        names = [n for n in entry["names"]
                 if not excluded(n, excludes)
                 and ext_allowed(n, include_ext, exclude_ext)]
        if not names:
            continue
        # Every surviving name is a directory entry the walk meets and counts,
        # before the size filter has any say.
        files_seen += len(names)
        bytes_seen += entry["size"] * len(names)
        if entry["size"] < min_size:
            continue
        survivors.append(
            {"name": canonical(names),
             "names": sorted(names),
             "size": entry["size"],
             "sha": entry["sha"],
             "readable": entry["readable"],
             # A name hidden by a filter still pins the inode, exactly like a
             # name outside the tree.
             "deletable": entry["deletable"] and names == sorted(entry["names"])})

    # A file alone at its size is never opened, so an unreadable one is never
    # discovered to be unreadable. Only records that share a size get that far.
    by_size: dict = {}
    for rec in survivors:
        by_size.setdefault(rec["size"], []).append(rec)

    by_content: dict = {}
    unreadable = []
    size_groups = []
    for size, bucket in by_size.items():
        if len(bucket) < 2:
            continue
        # What a walk and a size bucket alone can say: these files might be
        # duplicates. Milestone M1 is judged on exactly this much.
        size_groups.append({
            "size": size,
            "members": sorted((r["name"] for r in bucket),
                              key=lambda n: (len(n), n))})
        for rec in bucket:
            if rec["readable"]:
                by_content.setdefault((size, rec["sha"]), []).append(rec)
            else:
                unreadable.append(rec["name"])
    size_groups.sort(key=lambda g: (-g["size"],
                                    os.path.basename(g["members"][0]),
                                    g["members"][0]))

    groups = []
    for (size, _sha), members in by_content.items():
        if len(members) < 2:
            continue
        members.sort(key=lambda m: (len(m["name"]), m["name"]))
        groups.append({"size": size, "members": members})
    groups.sort(key=lambda g: (-g["size"],
                               os.path.basename(g["members"][0]["name"]),
                               g["members"][0]["name"]))

    return {
        "groups": [[m["name"] for m in g["members"]] for g in groups],
        "size_groups": [g["members"] for g in size_groups],
        "detail": groups,
        "files_seen": files_seen,
        "bytes_seen": bytes_seen,
        "unreadable": sorted(unreadable),
        # Proving a group means reading every byte of every member of it this
        # scan. No implementation that obeys that can read less than this.
        "byte_floor": sum(g["size"] * len(g["members"]) for g in groups),
    }


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description="build the conformance trees")
    ap.add_argument("--base", required=True)
    ap.add_argument("--seed", type=int, default=20260822)
    ap.add_argument("--tree", action="append", default=[])
    args = ap.parse_args()

    for name in (args.tree or list(BUILDERS)):
        m = ensure(args.base, name, args.seed)
        print("{:14s} {:5d} files  {:8.1f} MB  {} groups".format(
            name, m["file_count"], m["total_bytes"] / 1e6,
            len(expected(m)["groups"])))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
