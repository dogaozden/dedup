#!/usr/bin/env python3
"""dedup — find byte-exact duplicate files and move the extras to the Trash.

Scans the current directory (or given paths) recursively, groups files whose
contents are identical, and opens an interactive browser for choosing which
copies to keep.

Speed comes from never reading a file it doesn't have to, and never reading
the same bytes twice:

  1. list every file's size   — one bulk syscall per directory, no reads
  2. discard unique sizes     — a file with a unique size cannot have a twin
  3. a page from each end     — one open apiece, and it settles most of them;
                                then the rest of the first 64 KiB, for
                                whatever those two pages left crowded
  4. hash 8 windows across it — settles big files that share a header
  5. compare the bytes        — in step, stopping the moment two files differ

What an earlier stage read is kept and compared in stage 5 rather than read
again, so a file that turns out to be a duplicate is read exactly once, end
to end. That is also why stage 5 can be trusted: it compares bytes, never a
digest of them.

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
import sqlite3
import sys
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path

__version__ = "1.0.0"

HEAD_BYTES = 4 * 1024           # stage-3 first pass: one page, enough to
                                # separate files that differ in their header
DEEP_HEAD = 64 * 1024           # ...and how far stage 3 extends for the files
                                # a single page could not tell apart
TAIL_BYTES = 4 * 1024           # one page from the far end, read before any
                                # more of the head: same-size files that share
                                # a header are the ordinary case, and their
                                # last page is where they differ
SAMPLE_BYTES = 64 * 1024        # per sample point in stage 4. A window only
                                # ever catches a difference that spans a
                                # region, and for that it is how many windows
                                # there are and how far apart they sit that
                                # decides, not how fat each one is
SAMPLE_POINTS = 8               # spread across the part stage 3 has not read
SAMPLE_SPAN = 8 * 1024 * 1024   # below this, just read it all: sampling a
                                # smaller file costs an open of its own and
                                # saves less than that open is worth
CHUNK = 4 * 1024 * 1024         # comparison chunk on flash
SLOW_CHUNK = 16 * 1024 * 1024   # ...and on a spinning disk, where every extra
                                # file in a group costs a seek per round
COMPARE_BUDGET = 128 * 1024 * 1024   # ceiling on bytes held for one group
MIN_CHUNK = 128 * 1024          # ...which the chunk shrinks towards, but not
                                # below, so a group of a thousand copies still
                                # fits and a read is still worth making
HASH_BATCH = 64                 # files handed to a reader thread at a time —
                                # one task per file spends more time in the
                                # queue than in the read
PARTITION_FANOUT = 256          # a compare pass that strips off nothing but
                                # its own pivot, with this many blocks still
                                # pending, means the pivot loop is going
                                # quadratic — see partition_blocks
RETAIN_BUDGET = 128 * 1024 * 1024    # ceiling on head bytes carried from stage
                                # 3 to stage 5 so the compare does not read
                                # them a second time — see find_duplicates

CACHE_SCHEMA = 2                # bump when the stored columns change meaning.
                                # 2: stage 3 became one digest of both end
                                # pages plus a separate deep-head tier, and
                                # stage 4 changed where it looks — every
                                # digest a version-1 file holds was taken over
                                # different bytes than the column it sits in
                                # now names
CACHE_TTL_DAYS = 180            # entries not seen in this long are dropped


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
    mtime_ns: int = 0                           # exact mtime, for the cache key
    head: bytes = b""       # bytes stage 3 read from offset 0, kept so stage 5
                            # can compare them instead of reading them again
    tail: bytes = b""       # ...and the same for the page at the far end

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
    # True when the members were matched from remembered digests instead of
    # being read this scan. Such a group is a candidate, not a conclusion:
    # nothing may be deleted from it until verify_attested has read the bytes.
    attested: bool = False

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

_solid_state_cache: dict = {}


def mount_point(path: str):
    """The volume root `path` sits on — diskutil only accepts these, not
    arbitrary paths inside a volume."""
    try:
        path = os.path.realpath(path)
        dev = os.stat(path).st_dev
    except OSError:
        return None
    while True:
        parent = os.path.dirname(path)
        if parent == path:
            return path
        try:
            if os.stat(parent).st_dev != dev:
                return path
        except OSError:
            return path
        path = parent


def is_solid_state(path: str):
    """True for flash, False for a spinning disk, None when it can't be told.

    USB enclosures, network shares and disk images generally don't report it.
    'Unknown' is treated as 'don't parallelise' — guessing wrong on a spinning
    disk costs more than the parallelism was ever going to win.
    """
    if sys.platform != "darwin":
        return None
    try:
        key = os.stat(path).st_dev
    except OSError:
        return None
    if key in _solid_state_cache:
        return _solid_state_cache[key]

    answer = None
    root = mount_point(path)
    if root:
        try:
            import plistlib
            import subprocess
            out = subprocess.run(["diskutil", "info", "-plist", root],
                                 capture_output=True, timeout=5)
            if out.returncode == 0:
                value = plistlib.loads(out.stdout).get("SolidState")
                if isinstance(value, bool):
                    answer = value
        except Exception:
            answer = None

    _solid_state_cache[key] = answer
    return answer


def all_solid_state(roots: list[str]) -> bool:
    """True only when every root is known to be flash. 'Don't know' is a no."""
    return bool(roots) and all(is_solid_state(r) is True for r in roots)


def walk_workers(roots: list[str], requested) -> int:
    """How many directories to read at once: explicit setting, or by device."""
    if requested is not None:
        return max(1, requested)
    if not bulk_available():
        return 1
    # Every root has to be flash before reading several at a time. Four is
    # measurably the best point: past it the threads spend more time waiting
    # on each other to parse than they save on the syscalls.
    if all_solid_state(roots):
        return min(4, (os.cpu_count() or 4))
    return 1


def read_jobs(roots: list[str], requested) -> int:
    """Concurrent readers. Flash likes queue depth; a spinning disk does not —
    parallel reads there just make the heads seek between files."""
    if requested is not None:
        return max(1, requested)
    if all_solid_state(roots):
        return min(8, (os.cpu_count() or 4))
    return 2


def file_ext(name: str) -> str:
    """The extension --ext/--skip-ext match on: the final suffix, lowercased,
    no dot. A dotfile with no other dot (.bashrc, .DS_Store) counts as having
    none — same as a name with no dot at all, and the same rule pathlib's
    .suffix uses."""
    stem, dot, ext = name.rpartition(".")
    if not dot or not stem:
        return ""
    return ext.lower()


def ext_allowed(name: str, include_ext, exclude_ext) -> bool:
    """Whether `name` survives --ext/--skip-ext. `include_ext` is None when
    no whitelist was given, so that stage always passes; a file with no
    extension fails a whitelist but can never match a blacklist."""
    ext = file_ext(name)
    if include_ext is not None and ext not in include_ext:
        return False
    return ext not in exclude_ext


def walk_files(roots: list[str], excludes: list[str], min_size: int,
               stats: ScanStats, on_progress=None, workers=None,
               include_ext: frozenset[str] | None = None,
               exclude_ext: frozenset[str] = frozenset()) -> list[FileRec]:
    """Collect every regular file under `roots`. Symlinks are never followed.

    Uses the bulk directory-metadata syscall where it's available, which is
    roughly twice as fast as stat()ing each file, and falls back to a plain
    scandir walk everywhere else. `workers` defaults to reading several
    directories at once on flash and one at a time on a spinning disk.
    """
    if bulk_available():
        return _walk_bulk(roots, excludes, min_size, stats, on_progress,
                          walk_workers(roots, workers), include_ext, exclude_ext)
    return _walk_scandir(roots, excludes, min_size, stats, on_progress,
                         include_ext, exclude_ext)


