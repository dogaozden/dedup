#!/usr/bin/env python3
"""dedup — find byte-exact duplicate files and move the extras to the Trash.

Scans the current directory (or given paths) recursively, groups files whose
contents are identical, and opens an interactive browser for choosing which
copies to keep.

Speed comes from never reading a file it doesn't have to:

  1. list every file's size   — one bulk syscall per directory, no reads
  2. discard unique sizes     — a file with a unique size cannot have a twin
  3. hash the first 64 KiB    — cheap, kills most remaining candidates
  4. hash in full             — only for files that survived stages 2 and 3

On a typical tree this reads a low single-digit percentage of the bytes on disk.

Filenames and extensions are ignored: two files are duplicates when their bytes
match, so a renamed copy or a .jpg/.jpeg pair is still caught.
"""
from __future__ import annotations

import argparse
import curses
import fnmatch
import hashlib
import os
import shutil
import sys
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

__version__ = "1.0.0"

HEAD_BYTES = 64 * 1024          # stage-3 sample size
CHUNK = 4 * 1024 * 1024         # streaming read size for full hashes
DIGEST_SIZE = 32                # blake2b-256; collision risk is not a practical concern


# ─────────────────────────────────────────────────────────────────────────────
# formatting
# ─────────────────────────────────────────────────────────────────────────────

def human_size(n: int) -> str:
    """Format a byte count the way a person would say it."""
    if n < 1024:
        return f"{n} B"
    for unit in ("KB", "MB", "GB", "TB", "PB"):
        n /= 1024.0
        if n < 1024:
            return f"{n:.1f} {unit}" if n < 100 else f"{n:.0f} {unit}"
    return f"{n:.1f} EB"


def human_count(n: int) -> str:
    return f"{n:,}"


def elide_left(text: str, width: int) -> str:
    """Trim from the left so the end of a path (the filename) stays visible."""
    if width <= 1:
        return ""
    if len(text) <= width:
        return text
    return "…" + text[-(width - 1):]


# ─────────────────────────────────────────────────────────────────────────────
# data model
# ─────────────────────────────────────────────────────────────────────────────

@dataclass
class FileRec:
    path: str
    size: int
    mtime: float
    dev: int
    ino: int
    nlink: int = 1
    links: list = field(default_factory=list)   # every name we found for this inode

    @property
    def display(self) -> str:
        """Relative to the working directory, but only when that's an
        improvement — a path outside it reads better absolute than as a
        pile of '..' segments."""
        try:
            rel = os.path.relpath(self.path)
        except ValueError:
            return self.path
        if rel.startswith(".."):
            return self.path
        return rel

    @property
    def date(self) -> str:
        return time.strftime("%Y-%m-%d", time.localtime(self.mtime))

    @property
    def all_names(self) -> list:
        return self.links or [self.path]

    @property
    def deletable(self) -> bool:
        """True when removing our copies actually frees the bytes.

        A hardlinked inode only releases its storage once every name is gone.
        If the scan can't see all of them, some name outside the tree is still
        holding the data down and deleting what we can see frees nothing.
        """
        return len(self.all_names) >= self.nlink


@dataclass
class DupGroup:
    size: int
    files: list[FileRec]

    @property
    def reclaimable(self) -> int:
        """Bytes freed by deleting every copy we're allowed to, keeping one."""
        deletable = sum(1 for f in self.files if f.deletable)
        return self.size * min(deletable, len(self.files) - 1)

    @property
    def name(self) -> str:
        return os.path.basename(self.files[0].path)


@dataclass
class ScanStats:
    files_seen: int = 0
    bytes_seen: int = 0
    unreadable: list[str] = field(default_factory=list)
    hashed_files: int = 0
    hashed_bytes: int = 0


# ─────────────────────────────────────────────────────────────────────────────
# scanning
# ─────────────────────────────────────────────────────────────────────────────

def walk_files(roots: list[str], excludes: list[str], min_size: int,
               stats: ScanStats, on_progress=None) -> list[FileRec]:
    """Collect every regular file under `roots`. Symlinks are never followed.

    Uses the bulk directory-metadata syscall where it's available, which is
    roughly twice as fast as stat()ing each file, and falls back to a plain
    scandir walk everywhere else.
    """
    if bulk_available():
        return _walk_bulk(roots, excludes, min_size, stats, on_progress)
    return _walk_scandir(roots, excludes, min_size, stats, on_progress)


def _walk_scandir(roots: list[str], excludes: list[str], min_size: int,
                  stats: ScanStats, on_progress=None) -> list[FileRec]:
    """Portable walk: one stat() per file."""
    out: list[FileRec] = []
    stack = [os.path.abspath(r) for r in roots]
    last_tick = 0.0

    while stack:
        current = stack.pop()
        try:
            entries = list(os.scandir(current))
        except (PermissionError, OSError) as e:
            stats.unreadable.append(f"{current}: {e}")
            continue

        for entry in entries:
            name = entry.name
            if any(fnmatch.fnmatch(name, pat) for pat in excludes):
                continue
            try:
                if entry.is_symlink():
                    continue
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                    continue
                if not entry.is_file(follow_symlinks=False):
                    continue
                st = entry.stat(follow_symlinks=False)
            except (PermissionError, OSError) as e:
                stats.unreadable.append(f"{entry.path}: {e}")
                continue

            stats.files_seen += 1
            stats.bytes_seen += st.st_size
            if st.st_size < min_size:
                continue
            out.append(FileRec(entry.path, st.st_size, st.st_mtime,
                               st.st_dev, st.st_ino, st.st_nlink))

        if on_progress:
            now = time.monotonic()
            if now - last_tick > 0.1:
                on_progress(stats)
                last_tick = now

    return out


