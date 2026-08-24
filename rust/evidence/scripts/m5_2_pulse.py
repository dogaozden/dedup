#!/usr/bin/env python3
"""Drive the M5.2 surfaces through a real terminal: the pulse, and sizes.

Two laws are being checked, and neither of them can be checked from inside the
program:

  * the progress line exists only when stderr is a terminal and nobody asked
    for quiet, and it is gone — cleared, not scrolled — before anything else
    reaches the screen; and
  * a size typed the way a person types it reaches the scan as that many
    bytes, all the way from a key press to the groups in the report.

Nothing is mocked: the release binary, a pty, the bytes a keyboard sends, and
trees generated fresh under the scratch directory given.

Usage: m5_2_pulse.py <path-to-dedup-rs> <scratch-dir> [path-to-main-dedup-rs]
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
RUB = b"\x7f"
ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")

ALT_ON = "\x1b[?1049h"
ALT_OFF = "\x1b[?1049l"
CLEAR = "\r\x1b[2K"

MIB = 1024 * 1024


def squeeze(text):
    """What the screen said, with escape sequences and spacing gone."""
    return "".join(ANSI.sub("", text).split())


def build(root, pairs=6, mib=24, smalls=4000):
    """A tree big enough that a scan outlives one repaint.

    Big identical files so the comparison has to read every byte of them, and
    a crowd of small ones so the walk has something to count. Deterministic:
    the same tree every run, on any machine.
    """
    rng = random.Random(20260823)
    page = bytes(rng.getrandbits(8) for _ in range(65536))
    blocks = mib * MIB // 65536
    for pair in range(pairs):
        # Identical first halves, a second half of its own per pair. Every
        # pair is its own group, and — because the sample stage looks through
        # the whole file rather than at its head — it is still its own group
        # by the time the comparison sees it, which is what gives that stage
        # more than one thing to count.
        payload = page * (blocks // 2) + bytes([pair]) * (65536 * (blocks - blocks // 2))
        for copy in ("a", "b"):
            with open(os.path.join(root, "big-%d%s.bin" % (pair, copy)), "wb") as fh:
                fh.write(payload)
    os.makedirs(os.path.join(root, "many"))
    for n in range(smalls):
        with open(os.path.join(root, "many", "s%05d.bin" % n), "wb") as fh:
            fh.write(page[: 8192 if n % 3 else 4096])
    # One 2 MiB pair and one 4 KiB pair with names of their own, so a min-size
    # can be seen taking effect at three different magnitudes.
    for name, size in (("mid-a.bin", 2 * MIB), ("mid-b.bin", 2 * MIB),
                       ("tiny-a.bin", 4096), ("tiny-b.bin", 4096)):
        with open(os.path.join(root, name), "wb") as fh:
            fh.write(page[:65536] * (size // 65536) if size >= 65536
                     else page[:size])


def fresh(base, name, **kw):
    root = os.path.join(base, name)
    home = os.path.join(base, name + "-home")
    os.makedirs(root)
    os.makedirs(home)
    build(root, **kw)
    return root, home


def environment(home):
    return dict(os.environ, HOME=home, TERM="xterm-256color",
                DEDUP_CACHE_DIR=os.path.join(home, "cache"))


def drive(binary, root, home, keys, extra=(), size=(40, 120), patience=120,
          stdout_to=None):
    """Run under a pty, typing `keys` as the run reaches each screen.

    `keys` is a list of (what to wait for, what to type). Waiting on the screen
    rather than on the clock is what keeps this honest on a machine slower or
    faster than the one it was written on — and it is not optional for a key
    the browser is meant to read: the terminal is in canonical mode until the
    browser takes it, so a `q` sent before then is echoed at the scan and never
    delivered.

    The pty is drained the whole time, not only around keystrokes, so a run
    that types nothing is still watched.
    """
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    piped = open(stdout_to, "wb") if stdout_to else None
    process = subprocess.Popen(
        [os.path.abspath(binary), os.path.abspath(root), *extra],
        stdin=slave, stdout=piped or slave, stderr=slave,
        env=environment(home), start_new_session=True)
    os.close(slave)
    if piped:
        piped.close()

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
    while time.time() < deadline:
        drain()
        if process.poll() is not None:
            break
        if pending and time.time() >= ready_at:
            wanted, key = pending[0]
            if wanted(output.decode("utf8", "replace")):
                pending.pop(0)
                try:
                    os.write(master, key)
                except OSError:
                    break
                # A beat, so the screen the next key answers is fully written.
                ready_at = time.time() + 0.25
        time.sleep(0.02)
    if process.poll() is None:
        process.kill()
        killed = True
    code = process.wait()
    drain()
    os.close(master)
    if killed:
        output.extend(b"\n[the run did not exit; killed]")
    return code, output.decode("utf-8", "replace")


def report(home, name="report.json"):
    path = os.path.join(home, name)
    if not os.path.exists(path):
        return None
    with open(path) as fh:
        return json.load(fh)


def groups_of(obj):
    return sorted(tuple(sorted(group)) for group in obj["groups"])


def typed(text):
    return [bytes([c]) for c in text.encode()]


def after_menu(text):
    """The bytes written between the menu giving the screen back and the
    browser taking it, which is exactly where the progress line lives."""
    left = text.find(ALT_OFF)
    if left < 0:
        return None, None
    entered = text.find(ALT_ON, left)
    return text[left + len(ALT_OFF):entered if entered >= 0 else None], entered


def at_menu(text):
    return "dedup-rs—beforethescan" in squeeze(text)


def at_browser(text):
    """The second alternate screen of the run: the menu's, then the
    browser's."""
    return text.count(ALT_ON) >= 2


