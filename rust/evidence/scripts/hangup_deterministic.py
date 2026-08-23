#!/usr/bin/env python3
"""Show a full-screen view spinning when its terminal dies, on demand.

The driver from REVIEW-M5_1.md, kept because it is what reproduced binding item
1 before the fix and is the shortest way to reproduce it again on any build.
The delay before the terminal is taken away is a PARAMETER, so the process is
provably past its 250 ms poll and inside `event::read()` when the descriptor
goes — which is what makes the answer the same every run.

`tui-terminal-death.py` is the regression test and covers more; this is the
minimal reproducer.

usage: hangup_deterministic.py <binary> <delay-seconds> [repeats]"""
import fcntl, os, pty, re, select, shutil, signal, struct, subprocess, sys
import tempfile, termios, time

BIN, DELAY = sys.argv[1], float(sys.argv[2])
REPEATS = int(sys.argv[3]) if len(sys.argv) > 3 else 1
WATCH = 5.0

def corpus(root):
    for name in ("a.bin", "copy-of-a.bin"):
        open(os.path.join(root, name), "wb").write(b"duplicate payload " * 4096)
    open(os.path.join(root, "other.bin"), "wb").write(b"something else " * 4096)

def cpu_seconds(pid):
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)],
                         capture_output=True, text=True).stdout.strip()
    if not out:
        return None
    total = 0.0
    for part in out.split(":"):
        total = total * 60 + float(part)
    return total

def once(tree):
    master, follower = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    pid = os.fork()
    if pid == 0:
        os.setsid()
        os.dup2(follower, 0); os.dup2(follower, 1); os.dup2(follower, 2)
        os.close(master)
        if follower > 2:
            os.close(follower)
        os.execv(BIN, [BIN, tree, "-i", "--min-size", "4096"])
        os._exit(127)
    os.close(follower)
    painted, deadline = b"", time.time() + 10
    while time.time() < deadline:
        r, _, _ = select.select([master], [], [], 0.25)
        if not r:
            if painted:
                break
            continue
        try:
            chunk = os.read(master, 8192)
        except OSError:
            break
        if not chunk:
            break
        painted += chunk
        if b"\x1b[6n" in chunk:
            os.write(master, b"\x1b[1;1R")
    visible = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "",
                     painted.decode("utf8", "replace"))
    screen = "menu" if "before the scan" in visible else "browser"
    time.sleep(DELAY)                      # provably past the 250 ms poll
    before = cpu_seconds(pid) or 0.0
    os.close(master)                       # the terminal dies here
    end = time.time() + WATCH
    while time.time() < end:
        done, st, usage = os.wait4(pid, os.WNOHANG)
        if done:
            return "LEFT(%s, exit=%s, %.2f CPU-s)" % (
                screen, os.waitstatus_to_exitcode(st), usage.ru_utime + usage.ru_stime)
        time.sleep(0.05)
    spent = (cpu_seconds(pid) or 0.0) - before
    os.kill(pid, signal.SIGKILL); os.waitpid(pid, 0)
    return "SPINNING(%s, %.2f CPU-s in %.0fs)" % (screen, spent, WATCH)

tree = tempfile.mkdtemp(prefix="hangup-")
corpus(tree)
spins = 0
for i in range(REPEATS):
    verdict = once(tree)
    spins += verdict.startswith("SPINNING")
    print("  run %d: %s" % (i + 1, verdict))
shutil.rmtree(tree, ignore_errors=True)
print("SPUN %d/%d" % (spins, REPEATS))
sys.exit(1 if spins else 0)
