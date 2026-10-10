#!/usr/bin/env python3
"""M10 phase 1, end to end, the way a person runs it.

M10-FOLDERS.md, Verification: a scratch tree with two identical folders (some
small files, a `.DS_Store` that differs), a near-copy that differs in one small
file, and a loose third copy of one file. Runs `--list`, then `--auto shortest
-y`. Then checks that the folder went to the Trash as one directory, that its
twin is intact, that the near-copy's own files are untouched, and that a second
run finds nothing to remove.

What `--auto shortest` does to the near-copy's big file is the ordinary file
rule, not a folder rule: a kept copy of those bytes lives in the kept folder,
so the near-copy's copy is an extra and goes. The check is that the near-copy
folder stays, its small files stay byte for byte, and only its big duplicate
leaves.

Every name this script creates carries a random nonce, so the entries it puts
in the real Trash are the ones it can name, and the only ones it removes. The
Trash is checked by name in ~/.Trash, as M10 asks, and nothing else in it is
touched.

Nothing here reads a real user folder. The tree lives under the scratch root
given on the command line (default /private/tmp/claude-501/m10-scratch).

Usage, from the repository root (after `cargo build --release` in rust/):
    rust/evidence/scripts/m10_folders.py [scratch-root]
"""
import json
import os
import random
import shutil
import subprocess
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
BINARY = os.path.abspath(os.path.join(HERE, "..", "..", "target", "release", "dedup-rs"))
SCRATCH = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else
                          "/private/tmp/claude-501/m10-scratch")
TRASH = os.path.join(os.path.expanduser("~"), ".Trash")

NONCE = uuid.uuid4().hex[:8]
TREE = os.path.join(SCRATCH, "e2e-" + NONCE)
# Every name carries the nonce, files included: whatever the run moves to the
# Trash is then a name this script can recognise and remove, and nothing else.
KEPT = "Holiday " + NONCE                 # shortest: this one is kept
TWIN = "Holiday " + NONCE + " copy"      # ...and this whole folder goes
NEAR = "Near " + NONCE                   # differs in one small file
LOOSE_DIR = "Loose " + NONCE
BIG = "big-" + NONCE + ".jpg"            # the big file in both copies
NOTES = "notes-" + NONCE + ".txt"        # the small file in every folder
TINY = "tiny-" + NONCE + ".bin"          # the small file under sub/
BIG_NEAR = "near-big-" + NONCE + ".jpg"  # the near-copy's big duplicate
THIRD = "third-" + NONCE + ".jpg"        # the loose third copy

failures = []
TRASH_BEFORE = set(os.listdir(TRASH))


def check(ok, what):
    print(("  pass  " if ok else "  FAIL  ") + what)
    if not ok:
        failures.append(what)


def run(*args):
    done = subprocess.run([BINARY] + list(args), capture_output=True, text=True)
    return done.returncode, done.stdout, done.stderr


def snapshot(root):
    """Every file under `root`, as relative path -> bytes, plus the folders."""
    files, folders = {}, set()
    for base, dirs, names in os.walk(root):
        for name in dirs:
            folders.add(os.path.relpath(os.path.join(base, name), root))
        for name in names:
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root)
            if os.path.islink(path):
                files[rel] = ("link", os.readlink(path))
            else:
                with open(path, "rb") as fh:
                    files[rel] = ("file", fh.read())
    return files, folders


def build():
    rng = random.Random(NONCE)
    big = rng.randbytes(10_500_000)
    tiny = rng.randbytes(5000)
    os.makedirs(TREE)
    # The two copies are the same bytes, so the tiny file is drawn once and
    # written to both. Drawing it per copy would make them differ.
    for copy in (KEPT, TWIN):
        os.makedirs(os.path.join(TREE, copy, "sub"))
        with open(os.path.join(TREE, copy, BIG), "wb") as fh:
            fh.write(big)
        with open(os.path.join(TREE, copy, NOTES), "w") as fh:
            fh.write("same note\n")
        with open(os.path.join(TREE, copy, "sub", TINY), "wb") as fh:
            fh.write(tiny)
    with open(os.path.join(TREE, KEPT, ".DS_Store"), "wb") as fh:
        fh.write(b"view from the first copy")
    with open(os.path.join(TREE, TWIN, ".DS_Store"), "wb") as fh:
        fh.write(b"view from the second copy")
    os.makedirs(os.path.join(TREE, NEAR, "sub"))
    with open(os.path.join(TREE, NEAR, BIG_NEAR), "wb") as fh:
        fh.write(big)
    with open(os.path.join(TREE, NEAR, NOTES), "w") as fh:
        fh.write("same note\n")
    with open(os.path.join(TREE, NEAR, "sub", TINY), "wb") as fh:
        fh.write(rng.randbytes(5000))  # the one small file that differs
    os.makedirs(os.path.join(TREE, LOOSE_DIR))
    with open(os.path.join(TREE, LOOSE_DIR, THIRD), "wb") as fh:
        fh.write(big)


