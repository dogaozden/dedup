#!/usr/bin/env python3
"""XBENCH — our Rust tool against the best open-source duplicate finders.

A measurement instrument, not an RL environment. Nobody optimizes against
this suite, and the only thing it is for is producing numbers we would be
comfortable publishing next to fclones' own. Everything below exists to make
a competitor's number as trustworthy as ours:

  same corpus     one generated tree per scenario, every tool pointed at it,
                  nobody's corpus rebuilt between tools;

  same task       flags aligned per semantic in xbench/tools.py, one comment
                  per choice, and the differences that no flag can close are
                  written into the report instead of being smoothed over;

  same clock      interleaved rounds (ours, fclones, jdupes, ours, ...), one
                  untimed prime run each so every tool meets the same
                  page-cache, and the median of the per-tool times with the
                  dispersion a median carries;

  same scrutiny   every tool's answer is parsed and checked against the
                  corpus's construction-derived truth, and a fast wrong
                  answer is reported as exactly that.

No tool is ever run in a mode that changes the filesystem, ours included.

Two lanes, never mixed. Lane A is the single cold-ish scan with every
persistent cache off. Lane B is the repeat scan with every tool's own cache
on — fclones 0.35 has --cache and jdupes has -y, which XBENCH.md was written
not knowing, and turning all three on is a fairer contrast than turning only
ours on would have been.
"""
from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
STATE = os.path.join(HERE, "state")
REPORTS = os.path.join(HERE, "reports")

sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "bench"))
import corpus                                             # noqa: E402
import corpora                                            # noqa: E402
import tools as toolkit                                   # noqa: E402


