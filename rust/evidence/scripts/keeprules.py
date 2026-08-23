#!/usr/bin/env python3
"""REVIEW-M4 items 2 and 3: which copy each keep rule leaves behind.

Identical trees, one per implementation, --hard so nothing reaches a Trash.
"""
import os, shutil, subprocess, sys
S = "/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/keep"
RUST = "/Users/dogaozden/AI_Projects/dedup-rs/target/release/dedup-rs"
DEDUP = "/Users/dogaozden/ox/dedup-oracle/dedup.py"
BODY = b"identical content in every copy"
BASE = 1_700_000_000

CASES = {
    "all mtimes equal":        {"aaa.bin": BASE, "bb.bin": BASE, "c.bin": BASE},
    "two tied newest":         {"aaa.bin": BASE + 5, "bb.bin": BASE + 5, "c.bin": BASE},
    "sub-microsecond oldest":  {"aaa.bin": BASE * 10**9 + 0, "bb.bin": BASE * 10**9 + 1,
                                "c.bin": (BASE + 8) * 10**9},
    "plainly ordered":         {"aaa.bin": BASE + 1, "bb.bin": BASE + 2, "c.bin": BASE + 3},
}

def build(root, times):
    shutil.rmtree(root, ignore_errors=True); os.makedirs(root)
    for name, when in times.items():
        path = os.path.join(root, name)
        with open(path, "wb") as fh:
            fh.write(BODY)
        ns = when if when > 10**12 else when * 10**9
        os.utime(path, ns=(ns, ns))

failures = 0
print("{:<24} {:<10} {:<14} {:<14} {}".format("case", "rule", "oracle keeps", "dedup-rs keeps", ""))
for label, times in CASES.items():
    for rule in ("shortest", "oldest", "newest"):
        kept = {}
        for who in ("oracle", "rust"):
            root = os.path.join(S, who)
            build(root, times)
            env = dict(os.environ)
            env["DEDUP_CACHE_DIR"] = os.path.join(S, "cache")
            env["HOME"] = S
            flags = ["--min-size", "1", "--no-cache", "--auto", rule, "-y", "--hard"]
            cmd = [sys.executable, DEDUP, root] + flags + ["-q"] if who == "oracle" \
                  else [RUST, root, "--report-json", os.path.join(S, "r.json")] + flags
            subprocess.run(cmd, capture_output=True, env=env)
            kept[who] = sorted(os.listdir(root))
        same = kept["oracle"] == kept["rust"]
        failures += not same
        print("{:<24} {:<10} {:<14} {:<14} {}".format(
            label, rule, ",".join(kept["oracle"]), ",".join(kept["rust"]),
            "same" if same else "DIFFER"))
print("\n{} rule check(s) differing".format(failures))
sys.exit(1 if failures else 0)
