#!/usr/bin/env python3
"""One measurement, in a process of its own.

Loads a named dedup.py, runs one scan over one corpus, and prints a JSON
object describing what happened. A fresh process per measurement keeps the
solid-state cache, the import cost and the peak RSS from one run out of the
next one.

The byte count here is taken by wrapping builtins.open and adding up what
.read() actually hands back, not by asking the tool. A tool that reports its
own effort is marking its own homework, and the whole point of this harness is
that it cannot.
"""
from __future__ import annotations

import argparse
import builtins
import importlib.util
import json
import os
import resource
import sys
import time
import traceback

# Taken before anything else happens, so the harness can see how much of the
# process went on outside the timed call.
STARTED = time.perf_counter()


class _Counted:
    """A file wrapper that adds up the bytes read through it.

    Everything except read() goes straight to the real file, so seeks and
    closes behave exactly as they did before.
    """

    __slots__ = ("_f", "_tally")

    def __init__(self, fh, tally):
        self._f = fh
        self._tally = tally

    def read(self, *args):
        block = self._f.read(*args)
        self._tally[0] += len(block)
        return block

    def __enter__(self):
        self._f.__enter__()
        return self

    def __exit__(self, *exc):
        return self._f.__exit__(*exc)

    def __iter__(self):
        return iter(self._f)

    def __getattr__(self, name):
        return getattr(self._f, name)


def instrument(tree: str, volume: str):
    """Replace builtins.open. Returns (tally, tripwire, restore)."""
    real_open = builtins.open
    tally = [0]
    tripwire = []
    tree_prefix = os.path.join(tree, "")
    volume_prefix = os.path.join(volume, "")

    def counting_open(file, *args, **kwargs):
        fh = real_open(file, *args, **kwargs)
        if isinstance(file, (str, bytes)):
            path = file if isinstance(file, str) else os.fsdecode(file)
            if not path.startswith(os.sep):
                path = os.path.abspath(path)
            if path.startswith(tree_prefix):
                return _Counted(fh, tally)
            # Anything else on the corpus volume is off limits — the manifest
            # sits there, and a tool that reads the answer key is not scanning.
            if path.startswith(volume_prefix):
                tripwire.append(path)
        return fh

    builtins.open = counting_open

    def restore():
        builtins.open = real_open

    return tally, tripwire, restore


def load_dedup(path: str):
    """Import a dedup.py from an explicit file, whatever else is on sys.path."""
    spec = importlib.util.spec_from_file_location("dedup_under_test", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load {}".format(path))
    mod = importlib.util.module_from_spec(spec)
    sys.modules["dedup_under_test"] = mod
    spec.loader.exec_module(mod)
    return mod


def sabotage(mod, mode: str):
    """Deliberate damage, applied to the loaded module only.

    These exist so the harness can prove its gates fire. Nothing on disk is
    touched, so a sabotaged run cannot leak into a real one.
    """
    if mode == "truncate-compare":
        # Drop the last byte of every block stage 5 compares. Files that differ
        # only in their final byte then look identical and are wrongly merged.
        real = mod.read_block

        def read_block(fh, size):
            block = real(fh, size)
            return block[:-1] if block else block

        mod.read_block = read_block
    elif mode != "underreport-bytes":
        raise SystemExit("unknown sabotage: {}".format(mode))


def relative(path: str, tree: str) -> str:
    try:
        rel = os.path.relpath(path, tree)
    except ValueError:
        return path
    return path if rel.startswith("..") else rel


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dedup", required=True)
    ap.add_argument("--root", required=True, help="the tree/ directory to scan")
    ap.add_argument("--volume", required=True, help="corpus volume, for the tripwire")
    ap.add_argument("--min-size", type=int, required=True)
    ap.add_argument("--sabotage", default=None)
    ap.add_argument("--cache-dir", default=None,
                    help="scan with the re-scan cache kept here. Without it "
                         "the scan gets no cache at all, which is how every "
                         "scored scenario is measured.")
    args = ap.parse_args()

    tree = os.path.abspath(args.root)
    out = {"ok": False}
    try:
        mod = load_dedup(args.dedup)
        if args.sabotage:
            sabotage(mod, args.sabotage)

        # main() settles the reader count before it starts timing, and asking
        # costs a diskutil subprocess, so it happens here too — outside the
        # clock, and with the answer cached for the scan itself.
        jobs = mod.read_jobs([tree], None)
        solid = mod.is_solid_state(tree)

        # No --cache-dir means no cache object at all, and the keyword is not
        # even passed — so this drives an older dedup.py that has never heard
        # of a cache exactly as it always did.
        cache = None
        extra = {}
        if args.cache_dir:
            if not hasattr(mod, "ScanCache"):
                raise RuntimeError("{} has no ScanCache".format(args.dedup))
            cache = mod.ScanCache(args.cache_dir)
            extra["cache"] = cache

        stats = mod.ScanStats()
        tally, tripwire, restore = instrument(tree, os.path.abspath(args.volume))
        try:
            started = time.perf_counter()
            groups = mod.find_duplicates(
                [tree], excludes=[], min_size=args.min_size, jobs=jobs,
                stats=stats, verify=False, reporter=None, walkers=None, **extra
            )
            if cache is not None:
                cache.close()      # writing it back is part of the scan's cost
            seconds = time.perf_counter() - started
        finally:
            restore()

        reported = stats.hashed_bytes
        if args.sabotage == "underreport-bytes":
            reported //= 2

        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        out = {
            "ok": True,
            "seconds": seconds,
            "setup_seconds": started - STARTED,
            "bytes_instrumented": tally[0],
            "bytes_reported": reported,
            "maxrss_bytes": rss if sys.platform == "darwin" else rss * 1024,
            "jobs": jobs,
            "solid_state": solid,
            "files_seen": stats.files_seen,
            "bytes_seen": stats.bytes_seen,
            "hashed_files": stats.hashed_files,
            "unreadable": [relative(p, tree) for p in stats.unreadable],
            "tripwire": tripwire,
            "groups": [
                {
                    "size": g.size,
                    # False on any dedup.py that predates the cache, which is
                    # exactly what it means there: everything was read.
                    "attested": bool(getattr(g, "attested", False)),
                    "members": [
                        {"names": [relative(n, tree) for n in rec.all_names],
                         "deletable": rec.deletable}
                        for rec in g.files
                    ],
                }
                for g in groups
            ],
        }
    except BaseException as e:                       # noqa: BLE001 - report, never crash
        out = {"ok": False, "error": "{}: {}".format(type(e).__name__, e),
               "traceback": traceback.format_exc()}

    json.dump(out, sys.stdout)
    sys.stdout.write("\n")
    return 0 if out["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