def _load_rbench():
    """The RBENCH harness, loaded by path rather than by name.

    Its volume fence, its advisory lock, its live-harness check and its
    dispersion statistic are the protocol XBENCH is required to share, and a
    second copy of them would be a second thing to keep in step. It is loaded
    by explicit path because `import run` is ambiguous in this repo — bench/
    and rbench/ both contain a run.py, both directories end up on sys.path,
    and which one wins depends on the order the modules above happened to
    insert their own paths.
    """
    import importlib.util
    path = os.path.join(REPO, "rbench", "run.py")
    spec = importlib.util.spec_from_file_location("rbench_run", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


rbench = _load_rbench()

DEFAULT_ROOT = rbench.DEFAULT_ROOT
MIN_ROUNDS = 5                       # XBENCH.md's floor, against RBENCH's 9
SCAN_TIMEOUT = 3600

# RBENCH's live-harness check knows about RBENCH's harnesses. XBENCH adds
# itself and the two competitors: a stray fclones or jdupes left running by a
# person is exactly as damaging to a measurement as a stray dedup-rs, and a
# second xbench on the same volume is worse than either.
#
# The leading class is (^|[/\s]) rather than RBENCH's (^|/) because this
# harness is normally started as `python3 xbench/compare.py`, where the
# character before the path is a space: anchored on / alone, xbench would not
# recognise a second copy of itself.
rbench.BUSY_PATTERN = re.compile(
    rbench.BUSY_PATTERN.pattern
    + r"|(^|[/\s])xbench/compare\.py|(^|/)fclones(\s|$)|(^|/)jdupes(\s|$)")

# Written beside each corpus this suite generates. corpus.load() checks
# bench/corpus.py's generator version and knows nothing about ours, so a
# change to xbench/corpora.py would otherwise be measured against a tree built
# by the previous version of it.
STAMP = "xbench-generator.json"

# Fixed startup must be a small part of what is being measured, or the table
# is a report about process spawning. XBENCH.md sets the bar at 5%.
SETUP_SHARE = 0.05


class Fatal(rbench.Fatal):
    """Something about the machine or the harness is wrong — exit 2."""


def human(n) -> str:
    return rbench.human(n)


# ─────────────────────────────────────────────────────────────────────────────
# one timed scan
# ─────────────────────────────────────────────────────────────────────────────

def scan(tool, tree: str, min_size: int, workdir: str, tag: str,
         cached: bool, rss: bool = False) -> dict:
    """Spawn one tool once and time the whole process, spawn to exit.

    The tool's own opinion of how long it took is never asked for. Output goes
    to a file this harness opened — see the note in tools.py about why no tool
    is made to write down a pipe.

    Peak RSS costs a `/usr/bin/time -l` wrapper, so it is taken on a separate
    untimed run rather than inside the measurement: the wrapper is another
    fork and another exec, and paying for it in the timed path would put it in
    every number in the table.
    """
    out = os.path.join(workdir, "out", "{}.json".format(tag))
    os.makedirs(os.path.dirname(out), exist_ok=True)
    try:
        os.unlink(out)                  # never mistake a stale answer for output
    except OSError:
        pass

    argv = tool.argv(tree, min_size, out, cached, workdir)
    tool.check_safe(argv)
    if rss:
        argv = ["/usr/bin/time", "-l"] + argv
    env = tool.env(workdir, cached)

    sink = open(out, "w") if tool.stdout_to_file() else subprocess.DEVNULL
    started = time.perf_counter()
    try:
        proc = subprocess.run(argv, stdout=sink, stderr=subprocess.PIPE,
                              text=True, env=env, timeout=SCAN_TIMEOUT)
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timed out after {}s".format(SCAN_TIMEOUT)}
    finally:
        if sink is not subprocess.DEVNULL:
            sink.close()
    seconds = time.perf_counter() - started

    # jdupes exits 0 with no duplicates and non-zero on real trouble; fclones
    # and ours both exit 0 on success. Anything else is a failed run and is
    # never quietly turned into a timing.
    if proc.returncode != 0:
        return {"ok": False,
                "error": "exited {}: {}".format(proc.returncode,
                                                (proc.stderr or "").strip()[-300:])}
    try:
        groups = tool.parse(tree, out)
    except (OSError, ValueError, KeyError) as e:
        return {"ok": False, "error": "unreadable output: {}".format(e)}

    got = {"ok": True, "seconds": seconds, "groups": groups}
    if rss:
        got["max_rss"] = toolkit.parse_max_rss(proc.stderr or "")
    return got


def setup_seconds(tool, workdir: str) -> float:
    """How long this tool takes to scan an empty directory.

    A ceiling on fixed overhead, taken per tool because the tools do not pay
    the same one, and reported so that a scenario whose times are close to it
    can be recognised as a report about process startup.
    """
    empty = os.path.join(workdir, "empty")
    shutil.rmtree(empty, ignore_errors=True)
    os.makedirs(empty)
    times = []
    for i in range(3):
        got = scan(tool, empty, 1, workdir, "setup-{}-{}".format(tool.key, i),
                   cached=False)
        if not got.get("ok"):
            raise Fatal("{} could not scan an empty directory: {}"
                        .format(tool.key, got.get("error", "?")))
        times.append(got["seconds"])
    return min(times)


# ─────────────────────────────────────────────────────────────────────────────
# one scenario, one lane
# ─────────────────────────────────────────────────────────────────────────────

def measure(scenario: str, tree: str, truth, min_size: int, tools: list,
            workdir: str, rounds: int, cached: bool) -> dict:
    """Interleaved rounds over every tool, plus the correctness each one earned.

    The interleave is the point: A,B,C,A,B,C for the whole run, so a machine
    that gets slower over the afternoon makes every tool slower in the same
    proportion rather than punishing whoever was scheduled last. Each tool
    gets one untimed prime run before the clock starts, so no tool is the one
    that paid to pull the corpus into the page cache.
    """
    lane = "B" if cached else "A"
    entry = {"lane": lane, "rounds": rounds, "tools": {}}

    for tool in tools:
        tool.reset_cache(workdir)
        seen = {"seconds": [], "correct": None, "detail": "", "max_rss": None,
                "groups_found": None, "paths_found": None, "error": None}
        entry["tools"][tool.key] = seen

    # ── the untimed prime run each tool gets before the clock starts ─────────
    #
    # In lane A this is a page-cache warm-up and nothing more. In lane B it is
    # also the run that fills each tool's persistent cache, which is the whole
    # reason lane B exists.
    for tool in tools:
        seen = entry["tools"][tool.key]
        got = scan(tool, tree, min_size, workdir,
                   "{}-{}-{}-prime".format(scenario, lane, tool.key), cached)
        if not got.get("ok"):
            seen["error"] = got.get("error", "?")
            continue
        exact, detail = toolkit.compare(
            truth, toolkit.groups_as_inodes(tree, got["groups"]))
        seen["correct"], seen["detail"] = exact, detail
        seen["groups_found"] = len(got["groups"])
        seen["paths_found"] = sum(len(g) for g in got["groups"])

    # ── peak RSS, out of band ────────────────────────────────────────────────
    for tool in tools:
        seen = entry["tools"][tool.key]
        if seen["error"]:
            continue
        got = scan(tool, tree, min_size, workdir,
                   "{}-{}-{}-rss".format(scenario, lane, tool.key), cached,
                   rss=True)
        if got.get("ok"):
            seen["max_rss"] = got.get("max_rss")

    # ── the timed rounds ─────────────────────────────────────────────────────
    live = [t for t in tools if not entry["tools"][t.key]["error"]]
    for index in range(rounds):
        for tool in live:
            seen = entry["tools"][tool.key]
            got = scan(tool, tree, min_size, workdir,
                       "{}-{}-{}-{}".format(scenario, lane, tool.key, index),
                       cached)
            if not got.get("ok"):
                seen["error"] = got.get("error", "?")
                continue
            seen["seconds"].append(got["seconds"])
            # An answer that changes between rounds is not an answer. Checked
            # every round rather than once, because a cache is exactly the
            # thing that can make round two disagree with round one.
            exact, detail = toolkit.compare(
                truth, toolkit.groups_as_inodes(tree, got["groups"]))
            if exact != seen["correct"]:
                seen["correct"], seen["detail"] = False, (
                    "unstable: round {} disagreed with the prime run "
                    "({})".format(index, detail))

    for tool in tools:
        seen = entry["tools"][tool.key]
        if seen["seconds"]:
            seen["median"] = statistics.median(seen["seconds"])
            seen["spread"] = rbench.spread(seen["seconds"])
        else:
            seen["median"] = seen["spread"] = None
    return entry


def winner(entry: dict, tools: list) -> str:
    """Who won this scenario, stated plainly, or why nobody did.

    Correctness first: a tool with the wrong answer is not in the race,
    whatever its clock said. XBENCH.md forbids a crowned number across
    scenarios, so this is only ever said per scenario and never added up.
    """
    label = {t.key: t.label for t in tools}
    ranked = sorted(((s["median"], label.get(key, key))
                     for key, s in entry["tools"].items()
                     if s.get("median") is not None and s.get("correct")),
                    key=lambda p: p[0])
    if not ranked:
        return "no tool produced both a correct answer and a time"
    if len(ranked) == 1:
        return "{} (the only tool with a correct answer)".format(ranked[0][1])
    (best_s, best), (next_s, runner) = ranked[0], ranked[1]
    return "{} by {:.2f}x over {}".format(best, next_s / best_s, runner)


# ─────────────────────────────────────────────────────────────────────────────
# the report
# ─────────────────────────────────────────────────────────────────────────────

def scenario_table(entry: dict, tools: list) -> str:
    # `paths` sits beside `groups` because that is where the one difference no
    # flag could close becomes visible: on a tree with hardlinks, fclones
    # prints more paths than the others for the same groups.
    head = ("tool", "median s", "spread", "correctness", "groups", "paths",
            "peak RSS")
    rows = [head]
    for tool in tools:
        seen = entry["tools"].get(tool.key)
        if seen is None:
            continue
        if seen["error"]:
            rows.append((tool.label, "-", "-", "FAILED: " + seen["error"][:60],
                         "-", "-", "-"))
            continue
        rows.append((
            tool.label,
            "{:.4f}".format(seen["median"]) if seen["median"] else "-",
            "±{:.1%}".format(seen["spread"] / seen["median"])
            if seen["median"] else "-",
            ("exact " if seen["correct"] else "DIFFERS ") + seen["detail"],
            str(seen["groups_found"]) if seen["groups_found"] is not None else "-",
            str(seen["paths_found"]) if seen["paths_found"] is not None else "-",
            human(seen["max_rss"]) if seen["max_rss"] else "-",
        ))
    widths = [max(len(r[i]) for r in rows) for i in range(len(head))]
    out = ["| " + " | ".join(c.ljust(widths[i]) for i, c in enumerate(head)) + " |",
           "|" + "|".join("-" * (w + 2) for w in widths) + "|"]
    for row in rows[1:]:
        out.append("| " + " | ".join(c.ljust(widths[i]) for i, c in enumerate(row)) + " |")
    return "\n".join(out)


def write_report(results: dict, tools: list, path: str) -> None:
    meta = results["meta"]
    out = ["# XBENCH — {}".format(meta["started_iso"][:10]), ""]

    if meta["smoke"]:
        out += [
            "> **SMOKE RUN — NOT PUBLISHABLE.** Taken at {} scale on {}, to "
            "prove the harness functions. Corpora this small make fixed "
            "startup a large share of every time in the table{}. Nothing here "
            "is a result: the correctness column and the fact that every tool "
            "ran at all are the only things this report establishes."
            .format(meta["scale"], meta["volume_kind"],
                    ", and the machine was not quiet"
                    if meta["other_harnesses_live"] else ""), ""]

    out += ["## Environment", "",
            "| field | value |", "|-------|-------|"]
    for key, value in (
            ("host", meta["host"]),
            ("macOS", meta["os"]),
            ("CPU", meta["cpu"]),
            ("our commit", meta["commit"]),
            ("volume", "{} ({})".format(meta["root"], meta["volume_kind"])),
            ("scale", meta["scale"]),
            ("generation seed", meta["seed"]),
            ("rounds per tool", meta["rounds"]),
            ("load average", "{:.2f} {:.2f} {:.2f}".format(*meta["load_average"])),
            ("other harnesses live", ", ".join(meta["other_harnesses_live"]) or "none"),
            ("corpus generator", "bench/corpus.py v{}, xbench v{}".format(
                corpus.GENERATOR_VERSION, corpora.XBENCH_GENERATOR_VERSION)),
    ):
        out.append("| {} | {} |".format(key, value))
    out.append("")

    out += ["### Tool versions", "", "| tool | version | fixed startup |",
            "|------|---------|---------------|"]
    for tool in tools:
        out.append("| {} | {} | {} |".format(
            tool.label, meta["versions"].get(tool.key, "?"),
            "{:.4f}s".format(meta["setup"][tool.key])
            if meta["setup"].get(tool.key) is not None else "-"))
    out += ["", meta["czkawka_note"], ""]

    out += ["## Flag alignment", "",
            "Every tool is asked the same question. This is how each one has "
            "to be told to ask it; the reasoning behind each row is in "
            "`xbench/tools.py`.", "",
            toolkit.alignment_table(), ""]

    out += ["### Differences no flag can close", ""]
    for line in results["deltas"]:
        out.append("- " + line)
    out.append("")

    for lane, title, blurb in (
            ("A", "Lane A — single scan, all caches off",
             "Every persistent cache disabled (ours `--no-cache`, fclones "
             "without `--cache`, jdupes without `-y`). One untimed prime run "
             "per tool first, so all three meet the same page-cache."),
            ("B", "Lane B — repeat scan, every tool's own cache on",
             "Each tool re-run the way a user would re-run it, with its own "
             "persistent cache enabled and isolated to a directory this run "
             "owns. The prime run is what fills those caches.")):
        lanes = results["lanes"].get(lane)
        if not lanes:
            continue
        out += ["## {}".format(title), "", blurb, ""]
        for name in results["order"]:
            entry = lanes.get(name)
            if entry is None:
                continue
            shape = results["shapes"][name]
            out += ["### {} ({})".format(name, lane), "",
                    "{} files, {}, min-size {}, {} planted groups.".format(
                        shape["file_count"], human(shape["total_bytes"]),
                        human(shape["min_size"]), shape["planted_groups"]), "",
                    scenario_table(entry, tools), "",
                    "**Winner:** {}".format(winner(entry, tools)), ""]

    out += ["## What this suite does not say", "",
            "- No geomean, and no single headline across tools. XBENCH.md "
            "forbids it: one crowned number invites gaming, and averaging "
            "across shapes hides the scenarios where we lose.",
            "- fclones' times and ours are not buying the same guarantee. "
            "fclones groups by hash and never compares bytes; ours and jdupes "
            "prove equality by reading. See the flag-alignment table.",
            "- jdupes is single-threaded by design. Its column is not a "
            "hardware result.", ""]
    with open(path, "w") as fh:
        fh.write("\n".join(out))


# ─────────────────────────────────────────────────────────────────────────────
# the run
# ─────────────────────────────────────────────────────────────────────────────

def semantic_deltas(results: dict) -> list:
    """The differences the flag table could not close, as the report states them.

    XBENCH.md law 2: where a tool cannot express our semantics, the difference
    is recorded rather than silently ignored.
    """
    return [
        "**Hardlinks are printed differently.** fclones lists every name a "
        "hardlinked inode wears; ours and jdupes list one. Correctness is "
        "therefore judged on inodes (`lstat` on every reported path) rather "
        "than on names, which is fair to all three. The raw path counts still "
        "differ and that is a real difference in what each tool tells a user.",
        "**fclones does not prove equality.** It groups by a 128-bit "
        "non-cryptographic hash (metro, its default) and never compares bytes. "
        "Ours and jdupes both read to confirm. fclones is doing strictly less "
        "work for an answer that looks the same, and its speed should be read "
        "with that in hand.",
        "**jdupes has no native minimum-size flag.** It is given "
        "`-X size+=:N`, an extension filter, which is inclusive of N and was "
        "verified to match `--min N` and `--min-size N` on a planted tree.",
        "**XBENCH.md's premise about caches is out of date.** The spec says "
        "fclones has no persistent cache. fclones 0.35.0 ships `--cache` and "
        "jdupes ships `-y <db>`; lane B enables all three rather than only "
        "ours, which is the fairer contrast the spec was reaching for.",
        "**czkawka is absent.** " + toolkit.czkawka_note().split(": ", 1)[1],
    ]


def stamp_current(base: str) -> bool:
    """Whether this corpus was built by the current xbench/corpora.py."""
    try:
        with open(os.path.join(base, STAMP)) as fh:
            return json.load(fh).get("version") == corpora.XBENCH_GENERATOR_VERSION
    except (OSError, ValueError):
        return False


def write_stamp(base: str) -> None:
    with open(os.path.join(base, STAMP), "w") as fh:
        json.dump({"version": corpora.XBENCH_GENERATOR_VERSION}, fh)


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="xbench/compare.py",
        description="Compare our Rust dedup tool against fclones and jdupes "
                    "on identical generated corpora, interleaved and checked.")
    p.add_argument("--scenario", action="append", metavar="NAME",
                   help="run only this scenario (repeatable)")
    p.add_argument("--tool", action="append", metavar="KEY",
                   help="run only this tool (ours, fclones, jdupes)")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--scale", choices=sorted(corpus.SCALES), default="default")
    p.add_argument("--rounds", type=int, default=MIN_ROUNDS,
                   help="interleaved rounds per tool (minimum {})".format(MIN_ROUNDS))
    p.add_argument("--lane", choices=("A", "B", "both"), default="both")
    p.add_argument("--root", default=DEFAULT_ROOT,
                   help="where corpora live (the RAM disk, or a local scratch "
                        "directory for harness development)")
    p.add_argument("--binary", default=os.path.join(REPO, "rust", "target",
                                                    "release", "dedup-rs"))
    p.add_argument("--allow-busy", action="store_true",
                   help="measure even though another harness is live (never "
                        "for a number anyone will quote)")
    return p


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return _run(args)
    except rbench.Fatal as e:
        print("xbench: {}".format(e), file=sys.stderr)
        return 2


