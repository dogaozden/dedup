#!/usr/bin/env python3
"""A candidate that finds every duplicate and then quietly loses one group.

The most flattering way to be wrong: the report is well-formed, the counters
are honest, the paths are real, and one group at the end of the listing — the
smallest one, the least likely to be noticed — never made it into the JSON.
If the M2 gate compared anything less than the whole listing, this would pass.
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
    if report.get("groups"):
        report["groups"] = report["groups"][:-1]
    with open(out, "w") as fh:
        json.dump(report, fh)
    return 0


if __name__ == "__main__":
    sys.exit(main())
