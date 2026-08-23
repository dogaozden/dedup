#!/usr/bin/env python3
"""Check that the browser hands the terminal back the way it found it.

M5's fourth hard rule is that raw mode and the alternate screen are released
on every exit path. This watches from outside: it reads the pty's termios
before, during and after a session, and looks for the alternate-screen enter
and leave sequences in what the program wrote.

Usage: m5_terminal_restore.py <path-to-dedup-rs> <scratch-dir>
"""

import fcntl
import os
import pty
import shutil
import struct
import subprocess
import sys
import termios
import time

ALT_ENTER = b"\x1b[?1049h"
ALT_LEAVE = b"\x1b[?1049l"


def modes(fd):
    """(canonical input, echo) as the line discipline currently has them."""
    attributes = termios.tcgetattr(fd)
    local = attributes[3]
    return bool(local & termios.ICANON), bool(local & termios.ECHO)


def session(binary, root, home, keys):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    watcher = os.dup(slave)  # kept open so the modes can still be read after
    process = subprocess.Popen(
        [binary, root, "-i", "--min-size", "1", "--report-json", os.path.join(home, "r.json")],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=dict(os.environ, HOME=home, TERM="xterm-256color"),
        start_new_session=True,
    )
    os.close(slave)

    output = bytearray()
    os.set_blocking(master, False)

    def drain():
        try:
            chunk = os.read(master, 262144)
        except (BlockingIOError, OSError):
            return
        output.extend(chunk)
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")

    before = modes(watcher)
    for _ in range(15):
        time.sleep(0.1)
        drain()
    during = modes(watcher)
    for key in keys:
        os.write(master, key)
        for _ in range(4):
            time.sleep(0.08)
            drain()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
    drain()
    after = modes(watcher)
    os.close(watcher)
    os.close(master)
    return before, during, after, bytes(output)


def report(label, before, during, after, output):
    raw_entered = during != before and during[0] is False
    print(f"=== {label} ===")
    print(f"    modes before  (canonical, echo): {before}")
    print(f"    modes during  (canonical, echo): {during}")
    print(f"    modes after   (canonical, echo): {after}")
    print(f"    raw mode was entered: {raw_entered}")
    print(f"    raw mode was released: {after == before}")
    print(f"    alternate screen entered: {ALT_ENTER in output}")
    print(f"    alternate screen left: {ALT_LEAVE in output}")
    print(f"    left after it entered: {output.rfind(ALT_LEAVE) > output.rfind(ALT_ENTER)}")
    bad = 0
    bad += not raw_entered
    bad += after != before
    bad += ALT_ENTER not in output
    bad += ALT_LEAVE not in output
    bad += output.rfind(ALT_LEAVE) <= output.rfind(ALT_ENTER)
    return bad


def fresh(scratch, name):
    base = os.path.join(scratch, name)
    shutil.rmtree(base, ignore_errors=True)
    root, home = os.path.join(base, "tree"), os.path.join(base, "home")
    os.makedirs(root)
    os.makedirs(home)
    for name in ("a.bin", "b.bin"):
        with open(os.path.join(root, name), "wb") as handle:
            handle.write(b"content the browser will show" * 4)
    return root, home


def main():
    binary, scratch = sys.argv[1], sys.argv[2]
    os.makedirs(scratch, exist_ok=True)
    failures = 0

    root, home = fresh(scratch, "m5-restore-quit")
    failures += report("quitting with q", *session(binary, root, home, [b"q"]))
    print()

    root, home = fresh(scratch, "m5-restore-interrupt")
    failures += report("interrupting with ctrl-c", *session(binary, root, home, [b"\x03"]))
    print()

    root, home = fresh(scratch, "m5-restore-delete")
    failures += report(
        "exiting because nothing is left to browse",
        *session(binary, root, home, [b"a", b"\r", b"y"]),
    )
    print()

    print(f"{failures} check(s) differing")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
