#!/usr/bin/env python3
"""Alternated timed runs, oracle against port. No hyperfine on this machine."""
import os, statistics, subprocess, sys, time
SCRATCH = "/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad"
ROOT = sys.argv[1]
JOBS = sys.argv[2]
RUNS = int(sys.argv[3]) if len(sys.argv) > 3 else 6
ORACLE = ["python3", "/Users/dogaozden/ox/dedup-oracle/dedup.py", ROOT,
          "--min-size", "1", "--no-cache", "-j", JOBS, "-q", "--list"]
RUST = ["/Users/dogaozden/AI_Projects/dedup-rs/target/release/dedup-rs", ROOT,
        "--min-size", "1", "--no-cache", "-j", JOBS, "--list",
        "--report-json", os.path.join(SCRATCH, "bench-report.json")]
env = dict(os.environ); env["DEDUP_CACHE_DIR"] = os.path.join(SCRATCH, "bench-cache")

def once(cmd):
    start = time.monotonic()
    proc = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    elapsed = time.monotonic() - start
    if proc.returncode != 0:
        sys.exit("{} exited {}".format(cmd[0], proc.returncode))
    return elapsed

print("warming the page cache (untimed)")
once(ORACLE); once(RUST)
oracle, rust = [], []
for run in range(RUNS):
    o = once(ORACLE); r = once(RUST)
    oracle.append(o); rust.append(r)
    print("  run {}  oracle {:7.3f}s   dedup-rs {:7.3f}s   {:.2f}x".format(run + 1, o, r, o / r))
mo, mr = statistics.median(oracle), statistics.median(rust)
print("\n-j {}  median over {} runs: oracle {:.3f}s, dedup-rs {:.3f}s -> {:.2f}x".format(
    JOBS, RUNS, mo, mr, mo / mr))
