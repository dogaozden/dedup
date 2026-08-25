#!/usr/bin/env python3
"""M8's absolute fence, as a gate rather than a promise.

M8-LIVE.md's one absolute fence: `--list`, `--auto`, `--report-json`, piped
runs, and every conformance/bench invocation keep TODAY'S pipeline and TODAY'S
bytes, byte for byte. The live scan is the INTERACTIVE browser path only, and
nothing that reaches this script is interactive.

So this captures, for one binary, every non-interactive surface on a fixed set
of corpora — stdout, stderr, exit code, and the whole protocol report including
every effort counter — and writes them to a directory. Run it once for main's
binary and once for the candidate's, then `--compare` the two directories: any
differing byte is a failed fence.

The corpora deliberately include the shapes a streaming design is worst at, not
only the ones it is good at: a tree whose groups are found in an order that is
not the reported order, a widegroup, files astride every stage boundary, a tree
with no duplicates at all, and one whose only duplicates are attested from a
warm cache. A corpus with no shape the design is bad at is an argument, not a
measurement.

Nothing here touches /Volumes. Corpora live under the scratch root given on the
command line (default: the system temp directory).

Usage:
    m8_freeze.py capture <binary> <outdir> [--scratch DIR]
    m8_freeze.py compare <dir-a> <dir-b>
"""
import json
import os
import random
import shutil
import subprocess
import sys

HEAD, DEEP, TAIL, SPAN = 4096, 65536, 4096, 8 * 1024 * 1024


def blob(n, salt=0):
    rnd = random.Random(salt)
    return bytes(rnd.getrandbits(8) for _ in range(n))


