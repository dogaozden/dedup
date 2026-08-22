#!/usr/bin/env python3
"""The oracle, wearing the conformance protocol.

Loads the repo's dedup.py, runs one scan over one tree with the flags it was
given, and writes the protocol JSON that REWRITE.md §2 asks every
implementation to produce. It is both the reference answer the referee compares
against and, when pointed at itself, a candidate — which is how the referee's
own gates are proved to have teeth.

The byte and open counts here are taken by wrapping builtins.open and adding up
what .read() actually hands back, the same way bench/_measure.py does it, not by
asking the tool. A tool that reports its own effort is marking its own homework.
The candidate's numbers are self-reported by necessity — it is a foreign binary
— which is why the referee also holds them against a floor derived from the
tree itself.
"""
from __future__ import annotations

import argparse
import builtins
import importlib.util
import json
import os
import sys
import traceback

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


def instrument(root: str, home: str):
    """Replace builtins.open. Returns (tally, opens, tripwire, restore).

    `opens` counts file opens inside the scanned tree, which is the number the
    protocol asks for. Directory opens are not counted: the bulk-metadata walk
    opens each directory to read it, an implementation detail both the oracle
    and the port share, and charging for it would price a syscall neither of
    them makes a choice about.
    """
    real_open = builtins.open
    tally = [0]
    opens = [0]
    tripwire = []
    root_prefix = os.path.join(root, "")
    home_prefix = os.path.join(home, "")

    def counting_open(file, *args, **kwargs):
        fh = real_open(file, *args, **kwargs)
        if isinstance(file, (str, bytes)):
            path = file if isinstance(file, str) else os.fsdecode(file)
            if not path.startswith(os.sep):
                path = os.path.abspath(path)
            if path.startswith(root_prefix):
                opens[0] += 1
                return _Counted(fh, tally)
            # Anything else beside the tree is off limits — the manifest sits
            # there, and a tool that reads the answer key is not scanning.
            if path.startswith(home_prefix):
                tripwire.append(path)
        return fh

    builtins.open = counting_open

    def restore():
        builtins.open = real_open

    return tally, opens, tripwire, restore


