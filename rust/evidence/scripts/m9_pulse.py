#!/usr/bin/env python3
"""The pty battery: what M9 puts on a real terminal, and what it still does not.

Three of M9's claims cannot be reached by `cargo test`, because they are about
what a terminal sees and `cargo test` never has one:

  * the stderr pulse names the file being read,
  * it names it during the COMPARISON, which is the stage a cloud placeholder
    stalls in and the stage that opens no files of its own,
  * and the pre-flight menu offers `--cloud`.

`m9_mutants.py` deliberately leaves two mutants for this script — deleting the
name from the pulse's line, and deleting `effort.reading` from the comparison —
and this is where they are supposed to die. It also re-proves M5.2's inertness
on the two paths M9 touched, because a milestone that changes every progress
display is exactly the kind that quietly starts writing to a pipe.

Usage, from the repository root:
    python3 rust/evidence/scripts/m9_pulse.py <binary> <scratch-dir>
"""

import fcntl
import json
import os
import pty
import re
import struct
import subprocess
import sys
import termios
import time

ANSI = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b[()][B0]|\x1b[=>]")
ALT_ON = "\x1b[?1049h"
REPAINT = "\r\x1b[2K"

# One member of the group, sized so the comparison outlasts the pulse's 100 ms
# beat. Below this the whole stage finishes inside a single beat, the ticker
# never paints, and the only frames are the boundary ones — which name nothing,
# by design, because a stage that has just begun has read nothing yet. That is
# not a defect and it is not what this script is measuring; it is why the
# corpus is this size.
MEMBER = 300 * 1024 * 1024
COPIES = 4


def squeeze(text):
    return "".join(ANSI.sub("", text).split())


