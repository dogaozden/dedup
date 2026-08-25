#!/usr/bin/env python3
"""Kill the live browser every way a terminal can die, with the scan running.

M5's death tests asked one question: does the process leave when its terminal
does. M8 makes that question harder in a way the old tests cannot see. There
are now worker threads behind the browser, and the browser's drop guard runs
on the browser's thread — so the terminal can be restored perfectly while a
scan thread carries on reading a disk nobody is watching, or spins on a core
forever. A user with a working terminal and a process eating a CPU has not
been served.

So every scenario here asserts BOTH halves:

  * the terminal was given back (modes, alternate screen) wherever the drop
    guard runs at all, and
  * the KERNEL agrees the process actually left — a real exit code, promptly,
    with no worker still burning a core. That second half is the one M8 adds,
    and a spinning worker cannot hide from it: `std::thread::scope` will not
    return until its threads do, so a scan that would not stop shows up here
    as a process that never leaves, and the CPU burned while not leaving says
    whether it was spinning or merely stuck.

Two of these stimuli kill without unwinding, so neither the drop guard nor
the panic hook runs and the terminal is left raw. That is NOT this
milestone's doing: `m5_2_signals.py` already records it as "browser/signal
... TERMINAL WRECKED ... M5's, reported not fixed". It is printed for each
scenario and not counted as a failure — failing on it would be this script
claiming a defect it did not find and is not fixing.

The scan is still running when each stimulus lands, and the script says so
rather than assuming it: a corpus that finished first would make every one of
these pass while testing the ordinary quit.

The orphan scenario is the one that matters most and the one an ordinary
shell cannot show: a process with NO controlling terminal gets no SIGHUP when
its master closes. It only learns by reading, and a terminal that has gone is
readable forever and empty forever.

Nothing here touches /Volumes. The corpus lives under a temporary directory.

usage: m8_live_death.py <path to dedup-rs>
"""
import fcntl
import os
import pty
import random
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

WATCH = 6.0        # seconds to give the process to notice and leave
SPIN = 0.20        # CPU-seconds inside WATCH that count as spinning
SETTLE = 1.5       # comfortably past the browser's 100 ms live poll
PANIC = 101        # a Rust panic's exit code
MIN_SIZE = 1
ALT_ON = b"\x1b[?1049h"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")


def corpus(root, smalls=20000, pairs=6, mib=4):
    """Enough to keep the scan busy while the terminal is being killed.

    The walk over `smalls` files is what buys the time: these are unique, so
    they form no group and the browser is not waiting on them for anything but
    progress. The `pairs` are real duplicates so there is something to browse.
    """
    rng = random.Random(20260824)
    page = bytes(rng.getrandbits(8) for _ in range(65536))
    for pair in range(pairs):
        body = bytes([pair]) * 65536 + page * ((mib * 1024 * 1024) // 65536 - 1)
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(body)
    many = os.path.join(root, "many")
    os.makedirs(many)
    for n in range(smalls):
        body = bytearray(page[:4096])
        body[:8] = n.to_bytes(8, "little")
        with open(os.path.join(many, "s%05d.bin" % n), "wb") as fh:
            fh.write(bytes(body))
    when = time.time() - 10
    for base, _, names in os.walk(root):
        for name in names:
            os.utime(os.path.join(base, name), (when, when))


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


def start(binary, tree, own_terminal):
    """Run the tool on a pty. Returns (pid, master).

    `own_terminal` decides the scenario: with a controlling terminal the
    kernel sends SIGHUP when the master closes, without one it sends nothing
    at all and the process has to notice by reading.
    """
    master, follower = pty.openpty()
    # A fresh pty is 0x0, and the browser refuses to act on a screen it could
    # not draw, so without a size it never reaches the event loop at all.
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))

    pid = os.fork()
    if pid == 0:
        os.setsid()
        if own_terminal:
            fcntl.ioctl(follower, termios.TIOCSCTTY, 0)
        os.dup2(follower, 0)
        os.dup2(follower, 1)
        os.dup2(follower, 2)
        os.close(master)
        if follower > 2:
            os.close(follower)
        os.execv(binary, [binary, tree, "-i", "--min-size", str(MIN_SIZE)])
        os._exit(127)

    os.close(follower)
    return pid, master


def squeeze(raw):
    return b"".join(ANSI.sub(b"", raw).split())


