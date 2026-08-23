#!/usr/bin/env python3
"""Prove the parsers before believing anything they say.

XBENCH's correctness column is only worth reading if the code that turns each
tool's output into groups is right. Every adapter is checked here against a
tree small enough to hold in your head and planted so that the answer is known
before any tool sees it — including the cases where the tools disagree with
each other, which is where a parser written from a manual instead of from
observed output goes wrong.

Run it after touching tools.py, and after any competitor upgrade:

    python3 xbench/verify_parsers.py

Three things are asserted, not one:

  * every tool's answer, reduced to inodes, equals the planted answer. This
    is the check the correctness column depends on.
  * the RAW PATH counts still differ in the documented direction — fclones
    printing three names where ours and jdupes print two. That difference is
    recorded in tools.py as a semantic delta, and if a future fclones stops
    doing it, this check fails and the note gets corrected instead of quietly
    becoming false.
  * the checker FAILS when it should. A correctness column that has only ever
    printed "exact" has not been shown to work — it has been shown to agree,
    which is a different thing and is what a checker that returned True
    unconditionally would also do. So a correct answer is deliberately
    corrupted six ways and every corruption must be caught. This is RBENCH's
    selftest idea applied to the one gate XBENCH actually has.
"""
from __future__ import annotations

import os
import random
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "bench"))
import corpus                                             # noqa: E402
import tools as toolkit                                   # noqa: E402

MIN_SIZE = 4096


def plant(tree: str) -> corpus.Corpus:
    """A tree containing one of everything the three tools disagree about.

    Built through corpus.Corpus, so the answer key comes out of the same
    construction record the real scenarios use rather than out of a list
    written beside it.
    """
    rng = random.Random(20260823)
    c = corpus.Corpus(tree, MIN_SIZE)

    # A plain pair and a plain trio, in different directories: the easy cases,
    # here so that a parser that returns nothing at all cannot pass.
    pair = corpus.rand(rng, 4096)
    c.write("a.bin", pair)
    c.write("sub/b.bin", pair)
    trio = corpus.rand(rng, 8192)
    for rel in ("c.bin", "sub/d.bin", "e.bin"):
        c.write(rel, trio)

    # Hidden. fclones skips these unless --hidden; jdupes includes them unless
    # -A. If the alignment table is wrong, this group goes missing for exactly
    # one tool.
    hidden = corpus.rand(rng, 4096)
    c.write(".hidden/h1.bin", hidden)
    c.write(".hidden/h2.bin", hidden)

    # A hardlink cluster with no content twin. Hardlinks alone are not
    # duplicates, and no tool may report this.
    c.write("hl/a.bin", corpus.rand(rng, 4096))
    c.link("hl/a.bin", "hl/b.bin")

    # A hardlink cluster that DOES have a content twin: one inode wearing two
    # names, plus a separate file holding the same bytes. Two members, not
    # three — and this is the group fclones prints differently.
    linked = corpus.rand(rng, 4096)
    c.write("hlc/a.bin", linked)
    c.link("hlc/a.bin", "hlc/b.bin")
    c.write("hlc/twin.bin", linked)

    # Below min_size. Identical, and must be reported by nobody.
    tiny = corpus.rand(rng, 10)
    c.write("tiny/a", tiny)
    c.write("tiny/b", tiny)

    # A symlink to a real group member: never followed, never a member.
    c.symlink("link-to-a.bin", os.path.join(tree, "a.bin"))

    # A wide group, because a parser can be right about pairs and wrong about
    # the shape of anything larger.
    wide = corpus.rand(rng, 16384)
    for i in range(5):
        c.write("wide/w{}.bin".format(i), wide)
    return c


# ─────────────────────────────────────────────────────────────────────────────
# the checker, sabotaged on purpose
# ─────────────────────────────────────────────────────────────────────────────

def mutations(groups: list) -> list:
    """(name, corrupted answer, what a working checker must say) for each way
    a duplicate finder can be wrong.

    The corruptions are applied to a KNOWN-GOOD parsed answer, so anything
    that still compares equal is a hole in the checker rather than a quirk of
    a tool. Every one of these is a mistake a real duplicate finder can make.
    """
    ordered = sorted(groups, key=len, reverse=True)
    wide = ordered[0]                     # the 5-member group
    pair = ordered[-1]                    # a 2-member group
    out = [
        # Missed a duplicate entirely: the classic false negative.
        ("drop-group", [g for g in groups if g is not pair], "missed"),
        # Claimed two unrelated files are copies: the classic false positive,
        # and the one that costs a user data if they act on it.
        ("spurious-group", groups + [["u-not-a-dupe-1", "u-not-a-dupe-2"]],
         "spurious"),
        # Ran two real groups together — right that they are duplicates, wrong
        # about which of what.
        ("merge-groups", [wide + pair] + [g for g in groups
                                          if g is not wide and g is not pair],
         "spurious"),
        # Found part of a group. A subset is a different answer, not a
        # partially correct one.
        ("split-group", [wide[:2]] + [g for g in groups if g is not wide],
         "spurious"),
        # Reported a hardlink cluster as a duplicate of itself. Two names, one
        # inode, deleting either frees nothing. This is what `-H` would do to
        # any of these tools, and until the fix in groups_as_inodes it was
        # passing through the checker unnoticed.
        ("hardlink-as-duplicate", groups + [["hl/a.bin", "hl/b.bin"]],
         "spurious"),
        # Named a file that is not there. lstat cannot resolve it, and the
        # group must not be allowed to match a real one by accident.
        ("phantom-member", [["a.bin", "sub/does-not-exist.bin"]]
         + [g for g in groups if g is not pair], "spurious"),
    ]
    return out


