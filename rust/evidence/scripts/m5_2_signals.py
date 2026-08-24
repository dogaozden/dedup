#!/usr/bin/env python3
"""What signals do to a terminal this program is holding.

M5.2 is the first thing outside the TUI to write to the terminal, so the M5.4
law — a user's wrecked terminal is a failed milestone — has to be re-asked of
the progress line, and asking it turned up an older answer about the browser.

Four questions, each driven on a real pty, each sampling `(canonical, echo,
ISIG)` before, during and after:

  scan/keyboard    ctrl-c while the pulse is painting and nothing else holds
                   the terminal. Canonical mode is on, so this really is a
                   SIGINT. Nothing to restore — the pulse writes text and never
                   a mode — so the terminal must come back untouched, with the
                   last line painted left behind.
  menu/keyboard    ctrl-c at the pre-flight menu.
  browser/keyboard ctrl-c at the browser.
  browser/signal   SIGINT sent with kill(2) while the browser holds raw mode
                   and the alternate screen — which no keystroke can do, since
                   raw mode clears ISIG.

Usage: m5_2_signals.py <path-to-dedup-rs> <scratch-dir>
"""

import fcntl
import os
import pty
import re
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time

MIB = 1024 * 1024
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]")


def modes(fd):
    attrs = termios.tcgetattr(fd)
    return (bool(attrs[3] & termios.ICANON), bool(attrs[3] & termios.ECHO),
            bool(attrs[3] & termios.ISIG))