# ── bulk directory metadata (macOS) ──────────────────────────────────────────
#
# getattrlistbulk() returns metadata for a whole batch of directory entries in
# one syscall, where scandir+stat costs one syscall per file. On a 284k-file
# tree that's 33k syscalls instead of 284k, and about twice as fast.
#
# Only the fields needed to *filter* are read in bulk — name, type, size. The
# few files that clear min_size then get an ordinary lstat() for mtime, inode
# and link count, which keeps the struct parsing simple and the numbers exact.

ATTR_BIT_MAP_COUNT = 5
ATTR_CMN_NAME = 0x00000001
ATTR_CMN_OBJTYPE = 0x00000008
ATTR_CMN_RETURNED_ATTRS = 0x80000000
ATTR_FILE_DATALENGTH = 0x00000200
VREG, VDIR = 1, 2                 # vnode types we care about
O_DIRECTORY = 0x00100000          # macOS value
BULK_BUF = 128 * 1024

_libc = None


def bulk_available() -> bool:
    """True when getattrlistbulk() can be used on this platform."""
    global _libc
    if sys.platform != "darwin":
        return False
    if _libc is None:
        import ctypes
        import ctypes.util
        try:
            lib = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
            lib.getattrlistbulk.argtypes = [ctypes.c_int, ctypes.c_void_p,
                                            ctypes.c_void_p, ctypes.c_size_t,
                                            ctypes.c_uint64]
            lib.getattrlistbulk.restype = ctypes.c_int
            _libc = lib
        except (OSError, AttributeError):
            _libc = False
    return bool(_libc)


def _walk_bulk(roots: list[str], excludes: list[str], min_size: int,
               stats: ScanStats, on_progress=None) -> list[FileRec]:
    if not bulk_available():
        return _walk_scandir(roots, excludes, min_size, stats, on_progress)

    import ctypes
    import struct

    class attrlist(ctypes.Structure):
        _fields_ = [("bitmapcount", ctypes.c_ushort), ("reserved", ctypes.c_uint16),
                    ("commonattr", ctypes.c_uint32), ("volattr", ctypes.c_uint32),
                    ("dirattr", ctypes.c_uint32), ("fileattr", ctypes.c_uint32),
                    ("forkattr", ctypes.c_uint32)]

    al = attrlist(ATTR_BIT_MAP_COUNT, 0,
                  ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE,
                  0, 0, ATTR_FILE_DATALENGTH, 0)
    buf = ctypes.create_string_buffer(BULK_BUF)
    view = memoryview(buf).cast("B")
    u32 = struct.Struct("<I").unpack_from
    i64 = struct.Struct("<q").unpack_from

    out: list[FileRec] = []
    stack = [os.path.abspath(r) for r in roots]
    last_tick = 0.0

    while stack:
        current = stack.pop()
        try:
            fd = os.open(current, os.O_RDONLY | O_DIRECTORY)
        except OSError as e:
            stats.unreadable.append(f"{current}: {e}")
            continue
        try:
            while True:
                count = _libc.getattrlistbulk(fd, ctypes.byref(al), buf, BULK_BUF, 0)
                if count <= 0:
                    break
                offset = 0
                for _ in range(count):
                    start = offset
                    entry_len = u32(buf, start)[0]
                    offset = start + entry_len
                    p = start + 4
                    returned_common = u32(buf, p)[0]
                    returned_file = u32(buf, p + 12)[0]
                    p += 20                       # attribute_set_t is 5 x uint32

                    if not returned_common & ATTR_CMN_NAME:
                        continue
                    rel, nlen = u32(buf, p)[0], u32(buf, p + 4)[0]
                    name = bytes(view[p + rel:p + rel + nlen - 1])
                    p += 8

                    objtype = 0
                    if returned_common & ATTR_CMN_OBJTYPE:
                        objtype = u32(buf, p)[0]
                        p += 4
                    size = 0
                    if returned_file & ATTR_FILE_DATALENGTH:
                        size = i64(buf, p)[0]

                    if objtype != VDIR and objtype != VREG:
                        continue

                    # Decoding the name and building the path costs more than
                    # everything else here, so the files that can't qualify are
                    # dropped on the raw bytes first.
                    if excludes:
                        name_str = name.decode("utf-8", "surrogateescape")
                        if any(fnmatch.fnmatch(name_str, pat) for pat in excludes):
                            continue
                    else:
                        name_str = None

                    if objtype == VREG:
                        stats.files_seen += 1
                        stats.bytes_seen += size
                        if size < min_size:
                            continue

                    if name_str is None:
                        name_str = name.decode("utf-8", "surrogateescape")
                    path = os.path.join(current, name_str)
                    if objtype == VDIR:
                        stack.append(path)
                        continue
                    # Only survivors pay for a stat, and it keeps inode,
                    # link count and mtime exactly as the portable walk sees them.
                    try:
                        st = os.lstat(path)
                    except OSError as e:
                        stats.unreadable.append(f"{path}: {e}")
                        continue
                    out.append(FileRec(path, st.st_size, st.st_mtime,
                                       st.st_dev, st.st_ino, st.st_nlink))
        except OSError as e:
            stats.unreadable.append(f"{current}: {e}")
        finally:
            os.close(fd)

        if on_progress:
            now = time.monotonic()
            if now - last_tick > 0.1:
                on_progress(stats)
                last_tick = now

    return out


