#!/usr/bin/env python3
"""The guards on the deletion path, each proved by breaking it.

REVIEW-M8 items 1 and 2. Three mutants, each undoing one thing the fix round
added, each required to fail `cargo test`:

  * the stale-mark fix — `absorb_found` back to pruning by group id alone, and
    `selection()` back to indexing directly. This is the defect the reviewer
    found: a refusal that also broke a candidate republished a SHORTER group
    while the marks against it survived, and the browser thread then panicked
    indexing past its end. A panic there used to hang the process; see
    `m8_panic_probe.py` for that half.
  * `prove_marked`'s `whole` forced TRUE — the proof passes everything, and a
    group whose bytes disagree would be deleted from.
  * `prove_marked`'s `whole` forced FALSE — the proof refuses everything, and
    nothing could ever be deleted.

The second and third both SURVIVED `cargo test` before this round; only the
pty battery noticed. That is why they are here.

The source is restored and rebuilt afterwards, and the restore is proved by
comparing the release binary's hash with the one this script built from the
committed source before it started — the stale-mutant rule, checked rather
than asserted.

Nothing here touches /Volumes and nothing is written outside `rust/src`.

usage: m8_deletion_mutants.py
"""
import hashlib
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RUST = os.path.dirname(os.path.dirname(HERE))
BINARY = os.path.join(RUST, "target", "release", "dedup-rs")
MODEL = os.path.join(RUST, "src", "tui", "model.rs")
PIPELINE = os.path.join(RUST, "src", "pipeline.rs")

WHOLE = "        let whole = classes.len() == 1 && classes[0].len() == members.len();"

# (name, [(file, find, replace), ...])
MUTANTS = [
    ("the stale-mark fix reverted", [
        (MODEL,
         "            self.marked.clear();\n            return;",
         "            let live: std::collections::BTreeSet<usize> =\n"
         "                self.groups.iter().map(|g| g.index).collect();\n"
         "            self.marked.retain(|(id, _)| live.contains(id));\n"
         "            return;"),
        (MODEL,
         "record: self.groups[position].members.get(member)?.record,",
         "record: self.groups[position].members[member].record,"),
    ]),
    ("the proof always PASSES  (whole = true)", [
        (PIPELINE, WHOLE, "        let whole = true;\n        let _ = &classes;"),
    ]),
    ("the proof always FAILS   (whole = false)", [
        (PIPELINE, WHOLE, "        let whole = false;"),
    ]),
]


def sha(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=RUST, capture_output=True,
                          text=True)


def main():
    originals = {path: open(path).read() for path in (MODEL, PIPELINE)}
    cargo("build", "--release")
    before = sha(BINARY)
    problems = []
    try:
        for name, edits in MUTANTS:
            for path, find, replace in edits:
                body = open(path).read()
                if find not in body:
                    problems.append("%s: the text to mutate is gone — update "
                                    "this script" % name)
                    continue
                open(path, "w").write(body.replace(find, replace, 1))
            done = cargo("test", "--release")
            failed = [line.split()[1] for line in done.stdout.splitlines()
                      if line.startswith("test ") and line.rstrip().endswith("FAILED")]
            panics = sorted({line.strip() for line in done.stdout.splitlines()
                             if "index out of bounds" in line})
            print("%-40s %s" % (name, "caught" if failed else "SURVIVED"))
            for test in failed:
                print("%-40s   by %s" % ("", test))
            for panic in panics:
                print("%-40s   %s" % ("", panic))
            if not failed:
                problems.append("%s: nothing failed — this guard is guarded "
                                "by nothing" % name)
            for path in originals:
                open(path, "w").write(originals[path])
    finally:
        for path, body in originals.items():
            open(path, "w").write(body)
        cargo("build", "--release")

    print()
    green = cargo("test", "--release")
    line = [ln for ln in green.stdout.splitlines() if ln.startswith("test result")]
    print("$ cargo test   (source restored)")
    print("   %s" % (line[0] if line else "no result line"))
    if "0 failed" not in (line[0] if line else ""):
        problems.append("the suite is not green again after the mutants")

    after = sha(BINARY)
    print("$ shasum -a 256 target/release/dedup-rs   (the stale-mutant rule)")
    print("   before: %s" % before)
    print("   after:  %s" % after)
    if before != after:
        problems.append("the restored binary is not the one we started with")

    print()
    for problem in problems:
        print("FAIL  %s" % problem)
    if not problems:
        print("pass  every guard on the deletion path fails cargo test when "
              "it is taken away")
    return len(problems)


if __name__ == "__main__":
    sys.exit(main())