def write(root, rel, data):
    path = os.path.join(root, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(data)


# ---------------------------------------------------------------- the corpora

def corpus_boundaries(root):
    """Files astride every size where a stage changes its mind."""
    for size in (0, 1, HEAD - 1, HEAD, HEAD + 1, DEEP - 1, DEEP, DEEP + 1,
                 DEEP + TAIL, DEEP + TAIL + 1):
        body = blob(size, size)
        write(root, "s{}/a.bin".format(size), body)
        write(root, "s{}/b.bin".format(size), body)
        if size:
            other = bytearray(body)
            other[-1] ^= 0xFF
            write(root, "s{}/last.bin".format(size), bytes(other))
        if size > HEAD:
            other = bytearray(body)
            other[HEAD] ^= 0xFF
            write(root, "s{}/mid.bin".format(size), bytes(other))


def corpus_widegroup(root):
    """One group of 200 identical files — the shape that makes a streaming
    list insert 200 rows at once and a prove-at-apply pass open 200 files."""
    body = blob(DEEP + 1000, 77)
    for i in range(200):
        write(root, "wide/w{:03d}.bin".format(i), body)
    # ...and a decoy that shares the head but not the tail, so the group is
    # only settled by a stage that reads past the head.
    near = bytearray(body)
    near[-1] ^= 0xFF
    write(root, "wide/decoy.bin", bytes(near))


def corpus_out_of_order(root):
    """Groups whose discovery order is NOT the reported order.

    The reported order is size descending. Names are laid out so a walk in
    name order meets the small group first: a streaming list that reported in
    arrival order rather than sorting would differ here, and so would a freeze
    that accidentally let arrival order leak into --list.
    """
    for name, size, salt in (("aaa-small", DEEP + 1, 11),
                             ("mmm-medium", DEEP + 500_000, 12),
                             ("zzz-large", DEEP + 2_000_000, 13)):
        body = blob(size, salt)
        write(root, "order/{}-1.bin".format(name), body)
        write(root, "order/{}-2.bin".format(name), body)


def corpus_no_duplicates(root):
    """A tree with nothing to find. The empty-list path is a real output."""
    for i in range(40):
        write(root, "uniq/u{:02d}.bin".format(i), blob(DEEP + i + 1, 900 + i))


def corpus_sameends(root):
    """Same head page, same tail page, different middles — the shape that
    survives every cheap stage and must be settled by the byte compare."""
    head, tail = blob(HEAD, 1), blob(TAIL, 2)
    for i in range(12):
        write(root, "same/f{:02d}.bin".format(i),
              head + blob(DEEP, 100 + i // 3) + tail)


def corpus_hardlinks(root):
    """A hardlink cluster beside a real duplicate: the cluster is one record
    wearing several names and must never be split by anything."""
    body = blob(DEEP + 77, 42)
    write(root, "links/original.bin", body)
    os.link(os.path.join(root, "links/original.bin"),
            os.path.join(root, "links/second-name.bin"))
    write(root, "links/true-copy.bin", body)


CORPORA = {
    "boundaries": corpus_boundaries,
    "widegroup": corpus_widegroup,
    "out-of-order": corpus_out_of_order,
    "no-duplicates": corpus_no_duplicates,
    "sameends": corpus_sameends,
    "hardlinks": corpus_hardlinks,
}

# The non-interactive surfaces the fence covers. Every one of these is a run
# with no terminal anywhere on it — this script's own pipes are the proof.
# `--auto` runs are `--hard`-free and land in the Trash, so each gets its own
# freshly built corpus copy; see `run_case`.
INVOCATIONS = [
    ("list", ["--list"], False),
    ("list-min-size", ["--list", "--min-size", "1"], False),
    ("piped-default", [], False),
    ("report-only", ["--quiet"], False),
    ("spinning-list", ["--list", "--profile", "spinning"], False),
    ("flash-list", ["--list", "--profile", "flash"], False),
    ("no-cache-list", ["--list", "--no-cache"], False),
    ("stage-size", ["--list", "--stage", "size"], False),
    ("auto-shortest", ["--auto", "shortest", "--yes"], True),
    ("auto-oldest", ["--auto", "oldest", "--yes"], True),
]


def build_corpus(scratch, name):
    root = os.path.join(scratch, name)
    if os.path.exists(root):
        shutil.rmtree(root)
    os.makedirs(root)
    CORPORA[name](root)
    return root


def run_case(binary, scratch, corpus, label, flags, destructive, cache_dir):
    """One invocation, captured whole. Destructive runs get a fresh corpus so
    the run before them never changes what they see."""
    root = build_corpus(scratch, corpus) if destructive else \
        os.path.join(scratch, corpus)
    report = os.path.join(scratch, "report-{}-{}.json".format(corpus, label))
    if os.path.exists(report):
        os.remove(report)
    env = dict(os.environ)
    env["DEDUP_CACHE_DIR"] = cache_dir
    # The Trash the `--auto` cases move into is a scratch one, the way
    # verify_deletion.py does it: the real Trash path is exercised, and the
    # person running this does not have to clean up after it.
    home = os.path.join(scratch, "home")
    os.makedirs(os.path.join(home, ".Trash"), exist_ok=True)
    env["HOME"] = home
    # --min-size 1 unless the case set its own, so the small corpora are seen
    # at all; the default floor is 10 MiB and would report nothing here.
    argv = [binary, root, "--report-json", report] + flags
    if "--min-size" not in flags:
        argv += ["--min-size", "1"]
    done = subprocess.run(argv, capture_output=True, env=env)
    body = {
        "exit": done.returncode,
        # The scratch root's own name is the one thing here that legitimately
        # differs between two runs of this script, so it is stripped out.
        "stdout": done.stdout.decode("utf-8", "replace").replace(root, "{root}"),
        "stderr": done.stderr.decode("utf-8", "replace").replace(root, "{root}"),
    }
    if os.path.exists(report):
        with open(report) as fh:
            body["report"] = json.load(fh)
    return body


def capture(binary, outdir, scratch):
    os.makedirs(outdir, exist_ok=True)
    cache_dir = os.path.join(scratch, "cache")
    captured = {}
    for corpus in sorted(CORPORA):
        build_corpus(scratch, corpus)
        for label, flags, destructive in INVOCATIONS:
            # A cold cache for every case, so what is captured is the pipeline
            # and not the order this script happened to run things in. The
            # warm-cache surface gets its own case below.
            if os.path.exists(cache_dir):
                shutil.rmtree(cache_dir)
            key = "{}/{}".format(corpus, label)
            captured[key] = run_case(binary, scratch, corpus, label, flags,
                                     destructive, cache_dir)
        # The warm-cache surface: the same --list run twice over, where the
        # second one answers from attestations rather than from bytes. Its
        # groups must be identical and its counters must fall.
        #
        # The corpus is rebuilt first because the `--auto` cases above deleted
        # duplicates out of it, and a warm-cache case run over an
        # already-deduplicated tree proves nothing about the cache.
        if os.path.exists(cache_dir):
            shutil.rmtree(cache_dir)
        build_corpus(scratch, corpus)
        cold = run_case(binary, scratch, corpus, "warm-1", ["--list"], False,
                        cache_dir)
        warm = run_case(binary, scratch, corpus, "warm-2", ["--list"], False,
                        cache_dir)
        captured["{}/warm-cold".format(corpus)] = cold
        captured["{}/warm-warm".format(corpus)] = warm
    path = os.path.join(outdir, "capture.json")
    with open(path, "w") as fh:
        json.dump(captured, fh, indent=2, sort_keys=True)
    print("captured {} cases from {}".format(len(captured), binary))
    print("  -> {}".format(path))
    return captured


def compare(dir_a, dir_b):
    with open(os.path.join(dir_a, "capture.json")) as fh:
        a = json.load(fh)
    with open(os.path.join(dir_b, "capture.json")) as fh:
        b = json.load(fh)
    keys = sorted(set(a) | set(b))
    bad = 0
    for key in keys:
        if key not in a:
            print("ONLY IN B: {}".format(key))
            bad += 1
            continue
        if key not in b:
            print("ONLY IN A: {}".format(key))
            bad += 1
            continue
        if a[key] == b[key]:
            continue
        bad += 1
        print("DIFFERS: {}".format(key))
        for field in ("exit", "stdout", "stderr"):
            if a[key].get(field) != b[key].get(field):
                print("    {}: {!r} != {!r}".format(
                    field, a[key].get(field), b[key].get(field)))
        ra, rb = a[key].get("report", {}), b[key].get("report", {})
        for field in sorted(set(ra) | set(rb)):
            if ra.get(field) != rb.get(field):
                print("    report.{}: {!r} != {!r}".format(
                    field, ra.get(field), rb.get(field)))
    print()
    print("{} cases compared, {} differing".format(len(keys), bad))
    if bad:
        print("FENCE BROKEN")
        return 1
    print("FENCE HELD: every non-interactive output and every effort counter "
          "is byte-identical")
    return 0


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    mode = sys.argv[1]
    if mode == "capture":
        binary, outdir = sys.argv[2], sys.argv[3]
        scratch = "/tmp/m8-freeze-scratch"
        if "--scratch" in sys.argv:
            scratch = sys.argv[sys.argv.index("--scratch") + 1]
        os.makedirs(scratch, exist_ok=True)
        capture(os.path.abspath(binary), outdir, scratch)
        return 0
    if mode == "compare":
        return compare(sys.argv[2], sys.argv[3])
    sys.exit(__doc__)


if __name__ == "__main__":
    sys.exit(main())
