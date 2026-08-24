#!/usr/bin/env python3
"""Every fenced output block in a milestone claim must come from a file.

MILESTONES.md is the permanent record and its rule is that captured output is
quoted, never retyped. Until now that rule was enforced per-builder, by whoever
happened to remember it. REVIEW-M5_2 found fifteen lines in the M5.2 claim with
no evidence file behind them; REVIEW-M7 checked all ten M7 blocks by hand,
found them faithful, and asked for the check to become a script so the next
milestone does not depend on somebody being careful.

This is that script. It pulls every ```text block out of one `## M<n>` section
of MILESTONES.md and requires each to appear, verbatim, inside a file under
`rust/evidence/`. Blocks fenced as plain ``` are treated as commands, not
output, and are not checked — a claim has to be able to show what it ran.

Exit 0 when every block is traced, 1 when any is not.

Run from the repository root:

    python3 rust/evidence/scripts/claim_audit.py M7
    python3 rust/evidence/scripts/claim_audit.py --all
"""

import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
CLAIMS = os.path.join(REPO, "rust", "MILESTONES.md")
EVIDENCE = os.path.join(REPO, "rust", "evidence")


def sections(text: str):
    """Each `## M<n> ...` heading and the body under it."""
    marks = [(m.start(), m.group(1)) for m in
             re.finditer(r"^## (M[0-9._]+)\b.*$", text, re.M)]
    for index, (start, name) in enumerate(marks):
        end = marks[index + 1][0] if index + 1 < len(marks) else len(text)
        yield name, text[start:end]


def evidence_files() -> dict:
    out = {}
    for name in sorted(os.listdir(EVIDENCE)):
        path = os.path.join(EVIDENCE, name)
        if os.path.isfile(path):
            try:
                with open(path) as fh:
                    out[name] = fh.read()
            except UnicodeDecodeError:
                continue
    return out


def audit(name: str, body: str, files: dict) -> int:
    blocks = re.findall(r"```text\n(.*?)```", body, re.S)
    if not blocks:
        print("{}: no fenced output blocks".format(name))
        return 0
    missing = 0
    for block in blocks:
        quoted = block.rstrip("\n")
        found = [f for f, text in files.items() if quoted in text]
        # Two milestones can capture the same script's output, so prefer the
        # file this section named itself after before falling back.
        found.sort(key=lambda f: (not f.lower().startswith(name.lower()), f))
        if found:
            extra = "" if len(found) == 1 else "  (also in {})".format(
                ", ".join(found[1:]))
            print("  {:>4} lines  <- {}{}".format(
                len(quoted.splitlines()), found[0], extra))
        else:
            missing += 1
            head = quoted.splitlines()[:3]
            print("  {:>4} lines  <- NOTHING IN evidence/ CONTAINS THIS".format(
                len(quoted.splitlines())))
            for line in head:
                print("        | " + line)
    print("{}: {} block(s), {} untraced".format(name, len(blocks), missing))
    return missing


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    want = sys.argv[1]
    with open(CLAIMS) as fh:
        text = fh.read()
    files = evidence_files()
    missing = 0
    seen = 0
    for name, body in sections(text):
        if want != "--all" and name != want:
            continue
        seen += 1
        print("=== {} ===".format(name))
        missing += audit(name, body, files)
    if not seen:
        raise SystemExit("no section called {} in MILESTONES.md".format(want))
    print()
    if missing:
        print("{} block(s) in the claim are not quotes of anything in "
              "evidence/".format(missing))
        return 1
    print("every fenced output block is a verbatim quote of a file in evidence/")
    return 0


if __name__ == "__main__":
    sys.exit(main())