def reach_the_live_browser(master, pid):
    """Answer the pty until the BROWSER is up with the scan still running.

    Two screens have to be got through, and confusing them is how a death test
    accidentally kills a run that never started: the pre-flight form takes an
    alternate screen of its own, and only the second one is the browser.
    """
    seen = b""
    accepted = False
    deadline = time.time() + 30.0
    while time.time() < deadline:
        ready, _, _ = select.select([master], [], [], 0.2)
        if ready:
            try:
                chunk = os.read(master, 262144)
            except OSError:
                break
            if not chunk:
                break
            seen += chunk
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")
        if os.waitpid(pid, os.WNOHANG)[0]:
            return seen, False, "the process left before the browser opened"
        flat = squeeze(seen)
        if not accepted and b"dedup-rs\xe2\x80\x94beforethescan" in flat:
            os.write(master, b"\r")
            accepted = True
            continue
        if accepted and seen.count(ALT_ON) >= 2 and b"groups\xc2\xb7" in flat:
            return seen, recently_scanning(seen), None
    return seen, False, "the browser never opened"


STAGE_LABELS = (b"Scanning\xe2\x80\xa6", b"Readingsmallfiles\xe2\x80\xa6",
                b"Sampling\xe2\x80\xa6", b"Samplingfurther\xe2\x80\xa6",
                b"Samplingdeep\xe2\x80\xa6")


def recently_scanning(seen, window=6000):
    """Is a progress row among the last bytes the browser drew?"""
    return any(label in squeeze(seen[-window:]) for label in STAGE_LABELS)


def modes(fd):
    mode = termios.tcgetattr(fd)
    return (bool(mode[3] & termios.ICANON),
            bool(mode[3] & termios.ECHO),
            bool(mode[3] & termios.ISIG))


def verdict(pid, master, began, close_master):
    """Watch until the process leaves, then say what the kernel saw.

    The spin check is asked of a process that did NOT leave, and both readings
    come from `ps` so they are the same kind of number. Mixing a `ps` baseline
    with a `wait4` rusage total — an earlier draft of this script did — reports
    a process's whole lifetime as CPU burned after the stimulus, and every
    scenario fails for a reason that is entirely the script's.

    A process that left promptly was not spinning, and that is not a
    convenience: `std::thread::scope` holds the exit until its threads are
    done, so a worker that would not stop is precisely the case that cannot
    produce a prompt exit.
    """
    baseline = cpu_seconds(pid) or 0.0
    tail = bytearray()
    deadline = time.time() + WATCH
    while time.time() < deadline:
        if not close_master:
            # Keep draining, or the process can block writing to a full pty
            # and look like a hang that is really back-pressure.
            ready, _, _ = select.select([master], [], [], 0.05)
            if ready:
                try:
                    tail.extend(os.read(master, 262144))
                except OSError:
                    pass
        done, raw, _ = os.wait4(pid, os.WNOHANG)
        if done:
            return {
                "left": True,
                "code": os.waitstatus_to_exitcode(raw),
                "took": time.time() - began,
                "tail": bytes(tail),
            }
        time.sleep(0.02)
    spent = (cpu_seconds(pid) or 0.0) - baseline
    os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    return {"left": False, "code": None, "spent": spent,
            "took": time.time() - began, "tail": bytes(tail),
            "how": "SPINNING" if spent > SPIN else "hung"}