def _walk_scandir(roots: list[str], excludes: list[str], min_size: int,
                  stats: ScanStats, on_progress=None,
                  include_ext: frozenset[str] | None = None,
                  exclude_ext: frozenset[str] = frozenset()) -> list[FileRec]:
    """Portable walk: one stat() per file."""
    out: list[FileRec] = []
    stack = [os.path.abspath(r) for r in roots]
    last_tick = 0.0
    ext_filtering = include_ext is not None or exclude_ext

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
                if ext_filtering and not ext_allowed(name, include_ext, exclude_ext):
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
                               st.st_dev, st.st_ino, st.st_nlink,
                               mtime_ns=st.st_mtime_ns))

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


class DirReader:
    """Reads one directory per call with getattrlistbulk.

    Holds its own buffer, so each thread needs its own reader.
    """

    def __init__(self, excludes: list[str], min_size: int,
                include_ext: frozenset[str] | None = None,
                exclude_ext: frozenset[str] = frozenset()):
        import ctypes
        import struct

        class attrlist(ctypes.Structure):
            _fields_ = [("bitmapcount", ctypes.c_ushort), ("reserved", ctypes.c_uint16),
                        ("commonattr", ctypes.c_uint32), ("volattr", ctypes.c_uint32),
                        ("dirattr", ctypes.c_uint32), ("fileattr", ctypes.c_uint32),
                        ("forkattr", ctypes.c_uint32)]

        self._ctypes = ctypes
        self.excludes = excludes
        self.min_size = min_size
        self.include_ext = include_ext
        self.exclude_ext = exclude_ext
        self.attrs = attrlist(ATTR_BIT_MAP_COUNT, 0,
                              ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE,
                              0, 0, ATTR_FILE_DATALENGTH, 0)
        self.buf = ctypes.create_string_buffer(BULK_BUF)
        self.view = memoryview(self.buf).cast("B")
        self.u32 = struct.Struct("<I").unpack_from
        self.i64 = struct.Struct("<q").unpack_from

    def read(self, directory: str):
        """-> (files, subdirs, files_seen, bytes_seen, errors) for one directory."""
        ctypes = self._ctypes
        buf, view, u32, i64 = self.buf, self.view, self.u32, self.i64
        excludes, min_size = self.excludes, self.min_size
        include_ext, exclude_ext = self.include_ext, self.exclude_ext
        ext_filtering = include_ext is not None or exclude_ext

        files: list[FileRec] = []
        subdirs: list[str] = []
        errors: list[str] = []
        seen = nbytes = 0

        try:
            fd = os.open(directory, os.O_RDONLY | O_DIRECTORY)
        except OSError as e:
            return files, subdirs, seen, nbytes, [f"{directory}: {e}"]

        try:
            while True:
                count = _libc.getattrlistbulk(fd, ctypes.byref(self.attrs),
                                              buf, BULK_BUF, 0)
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
                    # everything else here, so files that can't qualify are
                    # dropped on the raw bytes first.
                    if excludes:
                        name_str = name.decode("utf-8", "surrogateescape")
                        if any(fnmatch.fnmatch(name_str, pat) for pat in excludes):
                            continue
                    else:
                        name_str = None

                    if objtype == VREG and ext_filtering:
                        if name_str is None:
                            name_str = name.decode("utf-8", "surrogateescape")
                        if not ext_allowed(name_str, include_ext, exclude_ext):
                            continue

                    if objtype == VREG:
                        seen += 1
                        nbytes += size
                        if size < min_size:
                            continue

                    if name_str is None:
                        name_str = name.decode("utf-8", "surrogateescape")
                    path = os.path.join(directory, name_str)
                    if objtype == VDIR:
                        subdirs.append(path)
                        continue
                    # Only survivors pay for a stat, and it keeps inode, link
                    # count and mtime exactly as the portable walk sees them.
                    try:
                        st = os.lstat(path)
                    except OSError as e:
                        errors.append(f"{path}: {e}")
                        continue
                    files.append(FileRec(path, st.st_size, st.st_mtime,
                                         st.st_dev, st.st_ino, st.st_nlink,
                                         mtime_ns=st.st_mtime_ns))
        except OSError as e:
            errors.append(f"{directory}: {e}")
        finally:
            os.close(fd)

        return files, subdirs, seen, nbytes, errors


def _walk_bulk(roots: list[str], excludes: list[str], min_size: int,
               stats: ScanStats, on_progress=None, workers: int = 1,
               include_ext: frozenset[str] | None = None,
               exclude_ext: frozenset[str] = frozenset()) -> list[FileRec]:
    if not bulk_available():
        return _walk_scandir(roots, excludes, min_size, stats, on_progress,
                             include_ext, exclude_ext)
    if workers > 1:
        return _walk_bulk_threaded(roots, excludes, min_size, stats,
                                   on_progress, workers, include_ext, exclude_ext)

    reader = DirReader(excludes, min_size, include_ext, exclude_ext)
    out: list[FileRec] = []
    stack = [os.path.abspath(r) for r in roots]
    last_tick = 0.0

    while stack:
        files, subdirs, seen, nbytes, errors = reader.read(stack.pop())
        out.extend(files)
        stack.extend(subdirs)
        stats.files_seen += seen
        stats.bytes_seen += nbytes
        stats.unreadable.extend(errors)

        if on_progress:
            now = time.monotonic()
            if now - last_tick > 0.1:
                on_progress(stats)
                last_tick = now

    return out