def keys_for(sequence, then=()):
    """`sequence` typed at the menu, then `then` typed at the browser."""
    always = lambda text: True
    return ([(at_menu, sequence[0])]
            + [(always, key) for key in sequence[1:]]
            + [(at_browser, key) for key in then])


# ------------------------------------------------------------------ the cases


def piped(binary, root, home, where, jobs=None):
    flags = ["--min-size", "1", "--no-cache", "--list", "--report-json", where]
    if jobs:
        flags += ["-j", str(jobs)]
    return subprocess.run(
        [os.path.abspath(binary), os.path.abspath(root), *flags],
        capture_output=True, timeout=300, env=environment(home))


def case_pipe_is_silent(binary, base, main):
    """Not a terminal, no line — and the run is the run it always was.

    The counters are diffed at one reader. `seeks` is not deterministic with
    more than one: concurrent readers interleave and so count against each
    other, which is the honest answer for one set of heads and is why M6 scores
    it the way it does. Main disagrees with itself by one on a repeated run at
    the default `-j`, so a comparison there would be measuring the scheduler.
    """
    root, home = fresh(base, "piped", pairs=2, mib=4, smalls=200)
    mine = os.path.join(home, "mine.json")
    run = piped(binary, root, home, mine)
    if run.returncode != 0:
        return "FAIL: exit {}".format(run.returncode)
    if run.stderr != b"":
        return "FAIL: stderr was not empty: {!r}".format(run.stderr[:200])
    if b"\x1b" in run.stdout:
        return "FAIL: an escape sequence reached stdout"
    if main is None:
        return "ok (stderr empty, no escapes on stdout; no main build to diff)"
    theirs = os.path.join(home, "theirs.json")
    was = piped(main, root, home, theirs)
    if was.stdout != run.stdout:
        return "FAIL: stdout differs from main"
    if was.stderr != run.stderr:
        return "FAIL: stderr differs from main"
    counters = ("bytes_read", "opens", "seeks", "files_seen", "bytes_seen")
    piped(binary, root, home, mine, jobs=1)
    piped(main, root, home, theirs, jobs=1)
    mine_json, theirs_json = report(home, "mine.json"), report(home, "theirs.json")
    for field in counters:
        if mine_json[field] != theirs_json[field]:
            return "FAIL: {} is {} here and {} on main".format(
                field, mine_json[field], theirs_json[field])
    if groups_of(mine_json) != groups_of(theirs_json):
        return "FAIL: the groups differ from main"
    return ("ok (stdout byte-identical to main, {} bytes; stderr empty on both; "
            "{} counters equal at -j1)".format(len(run.stdout), len(counters)))


