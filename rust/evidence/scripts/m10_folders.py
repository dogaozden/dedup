#!/usr/bin/env python3
"""M10 phase 1, end to end, the way a person runs it.

Two scratch trees, each with two identical folders (some small files, a
`.DS_Store` that differs), a near-copy that differs in one small file, and a
loose third copy of one file. The scan cache is pointed at a scratch directory
of its own, and every tree is aged ten seconds so the cache's racily-clean rule
does not throw its rows away.

Scenario A, the happy path:
  cold `--list` (learns), warm `--list` (reads nothing, and the folder group is
  labelled `candidate`), `--auto shortest -y`, then a second `--auto`, which
  finds nothing to remove.

Scenario B, a forged file:
  cold `--list`, warm `--list`, then one small file in the copy that would be
  trashed is rewritten with the same size and its mtime put back, so the cache
  still believes the old digest. `--auto` must refuse that folder group and
  leave both folders intact. A later `--list` must read again, which shows the
  refused cache entries were dropped.

What `--auto` does to the near-copy's big file and the loose third copy is the
ordinary file rule, not a folder rule: a kept copy of those bytes exists, so
the extra copies go. The check is that the near-copy folder stays, its small
differing file stays byte for byte, and only its identical files leave.

Every name this script creates carries a random nonce, files included, so the
entries it puts in the real Trash are ones it can name. Cleanup removes its
scratch trees, its cache directories, and every new Trash entry whose name
carries one of its nonces. Nothing else in the Trash is named or touched.

Nothing here reads a real user folder. Trees and caches live under the scratch
root given on the command line (default /private/tmp/claude-501/m10-scratch).

Usage, from the repository root (after `cargo build --release` in rust/):
    rust/evidence/scripts/m10_folders.py [scratch-root]
"""
import json
import os
import random
import shutil
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
BINARY = os.path.abspath(os.path.join(HERE, "..", "..", "target", "release", "dedup-rs"))
SCRATCH = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else
                          "/private/tmp/claude-501/m10-scratch")
TRASH = os.path.join(os.path.expanduser("~"), ".Trash")
TRASH_BEFORE = set(os.listdir(TRASH))

failures = []
nonces = []


def check(ok, what):
    print(("  pass  " if ok else "  FAIL  ") + what)
    if not ok:
        failures.append(what)


class Run:
    """One scratch tree, with its own nonce, names and scan cache."""

    def __init__(self, label):
        self.nonce = uuid.uuid4().hex[:8]
        nonces.append(self.nonce)
        n = self.nonce
        self.label = label
        self.root = os.path.join(SCRATCH, "e2e-{}-{}".format(label, n))
        self.cache = os.path.join(SCRATCH, "cache-{}-{}".format(label, n))
        self.kept = "Holiday " + n
        self.twin = "Holiday " + n + " copy"
        self.near = "Near " + n
        self.loose = "Loose " + n
        self.big = "big-" + n + ".jpg"
        self.notes = "notes-" + n + ".txt"
        self.tiny = "tiny-" + n + ".bin"
        self.near_big = "near-big-" + n + ".jpg"
        self.third = "third-" + n + ".jpg"

    def path(self, *parts):
        return os.path.join(self.root, *parts)

    def trash(self, name):
        return os.path.join(TRASH, name)

    def build(self):
        rng = random.Random(self.nonce)
        big = rng.randbytes(10_500_000)
        tiny = rng.randbytes(5000)
        os.makedirs(self.root)
        # The two copies are the same bytes, so the tiny file is drawn once and
        # written to both. Drawing it per copy would make them differ.
        for copy in (self.kept, self.twin):
            os.makedirs(self.path(copy, "sub"))
            write(self.path(copy, self.big), big)
            write(self.path(copy, self.notes), b"same note\n")
            write(self.path(copy, "sub", self.tiny), tiny)
        write(self.path(self.kept, ".DS_Store"), b"view from the first copy")
        write(self.path(self.twin, ".DS_Store"), b"view from the second copy")
        os.makedirs(self.path(self.near, "sub"))
        write(self.path(self.near, self.near_big), big)
        write(self.path(self.near, self.notes), b"same note\n")
        write(self.path(self.near, "sub", self.tiny), rng.randbytes(5000))  # differs
        os.makedirs(self.path(self.loose))
        write(self.path(self.loose, self.third), big)
        self.age()

    def age(self):
        """Push every mtime into the past. Without this a freshly written file
        is racily clean, the cache drops its row, and a warm scan reads again."""
        when = time.time() - 60
        for base, dirs, names in os.walk(self.root):
            for name in names:
                os.utime(os.path.join(base, name), (when, when))

    def cli(self, *args):
        env = dict(os.environ, DEDUP_CACHE_DIR=self.cache)
        done = subprocess.run([BINARY, self.root] + list(args), capture_output=True,
                              text=True, env=env)
        return done.returncode, done.stdout, done.stderr

    def list_json(self):
        """A `--list` run, with its report read back: stdout, and the report."""
        report = os.path.join(SCRATCH, "list-{}.json".format(self.nonce))
        code, out, err = self.cli("--list", "--report-json", report)
        with open(report) as fh:
            body = json.load(fh)
        os.remove(report)
        return code, out, err, body

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)
        shutil.rmtree(self.cache, ignore_errors=True)
        for name in sorted(set(os.listdir(TRASH)) - TRASH_BEFORE):
            if any(nonce in name for nonce in nonces):
                path = self.trash(name)
                if os.path.isdir(path) and not os.path.islink(path):
                    shutil.rmtree(path)
                else:
                    os.remove(path)


