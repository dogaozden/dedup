#!/usr/bin/env python3
"""A panic on the browser thread must END the process, not hang it.

REVIEW-M8 item 1, second half. `live_scan` runs the browser inside a
`std::thread::scope`, which will not return until the scan thread does, and the
scan thread's serving loop runs `while !live.cancelled()`. A panic on the
browser thread unwinds straight into that join — so if nothing cancels on the
way out, the scan spins on, the join never finishes, and the panic is never
resumed. The reviewer measured the result: still alive after 25 seconds, killed
at the limit. The panic hook restores the terminal, so the user is left with a
working shell and a process that will not die.

This is that probe, kept as a script rather than as a one-off. It injects a
`panic!` into `live_loop` immediately after `fresh.finished` — the reviewer's
own injection point — rebuilds the release binary, drives it on a real pty, and
requires the process to leave on its own. Then it restores the source, rebuilds,
and PROVES the restore by comparing the binary's hash with the one it started
from: the stale-mutant rule, checked rather than asserted.

Nothing here touches /Volumes. The corpus lives under a temporary directory.

usage: m8_panic_probe.py [--keep-going]
"""
import fcntl
import hashlib
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
RUST = os.path.dirname(os.path.dirname(HERE))
SOURCE = os.path.join(RUST, "src", "tui", "mod.rs")
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")

ANCHOR = """        let fresh = watch.fresh();
        let ending = fresh.finished;
"""
INJECT = ANCHOR + """        if ending {
            panic!("m8 panic probe: browser panic after the scan finished");
        }
"""
PANIC = 101          # a Rust panic's exit code
LIMIT = 15.0         # seconds to give the process to leave on its own
ALT_ON = b"\x1b[?1049h"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")


def squeeze(raw):
    return b"".join(ANSI.sub(b"", raw).split())


def sha(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def build():
    done = subprocess.run(["cargo", "build", "--release"], cwd=RUST,
                          capture_output=True, text=True)
    if done.returncode != 0:
        raise SystemExit("build failed:\n" + done.stderr[-2000:])


def corpus(root):
    """Small on purpose: the panic is injected on the frame where the scan has
    just finished, so the scan has to finish quickly for the probe to fire."""
    body = b"duplicate payload " * 4096
    for name in ("a.bin", "copy-of-a.bin"):
        with open(os.path.join(root, name), "wb") as fh:
            fh.write(body)
    with open(os.path.join(root, "other.bin"), "wb") as fh:
        fh.write(b"something else " * 4096)


def drive(tree, home):
    """Run the browser on a pty, accept the menu, and watch for the exit."""
    master, follower = pty.openpty()
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    process = subprocess.Popen(
        [BINARY, tree, "-i", "--min-size", "1"],
        stdin=follower, stdout=follower, stderr=follower,
        env=dict(os.environ, HOME=home, TERM="xterm-256color",
                 DEDUP_CACHE_DIR=os.path.join(home, "cache")),
        start_new_session=True)
    os.close(follower)

    seen = bytearray()
    os.set_blocking(master, False)
    began = time.time()
    accepted = False
    code = None
    while time.time() - began < LIMIT:
        ready, _, _ = select.select([master], [], [], 0.05)
        if ready:
            try:
                chunk = os.read(master, 262144)
                seen.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            except OSError:
                pass
        if process.poll() is not None:
            code = process.returncode
            break
        if not accepted and b"dedup-rs\xe2\x80\x94beforethescan" in squeeze(seen):
            os.write(master, b"\r")
            accepted = True
        time.sleep(0.02)
    took = time.time() - began
    alive = process.poll() is None
    if alive:
        process.kill()
        process.wait()
    try:
        os.close(master)
    except OSError:
        pass
    return code, took, alive, bytes(seen), accepted


def main():
    before = sha(BINARY) if os.path.exists(BINARY) else None
    original = open(SOURCE).read()
    if ANCHOR not in original:
        raise SystemExit("the injection point moved; update ANCHOR in this script")

    problems = []
    print("$ inject a panic into live_loop, immediately after fresh.finished")
    try:
        open(SOURCE, "w").write(original.replace(ANCHOR, INJECT, 1))
        build()
        tree = tempfile.mkdtemp(prefix="m8-panic-")
        home = tempfile.mkdtemp(prefix="m8-panic-home-")
        os.makedirs(os.path.join(home, ".Trash"), exist_ok=True)
        corpus(tree)
        code, took, alive, seen, accepted = drive(tree, home)
        shutil.rmtree(tree, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)

        print("    browser reached:            %s" % accepted)
        print("    alternate screens entered:  %d" % seen.count(ALT_ON))
        print("    panic message on screen:    %s"
              % (b"m8 panic probe" in seen))
        print("    exited on its own:          %s" % (not alive))
        print("    exit code:                  %s" % code)
        print("    seconds to leave:           %.2f" % took)

        if not accepted:
            problems.append("the pre-flight form never appeared, so the "
                            "browser was never reached")
        if b"m8 panic probe" not in seen:
            problems.append("the injected panic never fired — the probe did "
                            "not test what it is named for")
        if alive:
            problems.append("THE PROCESS NEVER LEFT: still alive after %.0fs, "
                            "killed at the limit" % LIMIT)
        elif code != PANIC:
            problems.append("exit %s, wanted %d (a panic that left on purpose)"
                            % (code, PANIC))
    finally:
        open(SOURCE, "w").write(original)
        build()

    print()
    print("$ source restored and rebuilt   (the stale-mutant rule)")
    after = sha(BINARY)
    print("    binary before the probe:  %s" % (before or "(none)"))
    print("    binary after the restore: %s" % after)
    if before is not None and before != after:
        problems.append("the restored binary is not the one we started with")
    else:
        print("    identical: the mutant is gone")

    print()
    for problem in problems:
        print("FAIL  %s" % problem)
    if not problems:
        print("pass  a browser-thread panic ends the process instead of "
              "hanging it")
    return len(problems)


if __name__ == "__main__":
    sys.exit(main())