def case_terminal_paints_and_hands_over(binary, base, main):
    """The line paints, then is cleared, then the browser's screen opens."""
    # Three times the small files of the other cases, so the stage that reads
    # them outlives several beats and the bar is caught partway rather than
    # only at its end.
    root, home = fresh(base, "painted", smalls=12000)
    # One reader, so the scan reliably outlives a 100 ms beat on a fast disk.
    code, text = drive(binary, root, home, keys_for([ENTER], then=[b"q"]),
                       extra=["-i", "-j", "1", "--min-size", "1", "--no-cache",
                              "--report-json", os.path.join(home, "report.json")])
    if code != 0:
        return "FAIL: exit {}".format(code)
    between, entered = after_menu(text)
    if between is None:
        return "FAIL: the menu never gave the screen back"
    if entered < 0:
        return "FAIL: the browser never opened"
    painted = [label for label in ("Scanning", "Reading small files", "Sampling",
                                   "Sampling further", "Sampling deep",
                                   "Comparing") if label + "…" in between]
    if not painted:
        return "FAIL: no stage ever painted:\n{!r}".format(between[:400])
    if "\x1b[2K" not in between:
        return "FAIL: the line was never repainted over itself"
    # The law: nothing of the line survives into the browser's screen. The last
    # thing written before the alternate screen opens is the clear.
    if not between.endswith(CLEAR):
        return ("FAIL: the browser opened on top of the line, not after it:\n"
                "{!r}".format(between[-200:]))
    if "Group1of" not in squeeze(text[entered:]):
        return "FAIL: the browser screen is not the browser"
    # The bar has to move, and move forwards, within the stage it belongs to.
    # A stage whose counter is never told anything paints a bar frozen at
    # nought and looks identical to one that has genuinely just started.
    bars = re.findall(r"\x1b\[2K\s+([A-Za-z][A-Za-z ]*?)…\s+[█░]+\s+\d+%\s+"
                      r"([\d,]+)/([\d,]+) ·", between)
    if not bars:
        return "FAIL: no bar was ever drawn:\n{!r}".format(between[:400])
    moved = []
    for label in dict.fromkeys(stage for stage, _, _ in bars):
        counts = [(int(done.replace(",", "")), int(total.replace(",", "")))
                  for stage, done, total in bars if stage == label]
        dones = [done for done, _ in counts]
        if dones != sorted(dones) or any(done > total for done, total in counts):
            return "FAIL: {} went backwards or past its end: {}".format(
                label, counts)
        moved.append("{} {}→{}/{}".format(label.strip(), dones[0], dones[-1],
                                          counts[-1][1]))
    if not any(done for _, done, _ in
               [(s, int(d.replace(",", "")), t) for s, d, t in bars]):
        return "FAIL: no bar ever moved off nought: {}".format(bars)
    repaints = between.count("\x1b[2K")
    return ("ok ({} repaints, stages {}, bar {}, cleared before the "
            "browser)".format(repaints, "+".join(painted), "; ".join(moved)))