def _run(args) -> int:
    root = rbench.check_root(args.root)
    if args.rounds < MIN_ROUNDS and not args.scenario:
        raise Fatal("--rounds below {} is not a comparison".format(MIN_ROUNDS))
    if not os.path.isfile(args.binary):
        raise Fatal("no dedup-rs at {} — build rust/ at --release first"
                    .format(args.binary))

    # Load discipline, RBENCH's rule and RBENCH's implementation: refuse to
    # measure while anything else on this machine might be scanning, because a
    # second harness does not merely slow this one down.
    busy = rbench.other_harnesses()
    if busy and not args.allow_busy:
        raise Fatal("another harness is live; measuring now would time the "
                    "contention, not the tools:\n  " + "\n  ".join(busy))

    os.makedirs(STATE, exist_ok=True)
    os.makedirs(REPORTS, exist_ok=True)
    if root == DEFAULT_ROOT:
        megabytes, halve = rbench.ram_disk_mb()
        note = rbench.ensure_volume(root, megabytes)
    else:
        # Harness development runs on a local scratch directory. Gates are
        # valid there and timings are not, and the report says so.
        os.makedirs(root, exist_ok=True)
        megabytes, halve, note = 0, False, "local scratch — not a measurement volume"

    lock = rbench.VolumeLock(root).acquire()
    try:
        return _locked(args, root, note, halve, busy)
    finally:
        lock.release()


