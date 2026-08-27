#!/usr/bin/env python3
"""End-to-end proof that a dataless file is excluded, counted, and let back in
by --cloud — on a real tree, through the real binary, with a real kernel flag.

THE PROBLEM THIS SOLVES. A real cloud placeholder cannot be fabricated.
SF_DATALESS (0x40000000) is one of the SF_SYNTHETIC flags, which <sys/stat.h>
keeps out of SF_SUPPORTED precisely so that no userland process can set it —
root included. Only a File Provider extension makes one. So a test that wants
to watch this tool meet a placeholder has nothing to show it.

WHAT THIS DOES INSTEAD. It swaps the one constant that says WHICH flag means
"in the cloud" for UF_HIDDEN (0x00008000), which `chflags` will set on any file
you own, rebuilds, and then runs the real binary over a real tree with a real
flag set by the real kernel. Everything between the flag and the user is
exercised: getattrlistbulk carrying ATTR_CMN_FLAGS, the walk's exclusion, the
skip counter, --report-json's conditional key, the stderr notice, and --cloud
putting the file back. The ONLY thing this cannot show is that 0x40000000 is
the right number, and that is checked two other ways — against the SDK header
in `the_dataless_flag_is_the_detector_and_nothing_else_is`, and against lstat's
own st_flags in `the_bulk_read_agrees_with_lstat_field_by_field`, which is the
test that says the attribute is being read out of the right offset at all.

That last test is also what licenses the substitution: it sets UF_HIDDEN with
chflags and finds it in exactly the field this script is about to reinterpret,
so the two flags travel the same path from kernel to walk.

THE STALE-MUTANT RULE. This script edits src/, builds, and restores. The
restore is proved, not asserted: the source is diffed against git and the
rebuilt binary's hash is compared to the one taken before the mutant. Anything
that runs after this must still `cargo build --release` first.

Usage, from the repository root:
    python3 rust/evidence/scripts/m9_dataless_walk.py <scratch-dir>
"""

import json
import os
import shutil
import subprocess
import sys

RUST = "rust"
SOURCE = os.path.join(RUST, "src", "cloud.rs")
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")

REAL = "pub const SF_DATALESS: u32 = 0x4000_0000;"
# UF_HIDDEN. An owner-settable flag, so no privilege is needed, and one this
# tool does nothing else about, so nothing but the detector changes meaning.
STAND_IN = "pub const SF_DATALESS: u32 = 0x0000_8000;"

BIG = 3 * 1024 * 1024


