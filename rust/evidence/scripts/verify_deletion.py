#!/usr/bin/env python3
"""Independent M4 deletion check on scratch trees, mirroring the referee's
gate_deletion — minus gate_trashes, which needs a second volume.

Reads conformance/trees.py and run.py's rules; writes only under the scratchpad.
"""
import os, shutil, subprocess, sys
HERE = "/Users/dogaozden/ox/dedup-oracle/conformance"
sys.path.insert(0, HERE)
import trees
S = "/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/m4"
SEED = 20260822
RUST = "/Users/dogaozden/AI_Projects/dedup-rs/target/release/dedup-rs"
DEDUP = "/Users/dogaozden/ox/dedup-oracle/dedup.py"

def snapshot(root):
    out = set()
    for dirpath, _, filenames in os.walk(root):
        for fn in filenames:
            out.add(os.path.relpath(os.path.join(dirpath, fn), root))
    return out

def doomed_names(key):
    """Who lives and who dies, from the tree's construction record — not from
    either implementation. Same rule as run.py:533."""
    doomed = set()
    for group in key["detail"]:
        members = sorted(group["members"], key=lambda m: (len(m["name"]), m["name"]))
        for member in members[1:]:
            if member["deletable"]:
                doomed.update(member["names"])
    return doomed

failures = []
for tree in ("deletion", "links"):
    for who in ("oracle", "candidate"):
        base = os.path.join(S, "{}-{}".format(tree, who))
        trees.unlock(base); shutil.rmtree(base, ignore_errors=True)
        manifest = trees.build(base, tree, SEED)
        root = os.path.join(trees.tree_dir(base, tree, SEED), "tree")
        home = os.path.join(S, "home-{}-{}".format(tree, who))
        shutil.rmtree(home, ignore_errors=True); os.makedirs(os.path.join(home, ".Trash"))
        env = dict(os.environ)
        env["HOME"] = home
        env["DEDUP_CACHE_DIR"] = os.path.join(S, "cache-{}-{}".format(tree, who))
        flags = ["--min-size", "1", "--no-cache", "--auto", "shortest", "-y"]
        key = trees.expected(manifest, min_size=1)
        before = snapshot(root)
        cmd = ([sys.executable, DEDUP, root] + flags + ["-q"]) if who == "oracle" else \
              ([RUST, root, "--report-json", os.path.join(S, "r.json")] + flags)
        proc = subprocess.run(cmd, capture_output=True, text=True, env=env)
        after = snapshot(root)
        trashed = set(os.listdir(os.path.join(home, ".Trash")))
        doomed = doomed_names(key)
        problem = ""
        killed_wrongly = sorted((before - after) - doomed)
        survived_wrongly = sorted(doomed & after)
        if killed_wrongly:
            problem = "deleted {} it had no business deleting, first {!r}".format(len(killed_wrongly), killed_wrongly[0])
        elif survived_wrongly:
            problem = "left {} duplicate(s) behind, first {!r}".format(len(survived_wrongly), survived_wrongly[0])
        elif trashed != {os.path.basename(p) for p in doomed}:
            problem = "Trash holds {} name(s), the tree lost {}".format(len(trashed), len(doomed))
        print("  {:<24} {}".format("{}/{}".format(tree, who), problem or
              "pass ({} removed, {} trashed, exit {})".format(len(before - after), len(trashed), proc.returncode)))
        if problem:
            failures.append((tree, who, problem + " (exit {})".format(proc.returncode)))
print("\n{} check(s), {} failed".format(4, len(failures)))
for f in failures:
    print("  ", f)
sys.exit(1 if failures else 0)