def _locked(args, root, note, halve, busy) -> int:
    tools = toolkit.discover(args.binary, args.tool)
    if len(tools) < 2:
        raise Fatal("only {} tool(s) found — install fclones and jdupes"
                    .format(len(tools)))

    names = args.scenario or list(corpora.XBENCH_SCENARIOS)
    for name in names:
        if name not in corpus.SCENARIOS:
            raise Fatal("unknown scenario: {}".format(name))
    order = [n for n in corpora.XBENCH_SCENARIOS if n in set(names)]
    order += [n for n in names if n not in set(order)]

    smoke = args.scale == "smoke" or root != DEFAULT_ROOT
    load = os.getloadavg()
    print("xbench: volume {} ({})".format(root, note))
    print("xbench: scale {}, seed {}, rounds {}, lanes {}".format(
        args.scale, args.seed, args.rounds, args.lane))
    print("xbench: tools {}".format(", ".join(t.label for t in tools)))
    print("xbench: load average {:.2f} {:.2f} {:.2f}".format(*load))
    if busy:
        print("xbench: WARNING — another harness is live; these numbers are "
              "not a record")

    workdir = os.path.join(STATE, "work")
    shutil.rmtree(workdir, ignore_errors=True)
    os.makedirs(workdir)

    setup = {}
    for tool in tools:
        setup[tool.key] = setup_seconds(tool, workdir)

    lanes = {"A": {}, "B": {}}
    shapes = {}
    for name in order:
        scale = args.scale
        factor = corpus.SCALES[scale] * (0.5 if halve else 1.0)
        min_size = corpus.SCENARIOS[name][0]
        base = corpus.corpus_dir(root, name, scale, args.seed)
        manifest = corpus.load(root, name, scale, args.seed)
        if manifest is not None and name in corpora.SCENARIOS \
                and not stamp_current(base):
            manifest = None
        if manifest is None or manifest.get("factor") != factor:
            rbench.ensure_space(
                root, int(corpus.estimated_bytes(name, factor) * 1.4), {base})
            started = time.monotonic()
            manifest = corpus.build(root, name, args.seed, scale, factor)
            write_stamp(base)
            print("  built {:<10} {:>7} files  {:>9}  in {:.1f}s".format(
                name, manifest["file_count"], human(manifest["total_bytes"]),
                time.monotonic() - started))
        tree = os.path.join(base, "tree")
        truth = toolkit.truth_as_inodes(tree, manifest)
        shapes[name] = {"file_count": manifest["file_count"],
                        "total_bytes": manifest["total_bytes"],
                        "min_size": min_size,
                        "planted_groups": len(truth)}

        before = rbench.fingerprint(tree)
        for lane in ("A", "B"):
            if args.lane not in (lane, "both"):
                continue
            print("xbench: {} lane {} ...".format(name, lane),
                  end=" ", flush=True)
            entry = measure(name, tree, truth, min_size, tools, workdir,
                            args.rounds, cached=(lane == "B"))
            lanes[lane][name] = entry
            print(" ".join(
                "{}={}".format(t.key,
                               "{:.3f}s".format(entry["tools"][t.key]["median"])
                               if entry["tools"][t.key]["median"] else "fail")
                for t in tools))
        # The corpus must be the same tree at the end of a scenario as at the
        # start. A tool that edited it measured something nobody else did.
        moved = rbench.fingerprint_diff(before, rbench.fingerprint(tree))
        if moved:
            raise Fatal("the corpus moved under {}: {}".format(name, moved))

    results = {
        "meta": {
            "host": platform.node(),
            "os": "{} {}".format(platform.system(), platform.mac_ver()[0]
                                 or platform.release()),
            "cpu": subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"],
                                  capture_output=True, text=True).stdout.strip(),
            "commit": rbench.git("rev-parse", "HEAD").strip()[:12],
            "root": root, "volume_kind": note,
            "scale": args.scale, "seed": args.seed, "rounds": args.rounds,
            "load_average": list(load),
            "other_harnesses_live": busy,
            "halved_for_memory": halve,
            "smoke": smoke,
            "versions": {t.key: t.version() for t in tools},
            "setup": setup,
            "czkawka_note": toolkit.czkawka_note(),
            "started_iso": datetime.now().astimezone().isoformat(timespec="seconds"),
        },
        "order": order,
        "shapes": shapes,
        "lanes": lanes,
    }
    results["deltas"] = semantic_deltas(results)

    for tool in tools:
        share = setup[tool.key]
        slowest = min((e["tools"][tool.key]["median"]
                       for lane in lanes.values() for e in lane.values()
                       if e["tools"][tool.key].get("median")), default=None)
        if slowest and share > slowest * SETUP_SHARE:
            print("xbench: NOTE — {}'s fixed startup ({:.3f}s) is more than "
                  "{:.0%} of its fastest scenario ({:.3f}s); this scale is too "
                  "small to publish".format(tool.label, share, SETUP_SHARE,
                                            slowest))

    stamp = datetime.now().strftime("%Y-%m-%d")
    if smoke:
        stamp += "-smoke"
    md = os.path.join(REPORTS, stamp + ".md")
    js = os.path.join(REPORTS, stamp + ".json")
    write_report(results, tools, md)
    with open(js, "w") as fh:
        json.dump(results, fh, indent=1)

    print("\n{}\n{}".format(md, js))
    if smoke:
        print("\nSMOKE — these numbers are not publishable. They prove the "
              "harness runs, and nothing else.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