def drop_hardlinks(files: list[FileRec]) -> list[FileRec]:
    """Collapse each (device, inode) to a single record listing all its names.

    Hardlinks are one file wearing several names. Counting each name as a
    separate duplicate would promise space that deleting them never frees, so
    the names travel together on one record and are removed as a unit.
    """
    by_inode: dict = {}
    order: list = []
    for f in files:
        key = (f.dev, f.ino)
        if key not in by_inode:
            by_inode[key] = f
            order.append(key)
        by_inode[key].links.append(f.path)
    # shortest name first, so the record displays the tidiest path
    for rec in by_inode.values():
        rec.links.sort(key=lambda p: (len(p), p))
        rec.path = rec.links[0]
    return [by_inode[k] for k in order]


# ─────────────────────────────────────────────────────────────────────────────
# hashing
# ─────────────────────────────────────────────────────────────────────────────

def hash_head(path: str, size: int) -> bytes | None:
    try:
        with open(path, "rb") as fh:
            return hashlib.blake2b(fh.read(HEAD_BYTES), digest_size=DIGEST_SIZE).digest()
    except (PermissionError, OSError):
        return None


def hash_full(path: str, size: int) -> bytes | None:
    h = hashlib.blake2b(digest_size=DIGEST_SIZE)
    try:
        with open(path, "rb") as fh:
            while True:
                block = fh.read(CHUNK)
                if not block:
                    break
                h.update(block)
        return h.digest()
    except (PermissionError, OSError):
        return None


def hash_stage(files: list[FileRec], fn, jobs: int, stats: ScanStats,
               on_progress=None) -> dict[bytes, list[FileRec]]:
    """Hash `files` with `fn` and bucket them by digest."""
    buckets: dict[bytes, list[FileRec]] = defaultdict(list)
    last_tick = 0.0

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for rec, digest in zip(files, pool.map(lambda f: fn(f.path, f.size), files)):
            if digest is None:
                stats.unreadable.append(rec.path)
                continue
            buckets[digest].append(rec)
            stats.hashed_files += 1
            stats.hashed_bytes += min(rec.size, HEAD_BYTES) if fn is hash_head else rec.size
            if on_progress:
                now = time.monotonic()
                if now - last_tick > 0.1:
                    on_progress(stats)
                    last_tick = now

    return buckets


def bytes_identical(paths: list[str]) -> bool:
    """Byte-for-byte comparison, for --verify."""
    try:
        handles = [open(p, "rb") for p in paths]
    except OSError:
        return False
    try:
        while True:
            blocks = [fh.read(CHUNK) for fh in handles]
            first = blocks[0]
            if any(b != first for b in blocks[1:]):
                return False
            if not first:
                return True
    finally:
        for fh in handles:
            fh.close()


# ─────────────────────────────────────────────────────────────────────────────
# pipeline
# ─────────────────────────────────────────────────────────────────────────────

def find_duplicates(roots: list[str], *, excludes: list[str], min_size: int,
                    jobs: int, verify: bool, stats: ScanStats,
                    reporter=None) -> list[DupGroup]:
    """Run the four-stage narrowing described in the module docstring."""
    def say(key: str, total: int = 0) -> None:
        if reporter:
            reporter.stage(key, total, stats)

    tick = reporter.tick if reporter else None

    say("scan")
    files = walk_files(roots, excludes, min_size, stats, tick)
    if reporter:
        reporter.finish_stage()

    # stage 2 — group by size
    by_size: dict[int, list[FileRec]] = defaultdict(list)
    for f in files:
        by_size[f.size].append(f)
    candidates: list[FileRec] = []
    for size, group in by_size.items():
        group = drop_hardlinks(group)
        if len(group) > 1:
            candidates.extend(group)

    if not candidates:
        return []

    # stage 3 — sample the head
    say("head", len(candidates))
    head_buckets = hash_stage(candidates, hash_head, jobs, stats, tick)
    if reporter:
        reporter.finish_stage()

    survivors: list[FileRec] = []
    for group in head_buckets.values():
        if len(group) > 1:
            survivors.extend(group)

    if not survivors:
        return []

    # stage 4 — full content hash
    say("full", len(survivors))
    full_buckets = hash_stage(survivors, hash_full, jobs, stats, tick)
    if reporter:
        reporter.finish_stage()

    groups: list[DupGroup] = []
    for members in full_buckets.values():
        if len(members) < 2:
            continue
        if verify and not bytes_identical([m.path for m in members]):
            continue
        members.sort(key=lambda f: (len(f.path), f.path))
        groups.append(DupGroup(members[0].size, members))

    groups.sort(key=lambda g: (-g.size, g.name))
    return groups


