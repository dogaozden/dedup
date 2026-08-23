#!/usr/bin/env python3
"""Break the one-table rule on purpose and check the golden test notices.

M5_1-MENU.md's requirement is that the parser, `--help` and the menu all come
from one table so they cannot drift apart, and that a golden test enforces it.
A golden test that passes proves nothing by itself — the question the reviewer
will ask is whether it would fail if one of the three stopped reading the
table. So each mutant here makes exactly that happen.

The tree is copied to scratch first; nothing is built in the working checkout.

Run from the repository root:

    python3 rust/evidence/scripts/menu_mutants.py
"""

import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.abspath(__file__)))))
SCRATCH = ("/private/tmp/claude-501/-Users-dogaozden-AI-Projects-tib/"
           "620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad")
BASE = os.path.join(SCRATCH, "feat-menu-mutants")

# (name, what it breaks, file, the text as written, the text as mutated)
MUTANTS = [
    (
        "help-misses-a-flag",
        "renders every flag but the first",
        "src/cli.rs",
        "    for flag in FLAGS {\n        let mut said = wrapped(flag.help, room);",
        "    for flag in FLAGS.iter().skip(1) {\n        let mut said = wrapped(flag.help, room);",
    ),
    (
        "help-invents-a-flag",
        "hand-writes a line for a flag the parser does not accept",
        "src/cli.rs",
        None,  # spelled out in INVENT below: an insertion, not a replacement
        None,
    ),
    (
        "help-default-drifts",
        "says a default the code does not have",
        "src/cli.rs",
        '        help: "ignore files smaller than this",\n        default: "10485760 bytes (10 MiB)",',
        '        help: "ignore files smaller than this",\n        default: "1048576 bytes (1 MiB)",',
    ),
    (
        "menu-writes-its-own-default",
        "shows a default of its own instead of the table's",
        "src/tui/menu.rs",
        "            _ if self.value.is_empty() => (self.flag.default.to_string(), false),",
        '            _ if self.value.is_empty() => ("(unset)".to_string(), false),',
    ),
    (
        "menu-drops-a-field",
        "leaves a field out of the argument list it hands back",
        "src/tui/menu.rs",
        "                Takes::Nothing if row.on => out.push(row.flag.long.to_string()),",
        "                Takes::Nothing if row.on => {}",
    ),
    (
        "menu-skips-validation",
        "accepts whatever is typed and lets the scan find out",
        "src/tui/menu.rs",
        "        let flag = self.rows[row].flag;",
        "        return None;\n        #[allow(unreachable_code)]\n        let flag = self.rows[row].flag;",
    ),
]

# The invented-flag mutant is an insertion rather than a replacement, so it is
# spelled out here instead of as a pair of anchors.
INVENT = (
    '    out.push_str(&format!(\n        "  {:widest$}  {}\\n",\n        HELP_FLAGS.join(", "),\n'
    '        "print this and stop",\n    ));',
    '    out.push_str(&format!(\n        "  {:widest$}  {}\\n",\n        HELP_FLAGS.join(", "),\n'
    '        "print this and stop",\n    ));\n    out.push_str("      --turbo             go faster\\n");',
)


