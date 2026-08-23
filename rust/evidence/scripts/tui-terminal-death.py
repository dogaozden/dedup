#!/usr/bin/env python3
"""What the full-screen views do when their terminal dies, and that they still
take keys.

An orphaned `dedup-rs -i` was once found burning a whole core for eight hours.
The cause is not in this repository: crossterm's Unix reader answers a
descriptor that is at end-of-file by looping on `read` inside `try_read`, where
a zero-length read produces no event and no error, so `event::read` never
returns and nothing at the call site can catch it. The fix is to ask `poll`
first — `sys::wait_on` — and leave when it reports a hangup.

This drives the real binary on a real pty, because that is the only way to see
the whole thing: the loop, the reader, the terminal and the exit.

**Every screen that owns the terminal is covered separately, and each scenario
asserts which one it reached.** M5.1 put the pre-flight menu in front of the
browser and reintroduced this exact bug there; the earlier version of this
script silently stopped testing the browser at that moment, because `-i` no
longer reaches it before a key is pressed, and it never checked. A test that
does not say which screen it is on stops covering the screen it was written for
the moment somebody adds another.

    keys/menu       a keystroke reaches the menu and `q` leaves it.
    keys/browser    ...and again for the browser, one Enter further in.
    hangup/menu     the terminal dies while the menu is up.
    hangup/browser  ...and again for the browser.

The `keys` cases guard the `hangup` ones: a view that ignored input altogether
would also not spin, and would pass a hangup test on its own.

**The delay before the terminal dies is fixed and past the poll**, so the
process is provably inside `event::read()` when the descriptor goes away.
Closing the master as soon as the paint pauses — what this script used to do —
lands inside the 250 ms poll about half the time, and passes a broken build
that often.

The child is given the pty as its standard input but NOT as its controlling
terminal, which is the orphan the bug lives in. With a controlling terminal the
kernel sends SIGHUP when the master closes and the process dies of that instead,
which is why the bug is invisible from an ordinary shell.

usage: tui-terminal-death.py <path to dedup-rs>            # every scenario
"""
import fcntl
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

WATCH = 5.0        # seconds to give the view to notice and leave
SPIN = 0.20        # CPU-seconds inside WATCH that count as spinning
MIN_SIZE = 4096    # the default is 10 MiB; this corpus is deliberately small
# Comfortably past the 250 ms poll, so the process is inside `event::read()`
# rather than somewhere in `wait_on` when the terminal is taken away.
SETTLE = 1.5
PANIC = 101        # a Rust panic's exit code: leaving is not enough, it has
                   # to leave on purpose


def corpus(root):
    """Two identical files and one that differs: exactly one group to browse."""
    for name in ("a.bin", "copy-of-a.bin"):
        with open(os.path.join(root, name), "wb") as fh:
            fh.write(b"duplicate payload " * 4096)
    with open(os.path.join(root, "other.bin"), "wb") as fh:
        fh.write(b"something else " * 4096)


def cpu_seconds(pid):
    """CPU this pid has burned, from ps. None once it is gone."""
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)],
                         capture_output=True, text=True)
    field = out.stdout.strip()
    if not field:
        return None
    seconds = 0.0
    for part in field.split(":"):
        seconds = seconds * 60 + float(part)
    return seconds


def start(binary, tree):
    """Run the tool on a pty it does not control. Returns (pid, master)."""
    master, follower = pty.openpty()
    # A fresh pty is 0x0, and the browser refuses to act on a screen it could
    # not draw, so without a size it never reaches the event loop at all.
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

    pid = os.fork()
    if pid == 0:
        os.setsid()                    # a new session, and deliberately no
        os.dup2(follower, 0)           # TIOCSCTTY: no controlling terminal,
        os.dup2(follower, 1)           # so no SIGHUP when the master closes
        os.dup2(follower, 2)
        os.close(master)
        if follower > 2:
            os.close(follower)
        os.execv(binary, [binary, tree, "-i", "--min-size", str(MIN_SIZE)])
        os._exit(127)

    os.close(follower)
    return pid, master


def wait_until_painted(master, pid):
    """Read until a frame is drawn, answering the pty like a terminal."""
    painted = b""
    deadline = time.time() + 10.0
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.25)
        if not ready:
            if painted:
                return painted        # drawn, and now waiting on us
            continue
        try:
            chunk = os.read(master, 8192)
        except OSError:
            break
        if not chunk:
            break
        painted += chunk
        # Be a terminal: the view asks where the cursor is and will not
        # start until something answers. A real emulator replies; so do we.
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")
    return painted


def reap(pid, seconds):
    """Wait up to `seconds` for the child.

    Returns (status, waited, cpu). `cpu` comes from the kernel's own accounting
    for the finished child rather than from sampling `ps`, which reports nothing
    at all once the process is gone.
    """
    end = time.time() + seconds
    while time.time() < end:
        done, status, usage = os.wait4(pid, os.WNOHANG)
        if done:
            cpu = usage.ru_utime + usage.ru_stime
            return status, seconds - (end - time.time()), cpu
        time.sleep(0.05)
    return None, seconds, None