# ─────────────────────────────────────────────────────────────────────────────
# trash
# ─────────────────────────────────────────────────────────────────────────────

def volume_root(path: str) -> Path:
    """Find the mount point of the volume holding `path`."""
    p = Path(path).resolve()
    try:
        dev = p.stat().st_dev
    except OSError:
        return Path("/")
    cur = p
    while cur != cur.parent:
        try:
            if cur.parent.stat().st_dev != dev:
                return cur
        except OSError:
            return cur
        cur = cur.parent
    return Path("/")


def trash_dir_for(path: str) -> Path:
    """The correct Trash for the volume `path` lives on.

    Files on an external drive go to that drive's .Trashes rather than the
    home folder, so emptying the Trash doesn't mean copying gigabytes across
    a USB cable first.
    """
    root = volume_root(path)
    if root == Path("/"):
        return Path.home() / ".Trash"
    return root / ".Trashes" / str(os.getuid())


def unique_destination(directory: Path, name: str) -> Path:
    """Pick a non-colliding name inside `directory`."""
    target = directory / name
    if not target.exists():
        return target
    stem, ext = os.path.splitext(name)
    for n in range(2, 10000):
        candidate = directory / f"{stem} {n}{ext}"
        if not candidate.exists():
            return candidate
    return directory / f"{stem} {int(time.time())}{ext}"


def send_to_trash(path: str) -> tuple[bool, str]:
    """Move one file to the Trash. Returns (ok, message)."""
    try:
        tdir = trash_dir_for(path)
        tdir.mkdir(parents=True, exist_ok=True)
        shutil.move(path, str(unique_destination(tdir, os.path.basename(path))))
        return True, ""
    except Exception as e:                      # noqa: BLE001 - report, never crash
        return False, str(e)


def delete_permanently(path: str) -> tuple[bool, str]:
    try:
        os.unlink(path)
        return True, ""
    except Exception as e:                      # noqa: BLE001
        return False, str(e)


def remove_record(rec: FileRec, remover) -> tuple[bool, str]:
    """Remove every name pointing at this record's inode.

    Storage is only released once the last hardlink is gone, so a partial
    success here would free nothing while looking like it worked.
    """
    errors = []
    for name in rec.all_names:
        ok, err = remover(name)
        if not ok:
            errors.append(f"{name}: {err}")
    return (not errors), "; ".join(errors)


# ─────────────────────────────────────────────────────────────────────────────
# scan progress (plain terminal, before the TUI takes over)
# ─────────────────────────────────────────────────────────────────────────────

class Reporter:
    """Single-line live progress written to stderr."""

    LABELS = {
        "scan": "Scanning",
        "head": "Sampling",
        "full": "Hashing",
    }

    def __init__(self, enabled: bool = True):
        self.enabled = enabled and sys.stderr.isatty()
        self.label = ""
        self.total = 0
        self.started = time.monotonic()
        self.base_files = 0

    def stage(self, key: str, total: int = 0, stats: ScanStats | None = None) -> None:
        self.label = self.LABELS.get(key, key)
        self.total = total
        self.started = time.monotonic()
        # progress is per-stage, but ScanStats counts cumulatively
        self.base_files = stats.hashed_files if stats else 0

    def tick(self, stats: ScanStats) -> None:
        if not self.enabled:
            return
        if self.label == "Scanning":
            body = f"{human_count(stats.files_seen)} files · {human_size(stats.bytes_seen)}"
        else:
            done = stats.hashed_files - self.base_files
            pct = (done / self.total * 100) if self.total else 0
            bar_w = 24
            filled = int(bar_w * pct / 100)
            bar = "█" * filled + "░" * (bar_w - filled)
            elapsed = max(0.001, time.monotonic() - self.started)
            rate = human_size(int(stats.hashed_bytes / elapsed)) + "/s"
            body = f"{bar} {pct:3.0f}%  {human_count(done)}/{human_count(self.total)} · {rate}"
        sys.stderr.write(f"\r\033[2K  {self.label}…  {body}")
        sys.stderr.flush()

    def finish_stage(self) -> None:
        if self.enabled:
            sys.stderr.write("\r\033[2K")
            sys.stderr.flush()

    def done(self, msg: str) -> None:
        if self.enabled:
            sys.stderr.write(f"\r\033[2K{msg}\n")
            sys.stderr.flush()


# ─────────────────────────────────────────────────────────────────────────────
# interactive browser
# ─────────────────────────────────────────────────────────────────────────────

C_HEADER, C_ACCENT, C_MARK, C_KEEP, C_DIM, C_CURSOR, C_WARN = range(1, 8)

HELP_LINES = [
    ("↑ ↓  or  j k", "move between files in this group"),
    ("← →  or  h l", "previous / next group"),
    ("space", "mark or unmark the highlighted file"),
    ("a", "mark every copy except the highlighted one"),
    ("u", "clear all marks in this group"),
    ("g / G", "jump to first / last group"),
    ("b", "bulk actions across every group"),
    ("ENTER", "review and apply — move marked files to Trash"),
    ("?", "toggle this help"),
    ("q", "quit without deleting anything"),
]

