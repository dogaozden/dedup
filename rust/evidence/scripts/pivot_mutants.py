#!/usr/bin/env python3
"""Break the pivot-window comparison on purpose and check the tests notice.

A test that passes proves nothing on its own. REVIEW-M5 set the standard for
anything guarding a file from deletion: add the test, then prove it fails when
the guard goes. Stage 5 is such a thing — it decides which files are
byte-identical, and `--auto` deletes on that answer. Under `--profile spinning`
it is now this sweep that decides.

Each mutant is a one-edit change a plausible implementation could have shipped:
a residue merged instead of compared again, a regroup that restarts a run too
late or too early, a lost pivot settled anyway, a window that never grows. The
tree is copied to scratch first, so no mutant is ever built in the working
checkout and no stale mutant binary can be left behind in it.

Run from the repository root:

    python3 rust/evidence/scripts/pivot_mutants.py
"""

import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
BASE = os.path.join(REPO, "rust", "target", "m7", "mutants")

# (name, what it breaks, the line as written, the line as mutated)
MUTANTS = [
    (
        "merge-the-residue",
        "calls everything that left the pivot in one run equal, without reading again",
        "        if ready.len() < 2 {\n            continue; // a class of one is settled\n        }",
        "        if ready.len() < 2 {\n            continue; // a class of one is settled\n        }\n"
        "        if from > prefix {\n            matched.push(ready);\n            continue;\n        }",
    ),
    (
        "drop-the-residue",
        "never offers a member that left the pivot to any later pivot",
        "        let left = apart\n            .into_pairs()\n            .into_iter()\n"
        "            .filter(|(_, slots)| slots.len() > 1)",
        "        let left = apart\n            .into_pairs()\n            .into_iter()\n"
        "            .filter(|(_, slots)| slots.len() > usize::MAX)",
    ),
    (
        "regroup-past-the-parting",
        "restarts a residue AFTER the run it parted in, so the parting byte is never re-read",
        "                        verdict = Verdict::Apart(offset + done);",
        "                        verdict = Verdict::Apart(offset + done + block.len());",
    ),
    (
        "regroup-at-the-window",
        "restarts a residue at the window rather than at the run it parted in",
        "                    if block != *theirs {\n                        verdict = Verdict::Apart(offset + done);",
        "                    if block != *theirs {\n                        verdict = Verdict::Apart(offset);",
    ),
    (
        "one-bucket-for-every-parting",
        "files every leaver under one offset, so two that parted in different runs meet again",
        "                    Verdict::Apart(at) => sweep.left(slot, at),",
        "                    Verdict::Apart(_) => sweep.left(slot, from),",
    ),
    (
        "hash-the-run-that-parted",
        "lets a leaver's last run reach its digest, which it will read again",
        "                    if context.learn {\n                        hashers[slot].update(&block);\n                    }\n                    done += block.len();",
        "                    done += block.len();",
    ),
    (
        "only-the-first-run-of-a-window",
        "judges a member on the first run of each window and skips the rest",
        "                for (want, theirs) in &held {",
        "                for (want, theirs) in held.iter().take(1) {",
    ),
    (
        "settle-on-a-lost-pivot",
        "settles the pivot's class even though the pivot could not be read",
        "            Ending::Lost => {\n                unreadable.push(records[opened[pivot]].path().to_string());\n                sweep.abandoned()\n            }",
        "            Ending::Lost => {\n                unreadable.push(records[opened[pivot]].path().to_string());\n"
        "                let (live, left) = sweep.residue();\n"
        "                if live.len() > 1 {\n                    matched.push(live);\n                }\n"
        "                left\n            }",
    ),
    (
        "strand-a-lost-pivots-class",
        "drops the members a lost pivot had already proved equal to each other",
        "    fn abandoned(self) -> Vec<(Vec<usize>, usize)> {\n        let at = self.matched_to;\n"
        "        let (live, mut left) = self.residue();\n        if live.len() > 1 {\n"
        "            left.insert(0, (live, at));\n        }",
        "    fn abandoned(self) -> Vec<(Vec<usize>, usize)> {\n        let _ = self.matched_to;\n"
        "        let (_live, left) = self.residue();",
    ),
    (
        "flat-windows",
        "never lets a window grow, so every window costs a head movement per member",
        "            window = window.saturating_mul(PIVOT_GROWTH).min(ceiling);",
        "            window = window.min(ceiling);",
    ),
    (
        "no-seek-back",
        "leaves a regrouped class's handles wherever the last comparison stopped",
        "            if positions[slot] != from as u64 {",
        "            if false && positions[slot] != from as u64 {",
    ),
    (
        "runs-as-long-as-the-chunk",
        "reads a window in chunk-long runs, so a parting is placed a chunk wide",
        "fn pivot_run(chunk: usize) -> usize {\n    chunk.min(PIVOT_RUN)\n}",
        "fn pivot_run(chunk: usize) -> usize {\n    chunk\n}",
    ),
]


def run(command, cwd, extra_env=None):
    environment = dict(os.environ)
    environment.update(extra_env or {})
    return subprocess.run(command, cwd=cwd, capture_output=True, text=True,
                          env=environment)


def failing_tests(output: str):
    names = []
    for line in output.splitlines():
        line = line.strip()
        if line.startswith("test ") and line.endswith(" ... FAILED"):
            names.append(line[len("test "):-len(" ... FAILED")])
    return sorted(names)


def main() -> int:
    source = os.path.join(REPO, "rust", "src", "compare.rs")
    with open(source) as fh:
        original = fh.read()

    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    tree = os.path.join(BASE, "rust")
    shutil.copytree(os.path.join(REPO, "rust"), tree,
                    ignore=shutil.ignore_patterns("target", "evidence"))

    print("Baseline: the tree as written.")
    clean = run(["cargo", "test", "--", "compare::tests"], tree)
    if clean.returncode != 0:
        print(clean.stdout[-3000:])
        raise SystemExit("the unmutated tree does not pass; nothing to compare against")
    print("  compare::tests all pass\n")

    survived = 0
    for name, what, before, after in MUTANTS:
        if original.count(before) != 1:
            raise SystemExit(
                "mutant {}: its anchor appears {} times, not once — the source "
                "moved and this script is stale".format(name, original.count(before)))
        with open(os.path.join(tree, "src", "compare.rs"), "w") as fh:
            fh.write(original.replace(before, after))
        result = run(["cargo", "test", "--", "compare::tests"], tree)
        broke = failing_tests(result.stdout)
        # A mutant that will not compile is not evidence of anything; say so.
        if result.returncode != 0 and not broke:
            state = "did not build"
        elif broke:
            state = "caught by " + ", ".join(t.split("::")[-1] for t in broke)
        else:
            state = "SURVIVED — no test noticed"
            survived += 1
        print("mutant {:<32} {}".format(name, what))
        print("  {}\n".format(state))

    with open(os.path.join(tree, "src", "compare.rs"), "w") as fh:
        fh.write(original)
    shutil.rmtree(BASE, ignore_errors=True)
    if survived:
        print("{} mutant(s) survived: the net has holes".format(survived))
        return 1
    print("every mutant was caught")
    return 0


if __name__ == "__main__":
    sys.exit(main())
