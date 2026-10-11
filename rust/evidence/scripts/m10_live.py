#!/usr/bin/env python3
"""M10 phase 2 through a real terminal: folder groups in the live browser.

Builds on m8_live_end_to_end.py's pty driver, so the keys are the bytes a
keyboard sends and the screen is what a person would read. Two cases:

  * apply: the live browser opens, the folder candidate appears labelled
    `candidate`, the user marks the twin folder, confirms, and the folder is in
    the Trash as one directory with its twin intact and nothing else touched.

  * refusal: the same, but a file is added to the twin AFTER the candidate has
    appeared and BEFORE the apply. The apply proves the folder again, finds the
    change, refuses that folder group whole, says so on screen, and leaves both
    folders where they were.

Nothing is mocked. The run uses a scratch HOME, so its Trash is a scratch
directory and the real ~/.Trash is never named, listed or changed. The tree is
built under the scratch root and removed afterwards.

Usage: m10_live.py <path-to-dedup-rs> <scratch-dir>
"""

import os
import random
import shutil
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import m8_live_end_to_end as m8  # noqa: E402

ENTER = m8.ENTER


def build(root, nonce):
    """Two identical folders, each with a file over the default 10 MiB floor, a
    small file, and a stray .DS_Store that differs. Nothing else."""
    rng = random.Random(nonce)
    big = rng.randbytes(10_500_000)
    tiny = rng.randbytes(5000)
    kept = "Holiday " + nonce
    twin = kept + " copy"
    for copy in (kept, twin):
        os.makedirs(os.path.join(root, copy, "sub"))
        with open(os.path.join(root, copy, "big.jpg"), "wb") as fh:
            fh.write(big)
        with open(os.path.join(root, copy, "notes.txt"), "w") as fh:
            fh.write("same note\n")
        with open(os.path.join(root, copy, "sub", "tiny.bin"), "wb") as fh:
            fh.write(tiny)
    with open(os.path.join(root, kept, ".DS_Store"), "wb") as fh:
        fh.write(b"view from the first copy")
    with open(os.path.join(root, twin, ".DS_Store"), "wb") as fh:
        fh.write(b"view from the second copy")
    m8.age(root)
    return kept, twin


def folder_shown(twin):
    """The candidate folder group is on screen: labelled, with its twin's row."""
    return lambda text: ("candidate" in m8.squeeze(text)
                         and m8.squeeze(twin) + "/" in m8.squeeze(text))


def tree_snapshot(base):
    out = {}
    for dirpath, _, names in os.walk(base):
        for name in names:
            path = os.path.join(dirpath, name)
            with open(path, "rb") as fh:
                out[os.path.relpath(path, base)] = fh.read()
    return out


def run_case(binary, scratch, nonce, refuse, finished=False):
    root = os.path.join(scratch, "live-%s-%s" % ("refusal" if refuse else "apply", nonce))
    home = root + "-home"
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(root)
    os.makedirs(os.path.join(home, ".Trash"))
    kept, twin = build(root, nonce)
    before = tree_snapshot(root)
    problems = []

    added = []

    def add_a_file_once(text):
        """Runs as a key's wait condition, so it fires the moment the candidate
        is on screen: the scan has settled, and the apply has not yet begun."""
        if not folder_shown(twin)(text):
            return False
        if not added:
            with open(os.path.join(root, twin, "late.txt"), "w") as fh:
                fh.write("arrived after the scan\n")
            added.append(True)
        return True

    # The finished-scan browser is the one a `--report-json` run opens on a
    # terminal: the scan is complete, so its folder group is proven, not a
    # candidate, and the apply has nothing to wait for.
    if finished:
        shown = lambda text: m8.squeeze(twin) + "/" in m8.squeeze(text)  # noqa: E731
    else:
        shown = folder_shown(twin)
    first = add_a_file_once if refuse else shown
    keys = [
        (m8.at_menu, ENTER),
        (lambda t: m8.at_browser(t) and first(t), b"j"),
        (lambda t: shown(t), b" "),
        (lambda t: "marked" in m8.squeeze(t), ENTER),
        (lambda t: "Confirm" in m8.squeeze(t), b"y"),
    ]
    if refuse:
        keys.append((lambda t: "refused" in m8.squeeze(t), b"q"))
    extra = ["--report-json", os.path.join(scratch, "finished-%s.json" % nonce)] if finished else []
    code, text, seen = m8.drive(binary, root, home, keys, extra=extra)
    if extra:
        os.remove(extra[1])

    if not shown(text):
        problems.append("the folder group never reached the screen")
    if finished and "candidate" in m8.squeeze(text):
        problems.append("a finished scan's folder group was labelled a candidate")
    if code != 0:
        problems.append("exit %d, wanted 0" % code)

    after = tree_snapshot(root)
    trashed = os.path.join(home, ".Trash", twin)
    if refuse:
        if not added:
            problems.append("the file was never added: the candidate did not appear")
        if "refused" not in m8.squeeze(text):
            problems.append("the screen never said the folder group was refused")
        if os.path.exists(trashed):
            problems.append("the refused folder reached the Trash")
        if not os.path.isdir(os.path.join(root, twin)):
            problems.append("the refused folder left the tree")
        if after.get(os.path.join(twin, "late.txt")) != b"arrived after the scan\n":
            problems.append("the file added after the scan is not where it was put")
        for name, body in before.items():
            if name.startswith(os.path.join(twin, "late")):
                continue
            if after.get(name) != body:
                problems.append("%s changed in a run that should have refused" % name)
    else:
        if os.path.exists(os.path.join(root, twin)):
            problems.append("the twin folder is still in the tree")
        if not os.path.isdir(trashed) or os.path.islink(trashed):
            problems.append("the twin is not in the Trash as one directory")
        else:
            moved = tree_snapshot(trashed)
            expected = {name[len(twin) + 1:]: body for name, body in before.items()
                        if name.startswith(twin + os.sep)}
            if moved != expected:
                problems.append("the moved folder is not byte-identical to the twin")
        kept_after = {name: body for name, body in after.items()
                      if name.startswith(kept + os.sep)}
        kept_before = {name: body for name, body in before.items()
                       if name.startswith(kept + os.sep)}
        if kept_after != kept_before:
            problems.append("the kept folder is not byte-identical to before")
        if any(not name.startswith((kept + os.sep, twin + os.sep)) for name in after):
            problems.append("something outside the two folders is still there, or moved")
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(home, ignore_errors=True)
    return problems


CASES = [
    ("a marked folder applies: one directory to the Trash, twin intact",
     lambda binary, scratch, nonce: run_case(binary, scratch, nonce, refuse=False)),
    ("a file added after the scan refuses the folder group whole",
     lambda binary, scratch, nonce: run_case(binary, scratch, nonce, refuse=True)),
    ("the finished-scan browser applies a folder the same way",
     lambda binary, scratch, nonce: run_case(binary, scratch, nonce, refuse=False, finished=True)),
]


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary = os.path.abspath(sys.argv[1])
    scratch = os.path.abspath(sys.argv[2])
    os.makedirs(scratch, exist_ok=True)
    failures = 0
    for name, case in CASES:
        nonce = uuid.uuid4().hex[:8]
        problems = case(binary, scratch, nonce)
        if problems:
            failures += 1
            print("FAIL  %s" % name)
            for problem in problems:
                print("        %s" % problem)
        else:
            print("pass  %s" % name)
    print()
    print("%d case(s), %d failed" % (len(CASES), failures))
    return failures


if __name__ == "__main__":
    sys.exit(main())