def load_dedup(path: str):
    """Import a dedup.py from an explicit file, whatever else is on sys.path."""
    spec = importlib.util.spec_from_file_location("dedup_oracle", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load {}".format(path))
    mod = importlib.util.module_from_spec(spec)
    sys.modules["dedup_oracle"] = mod
    spec.loader.exec_module(mod)
    return mod


def relative(path: str, root: str) -> str:
    rel = os.path.relpath(path, root)
    return path if rel.startswith("..") else rel


def canonical(names: list) -> str:
    """The one name that stands for a record in the report.

    A hardlinked inode wears several names and the walk meets them in whatever
    order the directory hands them over, so the oracle's own `rec.path` — the
    first name it happened to see — is not stable between runs. Shortest, then
    lexicographic, is; and it is the rule the oracle already uses to choose a
    keeper, so the name in the report is the name that survives a deletion.
    """
    return min(names, key=lambda n: (len(n), n))


def report_groups(groups, root: str) -> list:
    """Groups as the protocol wants them: one path per record, sorted."""
    out = []
    for g in groups:
        members = sorted((canonical([relative(n, root) for n in rec.all_names])
                          for rec in g.files),
                         key=lambda n: (len(n), n))
        out.append((g.size, members))
    out.sort(key=lambda sm: (-sm[0], os.path.basename(sm[1][0]), sm[1][0]))
    return [members for _size, members in out]


def unreadable_paths(stats, root: str) -> list:
    """Bare paths, no error text.

    The oracle records an unreadable file as "<path>: <errno message>", and no
    two languages spell that message the same way. The protocol carries the
    path alone so a port is judged on which files it could not read, not on how
    its standard library phrases the refusal.
    """
    out = []
    for entry in stats.unreadable:
        rel = relative(entry, root)
        out.append(rel.split(": ", 1)[0])
    return sorted(set(out))


def size_stage(mod, root: str, args, include_ext, exclude_ext) -> tuple:
    """Stage 1 and 2 only: the candidate set an M1 implementation can produce.

    Runs the oracle's own walk and hardlink collapse rather than a copy of
    them, so what this reports is what the oracle believes, not what the
    harness author believed the oracle believes.
    """
    stats = mod.ScanStats()
    files = mod.walk_files([root], args.exclude, args.min_size, stats, None,
                           args.parallel, include_ext, exclude_ext)
    by_size: dict = {}
    for f in files:
        by_size.setdefault(f.size, []).append(f)

    groups = []
    for size, bucket in by_size.items():
        bucket = mod.drop_hardlinks(bucket)
        if len(bucket) > 1:
            members = sorted(
                (canonical([relative(n, root) for n in rec.all_names])
                 for rec in bucket),
                key=lambda n: (len(n), n))
            groups.append((size, members))
    groups.sort(key=lambda sm: (-sm[0], os.path.basename(sm[1][0]), sm[1][0]))
    return [m for _s, m in groups], stats


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="oracle_driver",
        description="run the oracle and emit the conformance protocol JSON")
    p.add_argument("root", help="directory to scan")
    p.add_argument("--report-json", required=True, metavar="PATH")
    p.add_argument("--dedup", default=None,
                   help="path to the oracle's dedup.py (default: the one two "
                        "directories up from this file)")
    p.add_argument("--stage", choices=("size", "full"), default="full",
                   help="'size' stops after walk and size grouping, which is "
                        "the candidate set milestone M1 is gated on")
    # Everything below is the oracle's own command line, spelled the same way.
    p.add_argument("-l", "--list", action="store_true")
    p.add_argument("--min-size", type=int, default=1, metavar="BYTES")
    p.add_argument("-j", "--jobs", type=int, default=None)
    p.add_argument("-p", "--parallel", type=int, default=None)
    p.add_argument("-x", "--exclude", action="append", default=[], metavar="GLOB")
    p.add_argument("--ext", action="append", default=[], metavar="LIST")
    p.add_argument("--skip-ext", action="append", default=[], metavar="LIST")
    p.add_argument("--no-cache", action="store_true")
    p.add_argument("--auto", nargs="?", const="shortest", metavar="RULE")
    p.add_argument("-y", "--yes", action="store_true")
    p.add_argument("--hard", action="store_true")
    p.add_argument("-q", "--quiet", action="store_true")
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    here = os.path.dirname(os.path.abspath(__file__))
    dedup_path = args.dedup or os.path.join(os.path.dirname(here), "dedup.py")
    root = os.path.abspath(args.root)
    # The manifest lives one level up from the tree; nothing there is the
    # scan's business.
    home = os.path.dirname(root)

    out = {"ok": False}
    groups = []
    mod = cache = None
    try:
        mod = load_dedup(dedup_path)
        include_ext, exclude_ext = mod.resolve_ext_filters(args.ext, args.skip_ext)

        # Settled before the instrumentation goes on: asking costs a diskutil
        # subprocess, and it is not part of what the scan reads.
        jobs = mod.read_jobs([root], args.jobs)

        tally, opens, tripwire, restore = instrument(root, home)
        try:
            if args.stage == "size":
                reported, stats = size_stage(mod, root, args, include_ext,
                                             exclude_ext)
            else:
                cache = None if args.no_cache else mod.ScanCache()
                stats = mod.ScanStats()
                groups = mod.find_duplicates(
                    [root], excludes=args.exclude, min_size=args.min_size,
                    jobs=jobs, stats=stats, verify=False, reporter=None,
                    walkers=args.parallel, cache=cache,
                    include_ext=include_ext, exclude_ext=exclude_ext)
                reported = report_groups(groups, root)
        finally:
            restore()

        out = {
            "ok": True,
            "groups": reported,
            "bytes_read": tally[0],
            "files_seen": stats.files_seen,
            "bytes_seen": stats.bytes_seen,
            "unreadable": unreadable_paths(stats, root),
            "opens": opens[0],
            # Not part of the protocol; the referee uses it to tell a scan that
            # read nothing because it was cached from one that read nothing
            # because it found nothing.
            "attested_groups": sum(1 for g in groups
                                   if getattr(g, "attested", False)),
            "tripwire": tripwire,
        }
    except BaseException as e:            # noqa: BLE001 - report, never crash
        out = {"ok": False, "error": "{}: {}".format(type(e).__name__, e),
               "traceback": traceback.format_exc()}

    with open(args.report_json, "w") as fh:
        json.dump(out, fh, indent=1)

    try:
        if not out["ok"]:
            print(out.get("traceback", out.get("error", "")), file=sys.stderr)
            return 1

        # Deletion happens after the report is written, so the report always
        # describes the scan rather than the wreckage. The cache is still open
        # here on purpose: a group the cache vouched for has to be read again
        # before anything in it goes, and refusing is the whole point of the
        # poison gate.
        if args.auto and args.stage == "full":
            return mod.run_auto(groups, args.auto, args.hard, args.yes, cache,
                                mod.all_solid_state([root]))
        return 0
    finally:
        if cache is not None:
            cache.close()


if __name__ == "__main__":
    sys.exit(main())