def own_the_terminal():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def build(root, pairs=3, mib=24, smalls=6000):
    """Enough to read that a scan outlives a repaint by a wide margin, and
    duplicate pairs so the browser has something to open on.

    Deliberately file-heavy: the interrupt has to land while the line is up,
    and a scan that finishes 200 ms after its first paint leaves no window. At
    one reader this is a second or two of scanning — ten beats and more.
    """
    payload = bytes(range(256)) * (mib * MIB // 256)
    for pair in range(pairs):
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(bytes([pair]) * 65536)
                fh.write(payload[65536:])
    os.makedirs(os.path.join(root, "many"))
    for n in range(smalls):
        with open(os.path.join(root, "many", "s%04d.bin" % n), "wb") as fh:
            fh.write(payload[: 8192 if n % 3 else 4096])


def probe(binary, root, home, where, how):
    """Run to `where` (scan, menu or browser) and interrupt it by `how`."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    environment = dict(os.environ, HOME=home, TERM="xterm-256color",
                       DEDUP_CACHE_DIR=os.path.join(home, "cache"))
    flags = ["--min-size", "1", "--no-cache", "-j", "1",
             "--report-json", os.path.join(home, "r.json")]
    flags += ["--list"] if where == "scan" else ["-i"]
    before = modes(master)
    process = subprocess.Popen([os.path.abspath(binary), os.path.abspath(root), *flags],
                               stdin=slave, stdout=slave, stderr=slave,
                               env=environment, preexec_fn=own_the_terminal)
    os.close(slave)
    seen = bytearray()
    os.set_blocking(master, False)

    def drain():
        try:
            chunk = os.read(master, 262144)
        except (BlockingIOError, OSError):
            return
        seen.extend(chunk)
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")

    def wait_for(wanted, seconds=40):
        end = time.time() + seconds
        while time.time() < end and process.poll() is None:
            drain()
            if wanted(seen.decode("utf8", "replace")):
                return True
            time.sleep(0.02)
        return False

    reached = True
    if where == "scan":
        # The line has to be up, or the interrupt lands on nothing.
        reached = wait_for(lambda t: "\x1b[2K" in t and "…" in t)
    else:
        wait_for(lambda t: "before the scan" in ANSI.sub(" ", t))
        time.sleep(0.4)
        if where == "browser":
            os.write(master, b"\r")
            reached = wait_for(lambda t: t.count("\x1b[?1049h") >= 2)
            time.sleep(0.6)
    drain()
    during = modes(master)

    if where == "scan" and not reached:
        # A build with no progress line has nothing to interrupt through, which
        # is what running this against main@fbc196d looks like. Not a failure —
        # this row is M5.2's alone.
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        print("== %s / %s ==" % (where, how))
        print("   no progress line ever appeared: nothing to interrupt through")
        print()
        return {"intact": None, "ignored": False, "exit": None, "absent": True}
    if process.poll() is not None:
        os.close(master)
        raise SystemExit(
            "%s/%s: the run left (exit %s) before it could be interrupted — "
            "the corpus is too small for the window this needs"
            % (where, how, process.returncode))
    if how == "keyboard":
        os.write(master, b"\x03")
    else:
        os.kill(process.pid, signal.SIGINT)

    end = time.time() + 6
    while time.time() < end and process.poll() is None:
        drain()
        time.sleep(0.02)
    left = process.poll()
    ignored = left is None
    if ignored:
        # It did not react at all. Whatever the modes say after the SIGKILL
        # below is about the SIGKILL, so the row reports what really happened:
        # nothing. Bounded, like everything else here — a probe that can hang
        # is a probe that will, on a machine with other work on it.
        process.kill()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
    drain()
    after = modes(master)
    os.close(master)
    text = seen.decode("utf8", "replace")
    print("== %s / %s ==" % (where, how))
    print("   reached %s before interrupting: %s" % (where, reached))
    print("   (canonical, echo, ISIG) before : %s" % (before,))
    print("   (canonical, echo, ISIG) during : %s" % (during,))
    print("   (canonical, echo, ISIG) after  : %s" % (after,))
    print("   exit: %s" % ("IGNORED — still running after 5s, then killed"
                           if ignored else left))
    print("   alternate screen entered %d, left %d"
          % (text.count("\x1b[?1049h"), text.count("\x1b[?1049l")))
    if ignored:
        print("   terminal came back as it was: %s (after our SIGKILL, not "
              "after the interrupt — the interrupt did nothing)" % (before == after))
    else:
        print("   terminal came back as it was: %s" % (before == after))
    if where == "scan":
        print("   last bytes: %r" % text[-90:])
    print()
    return {"intact": before == after, "ignored": ignored, "exit": left}


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    binary, base = sys.argv[1], os.path.abspath(sys.argv[2])
    base = os.path.join(base, "m5_2-signals")
    shutil.rmtree(base, ignore_errors=True)
    root, home = os.path.join(base, "tree"), os.path.join(base, "home")
    os.makedirs(root)
    os.makedirs(home)
    build(root)

    got = {}
    for where, how in (("scan", "keyboard"), ("menu", "keyboard"),
                       ("browser", "keyboard"), ("browser", "signal")):
        got[(where, how)] = probe(binary, root, home, where, how)

    shutil.rmtree(base, ignore_errors=True)

    def say(where, how, expected):
        row = got[(where, how)]
        state = ("no progress line" if row.get("absent")
                 else "ignored" if row["ignored"]
                 else "terminal intact" if row["intact"]
                 else "TERMINAL WRECKED")
        print("  %-16s exit %-6s %-16s %s" % (
            where + "/" + how,
            "killed" if row["ignored"] else row["exit"],
            state, expected))
        return state

    print("ctrl-c at the keyboard, by screen:")
    scan = say("scan", "keyboard",
               "canonical mode, so it is a real SIGINT; nothing to restore")
    menu = say("menu", "keyboard",
               "raw mode clears ISIG and the menu's key map drops the "
               "modifiers, so nothing happens — M5.1's, reported not fixed")
    browser = say("browser", "keyboard",
                  "raw mode clears ISIG; read as a key and mapped to Quit")
    print()
    print("a signal from anywhere but the keyboard, at the browser:")
    signalled = say("browser", "signal",
                    "no unwinding, so neither the drop guard nor the panic "
                    "hook runs — M5's, reported not fixed")
    print()
    print("The progress line never wrecks a terminal: it writes text and never")
    print("a mode, and an interrupt through it leaves the modes untouched.")
    wrong = [name for name, state, want in
             (("scan/keyboard", scan, "terminal intact"),
              ("menu/keyboard", menu, "ignored"),
              ("browser/keyboard", browser, "terminal intact"),
              ("browser/signal", signalled, "TERMINAL WRECKED"))
             # "no progress line" is what a build without a pulse says; the
             # other three rows still have to hold on such a build.
             if state != want and state != "no progress line"]
    if wrong:
        print("CHANGED since this was written: %s" % ", ".join(wrong))
        return 1
    print("All four behave as recorded.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
