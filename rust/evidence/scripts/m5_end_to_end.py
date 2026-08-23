#!/usr/bin/env python3
"""Drive the real browser through a real terminal and check the disk afterwards.

A TUI cannot be conformance-gated, so this is the substitute the M5 spec asks
for: scripted keystrokes into a pty, the actual `dedup-rs` binary on the other
end, and assertions about which files still exist when it exits.

Nothing here is a mock. The binary is the release build, the terminal is a
pty, the keystrokes are the bytes a keyboard sends, and the trees are
generated fresh under the session scratchpad. HOME is pointed at a scratch
directory so Trash placement can be checked without touching the user's own
Trash.

Usage: m5_end_to_end.py <path-to-dedup-rs> <scratch-dir>
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

ENTER = b"\r"
ESC = b"\x1b"


def build(root, files, links=()):
    """Write a tree. `files` is name -> bytes; `links` is (existing, new)."""
    for name, content in files.items():
        path = os.path.join(root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as handle:
            handle.write(content)
    for existing, new in links:
        target = os.path.join(root, new)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        os.link(os.path.join(root, existing), target)


def survivors(root):
    found = []
    for directory, _, names in os.walk(root):
        for name in names:
            found.append(os.path.relpath(os.path.join(directory, name), root))
    return sorted(found)


def trashed(home):
    trash = os.path.join(home, ".Trash")
    if not os.path.isdir(trash):
        return []
    return sorted(os.listdir(trash))


def drive(binary, root, home, keys, extra=()):
    """Run the browser under a pty, send `keys`, return (exit code, output)."""
    master, slave = pty.openpty()
    # A pty starts with no window size, and a terminal of no size is one the
    # browser will not lay out. Give it the same 100x30 every case uses.
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    environment = dict(os.environ, HOME=home, TERM="xterm-256color")
    environment.pop("DEDUP_CACHE_DIR", None)
    environment["DEDUP_CACHE_DIR"] = os.path.join(home, "cache")
    command = [
        binary,
        root,
        "-i",
        "--min-size",
        "1",
        "--report-json",
        os.path.join(home, "report.json"),
        *extra,
    ]
    process = subprocess.Popen(
        command,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=environment,
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
        # A terminal answers when it is asked where the cursor is. Nothing
        # else on this pty will, and the browser waits for the reply.
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")

    for _ in range(15):  # let the scan finish and the first frame paint
        time.sleep(0.1)
        drain()
    for key in keys:
        os.write(master, key)
        for _ in range(4):
            time.sleep(0.08)
            drain()
    try:
        code = process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        code = process.wait()
        output.extend(b"\n[the browser did not exit; killed]")
    drain()
    os.close(master)
    return code, output.decode("utf-8", "replace")


def case(name, expectation):
    print(f"=== {name} ===")
    print(f"    expect: {expectation}")


def check(label, actual, expected):
    verdict = "ok" if actual == expected else "DIFFERENT"
    print(f"    {label}: {actual!r} ({verdict})")
    return actual == expected


def fresh(scratch, name):
    base = os.path.join(scratch, name)
    shutil.rmtree(base, ignore_errors=True)
    root = os.path.join(base, "tree")
    home = os.path.join(base, "home")
    os.makedirs(root)
    os.makedirs(home)
    return root, home


def main():
    binary, scratch = sys.argv[1], sys.argv[2]
    os.makedirs(scratch, exist_ok=True)
    failures = 0

    # 1. Mark every copy but the highlighted one, confirm, and check the disk.
    case(
        "confirmed deletion moves exactly the marked copies to the Trash",
        "a.bin and x.dat survive; the other three are in the scratch Trash",
    )
    root, home = fresh(scratch, "m5-e2e-confirm")
    build(
        root,
        {
            "a.bin": b"the first content" * 8,
            "b.bin": b"the first content" * 8,
            "c.bin": b"the first content" * 8,
            "x.dat": b"second content",
            "y.dat": b"second content",
        },
    )
    code, _ = drive(binary, root, home, [b"a", ENTER, b"y", b"a", ENTER, b"y"])
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["a.bin", "x.dat"])
    failures += not check("trash", trashed(home), ["b.bin", "c.bin", "y.dat"])
    print()

    # 2. The confirmation is not a formality.
    case(
        "cancelling the confirmation removes nothing",
        "all three files survive and the Trash is empty",
    )
    root, home = fresh(scratch, "m5-e2e-cancel")
    build(root, {n: b"identical bytes here" for n in ("a.bin", "b.bin", "c.bin")})
    code, _ = drive(binary, root, home, [b"a", ENTER, b"n", b"q"])
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["a.bin", "b.bin", "c.bin"])
    failures += not check("trash", trashed(home), [])
    print()

    # 3. Quitting with marks outstanding is not a deletion either.
    case(
        "quitting with copies marked removes nothing",
        "all three files survive",
    )
    root, home = fresh(scratch, "m5-e2e-quit")
    build(root, {n: b"identical bytes here" for n in ("a.bin", "b.bin", "c.bin")})
    code, _ = drive(binary, root, home, [b"a", b"q"])
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["a.bin", "b.bin", "c.bin"])
    failures += not check("trash", trashed(home), [])
    print()

    # 4. --hard means gone, not moved.
    case(
        "--hard deletes permanently and puts nothing in the Trash",
        "a.bin survives, b.bin is gone, the Trash stays empty",
    )
    root, home = fresh(scratch, "m5-e2e-hard")
    build(root, {"a.bin": b"permanent content", "b.bin": b"permanent content"})
    code, _ = drive(binary, root, home, [b"a", ENTER, b"y"], extra=["--hard"])
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["a.bin"])
    failures += not check("trash", trashed(home), [])
    print()

    # 5. The survivor guard, driven from the keyboard: space on every copy.
    case(
        "space on every copy in a group cannot empty it",
        "the last copy refuses to be marked, so one survives the confirmation",
    )
    root, home = fresh(scratch, "m5-e2e-guard")
    build(root, {n: b"guarded content xx" for n in ("a.bin", "b.bin", "c.bin")})
    keys = [b" ", b"j", b" ", b"j", b" ", ENTER, b"y"]
    code, output = drive(binary, root, home, keys)
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["c.bin"])
    failures += not check("trash", trashed(home), ["a.bin", "b.bin"])
    seen = "at least one copy has to stay" in output
    print(f"    refusal shown on screen: {seen} ({'ok' if seen else 'DIFFERENT'})")
    failures += not seen
    print()

    # 6. A hardlink cluster goes as a unit.
    case(
        "removing a hardlinked record removes every name it wears",
        "both names of the cluster go together; the other copy survives",
    )
    root, home = fresh(scratch, "m5-e2e-links")
    build(
        root,
        {"a.bin": b"clustered content", "z.bin": b"clustered content"},
        links=[("a.bin", "sub/also-a.bin")],
    )
    # The cluster is the shortest name, so it is member 0; keep z.bin instead.
    code, _ = drive(binary, root, home, [b"j", b"a", ENTER, b"y"])
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["z.bin"])
    failures += not check("trash", trashed(home), ["a.bin", "also-a.bin"])
    print()

    # 7. No terminal, no browser.
    case(
        "-i without a terminal refuses instead of guessing",
        "exit 2, a message naming --list and --auto, and nothing removed",
    )
    root, home = fresh(scratch, "m5-e2e-notty")
    build(root, {n: b"piped content here" for n in ("a.bin", "b.bin")})
    piped = subprocess.run(
        [binary, root, "-i", "--min-size", "1", "--report-json", os.path.join(home, "r.json")],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        env=dict(os.environ, HOME=home),
    )
    failures += not check("exit", piped.returncode, 2)
    print(f"    stderr: {piped.stderr.strip()}")
    failures += not check("survivors", survivors(root), ["a.bin", "b.bin"])
    print()

    print(f"{failures} check(s) differing")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
