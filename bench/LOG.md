# Optimization log — branch `opus`

## Preamble: two optimizers, one machine

This log is branch `opus`. A second optimizer is running the same `GOAL.md`
loop on branch `luna` **at the same time**, on the same Mac and the same RAM
disk. Two consequences worth recording before any numbers:

- I work out of a separate git worktree at `/Users/dogaozden/AI_Projects/dedup-opus`
  so I never touch the other tree, its branch, or its log.
- `/Volumes/dedupbench` is shared and cannot be. `ensure_space()` in
  `bench/run.py` deletes corpora the current run does not need whenever the
  volume is tight, and it is tight (419 MB free of 2.8 GB). Two runs at once
  can delete a tree out from under each other's scan, and the `deletion`
  scenario `rmtree`s the shared `<volume>/.Trashes` at both ends. So every
  harness run below was taken under an advisory lockfile
  (`/Volumes/dedupbench/BENCH.lock`) plus a check for live `bench/run.py` /
  `bench/_measure.py` processes. Details and the protocol are in
  `NOTE-FROM-OPUS.md`, left untracked in the other worktree.
- Seed-2 confirmations are run at `--scale smoke`, which `GOAL.md` §4 sanctions,
  because a seed-2 run at default scale evicts the seed-1 corpora the other
  optimizer is using.

**Prior art read before starting.** Branch `luna` already had commit `a77e106`
"perf: skip stage-3 head during sampling" — the head double-read from the idea
menu. I read it and am not redoing it. `opus` is branched from `main`
(`93d7c11`), not from `luna`, so these two branches are independent attempts
against the same pinned baseline (`012acc4`).

## Baseline — unmodified tree on `opus`

`python3 -m pytest tests/ -q` → 117 passed. `python3 bench/run.py` → exit 0.

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.468    0.478     0.98x     52       53
photos       pass  150.8 MB   150.8 MB    1.00x     0.024    0.055     0.44x     147      97
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.032    0.032     0.99x     109      111
needle       pass  244.1 MB   244.1 MB    1.00x     4.048    4.235     0.96x     313      305
samesize     pass  348.4 MB   348.4 MB    1.00x     1.772    1.813     0.98x     284      219
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.058    0.057     1.01x     152      153
```

The `photos` 0.44x and the `samesize` RSS 284-vs-219 in that table are both
noise on identical code — a useful calibration of what this machine's floor
actually looks like. `photos` finishes in 24 ms.

## 2026-08-21 — Deterministic tie-break on the final group sort

**Hypothesis:** Not a performance idea. `GOAL.md` §7 records a known bug: the
final sort key is `(-size, basename)`, so two groups tying on both fall back to
whatever order the pipeline emitted, which traces to inode allocation order.
Fixing it first makes every later comparison — mine and the other branch's —
read against a stable output ordering.

**Change:** Added the keeper's full path as a third sort key in
`find_duplicates()`: `(-g.size, g.name, g.files[0].path)`. Paths are unique
across groups, so this makes the order total. Added a regression test,
`test_groups_tying_on_size_and_name_order_by_keeper_path`, which builds two
40-byte groups that both keep a file named `same.bin` and asserts the order.
Verified the test fails on the unfixed code 5 runs out of 5, so it is a real
regression test and not one that passes by luck.

**Numbers:** full suite, seed 1, exit 0, 118 passed.

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.463    0.460     1.01x     53       53
photos       pass  150.8 MB   150.8 MB    1.00x     0.023    0.023     0.98x     149      146
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.032    0.033     0.97x     109      109
needle       pass  244.1 MB   244.1 MB    1.00x     4.293    4.442     0.97x     304      297
samesize     pass  348.4 MB   348.4 MB    1.00x     1.639    1.630     1.01x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.056    0.056     1.00x     152      152
```

Seed 2, smoke scale, exit 0:

```text
typical      pass  5.5 MB     5.5 MB      1.00x     0.030    0.029     1.02x     20       20
photos       pass  26.5 MB    26.5 MB     1.00x     0.007    0.007     0.97x     43       43
bigheader    pass  112.5 MB   112.5 MB    1.00x     0.019    0.022     0.87x     103      102
needle       pass  23.4 MB    23.4 MB     1.00x     0.327    0.343     0.95x     46       46
samesize     pass  75.9 MB    75.9 MB     1.00x     0.036    0.034     1.04x     83       83
widegroup    pass  18.1 MB    18.1 MB     1.00x     0.012    0.011     1.05x     31       31
```

**Verdict:** KEPT. Bytes identical on every scenario, time within noise on
every scenario, which is exactly what a sort-key change should do. Kept for
the determinism, not for speed.

**Note for `luna`:** `samesize` at `--scale smoke` runs in 0.036 s and does not
reproduce the quadratic partition behaviour that the default scale shows at
1.6–1.8 s. Smoke is not a useful signal for anything touching `partition_blocks`.