def scenario(binary, tree, name, own_terminal, stimulus, unwinds):
    """One stimulus, against a browser with a live scan behind it.

    `unwinds` says whether the drop guard gets to run at all. Where it does
    not — a signal with no handler — the terminal's state is reported and not
    graded; see the note at the top of this file.
    """
    problems = []
    pid, master = start(binary, tree, own_terminal)
    seen, scanning, why = reach_the_live_browser(master, pid)
    if why:
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except (ProcessLookupError, ChildProcessError):
            pass
        return [why], None
    # Comfortably past the browser's poll, so it is inside a wait rather than
    # mid-frame when the world is taken away.
    time.sleep(SETTLE)
    # Asked again HERE, not at the moment the browser was found: the sleep
    # above is long enough for a small corpus to finish in, and a scenario
    # whose scan had already ended would be testing the ordinary quit under
    # this scenario's name.
    for _ in range(20):
        ready, _, _ = select.select([master], [], [], 0.02)
        if not ready:
            break
        try:
            seen += os.read(master, 262144)
        except OSError:
            break
    # Only what the terminal has been sent RECENTLY. The whole transcript is
    # the wrong thing to ask: it still holds every frame drawn since the
    # browser opened, so once a stage label has appeared once it is in there
    # for ever and this check answers "yes" to every scenario, including the
    # ones whose scan finished a second ago. The browser repaints the progress
    # row about ten times a second while a scan is running and stops the
    # moment it is not, so a recent window is the honest question.
    still = recently_scanning(seen)
    if not still:
        problems.append("the scan had already finished when the stimulus "
                        "landed — this scenario did not test what it is named "
                        "for")

    began = time.time()
    closed = stimulus(pid, master)
    got = verdict(pid, master, began, closed)
    got["scanning"] = still

    if not got["left"]:
        problems.append("did not leave within %.0fs (%s, %.2f CPU-seconds "
                        "after the stimulus) — a worker did not stop"
                        % (WATCH, got["how"], got["spent"]))
    else:
        if got["took"] > 3.0:
            problems.append("took %.1fs to leave" % got["took"])
        if got["code"] == PANIC:
            problems.append("panicked (exit 101) instead of leaving")
        if unwinds and got["code"] is not None and got["code"] < 0:
            problems.append("died of signal %d rather than leaving on purpose"
                            % -got["code"])
    if not closed:
        after = modes(master)
        got["terminal"] = ("intact" if after[0] and after[1]
                           else "LEFT RAW (canonical=%s echo=%s)"
                           % (after[0], after[1]))
        if unwinds and not (after[0] and after[1]):
            problems.append("the terminal was left raw and the guard should "
                            "have run: %s" % got["terminal"])
        if unwinds and ALT_ON in seen and b"\x1b[?1049l" not in got["tail"]:
            problems.append("the alternate screen was never left")
    try:
        os.close(master)
    except OSError:
        pass
    return problems, got


def close_the_master(pid, master):
    os.close(master)
    return True


def send_sighup(pid, master):
    os.kill(pid, signal.SIGHUP)
    return False


def send_sigterm(pid, master):
    os.kill(pid, signal.SIGTERM)
    return False


def send_ctrl_c(pid, master):
    os.write(master, b"\x03")
    return False


# (name, its own controlling terminal, stimulus, does the drop guard run)
SCENARIOS = [
    ("the terminal closes and there is no SIGHUP to warn anyone", False,
     close_the_master, True),
    # With a controlling terminal the kernel sends SIGHUP when the master
    # closes, and SIGHUP with no handler kills without unwinding — the same
    # path as the two below, and the same recorded M5 behaviour.
    ("the terminal closes and SIGHUP arrives", True, close_the_master, False),
    ("Ctrl+C typed at the live browser", False, send_ctrl_c, True),
    # Below: killed without unwinding. The process must still LEAVE, and no
    # worker may be left burning a core; the terminal's state is M5's recorded
    # defect and is reported rather than graded.
    ("SIGHUP while the browser is up and the scan is running", False,
     send_sighup, False),
    ("SIGTERM while the browser is up and the scan is running", False,
     send_sigterm, False),
]


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary = os.path.abspath(sys.argv[1])
    # Built once and shared: nothing here deletes anything, and building
    # seventy thousand files five times over is most of the runtime.
    tree = tempfile.mkdtemp(prefix="m8-death-")
    failures = 0
    try:
        corpus(tree)
        for name, own_terminal, stimulus, unwinds in SCENARIOS:
            problems, got = scenario(binary, tree, name, own_terminal,
                                     stimulus, unwinds)
            if problems:
                failures += 1
                print("FAIL  %s" % name)
                for problem in problems:
                    print("        %s" % problem)
            else:
                print("pass  %s" % name)
            if got:
                print("        exit %-5s  left in %.2fs  scan still running: %s"
                      % (got["code"], got["took"], got["scanning"]))
                if "terminal" in got:
                    print("        terminal afterwards: %s%s"
                          % (got["terminal"],
                             "" if unwinds else "   (M5's, reported not fixed)"))
                if not got["left"]:
                    print("        %.2f CPU-seconds after the stimulus (%s)"
                          % (got["spent"], got["how"]))
    finally:
        shutil.rmtree(tree, ignore_errors=True)
    print()
    print("%d scenario(s), %d failed" % (len(SCENARIOS), failures))
    return failures


if __name__ == "__main__":
    sys.exit(main())