def run(argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def build():
    done = run(["cargo", "build", "--release"], cwd=RUST)
    if done.returncode != 0:
        raise SystemExit("build failed:\n" + done.stderr[-2000:])
    return hash_of(BINARY)


def hash_of(path):
    return run(["shasum", "-a", "256", path]).stdout.split()[0]


def build_corpus(root):
    """Four copies of one file, two lone files, and nothing hidden yet."""
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(os.path.join(root, "media"))
    for index in range(4):
        with open(os.path.join(root, "media", f"reel_{index}.mov"), "wb") as fh:
            fh.write(b"REEL" * (BIG // 4))
    for index in range(2):
        with open(os.path.join(root, f"lone_{index}.bin"), "wb") as fh:
            fh.write(bytes([index]) * BIG)
    # The racily-clean rule, as every harness here has to: a file whose mtime
    # is this second may be dropped from the cache non-deterministically.
    when = os.path.getmtime(root) - 10
    for base, _, names in os.walk(root):
        for name in names:
            os.utime(os.path.join(base, name), (when, when))


def scan(root, home, *extra):
    """One --list run, and everything it said."""
    report = os.path.join(home, "report.json")
    environment = dict(
        os.environ, HOME=home, DEDUP_CACHE_DIR=os.path.join(home, "cache")
    )
    done = subprocess.run(
        [
            os.path.abspath(BINARY),
            os.path.abspath(root),
            "--min-size",
            "1",
            "--no-cache",
            "--list",
            "--report-json",
            report,
            *extra,
        ],
        capture_output=True,
        text=True,
        env=environment,
    )
    with open(report) as fh:
        said = json.load(fh)
    return done, said


def members(said):
    """Every path in every group, flattened and sorted."""
    return sorted(name for group in said["groups"] for name in group)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: m9_dataless_walk.py <scratch-dir>")
    scratch = sys.argv[1]
    os.makedirs(scratch, exist_ok=True)
    home = os.path.join(scratch, "home")
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(home)
    root = os.path.join(scratch, "tree")

    with open(SOURCE) as fh:
        original = fh.read()
    if REAL not in original:
        raise SystemExit(f"{SOURCE} does not carry {REAL!r} — refusing to edit")

    print("$ cargo build --release   (the committed source, before any mutant)")
    clean = build()
    print(f"  {clean}  target/release/dedup-rs")
    print()

    failures = []
    try:
        print("=== the mutant: SF_DATALESS means UF_HIDDEN for this build only ===")
        print(f"-   {REAL}")
        print(f"+   {STAND_IN}")
        with open(SOURCE, "w") as fh:
            fh.write(original.replace(REAL, STAND_IN))
        mutant = build()
        print(f"  {mutant}  target/release/dedup-rs   (a different binary)")
        if mutant == clean:
            failures.append("the mutant build produced the same binary")
        print()

        build_corpus(root)

        # ---------------------------------------------------------------
        print("=== 1. nothing flagged: the baseline this is measured against ===")
        done, said = scan(root, home)
        print(f"  groups:        {[len(g) for g in said['groups']]}")
        print(f"  files_seen:    {said['files_seen']}")
        print(f"  bytes_read:    {said['bytes_read']}")
        print(f"  skipped_cloud: {said.get('skipped_cloud', '(absent)')}")
        print(f"  stderr:        {done.stderr.strip()!r}")
        if [len(g) for g in said["groups"]] != [4]:
            failures.append("the baseline did not find the group of four")
        if "skipped_cloud" in said:
            failures.append("a tree with nothing flagged reported skipped_cloud")
        if done.stderr.strip() != "":
            failures.append("a tree with nothing flagged said something on stderr")
        plain_read = said["bytes_read"]
        plain_seen = said["files_seen"]
        print()

        # ---------------------------------------------------------------
        print("=== 2. one member of the group is a placeholder ===")
        hidden = os.path.join(root, "media", "reel_2.mov")
        subprocess.run(["chflags", "hidden", hidden], check=True)
        print(f"$ chflags hidden {os.path.relpath(hidden, root)}")
        done, said = scan(root, home)
        print(f"  groups:        {[len(g) for g in said['groups']]}")
        print(f"  members:       {members(said)}")
        print(f"  files_seen:    {said['files_seen']}   (was {plain_seen})")
        print(f"  bytes_read:    {said['bytes_read']}   (was {plain_read})")
        print(f"  skipped_cloud: {said.get('skipped_cloud', '(absent)')}")
        print(f"  unreadable:    {said['unreadable']}")
        print(f"  stderr:        {done.stderr.strip()!r}")
        if [len(g) for g in said["groups"]] != [3]:
            failures.append("the placeholder was not dropped from its group")
        if "media/reel_2.mov" in members(said):
            failures.append("the placeholder is still in a group")
        if said.get("skipped_cloud") != 1:
            failures.append(f"skipped_cloud is {said.get('skipped_cloud')}, want 1")
        if said["unreadable"]:
            failures.append("a skipped file was reported as unreadable")
        if said["files_seen"] != plain_seen - 1:
            failures.append("files_seen counted a file the scan never looked at")
        if said["bytes_read"] >= plain_read:
            failures.append("skipping a file did not read fewer bytes")
        if "1 file in cloud" not in done.stderr:
            failures.append("the run did not say a file was in the cloud")
        if "--cloud to include" not in done.stderr:
            failures.append("the notice did not say how to include it")
        print()

        # ---------------------------------------------------------------
        print("=== 3. --cloud puts it back ===")
        done, said = scan(root, home, "--cloud")
        print(f"  groups:        {[len(g) for g in said['groups']]}")
        print(f"  members:       {members(said)}")
        print(f"  files_seen:    {said['files_seen']}")
        print(f"  bytes_read:    {said['bytes_read']}")
        print(f"  skipped_cloud: {said.get('skipped_cloud', '(absent)')}")
        print(f"  stderr:        {done.stderr.strip()!r}")
        if [len(g) for g in said["groups"]] != [4]:
            failures.append("--cloud did not restore the group of four")
        if "media/reel_2.mov" not in members(said):
            failures.append("--cloud did not put the placeholder back")
        if "skipped_cloud" in said:
            failures.append("--cloud reported a skip it did not make")
        if said["files_seen"] != plain_seen:
            failures.append("--cloud did not see every file")
        if done.stderr.strip() != "":
            failures.append("--cloud printed a cloud notice anyway")
        print()

        # ---------------------------------------------------------------
        print("=== 4. a second placeholder, outside any group ===")
        lone = os.path.join(root, "lone_0.bin")
        subprocess.run(["chflags", "hidden", lone], check=True)
        print(f"$ chflags hidden {os.path.relpath(lone, root)}")
        done, said = scan(root, home)
        print(f"  groups:        {[len(g) for g in said['groups']]}")
        print(f"  skipped_cloud: {said.get('skipped_cloud', '(absent)')}")
        print(f"  stderr:        {done.stderr.strip()!r}")
        if said.get("skipped_cloud") != 2:
            failures.append(f"skipped_cloud is {said.get('skipped_cloud')}, want 2")
        if "2 files in cloud" not in done.stderr:
            failures.append("the notice did not pluralise for two files")
        print()

        # ---------------------------------------------------------------
        print("=== 5. the whole tree is placeholders ===")
        for base, _, names in os.walk(root):
            for name in names:
                subprocess.run(["chflags", "hidden", os.path.join(base, name)])
        done, said = scan(root, home)
        print(f"  groups:        {said['groups']}")
        print(f"  files_seen:    {said['files_seen']}")
        print(f"  bytes_read:    {said['bytes_read']}")
        print(f"  skipped_cloud: {said.get('skipped_cloud', '(absent)')}")
        print(f"  exit:          {done.returncode}")
        print(f"  stderr:        {done.stderr.strip()!r}")
        if said["groups"]:
            failures.append("a tree of nothing but placeholders found duplicates")
        if said["bytes_read"] != 0:
            failures.append("a tree of nothing but placeholders was read anyway")
        if said["files_seen"] != 0:
            failures.append("a tree of nothing but placeholders was counted as seen")
        if said.get("skipped_cloud") != 6:
            failures.append(f"skipped_cloud is {said.get('skipped_cloud')}, want 6")
        if done.returncode != 0:
            failures.append("a tree of nothing but placeholders was an error")
        print()

    finally:
        print("=== the stale-mutant rule: restored, and proved restored ===")
        with open(SOURCE, "w") as fh:
            fh.write(original)
        print(f"$ git diff --stat -- {SOURCE}")
        diff = run(["git", "diff", "--stat", "--", SOURCE]).stdout.strip()
        print(f"  {diff!r}   (empty = the file is the committed one)")
        if diff:
            failures.append("the source was NOT restored")
        restored = build()
        print("$ cargo build --release")
        print(f"  {restored}  target/release/dedup-rs")
        print(
            f"  {'identical to the pre-mutant binary' if restored == clean else 'DIFFERENT — THE BINARY IS STALE'}"
        )
        if restored != clean:
            failures.append("the rebuilt binary is not the pre-mutant binary")

    print()
    if failures:
        print(f"FAILED ({len(failures)}):")
        for why in failures:
            print(f"  - {why}")
        return 1
    print(
        "every case held: a flagged file is excluded before it becomes a record, "
        "counted where a person can see it, absent from --report-json when the "
        "count is zero, and read only when --cloud asks for it"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