def trash_entry(name):
    return os.path.join(TRASH, name)


def cleanup():
    """Remove exactly what this run created: its scratch tree, and every new
    entry in the Trash whose name carries this run's nonce. Nothing else in the
    Trash is named, let alone removed."""
    shutil.rmtree(TREE, ignore_errors=True)
    for name in sorted(set(os.listdir(TRASH)) - TRASH_BEFORE):
        if NONCE not in name:
            continue
        path = trash_entry(name)
        if os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path)
        else:
            os.remove(path)


def main():
    print("== build the scratch tree", TREE)
    build()
    before, _ = snapshot(TREE)
    kept_before = {k: v for k, v in before.items() if k.startswith(KEPT + os.sep)}
    twin_before = {k[len(TWIN) + 1:]: v for k, v in before.items() if k.startswith(TWIN + os.sep)}
    near_before = {k: v for k, v in before.items() if k.startswith(NEAR + os.sep)}
    for name in (TWIN, BIG_NEAR, THIRD):
        check(not os.path.lexists(trash_entry(name)), "nothing named {} in the Trash yet".format(name))

    print("== --list")
    report_path = os.path.join(SCRATCH, "list-" + NONCE + ".json")
    code, out, err = run(TREE, "--list", "--min-size", "1", "--report-json", report_path)
    print(out)
    check(code == 0, "--list exits 0 (exit {})".format(code))
    check("folder group(s)" in out and TWIN + "/" in out and KEPT + "/" in out,
          "--list shows the folder group, its twin and the kept folder")
    folder_lines = [line for line in out.splitlines() if "/   (" in line or line.rstrip().endswith("/")]
    check(not any(NEAR in line for line in folder_lines),
          "--list does not offer the near-copy as a folder")
    with open(report_path) as fh:
        report = json.load(fh)
    os.remove(report_path)
    check(report.get("folders") == [[KEPT, TWIN]],
          "report-json has folders = [[kept, twin]] (got {})".format(report.get("folders")))

    print("== --auto shortest -y")
    code, out, err = run(TREE, "--auto", "shortest", "-y", "--min-size", "1")
    print(out)
    if err.strip():
        print(err)
    check(code == 0, "--auto exits 0 (exit {})".format(code))

    after, _ = snapshot(TREE)
    print("== checks")
    check(not os.path.lexists(os.path.join(TREE, TWIN)), "the twin folder left the tree")
    trashed = trash_entry(TWIN)
    check(os.path.isdir(trashed) and not os.path.islink(trashed),
          "the twin is in the Trash as one directory: {}".format(trashed))
    moved, _ = snapshot(trashed) if os.path.isdir(trashed) else ({}, set())
    check(moved == twin_before,
          "every file of the moved folder is byte-identical to before, its .DS_Store included")

    kept_after = {k: v for k, v in after.items() if k.startswith(KEPT + os.sep)}
    check(kept_after == kept_before, "the kept twin is intact, byte for byte")

    check(os.path.isdir(os.path.join(TREE, NEAR)), "the near-copy folder is still there")
    near_after = {k: v for k, v in after.items() if k.startswith(NEAR + os.sep)}
    # The near-copy's own bytes stay. Its notes and big file are byte-identical
    # to files in the kept folder, so the ordinary file rule removes them, as it
    # always has; the differing small file has no twin and stays.
    check(near_after.get(os.path.join(NEAR, "sub", TINY)) ==
          near_before.get(os.path.join(NEAR, "sub", TINY)),
          "the near-copy's differing small file is untouched")
    check(os.path.join(NEAR, NOTES) not in after and os.path.join(NEAR, BIG_NEAR) not in after,
          "the near-copy's identical files left as ordinary file duplicates of the kept folder")
    check(os.path.isfile(trash_entry(BIG_NEAR)), "that big duplicate is in the Trash by name")
    check(not os.path.lexists(os.path.join(TREE, LOOSE_DIR, THIRD)),
          "the loose third copy left the tree")
    check(os.path.isfile(trash_entry(THIRD)), "the loose third copy is in the Trash by name")

    print("== the second run")
    code, out, err = run(TREE, "--auto", "shortest", "-y", "--min-size", "1")
    print(out)
    check(code == 0, "second --auto exits 0 (exit {})".format(code))
    check("Nothing to remove." in out, "the second run finds nothing to remove")
    code, out, _ = run(TREE, "--list", "--min-size", "1")
    check("No duplicates found." in out, "the second --list finds no duplicates")


if __name__ == "__main__":
    try:
        main()
    finally:
        cleanup()
    print()
    if failures:
        print("FAILED: {} check(s)".format(len(failures)))
        for line in failures:
            print("  - " + line)
        sys.exit(1)
    print("ALL CHECKS PASSED")
