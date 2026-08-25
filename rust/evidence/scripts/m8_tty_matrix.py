#!/usr/bin/env python3
"""What each flag combination actually does ON A TERMINAL.

REVIEW-M8 item 3's second half. The freeze proof is 84 non-interactive cases,
so it cannot see the terminal side at all: `has_terminal()` is false in every
one of them and the rest of `live_wanted` is never evaluated. `live_flags` is
now unit-tested, but a unit test asks the function what it thinks — this asks
the binary what it does.

Every case runs on a real pty (winsize set, `ESC[6n` answered, the pre-flight
form accepted) and reports four things: the exit code, how many alternate
screens were entered, whether any group was ever labelled `candidate` — which
only the live browser does — and whether the report file was written.

The expectations are the fence, spelled out per row:

  --list, --auto, --report-json and --stage size stay on the FROZEN path on a
  terminal, with the report still written where one was asked for.

  --list --interactive does NOT, and that is deliberate: `-i` means "browse
  them" and has always overruled `--list`. REVIEW-M8 item 4 asked for that to
  be written down rather than changed, and this row is where it is measured.

Nothing here touches /Volumes. The corpus lives under the scratch directory.

usage: m8_tty_matrix.py <path-to-dedup-rs> <scratch-dir>
"""
import fcntl
import os
import pty
import random
import re
import select
import shutil
import struct
import subprocess
import sys
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")
ALT_ON = b"\x1b[?1049h"
LIMIT = 45.0


def squeeze(raw):
    return b"".join(ANSI.sub(b"", raw).split())


def build(root):
    """Big enough that a live browser would have something to label, small
    enough that every case finishes quickly."""
    rng = random.Random(20260825)
    page = bytes(rng.getrandbits(8) for _ in range(65536))
    for pair in range(4):
        body = bytes([pair + 1]) * 65536 + page * 32
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(body)
    when = time.time() - 10
    for name in os.listdir(root):
        os.utime(os.path.join(root, name), (when, when))


def run(binary, tree, home, argv, report):
    """One case, on a pty, with the pre-flight form accepted if it appears."""
    if os.path.exists(report):
        os.remove(report)
    master, follower = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    process = subprocess.Popen(
        [binary, tree, "--min-size", "1", *argv],
        stdin=follower, stdout=follower, stderr=follower,
        env=dict(os.environ, HOME=home, TERM="xterm-256color",
                 DEDUP_CACHE_DIR=os.path.join(home, "cache")),
        start_new_session=True)
    os.close(follower)

    seen = bytearray()
    os.set_blocking(master, False)
    began = time.time()
    accepted = False
    quit_sent = False
    while time.time() - began < LIMIT:
        ready, _, _ = select.select([master], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(master, 262144)
                seen.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            except OSError:
                pass
        if process.poll() is not None:
            break
        flat = squeeze(seen)
        if not accepted and b"dedup-rs\xe2\x80\x94beforethescan" in flat:
            os.write(master, b"\r")
            accepted = True
            time.sleep(0.3)
            continue
        # Anything that opened a browser is waiting for a key; give it a
        # moment to draw a group first so the label can be seen.
        if accepted and not quit_sent and b"groups\xc2\xb7" in flat:
            time.sleep(0.6)
            try:
                seen.extend(os.read(master, 262144))
            except OSError:
                pass
            os.write(master, b"q")
            quit_sent = True
        time.sleep(0.02)
    alive = process.poll() is None
    if alive:
        process.kill()
    code = process.wait()
    try:
        os.close(master)
    except OSError:
        pass
    text = bytes(seen)
    return {
        "exit": "killed" if alive else code,
        "alt": text.count(ALT_ON),
        "candidate": b"candidate" in squeeze(text),
        "report": os.path.exists(report),
    }


# (label, argv, live path expected, report expected)
CASES = [
    ("bare interactive", [], True, False),
    ("--list", ["--list"], False, False),
    ("--list --interactive", ["--list", "--interactive"], True, False),
    ("--report-json", ["--report-json", "{report}"], False, True),
    # Without `--list`, so this probes the BROWSER's own refusal of a size
    # scan rather than just printing and stopping — the reviewer's row.
    ("--stage size", ["--stage", "size"], False, False),
    ("--auto shortest --yes", ["--auto", "shortest", "--yes"], False, False),
]


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary, scratch = os.path.abspath(sys.argv[1]), sys.argv[2]
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(scratch)
    report = os.path.join(scratch, "report.json")

    print("%-24s %8s %5s %10s %8s   %s"
          % ("case", "exit", "alt", "candidate", "report", "verdict"))
    problems = []
    for label, argv, live, wants_report in CASES:
        tree = os.path.join(scratch, label.replace(" ", "_").replace("-", ""))
        home = tree + "-home"
        os.makedirs(tree)
        os.makedirs(os.path.join(home, ".Trash"))
        build(tree)
        got = run(binary, tree, home,
                  [a.replace("{report}", report) for a in argv], report)

        trouble = []
        if got["candidate"] != live:
            trouble.append("live path %s, wanted %s" % (got["candidate"], live))
        if wants_report and not got["report"]:
            trouble.append("no report was written")
        if got["exit"] == "killed":
            trouble.append("the run never exited")
        print("%-24s %8s %5d %10s %8s   %s"
              % (label, got["exit"], got["alt"], got["candidate"],
                 got["report"], "; ".join(trouble) if trouble else "as the fence requires"))
        problems.extend("%s: %s" % (label, t) for t in trouble)

    print()
    for problem in problems:
        print("FAIL  %s" % problem)
    if not problems:
        print("pass  --list, --auto, --report-json and --stage size stay on the "
              "frozen path on a terminal;")
        print("      --list --interactive is an interactive run and takes the "
              "live path, as -i has always meant.")
    return len(problems)


if __name__ == "__main__":
    sys.exit(main())