def case_the_walk_counts_out_loud(binary, base, main):
    """The other body shape: files and bytes, with no bar, while walking.

    A `--min-size` above everything in the tree means the walk is the only
    stage there is, so this cannot accidentally be watching a reading stage.
    """
    root = os.path.join(base, "walked")
    home = os.path.join(base, "walked-home")
    os.makedirs(home)
    # A hundred thousand of them, spread over four hundred directories, which
    # is what it takes for the walk alone to outlive a beat on a fast disk.
    for directory in range(400):
        here = os.path.join(root, "d%03d" % directory)
        os.makedirs(here)
        for n in range(250):
            with open(os.path.join(here, "w%04d.bin" % n), "wb") as fh:
                fh.write(b"%08d" % n)
    code, text = drive(binary, root, home, [],
                       extra=["--list", "--min-size", "1g", "--no-cache",
                              "--report-json", os.path.join(home, "report.json")])
    if code != 0:
        return "FAIL: exit {}".format(code)
    if "Scanning…" not in text:
        return "FAIL: the walk never painted:\n{!r}".format(text[:400])
    if "files ·" not in text:
        return "FAIL: the walk drew a bar instead of a count:\n{!r}".format(text[:400])
    said = report(home)
    if said is None or said["files_seen"] != 100000 or said["groups"]:
        return "FAIL: the walk did not see the tree it walked"
    # The count on screen is the count in the report, not something beside it.
    shown = [int(seen.replace(",", "")) for seen in
             re.findall(r"Scanning…\s+([\d,]+) files", text)]
    if not shown or max(shown) > said["files_seen"] or shown != sorted(shown):
        return "FAIL: the counts shown were {}, the scan saw {}".format(
            shown, said["files_seen"])
    return ("ok ({} repaints, counts {}…{} against {} files seen, "
            "never ahead and never backwards)".format(
                text.count("\x1b[2K"), shown[0], shown[-1], said["files_seen"]))


def case_quiet_paints_nothing(binary, base, main):
    """A terminal is not enough; --quiet still means nothing."""
    root, home = fresh(base, "quiet")
    code, text = drive(binary, root, home, keys_for([ENTER], then=[b"q"]),
                       extra=["-i", "-j", "1", "--quiet", "--min-size", "1",
                              "--no-cache", "--report-json",
                              os.path.join(home, "report.json")])
    if code != 0:
        return "FAIL: exit {} — the screen ended {!r}".format(code, text[-300:])
    between, entered = after_menu(text)
    if between is None or entered < 0:
        return "FAIL: the run never reached the browser"
    if between.strip() != "":
        return "FAIL: --quiet still wrote {!r}".format(between[:200])
    said = report(home)
    if said is None or not said["groups"]:
        return "FAIL: the quiet run found nothing"
    return "ok (nothing between the menu and the browser, {} groups found)".format(
        len(said["groups"]))


def case_list_stdout_stays_clean(binary, base, main):
    """A terminal on stderr and a pipe on stdout: the line goes to the
    terminal and the report goes down the pipe, and neither is in the other."""
    root, home = fresh(base, "listed", pairs=3, mib=16, smalls=1500)
    where = os.path.join(home, "stdout.txt")
    code, text = drive(binary, root, home, [],
                       extra=["--list", "-j", "1", "--min-size", "1",
                              "--no-cache", "--report-json",
                              os.path.join(home, "report.json")],
                       stdout_to=where)
    if code != 0:
        return "FAIL: exit {}".format(code)
    with open(where, "rb") as fh:
        listed = fh.read()
    if b"\x1b" in listed or b"\r" in listed:
        return "FAIL: the progress line leaked into the piped stdout"
    if b"duplicate group(s)" not in listed:
        return "FAIL: the list never reached the pipe: {!r}".format(listed[:200])
    if "\x1b[2K" not in text:
        return "FAIL: no line was painted on the terminal"
    if "duplicate group(s)" in text:
        return "FAIL: the list was written to the terminal as well as the pipe"
    return ("ok ({} bytes of list down the pipe, {} repaints on the terminal, "
            "neither in the other)".format(len(listed), text.count("\x1b[2K")))


def case_menu_takes_human_sizes(binary, base, main):
    """100mb, 1m and 1k typed into the field, and the groups that follow."""
    wanted = {
        # Nothing in the tree reaches 100 MiB, one pair of every size above a
        # mebibyte does, and a kibibyte lets the small ones in too.
        "100mb": [],
        "1m": ([("big-%da.bin" % n, "big-%db.bin" % n) for n in range(6)]
               + [("mid-a.bin", "mid-b.bin")]),
        "1k": None,   # everything, checked by count instead
    }
    lines = []
    for word, expected in wanted.items():
        root, home = fresh(base, "size-" + word, pairs=6, mib=24, smalls=40)
        where = os.path.join(home, "report.json")
        keys = keys_for([SPACE] + typed(word) + [ENTER, ENTER], then=[b"q"])
        code, text = drive(binary, root, home, keys,
                           extra=["-i", "--no-cache", "--report-json", where])
        if code != 0:
            return "FAIL: {} exited {}".format(word, code)
        said = report(home)
        if said is None:
            return "FAIL: {} wrote no report".format(word)
        got = groups_of(said)
        if expected is not None and got != sorted(expected):
            return "FAIL: {} reported {}, wanted {}".format(word, got, expected)
        if expected is None and len(got) < 7:
            return "FAIL: {} reported only {} groups".format(word, len(got))
        lines.append("{}→{} group(s)".format(word, len(got)))
    return "ok ({})".format(", ".join(lines))


