# ROUND 2 — GOAL: SCORE ≤ 0.50

You are optimizing `dedup`, a duplicate-file finder, at
`/Users/dogaozden/AI_Projects/dedup`.

**Your branch: `BRANCH`.** Create it off `main`. You may be racing other
optimizers on their own branches — never touch a branch or worktree that is
not yours, and never touch `main`.

**The number.**

    cd /Users/dogaozden/AI_Projects/dedup && python3 bench/run.py --score

It prints `SCORE: <n>` only when every correctness gate passes and no
scenario's bytes regress. No green gates, no score. Lower is better.
1.000 = the pinned baseline. Measure before your first change.

**Target: SCORE ≤ 0.50, reproduced at `--seed 1` and `--seed 2`, exit 0.**

**Reality, so you budget effort honestly.** `main` starts at ~0.85 (measured
0.849–0.852). The believed floor is ~0.60: bytes read have an
information-theoretic minimum — true duplicates must be read in full to be
proven — and the harness enforces that with a byte-floor gate. Getting under
0.60 requires a structural idea nobody has had yet. That is why this target
exists. Round one's full journals are in `bench/LOG.md`; do not re-run their
dead ends, and remember their hardest lesson: a rejection is only valid
against the tree it was measured on.

**Honest failure beats fake success.** If you exhaust ideas (three
consecutive attempts below the noise floor, per GOAL.md) without reaching
0.50: write the exhaustion summary with your best verified score and stop.
That counts as completing this goal. A gamed score is a failed run: the
accepted score is computed by a referee — a clean checkout of `bench/` from
`main`, scoring your `dedup.py` via `--dedup` — so edits to your local
`bench/` cannot move the number that counts. Any claimed score below 0.60
gets a line-by-line audit of your diff before it is believed.

**Freedom.** Restructure anything — pipeline stages, threading model,
`multiprocessing`, constants, algorithms — provided the inviolables in
`bench/GOAL.md` hold: byte-exact proof before anything is called a duplicate,
at least one survivor per group, no persistent state between scans, stdlib
only, one file, Python 3.9. `bench/` and `tests/` accept additions only.

**Process.** Read `bench/GOAL.md` fully before touching anything — it is
binding. One change at a time, measured. Journal every attempt in
`bench/LOG.md`. Conventional commits, push your branch after each keep. The
harness serializes benchmark runs through an advisory lock on
`/Volumes/dedupbench` — if it is held, wait; never delete the lockfile.
Nothing outside the repo and `/Volumes/dedupbench`, never
`/Volumes/My New Passport`, no sudo.
