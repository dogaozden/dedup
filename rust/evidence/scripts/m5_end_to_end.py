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
import json
import os
import pty
import re
import shutil
import struct
import subprocess
import sys
import termios
import time

ENTER = b"\r"
ESC = b"\x1b"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")


def squeeze(output):
    """What the screen said, with the escape sequences and the spacing gone.

    ratatui repaints only the cells that changed and moves the cursor between
    them, so a sentence reaches the pty in pieces with escape sequences in the
    gaps. Searching the raw stream for a phrase therefore fails on text that is
    plainly on screen. Strip the sequences, then the whitespace, and compare
    what is left.
    """
    return "".join(ANSI.sub("", output).split())


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


def drive(binary, root, home, keys, extra=(), size=(30, 100), cwd=None):
    """Run the browser under a pty, send `keys`, return (exit code, output)."""
    master, slave = pty.openpty()
    # A pty starts with no window size, and a terminal of no size is one the
    # browser will not lay out. The menu comes first now and has a floor of its
    # own, so the terminal starts roomy and is shrunk to the size the case
    # asked for once the browser has it — which is a window being resized,
    # the only way a browser reaches a screen too small to read.
    ROOMY = (30, 100)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *ROOMY, 0, 0))
    # Absolute, because a case that runs from inside the tree would otherwise
    # put the run's own report, cache and Trash *in the tree being scanned* and
    # then count them among the survivors.
    home = os.path.abspath(home)
    environment = dict(os.environ, HOME=home, TERM="xterm-256color")
    environment.pop("DEDUP_CACHE_DIR", None)
    environment["DEDUP_CACHE_DIR"] = os.path.join(home, "cache")
    command = [
        os.path.abspath(binary),
        os.path.abspath(root),
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
        cwd=cwd,
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

    # Since M5.1 an interactive launch lands on the pre-flight menu, not on the
    # browser. Enter accepts the form and starts the scan; the keys below are
    # browser keys and must not be typed at a form that would ignore them. The
    # browser is waited for by name, and a case that never reaches it says so
    # rather than timing out with an exit code nobody can read.
    reached = False
    deadline = time.time() + 30
    while time.time() < deadline and process.poll() is None:
        drain()
        seen = output.decode("utf-8", "replace")
        if not reached and "before the scan" in re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", seen):
            time.sleep(0.3)
            os.write(master, ENTER)
            reached = True
            continue
        if reached and seen.count("\x1b[?1049h") >= 2:
            break
        time.sleep(0.05)
    time.sleep(0.4)
    drain()
    if output.decode("utf-8", "replace").count("\x1b[?1049h") < 2:
        # Said out loud, because this is the failure that hides: a screen in
        # front of the browser turns every key below into a test of something
        # else, and the case still prints numbers.
        print("    NOT REACHED: the browser screen never appeared")
    if tuple(size) != ROOMY:
        # ratatui asks the terminal its size on every frame, so the next
        # repaint — 250 ms away at most — is drawn at the new one.
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
        for _ in range(6):
            time.sleep(0.1)
            drain()
    for key in keys:
        # The browser may already have finished — a session that cleans up its
        # last group exits on its own. Writing into the far end of a pty
        # nobody is holding raises EIO, which is an answer, not an error.
        if process.poll() is not None:
            break
        try:
            os.write(master, key)
        except OSError:
            break
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
    seen = "atleastonecopyhastostay" in squeeze(output)
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

    # 7. REVIEW-M5 item 1: the bulk rule must rank the scan's own name.
    case(
        "bulk keep-shortest keeps the copy --auto shortest keeps",
        "b.bin survives both, even run from inside the tree where the drawn "
        "paths rank the other way round",
    )
    root, home = fresh(scratch, "m5-e2e-bulk-shortest")
    build(root, {"b.bin": b"ranked content", "photos/aaaa.bin": b"ranked content"})
    inside = os.path.join(root, "photos")
    code, _ = drive(binary, root, home, [b"b", b"3", ENTER, b"y"], cwd=inside)
    failures += not check("exit", code, 0)
    failures += not check("browser survivors", survivors(root), ["b.bin"])

    # The same tree again, cleaned by --auto from the same directory.
    again, home_again = fresh(scratch, "m5-e2e-bulk-auto")
    # Absolute for the same reason `drive` makes it absolute: this half runs
    # from inside the tree too.
    home_again = os.path.abspath(home_again)
    build(again, {"b.bin": b"ranked content", "photos/aaaa.bin": b"ranked content"})
    subprocess.run(
        [
            os.path.abspath(binary),
            os.path.abspath(again),
            "--min-size",
            "1",
            "--report-json",
            os.path.join(home_again, "r.json"),
            "--auto",
            "shortest",
            "-y",
            "--hard",
        ],
        cwd=os.path.join(again, "photos"),
        capture_output=True,
        env=dict(os.environ, HOME=home_again),
    )
    failures += not check("--auto survivors", survivors(again), ["b.bin"])
    failures += not check(
        "browser and --auto agree", survivors(root), survivors(again)
    )
    print()

    # 8. REVIEW-M5 item 2: no deletion on a screen too small to show the numbers.
    case(
        "a terminal too small to lay out will not confirm a deletion",
        "both copies survive; the frame says to resize rather than asking y/n",
    )
    root, home = fresh(scratch, "m5-e2e-too-small")
    build(root, {"a.bin": b"cramped content", "b.bin": b"cramped content"})
    code, output = drive(binary, root, home, [b" ", ENTER, b"y", b"q"], size=(8, 20))
    failures += not check("exit", code, 0)
    failures += not check("survivors", survivors(root), ["a.bin", "b.bin"])
    failures += not check("trash", trashed(home), [])
    painted = squeeze(output)
    told_to_resize = "Terminaltoosmall" in painted
    never_asked = "Reclaims" not in painted and "MovetoTrash" not in painted
    print(f"    painted the resize message: {told_to_resize}")
    print(f"    never painted a confirmation: {never_asked}")
    failures += not told_to_resize
    failures += not never_asked
    print()

    # 9. REVIEW-M5 non-binding note: a group the cache vouched for is read
    #    again before anything in it goes, and refused whole when it has
    #    stopped being true. Nothing else in the suite reaches that path.
    case(
        "a group the cache vouched for is re-read, disbelieved and refused",
        "the scan reads nothing and still groups them; the browser reads them, "
        "removes nothing, and says why",
    )
    root, home = fresh(scratch, "m5-e2e-attested")
    size = 24 * 1024 * 1024
    body = (b"attested content " * 64)[:1024] * (size // 1024)
    build(root, {"a.bin": body, "b.bin": body})
    # Backdate both copies so the cache is allowed to trust its rows: a row is
    # only believed when the file's mtime tick is strictly older than it.
    stamp = time.time() - 10
    for name in ("a.bin", "b.bin"):
        os.utime(os.path.join(root, name), (stamp, stamp))

    warm = subprocess.run(
        [
            os.path.abspath(binary),
            os.path.abspath(root),
            "--min-size",
            "1",
            "--report-json",
            os.path.join(home, "cold.json"),
        ],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        env=dict(os.environ, HOME=home, DEDUP_CACHE_DIR=os.path.join(home, "cache")),
    )
    failures += not check("cold scan exit", warm.returncode, 0)

    # One byte in the middle, and the clock put back to the nanosecond. Head,
    # tail and every sample point still match, so only a full re-read can tell.
    with open(os.path.join(root, "b.bin"), "r+b") as handle:
        handle.seek(size // 2)
        handle.write(b"X")
    os.utime(os.path.join(root, "b.bin"), (stamp, stamp))

    code, output = drive(binary, root, home, [b"a", ENTER, b"y", b"q"])
    with open(os.path.join(home, "report.json")) as handle:
        report = json.load(handle)
    failures += not check("exit", code, 0)
    failures += not check("the scan still grouped them", len(report["groups"]), 1)
    print(f"    the scan read {report['bytes_read']} bytes to do it")
    believed = report["bytes_read"] < size
    print(f"    grouped without reading them: {believed}")
    failures += not believed
    failures += not check("survivors", survivors(root), ["a.bin", "b.bin"])
    failures += not check("trash", trashed(home), [])
    said = squeeze(output)
    refused = "Refusingtotouch" in said and "nolongermatch" in said
    print(f"    said it was refusing, and why: {refused}")
    failures += not refused
    print()

    # 10. No terminal, no browser.
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
