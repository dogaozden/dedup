#!/usr/bin/env python3
"""Every fenced output block in a milestone claim must come from a file.

MILESTONES.md is the permanent record and its rule is that captured output is
quoted, never retyped. Until now that rule was enforced per-builder, by whoever
happened to remember it. REVIEW-M5_2 found fifteen lines in the M5.2 claim with
no evidence file behind them; REVIEW-M7 checked all ten M7 blocks by hand,
found them faithful, and asked for the check to become a script so the next
milestone does not depend on somebody being careful.

This is that script. It pulls every ```text block out of one `## M<n>` section
of MILESTONES.md and requires each to appear inside a file under
`rust/evidence/` — verbatim, or as an ordered excerpt (every line of the block
present in the file, in order), which is reported as an excerpt so a reader
knows lines were elided. Blocks fenced any other way are commands by
convention and are not checked, but they are COUNTED and named, so a block
that was never looked at can never pass as one that was. A section is an
ARCHIVE when a later section claims the same milestone (the CLAIMED that a
RE-CLAIMED superseded): its discrepancies are reported as history, not
defects, because evidence files are regenerated in place and the old claim
keeps its contemporaneous numbers on purpose.

Exit 0 when every block in every LIVE section is traced, 1 when any is not.

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
    """Every text file under evidence/, recursively, except scripts/."""
    out = {}
    for base, dirs, names in os.walk(EVIDENCE):
        dirs[:] = [d for d in dirs if d != "scripts"]
        for name in sorted(names):
            path = os.path.join(base, name)
            try:
                with open(path) as fh:
                    out[os.path.relpath(path, EVIDENCE)] = fh.read()
            except (UnicodeDecodeError, OSError):
                continue
    return out


def fenced(body: str):
    """(tag, raw, healed) for every fence in the section.

    A fence nested in a list is indented, and a regex that grabs up to the
    closing backticks swallows that indent as a phantom trailing line — the
    bug that produced this parser. Collecting lines between the fences kills
    the phantom. `raw` keeps the lines as written, because most evidence is
    quoted at the indentation it was captured with; `healed` strips the
    opening fence's own indent from each line, for the block whose author
    indented the whole quote to sit inside a bullet. A block is traced when
    EITHER form is found — the two disagree only about leading whitespace.
    """
    blocks = []
    tag = None
    indent = ""
    held: list = []
    for line in body.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("```"):
            if tag is None:
                tag = stripped[3:].strip() or "(plain)"
                indent = line[:len(line) - len(stripped)]
                held = []
            else:
                raw = "\n".join(held)
                healed = "\n".join(
                    line[len(indent):] if line.startswith(indent) else line
                    for line in held) if indent else raw
                blocks.append((tag, raw, healed))
                tag = None
            continue
        if tag is not None:
            held.append(line)
    return blocks


def excerpt_of(lines, text: str) -> bool:
    """Every non-blank line present in `text`, in order — an elided quote."""
    wanted = [line for line in lines if line.strip()]
    if not wanted:
        return False
    have = text.splitlines()
    at = 0
    for line in wanted:
        while at < len(have) and have[at] != line:
            at += 1
        if at == len(have):
            return False
        at += 1
    return True


def audit(name: str, body: str, files: dict, archive: bool) -> int:
    blocks = fenced(body)
    checked = [(raw, healed) for tag, raw, healed in blocks if tag == "text"]
    skipped = [tag for tag, _, _ in blocks if tag != "text"]
    if skipped:
        print("  ({} unchecked block(s), fenced as: {})".format(
            len(skipped), ", ".join(sorted(set(skipped)))))
    if not checked:
        print("{}: no fenced output blocks".format(name))
        return 0
    missing = 0
    for raw, healed in checked:
        forms = [raw.rstrip()]
        if healed.rstrip() != forms[0]:
            forms.append(healed.rstrip())
        lines = forms[0].splitlines()
        found = [f for f, text in files.items()
                 if any(form in text for form in forms)]
        # Two milestones can capture the same script's output, so prefer the
        # file this section named itself after before falling back.
        found.sort(key=lambda f: (not f.lower().startswith(name.lower()), f))
        if found:
            extra = "" if len(found) == 1 else "  (also in {})".format(
                ", ".join(found[1:]))
            print("  {:>4} lines  <- {}{}".format(len(lines), found[0], extra))
            continue
        partial = [f for f, text in files.items()
                   if any(excerpt_of(form.splitlines(), text)
                          for form in forms)]
        partial.sort(key=lambda f: (not f.lower().startswith(name.lower()), f))
        if partial:
            print("  {:>4} lines  <- excerpt of {} (lines elided, all "
                  "present in order)".format(len(lines), partial[0]))
            continue
        if archive:
            print("  {:>4} lines  <- not in current evidence (ARCHIVE: a "
                  "later claim superseded this section and its evidence was "
                  "regenerated in place)".format(len(lines)))
            for line in lines[:3]:
                print("        | " + line)
            continue
        missing += 1
        print("  {:>4} lines  <- NOTHING IN evidence/ CONTAINS THIS".format(
            len(lines)))
        for line in lines[:3]:
            print("        | " + line)
    print("{}: {} block(s), {} untraced".format(name, len(checked), missing))
    return missing


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__.strip().splitlines()[-1].strip())
    want = sys.argv[1]
    with open(CLAIMS) as fh:
        text = fh.read()
    files = evidence_files()
    named = list(sections(text))
    # The LAST section carrying a milestone's name is the live claim; every
    # earlier one under the same name is the archive a fix round left behind.
    last = {}
    for index, (name, _) in enumerate(named):
        last[name] = index
    missing = 0
    seen = 0
    for index, (name, body) in enumerate(named):
        if want != "--all" and name != want:
            continue
        seen += 1
        archive = index != last[name]
        print("=== {} ==={}".format(name, "  (superseded — archive)"
                                    if archive else ""))
        missing += audit(name, body, files, archive)
    if not seen:
        raise SystemExit("no section called {} in MILESTONES.md".format(want))
    print()
    if missing:
        print("{} block(s) in the claim are not quotes of anything in "
              "evidence/".format(missing))
        return 1
    print("every fenced output block is a verbatim quote of a file in "
          "evidence/ (or a labeled excerpt)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
