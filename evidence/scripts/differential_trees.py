#!/usr/bin/env python3
"""Differential test: oracle against port on trees the conformance corpus does
not contain. Compares the whole protocol report, field for field.

REVIEW-M1 made the point that the gate is a floor, not a ceiling. Trees are
built under a scratch directory given on the command line; nothing is written
anywhere else.
"""
import json, os, random, shutil, subprocess, sys
S = sys.argv[1] if len(sys.argv) > 1 else "/tmp/dedup-rs-differential"
DRIVER = "/Users/dogaozden/ox/dedup-oracle/conformance/oracle_driver.py"
RUST = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))), "target/release/dedup-rs")
HEAD, DEEP, TAIL, SPAN = 4096, 65536, 4096, 8 * 1024 * 1024

def blob(n, salt=0):
    rnd = random.Random(salt)
    return bytes(rnd.getrandbits(8) for _ in range(n))

def write(root, rel, data):
    path = os.path.join(root, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(data)

def tree_boundaries(root):
    """Every size where a stage changes its mind."""
    for size in (0, 1, HEAD - 1, HEAD, HEAD + 1, DEEP - 1, DEEP, DEEP + 1,
                 DEEP + TAIL, DEEP + TAIL + 1):
        body = blob(size, size)
        write(root, "s{}/a.bin".format(size), body)
        write(root, "s{}/b.bin".format(size), body)
        if size:
            other = bytearray(body); other[-1] ^= 0xFF
            write(root, "s{}/last.bin".format(size), bytes(other))
        if size > HEAD:
            other = bytearray(body); other[HEAD] ^= 0xFF
            write(root, "s{}/mid.bin".format(size), bytes(other))

def tree_sampling(root):
    """Astride the sampling threshold, where stage 4 turns on."""
    for size in (SPAN, SPAN + 1, SPAN + 4096):
        body = blob(size, size)
        write(root, "big/{}-a.iso".format(size), body)
        write(root, "big/{}-b.iso".format(size), body)
        near = bytearray(body); near[size // 2] ^= 0xFF
        write(root, "big/{}-mid.iso".format(size), bytes(near))
        tailed = bytearray(body); tailed[-1] ^= 0xFF
        write(root, "big/{}-tail.iso".format(size), bytes(tailed))

def tree_sameends(root):
    """Same head page, same tail page, different middles."""
    head, tail = blob(HEAD, 1), blob(TAIL, 2)
    for i in range(12):
        write(root, "same/f{:02d}.bin".format(i), head + blob(DEEP, 100 + i // 3) + tail)

def tree_nested(root):
    """Fifty levels deep, with duplicates at the bottom."""
    body = blob(9000, 5)
    deep = "/".join("d{:02d}".format(i) for i in range(50))
    write(root, deep + "/a.bin", body)
    write(root, "b.bin", body)
    write(root, deep + "/c.bin", blob(9000, 6))

def tree_links(root):
    """Hardlink clusters of several shapes, including one pinned outside."""
    body = blob(20000, 7)
    write(root, "cluster/a.bin", body)
    os.link(os.path.join(root, "cluster/a.bin"), os.path.join(root, "cluster/b.bin"))
    os.link(os.path.join(root, "cluster/a.bin"), os.path.join(root, "z.bin"))
    write(root, "twin.bin", body)
    write(root, "lone/x.bin", blob(20000, 8))
    os.link(os.path.join(root, "lone/x.bin"), os.path.join(root, "lone/y.bin"))
    write(root, "pinned.bin", blob(20000, 9))
    outside = os.path.join(S, "pinned-outside.bin")
    if os.path.exists(outside):
        os.unlink(outside)
    os.link(os.path.join(root, "pinned.bin"), outside)
    write(root, "pinned-twin.bin", blob(20000, 9))

def tree_unicode(root):
    """Names the byte-length shortcut would order differently from Python."""
    body = blob(5000, 12)
    for name in ("\U0001f642.bin", "café-résumé.bin", "漢字.bin", "a b'c\"d.bin", "plain.bin"):
        write(root, name, body)

TREES = {"boundaries": tree_boundaries, "sampling": tree_sampling,
         "sameends": tree_sameends, "nested": tree_nested,
         "links2": tree_links, "unicode": tree_unicode}
FLAGS = [["--min-size", "0"], ["--min-size", "1"], ["--min-size", "1", "-j", "1"],
         ["--min-size", "1", "-j", "4"], ["--min-size", "4096"],
         ["--min-size", "1", "--ext", "bin"], ["--min-size", "1", "-x", "d0*"]]
FIELDS = ("groups", "files_seen", "bytes_seen", "unreadable", "bytes_read", "opens")

shutil.rmtree(S, ignore_errors=True)
os.makedirs(S)
failures = checks = 0
for name, build in TREES.items():
    root = os.path.join(S, name)
    os.makedirs(root)
    build(root)
    for flags in FLAGS:
        flags = flags + ["--no-cache"]
        env = dict(os.environ); env["DEDUP_CACHE_DIR"] = os.path.join(S, "cache")
        oj, rj = os.path.join(S, "o.json"), os.path.join(S, "r.json")
        subprocess.run([sys.executable, DRIVER, root, "--report-json", oj] + flags,
                       capture_output=True, env=env)
        subprocess.run([RUST, root, "--report-json", rj] + flags, capture_output=True, env=env)
        o, r = json.load(open(oj)), json.load(open(rj))
        checks += 1
        bad = [f for f in FIELDS
               if (sorted(o[f]) if f == "unreadable" else o[f]) !=
                  (sorted(r[f]) if f == "unreadable" else r[f])]
        label = "{} {}".format(name, " ".join(flags[:-1]))
        if bad:
            failures += 1
            print("  {:<34} DIFFERS on {}".format(label, ", ".join(bad)))
            for f in bad:
                print("      oracle {} = {}".format(f, o[f]))
                print("      rust   {} = {}".format(f, r[f]))
        else:
            print("  {:<34} same ({} groups, {} bytes, {} opens)".format(
                label, len(o["groups"]), o["bytes_read"], o["opens"]))
print("\n{} differential check(s), {} differing".format(checks, failures))
sys.exit(1 if failures else 0)