def write(path, body):
    with open(path, "wb") as fh:
        fh.write(body)


def snapshot(root):
    """Every file under `root`, relative path -> (kind, bytes or target)."""
    files = {}
    for base, _, names in os.walk(root):
        for name in names:
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root)
            if os.path.islink(path):
                files[rel] = ("link", os.readlink(path))
            else:
                with open(path, "rb") as fh:
                    files[rel] = ("file", fh.read())
    return files


def candidate_lines(out):
    return [line for line in out.splitlines() if "candidate" in line]


def scenario_a():
    run = Run("a")
    print("== scenario A: {} (cache {})".format(run.root, run.cache))
    run.build()
    before = snapshot(run.root)
    kept_before = {k: v for k, v in before.items() if k.startswith(run.kept + os.sep)}
    twin_before = {k[len(run.twin) + 1:]: v for k, v in before.items()
                   if k.startswith(run.twin + os.sep)}
    near_before = {k: v for k, v in before.items() if k.startswith(run.near + os.sep)}
    for name in (run.twin, run.near_big, run.third):
        check(not os.path.lexists(run.trash(name)), "nothing named {} in the Trash yet".format(name))
    try:
        print("-- cold --list")
        code, out, err, body = run.list_json()
        print(out)
        check(code == 0, "cold --list exits 0")
        check("1 folder group(s)" in out and run.twin + "/" in out,
              "cold --list shows the folder group, its twin and the kept folder")
        check(not candidate_lines(out), "a cold scan's folder group is proven, not a candidate")
        check(body.get("folders") == [[run.kept, run.twin]],
              "report-json has folders = [[kept, twin]] (got {})".format(body.get("folders")))
        check(body["bytes_read"] > 0, "the cold scan read bytes ({})".format(body["bytes_read"]))

        print("-- warm --list (untouched tree)")
        code, out, err, body = run.list_json()
        print(out)
        check(code == 0, "warm --list exits 0")
        check(body["bytes_read"] == 0,
              "the warm scan read 0 bytes (got {})".format(body["bytes_read"]))
        check(any(line.lstrip().startswith("keep " + run.kept + "/") for line in candidate_lines(out)),
              "the warm scan still reports the folder group, labelled candidate")
        check(body.get("folders") == [[run.kept, run.twin]], "the warm report keeps the folder group")

        print("-- --auto shortest -y")
        code, out, err = run.cli("--auto", "shortest", "-y")
        print(out)
        if err.strip():
            print(err)
        check(code == 0, "--auto exits 0 (exit {})".format(code))
        after = snapshot(run.root)
        check(not os.path.lexists(run.path(run.twin)), "the twin folder left the tree")
        check(os.path.isdir(run.trash(run.twin)) and not os.path.islink(run.trash(run.twin)),
              "the twin is in the Trash as one directory")
        check(snapshot(run.trash(run.twin)) == {k: v for k, v in twin_before.items()},
              "every file of the moved folder is byte-identical to before, its .DS_Store included")
        kept_after = {k: v for k, v in after.items() if k.startswith(run.kept + os.sep)}
        check(kept_after == kept_before, "the kept twin is intact, byte for byte")
        check(os.path.isdir(run.path(run.near)), "the near-copy folder is still there")
        near_after = {k: v for k, v in after.items() if k.startswith(run.near + os.sep)}
        check(near_after.get(os.path.join(run.near, "sub", run.tiny)) ==
              near_before.get(os.path.join(run.near, "sub", run.tiny)),
              "the near-copy's differing small file is untouched")
        # Below the default 10 MiB floor the notes file is never a file group, so
        # it is not an ordinary duplicate and stays. The big file is one.
        check(os.path.join(run.near, run.notes) in after,
              "the near-copy's small identical file is below the floor, so it stays")
        check(os.path.join(run.near, run.near_big) not in after,
              "the near-copy's big duplicate left as an ordinary file duplicate")
        check(os.path.isfile(run.trash(run.near_big)), "that big duplicate is in the Trash by name")
        check(not os.path.lexists(run.path(run.loose, run.third)), "the loose third copy left the tree")
        check(os.path.isfile(run.trash(run.third)), "the loose third copy is in the Trash by name")

        print("-- the second --auto")
        code, out, err = run.cli("--auto", "shortest", "-y")
        print(out)
        check(code == 0 and "Nothing to remove." in out, "the second run finds nothing to remove")
    finally:
        run.cleanup()


