#!/usr/bin/env python3
"""Drive the real pre-flight menu through a real terminal.

The menu decides what a scan is, so the thing worth checking is not what it
draws but what the scan afterwards actually did. Every case here reads the
report the binary wrote and asserts on that.

Nothing is mocked: the release binary, a pty, the bytes a keyboard sends, and
trees generated fresh under the session scratchpad.

Usage: m5_1_menu_end_to_end.py <path-to-dedup-rs> <scratch-dir>
"""

import fcntl
import json
import os
import pty
import random
import re
import shutil
import struct
import subprocess
import sys
import termios
import time

ENTER = b"\r"
ESC = b"\x1b"
SPACE = b" "
DOWN = b"\x1b[B"
RUB = b"\x7f"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")

MIB = 1024 * 1024


def squeeze(output):
    """What the screen said, with escape sequences and spacing gone."""
    return "".join(ANSI.sub("", output).split())


def build(root):
    """Two duplicate pairs, one above a megabyte and one far below it."""
    rng = random.Random(7)
    page = bytes(rng.getrandbits(8) for _ in range(4096))
    for name, size in (("big-a.bin", 2 * MIB), ("big-b.bin", 2 * MIB),
                       ("small-a.bin", 4096), ("small-b.bin", 4096)):
        with open(os.path.join(root, name), "wb") as fh:
            fh.write(page * (size // 4096))
    # ...and one file that matches nobody, so a group is never everything.
    with open(os.path.join(root, "lonely.bin"), "wb") as fh:
        fh.write(bytes(rng.getrandbits(8) for _ in range(4096)))
    # Backdated, so a cache row written a moment from now outranks them and a
    # warm scan is allowed to trust it.
    old = time.time() - 3600
    for name in os.listdir(root):
        os.utime(os.path.join(root, name), (old, old))


def drive(binary, root, home, keys, extra=(), size=(30, 100)):
    """Run under a pty, send `keys`, return (exit code, screen text)."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    environment = dict(os.environ, HOME=home, TERM="xterm-256color")
    environment["DEDUP_CACHE_DIR"] = os.path.join(home, "cache")
    command = [os.path.abspath(binary), os.path.abspath(root), *extra]
    process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave,
                               env=environment, start_new_session=True)
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

    for _ in range(8):  # the menu paints before anything is scanned
        time.sleep(0.1)
        drain()
    for key in keys:
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
        code = process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.kill()
        code = process.wait()
        output.extend(b"\n[the menu did not exit; killed]")
    drain()
    os.close(master)
    return code, output.decode("utf-8", "replace")


def report(home):
    path = os.path.join(home, "report.json")
    if not os.path.exists(path):
        return None
    with open(path) as fh:
        return json.load(fh)


def groups_of(obj):
    return sorted(tuple(sorted(group)) for group in obj["groups"])


def typed(text):
    return [bytes([c]) for c in text.encode()]


def case_change_min_size(binary, base):
    """Edit the field, scan, and check the groups reflect what was typed."""
    root, home = fresh(base, "min-size")
    keys = [SPACE] + [RUB] + typed("1048576") + [ENTER, ENTER, b"q"]
    code, screen = drive(binary, root, home, keys,
                         extra=["-i", "--min-size", "1", "--no-cache",
                                "--report-json", os.path.join(home, "report.json")])
    said = report(home)
    if said is None:
        return "FAIL: no report was written (exit {})".format(code)
    got = groups_of(said)
    want = [("big-a.bin", "big-b.bin")]
    if got != want:
        return "FAIL: reported {}, wanted {}".format(got, want)
    if "min-size" not in squeeze(screen):
        return "FAIL: the menu never drew its min-size row"
    return "ok (1 group; the 4 KiB pair was filtered out by the typed value)"


def case_enter_through(binary, base):
    """One Enter must be the same run as no menu at all."""
    root, home = fresh(base, "through")
    through = os.path.join(home, "report.json")
    code, _ = drive(binary, root, home, [ENTER, b"q"],
                    extra=["-i", "--min-size", "1", "--no-cache",
                           "--report-json", through])
    if code != 0:
        return "FAIL: the enter-through run exited {}".format(code)
    menu_groups = groups_of(report(home))

    plain = os.path.join(home, "plain.json")
    subprocess.run([os.path.abspath(binary), os.path.abspath(root),
                    "--min-size", "1", "--no-cache", "--report-json", plain],
                   check=True, capture_output=True,
                   env=dict(os.environ, HOME=home,
                            DEDUP_CACHE_DIR=os.path.join(home, "cache")))
    with open(plain) as fh:
        piped_groups = groups_of(json.load(fh))
    if menu_groups != piped_groups:
        return "FAIL: menu gave {}, the piped run gave {}".format(
            menu_groups, piped_groups)
    return "ok ({} groups, identical to the piped run)".format(len(menu_groups))


def case_enter_through_flagless(binary, base):
    """The same, with no flags at all — the tool's real defaults."""
    root, home = fresh(base, "flagless")
    # One pair above the 10 MiB default, so a flagless run has something to
    # find; the small pair is below it and must be absent from both.
    rng = random.Random(11)
    page = bytes(rng.getrandbits(8) for _ in range(4096))
    for name in ("huge-a.bin", "huge-b.bin"):
        with open(os.path.join(root, name), "wb") as fh:
            fh.write(page * ((11 * MIB) // 4096))

    through = os.path.join(home, "report.json")
    code, _ = drive(binary, root, home, [ENTER, b"q"],
                    extra=["-i", "--report-json", through])
    if code != 0:
        return "FAIL: the enter-through run exited {}".format(code)
    menu_groups = groups_of(report(home))

    plain = os.path.join(home, "plain.json")
    subprocess.run([os.path.abspath(binary), os.path.abspath(root),
                    "--report-json", plain], check=True, capture_output=True,
                   env=dict(os.environ, HOME=home,
                            DEDUP_CACHE_DIR=os.path.join(home, "cache")))
    with open(plain) as fh:
        piped_groups = groups_of(json.load(fh))
    if menu_groups != piped_groups or menu_groups != [("huge-a.bin", "huge-b.bin")]:
        return "FAIL: menu {}, piped {}".format(menu_groups, piped_groups)
    return "ok ({}, the 10 MiB default honoured on both sides)".format(menu_groups[0])


def case_escape_scans_nothing(binary, base):
    root, home = fresh(base, "escape")
    where = os.path.join(home, "report.json")
    code, _ = drive(binary, root, home, [ESC],
                    extra=["-i", "--min-size", "1", "--no-cache",
                           "--report-json", where])
    if code != 0:
        return "FAIL: expected exit 0, got {}".format(code)
    if os.path.exists(where):
        return "FAIL: a report was written by a run that was cancelled"
    return "ok (exit 0, no report, nothing read)"


def case_toggle_a_switch(binary, base):
    """Space on the cache row must reach the options.

    Asserted on what the scan did, not on what the screen said. ratatui
    repaints only the cells that changed, so a phrase drawn into a later frame
    never reaches the pty whole and searching for it proves nothing either way
    — the mistake REVIEW-M5 caught in its own harness before it caught
    anything in the program.

    A warm cache reads almost nothing. `--no-cache` reads everything. So the
    byte count says whether the switch arrived.
    """
    root, home = fresh(base, "switch")
    where = os.path.join(home, "report.json")
    flags = ["-i", "--min-size", "1", "--report-json", where]

    # Warm it, through the menu, with the switch left alone.
    drive(binary, root, home, [ENTER, b"q"], extra=flags)
    cold = report(home)["bytes_read"]
    drive(binary, root, home, [ENTER, b"q"], extra=flags)
    warm = report(home)["bytes_read"]
    if warm >= cold:
        return "FAIL: the cache never warmed ({} then {} bytes)".format(cold, warm)

    # Now six downs to the no-cache row, space, and scan.
    code, _ = drive(binary, root, home, [DOWN] * 6 + [SPACE, ENTER, b"q"],
                    extra=flags)
    if code != 0 or report(home) is None:
        return "FAIL: exit {} with no report".format(code)
    ignored = report(home)["bytes_read"]
    if ignored <= warm:
        return ("FAIL: the switch did not reach the scan — warm read {} bytes, "
                "the toggled run read {}".format(warm, ignored))
    return ("ok (cold {} bytes, warm {}, switch on {} — the toggle reached the "
            "options)".format(cold, warm, ignored))


def case_pipe_never_sees_it(binary, base):
    """Not a terminal, not a menu — and the report is the old report."""
    root, home = fresh(base, "piped")
    where = os.path.join(home, "report.json")
    proc = subprocess.run(
        [os.path.abspath(binary), os.path.abspath(root), "--min-size", "1",
         "--no-cache", "--list", "--report-json", where],
        capture_output=True, text=True, timeout=60,
        env=dict(os.environ, HOME=home, DEDUP_CACHE_DIR=os.path.join(home, "cache")))
    if proc.returncode != 0:
        return "FAIL: exit {}".format(proc.returncode)
    if "before the scan" in proc.stdout:
        return "FAIL: the menu was drawn into a pipe"
    if len(groups_of(report(home))) != 2:
        return "FAIL: the piped run found the wrong groups"
    return "ok (2 groups printed, no menu in the output)"


def case_help_exits_zero(binary, base):
    proc = subprocess.run([os.path.abspath(binary), "--help"],
                          capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        return "FAIL: --help exited {} (it used to exit 2)".format(proc.returncode)
    for flag in ("--min-size", "--profile", "--auto", "-h, --help"):
        if flag not in proc.stdout:
            return "FAIL: --help does not mention {}".format(flag)
    short = subprocess.run([os.path.abspath(binary), "-h"],
                           capture_output=True, text=True, timeout=30)
    if short.stdout != proc.stdout or short.returncode != 0:
        return "FAIL: -h and --help disagree"
    return "ok (exit 0, {} lines, -h identical)".format(len(proc.stdout.splitlines()))


def fresh(base, name):
    root = os.path.join(base, name)
    home = os.path.join(base, name + "-home")
    os.makedirs(root)
    os.makedirs(home)
    build(root)
    return root, home


def main() -> int:
    if len(sys.argv) != 3:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    binary, base = sys.argv[1], sys.argv[2]
    base = os.path.join(base, "feat-m5_1-menu")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    cases = [
        ("--help exits 0 and lists the flags", case_help_exits_zero),
        ("a piped run never sees the menu", case_pipe_never_sees_it),
        ("enter-through equals the piped run", case_enter_through),
        ("enter-through with no flags at all", case_enter_through_flagless),
        ("typing a min-size changes the groups", case_change_min_size),
        ("space toggles the cache switch", case_toggle_a_switch),
        ("esc scans nothing", case_escape_scans_nothing),
    ]
    failed = 0
    for name, run in cases:
        answer = run(binary, base)
        failed += answer.startswith("FAIL")
        print("{:<40} {}".format(name, answer))
    shutil.rmtree(base, ignore_errors=True)
    print()
    if failed:
        print("{} case(s) wrong".format(failed))
        return 1
    print("every case behaves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