def build(root):
    os.makedirs(os.path.join(root, "media", "holidays"), exist_ok=True)
    block = (b"REEL" * 2048)[:8192]
    for index in range(COPIES):
        path = os.path.join(
            root, "media", "holidays", f"IMG_2019071{index}_183012_reel.MOV"
        )
        if os.path.exists(path) and os.path.getsize(path) == MEMBER:
            continue
        with open(path, "wb") as fh:
            for _ in range(MEMBER // len(block)):
                fh.write(block)
    # The racily-clean rule: a file whose mtime is this second may be dropped
    # from the cache non-deterministically.
    when = time.time() - 10
    for base, _, names in os.walk(root):
        for name in names:
            os.utime(os.path.join(base, name), (when, when))


def fresh(base, name):
    root = os.path.join(base, "tree")
    home = os.path.join(base, name + "-home")
    subprocess.run(["rm", "-rf", home], check=False)
    os.makedirs(os.path.join(home, ".Trash"))
    os.makedirs(root, exist_ok=True)
    build(root)
    return root, home


def environment(home):
    return dict(
        os.environ,
        HOME=home,
        TERM="xterm-256color",
        DEDUP_CACHE_DIR=os.path.join(home, "cache"),
    )


def drive(binary, root, home, keys=(), extra=(), size=(40, 140), patience=240):
    """Run under a pty, typing `keys` as the run reaches each screen."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    process = subprocess.Popen(
        [os.path.abspath(binary), os.path.abspath(root), *extra],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=environment(home),
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
        # Answer a cursor-position request, or the TUI waits for a reply that
        # never comes.
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
    return code, text


def frames(text):
    """Each repainted progress line, as it was painted.

    `[1:]` and not `[:]`: everything before the FIRST repaint was never a
    painted frame, it is whatever the run said on its way here. Counting it
    made a `--quiet` run that painted nothing at all look like it had painted
    one blank frame.
    """
    return [
        piece.split("\n")[0]
        for piece in text.split(REPAINT)[1:]
        if piece.strip()
    ]


def case_the_pulse_names_the_file(binary, base):
    """The claim, on a terminal: which file the numbers are about."""
    root, home = fresh(base, "names")
    code, text = drive(
        binary,
        root,
        home,
        extra=[
            "--min-size",
            "1",
            "--no-cache",
            "--list",
            "-j",
            "2",
            "--report-json",
            os.path.join(home, "report.json"),
        ],
    )
    if code != 0:
        return f"FAIL: exit {code}", []
    painted = frames(text)
    named = [one for one in painted if "←" in one]
    comparing = [one for one in named if "Comparing" in one]
    detail = [
        f"{len(painted)} frames painted, {len(named)} named a file, "
        f"{len(comparing)} of those during the comparison"
    ]
    detail += [f"    {one[:132]}" for one in comparing[:3]]
    if not named:
        return "FAIL: no painted frame named the file being read", detail
    if not comparing:
        # This is the mutant `m9_mutants.py` leaves for this script: the
        # comparison opens no files of its own, so a name recorded at open
        # time is blank for the whole of the longest stage.
        return (
            "FAIL: the comparison named no file — the stage a cloud "
            "placeholder stalls in is exactly the one that went blank",
            detail,
        )
    if not any(".MOV" in one for one in comparing):
        return "FAIL: the named file is not one of the corpus's files", detail
    if not any("…" in one for one in comparing):
        return "FAIL: a long path was not cut to fit the row", detail
    # The rate it shares the row with is a live one, not a lifetime average:
    # a stage reading at speed must show a non-zero rate.
    moving = [one for one in comparing if re.search(r"· [1-9][^ ]* [KMGT]iB/s", one)]
    if not moving:
        return "FAIL: no frame showed a non-zero rate while reading", detail
    detail.append(f"    {len(moving)} of the comparison's frames showed a live rate")
    return "ok", detail


def case_quiet_paints_nothing(binary, base):
    """A terminal is not enough; --quiet still means nothing."""
    root, home = fresh(base, "quiet")
    code, text = drive(
        binary,
        root,
        home,
        extra=[
            "--min-size",
            "1",
            "--no-cache",
            "--list",
            "--quiet",
            "-j",
            "2",
            "--report-json",
            os.path.join(home, "report.json"),
        ],
    )
    if code != 0:
        return f"FAIL: exit {code}", []
    painted = frames(text)
    if painted:
        return f"FAIL: --quiet painted {len(painted)} frame(s)", [
            f"    {one[:120]}" for one in painted[:3]
        ]
    if REPAINT in text:
        return "FAIL: --quiet emitted a repaint sequence", []
    return "ok", ["no repaint, no frame, on a real terminal"]


def case_a_pipe_is_silent(binary, base):
    """M5.2's inertness, re-proved on the path M9 rewrote."""
    root, home = fresh(base, "pipe")
    report = os.path.join(home, "report.json")
    done = subprocess.run(
        [
            os.path.abspath(binary),
            os.path.abspath(root),
            "--min-size",
            "1",
            "--no-cache",
            "--list",
            "-j",
            "2",
            "--report-json",
            report,
        ],
        capture_output=True,
        env=environment(home),
    )
    if done.returncode != 0:
        return f"FAIL: exit {done.returncode}", []
    if done.stderr:
        return f"FAIL: a piped run wrote to stderr: {done.stderr[:200]!r}", []
    if b"\x1b" in done.stdout:
        return "FAIL: an escape sequence reached a piped stdout", []
    with open(report) as fh:
        said = json.load(fh)
    if "skipped_cloud" in said:
        return "FAIL: a tree with no cloud files reported skipped_cloud", []
    if not said["groups"]:
        return "FAIL: the piped run found nothing, so it proved nothing", []
    return "ok", [
        f"stderr empty, stdout free of escapes, {len(said['groups'])} group(s) found, "
        "no skipped_cloud key"
    ]


def case_the_menu_offers_cloud(binary, base):
    """MENU_EXPOSURE says the form must carry it; this is the form.

    Read from the squeezed screen and not line by line: the TUI draws with
    cursor positioning rather than newlines, so stripping the escapes leaves
    the whole form on one line. `label` + `value` with the spacing removed is
    what a row looks like there — "cloudoff" is the `--cloud` row reading off.
    """
    root, home = fresh(base, "menu")
    at_menu = lambda text: "dedup-rs—beforethescan" in squeeze(text)
    detail = []
    seen = {}
    for label, extra in (("default", []), ("with --cloud", ["--cloud"])):
        code, text = drive(
            binary,
            root,
            home,
            keys=[(at_menu, b"q")],
            extra=["--min-size", "1", "--no-cache", *extra],
        )
        flat = squeeze(text)
        if "dedup-rs—beforethescan" not in flat:
            return f"FAIL: the menu never opened ({label}, exit {code})", [
                f"    {text[-200:]!r}"
            ]
        if code != 0:
            return f"FAIL: quitting the menu exited {code} ({label})", detail
        start = flat.find("dedup-rs—beforethescan")
        form = flat[start : start + 520]
        seen[label] = form
        detail.append(f"    {label}: ...{form[form.find('profileauto'):][:40]}...")

    # The row exists, sits with the other "what the scan is" rows, and is off.
    if "cloudoff" not in seen["default"]:
        return "FAIL: the form does not offer --cloud, off by default", detail
    if "profileautocloudoff" not in seen["default"]:
        return "FAIL: the cloud row is not where the form puts it", detail
    # ...and it is a real row, not a fixed string: the flag moves it.
    if "cloudon" not in seen["with --cloud"]:
        return "FAIL: --cloud on the command line did not turn the row on", detail
    if "cloudoff" in seen["with --cloud"]:
        return "FAIL: the row still reads off with --cloud given", detail
    detail.append(
        "    the row tracks the flag: off by default, on with --cloud given"
    )
    return "ok", detail


CASES = [
    ("the pulse names the file being read", case_the_pulse_names_the_file),
    ("--quiet paints nothing, terminal or not", case_quiet_paints_nothing),
    ("a pipe is silent and says nothing about the cloud", case_a_pipe_is_silent),
    ("the pre-flight menu offers --cloud, off", case_the_menu_offers_cloud),
]


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: m9_pulse.py <binary> <scratch-dir>")
    binary, base = sys.argv[1], sys.argv[2]
    os.makedirs(base, exist_ok=True)
    print(f"corpus: {COPIES} × {MEMBER // (1024 * 1024)} MiB identical files")
    print("(sized so the comparison outlasts the pulse's 100 ms beat)")
    print()
    bad = 0
    for name, case in CASES:
        verdict, detail = case(binary, base)
        print(f"{name}: {verdict}")
        for line in detail:
            print(f"  {line}")
        print()
        if verdict.startswith("FAIL"):
            bad += 1
    if bad:
        print(f"FAILED: {bad} of {len(CASES)} cases")
        return 1
    print(f"all {len(CASES)} cases held")
    return 0


if __name__ == "__main__":
    sys.exit(main())
