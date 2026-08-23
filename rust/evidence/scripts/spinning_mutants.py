#!/usr/bin/env python3
"""Break the capped comparison on purpose and check the tests notice.

A test that passes proves nothing on its own. REVIEW-M5 set the standard for
anything guarding a file from deletion: add the test, then prove it fails when
the guard goes. The capped sweep is such a thing — it decides which files are
byte-identical under `--profile spinning`, and `--auto` deletes on that answer.

Each mutant is a one-edit change to `split_identical_capped` that a plausible
implementation could have shipped. The tree is copied to scratch first, so the
mutants are never built in the working checkout.

Run from the repository root:

    python3 rust/evidence/scripts/spinning_mutants.py
"""

import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
SCRATCH = ("/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/"
           "620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad")
BASE = os.path.join(SCRATCH, "feat-spin-mutants")

# (name, what it breaks, the line as written, the line as mutated)
MUTANTS = [
    (
        "cohort-width",
        "reads SPIN_INTERLEAVE + 1 files at once instead of SPIN_INTERLEAVE",
        "for batch in clusters.chunks(SPIN_INTERLEAVE - 1) {",
        "for batch in clusters.chunks(SPIN_INTERLEAVE) {",
    ),
    (
        "drop-leftovers",
        "never offers a non-matching member to a later pivot",
        "            if !fused.is_empty() {\n                self.leftovers.push(fused);\n            }",
        "            if false && !fused.is_empty() {\n                self.leftovers.push(fused);\n            }",
    ),
    (
        "forget-the-partition",
        "carries only each cluster's representative, losing what a pass proved",
        "                } else {\n                    fused.extend_from_slice(cluster_of(representative));\n                }",
        "                } else {\n                    fused.push(representative);\n                }",
    ),
    (
        "settle-on-a-lost-pivot",
        "settles the pivot's class even though a cohort could not read it",
        "        if self.lost_the_pivot {",
        "        if false && self.lost_the_pivot {",
    ),
    (
        "strand-a-dead-cluster",
        "drops the other members of a cluster whose representative went away",
        "            let rest = cluster_of(index)[1..].to_vec();",
        "            let rest: Vec<usize> = Vec::new();\n            let _ = cluster_of(index);",
    ),
    (
        "merge-on-the-first-cohort",
        "stops sweeping once the pivot has matched somebody",
        "        for batch in clusters.chunks(SPIN_INTERLEAVE - 1) {",
        "        for batch in clusters.chunks(SPIN_INTERLEAVE - 1) {\n            if round.merged.len() > 1 {\n                clusters_left(&mut round, batch);\n                continue;\n            }",
    ),
]


def preamble() -> str:
    """A helper the merge-on-the-first-cohort mutant needs to compile."""
    return """
fn clusters_left(round: &mut Round, batch: &[Vec<usize>]) {
    round.leftovers.extend(batch.iter().cloned());
}
"""


# Mutants whose damage only shows in a whole scan, verified by building the
# mutant and running `spinning_meets_the_rest.py` against it.
WHOLE_PROGRAM = [
    (
        "one-cache-column",
        "files the head-only probe and the two-ends probe in the same column",
        "        Probe::Head { .. } => Field::Head,",
        "        Probe::Head { .. } => Field::Ends,",
        "src/stages.rs",
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
        mutated = original.replace(before, after)
        if name == "merge-on-the-first-cohort":
            mutated += preamble()
        with open(os.path.join(tree, "src", "compare.rs"), "w") as fh:
            fh.write(mutated)
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
        print("mutant {:<26} {}".format(name, what))
        print("  {}\n".format(state))

    with open(os.path.join(tree, "src", "compare.rs"), "w") as fh:
        fh.write(original)

    # The whole-program mutants. These need a release build, so the target
    # directory is kept in scratch and reused between them.
    target = os.path.join(BASE, "target")
    checker = os.path.join(REPO, "rust", "evidence", "scripts",
                           "spinning_meets_the_rest.py")
    binary = os.path.join(target, "release", "dedup-rs")
    env = {"CARGO_TARGET_DIR": target}
    print("Baseline: the same tree, built and run through "
          "spinning_meets_the_rest.py.")
    if run(["cargo", "build", "--release"], tree, env).returncode != 0:
        raise SystemExit("the unmutated tree does not build")
    clean = run([sys.executable, checker], REPO, {"DEDUP_RS_CANDIDATE": binary})
    if clean.returncode != 0:
        print(clean.stdout[-2000:])
        raise SystemExit("the unmutated tree fails its own combination checks")
    print("  every combination behaves\n")

    for name, what, before, after, where in WHOLE_PROGRAM:
        source_path = os.path.join(REPO, "rust", where)
        with open(source_path) as fh:
            source = fh.read()
        if source.count(before) != 1:
            raise SystemExit(
                "mutant {}: its anchor appears {} times, not once".format(
                    name, source.count(before)))
        with open(os.path.join(tree, where), "w") as fh:
            fh.write(source.replace(before, after))
        built = run(["cargo", "build", "--release"], tree, env)
        print("mutant {:<26} {}".format(name, what))
        if built.returncode != 0:
            print("  did not build\n")
        else:
            result = run([sys.executable, checker], REPO,
                         {"DEDUP_RS_CANDIDATE": binary})
            broke = [line for line in result.stdout.splitlines() if "FAIL" in line]
            if broke:
                print("  caught: " + broke[0].split("FAIL:")[1].strip()[:110] + "\n")
            else:
                print("  SURVIVED — no check noticed\n")
                survived += 1
        with open(os.path.join(tree, where), "w") as fh:
            fh.write(source)

    shutil.rmtree(BASE, ignore_errors=True)
    if survived:
        print("{} mutant(s) survived: the net has holes".format(survived))
        return 1
    print("every mutant was caught")
    return 0


if __name__ == "__main__":
    sys.exit(main())
