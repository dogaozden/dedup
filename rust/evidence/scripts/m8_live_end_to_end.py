#!/usr/bin/env python3
"""Drive the M8 live browser through a real terminal.

Four things are being checked, and none of them can be checked from inside the
program:

  * the browser is on screen BEFORE the scan has found anything — the whole
    premise of the milestone;
  * groups arrive while it is open, and are labelled `candidate` because
    nothing has compared their bytes yet;
  * marking and applying DURING a running scan removes exactly the marked
    copies and nothing else, with the byte proof paid at that moment; and
  * quitting mid-scan gives the terminal back.

Nothing is mocked: the release binary, a pty, the bytes a keyboard sends, and
a tree generated fresh under the scratch directory given. What survived is
read off the filesystem afterwards, not out of a report the program wrote
about itself — the live path takes no `--report-json`, and a deletion tool
should be graded on what is left on disk.

Wants a quiet machine. Every case waits on the screen rather than on the
clock; the outer patience is 120 seconds of wall time.

Usage: m8_live_end_to_end.py <path-to-dedup-rs> <scratch-dir>
"""

import fcntl
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
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")
ALT_ON = "\x1b[?1049h"
ALT_OFF = "\x1b[?1049l"
MIB = 1024 * 1024


def squeeze(text):
    """What the screen said, with escape sequences and spacing gone."""
    return "".join(ANSI.sub("", text).split())