# Which copy survives. Each returns the index to keep.
KEEP_RULES = {
    "oldest": lambda fs: min(range(len(fs)), key=lambda i: fs[i].mtime),
    "newest": lambda fs: max(range(len(fs)), key=lambda i: fs[i].mtime),
    "shortest": lambda fs: min(range(len(fs)), key=lambda i: (len(fs[i].path), fs[i].path)),
}

BULK_ACTIONS = [
    ("1", "Keep the oldest copy in every group", KEEP_RULES["oldest"]),
    ("2", "Keep the newest copy in every group", KEEP_RULES["newest"]),
    ("3", "Keep the shortest path in every group", KEEP_RULES["shortest"]),
]


class Browser:
    def __init__(self, groups: list[DupGroup], root_label: str, hard: bool):
        self.groups = groups
        self.root_label = root_label
        self.hard = hard
        self.gi = 0
        self.fi = 0
        self.marked: set[tuple[int, int]] = set()
        self.show_help = False
        self.flash = ""
        self.applied_files = 0
        self.applied_bytes = 0
        self.failures: list[str] = []

    # ── state helpers ────────────────────────────────────────────────────
    @property
    def group(self) -> DupGroup:
        return self.groups[self.gi]

    def marked_bytes(self) -> int:
        return sum(self.groups[g].size for (g, _) in self.marked)

    def group_marks(self, gi: int) -> set[int]:
        return {f for (g, f) in self.marked if g == gi}

    def toggle(self) -> None:
        key = (self.gi, self.fi)
        if key in self.marked:
            self.marked.discard(key)
            return
        if not self.group.files[self.fi].deletable:
            self.flash = "hardlinked outside this tree — deleting it frees nothing"
            return
        # never let the last surviving copy be marked
        if len(self.group_marks(self.gi)) >= len(self.group.files) - 1:
            self.flash = "at least one copy has to stay"
            return
        self.marked.add(key)

    def keep_only_current(self) -> None:
        for i, rec in enumerate(self.group.files):
            key = (self.gi, i)
            if i == self.fi or not rec.deletable:
                self.marked.discard(key)
            else:
                self.marked.add(key)

    def clear_group(self) -> None:
        for i in range(len(self.group.files)):
            self.marked.discard((self.gi, i))

    def apply_bulk(self, chooser) -> int:
        self.marked.clear()
        for gi, group in enumerate(self.groups):
            keep = chooser(group.files)
            for fi, rec in enumerate(group.files):
                if fi != keep and rec.deletable:
                    self.marked.add((gi, fi))
        return len(self.marked)

    # ── drawing ──────────────────────────────────────────────────────────
    @staticmethod
    def _put(win, y: int, x: int, text: str, attr=0) -> None:
        h, w = win.getmaxyx()
        if y < 0 or y >= h or x >= w:
            return
        try:
            win.addnstr(y, x, text, max(0, w - x - 1), attr)
        except curses.error:
            pass

    def draw(self, win) -> None:
        win.erase()
        h, w = win.getmaxyx()
        if h < 12 or w < 54:
            self._put(win, 0, 0, "Terminal too small — resize to at least 54×12")
            win.refresh()
            return
        if not self.groups:
            self._put(win, 1, 2, "No duplicates left.", curses.A_BOLD)
            self._put(win, 3, 2, "q to quit", curses.color_pair(C_DIM))
            win.refresh()
            return

        rule = "─" * (w - 2)
        total_reclaim = sum(g.reclaimable for g in self.groups)

        # header
        self._put(win, 0, 1, "dedup", curses.color_pair(C_ACCENT) | curses.A_BOLD)
        label = elide_left(self.root_label, w - 12)
        self._put(win, 0, w - len(label) - 2, label, curses.color_pair(C_DIM))
        self._put(win, 1, 1, rule, curses.color_pair(C_DIM))

        summary = (f"{human_count(len(self.groups))} groups · "
                   f"{human_size(total_reclaim)} reclaimable")
        self._put(win, 2, 2, summary, curses.A_BOLD)
        if self.marked:
            mark_txt = (f"{human_count(len(self.marked))} marked · "
                        f"{human_size(self.marked_bytes())} to free")
            self._put(win, 2, w - len(mark_txt) - 2, mark_txt,
                      curses.color_pair(C_MARK) | curses.A_BOLD)

        if self.show_help:
            self._draw_help(win, h, w)
            win.refresh()
            return

        # group heading
        g = self.group
        heading = f"Group {self.gi + 1} of {len(self.groups)}"
        detail = f"{human_size(g.size)} × {len(g.files)} copies"
        self._put(win, 4, 2, heading, curses.color_pair(C_ACCENT) | curses.A_BOLD)
        self._put(win, 4, w - len(detail) - 2, detail, curses.color_pair(C_DIM))
        self._put(win, 5, 1, rule, curses.color_pair(C_DIM))

        # file list
        top = 6
        avail = h - top - 5
        start = max(0, min(self.fi - avail // 2, len(g.files) - avail))
        start = max(0, start)
        for row, idx in enumerate(range(start, min(len(g.files), start + avail))):
            f = g.files[idx]
            y = top + row
            is_marked = (self.gi, idx) in self.marked
            is_cursor = idx == self.fi

            if not f.deletable:
                glyph, colour = "⊘", curses.color_pair(C_DIM)
            elif is_marked:
                glyph, colour = "●", curses.color_pair(C_MARK)
            else:
                glyph, colour = "○", curses.color_pair(C_KEEP)
            self._put(win, y, 3, "›" if is_cursor else " ",
                      curses.color_pair(C_ACCENT) | curses.A_BOLD)
            self._put(win, y, 5, glyph, colour | curses.A_BOLD)

            extra = len(f.all_names) - 1
            badge = f"  +{extra} link{'s' if extra > 1 else ''}" if extra else ""
            date_w = 12
            path_w = w - 9 - date_w - len(badge)
            path_attr = curses.A_REVERSE if is_cursor else (
                curses.color_pair(C_DIM) if is_marked else 0)
            self._put(win, y, 7, elide_left(f.display, path_w), path_attr)
            if badge:
                self._put(win, y, 7 + min(len(f.display), path_w), badge,
                          curses.color_pair(C_WARN))
            self._put(win, y, w - date_w - 1, f.date, curses.color_pair(C_DIM))

        if len(g.files) > avail:
            more = f"… {len(g.files) - avail} more"
            self._put(win, top + avail - 1, w - len(more) - 2, more,
                      curses.color_pair(C_DIM))

        # footer
        self._put(win, h - 4, 1, rule, curses.color_pair(C_DIM))
        if self.flash:
            self._put(win, h - 3, 2, self.flash,
                      curses.color_pair(C_WARN) | curses.A_BOLD)
        else:
            self._put(win, h - 3, 2,
                      "↑↓ file   ←→ group   space mark   a keep-this   u clear",
                      curses.color_pair(C_DIM))
        self._put(win, h - 2, 2,
                  "b bulk    ENTER apply   ? help    q quit",
                  curses.color_pair(C_DIM))
        win.refresh()

    def _draw_help(self, win, h: int, w: int) -> None:
        self._put(win, 4, 2, "Keys", curses.color_pair(C_ACCENT) | curses.A_BOLD)
        for i, (keys, desc) in enumerate(HELP_LINES):
            y = 6 + i
            if y >= h - 3:
                break
            self._put(win, y, 4, f"{keys:<14}", curses.A_BOLD)
            self._put(win, y, 20, desc, curses.color_pair(C_DIM))
        self._put(win, h - 2, 2, "? or esc to go back", curses.color_pair(C_DIM))

    # ── overlays ─────────────────────────────────────────────────────────
    def _overlay(self, win, title: str, lines: list[str], footer: str) -> None:
        h, w = win.getmaxyx()
        box_w = min(max(len(title) + 6, max((len(l) for l in lines), default=0) + 6,
                        len(footer) + 6), w - 4)
        box_h = len(lines) + 6
        y0 = max(0, (h - box_h) // 2)
        x0 = max(0, (w - box_w) // 2)

        self._put(win, y0, x0, "┌─ " + title + " " + "─" * max(0, box_w - len(title) - 5) + "┐",
                  curses.color_pair(C_ACCENT))
        for i in range(1, box_h - 1):
            self._put(win, y0 + i, x0, "│" + " " * (box_w - 2) + "│",
                      curses.color_pair(C_ACCENT))
        self._put(win, y0 + box_h - 1, x0, "└" + "─" * (box_w - 2) + "┘",
                  curses.color_pair(C_ACCENT))
        for i, line in enumerate(lines):
            self._put(win, y0 + 2 + i, x0 + 3, line)
        self._put(win, y0 + box_h - 2, x0 + 3, footer, curses.color_pair(C_DIM))
        win.refresh()

    def bulk_menu(self, win) -> None:
        lines = [f"{key}   {desc}" for key, desc, _ in BULK_ACTIONS]
        lines.append("0   Clear every mark")
        self._overlay(win, "Bulk actions", lines, "esc to cancel")
        while True:
            ch = win.getch()
            if ch in (27, ord("q")):
                return
            if ch == ord("0"):
                self.marked.clear()
                self.flash = "all marks cleared"
                return
            for key, desc, chooser in BULK_ACTIONS:
                if ch == ord(key):
                    n = self.apply_bulk(chooser)
                    self.flash = f"{human_count(n)} files marked — {desc.lower()}"
                    return

    def confirm_and_apply(self, win) -> bool:
        """Returns True if the user wants to leave after applying."""
        if not self.marked:
            self.flash = "nothing marked yet"
            return False

        verb = "Delete permanently" if self.hard else "Move to Trash"
        lines = [
            f"{verb}: {human_count(len(self.marked))} files",
            f"Reclaims {human_size(self.marked_bytes())}",
        ]
        if self.hard:
            lines.append("")
            lines.append("This cannot be undone.")
        self._overlay(win, "Confirm", lines, "y  confirm      n  cancel")

        while True:
            ch = win.getch()
            if ch in (ord("n"), ord("N"), 27):
                return False
            if ch in (ord("y"), ord("Y")):
                break

        self._overlay(win, "Working", ["Moving files…"], "")
        remover = delete_permanently if self.hard else send_to_trash
        removed: set[tuple[int, int]] = set()
        for (gi, fi) in sorted(self.marked):
            rec = self.groups[gi].files[fi]
            ok, err = remove_record(rec, remover)
            if ok:
                removed.add((gi, fi))
                self.applied_files += len(rec.all_names)
                self.applied_bytes += rec.size
            else:
                self.failures.append(err)

        # rebuild groups without the files that are now gone
        rebuilt: list[DupGroup] = []
        for gi, group in enumerate(self.groups):
            keep = [f for fi, f in enumerate(group.files) if (gi, fi) not in removed]
            if len(keep) > 1:
                rebuilt.append(DupGroup(group.size, keep))
        self.groups = rebuilt
        self.marked.clear()
        self.gi = min(self.gi, max(0, len(self.groups) - 1))
        self.fi = 0
        self.flash = f"moved {human_count(self.applied_files)} files"
        return not self.groups

    # ── main loop ────────────────────────────────────────────────────────
    def run(self, win) -> None:
        curses.curs_set(0)
        win.keypad(True)
        while True:
            self.draw(win)
            ch = win.getch()
            self.flash = ""

            if ch == curses.KEY_RESIZE:
                continue
            if self.show_help:
                if ch in (ord("?"), 27, ord("q")):
                    self.show_help = False
                continue

            if ch in (ord("q"), ord("Q")):
                return
            if ch == ord("?"):
                self.show_help = True
            elif ch in (curses.KEY_DOWN, ord("j")):
                self.fi = min(self.fi + 1, len(self.group.files) - 1)
            elif ch in (curses.KEY_UP, ord("k")):
                self.fi = max(self.fi - 1, 0)
            elif ch in (curses.KEY_RIGHT, ord("l"), ord("n")):
                if self.gi < len(self.groups) - 1:
                    self.gi += 1
                    self.fi = 0
            elif ch in (curses.KEY_LEFT, ord("h"), ord("p")):
                if self.gi > 0:
                    self.gi -= 1
                    self.fi = 0
            elif ch == ord("g"):
                self.gi, self.fi = 0, 0
            elif ch == ord("G"):
                self.gi, self.fi = len(self.groups) - 1, 0
            elif ch == ord(" "):
                self.toggle()
            elif ch == ord("a"):
                self.keep_only_current()
            elif ch == ord("u"):
                self.clear_group()
            elif ch == ord("b"):
                self.bulk_menu(win)
            elif ch in (curses.KEY_ENTER, 10, 13):
                if self.confirm_and_apply(win):
                    return


def init_colors() -> None:
    try:
        curses.start_color()
        curses.use_default_colors()
    except curses.error:
        return
    for pair, fg in (
        (C_HEADER, curses.COLOR_WHITE),
        (C_ACCENT, curses.COLOR_CYAN),
        (C_MARK, curses.COLOR_RED),
        (C_KEEP, curses.COLOR_GREEN),
        (C_DIM, curses.COLOR_BLUE),
        (C_CURSOR, curses.COLOR_YELLOW),
        (C_WARN, curses.COLOR_YELLOW),
    ):
        try:
            curses.init_pair(pair, fg, -1)
        except curses.error:
            pass


# ─────────────────────────────────────────────────────────────────────────────
# non-interactive output
# ─────────────────────────────────────────────────────────────────────────────

def plan_auto(groups: list[DupGroup], rule: str) -> list:
    """Pick one keeper per group; return the records to remove."""
    chooser = KEEP_RULES[rule]
    doomed = []
    for group in groups:
        keep = chooser(group.files)
        for i, rec in enumerate(group.files):
            if i != keep and rec.deletable:
                doomed.append(rec)
    return doomed


def run_auto(groups: list[DupGroup], rule: str, hard: bool, assume_yes: bool) -> int:
    """Non-interactive cleanup: keep one copy per group by `rule`, remove the rest."""
    doomed = plan_auto(groups, rule)
    if not doomed:
        print("\n  Nothing to remove.\n")
        return 0

    total = sum(r.size for r in doomed)
    verb = "Delete permanently" if hard else "Move to Trash"
    print(f"\n  Keeping the {rule} copy in each of "
          f"{human_count(len(groups))} groups.")
    print(f"  {verb}: {human_count(len(doomed))} files · "
          f"{human_size(total)} reclaimed\n")

    preview = 15
    for rec in doomed[:preview]:
        print(f"      {rec.display}")
    if len(doomed) > preview:
        print(f"      … and {human_count(len(doomed) - preview)} more")
    print()

    if not assume_yes:
        if not sys.stdin.isatty():
            sys.stdout.flush()      # keep the plan above the refusal
            print("  Refusing to delete without confirmation. "
                  "Re-run with -y to proceed.", file=sys.stderr)
            return 1
        try:
            answer = input("  Type 'yes' to proceed: ").strip()
        except (EOFError, KeyboardInterrupt):
            print("\n  Aborted.")
            return 1
        if answer != "yes":
            print("  Aborted. Nothing deleted.")
            return 1

    remover = delete_permanently if hard else send_to_trash
    done = failed = 0
    freed = 0
    failures = []
    for rec in doomed:
        ok, err = remove_record(rec, remover)
        if ok:
            done += len(rec.all_names)
            freed += rec.size
        else:
            failed += 1
            failures.append(err)

    where = "deleted" if hard else "moved to Trash"
    print(f"\n  {human_count(done)} files {where} · {human_size(freed)} reclaimed")
    if failures:
        print(f"  {human_count(failed)} could not be removed:", file=sys.stderr)
        for line in failures[:10]:
            print(f"    {line}", file=sys.stderr)
    print()
    return 0


def print_report(groups: list[DupGroup], limit: int | None = None) -> None:
    total = sum(g.reclaimable for g in groups)
    print(f"\n  {human_count(len(groups))} duplicate groups · "
          f"{human_size(total)} reclaimable\n")
    for i, g in enumerate(groups):
        if limit and i >= limit:
            print(f"  … and {human_count(len(groups) - limit)} more groups")
            break
        print(f"  {human_size(g.size)} × {len(g.files)}  "
              f"(reclaims {human_size(g.reclaimable)})")
        for f in g.files:
            extra = len(f.all_names) - 1
            note = ""
            if extra:
                note = f"   [+{extra} hardlink{'s' if extra > 1 else ''}]"
            if not f.deletable:
                note += "   [linked outside scan — cannot free space]"
            print(f"      {f.display}{note}")
        print()


# ─────────────────────────────────────────────────────────────────────────────
# cli
# ─────────────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="dedup",
        description="Find byte-exact duplicate files and move the extras to the Trash.",
        epilog="Runs on the current directory when no path is given.",
    )
    p.add_argument("paths", nargs="*", default=["."],
                   help="directories to scan (default: current directory)")
    p.add_argument("-l", "--list", action="store_true",
                   help="print a report instead of opening the browser")
    p.add_argument("--auto", nargs="?", const="shortest", metavar="RULE",
                   choices=list(KEEP_RULES),
                   help="skip the browser: keep one copy per group "
                        "(shortest|oldest|newest, default shortest) and remove "
                        "the rest after one confirmation")
    p.add_argument("-y", "--yes", action="store_true",
                   help="with --auto, don't ask for confirmation")
    p.add_argument("-j", "--jobs", type=int, default=min(8, (os.cpu_count() or 4)),
                   help="parallel readers (use 2 on a spinning external drive)")
    p.add_argument("--min-size", type=int, default=10 * 1024 * 1024, metavar="BYTES",
                   help="ignore files smaller than this (default: 10485760, 10 MB)")
    p.add_argument("-x", "--exclude", action="append", default=[], metavar="GLOB",
                   help="skip names matching GLOB (repeatable)")
    p.add_argument("--verify", action="store_true",
                   help="byte-compare matches before reporting them")
    p.add_argument("--hard", action="store_true",
                   help="delete permanently instead of moving to the Trash")
    p.add_argument("-q", "--quiet", action="store_true", help="no progress output")
    p.add_argument("-V", "--version", action="version", version=f"dedup {__version__}")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    for path in args.paths:
        if not os.path.isdir(path):
            print(f"dedup: not a directory: {path}", file=sys.stderr)
            return 2

    stats = ScanStats()
    reporter = Reporter(enabled=not args.quiet)
    started = time.monotonic()

    try:
        groups = find_duplicates(
            args.paths,
            excludes=args.exclude,
            min_size=args.min_size,
            jobs=max(1, args.jobs),
            verify=args.verify,
            stats=stats,
            reporter=reporter,
        )
    except KeyboardInterrupt:
        reporter.finish_stage()
        print("dedup: interrupted", file=sys.stderr)
        return 130

    elapsed = time.monotonic() - started
    read_pct = (stats.hashed_bytes / stats.bytes_seen * 100) if stats.bytes_seen else 0
    reporter.done(
        f"  Scanned {human_count(stats.files_seen)} files "
        f"({human_size(stats.bytes_seen)}) in {elapsed:.1f}s · "
        f"read {read_pct:.1f}% of them"
    )

    if stats.unreadable and not args.quiet:
        print(f"  {human_count(len(stats.unreadable))} items skipped "
              f"(permissions or I/O errors)", file=sys.stderr)

    if not groups:
        print("\n  No duplicates found.\n")
        return 0

    if args.auto:
        return run_auto(groups, args.auto, args.hard, args.yes)

    if args.list or not sys.stdout.isatty():
        print_report(groups)
        return 0

    browser = Browser(groups, os.path.abspath(args.paths[0]), args.hard)

    def _run(win):
        init_colors()
        browser.run(win)

    try:
        curses.wrapper(_run)
    except KeyboardInterrupt:
        pass

    if browser.applied_files:
        where = "deleted" if args.hard else "moved to Trash"
        print(f"\n  {human_count(browser.applied_files)} files {where} · "
              f"{human_size(browser.applied_bytes)} reclaimed\n")
    if browser.failures:
        print(f"  {human_count(len(browser.failures))} could not be removed:",
              file=sys.stderr)
        for line in browser.failures[:10]:
            print(f"    {line}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
