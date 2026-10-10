#!/usr/bin/env python3
"""M10's fence under `--no-folders`, measured on trees that HAVE folder candidates.

M10-FOLDERS.md: with `--no-folders`, every output, counters included, is
byte-identical to main, on EVERY tree. The M8/M9 freeze corpora have no
duplicate folder candidates, so on their own they cannot show that. This adds
one corpus that does (identical folders, a near-copy, a loose third copy) and
runs every non-interactive invocation of the freeze harness over it.

The base is main's binary run with the flags as they are. The candidate is the
branch's binary run with `--no-folders` appended to every invocation. Any byte
that differs is a failed fence. Default-flags runs of the candidate on the same
corpus are captured too, for the record only: those are supposed to differ.

Uses m8_freeze.py's corpora, invocations and comparison, unchanged.

Usage, from the repository root:
    m10_no_folders_freeze.py <main-binary> <branch-binary> <scratch-dir>
"""
import json
import os
import random
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import m8_freeze as m8  # noqa: E402


def corpus_folders(root):
    """Two identical folders with one file over the default 10 MiB floor, so the
    default invocations see a candidate too; a near-copy whose subfolder
    differs; and a loose third copy of the big file."""
    big = random.Random(5).randbytes(10_500_000)
    for copy in ("Photos", "Photos copy"):
        m8.write(root, copy + "/big.jpg", big)
        m8.write(root, copy + "/notes.txt", b"same note")
        m8.write(root, copy + "/sub/tiny.bin", random.Random(7).randbytes(5000))
    m8.write(root, "Photos/.DS_Store", b"one view")
    m8.write(root, "Photos copy/.DS_Store", b"another view")
    m8.write(root, "Near/big.jpg", big)
    m8.write(root, "Near/notes.txt", b"same note")
    m8.write(root, "Near/sub/tiny.bin", random.Random(8).randbytes(5000))
    m8.write(root, "Loose/third.jpg", big)


m8.CORPORA["folders"] = corpus_folders


def capture(binary, outdir, scratch, extra):
    """m8_freeze.capture, with `extra` flags appended to every invocation. The
    warm-cache pair runs too, with the same flags."""
    os.makedirs(outdir, exist_ok=True)
    cache_dir = os.path.join(scratch, "cache")
    captured = {}
    for corpus in sorted(m8.CORPORA):
        m8.build_corpus(scratch, corpus)
        for label, flags, destructive in m8.INVOCATIONS:
            shutil.rmtree(cache_dir, ignore_errors=True)
            key = "{}/{}".format(corpus, label)
            captured[key] = m8.run_case(binary, scratch, corpus, label,
                                        flags + extra, destructive, cache_dir)
        shutil.rmtree(cache_dir, ignore_errors=True)
        m8.build_corpus(scratch, corpus)
        captured["{}/warm-cold".format(corpus)] = m8.run_case(
            binary, scratch, corpus, "warm-1", ["--list"] + extra, False, cache_dir)
        captured["{}/warm-warm".format(corpus)] = m8.run_case(
            binary, scratch, corpus, "warm-2", ["--list"] + extra, False, cache_dir)
    path = os.path.join(outdir, "capture.json")
    with open(path, "w") as fh:
        json.dump(captured, fh, indent=2, sort_keys=True)
    print("captured {} cases from {} with {}".format(
        len(captured), binary, " ".join(extra) or "no extra flags"))
    return path


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main_binary, branch_binary, scratch = (os.path.abspath(arg) for arg in sys.argv[1:])
    os.makedirs(scratch, exist_ok=True)
    base_dir = os.path.join(scratch, "cap-main")
    fenced_dir = os.path.join(scratch, "cap-no-folders")
    default_dir = os.path.join(scratch, "cap-default")
    capture(main_binary, base_dir, os.path.join(scratch, "run-main"), [])
    capture(branch_binary, fenced_dir, os.path.join(scratch, "run-nf"), ["--no-folders"])
    print()
    print("=== the fence: main vs branch --no-folders ===")
    verdict = m8.compare(base_dir, fenced_dir)
    print()
    print("=== for the record: main vs branch at defaults (expected to differ) ===")
    capture(branch_binary, default_dir, os.path.join(scratch, "run-default"), [])
    m8.compare(base_dir, default_dir)
    sys.exit(verdict)


if __name__ == "__main__":
    main()
