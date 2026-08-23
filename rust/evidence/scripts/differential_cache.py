#!/usr/bin/env python3
"""Cache differential.

The referee's 5% warm ceiling is a property of the `mixed` tree, not of the
cache: the oracle only remembers a whole-file digest for a file that turned out
to have a duplicate, and a size class is only settled from memory when EVERY
member carries one. A tree with a unique file in a size class is therefore
re-read by the oracle too. So the criterion here is not "warm reads little", it
is "the port warms exactly as much as the oracle does, and the two can read
each other's cache files".
"""
import json, os, shutil, subprocess, sys, time
S = "/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/diff"
DRIVER = "/Users/dogaozden/ox/dedup-oracle/conformance/oracle_driver.py"
RUST = "/Users/dogaozden/AI_Projects/dedup-rs/target/release/dedup-rs"
FIELDS = ("groups", "files_seen", "bytes_seen", "unreadable")
TREES = ["boundaries", "sameends", "nested", "links2"]

def scan(which, root, cache):
    env = dict(os.environ); env["DEDUP_CACHE_DIR"] = cache
    out = os.path.join(S, "x.json")
    cmd = [sys.executable, DRIVER, root, "--report-json", out, "--min-size", "1"] \
          if which == "oracle" else [RUST, root, "--report-json", out, "--min-size", "1"]
    subprocess.run(cmd, capture_output=True, env=env)
    return json.load(open(out))

def tick():
    start = int(time.time())
    while int(time.time()) == start:
        time.sleep(0.02)

def pair(root, writer, reader, tag):
    cache = os.path.join(S, "cache-{}".format(tag))
    shutil.rmtree(cache, ignore_errors=True)
    tick()
    return scan(writer, root, cache), scan(reader, root, cache)

failures = checks = 0
print("{:<12} {:>10} {:>10} {:>10} {:>10}   verdict".format(
    "tree", "cold", "warm o->o", "warm o->r", "warm r->r"))
for name in TREES:
    root = os.path.join(S, name)
    oo_cold, oo_warm = pair(root, "oracle", "oracle", name + "-oo")
    orc, or_warm = pair(root, "oracle", "rust", name + "-or")
    rr_cold, rr_warm = pair(root, "rust", "rust", name + "-rr")
    rrc2, ro_warm = pair(root, "rust", "oracle", name + "-ro")
    checks += 1
    problems = []
    for field in FIELDS:
        if len({json.dumps(r[field], sort_keys=True) for r in
                (oo_cold, oo_warm, or_warm, rr_cold, rr_warm, ro_warm)}) != 1:
            problems.append(field)
    if rr_cold["bytes_read"] != oo_cold["bytes_read"]:
        problems.append("cold bytes")
    if or_warm["bytes_read"] != oo_warm["bytes_read"]:
        problems.append("port did not reuse the oracle's cache identically")
    if ro_warm["bytes_read"] != rr_warm["bytes_read"]:
        problems.append("oracle did not reuse the port's cache identically")
    if rr_warm["bytes_read"] != oo_warm["bytes_read"]:
        problems.append("warm bytes")
    failures += bool(problems)
    print("{:<12} {:>10} {:>10} {:>10} {:>10}   {}".format(
        name, oo_cold["bytes_read"], oo_warm["bytes_read"], or_warm["bytes_read"],
        rr_warm["bytes_read"], "; ".join(problems) or "identical, and interoperable"))
print("\n{} tree(s), {} differing".format(checks, failures))
sys.exit(1 if failures else 0)