def case_menu_shows_a_size_as_a_size(binary, base, main):
    """A size given in bytes on the command line is shown as a size, and the
    words shown are the words the scan runs on."""
    root, home = fresh(base, "prefill", pairs=1, mib=4, smalls=40)
    where = os.path.join(home, "report.json")
    code, text = drive(binary, root, home, keys_for([ENTER], then=[b"q"]),
                       extra=["-i", "--min-size", "2097152", "--no-cache",
                              "--report-json", where])
    if code != 0:
        return "FAIL: exit {}".format(code)
    screen = squeeze(text)
    if "min-size2MiB" not in screen:
        return "FAIL: the field did not show 2 MiB:\n{}".format(screen[:400])
    if "2097152" in screen:
        return "FAIL: the field still shows the byte count"
    said = report(home)
    got = groups_of(said)
    want = [("big-0a.bin", "big-0b.bin"), ("mid-a.bin", "mid-b.bin")]
    if got != sorted(want):
        return "FAIL: the humanised prefill changed the scan: {}".format(got)
    return "ok (shown as 2 MiB, and 2 MiB is what the scan used)"


def case_a_bad_size_is_refused_in_the_field(binary, base, main):
    """The teaching message, on the screen the maintainer hit it on."""
    root, home = fresh(base, "refused", pairs=1, mib=4, smalls=20)
    where = os.path.join(home, "report.json")
    keys = keys_for([SPACE] + typed("100 bananas") + [ENTER, ESC, ESC])
    code, text = drive(binary, root, home, keys,
                       extra=["-i", "--no-cache", "--report-json", where])
    if code != 0:
        return "FAIL: exit {}".format(code)
    screen = squeeze(text)
    if "notasize:" not in screen or "512k,100mb,1.5g" not in screen:
        return "FAIL: the field did not teach:\n{}".format(screen[:600])
    if os.path.exists(where):
        return "FAIL: a scan ran on a value the field refused"
    return "ok (refused in the field, in words that say what to type instead)"


def main_(argv):
    if not 3 <= len(argv) <= 4:
        raise SystemExit(__doc__.strip().splitlines()[-1])
    binary, base = argv[1], argv[2]
    main = argv[3] if len(argv) > 3 else None
    base = os.path.join(os.path.abspath(base), "m5_2-pulse")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    cases = [
        ("a piped run says nothing at all", case_pipe_is_silent),
        ("a terminal gets a line, and gets it back", case_terminal_paints_and_hands_over),
        ("the walk counts files out loud", case_the_walk_counts_out_loud),
        ("--quiet means quiet on a terminal too", case_quiet_paints_nothing),
        ("--list down a pipe, the line on the terminal", case_list_stdout_stays_clean),
        ("the menu takes 100mb, 1m and 1k", case_menu_takes_human_sizes),
        ("bytes on the command line, a size on screen", case_menu_shows_a_size_as_a_size),
        ("a size that is not one is refused in the field", case_a_bad_size_is_refused_in_the_field),
    ]
    failed = 0
    for name, run in cases:
        answer = run(binary, base, main)
        failed += answer.startswith("FAIL")
        print("{:<46} {}".format(name, answer))
        sys.stdout.flush()
    shutil.rmtree(base, ignore_errors=True)
    print()
    if failed:
        print("{} case(s) wrong".format(failed))
        return 1
    print("every case behaves")
    return 0


if __name__ == "__main__":
    sys.exit(main_(sys.argv))
