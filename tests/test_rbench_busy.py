"""Tests for RBENCH's rival detection.

The bench refuses to time anything while another harness is scanning the same
volume. That refusal is only as good as the pattern behind it, and the pattern
has already been wrong once in the direction that matters: too narrow, so the
check was quietly off for the commonest way of starting a second copy.
"""
import importlib.util
import os

import pytest

RUN_PY = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "rbench", "run.py")


def load_rbench():
    spec = importlib.util.spec_from_file_location("rbench_run", RUN_PY)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture(scope="module")
def rbench():
    return load_rbench()


# Command lines that are a rival harness, and must be seen as one.
RIVALS = [
    # The reason this test exists: the ordinary invocation from the repository
    # root, where the character before the path is a space, not a slash.
    "python3 rbench/run.py",
    "python3 rbench/run.py --scale default --seed 2",
    "python3 /Users/someone/AI_Projects/dedup/rbench/run.py",
    "/usr/bin/python3 /tmp/rbench-referee/rbench/run.py --candidate 95afc0f",
    # The Python bench, which shares the volume and the name shape.
    "python3 bench/run.py",
    "python3 /abs/path/bench/run.py",
    # The tool itself, however it was reached.
    "rust/target/release/dedup-rs /Volumes/dedupbench/x --list",
    "dedup-rs /Volumes/dedupbench/x",
    # The other harnesses that read the same corpora.
    "python3 rbench/_measure.py",
    "python3 conformance/oracle_driver.py --root /tmp/x",
    "python3 conformance/run.py --gate all",
]

# Command lines that are not a harness, and must not stop a run.
BYSTANDERS = [
    "python3 tools/run.py",             # some other project's runner
    "/Applications/Firefox.app/Contents/MacOS/firefox",
    "python3 -m http.server",
    "rsync -a /Volumes/dedupbench/ /backup/",
    "dedup-rs-notes.txt",               # the name inside a longer word
    "mydedup-rs /tmp/x",
]

# Anything that merely names the harness path is treated as the harness, and
# these are the cost of that. The check is deliberately built to fail closed:
# a false positive is a refusal you can see and argue with, while a false
# negative is a number quoted from a run that was racing somebody else. Widening
# the anchor to catch `python3 rbench/run.py` brought these along, and xbench
# made the same trade for the same reason. Recorded so that anyone who tightens
# the pattern later can see what was known at the time — not as an ideal.
FAILS_CLOSED = [
    "vim rbench/run.py",
    "grep -rn rbench/run.py .",
]


@pytest.mark.parametrize("command", RIVALS)
def test_a_rival_harness_is_recognised(rbench, command):
    assert rbench.BUSY_PATTERN.search(command), \
        "{!r} is a rival harness and was not recognised".format(command)


@pytest.mark.parametrize("command", BYSTANDERS)
def test_a_bystander_is_left_alone(rbench, command):
    assert not rbench.BUSY_PATTERN.search(command), \
        "{!r} is not a harness and would have blocked a run".format(command)


@pytest.mark.parametrize("command", FAILS_CLOSED)
def test_naming_the_harness_is_enough_to_stop_a_run(rbench, command):
    assert rbench.BUSY_PATTERN.search(command), \
        "{!r} no longer matches — if that was deliberate, move it to " \
        "BYSTANDERS and say why".format(command)