# Mutants whose damage only shows in a running program on a real terminal.
# Each is verified by building it and running `tui-terminal-death.py` against
# it, and each must fail a scenario naming the screen it broke.
WHOLE_PROGRAM = [
    (
        "one-of-three-answers",
        "enters the blocking read when poll said nothing was typed",
        "src/tui/menu.rs",
        """        match crate::sys::wait_for_terminal_input(super::WAIT_FOR_KEY) {
            crate::sys::Input::Ready => {}
            crate::sys::Input::HungUp => break Answer::TerminalGone,
            // Nothing typed. Round the loop to repaint, which is also how a
            // window resized while nobody was typing gets its new size.
            crate::sys::Input::Waited => continue,
        }""",
        """        if crate::sys::wait_for_terminal_input(super::WAIT_FOR_KEY)
            == crate::sys::Input::HungUp
        {
            break Answer::TerminalGone;
        }""",
    ),
    (
        "drop-the-dead-terminal",
        "lets ratatui print to a descriptor that is gone, panicking on the way out",
        "src/tui/menu.rs",
        "    if matches!(answer, Answer::TerminalGone) {",
        "    if false && matches!(answer, Answer::TerminalGone) {",
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


SUITE = ["cargo", "test", "--", "cli::tests", "tui::menu::tests"]


def main() -> int:
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE)
    tree = os.path.join(BASE, "rust")
    shutil.copytree(os.path.join(REPO, "rust"), tree,
                    ignore=shutil.ignore_patterns("target", "evidence"))

    print("Baseline: the tree as written.")
    clean = run(SUITE, tree)
    if clean.returncode != 0:
        print(clean.stdout[-3000:])
        raise SystemExit("the unmutated tree does not pass")
    print("  cli::tests and tui::menu::tests all pass\n")

    survived = 0
    for name, what, where, before, after in MUTANTS:
        if name == "help-invents-a-flag":
            where, before, after = "src/cli.rs", INVENT[0], INVENT[1]
        source_path = os.path.join(REPO, "rust", where)
        with open(source_path) as fh:
            source = fh.read()
        if source.count(before) != 1:
            raise SystemExit(
                "mutant {}: its anchor appears {} times in {}, not once — the "
                "source moved and this script is stale".format(
                    name, source.count(before), where))
        with open(os.path.join(tree, where), "w") as fh:
            fh.write(source.replace(before, after))
        result = run(SUITE, tree)
        broke = failing_tests(result.stdout)
        if result.returncode != 0 and not broke:
            state = "did not build"
        elif broke:
            state = "caught by " + ", ".join(t.split("::")[-1] for t in broke)
        else:
            state = "SURVIVED — no test noticed"
            survived += 1
        print("mutant {:<28} {}".format(name, what))
        print("  {}\n".format(state))
        with open(os.path.join(tree, where), "w") as fh:
            fh.write(source)

    # The terminal-death mutants. These need a release build and a pty, so the
    # target directory lives in scratch and is reused between them.
    target = os.path.join(BASE, "target")
    binary = os.path.join(target, "release", "dedup-rs")
    checker = os.path.join(REPO, "rust", "evidence", "scripts",
                           "tui-terminal-death.py")
    env = {"CARGO_TARGET_DIR": target}
    print("Baseline: the same tree, built and run through tui-terminal-death.py.")
    if run(["cargo", "build", "--release"], tree, env).returncode != 0:
        raise SystemExit("the unmutated tree does not build")
    clean = run([sys.executable, checker, binary], REPO)
    if clean.returncode != 0:
        print(clean.stdout[-2000:])
        raise SystemExit("the unmutated tree fails its own terminal-death scenarios")
    print("  every scenario passed\n")

    for name, what, where, before, after in WHOLE_PROGRAM:
        source_path = os.path.join(REPO, "rust", where)
        with open(source_path) as fh:
            source = fh.read()
        if source.count(before) != 1:
            raise SystemExit(
                "mutant {}: its anchor appears {} times in {}, not once".format(
                    name, source.count(before), where))
        with open(os.path.join(tree, where), "w") as fh:
            fh.write(source.replace(before, after))
        built = run(["cargo", "build", "--release"], tree, env)
        print("mutant {:<28} {}".format(name, what))
        if built.returncode != 0:
            print("  did not build\n")
        else:
            result = run([sys.executable, checker, binary], REPO)
            broke = [line.strip() for line in result.stdout.splitlines()
                     if "FAIL" in line]
            if broke:
                print("  caught: {}\n".format(broke[0]))
            else:
                print("  SURVIVED — no scenario noticed\n")
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