def build(root, pairs=4, mib=6, smalls=9000):
    """A tree whose scan outlives several repaints, and whose best group is a
    candidate.

    Three deliberate shapes:

      * `pairs` identical big pairs, each with a HEAD OF ITS OWN. The distinct
        head matters: it is what lets the end probe put each pair in a bucket
        of its own. An earlier draft gave every pair the same head, so all of
        them arrived as one candidate group of eight — correct behaviour, and
        useless for testing a deletion, because that group fails its proof.
        They still reach the browser as candidates: nothing has compared their
        bytes, only hashed the ends.
      * one `liar` group whose members agree through the whole deep head and
        differ far into the file. The narrowing stages cannot tell them apart,
        so it arrives as a candidate and FAILS its proof — see
        `case_a_failed_proof_deletes_nothing`.
      * `smalls` files with content unique to each. They form no group at all;
        they exist to give the walk enough to do that the browser is
        demonstrably open while the scan is still running.

    Deterministic: the same tree every run, on any machine.
    """
    rng = random.Random(20260824)
    page = bytes(rng.getrandbits(8) for _ in range(65536))
    blocks = mib * MIB // 65536
    for pair in range(pairs):
        # A head unique to this pair, then a body unique to this pair.
        head = bytes([pair + 1]) * 65536
        payload = head + page * (blocks // 2) + \
            bytes([pair + 200]) * (65536 * (blocks - blocks // 2 - 1))
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(payload)

    # The liar: same size, same ends, same deep head, different middles. Every
    # cheap stage says these three are the same file and only the byte compare
    # knows better.
    shared_head = bytes([0xAB]) * (128 * 1024)
    shared_tail = bytes([0xCD]) * 65536
    filler = mib * MIB - len(shared_head) - len(shared_tail)
    os.makedirs(os.path.join(root, "liar"))
    for n in range(3):
        with open(os.path.join(root, "liar", "l%d.bin" % n), "wb") as fh:
            fh.write(shared_head + bytes([n]) * filler + shared_tail)

    os.makedirs(os.path.join(root, "many"))
    for n in range(smalls):
        # Unique per file: a distinct 8-byte stamp in an otherwise shared
        # page. No two of these are ever a group.
        body = bytearray(page[:4096])
        body[:8] = n.to_bytes(8, "little")
        with open(os.path.join(root, "many", "s%05d.bin" % n), "wb") as fh:
            fh.write(bytes(body))
    age(root)


def age(root):
    """Ten seconds into the past, so the cache may be trusted at all.

    A row is believed only while the file's mtime tick is strictly older than
    the second the writing scan started in (cache.rs, git's racily-clean
    rule). A corpus built and scanned inside one second is racily clean and
    every row is dropped — which would make a cache test pass for the wrong
    reason, or fail for one.
    """
    when = time.time() - 10
    for base, _, names in os.walk(root):
        for name in names:
            os.utime(os.path.join(base, name), (when, when))


def fresh(base, name, **kw):
    root = os.path.join(base, name)
    home = os.path.join(base, name + "-home")
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(home, ignore_errors=True)
    os.makedirs(root)
    os.makedirs(os.path.join(home, ".Trash"))
    build(root, **kw)
    return root, home


def environment(home):
    return dict(os.environ, HOME=home, TERM="xterm-256color",
                DEDUP_CACHE_DIR=os.path.join(home, "cache"))


def drive(binary, root, home, keys, extra=(), size=(40, 120), patience=120):
    """Run under a pty, typing `keys` as the run reaches each screen.

    `keys` is a list of (what to wait for, what to type). Waiting on the
    screen rather than on the clock is what keeps this honest on a machine
    slower than the one it was written on — and it is not optional for a key
    the browser is meant to read: the terminal is in canonical mode until the
    browser takes it, so a key sent before then is echoed and never delivered.
    """
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    process = subprocess.Popen(
        [os.path.abspath(binary), os.path.abspath(root), *extra],
        stdin=slave, stdout=slave, stderr=slave,
        env=environment(home), start_new_session=True)
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

    pending = list(keys)
    deadline = time.time() + patience
    ready_at = 0.0
    killed = False
    seen = []
    while time.time() < deadline:
        drain()
        if process.poll() is not None:
            break
        if pending and time.time() >= ready_at:
            wanted, key = pending[0]
            if wanted(output.decode("utf8", "replace")):
                pending.pop(0)
                # What the screen said at the moment the key went in — the
                # only way to assert on a frame that a later repaint erases.
                seen.append(output.decode("utf8", "replace"))
                try:
                    os.write(master, key)
                except OSError:
                    break
                ready_at = time.time() + 0.3
        time.sleep(0.02)
    if process.poll() is None:
        process.kill()
        killed = True
    code = process.wait()
    drain()
    os.close(master)
    text = output.decode("utf-8", "replace")
    if killed:
        text += "\n[the run did not exit; killed]"
    if pending:
        text += "\n[%d key(s) never sent: the screen never arrived]" % len(pending)
    return code, text, seen


def at_menu(text):
    """The M5.1 pre-flight form, which every interactive launch sees first."""
    return "dedup-rs—beforethescan" in squeeze(text)


def alt_screens(text):
    """How many alternate screens have been entered.

    The menu takes one and the browser takes another, so this is how a case
    tells the two apart. Getting it wrong is not academic: an earlier draft of
    this script sent `q` at the first alternate screen it saw, quit the MENU,
    and reported that quitting mid-scan restored the terminal — a pass for a
    scan that had never started.
    """
    return text.count(ALT_ON)


def at_browser(text):
    """The browser is up: its own screen, and its summary line."""
    return alt_screens(text) >= 2 and "groups·" in squeeze(text)


def scanning(text):
    """A frame drawn while the scan is still running.

    The progress row only exists while there is a scan behind the browser, so
    its presence IS the proof that the browser opened before the scan ended.
    """
    return any(
        label in squeeze(text)
        for label in ("Scanning…", "Readingsmallfiles…", "Sampling…",
                      "Samplingfurther…", "Samplingdeep…")
    )


def on_disk(root):
    """Every file still in the tree, by path relative to it."""
    out = set()
    for base, _, names in os.walk(root):
        for name in names:
            out.add(os.path.relpath(os.path.join(base, name), root))
    return out


def modes(fd):
    """(canonical, echo) as the terminal currently has them."""
    mode = termios.tcgetattr(fd)
    return bool(mode[3] & termios.ICANON), bool(mode[3] & termios.ECHO)


# ------------------------------------------------------------------ the cases

def case_opens_before_the_scan_ends(binary, scratch):
    """The browser is on screen while the scan is still running."""
    root, home = fresh(scratch, "opens")
    # Accept the pre-flight form, then wait for a BROWSER frame that still has
    # a progress row on it — a browser drawn after the scan had finished would
    # have no progress to draw.
    code, text, seen = drive(
        binary, root, home,
        [
            (at_menu, ENTER),
            (lambda t: at_browser(t) and scanning(t), b"q"),
        ],
        extra=["--min-size", "1"])
    problems = []
    if alt_screens(text) < 2:
        problems.append("the browser never opened its own screen (saw %d)"
                        % alt_screens(text))
    if len(seen) < 2:
        problems.append("the browser never drew while the scan was running")
    elif not scanning(seen[1]):
        problems.append("the frame had no progress row on it")
    # ...and the stderr pulse must not be fighting the alternate screen. In
    # live mode there is no line at all: the browser is the progress display.
    # Only the stderr pulse writes "carriage return, clear the line, two
    # spaces, a stage label"; ratatui positions its cursor and writes cells.
    tail = text.split(ALT_ON)[-1]
    if re.search(r"\r\x1b\[2K {2}\w[\w ]*…", tail):
        problems.append("the stderr pulse painted over the browser's screen")
    if code != 0:
        problems.append("exit %d, wanted 0" % code)
    return problems


def case_groups_stream_in_as_candidates(binary, scratch):
    """Groups arrive while the browser is open, labelled honestly."""
    root, home = fresh(scratch, "streams")
    code, text, seen = drive(
        binary, root, home,
        [
            (at_menu, ENTER),
            (lambda t: "candidate" in squeeze(t), b"q"),
        ],
        extra=["--min-size", "1"])
    problems = []
    if len(seen) < 2:
        problems.append("no group was ever labelled `candidate`")
        return problems
    frame = squeeze(seen[1])
    if "candidate" not in frame:
        problems.append("the candidate label never appeared")
    # A candidate arrived, so the list is not empty: the two states are
    # mutually exclusive and seeing both would mean the screen was lying.
    if "Nodetectedduplicates" in frame:
        problems.append("the browser drew a group and an empty list at once")
    if code != 0:
        problems.append("exit %d, wanted 0" % code)
    return problems


def case_apply_during_the_scan(binary, scratch):
    """Mark and delete while the scan is still running.

    The assertion is on the filesystem: exactly the marked copy is gone, its
    keeper is not, and nothing else in the tree was touched.
    """
    root, home = fresh(scratch, "apply")
    before = on_disk(root)
    code, text, seen = drive(
        binary, root, home,
        [
            (at_menu, ENTER),
            # Wait for a group, keep the copy under the cursor, apply, confirm.
            (lambda t: "candidate" in squeeze(t) or "Group1of" in squeeze(t), b"a"),
            (lambda t: "marked" in squeeze(t), ENTER),
            (lambda t: "Confirm" in squeeze(t), b"y"),
            (lambda t: "movedtoTrash" in squeeze(t) or "moved" in squeeze(t)
             or "groups·" in squeeze(t), b"q"),
        ],
        extra=["--min-size", "1"])
    after = on_disk(root)
    gone = before - after
    problems = []
    if len(seen) > 1 and not scanning(seen[1]):
        problems.append("the scan had already finished when the mark went in "
                        "— this case did not test what it is named for")
    if code != 0:
        problems.append("exit %d, wanted 0" % code)
    if not gone:
        problems.append("nothing was removed at all")
    trash = set(os.listdir(os.path.join(home, ".Trash")))
    if len(trash) != len(gone):
        problems.append("Trash holds %d name(s), the tree lost %d"
                        % (len(trash), len(gone)))
    # Every removal must have left a survivor with the same content behind it.
    for name in gone:
        stem = os.path.basename(name)
        if stem.startswith("big-"):
            twin = name.replace("a.bin", "b.bin") if "a.bin" in name \
                else name.replace("b.bin", "a.bin")
            if twin not in after:
                problems.append("both copies of %s went" % stem)
    if after - before:
        problems.append("the run created files in the tree: %r" % (after - before))
    return problems


def case_quit_mid_scan_restores_the_terminal(binary, scratch):
    """Quitting while the scan is live gives the terminal back.

    The stimulus has to land on the BROWSER, not on the pre-flight form: the
    form has an alternate screen of its own, and a `q` sent at it quits a run
    that has not started scanning. So the form is accepted first, and the quit
    waits for a second alternate screen with a progress row still on it.
    """
    # A longer walk, so the browser is reliably still scanning when the
    # quit lands. With the ordinary corpus the scan was sometimes over
    # before the first frame could be read.
    root, home = fresh(scratch, "quit", smalls=30000)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    process = subprocess.Popen(
        [os.path.abspath(binary), os.path.abspath(root), "--min-size", "1"],
        stdin=slave, stdout=slave, stderr=slave,
        env=environment(home), start_new_session=True)
    os.close(slave)
    output = bytearray()
    os.set_blocking(master, False)
    problems = []
    deadline = time.time() + 90
    accepted = False
    quit_at = None
    during = None
    mid_scan = False
    browser_since = float("inf")
    while time.time() < deadline:
        try:
            chunk = os.read(master, 262144)
            output.extend(chunk)
            if b"\x1b[6n" in chunk:
                os.write(master, b"\x1b[1;1R")
        except (BlockingIOError, OSError):
            pass
        if process.poll() is not None:
            break
        text = output.decode("utf8", "replace")
        if accepted and browser_since == float("inf") and at_browser(text):
            browser_since = time.time()
        if not accepted and at_menu(text):
            os.write(master, ENTER)
            accepted = True
            time.sleep(0.3)
        elif accepted and quit_at is None and at_browser(text) and (
                scanning(text) or time.time() > browser_since + 5):
            # Raw mode is on, the browser's alternate screen is up, and the
            # scan is still running: this is the moment a quit has the most to
            # put back, and the hardest one for it to put back correctly.
            during = modes(master)
            mid_scan = scanning(text)
            os.write(master, b"q")
            quit_at = time.time()
        time.sleep(0.02)
    burned = None
    if process.poll() is None:
        process.kill()
        problems.append("the browser did not exit on q")
    # `wait4` rather than `wait`: terminal modes restored is NOT enough on its
    # own here. The main thread's drop guard can put the screen back while a
    # worker spins forever on a core, and the user would be left with a
    # working terminal and a process eating a CPU. The only way to see that is
    # to ask the kernel what the process actually burned.
    try:
        _, status, usage = os.wait4(process.pid, 0)
        code = os.waitstatus_to_exitcode(status)
        burned = usage.ru_utime + usage.ru_stime
    except (ChildProcessError, OSError):
        code = process.returncode
    try:
        os.read(master, 262144)
    except (BlockingIOError, OSError):
        pass
    text = output.decode("utf8", "replace")
    os.close(master)

    if not accepted:
        problems.append("the pre-flight form never appeared")
    if quit_at is None:
        problems.append("the browser never drew a frame at all")
    elif not mid_scan:
        problems.append("the scan had already finished when the quit went in "
                        "— this case did not test what it is named for")
    if during and during[0]:
        problems.append("the browser was not in raw mode while it was up")
    if text.count(ALT_OFF) < 2:
        problems.append("an alternate screen was never left (saw %d of 2)"
                        % text.count(ALT_OFF))
    if code != 0:
        problems.append("exit %s, wanted 0" % code)
    if quit_at is not None and burned is not None:
        took = time.time() - quit_at
        if took > 10:
            problems.append("quitting took %.1fs — the scan did not stop" % took)
    return problems


def case_a_failed_proof_deletes_nothing(binary, scratch):
    """A candidate that the bytes disown loses nothing.

    The `liar` group agrees through its ends and its whole deep head and
    differs in the middle, so every narrowing stage calls it a group and only
    the byte comparison knows better. On the live path it therefore reaches
    the browser as a candidate, and the proof at apply time is the ONLY thing
    standing between it and three files being deleted down to one.

    This is the deletion-safety edge of the whole milestone: if this case ever
    removes a file, the milestone is wrong no matter what else passes.
    """
    # The liar group is the ONLY group here, so the browser opens on it and
    # nothing has to be navigated to. That is not tidiness: ratatui draws
    # differentially, so after a cursor move the pty carries only the cells
    # that changed, and a case that walked to a group could not then read its
    # members back out of the transcript.
    root, home = fresh(scratch, "liar", pairs=0, smalls=2000)
    before = on_disk(root)
    liars = {name for name in before if name.startswith("liar" + os.sep)}
    code, text, seen = drive(
        binary, root, home,
        [
            (at_menu, ENTER),
            # The liar group has three copies where every other group has
            # two, so it reclaims the most and the browser opens on it. The
            # wait is on its members being ON SCREEN rather than on a
            # position, so a change in the ordering fails this loudly instead
            # of quietly pointing it at a group that would have passed.
            (lambda t: at_browser(t) and "liar/l" in ANSI.sub("", t), b"a"),
            (lambda t: "marked" in squeeze(t), ENTER),
            (lambda t: "Confirm" in squeeze(t), b"y"),
            (lambda t: "refused" in squeeze(t) or "movedtoTrash" in squeeze(t), b"q"),
        ],
        extra=["--min-size", "1"])
    after = on_disk(root)
    problems = []
    still = {name for name in after if name.startswith("liar" + os.sep)}
    if still != liars:
        problems.append("THE PROOF DID NOT HOLD: liar files went: %r"
                        % sorted(liars - still))
    trash = os.listdir(os.path.join(home, ".Trash"))
    if any(name.startswith("l") and name.endswith(".bin") for name in trash):
        problems.append("a liar file reached the Trash: %r" % trash)
    if "refused" not in squeeze(text):
        problems.append("the screen never said the group was refused")
    if code != 0:
        problems.append("exit %d, wanted 0" % code)
    return problems


CASES = [
    ("the browser opens before the scan ends", case_opens_before_the_scan_ends),
    ("groups stream in and say they are candidates", case_groups_stream_in_as_candidates),
    ("mark-and-apply during a running scan", case_apply_during_the_scan),
    ("quitting mid-scan restores the terminal", case_quit_mid_scan_restores_the_terminal),
    ("a failed proof deletes nothing", case_a_failed_proof_deletes_nothing),
]


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    binary, scratch = sys.argv[1], sys.argv[2]
    os.makedirs(scratch, exist_ok=True)
    failures = 0
    for name, case in CASES:
        began = time.time()
        problems = case(binary, scratch)
        took = time.time() - began
        if problems:
            failures += 1
            print("FAIL  %s  (%.1fs)" % (name, took))
            for problem in problems:
                print("        %s" % problem)
        else:
            print("pass  %s  (%.1fs)" % (name, took))
    print()
    print("%d case(s), %d failed" % (len(CASES), failures))
    return failures


if __name__ == "__main__":
    sys.exit(main())
