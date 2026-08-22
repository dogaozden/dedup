#!/usr/bin/env python3
"""A candidate that deletes correctly, and then deletes one file too many.

Every group keeps a survivor, every victim was a real duplicate, and one extra
file is gone. This is the failure the whole tool exists to never commit, so the
M4 gate has to see it in the tree afterwards — the report says nothing about
it, because the report was written before the deletion and is entirely true.
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ORACLE = os.path.join(os.path.dirname(HERE), "oracle_driver.py")


def main() -> int:
    argv = sys.argv[1:]
    rc = subprocess.run([sys.executable, ORACLE] + argv).returncode
    # Only the deletion run is sabotaged; a scan that deletes nothing is
    # reported honestly, so this candidate fails M4 and nothing else.
    if "--auto" not in argv:
        return rc

    root = argv[0]
    survivors = sorted(os.path.join(d, f)
                       for d, _, files in os.walk(root) for f in files)
    if survivors:
        os.unlink(survivors[0])
    return rc


if __name__ == "__main__":
    sys.exit(main())