def scenario_b():
    run = Run("b")
    print("== scenario B: {} (cache {})".format(run.root, run.cache))
    run.build()
    twin_before = snapshot(run.path(run.twin))
    kept_before = snapshot(run.path(run.kept))
    try:
        run.list_json()
        code, out, err, body = run.list_json()
        check(body["bytes_read"] == 0 and candidate_lines(out),
              "the warm scan before the forgery reads 0 bytes and labels the group candidate")

        # Same size, different bytes, mtime put back: the cache still believes
        # the digest it stored for the old bytes.
        target = run.path(run.twin, run.notes)
        before = os.stat(target)
        write(target, b"same notE\n")  # ten bytes, like the original
        os.utime(target, ns=(before.st_atime_ns, before.st_mtime_ns))
        check(os.path.getsize(target) == before.st_size, "the forged file keeps its size")
        check(os.stat(target).st_mtime_ns == before.st_mtime_ns, "the forged file keeps its mtime")

        print("-- --auto shortest -y after the forgery")
        code, out, err = run.cli("--auto", "shortest", "-y")
        print(out)
        print(err)
        check(code == 0, "--auto exits 0 (exit {})".format(code))
        check("Refusing the folder group at {}/".format(run.kept) in err and
              "{}/".format(run.twin) in err,
              "--auto says which folder it refused, and why")
        check(os.path.isdir(run.path(run.twin)) and os.path.isdir(run.path(run.kept)),
              "both folders are still where they were")
        twin_after = snapshot(run.path(run.twin))
        check(twin_after.get(run.notes) == ("file", b"same notE\n"), "the forged file is still in place")
        check({k: v for k, v in twin_after.items() if k != run.notes} ==
              {k: v for k, v in twin_before.items() if k != run.notes},
              "every other file in the refused folder is untouched")
        check(snapshot(run.path(run.kept)) == kept_before, "the kept folder is intact, byte for byte")

        print("-- a --list after the refusal")
        code, out, err, body = run.list_json()
        print(out)
        check(body["bytes_read"] > 0,
              "the refused cache entries were dropped, so the next scan reads again ({})".format(
                  body["bytes_read"]))
        check(body.get("folders", []) == [], "the forged copy no longer matches, so no folder group")
    finally:
        run.cleanup()


def main():
    scenario_a()
    scenario_b()


if __name__ == "__main__":
    main()
    print()
    if failures:
        print("FAILED: {} check(s)".format(len(failures)))
        for line in failures:
            print("  - " + line)
        sys.exit(1)
    print("ALL CHECKS PASSED")
