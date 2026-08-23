#!/usr/bin/env python3
"""The competitors, and the flags that make them answer the same question.

Every tool here is asked to do one job: walk a tree and report which files
hold identical bytes. They disagree about defaults — what counts as a file,
what a hardlink is, whether a hash is proof — so the job only becomes the
same job once those defaults are spelled out and overridden. That is what
the ALIGNMENT table below is: one row per semantic, one comment per choice,
and an honest note wherever a tool cannot be made to answer our question.

Nothing in this file may run a tool's deletion, linking or dedupe mode. Each
adapter builds a scan-and-report command line and nothing else, and the
commands are asserted against a deny-list before they are ever spawned.

Every tool writes its answer to a file the harness opens, rather than down a
pipe the harness drains: piping puts the reader's scheduling inside the
measurement, and only one of these tools has an --output flag, so redirection
is what makes the three commands comparable.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess

# Modes that change the filesystem. XBENCH measures scans; it never lets a
# competitor loose on a tree, not even a generated one. Matched against the
# whole argv of anything about to be spawned.
FORBIDDEN = ("--delete", "--link-soft", "--link-hard", "--dedupe", "-B",
             "-d", "-l", "-L", "-N", "remove", "move", "link", "dedupe")


class ToolError(Exception):
    """A competitor is missing, or refused to run."""


# ─────────────────────────────────────────────────────────────────────────────
# the flag alignment table
# ─────────────────────────────────────────────────────────────────────────────
#
# Read this as: for each semantic, what our tool does, and what each competitor
# has to be told so it does the same. "(default)" means the tool already agrees
# and is given no flag — recorded anyway, because an unstated default is the
# most common way a benchmark quietly stops being fair.
#
# Verified empirically on 2026-08-23 against fclones 0.35.0 and jdupes 1.31.1
# on a hand-planted tree whose answer was known in advance; the observations
# below are what those runs actually printed, not what the manuals promise.

ALIGNMENT = [
    {
        "semantic": "recursion",
        "ours": "always recursive",
        "fclones": "(default) recursive",
        "jdupes": "-r",
        "why": "jdupes does NOT recurse without -r: given a directory it "
               "considers only the files directly inside it. Omitting -r "
               "would have jdupes scan a hundredth of the corpus and finish "
               "first, which is the single easiest way to publish a fake win.",
    },
    {
        "semantic": "minimum size",
        "ours": "--min-size N (inclusive)",
        "fclones": "--min N (inclusive)",
        "jdupes": "-X size+=:N (>= N)",
        "why": "Each scenario carries its own min_size from bench/corpus.py, "
               "and all three spellings are inclusive of N. fclones' own "
               "default is 1 byte, jdupes has no minimum at all and only "
               "excludes zero-length files; left alone the three tools would "
               "be sizing up different populations. jdupes' '+=' is 'larger "
               "or equal' — the '=' is what makes it inclusive, and dropping "
               "it would silently exclude every file of exactly N bytes.",
    },
    {
        "semantic": "hidden files",
        "ours": "included",
        "fclones": "--hidden",
        "jdupes": "(default) included",
        "why": "The two competitors default in opposite directions. fclones "
               "skips dotfiles unless told otherwise — measured: a planted "
               "group under .hidden/ was absent from its default output and "
               "present with --hidden. jdupes includes them unless given -A.",
    },
    {
        "semantic": "ignore files",
        "ours": "no such concept",
        "fclones": "--no-ignore",
        "jdupes": "(n/a)",
        "why": "fclones honours .gitignore and .fdignore by default. Our "
               "corpora contain neither, so this changes nothing today — it "
               "is passed so that the day a generator writes one, fclones "
               "does not quietly scan a smaller tree than everyone else.",
    },
    {
        "semantic": "symlinks",
        "ours": "(default) never followed, never a member",
        "fclones": "(default) not followed, not reported",
        "jdupes": "(default) not followed",
        "why": "All three already agree, so no flag is passed. Verified: a "
               "symlink planted beside its target appeared in no tool's "
               "output. fclones' -L/-S and jdupes' -s would each break this "
               "and are deliberately absent.",
    },
    {
        "semantic": "hardlinks",
        "ours": "names collapse onto one record per inode",
        "fclones": "(default) no -H; still lists every name",
        "jdupes": "(default) no -H; lists one name per inode",
        "why": "The one place where flags cannot finish the job. All three "
               "agree that hardlinks ALONE are not duplicates: a two-name "
               "inode with no content twin was reported by nobody. They "
               "disagree about how to print a group that does contain one. "
               "Planted: inode I with names {hlc_a, hlc_b}, plus a separate "
               "file hlc_twin holding the same bytes. Ours reported "
               "[hlc_a, hlc_twin]; jdupes reported [hlc_a, hlc_twin]; "
               "fclones reported [hlc_a, hlc_b, hlc_twin] — three paths, two "
               "inodes. fclones has no flag that collapses them. So "
               "correctness is judged on INODES, not paths (see "
               "groups_as_inodes): every tool's answer is mapped through "
               "lstat before it is compared, which is fair to all three and "
               "flatters none. The path-level difference is real and is "
               "recorded in the report rather than being normalised away.",
    },
    {
        "semantic": "proof of equality",
        "ours": "full byte comparison",
        "fclones": "hash only (metro, 128-bit, non-cryptographic)",
        "jdupes": "(default) full byte comparison",
        "why": "NOT a flag difference — a guarantee difference, and the "
               "reason correctness sits beside speed in every table. fclones "
               "never compares bytes; it groups by hash and is right with "
               "very high probability. Ours and jdupes prove it. fclones is "
               "therefore doing strictly less work for the same-looking "
               "answer, and a fair report says so rather than pretending the "
               "three tools bought the same thing. fclones' "
               "--skip-content-hash would make the gap far larger and is not "
               "used; -Q would do the same to jdupes and is not used either.",
    },
    {
        "semantic": "persistent cache (lane B only)",
        "ours": "cache on (lane A passes --no-cache)",
        "fclones": "--cache",
        "jdupes": "-y <db>",
        "why": "XBENCH.md was written believing fclones had no persistent "
               "cache. It has: 0.35.0 ships --cache, and jdupes ships a hash "
               "database behind -y. Lane B therefore turns all three on, "
               "which is a fairer contrast than the spec assumed. Each "
               "tool's cache is redirected into a directory this run owns — "
               "fclones writes to $HOME/Library/Caches/fclones, so its child "
               "process gets a private HOME (XDG_CACHE_HOME is ignored on "
               "macOS; measured). Sharing a cache between rounds of "
               "different tools, or between two runs on one machine, would "
               "let a tool read another's leavings.",
    },
    {
        "semantic": "threads",
        "ours": "(default) machine parallelism",
        "fclones": "(default) machine parallelism",
        "jdupes": "(default) single-threaded",
        "why": "Left alone on purpose. How much parallelism a tool reaches "
               "for is part of the tool, not part of the task, and pinning "
               "everyone to one thread would measure a program none of these "
               "projects ship. jdupes being single-threaded is a fact about "
               "jdupes and is noted in the report so nobody reads its column "
               "as a hardware result.",
    },
    {
        "semantic": "output destination",
        "ours": "--report-json FILE",
        "fclones": "-o FILE --format json",
        "jdupes": "-j, stdout redirected to FILE",
        "why": "Every tool writes to a file the harness opened, so no tool "
               "pays for a pipe the others do not. jdupes has no output flag, "
               "so its stdout is redirected to a file descriptor rather than "
               "drained by the parent.",
    },
    {
        "semantic": "empty-tree output",
        "ours": "normal JSON report",
        "fclones": "normal JSON, groups: []",
        "jdupes": "plain text, -j ignored",
        "why": "A jdupes quirk, not a flag: given a directory containing no "
               "files at all it prints 'No duplicates found.' and ignores -j "
               "entirely. A directory that has files but no duplicates does "
               "produce JSON with an empty matchSets. Only the fixed-startup "
               "measurement scans an empty tree, but that measurement would "
               "have failed outright without handling it.",
    },
    {
        "semantic": "progress reporting",
        "ours": "(default) quiet when not a tty",
        "fclones": "--progress=false",
        "jdupes": "-q",
        "why": "Rendering a progress bar is work, and it is not the work "
               "being measured. Every tool is silenced the way it offers.",
    },
]


def alignment_table() -> str:
    """The fairness table, first thing in every report."""
    head = ("semantic", "ours", "fclones", "jdupes")
    rows = [head] + [(a["semantic"], a["ours"], a["fclones"], a["jdupes"])
                     for a in ALIGNMENT]
    widths = [max(len(r[i]) for r in rows) for i in range(len(head))]
    out = ["| " + " | ".join(c.ljust(widths[i]) for i, c in enumerate(head)) + " |",
           "|" + "|".join("-" * (w + 2) for w in widths) + "|"]
    for row in rows[1:]:
        out.append("| " + " | ".join(c.ljust(widths[i]) for i, c in enumerate(row)) + " |")
    return "\n".join(out)


# ─────────────────────────────────────────────────────────────────────────────
# what a tool's answer is compared as
# ─────────────────────────────────────────────────────────────────────────────

def groups_as_inodes(tree: str, groups: list) -> frozenset:
    """A tool's answer, reduced to what it actually claims about the tree.

    Names are how tools disagree; inodes are what they are disagreeing about.
    fclones prints every name a hardlinked inode wears and jdupes prints one,
    and neither is wrong — but comparing name-sets would score that difference
    as a wrong answer, and it is not one. Mapping through lstat first asks the
    only question that matters: which files did you say hold identical bytes?

    lstat rather than stat, so a symlink counts as itself. A tool that reports
    a link beside its target is making a claim the others are not, and this
    must show up as a difference rather than being folded into the target.
    """
    out = set()
    for group in groups:
        inodes = set()
        for name in group:
            path = name if os.path.isabs(name) else os.path.join(tree, name)
            try:
                inodes.add(os.lstat(path).st_ino)
            except OSError:
                # A reported path that is not there is a claim about a file
                # that does not exist. Keep it, distinguishably, so the group
                # cannot accidentally match a real one.
                inodes.add("missing:" + os.path.relpath(path, tree))
        if len(inodes) > 1:
            out.add(frozenset(inodes))
    return frozenset(out)


def truth_as_inodes(tree: str, manifest: dict) -> frozenset:
    """The planted answer, in the same currency.

    Derived from corpus construction — the generator recorded every group it
    laid down and every name each inode wears — and resolved to inodes through
    exactly the path the tools' answers take, so the comparison is like for
    like rather than manifest-shaped.
    """
    return groups_as_inodes(tree, [[m["names"][0] for m in g["members"]]
                                   for g in manifest["groups"]])


def compare(want: frozenset, got: frozenset) -> tuple:
    """(exact, description) for one tool's answer against the truth.

    The description never repeats the verdict — the caller renders that — so
    that a table cell reads "exact (10 groups)" rather than saying exact twice.
    """
    if want == got:
        return True, "({} group{})".format(len(want), "" if len(want) == 1 else "s")
    missing, extra = want - got, got - want
    bits = []
    if missing:
        bits.append("{} missed".format(len(missing)))
    if extra:
        bits.append("{} spurious".format(len(extra)))
    return False, "{} groups vs {} — {}".format(len(got), len(want),
                                                ", ".join(bits))


# ─────────────────────────────────────────────────────────────────────────────
# the adapters
# ─────────────────────────────────────────────────────────────────────────────

class Tool:
    """One competitor: how to invoke it, and how to read what it wrote.

    Subclasses supply `argv`, `parse` and `version`. Everything else — the
    deny-list check, the private cache directory, the output file — is the
    same for all of them so that no tool gets a treatment the others did not.
    """

    key = ""
    label = ""

    def __init__(self, binary: str):
        self.binary = binary

    def version(self) -> str:
        raise NotImplementedError

    def argv(self, tree: str, min_size: int, out: str, cached: bool,
             workdir: str) -> list:
        raise NotImplementedError

    def env(self, workdir: str, cached: bool) -> dict:
        return dict(os.environ)

    def stdout_to_file(self) -> bool:
        """True when the tool has no output flag and must be redirected."""
        return False

    def parse(self, tree: str, out: str) -> list:
        raise NotImplementedError

    # ── shared plumbing ──────────────────────────────────────────────────────

    def check_safe(self, argv: list) -> None:
        """No competitor ever runs a mode that touches the filesystem."""
        for arg in argv[1:]:
            if arg in FORBIDDEN:
                raise ToolError("{}: refusing to run with {} — XBENCH scans "
                                "and reports, it never deletes".format(self.key, arg))

    def cache_dir(self, workdir: str) -> str:
        return os.path.join(workdir, "cache-" + self.key)

    def reset_cache(self, workdir: str) -> None:
        path = self.cache_dir(workdir)
        shutil.rmtree(path, ignore_errors=True)
        os.makedirs(path, exist_ok=True)


class Ours(Tool):
    """The Rust tool under test.

    Lane A passes --no-cache, so no round can be answered out of a previous
    round's memory. Lane B leaves the cache on and points DEDUP_CACHE_DIR at a
    directory this run owns: the tool's default is one shared directory per
    machine, which two runs on one box would read out of each other.
    """

    key = "ours"
    label = "dedup-rs"

    def version(self) -> str:
        """The tool has no --version, so its identity is the source it was
        built from: the commit, and whether rust/ was dirty when it was."""
        repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        head = subprocess.run(["git", "-C", repo, "rev-parse", "--short=12", "HEAD"],
                              capture_output=True, text=True)
        dirty = subprocess.run(["git", "-C", repo, "status", "--porcelain",
                                "--", "rust"], capture_output=True, text=True)
        if head.returncode != 0:
            return "dedup-rs (unknown build)"
        return "dedup-rs @ {}{}".format(head.stdout.strip(),
                                        "+dirty" if dirty.stdout.strip() else "")

    def argv(self, tree, min_size, out, cached, workdir):
        cmd = [self.binary, tree, "--report-json", out,
               "--min-size", str(min_size)]
        if not cached:
            cmd.append("--no-cache")
        return cmd

    def env(self, workdir, cached):
        env = dict(os.environ)
        env["DEDUP_CACHE_DIR"] = self.cache_dir(workdir)
        return env

    def parse(self, tree, out):
        with open(out) as fh:
            data = json.load(fh)
        # `groups` is a list of lists of tree-relative names, already collapsed
        # to one canonical name per inode.
        return [list(g) for g in data.get("groups", [])]

    def read_stats(self, out: str) -> dict:
        """Bytes and opens, self-reported. Only ours publishes these."""
        try:
            with open(out) as fh:
                data = json.load(fh)
        except (OSError, ValueError):
            return {}
        return {"bytes_read": data.get("bytes_read"),
                "opens": data.get("opens"),
                "files_seen": data.get("files_seen")}


class Fclones(Tool):
    """fclones 0.35.x — the fastest widely-known duplicate finder.

    --progress must precede the subcommand: it is a flag on `fclones`, not on
    `fclones group`, and putting it after is an error rather than a no-op.
    """

    key = "fclones"
    label = "fclones"

    def version(self) -> str:
        out = subprocess.run([self.binary, "--version"], capture_output=True,
                             text=True)
        return out.stdout.strip() or "unknown"

    def argv(self, tree, min_size, out, cached, workdir):
        return ([self.binary, "--progress=false", "group",
                 "--format", "json", "-o", out,
                 "--hidden",           # fclones skips dotfiles by default
                 "--no-ignore",        # ...and honours .gitignore by default
                 "--min", str(min_size)]
                + (["--cache"] if cached else [])
                + [tree])

    def env(self, workdir, cached):
        # fclones resolves its cache through the platform directory API, which
        # on macOS is $HOME/Library/Caches and ignores XDG_CACHE_HOME
        # (measured). Redirecting HOME for the child is the only handle there
        # is, and it is enough: fclones reads nothing else out of it.
        env = dict(os.environ)
        env["HOME"] = self.cache_dir(workdir)
        return env

    def parse(self, tree, out):
        with open(out) as fh:
            data = json.load(fh)
        return [list(g.get("files", [])) for g in data.get("groups", [])]


class Jdupes(Tool):
    """jdupes 1.31.x — the fdupes lineage, byte-for-byte by default.

    Single-threaded, and told to recurse explicitly because it will not do so
    on its own.
    """

    key = "jdupes"
    label = "jdupes"

    def version(self) -> str:
        out = subprocess.run([self.binary, "--version"], capture_output=True,
                             text=True)
        first = (out.stdout or out.stderr).strip().splitlines()
        return first[0] if first else "unknown"

    def argv(self, tree, min_size, out, cached, workdir):
        db = os.path.join(self.cache_dir(workdir), "hashdb.txt")
        return ([self.binary, "-r", "-q", "-j"]
                + (["-y", db] if cached else [])
                + ["-X", "size+=:{}".format(min_size), tree])

    def stdout_to_file(self):
        return True

    # Given a tree with no files in it at all, jdupes ignores -j and prints
    # this line as plain text instead of JSON. A tree that has files but no
    # duplicates does produce JSON, with an empty matchSets — so this is only
    # ever seen on an empty directory, which is exactly what the fixed-startup
    # measurement scans. Matched exactly rather than by "does it start with
    # {", so that real malformed output is still an error.
    NO_DUPES = "No duplicates found."

    def parse(self, tree, out):
        with open(out) as fh:
            text = fh.read()
        if text.strip() == self.NO_DUPES:
            return []
        data = json.loads(text)
        return [[f["filePath"] for f in s.get("fileList", [])]
                for s in data.get("matchSets", [])]


# ─────────────────────────────────────────────────────────────────────────────
# discovery
# ─────────────────────────────────────────────────────────────────────────────

def discover(ours_binary: str, want: list = None) -> list:
    """Every tool that is actually installed, ours first.

    A competitor that is not on this machine is left out of the report rather
    than reported as infinitely slow.
    """
    found = [Ours(ours_binary)]
    for cls in (Fclones, Jdupes):
        path = shutil.which(cls.key)
        if path:
            found.append(cls(path))
    if want:
        found = [t for t in found if t.key in set(want)]
    return found


def czkawka_note() -> str:
    """Why the third competitor is not here.

    `brew info czkawka` lists fifteen required dependencies including gtk4,
    ffmpeg, librsvg and adwaita-icon-theme: the formula builds the GUI as well
    as the CLI. XBENCH.md admits competitors that are trivially installable,
    and a desktop toolchain is not that. Recorded rather than silently
    omitted, so the absence is a decision and not an oversight.
    """
    return ("czkawka: not installed. The Homebrew formula pulls 15 "
            "dependencies including gtk4, ffmpeg and librsvg (it builds the "
            "GUI alongside the CLI), which is the cask/GUI baggage XBENCH.md "
            "excludes.")


_TIME_RSS = re.compile(r"^\s*(\d+)\s+maximum resident set size", re.M)


def parse_max_rss(stderr: str):
    """Peak RSS in bytes from `/usr/bin/time -l`, or None if it said nothing."""
    found = _TIME_RSS.search(stderr)
    return int(found.group(1)) if found else None
