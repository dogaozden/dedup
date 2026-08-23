#!/usr/bin/env python3
"""Which runs reach the browser, and which are left exactly as they were.

Adding an interactive mode to a tool the conformance harness drives is a chance
to change what a non-interactive run does by accident. This shows every way in
and out: with a terminal and without, with `--list`, with `-i`, and with
nothing duplicated.

Usage: m5_dispatch.py <path-to-dedup-rs> <scratch-dir>
"""

import fcntl
import os
import pty
import re
import shutil
import struct
import subprocess
import sys
import termios
import time

ALT_ENTER = b"\x1b[?1049h"


def tree(base, name, files):
    root = os.path.join(base, name, "tree")
    home = os.path.join(base, name, "home")
    shutil.rmtree(os.path.join(base, name), ignore_errors=True)
    os.makedirs(root)
    os.makedirs(home)
    for filename, content in files.items():
        with open(os.path.join(root, filename), "wb") as handle:
            handle.write(content)
    return root, home


def command(binary, root, home, extra):
    return [
        binary,
        root,
        "--min-size",
        "1",
        "--report-json",
        os.path.join(home, "r.json"),
        *extra,
    ]


def on_a_terminal(binary, root, home, extra, keys):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 90, 0, 0))
    process = subprocess.Popen(
        command(binary, root, home, extra),
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

    for _ in range(12):
        time.sleep(0.1)
        drain()
    for key in keys:
        os.write(master, key)
        for _ in range(3):
            time.sleep(0.08)
            drain()
    try:
        code = process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        code = process.wait()
    drain()
    os.close(master)
    return code, bytes(output)


def through_a_pipe(binary, root, home, extra):
    finished = subprocess.run(
        command(binary, root, home, extra),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        env=dict(os.environ, HOME=home),
    )
    return finished.returncode, finished.stdout + finished.stderr


def report(label, terminal, extra, code, raw):
    plain = re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]", b"", raw).decode("utf-8", "replace")
    words = " ".join(plain.split())
    print(f"=== {label} ===")
    print(f"    terminal: {terminal}   flags: {extra or '(none)'}")
    print(f"    exit: {code}   entered the browser: {ALT_ENTER in raw}")
    print(f"    said: {words[:96]}")
    print()


def main():
    binary, scratch = os.path.abspath(sys.argv[1]), sys.argv[2]
    base = os.path.join(scratch, "m5-dispatch")
    os.makedirs(base, exist_ok=True)
    duplicated = {"a.bin": b"the same content" * 20, "b.bin": b"the same content" * 20}
    distinct = {"a.bin": b"one thing", "b.bin": b"another thing entirely"}

    cases = [
        ("no flags: the oracle's default is the browser", True, [], duplicated, [b"q"]),
        ("--list: prints the groups and stops", True, ["--list"], duplicated, []),
        ("nothing duplicated: says so and stops", True, [], distinct, []),
        ("-i with nothing duplicated: not an error", True, ["-i"], distinct, []),
        ("no flags through a pipe: exactly as before M5", False, [], duplicated, []),
        ("--list through a pipe: exactly as before M5", False, ["--list"], duplicated, []),
        ("-i through a pipe: refuses, and says what to use", False, ["-i"], duplicated, []),
    ]
    for index, (label, terminal, extra, files, keys) in enumerate(cases):
        root, home = tree(base, f"case-{index}", files)
        if terminal:
            code, raw = on_a_terminal(binary, root, home, extra, keys)
        else:
            code, raw = through_a_pipe(binary, root, home, extra)
        report(label, terminal, extra, code, raw)
        survivors = sorted(os.listdir(root))
        if survivors != ["a.bin", "b.bin"]:
            print(f"    !! files went missing: {survivors}")
            return 1
    print("every case left both files where they were")
    return 0


if __name__ == "__main__":
    sys.exit(main())
