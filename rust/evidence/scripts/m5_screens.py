#!/usr/bin/env python3
"""Photograph the browser: drive a session and print what the screen showed.

The unit tests assert on ratatui's own buffer, which proves the layout but is
not something a reviewer can look at. This runs the real binary against a real
tree through a pty and reconstructs each frame from the escape sequences it
wrote, so the transcript below is the screen a user would have seen.

The reconstruction handles only what ratatui emits — absolute cursor moves,
erases, and text; colours are dropped, since a text transcript cannot carry
them. Colour is asserted in the render tests instead.

Usage: m5_screens.py <path-to-dedup-rs> <scratch-dir>
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

ROWS, COLUMNS = 22, 92
SEQUENCE = re.compile(rb"\x1b\[([0-9;?]*)([a-zA-Z])|\x1b([()][B0]|[=>])")


class Screen:
    """Just enough terminal to read a ratatui frame back off the wire."""

    def __init__(self):
        self.grid = [[" "] * COLUMNS for _ in range(ROWS)]
        self.row = 0
        self.column = 0

    def feed(self, data):
        position = 0
        while position < len(data):
            match = SEQUENCE.search(data, position)
            if match is None:
                self.text(data[position:])
                return
            self.text(data[position : match.start()])
            position = match.end()
            if match.group(2) is None:
                continue
            self.control(match.group(1).decode(), match.group(2).decode())

    def control(self, parameters, letter):
        numbers = [int(part) for part in parameters.split(";") if part.isdigit()]
        if letter == "H" or letter == "f":
            self.row = (numbers[0] if numbers else 1) - 1
            self.column = (numbers[1] if len(numbers) > 1 else 1) - 1
        elif letter == "J":
            self.grid = [[" "] * COLUMNS for _ in range(ROWS)]
            self.row = self.column = 0
        elif letter == "K":
            if 0 <= self.row < ROWS:
                for column in range(self.column, COLUMNS):
                    self.grid[self.row][column] = " "

    def text(self, raw):
        for character in raw.decode("utf-8", "replace"):
            if character == "\r":
                self.column = 0
            elif character == "\n":
                self.row += 1
                self.column = 0
            elif character == "\x1b" or ord(character) < 32:
                continue
            else:
                if 0 <= self.row < ROWS and 0 <= self.column < COLUMNS:
                    self.grid[self.row][self.column] = character
                self.column += 1

    def render(self):
        lines = ["".join(row).rstrip() for row in self.grid]
        while lines and not lines[-1]:
            lines.pop()
        return "\n".join(lines)


def main():
    # Resolved, because the browser is spawned with its cwd inside the tree.
    binary, scratch = os.path.abspath(sys.argv[1]), sys.argv[2]
    base = os.path.join(scratch, "m5-screens")
    shutil.rmtree(base, ignore_errors=True)
    root, home = os.path.join(base, "tree"), os.path.join(base, "home")
    os.makedirs(os.path.join(root, "backup/photos"))
    os.makedirs(os.path.join(root, "old"))
    os.makedirs(home)
    for name in ("photos/beach.jpg", "backup/photos/beach.jpg", "old/beach copy.jpg"):
        path = os.path.join(root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as handle:
            handle.write(b"a photograph of a beach, twice over" * 30)
    for name in ("notes.txt", "archive/notes.txt"):
        path = os.path.join(root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as handle:
            handle.write(b"some notes" * 4)
    # A name outside the tree pins one copy: the scan can see it but removing
    # what it can see would free nothing.
    os.link(os.path.join(root, "old/beach copy.jpg"), os.path.join(base, "pinned.jpg"))
    # ...and a second name inside the tree makes a cluster, which the browser
    # shows as one row wearing a badge.
    os.link(os.path.join(root, "notes.txt"), os.path.join(root, "archive/notes-link.txt"))

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
    process = subprocess.Popen(
        [binary, root, "-i", "--min-size", "1", "--report-json", os.path.join(home, "r.json")],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        cwd=root,  # so the browser shows the short relative paths
        env=dict(os.environ, HOME=home, TERM="xterm-256color"),
        start_new_session=True,
    )
    os.close(slave)
    stream = bytearray()
    os.set_blocking(master, False)

    def settle(rounds=6):
        for _ in range(rounds):
            time.sleep(0.1)
            try:
                chunk = os.read(master, 262144)
            except (BlockingIOError, OSError):
                continue
            stream.extend(chunk)
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")

    def shot(caption, keys=()):
        for key in keys:
            os.write(master, key)
            settle(4)
        settle(3)
        # Replayed from the start every time, so a sequence split across two
        # reads is still one sequence.
        screen = Screen()
        screen.feed(bytes(stream))
        print(f"----- {caption} -----")
        print(screen.render())
        print()

    settle(15)
    shot("the group list as it opens; the second copy is pinned from outside")
    shot("space on the pinned copy is refused, and says why", [b"j", b" "])
    shot("a keeps this copy and marks the rest that can actually be freed", [b"k", b"a"])
    shot("the bulk menu", [b"b"])
    shot("the confirmation: the count, the bytes, and Trash rather than gone", [b"\x1b", b"\r"])
    shot("n cancels, and the marks survive it; then the help screen", [b"n", b"?"])
    shot("the second group, where one record wears two names", [b"?", b"l"])
    shot("back, applied: one copy freed, the pinned one still there", [b"h", b"\r", b"y"])

    os.write(master, b"q")
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
    os.close(master)
    print("----- what survived on disk -----")
    for directory, _, names in sorted(os.walk(root)):
        for name in sorted(names):
            print("   ", os.path.relpath(os.path.join(directory, name), root))
    trash = os.path.join(home, ".Trash")
    print("----- what went to the Trash -----")
    for name in sorted(os.listdir(trash)) if os.path.isdir(trash) else []:
        print("   ", name)


if __name__ == "__main__":
    main()
