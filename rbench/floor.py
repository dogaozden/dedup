#!/usr/bin/env python3
"""What this machine costs before the algorithm does anything.

A benchmark target is a claim about what is reachable, and a target below what
the hardware charges is not ambitious, it is a honeypot: it can only be hit by
a candidate that stopped doing the work. The predecessor bench set one of those
once. This script exists so that round two's targets come out of arithmetic
instead of out of a feeling about how fast things ought to get.

Four terms, all measured here, none assumed:

  spawn       bare process creation on this machine — fork, exec, exit — and
              nothing else. Every scenario pays it once and nothing can refund
              it.

              Deliberately *not* the tool's own startup, which is measured and
              printed beside it but kept out of the arithmetic. Round one is
              why: the pinned `c238b30` spends about 55 ms before it looks at
              anything, and treating that as floor produces a honeypot of
              0.4286 against a round-one winner that legitimately scored
              0.3173. Its entire win was proving that startup was work rather
              than physics. A floor may only contain what nobody can remove,
              and the gap between these two numbers is by definition removable.

  walk        one lstat per file in the tree. The tool cannot know a file's
              size without asking, and it cannot skip a file it has not seen.
              This is the one term a legitimate optimization can still move:
              `getattrlistbulk` answers for a whole directory at once and the
              journal has not closed that idea. A second figure is printed with
              the walk costed at readdir instead, and a candidate landing
              between the two has probably written it rather than cheated.

  opens       one open/close per inode that has to be read. Only the members of
              planted duplicate groups are counted, because those are the only
              files RGOAL §5.3 requires anybody to open: a candidate is free to
              settle everything else on metadata if it can.

  read        the byte floor divided by measured RAM-disk throughput. The byte
              floor is what proving the planted groups costs — every member of
              every group, end to end, because nothing is a duplicate on the
              strength of a hash.

Summed, that is a lower bound on one scan. It is a soft one in both directions
and the table says which way each term leans:

  * The syscall terms are timed from Python, so each carries an interpreter
    call's overhead that the kernel does not charge. That inflates the floor.
    A bare loop over the same paths is measured and subtracted, which removes
    most of it but not the per-call dispatch. Where a floor is close to a
    measured time, suspect this before suspecting the candidate.
  * Throughput is measured with concurrent whole-file reads, the fastest shape
    this volume offers, so the read term is as small as the hardware allows.
  * Nothing here charges for hashing, comparing, allocating, scheduling, or
    any of the work the tool exists to do.

The baseline wall clock it is all held against is not measured here. It comes
from the last run of `rbench/run.py`, because RGOAL §6.1 says that harness is
the only thing on this machine allowed to time the tool, and a second timer
that disagreed with it would be a second opinion nobody asked for.

    python3 rbench/floor.py                 # at the scored scale
    python3 rbench/floor.py --scale smoke
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import run as harness                                   # noqa: E402
from run import Fatal, Void, human                      # noqa: E402

sys.path.insert(0, os.path.join(harness.REPO, "bench"))
import corpus                                           # noqa: E402

# The read probe: several files, read at once, because that is the shape the
# tool reads in and the fastest this volume goes. One big serial read measures
# a memcpy and would understate what the hardware allows.
#
# Small on purpose, and smaller than feels comfortable. A full volume is this
# script's normal working condition, not an edge case: the corpora are sized to
# leave about 140 MB, so a probe that wanted more than that would refuse to run
# exactly when someone needed it. Evicting a corpus instead is not an option —
# it costs more to rebuild than these numbers are worth, and the next scored run
# would silently pay for it. 48 MB at RAM-disk speed is still milliseconds,
# three orders of magnitude above the clock's resolution, and every figure is
# the fastest of REPEATS passes.
PROBE_FILES = 8
PROBE_BYTES = 6 << 20
PROBE_BUFFER = 1 << 20

# What the probes actually occupy (48 MB of read probe, 16 MB of small files)
# with room to land. Checked before anything is written.
PROBE_SPACE = 80 << 20

# The syscall probe. Enough files that the per-call figure is not a timer
# artefact, few enough that laying them down is instant.
PROBE_SMALL = 4000

# Repeats. These are microbenchmarks on a shared machine; the fastest run is
# the one least interrupted, and that is the one a floor wants.
REPEATS = 5

# Process creation is milliseconds and cheap to repeat, so it gets more tries
# at being uninterrupted than the probes that take real time.
EXEC_REPEATS = 25

PROBE_DIR = ".floor-probe"


# ─────────────────────────────────────────────────────────────────────────────
# the machine's constants
# ─────────────────────────────────────────────────────────────────────────────

def read_throughput(where: str) -> tuple:
    """(bytes per second, how) for the fastest whole-file reads this volume does."""
    paths = []
    block = os.urandom(PROBE_BUFFER)
    for i in range(PROBE_FILES):
        path = os.path.join(where, "read{:02d}.bin".format(i))
        with open(path, "wb") as fh:
            for _ in range(PROBE_BYTES // PROBE_BUFFER):
                fh.write(block)
        paths.append(path)
    total = PROBE_FILES * PROBE_BYTES

    def slurp(path: str) -> int:
        got = 0
        with open(path, "rb", buffering=0) as fh:
            while True:
                chunk = fh.read(PROBE_BUFFER)
                if not chunk:
                    return got
                got += len(chunk)

    def serial_pass() -> None:
        for path in paths:
            slurp(path)

    # The pool is built and warmed outside the timed region. Spinning up eight
    # threads costs about as long as this probe takes to read, and paying for
    # it inside the clock would report the thread pool as the volume's speed.
    with ThreadPoolExecutor(max_workers=PROBE_FILES) as pool:
        list(pool.map(lambda _: None, range(PROBE_FILES)))

        def parallel_pass() -> None:
            list(pool.map(slurp, paths))

        best_serial = min(_elapsed(serial_pass) for _ in range(REPEATS))
        best_parallel = min(_elapsed(parallel_pass) for _ in range(REPEATS))

    serial = total / best_serial
    parallel = total / best_parallel
    if parallel >= serial:
        return parallel, "{} concurrent readers".format(PROBE_FILES)
    return serial, "one reader"


def syscall_costs(where: str) -> dict:
    """Seconds per lstat and per open/close on this volume, Python overhead out.

    Both are timed from Python, so both include an interpreter call the kernel
    never sees. A bare loop over the same list is timed and subtracted, which
    removes the iteration but not the call dispatch — so what comes out is an
    upper bound on the syscall, and the floors built from it lean high.
    """
    root = os.path.join(where, "syscall")
    os.makedirs(root, exist_ok=True)
    paths = []
    for i in range(PROBE_SMALL):
        # Spread over directories, so this is not one implausibly hot dirent
        # cache line answering every question.
        sub = os.path.join(root, "d{:02d}".format(i % 40))
        os.makedirs(sub, exist_ok=True)
        path = os.path.join(sub, "f{:05d}".format(i))
        with open(path, "wb") as fh:
            fh.write(b"x")
        paths.append(path)

    subdirs = [os.path.join(root, d) for d in sorted(os.listdir(root))]

    def loop_bare():
        for path in paths:
            len(path)

    def loop_stat():
        for path in paths:
            os.lstat(path)

    def loop_open():
        for path in paths:
            os.close(os.open(path, os.O_RDONLY))

    def loop_dirent():
        # readdir and nothing else: names out of a directory, no per-file
        # question asked. This is what a bulk directory read gets for free, and
        # it is the floor under the walk that `getattrlistbulk` would move the
        # tool towards. It is optimistic on purpose — a bulk call that also
        # returned sizes would cost somewhat more than this and much less than
        # an lstat apiece.
        for directory in subdirs:
            for entry in os.scandir(directory):
                entry.name

    def best(fn) -> float:
        return min(_elapsed(fn) for _ in range(REPEATS))

    bare = best(loop_bare)
    raw_stat = best(loop_stat)
    raw_open = best(loop_open)
    raw_dirent = best(loop_dirent)
    return {
        "per_stat": max(0.0, raw_stat - bare) / PROBE_SMALL,
        "per_open": max(0.0, raw_open - bare) / PROBE_SMALL,
        "per_dirent": raw_dirent / PROBE_SMALL,
        "raw_stat": raw_stat / PROBE_SMALL,
        "raw_open": raw_open / PROBE_SMALL,
        "python_overhead": bare / PROBE_SMALL,
    }


def human_ms(seconds: float) -> str:
    return "{:.1f} ms".format(seconds * 1000.0)


def exec_floor_seconds() -> float:
    """Fork, exec, exit. What a process costs before it is anybody's program.

    This is the spawn term, rather than the tool's own startup, because a floor
    may only contain what nobody can remove. See the module docstring.
    """
    best = None
    for _ in range(EXEC_REPEATS):
        started = time.perf_counter()
        subprocess.run(["/usr/bin/true"])
        elapsed = time.perf_counter() - started
        best = elapsed if best is None else min(best, elapsed)
    return best


def spawn_seconds(binary: str, empty: str, cache_dir: str) -> float:
    """The tool against an empty directory *on the benchmark volume*.

    run.py's `setup_seconds` does this on the boot disk, which is right for the
    ceiling gate it feeds — that gate only asks whether fixed overhead has run
    away — and wrong here. A tool that asks the system about the volume it was
    pointed at pays a different price for the boot disk than for a RAM disk,
    and the whole difference lands in this one term.

    Measured against the pinned `c238b30`, whose startup shells out to
    `diskutil`, the boot-disk figure was 0.0909 s: larger than that same
    binary's entire measured scan of a 414 MB corpus. A floor above a time that
    was actually observed is wrong by inspection, which is why `_locked`
    checks every row for it before printing a honeypot line.
    """
    shutil.rmtree(empty, ignore_errors=True)
    os.makedirs(empty)
    times = []
    for i in range(REPEATS):
        got = harness.scan(binary, empty, 1, cache_dir=cache_dir,
                           tag="floor-spawn-{}".format(i))
        if not got.get("ok"):
            raise Fatal("the tool could not scan an empty directory on {}: {}"
                        .format(empty, got.get("error", "?")))
        times.append(got["seconds"])
    return min(times)


def _elapsed(fn) -> float:
    started = time.perf_counter()
    fn()
    return time.perf_counter() - started


# ─────────────────────────────────────────────────────────────────────────────
# the arithmetic
# ─────────────────────────────────────────────────────────────────────────────

def scenario_floor(name: str, manifest: dict, ceiling: dict, machine: dict) -> dict:
    """The four terms and their sum, for one scenario at one scale."""
    warm = name in harness.CACHE_ON
    files = manifest["file_count"]
    # One open per inode that has to be read end to end. The warm half of
    # `rescan` is the one measurement whose whole point is reading nothing, so
    # it pays neither the opens nor the bytes.
    opens = 0 if warm else sum(len(g) for g in ceiling["groups"])
    read_bytes = 0 if warm else ceiling["byte_floor"]

    terms = {
        "spawn": machine["spawn"],
        "walk": files * machine["per_stat"],
        "opens": opens * machine["per_open"],
        "read": read_bytes / machine["throughput"],
    }
    terms["total"] = sum(terms.values())
    # The same sum with the walk costed as a bulk directory read instead of an
    # lstat apiece. The tool does not do this yet and RGOAL's round-1 entry
    # still has `getattrlistbulk` open, so this is the floor under the floor:
    # the one term above that a known, legitimate, unwritten optimization can
    # still move.
    terms["bulk_total"] = (terms["total"] - terms["walk"]
                           + files * machine["per_dirent"])
    terms["files"] = files
    terms["opens_counted"] = opens
    terms["floor_bytes"] = read_bytes
    return terms


def render(rows: list) -> str:
    header = ("scenario", "files", "spawn", "walk", "opens", "read", "floor",
              "base s", "floor/base")
    out = [header]
    for name, terms, base in rows:
        out.append((
            name,
            "{:,}".format(terms["files"]),
            "{:.4f}".format(terms["spawn"]),
            "{:.4f}".format(terms["walk"]),
            "{:.4f}".format(terms["opens"]),
            "{:.4f}".format(terms["read"]),
            "{:.4f}".format(terms["total"]),
            "-" if base is None else "{:.4f}".format(base),
            "-" if base is None else "{:.1%}".format(terms["total"] / base),
        ))
    widths = [max(len(r[i]) for r in out) for i in range(len(header))]
    lines = []
    for i, row in enumerate(out):
        lines.append("  ".join(cell.rjust(widths[j]) if j else cell.ljust(widths[j])
                               for j, cell in enumerate(row)).rstrip())
        if i == 0:
            lines.append("─" * len(lines[0]))
    return "\n".join(lines)


def recorded_run(scale: str, column: str) -> tuple:
    """(medians, spawn spec, provenance) from the last run of rbench/run.py.

    `column` picks which side of that run to hold the floor against, because a
    floor is only meaningful against the wall clock a round will actually be
    scored on. Normally that is the pinned baseline. Between rounds, while the
    pin still names the commit the last round started from, it is whichever
    side of the last run was built from the commit the pin is about to move to.

    The spawn spec rides along because the spawn term and the wall clock have
    to come out of the same binary. Measuring one side's startup against the
    other side's clock is how you get a floor above a time that was actually
    observed — which is what this returns the means to avoid.
    """
    path = os.path.join(harness.STATE, "results.json")
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        raise Fatal("no run recorded at {}. Run rbench/run.py first: the wall "
                    "clock a floor is held against has to come from the "
                    "harness, not from here.".format(path))
    meta = data.get("meta", {})
    if meta.get("scale") != scale:
        raise Fatal("the last run was at scale {}, this floor is for {}. Run "
                    "rbench/run.py --scale {} first."
                    .format(meta.get("scale"), scale, scale))
    seconds = {name: entry[column + "_seconds"]
               for name, entry in data.get("scenarios", {}).items()}

    # The three shapes resolve_side records an identity in: a bare sha, a
    # `worktree <sha>` and a `path:<dir>`. A calibration run is a fourth: both
    # its sides are the pinned baseline, so asking for its candidate is asking
    # for the baseline.
    identity = str(meta.get("baseline_sha" if column == "baseline"
                            else "candidate", ""))
    if identity.startswith("baseline (calibration)"):
        identity = str(meta.get("baseline_sha", ""))
    if identity.endswith("+dirty"):
        raise Fatal("the last run's candidate was a dirty worktree, so there "
                    "is no commit to rebuild it from. Commit it, re-run "
                    "rbench/run.py, and try again.")
    if identity.startswith("path:"):
        spec = identity[len("path:"):]
    elif identity:
        spec = identity.split()[-1]
    else:
        raise Fatal("the last run recorded no {} identity".format(column))
    return seconds, spec, "{} of the run at {} — {}".format(
        column, meta.get("started_iso", "?"), identity)


# ─────────────────────────────────────────────────────────────────────────────
# the run
# ─────────────────────────────────────────────────────────────────────────────

def main(argv=None) -> int:
    p = argparse.ArgumentParser(
        prog="rbench/floor.py",
        description="What the machine charges before the algorithm starts. "
                    "Prints the arithmetic a round-2 target has to clear.")
    p.add_argument("--scale", choices=sorted(corpus.SCALES), default="r2")
    p.add_argument("--against", choices=("baseline", "candidate"),
                   default="baseline",
                   help="which side of the last run to hold the floor against "
                        "(default: baseline; use candidate while the pin is "
                        "still naming the previous round's commit)")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--root", default=harness.DEFAULT_ROOT)
    p.add_argument("--allow-busy", action="store_true")
    args = p.parse_args(argv)
    try:
        return _run(args)
    except Fatal as e:
        print("floor: {}".format(e), file=sys.stderr)
        return 2
    except Void as e:
        print("floor: {}".format(e), file=sys.stderr)
        return 2


def _run(args) -> int:
    root = harness.check_root(args.root)
    busy = harness.other_harnesses()
    if busy and not args.allow_busy:
        raise Fatal("another harness is live; every number below would be "
                    "contention:\n  " + "\n  ".join(busy))

    os.makedirs(harness.STATE, exist_ok=True)
    megabytes, halve = harness.ram_disk_mb()
    harness.ensure_volume(root, megabytes)

    lock = harness.VolumeLock(root).acquire()
    try:
        return _locked(args, root, halve)
    finally:
        lock.release()


def _locked(args, root, halve) -> int:
    # The oracle is always the pinned commit's, whatever the floor is held
    # against: it defines the ceiling stored beside every corpus, and nothing
    # here is entitled to a second opinion about that.
    sha = harness.baseline_sha()
    oracle_sha = harness.git("rev-parse", sha).strip()
    oracle_py = os.path.join(harness.STATE, "oracle-{}.py".format(oracle_sha[:12]))
    if not os.path.exists(oracle_py):
        with open(oracle_py, "w") as fh:
            fh.write(harness.git("show", "{}:dedup.py".format(oracle_sha)))

    baseline, spec, provenance = recorded_run(args.scale, args.against)
    spawn_id, binary = harness.resolve_side(spec, "the {} side".format(args.against))

    probe = os.path.join(root, "corpora", PROBE_DIR)
    shutil.rmtree(probe, ignore_errors=True)
    free = shutil.disk_usage(root).free
    if free < PROBE_SPACE:
        # Deliberately not ensure_space: evicting a corpus to measure the
        # machine would cost more to rebuild than these numbers are worth, and
        # the next scored run would silently pay for it.
        raise Fatal("{} free on {}, and the probes need {}. Free some room "
                    "under corpora/ and run this again."
                    .format(human(free), root, human(PROBE_SPACE)))
    os.makedirs(probe)
    try:
        print("floor: measuring the machine ...", flush=True)
        spawn = exec_floor_seconds()
        startup = spawn_seconds(binary, os.path.join(probe, "empty"),
                                os.path.join(harness.STATE, "cache", "floor"))
        throughput, how = read_throughput(probe)
        costs = syscall_costs(probe)
    finally:
        shutil.rmtree(probe, ignore_errors=True)

    machine = dict(costs, spawn=spawn, throughput=throughput)
    load = os.getloadavg()

    print("\nfloor: volume {}, scale {}, seed {}".format(root, args.scale, args.seed))
    print("floor: oracle pinned at {}; wall clock and spawn both from {}"
          .format(sha[:12], spawn_id))
    print("floor: load average {:.2f} {:.2f} {:.2f}".format(*load))
    print()
    print("  process creation, fork/exec/exit       {:.4f} s   (the floor term)"
          .format(spawn))
    print("  this binary, empty dir on the volume   {:.4f} s   ({:.0f}× that)"
          .format(startup, startup / spawn if spawn else 0))
    if startup > 2 * spawn:
        print("      {} of that is work this binary chooses to do before it "
              "looks at\n      anything, and work is removable — round one "
              "removed 55 ms of it. It is\n      reported, not counted."
              .format(human_ms(startup - spawn)))
    print("  RAM-disk read, {:<22}  {:.2f} GB/s".format(how, throughput / 1e9))
    print("  lstat                                  {:.2f} µs   "
          "({:.2f} µs raw, {:.2f} µs of that is Python)".format(
              costs["per_stat"] * 1e6, costs["raw_stat"] * 1e6,
              costs["python_overhead"] * 1e6))
    print("  open + close                           {:.2f} µs   "
          "({:.2f} µs raw)".format(
              costs["per_open"] * 1e6, costs["raw_open"] * 1e6))
    print("  readdir, per entry                     {:.2f} µs   "
          "(what a bulk walk would approach)".format(costs["per_dirent"] * 1e6))

    print("  wall clock held against                {}".format(provenance))

    rows = []
    for name in harness.ORDER:
        scale = harness.GATE_SCALE.get(name, args.scale)
        factor = corpus.scale_factor(name, scale) * (0.5 if halve else 1.0)
        _base, manifest, ceiling = harness.ensure_corpus(
            root, name, scale, args.seed, factor, oracle_py, oracle_sha)
        rows.append((name, scenario_floor(name, manifest, ceiling, machine),
                     baseline.get(name)))

    print()
    print(render(rows))

    scored = [(terms, base) for name, terms, base in rows
              if name in harness.SCORE_SCENARIOS and base]
    print()
    if len(scored) != len(harness.SCORE_SCENARIOS):
        print("No honeypot line: {} of {} scored scenarios have a baseline "
              "recorded at this scale. Run rbench/run.py first."
              .format(len(scored), len(harness.SCORE_SCENARIOS)))
        return 0

    # A lower bound above a time that was actually observed is wrong by
    # inspection, and it is the one error in here that cannot be argued with.
    # It is worth failing loudly on, because the number this script exists to
    # print is one that other people will quote at each other.
    impossible = [(name, terms, base) for name, terms, base in rows
                  if base and terms["total"] > base]
    if impossible:
        print("NO HONEYPOT LINE — this floor is wrong and must not be quoted.")
        for name, terms, base in impossible:
            print("  {} floors at {:.4f} s and was observed taking {:.4f} s."
                  .format(name, terms["total"], base))
        print("  A lower bound above a measured time is broken. The usual "
              "cause is a spawn\n  term measured on a different binary from "
              "the wall clock — check that the\n  header line above names one "
              "commit and not two.")
        return 1

    ratio = harness.geomean(terms["total"] / base for terms, base in scored)
    bulk = harness.geomean(terms["bulk_total"] / base for terms, base in scored)
    worst = max(scored, key=lambda tb: tb[0]["total"] / tb[1])
    print("HONEYPOT: targets below ≈{:.4f} are not reachable honestly."
          .format(ratio))
    print("  Geomean of floor/baseline over the seven scored scenarios. A "
          "candidate\n  claiming less than this did not get faster — it stopped "
          "doing something\n  the answer depends on, or it is not reporting "
          "what it did.")
    print("  Worst single scenario: {:.1%} of its baseline is already floor."
          .format(worst[0]["total"] / worst[1]))
    print()
    print("  ≈{:.4f} if the walk became a bulk directory read.".format(bulk))
    print("  The walk above is costed at one lstat per file, which is what the "
          "tool\n  does today and not what the kernel charges at best. "
          "`getattrlistbulk` is\n  still an open idea in the journal, and a "
          "candidate that lands between\n  these two numbers has most likely "
          "done it rather than cheated. Read the\n  diff before believing "
          "either reading.")
    print()
    print("  Every term leans optimistic where it is not measured — nothing "
          "here charges\n  for hashing, comparing or scheduling, and the byte "
          "floor only counts the\n  planted groups. The spawn and syscall "
          "terms lean the other way, since both\n  are timed through Python. "
          "Treat both figures as soft bounds.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