def _walk_bulk_threaded(roots: list[str], excludes: list[str], min_size: int,
                        stats: ScanStats, on_progress, workers: int,
                        include_ext: frozenset[str] | None = None,
                        exclude_ext: frozenset[str] = frozenset()) -> list[FileRec]:
    """Same walk, several directories at a time.

    Worth it on flash, where the queue depth is free. On a spinning disk the
    heads have to seek between the directories, so the caller decides.
    """
    import queue
    import threading

    pending = queue.Queue()
    for r in roots:
        pending.put(os.path.abspath(r))
    if pending.empty():
        return []                     # nothing queued, so nothing would ever
                                      # retire the counter that stops the threads

    out: list[FileRec] = []
    lock = threading.Lock()
    finished = threading.Event()
    last_tick = [0.0]
    # Directories queued but not yet processed. Every directory that will ever
    # exist is counted here before its parent is retired, so reaching zero
    # means the tree is genuinely exhausted — not merely momentarily idle.
    outstanding = [pending.qsize()]

    def work():
        reader = DirReader(excludes, min_size, include_ext, exclude_ext)
        # Results accumulate per thread and are merged once at the end; taking
        # the lock for every directory costs more than the parallelism wins.
        mine: list[FileRec] = []
        my_errors: list[str] = []
        while not finished.is_set():
            try:
                directory = pending.get(timeout=0.05)
            except queue.Empty:
                continue

            # Whatever happens, this directory has to be retired below, or the
            # counter never reaches zero and every thread spins forever. A
            # failed directory is worth reporting; a hung scan is not.
            try:
                files, subdirs, seen, nbytes, errors = reader.read(directory)
            except Exception as e:                       # noqa: BLE001
                files, subdirs, seen, nbytes = [], [], 0, 0
                errors = [f"{directory}: {e}"]
            mine.extend(files)
            if errors:
                my_errors.extend(errors)
            with lock:
                stats.files_seen += seen
                stats.bytes_seen += nbytes
                outstanding[0] += len(subdirs) - 1
                done = outstanding[0] == 0
                if on_progress:
                    now = time.monotonic()
                    if now - last_tick[0] > 0.1:
                        on_progress(stats)
                        last_tick[0] = now
            for d in subdirs:
                pending.put(d)
            if done:
                finished.set()
        with lock:
            out.extend(mine)
            stats.unreadable.extend(my_errors)

    threads = [threading.Thread(target=work, daemon=True) for _ in range(workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
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
        rec = by_inode.get(key)
        if rec is None:
            by_inode[key] = rec = f
            order.append(key)
        # Overlapping roots ("dedup . ." or a parent and its child) hand us the
        # same name twice. Keeping both would overstate the link count and make
        # the removal try to delete one file two times.
        if f.path not in rec.links:
            rec.links.append(f.path)
    for rec in by_inode.values():
        # Several names for one inode is normally a hardlink, and those are
        # genuinely separate directory entries that all have to go. But a
        # symlinked root can hand us two spellings of a single entry, and
        # deleting it twice would report a failure for work already done.
        # Only multi-name records pay for resolving this.
        if len(rec.links) > 1:
            distinct: dict = {}
            for p in rec.links:
                try:
                    real = os.path.realpath(p)
                except OSError:
                    real = p
                distinct.setdefault(real, p)
            rec.links = list(distinct.values())
        # shortest name first, so the record displays the tidiest path
        rec.links.sort(key=lambda p: (len(p), p))
        rec.path = rec.links[0]
    return [by_inode[k] for k in order]


# ─────────────────────────────────────────────────────────────────────────────
# hashing
# ─────────────────────────────────────────────────────────────────────────────

def _digest():
    """SHA-256 — every CPU this runs on has an instruction for it, which
    makes it about twice as fast here as BLAKE2b (3.0 vs 1.5 GB/s measured).

    These digests only narrow the field. Nothing is ever called a duplicate
    on the strength of a hash: stage 5 compares the bytes.
    """
    return hashlib.sha256()


def hash_head(path: str, size: int) -> bytes | None:
    try:
        with open(path, "rb") as fh:
            h = _digest()
            h.update(fh.read(HEAD_BYTES))
            return h.digest()
    except (PermissionError, OSError):
        return None


def hash_head_rest(path: str, size: int) -> bytes | None:
    """Hash HEAD_BYTES..DEEP_HEAD — the part of the head not already read.

    Only files that a one-page head left sitting in a bucket with someone
    else get this far, and they read the remainder rather than the head
    again, so no byte is read twice.
    """
    h = _digest()
    try:
        with open(path, "rb") as fh:
            fh.seek(HEAD_BYTES)
            h.update(fh.read(DEEP_HEAD - HEAD_BYTES))
        return h.digest()
    except (PermissionError, OSError):
        return None


def keep_head_rest(path: str, size: int) -> tuple | None:
    """hash_head_rest, and the block it read. Appended to what keep_ends kept,
    so the record carries one contiguous run of bytes from offset 0."""
    try:
        with open(path, "rb") as fh:
            fh.seek(HEAD_BYTES)
            block = fh.read(DEEP_HEAD - HEAD_BYTES)
    except (PermissionError, OSError):
        return None
    h = _digest()
    h.update(block)
    return h.digest(), block


def has_tail(size: int) -> bool:
    """Whether a file is long enough for the two end pages not to meet."""
    return size > DEEP_HEAD + TAIL_BYTES


def ends_bytes(size: int) -> int:
    """How much hash_ends reads — for honest byte accounting."""
    return min(size, HEAD_BYTES) + (TAIL_BYTES if has_tail(size) else 0)


def hash_ends(path: str, size: int) -> bytes | None:
    """SHA-256 one page from each end of the file, in one open.

    Same-size files that share a header are the ordinary case — one camera,
    one encoder, one backup format — and reading more of a header they have
    in common tells you nothing. The page at the other end is where they
    part, and asking for it costs a seek rather than another open: on the
    volumes this was measured on, opening a file is twenty times dearer than
    reading a page out of one already open.
    """
    h = _digest()
    try:
        with open(path, "rb") as fh:
            h.update(fh.read(HEAD_BYTES))
            if has_tail(size):
                fh.seek(size - TAIL_BYTES)
                h.update(read_block(fh, TAIL_BYTES))
        return h.digest()
    except (PermissionError, OSError):
        return None


def keep_ends(path: str, size: int) -> tuple | None:
    """hash_ends, and the blocks it read, for hash_stage to keep."""
    tail = b""
    try:
        with open(path, "rb") as fh:
            head = fh.read(HEAD_BYTES)
            if has_tail(size):
                fh.seek(size - TAIL_BYTES)
                tail = read_block(fh, TAIL_BYTES)
    except (PermissionError, OSError):
        return None
    h = _digest()
    h.update(head)
    h.update(tail)
    return h.digest(), head, tail


def sample_points(size: int) -> list:
    """Where stage 4 looks.

    Everything before DEEP_HEAD and everything from the tail page onward has
    already been read by stage 3 — every file that reaches stage 4 went
    through both — so the windows spread across what is left instead of
    across the whole file, and no byte is read twice.
    """
    first = DEEP_HEAD
    last = size - TAIL_BYTES - SAMPLE_BYTES
    step = (last - first) // (SAMPLE_POINTS - 1)
    return [first + i * step for i in range(SAMPLE_POINTS)]


def hash_sample(path: str, size: int) -> bytes | None:
    """Hash a few windows spread through the file instead of all of it.

    On a slow disk the full read is the whole cost of a scan, and most
    same-size files are not duplicates. Sampling settles those after a few
    hundred KB rather than after gigabytes. Survivors still get read in
    full, so the result stays byte-exact.

    A window can only separate two files if it lands on something they do
    not share, so what a sample is really buying is coverage of the file's
    length. Eight windows of 64 KiB cover eight places; four of a megabyte
    cover four, for eight times the reading. A difference small enough to
    fall between windows is missed either way, and stage 5 is what catches
    that.
    """
    if size <= SAMPLE_SPAN:
        return hash_full(path, size)
    h = _digest()
    h.update(str(size).encode())          # same samples, different length
    try:
        with open(path, "rb", buffering=0) as fh:
            for offset in sample_points(size):
                fh.seek(offset)
                block = read_block(fh, SAMPLE_BYTES)
                if not block:
                    break
                h.update(block)
        return h.digest()
    except (PermissionError, OSError):
        return None


def sampled_bytes(size: int) -> int:
    """How much hash_sample actually reads — for honest progress accounting."""
    return size if size <= SAMPLE_SPAN else SAMPLE_BYTES * SAMPLE_POINTS


def hash_full(path: str, size: int) -> bytes | None:
    h = _digest()
    try:
        with open(path, "rb", buffering=0) as fh:
            while True:
                block = fh.read(CHUNK)
                if not block:
                    break
                h.update(block)
        return h.digest()
    except (PermissionError, OSError):
        return None


def hash_stage(files: list[FileRec], fn, jobs: int, stats: ScanStats,
               on_progress=None, keep: tuple = (), cache=None) -> dict:
    """Hash `files` with `fn` and bucket them by size and digest.

    Size is part of the key because a head or sample digest covers only part
    of the file: two files of different lengths can easily agree on their
    first 64 KiB and never be duplicates.

    `keep` names the record attributes — "head", "tail" — that `fn`'s blocks
    are appended to, each paired with the length that run must already have
    for this block to continue it; `fn` then returns the blocks alongside the
    digest. Stages read each end strictly outward, so what a record ends up
    carrying is one contiguous run from offset 0 and one that ends at EOF.

    With a cache attached, files whose digest is already remembered are
    bucketed without being opened, and the ones that do get read are
    remembered on the way past. That is also what the paired length is for:
    a file this stage answered from memory was never read and carries
    nothing, so a later stage's block must not be glued to the front of the
    gap where the earlier one would have been. Stage 5 reads such a file from
    zero instead, which is slower and right.
    """
    buckets: dict = defaultdict(list)
    last_tick = 0.0

    field = CACHED_FIELD.get(fn) if cache is not None else None
    todo = files
    if field:
        todo = []
        for rec in files:
            digest = cache.get(rec, field)
            if digest is None:
                todo.append(rec)
                continue
            buckets[(rec.size, digest)].append(rec)
            stats.hashed_files += 1     # settled, and it cost nothing

    def batch(recs: list) -> list:
        return [fn(f.path, f.size) for f in recs]

    # Enough batches to keep every reader busy, but no smaller than that: on a
    # tree of a hundred thousand small files, one task per file costs more in
    # queueing than the read it is carrying.
    span = max(1, min(HASH_BATCH, -(-len(todo) // max(1, jobs * 4))))
    batches = [todo[i:i + span] for i in range(0, len(todo), span)]

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for recs, digests in zip(batches, pool.map(batch, batches)):
            for rec, digest in zip(recs, digests):
                if digest is None:
                    stats.unreadable.append(rec.path)
                    continue
                if keep:
                    blocks = digest[1:]
                    digest = digest[0]
                    for (attr, follows), block in zip(keep, blocks):
                        held = getattr(rec, attr)
                        if len(held) == follows:
                            setattr(rec, attr, held + block)
                buckets[(rec.size, digest)].append(rec)
                if field:
                    cache.remember(rec, field, digest)
                stats.hashed_files += 1
                if fn is hash_ends or fn is keep_ends:
                    stats.hashed_bytes += ends_bytes(rec.size)
                elif fn is hash_head:
                    stats.hashed_bytes += min(rec.size, HEAD_BYTES)
                elif fn is hash_head_rest or fn is keep_head_rest:
                    stats.hashed_bytes += max(
                        0, min(rec.size, DEEP_HEAD) - HEAD_BYTES)
                elif fn is hash_sample:
                    stats.hashed_bytes += sampled_bytes(rec.size)
                else:
                    stats.hashed_bytes += rec.size
                if on_progress:
                    now = time.monotonic()
                    if now - last_tick > 0.1:
                        on_progress(stats)
                        last_tick = now

    return buckets


# Which column of the cache a hashing stage fills. A stage's keeping and
# non-keeping forms read the same bytes and produce the same digest, so they
# share a column. Two functions are deliberately absent: hash_full, because
# the whole-file digest is only ever learned in stage 5 from bytes that were
# compared rather than merely hashed, and hash_head, which no stage calls now
# that stage 3 reads both ends at once — a digest of the first page is not a
# digest of the two end pages, and nothing should be able to confuse them.
CACHED_FIELD = {hash_ends: "ends", keep_ends: "ends",
                hash_head_rest: "deep", keep_head_rest: "deep",
                hash_sample: "sample"}


def compare_chunk(members: int, solid) -> int:
    """How much to read from each file per round.

    Flash likes small chunks — 4 MB measured fastest. A spinning disk pays a
    seek every time we move to the next file in the group, so it wants big
    ones. Either way a round holds one chunk per member, so a group with a
    lot of copies in it gets a smaller chunk to stay inside the budget.
    """
    return max(MIN_CHUNK, min(CHUNK if solid is True else SLOW_CHUNK,
                              COMPARE_BUDGET // max(1, members)))


def read_block(fh, size: int) -> bytes:
    """Read exactly `size` bytes, or whatever is left at the end of the file.

    An unbuffered read is one read(2), and read(2) is allowed to come back
    with less than it was asked for. Two identical files that happened to
    return different amounts would compare unequal and be split apart, so
    the loop matters — it's the difference between a missed duplicate and
    a found one.
    """
    parts, remaining = [], size
    while remaining > 0:
        block = fh.read(remaining)
        if not block:
            break
        parts.append(block)
        remaining -= len(block)
    return parts[0] if len(parts) == 1 else b"".join(parts)


def partition_blocks(blocks: list) -> list[list]:
    """Split (rec, block) pairs into runs that agree on this chunk.

    Comparing against a pivot rather than hashing each block is the point of
    the whole stage: a memcmp against a block that differs early costs
    essentially nothing next to a digest of the whole thing. That holds as
    long as each pass settles a decent share of what is left.

    It stops holding when a bucket is mostly distinct content — same size,
    same head, different bodies. Then every pass strips off just the pivot
    and the loop is quadratic in the number of files. When a pass does that
    to a bucket that is still large, finish the job by grouping on the block
    itself: one pass, and each block is looked at once.

    That stays byte-exact. A dict only puts two keys in one group when they
    are equal, and equality on bytes is a memcmp — the hash picks the slot,
    it never decides the answer. Nothing is called a duplicate here on the
    strength of a digest.
    """
    runs, pending = [], blocks
    while pending:
        pivot = pending[0]
        same, rest = [pivot], []
        for item in pending[1:]:
            (same if item[1] == pivot[1] else rest).append(item)
        runs.append(same)
        if len(same) == 1 and len(rest) > PARTITION_FANOUT:
            by_block: dict = {}
            for item in rest:
                by_block.setdefault(item[1], []).append(item)
            runs.extend(by_block.values())
            return runs
        pending = rest
    return runs


def split_identical(members: list, chunk: int, stats: ScanStats,
                    pool=None, tick=None, cache=None) -> list[list]:
    """Read every file in a group once, in step, and split them the moment
    their bytes differ.

    Two things make this cheaper than hashing each file and comparing digests.
    Files that turn out not to match stop being read at the byte where they
    diverge, instead of being read to the end. And comparing blocks costs
    almost nothing, where hashing them is the whole cost of a scan on a fast
    disk. It is also exact by construction — there is no digest left to
    collide, so nothing is deleted on the strength of a guess.
    """
    # A short file that stage 3 read to its end is already here in full.
    # Opening it again to read nothing costs an open — 22 µs against the 1.7
    # of the stat that says the file is still the one we read.
    if (len(members) > 1 and all(len(r.head) >= r.size for r in members)
            and unchanged(members)):
        settled = []
        for run in partition_blocks([(r, r.head) for r in members]):
            recs = [r for r, _ in run]
            stats.hashed_files += len(recs)
            if len(recs) > 1:
                settled.append(recs)
                if cache is not None:
                    # The whole file is in hand and it matched, so this is the
                    # same proof the reading path produces, learned for free.
                    for rec in recs:
                        h = _digest()
                        h.update(rec.head[:rec.size])
                        cache.remember(rec, "full", h.digest())
        return settled

    handles, opened = {}, []
    for rec in members:
        try:
            handles[id(rec)] = open(rec.path, "rb", buffering=0)
            opened.append(rec)
        except (PermissionError, OSError):
            stats.unreadable.append(rec.path)
    # The bytes are going past anyway, and SHA-256 runs at a few GB/s where a
    # disk does not, so a group read here is also a group learned. Only files
    # that reach the end still matching get their digest kept — one that
    # diverged early was never read in full.
    hashers = {id(rec): _digest() for rec in opened} if cache is not None else None
    # Files that wouldn't open are settled, and so is a lone survivor: count
    # them now or the progress bar stops short of the end.
    stats.hashed_files += len(members) - len(opened)

    # Whatever stage 3 kept is compared here instead of being read again —
    # the bytes themselves, not the digest of them, so the proof is the same
    # proof. Bytes on disk that have moved since they were read are not the
    # bytes we are about to delete something over, so the first stat that
    # disagrees puts the whole group back on reading from zero.
    prefix = min([len(r.head) for r in opened], default=0)
    tail = min([len(r.tail) for r in opened], default=0)
    if tail and any(prefix + tail > r.size for r in opened):
        tail = 0                        # the two kept runs would overlap
    if prefix or tail:
        for rec in opened:
            st = os.fstat(handles[id(rec)].fileno())
            if st.st_size != rec.size or st.st_mtime != rec.mtime:
                prefix = tail = 0
                break
    if prefix:
        for rec in opened:
            handles[id(rec)].seek(prefix)
    # Read up to where the kept tail begins; the tail settles the rest.
    limit = min([r.size - tail for r in opened], default=0) if tail else None
    # The digest being learned has to cover the file, not just the part of it
    # this call happens to read. Kept bytes are part of the file too, so they
    # go into the hasher in their place: head first, then everything read,
    # then the tail as each run finishes. A digest built any other way would
    # be remembered as a whole-file digest without being one, and the next
    # scan would match two files on their middles alone.
    if hashers is not None and prefix:
        for rec in opened:
            hashers[id(rec)].update(rec.head[:prefix])

    span = chunk

    def grab(rec):
        try:
            return rec, read_block(handles[id(rec)], span)
        except (PermissionError, OSError):
            return rec, None

    matched: list[list] = []
    try:
        if len(opened) < 2:
            live = []
            stats.hashed_files += len(opened)
        elif prefix:
            live = []
            for run in partition_blocks([(r, r.head[:prefix]) for r in opened]):
                recs = [r for r, _ in run]
                if len(recs) < 2:
                    stats.hashed_files += len(recs)   # settled on kept bytes
                else:
                    live.append(recs)
        else:
            live = [opened]
        pos = prefix
        while live:
            span = chunk if limit is None else min(chunk, limit - pos)
            if span <= 0:
                break               # everything up to the kept tail agrees
            pos += span
            following = []
            for group in live:
                blocks = list(pool.map(grab, group)) if pool else [grab(r) for r in group]
                for rec, block in blocks:
                    if block is None:
                        stats.unreadable.append(rec.path)
                        stats.hashed_files += 1
                blocks = [b for b in blocks if b[1] is not None]
                stats.hashed_bytes += sum(len(b) for _, b in blocks)
                if hashers is not None:
                    for rec, block in blocks:
                        hashers[id(rec)].update(block)

                for run in partition_blocks(blocks):
                    recs = [r for r, _ in run]
                    if len(recs) < 2:
                        stats.hashed_files += len(recs)   # settled, stop reading
                    elif run[0][1]:
                        following.append(recs)            # still together
                    else:
                        matched.append(recs)              # same bytes to EOF
                        stats.hashed_files += len(recs)
                        # Reaching EOF here means the read covered everything
                        # after the kept head. With a kept tail in hand the
                        # run stops short of EOF instead and is remembered
                        # below, once that tail has gone into the digest.
                        if hashers is not None and not tail:
                            for rec in recs:
                                cache.remember(rec, "full",
                                               hashers[id(rec)].digest())
                if tick:
                    tick()          # two huge files are one group and many
                                    # rounds; the bar has to move meanwhile
            live = following
        for group in live:          # only reached with a kept tail in hand
            for run in partition_blocks([(r, r.tail[-tail:]) for r in group]):
                recs = [r for r, _ in run]
                stats.hashed_files += len(recs)
                if len(recs) > 1:
                    matched.append(recs)        # same bytes to EOF
                    if hashers is not None:
                        for rec in recs:
                            h = hashers[id(rec)]
                            h.update(rec.tail[-tail:])
                            cache.remember(rec, "full", h.digest())
    finally:
        for fh in handles.values():
            fh.close()
    return matched


def unchanged(members: list) -> bool:
    """Whether every file is still the one whose bytes we kept.

    Kept bytes are only a proof of anything while the file on disk has not
    moved under us, and this tool deletes what it proves.
    """
    for rec in members:
        try:
            st = os.stat(rec.path)
        except (PermissionError, OSError):
            return False
        if st.st_size != rec.size or st.st_mtime != rec.mtime:
            return False
    return True


def release(files: list, pending: list) -> None:
    """Drop kept bytes for records no later stage will look at again."""
    live = {id(f) for g in pending for f in g}
    for f in files:
        if id(f) not in live:
            f.head = f.tail = b""


def compare_stage(buckets: list, jobs: int, solid, stats: ScanStats,
                  on_progress=None, cache=None) -> list[list]:
    """Run split_identical over every candidate group.

    Groups go one at a time with their members read in parallel, rather than
    the other way round: a folder whose duplicates are two enormous files is
    one group, and it should still use the whole disk queue.
    """
    out: list[list] = []
    last_tick = [0.0]

    def tick() -> None:
        if not on_progress:
            return
        now = time.monotonic()
        if now - last_tick[0] > 0.1:
            on_progress(stats)
            last_tick[0] = now

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for members in buckets:
            chunk = compare_chunk(len(members), solid)
            out.extend(split_identical(members, chunk, stats,
                                       pool if jobs > 1 else None, tick, cache))
    return out


# ─────────────────────────────────────────────────────────────────────────────
# the re-scan cache
# ─────────────────────────────────────────────────────────────────────────────
#
# Scanning the same volume twice reads the same bytes twice, and on a large
# disk that reading is the whole cost. Remembering each file's digests against
# (inode, size, mtime_ns) lets the narrowing stages skip everything nothing has
# touched since last time.
#
# The rule that makes this safe to ship in a deletion tool: the cache may only
# ever NARROW. A remembered digest can save a read. It can never be the reason
# a file is removed. Every group matched from memory is read and compared
# before anything in it is deleted — see verify_attested, which is the other
# half of this feature and not optional.

# Which slot of a stored entry each digest lives in. One column per set of
# bytes a stage actually reads: "ends" is the two end pages hashed together,
# "deep" the rest of the head behind them, "sample" the windows stage 4
# spreads over what is left, "full" the whole file.
CACHE_COLUMNS = {"ends": 2, "deep": 3, "sample": 4, "full": 5}


def cache_home() -> str:
    """Where the per-volume databases live."""
    override = os.environ.get("DEDUP_CACHE_DIR")
    if override:
        return override
    if sys.platform == "darwin":
        return os.path.expanduser("~/Library/Caches/dedup")
    base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache")
    return os.path.join(base, "dedup")


def volume_id(path: str) -> str:
    """A stable filename for the cache belonging to the volume `path` is on.

    The UUID survives a remount under a different name, which a mount point
    does not. Where there is no UUID to be had the mount point is all there
    is, and guessing wrong only ever costs one cold scan.
    """
    root = mount_point(path) or "/"
    if sys.platform == "darwin":
        try:
            import plistlib
            import subprocess
            out = subprocess.run(["diskutil", "info", "-plist", root],
                                 capture_output=True, timeout=5)
            if out.returncode == 0:
                uuid = plistlib.loads(out.stdout).get("VolumeUUID")
                if isinstance(uuid, str) and uuid:
                    return uuid
        except Exception:                       # noqa: BLE001 - fall back, never fail
            pass
    return hashlib.sha256(os.fsencode(root)).hexdigest()[:32]


class ScanCache:
    """Digests remembered from earlier scans, one sqlite file per volume.

    Rows are keyed by inode, which is exactly what drop_hardlinks collapses a
    file's names down to: one entry serves every name an inode wears. A row
    only applies while its size and mtime_ns still match, so an edited file is
    a miss and gets read.

    Forging an mtime defeats the key, and that is allowed to be true, because
    nothing here is trusted at deletion time.
    """

    def __init__(self, directory: str | None = None):
        self.directory = directory or cache_home()
        self.paths: dict = {}      # dev -> database file
        self.entries: dict = {}    # dev -> {ino: [size, mtime_ns, head, sample, full]}
        self.touched: dict = {}    # dev -> inodes whose row changed
        self.seen: dict = {}       # dev -> inodes this scan recognised
        self.warned = False

    # ── opening ──────────────────────────────────────────────────────────
    def _warn(self, message: str) -> None:
        """Say it once. An unusable cache is a slow scan, not a failed one."""
        if self.warned:
            return
        self.warned = True
        print(f"dedup: {message}; reading everything and rebuilding it",
              file=sys.stderr)

    @staticmethod
    def _prepare(con) -> None:
        con.execute("CREATE TABLE IF NOT EXISTS files ("
                    "ino INTEGER PRIMARY KEY, size INTEGER NOT NULL, "
                    "mtime_ns INTEGER NOT NULL, ends BLOB, deep BLOB, "
                    "sample BLOB, full BLOB, last_seen INTEGER NOT NULL)")
        con.execute(f"PRAGMA user_version = {CACHE_SCHEMA}")

    def _read(self, dev: int, db: str) -> None:
        con = sqlite3.connect(db)
        try:
            if con.execute("PRAGMA user_version").fetchone()[0] != CACHE_SCHEMA:
                # Another version's columns do not mean what this one thinks
                # they mean. Start over rather than interpret them.
                con.execute("DROP TABLE IF EXISTS files")
                self._prepare(con)
                con.commit()
                return
            self._prepare(con)
            for row in con.execute("SELECT ino, size, mtime_ns, ends, deep, "
                                   "sample, full FROM files"):
                self.entries[dev][row[0]] = list(row[1:])
        finally:
            con.close()

    def _load(self, dev: int, on_volume: str) -> None:
        if dev in self.entries:
            return
        self.entries[dev] = {}
        self.touched[dev] = set()
        self.seen[dev] = set()
        db = None
        try:
            os.makedirs(self.directory, exist_ok=True)
            db = os.path.join(self.directory, volume_id(on_volume) + ".db")
            self.paths[dev] = db
            self._read(dev, db)
        except (OSError, sqlite3.Error) as e:
            self._warn(f"the scan cache could not be read ({e})")
            self.entries[dev] = {}
            # A file sqlite cannot make sense of is not worth repairing. Clear
            # the way so close() can lay down a fresh one in its place.
            try:
                if db:
                    os.unlink(db)
            except OSError:
                self.paths.pop(dev, None)       # nowhere to write either

    def open_paths(self, paths: list[str]) -> None:
        """Open the cache for the volumes these paths sit on."""
        for path in paths:
            try:
                self._load(os.stat(path).st_dev, path)
            except OSError:
                continue

    def open_for(self, records: list) -> None:
        """...and for any further volume the scan turned out to reach into."""
        for rec in records:
            if rec.dev not in self.entries:
                self._load(rec.dev, rec.path)

    # ── remembering ──────────────────────────────────────────────────────
    def get(self, rec: FileRec, field: str):
        """The remembered digest for this file, or None if there is not one."""
        entry = self.entries.get(rec.dev, {}).get(rec.ino)
        if entry is None or entry[0] != rec.size or entry[1] != rec.mtime_ns:
            return None
        self.seen[rec.dev].add(rec.ino)
        return entry[CACHE_COLUMNS[field]]

    def remember(self, rec: FileRec, field: str, digest: bytes) -> None:
        volume = self.entries.get(rec.dev)
        if volume is None:
            return
        entry = volume.get(rec.ino)
        if entry is None or entry[0] != rec.size or entry[1] != rec.mtime_ns:
            entry = volume[rec.ino] = [rec.size, rec.mtime_ns,
                                       None, None, None, None]
        entry[CACHE_COLUMNS[field]] = digest
        self.touched[rec.dev].add(rec.ino)

    def forget(self, records: list) -> None:
        """Drop everything remembered about these files.

        Called when a group we vouched for turned out not to match, so the
        wrong answer cannot survive into the next scan.
        """
        for rec in records:
            volume = self.entries.get(rec.dev)
            if volume is not None and volume.pop(rec.ino, None) is not None:
                self.touched[rec.dev].add(rec.ino)
                self.seen[rec.dev].discard(rec.ino)

    def attested_runs(self, groups: list) -> tuple:
        """Split stage-5 candidates into (to compare, matched from memory).

        A group only goes the fast way when every member's whole-file digest is
        remembered. One unknown member and the whole group is read as usual,
        which re-learns the others on the way through.
        """
        pending, matched = [], []
        for members in groups:
            digests = [self.get(rec, "full") for rec in members]
            if any(d is None for d in digests):
                pending.append(members)
                continue
            by_digest: dict = defaultdict(list)
            for rec, digest in zip(members, digests):
                by_digest[digest].append(rec)
            for run in by_digest.values():
                if len(run) > 1:
                    matched.append(run)
        return pending, matched

    # ── closing ──────────────────────────────────────────────────────────
    def close(self) -> None:
        """Write back what changed and drop what has not been seen in months.

        Files that were recognised but not re-read have their last_seen moved
        forward too, or a tree nobody edits would eventually age out of its
        own cache and pay for a cold scan it never earned.
        """
        now = int(time.time())
        cutoff = now - CACHE_TTL_DAYS * 86400
        for dev, db in self.paths.items():
            volume = self.entries.get(dev, {})
            writing = self.touched.get(dev, set()) | self.seen.get(dev, set())
            rows, gone = [], []
            for ino in writing:
                entry = volume.get(ino)
                if entry is None:
                    gone.append((ino,))
                else:
                    rows.append((ino, entry[0], entry[1], entry[2], entry[3],
                                 entry[4], entry[5], now))
            try:
                con = sqlite3.connect(db)
                try:
                    self._prepare(con)
                    con.executemany(
                        "INSERT OR REPLACE INTO files "
                        "(ino, size, mtime_ns, ends, deep, sample, full, "
                        "last_seen) VALUES (?, ?, ?, ?, ?, ?, ?, ?)", rows)
                    con.executemany("DELETE FROM files WHERE ino = ?", gone)
                    con.execute("DELETE FROM files WHERE last_seen < ?", (cutoff,))
                    con.commit()
                finally:
                    con.close()
            except (OSError, sqlite3.Error) as e:
                self._warn(f"the scan cache could not be written ({e})")
        self.paths.clear()
        self.entries.clear()
        self.touched.clear()
        self.seen.clear()


def verify_group(group: DupGroup, solid=None, cache=None) -> bool:
    """Read a remembered group's members and confirm the bytes really agree.

    A group compared during this scan is already proved and is not read again.
    Anything else has only a digest behind it, and a digest is not what this
    tool deletes on.
    """
    if not group.attested:
        return True
    stats = ScanStats()
    members = list(group.files)
    runs = split_identical(members, compare_chunk(len(members), solid), stats)
    if len(runs) == 1 and len(runs[0]) == len(members) and not stats.unreadable:
        group.attested = False              # proved, here, now
        return True
    if cache is not None:
        cache.forget(members)
    return False


def verify_attested(groups: list, solid=None, cache=None) -> tuple:
    """Byte-compare every group matched from memory rather than read.

    Returns (safe to act on, refused). Nothing from a refused group may be
    deleted: its members disagree, so whatever was remembered of them was
    wrong, and the tool has no idea which copy is which.
    """
    safe, refused = [], []
    for group in groups:
        (safe if verify_group(group, solid, cache) else refused).append(group)
    return safe, refused


# ─────────────────────────────────────────────────────────────────────────────
# pipeline
# ─────────────────────────────────────────────────────────────────────────────

def find_duplicates(roots: list[str], *, excludes: list[str], min_size: int,
                    jobs: int, stats: ScanStats, verify: bool = False,
                    reporter=None, walkers=None, cache=None,
                    include_ext: frozenset[str] | None = None,
                    exclude_ext: frozenset[str] = frozenset()) -> list[DupGroup]:
    """Run the five-stage narrowing described in the module docstring.

    `verify` is accepted so older commands keep working. It no longer changes
    anything: the last stage compares bytes, so every answer is byte-exact.

    With no `cache`, this reads exactly what it always read. With one, the
    stages skip files that have not changed since a previous scan, and the
    groups they produce carry `attested` — a promise that still has to be
    kept before anything is deleted.
    """
    def say(key: str, total: int = 0) -> None:
        if reporter:
            reporter.stage(key, total, stats)

    tick = reporter.tick if reporter else None

    if cache is not None:
        cache.open_paths(roots)

    say("scan")
    files = walk_files(roots, excludes, min_size, stats, tick, walkers,
                       include_ext, exclude_ext)
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

    if cache is not None:
        cache.open_for(candidates)

    # Read in the order the volume laid the files down, not the order they
    # happened to be grouped in. On a spinning disk that turns a stage into
    # one sweep across the platter instead of a jump per file.
    def by_layout(f: FileRec):
        return (f.dev, f.ino)

    candidates.sort(key=by_layout)

    # Stage 5 has to see the bytes of the head to prove a group, and stage 3
    # is about to read them. Keeping them means stage 5 compares what is
    # already in hand instead of reading the same region a second time — a
    # third of the typical corpus. The cost is holding them until then, so
    # this only happens when the whole set fits in RETAIN_BUDGET: a hundred
    # thousand small files do not fit, and would gain nothing anyway, since
    # hardly any of them reach stage 5.
    keep = ((("head", 0), ("tail", 0))
            if sum(ends_bytes(f.size) for f in candidates) <= RETAIN_BUDGET
            else ())

    # stage 3 — one page from each end, in one open. The far page is where
    # same-size files that share a header part company, and reading more of a
    # header they agree on settles nothing: on the samesize corpus this page
    # settles what 86 MB of deeper head could not. It rides along with the
    # head because a second pass over those files would cost an open each,
    # and an open is twenty times a page read on the volumes measured here.
    say("head", len(candidates))
    end_buckets = hash_stage(candidates, keep_ends if keep else hash_ends,
                             jobs, stats, tick, keep, cache)
    if reporter:
        reporter.finish_stage()

    pending = [g for g in end_buckets.values() if len(g) > 1]
    if keep:
        release(candidates, pending)
    if not pending:
        return []

    # One page settles most candidates outright. Whatever it left sitting in
    # a bucket with someone else reads the rest of the deep head — only those
    # files pay for it, and only for the part not already read. A file no
    # longer than the first pass has nothing left to read and skips this.
    deep = [g for g in pending if g[0].size > HEAD_BYTES]
    if deep:
        pending = [g for g in pending if g[0].size <= HEAD_BYTES]
        crowded = sorted((f for g in deep for f in g), key=by_layout)
        keep_rest = ((("head", HEAD_BYTES),)
                     if keep and sum(min(f.size, DEEP_HEAD) - HEAD_BYTES
                                     for f in crowded) <= RETAIN_BUDGET
                     else ())
        say("head2", len(crowded))
        rest_buckets = hash_stage(
            crowded, keep_head_rest if keep_rest else hash_head_rest,
            jobs, stats, tick, keep_rest, cache)
        if reporter:
            reporter.finish_stage()
        pending += [g for g in rest_buckets.values() if len(g) > 1]
        if keep:
            release(crowded, pending)
        if not pending:
            return []

    # stage 4 — sample points spread through the file. Cheap way to settle
    # same-size files that only agree on their first 64 KiB.
    #
    # Only files big enough to be worth sampling go through it. Below
    # SAMPLE_SPAN there is nothing to sample: hash_sample reads the whole
    # file, and then stage 5 reads the whole file again. Every bucket holds
    # one size, so a bucket is entirely on one side of the line.
    small = [g for g in pending if g[0].size <= SAMPLE_SPAN]
    big = [g for g in pending if g[0].size > SAMPLE_SPAN]
    if big:
        survivors = sorted((f for g in big for f in g), key=by_layout)
        say("sample", len(survivors))
        sample_buckets = hash_stage(survivors, hash_sample, jobs, stats, tick,
                                    (), cache)
        if reporter:
            reporter.finish_stage()
        big = [g for g in sample_buckets.values() if len(g) > 1]
    pending = small + big
    if not pending:
        return []

    # stage 5 — compare what's left byte for byte, in step. Sampled agreement
    # is not proof and this tool deletes things, so the bytes get read; doing
    # it as a comparison rather than a hash means files that turn out to
    # differ stop being read where they diverge.
    #
    # A group every one of whose members carries a remembered whole-file digest
    # is set aside instead: it is reported, marked attested, and read later —
    # at deletion time, where being sure actually matters.
    remembered: list = []
    if cache is not None:
        pending, remembered = cache.attested_runs(pending)

    pending.sort(key=lambda g: min(by_layout(f) for f in g))
    say("compare", sum(len(g) for g in pending))
    matched = compare_stage(pending, jobs, all_solid_state(roots), stats, tick,
                            cache)
    if reporter:
        reporter.finish_stage()

    groups: list[DupGroup] = []
    for members, attested in [(m, False) for m in matched] + \
                             [(m, True) for m in remembered]:
        members.sort(key=lambda f: (len(f.path), f.path))
        groups.append(DupGroup(members[0].size, members, attested))

    # The keeper's path is the last word: size and basename can both tie, and
    # without a third key the order falls back to however the pipeline
    # happened to emit the groups — which traces back to inode allocation, so
    # the same tree scanned twice could report in a different order.
    groups.sort(key=lambda g: (-g.size, g.name, g.files[0].path))
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
        "sample": "Sampling deep",
        "head": "Sampling",
        "head2": "Sampling further",
        "compare": "Comparing",
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
    def __init__(self, groups: list[DupGroup], root_label: str, hard: bool,
                 cache=None, solid=None):
        self.groups = groups
        self.root_label = root_label
        self.hard = hard
        self.cache = cache
        self.solid = solid
        self.gi = 0
        self.fi = 0
        self.marked: set[tuple[int, int]] = set()
        self.show_help = False
        self.flash = ""
        self.applied_files = 0
        self.applied_bytes = 0
        self.failures: list[str] = []
        self.refused: list[DupGroup] = []

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

        self._overlay(win, "Working", ["Checking and moving files…"], "")
        self.apply_marks()
        if self.refused:
            self.flash = (f"{human_count(len(self.refused))} group(s) refused — "
                          "their copies no longer match")
        else:
            self.flash = f"moved {human_count(self.applied_files)} files"
        return not self.groups

    def apply_marks(self) -> int:
        """Remove every marked file it is safe to remove; return how many.

        Groups matched from the cache are compared here, at the last possible
        moment, because this is the moment it matters: the bytes have to agree
        in this process, on this run, before anything is taken away.
        """
        blocked = set()
        for gi in sorted({g for (g, _) in self.marked}):
            group = self.groups[gi]
            if not verify_group(group, self.solid, self.cache):
                blocked.add(gi)
                self.refused.append(group)

        remover = delete_permanently if self.hard else send_to_trash
        removed: set[tuple[int, int]] = set()
        for (gi, fi) in sorted(self.marked):
            if gi in blocked:
                continue
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
                rebuilt.append(DupGroup(group.size, keep, group.attested))
        self.groups = rebuilt
        self.marked.clear()
        self.gi = min(self.gi, max(0, len(self.groups) - 1))
        self.fi = 0
        return len(removed)

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


def report_refusals(refused: list) -> None:
    """Say plainly that some groups were not what the cache thought they were."""
    if not refused:
        return
    print(f"\n  Refusing to touch {human_count(len(refused))} group(s): their "
          f"copies no longer match.", file=sys.stderr)
    for group in refused[:10]:
        print(f"    {group.files[0].display} "
              f"(+{len(group.files) - 1} more)", file=sys.stderr)
    print("  Nothing was removed from them, and what was remembered about "
          "them has been discarded.", file=sys.stderr)


def run_auto(groups: list[DupGroup], rule: str, hard: bool, assume_yes: bool,
             cache=None, solid=None) -> int:
    """Non-interactive cleanup: keep one copy per group by `rule`, remove the rest."""
    # Groups matched from the cache have not been read yet. They are read here,
    # before a plan is even printed, so the plan describes files that really
    # are identical rather than files a digest said were.
    groups, refused = verify_attested(groups, solid, cache)
    report_refusals(refused)

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


def print_no_duplicates(stats: ScanStats, min_size: int, plain: bool = False) -> None:
    """Say so properly.

    After a long scan a single quiet line reads like the program died, and
    the usual reason for an empty result — the size floor — is invisible.
    """
    if plain:
        print("\n  No duplicates found.\n")
        return

    rule = "─" * 58
    print(f"\n  {rule}")
    print("   ✓  No duplicates found")
    print(f"  {rule}")
    print(f"\n     Checked {human_count(stats.files_seen)} files "
          f"({human_size(stats.bytes_seen)}).")
    print("     Every one of them is unique.\n")
    if min_size > 1:
        print(f"     Files under {human_size(min_size)} were skipped.")
        print("     Run with  --min-size 1  to include them.\n")


def print_report(groups: list[DupGroup], limit: int | None = None) -> None:
    total = sum(g.reclaimable for g in groups)
    print(f"\n  {human_count(len(groups))} duplicate groups · "
          f"{human_size(total)} reclaimable\n")
    attested = sum(1 for g in groups if g.attested)
    if attested:
        print(f"  {human_count(len(groups) - attested)} byte-compared just now · "
              f"{human_count(attested)} remembered from an earlier scan")
        print("  Remembered groups are re-read and verified before anything "
              "in them is deleted.\n")
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

def normalize_ext(raw: str) -> str:
    """One --ext/--skip-ext token: strip whitespace, drop an optional leading
    dot, lowercase. 'jpg', '.jpg', and 'JPG' all normalize the same."""
    token = raw.strip()
    if token.startswith("."):
        token = token[1:]
    return token.lower()


def resolve_ext_filters(ext_args: list[str], skip_ext_args: list[str]):
    """Turn repeated, comma-splittable --ext/--skip-ext arguments into
    (include_ext, exclude_ext). include_ext is None when --ext was never
    given — no whitelist, everything passes that stage. Raises ValueError on
    an empty token or an extension named by both flags."""
    def collect(raw_args: list[str], flag: str) -> frozenset[str]:
        tokens: set[str] = set()
        for raw in raw_args:
            for piece in raw.split(","):
                ext = normalize_ext(piece)
                if not ext:
                    raise ValueError(f"{flag}: empty extension in {raw!r}")
                tokens.add(ext)
        return frozenset(tokens)

    include_ext = collect(ext_args, "--ext") if ext_args else None
    exclude_ext = collect(skip_ext_args, "--skip-ext")

    if include_ext is not None:
        conflict = sorted(include_ext & exclude_ext)
        if conflict:
            raise ValueError(
                "in both --ext and --skip-ext: " + ", ".join(conflict))

    return include_ext, exclude_ext


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
    p.add_argument("-j", "--jobs", type=int, default=None,
                   help="concurrent readers (default: 8 on an SSD, 2 on a "
                        "spinning disk, where parallel reads only cause seeking)")
    p.add_argument("-p", "--parallel", type=int, default=None, metavar="N",
                   help="directories to scan at once (default: several on an "
                        "SSD, one on a spinning disk; -p 1 forces one)")
    p.add_argument("--min-size", type=int, default=10 * 1024 * 1024, metavar="BYTES",
                   help="ignore files smaller than this (default: 10485760, 10 MB)")
    p.add_argument("-x", "--exclude", action="append", default=[], metavar="GLOB",
                   help="skip names matching GLOB (repeatable)")
    p.add_argument("--ext", action="append", default=[], metavar="LIST",
                   help="only scan these extensions, comma-separated (repeatable)")
    p.add_argument("--skip-ext", action="append", default=[], metavar="LIST",
                   help="skip these extensions, comma-separated (repeatable)")
    p.add_argument("--verify", action="store_true",
                   help="no-op; matches are always byte-compared now")
    p.add_argument("--no-cache", action="store_true",
                   help="read every file, remember nothing (default is to "
                        "reuse digests from earlier scans of this volume)")
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

    try:
        include_ext, exclude_ext = resolve_ext_filters(args.ext, args.skip_ext)
    except ValueError as e:
        print(f"dedup: {e}", file=sys.stderr)
        return 2

    stats = ScanStats()
    reporter = Reporter(enabled=not args.quiet)
    started = time.monotonic()
    cache = None if args.no_cache else ScanCache()
    try:
        return _scan_and_act(args, stats, reporter, started, cache,
                             include_ext, exclude_ext)
    finally:
        if cache is not None:
            cache.close()


def _scan_and_act(args, stats: ScanStats, reporter, started: float, cache,
                  include_ext, exclude_ext) -> int:
    try:
        groups = find_duplicates(
            args.paths,
            excludes=args.exclude,
            min_size=args.min_size,
            jobs=read_jobs(args.paths, args.jobs),
            verify=args.verify,
            stats=stats,
            reporter=reporter,
            walkers=args.parallel,
            cache=cache,
            include_ext=include_ext,
            exclude_ext=exclude_ext,
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
        print_no_duplicates(stats, args.min_size, plain=args.list or not sys.stdout.isatty())
        return 0

    solid = all_solid_state(args.paths)
    attested = sum(1 for g in groups if g.attested)
    if attested and not args.list and not args.quiet:
        print(f"  {human_count(attested)} of {human_count(len(groups))} groups "
              f"came from the cache; they are read and compared again before "
              f"anything in them is deleted.", file=sys.stderr)

    if args.auto:
        return run_auto(groups, args.auto, args.hard, args.yes, cache, solid)

    if args.list or not sys.stdout.isatty():
        print_report(groups)
        return 0

    browser = Browser(groups, os.path.abspath(args.paths[0]), args.hard,
                      cache, solid)

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
    report_refusals(browser.refused)
    if browser.failures:
        print(f"  {human_count(len(browser.failures))} could not be removed:",
              file=sys.stderr)
        for line in browser.failures[:10]:
            print(f"    {line}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