def check_mutations(tree: str, truth, groups: list) -> bool:
    """Every corruption of a correct answer must be caught. No exceptions."""
    print("\nchecker sabotage — a correct answer, corrupted six ways:\n")
    ok = True
    for name, corrupted, expect in mutations(groups):
        exact, detail = toolkit.compare(
            truth, toolkit.groups_as_inodes(tree, corrupted))
        caught = (not exact) and expect in detail
        print("  {:<22} {:<7} {}".format(
            name, "caught" if caught else "MISSED", detail))
        if not caught:
            ok = False

    # ...and the control: the uncorrupted answer must still pass, or the
    # checker is simply rejecting everything and the six above prove nothing.
    exact, detail = toolkit.compare(truth, toolkit.groups_as_inodes(tree, groups))
    print("  {:<22} {:<7} {}".format(
        "control, no sabotage", "clean" if exact else "UNEXPECTED", detail))
    return ok and exact


def main() -> int:
    binary = os.path.join(REPO, "rust", "target", "release", "dedup-rs")
    if not os.path.isfile(binary):
        print("no dedup-rs at {} — build rust/ at --release first".format(binary),
              file=sys.stderr)
        return 2

    work = tempfile.mkdtemp(prefix="xb-verify-")
    tree = os.path.join(work, "tree")
    os.makedirs(tree)
    try:
        c = plant(tree)
        manifest = {"groups": c.groups()}
        truth = toolkit.truth_as_inodes(tree, manifest)
        print("planted {} groups under {}\n".format(len(truth), tree))

        # The planted answer, spelled out, so a reader can check the check.
        for group in sorted(manifest["groups"], key=lambda g: -g["size"]):
            print("  {:>6} B  {}".format(
                group["size"],
                " ".join(sorted(m["names"][0] for m in group["members"]))))
        print()

        found = toolkit.discover(binary)
        if len(found) < 3:
            print("only {} tools installed; install fclones and jdupes"
                  .format(len(found)), file=sys.stderr)
            return 2

        ok = True
        raw_paths = {}
        known_good = None
        for tool in found:
            tool.reset_cache(work)
            out = os.path.join(work, "{}.json".format(tool.key))
            argv = tool.argv(tree, MIN_SIZE, out, False, work)
            tool.check_safe(argv)
            import subprocess
            sink = open(out, "w") if tool.stdout_to_file() else subprocess.DEVNULL
            proc = subprocess.run(argv, stdout=sink, stderr=subprocess.PIPE,
                                  text=True, env=tool.env(work, False))
            if sink is not subprocess.DEVNULL:
                sink.close()
            if proc.returncode != 0:
                print("{:<9} FAILED to run: {}".format(
                    tool.key, proc.stderr.strip()[-200:]))
                ok = False
                continue

            groups = tool.parse(tree, out)
            raw_paths[tool.key] = sum(len(g) for g in groups)
            if tool.key == "ours":
                known_good = groups
            exact, detail = toolkit.compare(
                truth, toolkit.groups_as_inodes(tree, groups))
            print("{:<9} {:<8} {}   ({} paths in {} groups)".format(
                tool.key, "exact" if exact else "DIFFERS", detail,
                raw_paths[tool.key], len(groups)))
            if not exact:
                ok = False

        # The documented delta, asserted rather than assumed. Ours and jdupes
        # name one path per inode; fclones names them all, so it prints exactly
        # one more path than they do on this tree — the second name of the
        # hlc cluster.
        print()
        if raw_paths.get("fclones") == raw_paths.get("ours", 0) + 1 \
                and raw_paths.get("jdupes") == raw_paths.get("ours"):
            print("delta holds: fclones printed {} paths where ours and jdupes "
                  "printed {} — the hardlink's second name, as documented in "
                  "tools.py".format(raw_paths["fclones"], raw_paths["ours"]))
        else:
            print("DELTA CHANGED: paths per tool {} — tools.py's hardlink note "
                  "is now wrong and must be rewritten".format(raw_paths))
            ok = False

        if known_good is None:
            print("our tool produced no answer to sabotage", file=sys.stderr)
            ok = False
        elif not check_mutations(tree, truth, known_good):
            ok = False

        print("\nparser verification {}".format("PASSED" if ok else "FAILED"))
        return 0 if ok else 1
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
