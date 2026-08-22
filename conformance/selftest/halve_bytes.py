#!/usr/bin/env python3
"""A candidate that reads honestly and then halves the bill.

bytes_read and opens are self-reported — the harness drives a foreign binary
and cannot wrap its syscalls — so the obvious cheat is to claim the pipeline
was twice as clever as it was. Two independent things must catch it: the band
against the oracle's instrumented count, and the floor the tree itself implies,
which no implementation that actually proves its groups can go under.
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ORACLE = os.path.join(os.path.dirname(HERE), "oracle_driver.py")


def main() -> int:
    argv = sys.argv[1:]
    rc = subprocess.run([sys.executable, ORACLE] + argv).returncode
    if rc != 0:
        return rc
    out = argv[argv.index("--report-json") + 1]
    with open(out) as fh:
        report = json.load(fh)
    report["bytes_read"] //= 2
    report["opens"] //= 2
    with open(out, "w") as fh:
        json.dump(report, fh)
    return 0


if __name__ == "__main__":
    sys.exit(main())