def kill(pid):
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)


ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")


def which_screen(painted):
    """Which view drew this frame — asserted by every scenario below.

    The menu says so in its title. The browser's status line carries its own
    keymap, which nothing else draws.
    """
    visible = ANSI.sub("", painted.decode("utf8", "replace"))
    flat = " ".join(visible.split())
    if "before the scan" in flat:
        return "menu"
    if "space mark" in flat or "reclaimable" in flat or "No duplicates left" in flat:
        return "browser"
    return "unknown"


def reach(binary, tree, screen):
    """Start the tool and get to `screen`. Returns (pid, master, problem)."""
    pid, master = start(binary, tree)
    painted = wait_until_painted(master, pid)
    if not painted:
        return pid, master, "nothing was ever painted; nothing was tested"
    got = which_screen(painted)
    if got != "menu":
        return pid, master, "expected the menu first, saw {}".format(got)
    if screen == "menu":
        return pid, master, None

    # One Enter accepts the form; the scan then runs and the browser paints.
    os.write(master, b"\r")
    painted = wait_until_painted(master, pid)
    got = which_screen(painted)
    if got != "browser":
        return pid, master, ("expected the browser after Enter, saw {} — this "
                             "scenario did not test what it says".format(got))
    return pid, master, None


def scenario_keys(binary, tree, screen):
    """A keystroke still arrives, and `q` still leaves."""
    pid, master, problem = reach(binary, tree, screen)
    if problem:
        kill(pid)
        os.close(master)
        return False, problem

    os.write(master, b"q")
    status, waited, _cpu = reap(pid, WATCH)
    if status is None:
        kill(pid)
        os.close(master)
        return False, "'q' did not leave the {} within {:.0f}s".format(screen, WATCH)
    os.close(master)
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        return False, "'q' left the {} with exit code {} rather than 0".format(screen, code)
    return True, "'q' left the {} in {:.2f}s, exit 0".format(screen, waited)


def scenario_hangup(binary, tree, screen):
    """The terminal dies; the view must go, quickly, cheaply, and on purpose."""
    pid, master, problem = reach(binary, tree, screen)
    if problem:
        kill(pid)
        os.close(master)
        return False, problem

    # Provably past the poll and inside the blocking read.
    time.sleep(SETTLE)
    before = cpu_seconds(pid) or 0.0
    os.close(master)                   # the terminal dies here
    status, waited, cpu = reap(pid, WATCH)

    if status is None:
        # Still alive, so the kernel will not account for it yet: sample.
        spent = (cpu_seconds(pid) or 0.0) - before
        kill(pid)
        return False, ("the {} was still running {:.0f}s after the terminal "
                       "died, {:.2f} CPU-seconds in that window — {}"
                       .format(screen, WATCH, spent,
                               "SPINNING" if spent > SPIN else "hung"))

    # What it spent between the terminal dying and its own exit: the kernel's
    # total for the child, less what it had already spent scanning.
    spent = max(0.0, cpu - before)
    code = os.waitstatus_to_exitcode(status)
    if code == -signal.SIGHUP:
        return False, ("died of SIGHUP, so it had a controlling terminal and "
                       "the case under test never happened")
    if spent > SPIN:
        return False, ("the {} left after {:.2f}s but burned {:.2f} "
                       "CPU-seconds doing it".format(screen, waited, spent))
    if code == PANIC:
        # It leaves, and the terminal is restored either way — but ratatui's
        # `Terminal::drop` printing to a descriptor that is gone is a panic,
        # not an ending anybody chose.
        return False, ("the {} left in {:.2f}s but by panicking (exit {})"
                       .format(screen, waited, code))
    return True, ("the {} left {:.2f}s after the terminal died, exit code {}, "
                  "{:.2f} CPU-seconds".format(screen, waited, code, spent))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    binary = os.path.abspath(sys.argv[1])
    if not os.access(binary, os.X_OK):
        sys.exit("not executable: " + binary)

    print("dedup-rs: {}".format(binary))
    failures = 0
    scenarios = [("keys", screen, scenario_keys) for screen in ("menu", "browser")]
    scenarios += [("hangup", screen, scenario_hangup) for screen in ("menu", "browser")]
    for name, screen, run in scenarios:
        tree = tempfile.mkdtemp(prefix="tui-terminal-death-")
        try:
            corpus(tree)
            ok, detail = run(binary, tree, screen)
        finally:
            shutil.rmtree(tree, ignore_errors=True)
        print("  {:<16} {}  {}".format(name + "/" + screen,
                                       "PASS" if ok else "FAIL", detail))
        failures += 0 if ok else 1

    print("\n{}".format("all scenarios passed" if not failures
                        else "{} scenario(s) failed".format(failures)))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
