# Optimization log — branch `opus`

## Stop summary — 2026-08-22

Five ideas processed, which is where this run was told to stop. Two kept, one
rejected on reasoning the benchmark could not supply, one rejected on
measurement, one left on a branch because it is a trade an optimizer does not
get to make alone.

**Cumulative result — branch tip `df8f3f7` against the pinned baseline
`012acc4`, full suite, seed 1, exit 0, 119 passed:**

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.358    0.482     0.74x     35       53
photos       pass  150.8 MB   150.8 MB    1.00x     0.021    0.024     0.88x     148      149
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.033    0.032     1.02x     109      109
needle       pass  244.1 MB   244.1 MB    1.00x     2.950    4.222     0.70x     105      303
samesize     pass  348.4 MB   348.4 MB    1.00x     0.290    1.686     0.17x     284      219
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.052    0.057     0.92x     152      152
```

Time: `samesize` 5.8x faster, `needle` 1.43x, `typical` 1.35x, nothing slower
(`bigheader` 1.02x at 3 rounds is 0.99x at 9). Bytes: unchanged — **no bytes
were won on this branch**, and the one large bytes win found is sitting
unmerged on `opus-head-twotier` for the reason given below. RSS: `needle`
303 MB → 105 MB, `typical` 53 → 35. The `samesize` RSS column moves between
284 and 219 on identical code and is noise; the baseline run at the top of
this log shows the same swing.

**Kept**

- `227827e` — deterministic tie-break on the final group sort. Not a
  performance change; it closes the known nondeterminism bug so that every
  comparison after it reads against a stable ordering.
- `7b7b77b` — `partition_blocks` hands off to grouping-on-the-block when a
  compare pass strips off nothing but its own pivot. `samesize` 1.627s →
  0.302s (5.4x), seed 2 5.2x, everything else flat.
- `df8f3f7` — `hash_stage` gives each reader a batch instead of one task per
  file. `needle` 0.70x, `typical` 0.74x, RSS on `needle` down 65%.

**Rejected, and why**

- A flat smaller `HEAD_BYTES`. Passes every gate and every written rule — 4
  KiB gives `typical` 0.54x and `samesize` 0.76x with no regression anywhere —
  and is still wrong. Stage-5 fall-through is byte-identical from 64 KiB down
  to 1 KiB on these corpora, so the benchmark rewards shrinking the head
  without limit; no scenario contains files sharing a 1–64 KiB prefix, which
  is the case that would charge for it, and real camera raws do exactly that.
  The full argument is in the entry below.
- Unbuffered and raw-fd reads in `hash_head`. 12–15% faster on one thread,
  0.99x at the two readers the tool actually uses, because the bottleneck
  there is the GIL. A cautionary entry more than a result.
- Raising the reader count. `jobs=2` measured optimal on `needle` (1: 4.649s,
  2: 3.174s, 4: 3.333s, 8: 4.169s). Nothing to win.

**Left for the user — `opus-head-twotier` (`b7ef1a3`), not merged**

Reading the head in two tiers cuts `typical` from 108.7 MB to 73.1 MB with
bytes holding on every other scenario and full 64 KiB discrimination intact.
It costs `widegroup` 1.30x wall time (1.25x at `--rounds 9`), because its 519
files are true copies that always need both tiers and pay a second open and
seek for nothing. §4 says a cross-scenario trade goes to the user, so it did.
Whether it is worth taking depends on the device, and this RAM disk is the
wrong instrument to ask: with reads near-free it charges the extra open at
full weight while charging nothing for the seeks a real disk would.

**Where I would look next**

1. Kill `widegroup`'s regression and the two-tier head becomes a clean keep —
   the largest unclaimed bytes win in the tool. The cost is one extra `open`
   per crowded file; anything that avoids reopening (carrying the handle, or
   deciding tier depth per size-bucket before reading) turns a trade into a
   win.
2. `samesize` and `widegroup` spend 91.9 MB and 34.0 MB in stage 3 on heads
   that separate nothing, because every file in those buckets shares all 64
   KiB. Detecting a non-discriminating head *during* the stage rather than
   after would recover most of it.
3. Stage 5 re-reads from offset 0 bytes that stage 3 and stage 4 already read.
   The only legal way to exploit it is to keep those bytes and *compare* them
   rather than trust the digest, which trades RSS for I/O — worth measuring,
   given `COMPARE_BUDGET` already bounds what a group may hold.
4. Not stage-4 sample geometry. `bigheader` is 276.8 MB of stage 5 against
   33.6 MB of stage 4, and its stage 5 is the byte floor — six genuinely
   identical 56 MiB files that must be read end to end. There is little there.

**Two notes on the harness, for whoever maintains it**

- `bench/_measure.py` passes `reporter=None` and starts its clock *after* the
  module is imported. So two ideas the briefing offers in §7 — Reporter
  throttle overhead in the hot loops, and lazy-loading `curses` — cannot be
  measured by this harness at all: the throttle branch never executes, and
  import cost lands in `setup_seconds`, which is a 2-second gate and not a
  reported metric. §4's "no unmeasured wins" rule disqualifies both ideas that
  §7 recommends. (The other branch, `luna`, spent an idea confirming the
  curses half of this empirically and reverted it.)
- The RAM disk was 83–94% full for this entire run, so `ensure_space()`
  evicted corpora repeatedly. That is the harness working as designed, but it
  means a second seed at default scale costs the other seed's corpora, and
  with two optimizers sharing the volume it is a live hazard rather than a
  slow path. See the preamble.


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

## 2026-08-21 — Grouping on the block when the pivot loop goes quadratic

**Hypothesis:** `GOAL.md` §7 says `samesize`'s ~1,400-file bucket reaches
`compare_stage` as one group of mostly-distinct content, so `partition_blocks`
strips off one file per pass — order n²/2 block comparisons, and most of
`samesize`'s scan time. Grouping the blocks in one pass instead should remove
that without touching I/O at all: same bytes read, less CPU riding on top.

**Attempt 1 (rejected): replace the pivot loop outright with a dict.**
`runs.setdefault(item[1], []).append(item)` — still byte-exact, because a dict
only groups two keys when they compare equal and equality on bytes is a memcmp.
The hash picks the slot, it never decides the answer. Full suite, seed 1:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.462    0.467     0.99x     52       49
photos       pass  150.8 MB   150.8 MB    1.00x     0.073    0.021     3.47x     149      149
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.123    0.032     3.88x     110      109
needle       pass  244.1 MB   244.1 MB    1.00x     4.007    4.173     0.96x     307      307
samesize     pass  348.4 MB   348.4 MB    1.00x     0.215    1.701     0.13x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.101    0.057     1.76x     152      152
```

7.9× on `samesize` and a 3.9× regression on `bigheader`. The reason is
`compare_chunk()`: a small group gets a big chunk (up to `SLOW_CHUNK`, 16 MB),
and the dict must hash every block end to end, where the pivot loop's `==`
against a block that differs in its first bytes exits immediately. So the
dict is strictly worse exactly where groups are small and chunks are large.
That is a cross-scenario trade, and §4 says do not keep those. Rejected.

**Attempt 2 (kept): stay with the pivot loop, hand off only when it degenerates.**
A pass that strips off nothing but its own pivot, with more than
`PARTITION_FANOUT` (256) blocks still pending, is the signature of a bucket of
distinct content. At that point, and only then, group the remainder on the
block. Small groups never reach the condition and are byte-for-byte the code
they were before; the 1,400-file bucket trips it on its first pass.

256 is where the two costs cross. Above `PARTITION_FANOUT`, `compare_chunk()`
has already shrunk the chunk to `COMPARE_BUDGET // members` or `MIN_CHUNK`, so
the hand-off hashes at most ~128 MB per round no matter how wide the group;
below it, the quadratic is cheaper than that ceiling.

Full suite, seed 1, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.459    0.463     0.99x     53       53
photos       pass  150.8 MB   150.8 MB    1.00x     0.022    0.022     0.98x     149      146
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.031    0.031     1.00x     110      111
needle       pass  244.1 MB   244.1 MB    1.00x     4.011    4.027     1.00x     307      310
samesize     pass  348.4 MB   348.4 MB    1.00x     0.302    1.627     0.19x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.055    0.056     0.99x     152      152
```

`photos`, `bigheader` and `widegroup` re-measured at `--rounds 9` as §4
requires for the tens-of-milliseconds scenarios — 0.99x, 0.94x, 0.99x. The
1.09x `widegroup` reading in an earlier 3-round pass was noise.

Seed 2, smoke, all scenarios: bytes 1.00x and seconds 0.95x–1.07x everywhere,
no regressions. Seed 2, default scale, `samesize` — smoke does not reproduce
the effect at all (0.033 s there), so the win had to be confirmed at full
scale:

```text
samesize  pass  348.6 MB   348.6 MB    1.00x     0.300    1.565     0.19x     284      284
```

**Verdict:** KEPT. `samesize` 5.4× faster on seed 1 and 5.2× on seed 2, every
other scenario flat, bytes identical everywhere, RSS unchanged. Added
`test_wide_bucket_of_near_misses_finds_the_one_real_pair`, which builds
`PARTITION_FANOUT + 2` same-size files sharing a 64 KiB head plus one real
pair; verified by instrumenting the branch that the test does reach the
hand-off (fires at n=259) and still returns exactly the one true pair.

**Housekeeping:** confirming this at seed 2 default scale made the harness
evict `bigheader-default-seed1` and `adversarial-default-seed1` to fit
`samesize-default-seed2`. Those regenerate on the next run that needs them.

## 2026-08-22 — Shrinking the stage-3 head (rejected as a flat cut, moved to a branch as two tiers)

**Hypothesis:** Stage 3 reads 64 KiB of every candidate. Instrumenting the
per-stage byte split showed how much that is: on seed 1, stage 3 is 255.6 MB
of `needle`'s 255.9 MB, 89.8 MB of `typical`'s 114 MB, 91.9 MB of `samesize`
and 34.0 MB of `widegroup`. It is the largest lever in the tool that is not
already the byte floor.

**Attempt 1 (rejected): just make `HEAD_BYTES` smaller.** Full suite, seed 1,
at 4 KiB — bytes hold or improve on every scenario:

```text
scenario     bytes 64 KiB   bytes 16 KiB   bytes 4 KiB
typical      108.7 MB       101.5 (0.93x)  58.5 (0.54x)
photos       150.8 MB       148.6 (0.99x)  148.0 (0.98x)
bigheader    296.5 MB       296.1 (1.00x)  296.0 (1.00x)
needle       244.1 MB       244.1 (1.00x)  244.1 (1.00x)
samesize     348.4 MB       282.7 (0.81x)  266.2 (0.76x)
widegroup    158.8 MB       134.4 (0.85x)  128.4 (0.81x)
```

It passes every gate and every rule as written. It is still wrong, and the
reason matters more than the numbers. Instrumenting stage-5 fall-through shows
`typical` sends *exactly* 1533 files and 24,155,322 bytes into stage 5 at 64
KiB, at 16 KiB, at 4 KiB, at 2 KiB and at 1 KiB — identical, to the byte. The
head loses no discriminating power at any size, so the benchmark offers an
unbounded gradient toward `HEAD_BYTES` → 0. `needle` at 1 KiB reads 102.7 MB
against 255.9 MB.

That gradient is an artifact. No scenario contains same-size files that share
a prefix of 1–64 KiB and then differ, so nothing here can charge for losing
that band. Real data can: a thousand 25 MB raw files off one camera, identical
in size and sharing a 32 KiB header, are settled today for 64 KiB each and
would instead all fall through to stage 4 at 4 MB each — 64 MB becomes 4 GB.
`bigheader` does not catch it because its files share far more than 64 KiB and
so are unaffected at either setting. Rejected: the win is measured, the risk
is not, and the benchmark cannot tell them apart.

**Attempt 2 (moved to branch `opus-head-twotier`, not merged): two tiers.**
Read one page first; extend to the full 64 KiB only for files that page left
in a bucket with someone else, and only over the part not already read. No
byte is read twice and discrimination is exactly what it was, so the risk
above disappears.

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base
typical      pass  73.1 MB    108.7 MB    0.67x     0.484    0.482     1.01x
photos       pass  150.3 MB   150.8 MB    1.00x     0.026    0.030     0.89x
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.039    0.041     0.95x
needle       pass  244.1 MB   244.1 MB    1.00x     4.785    4.837     0.99x
samesize     pass  348.4 MB   348.4 MB    1.00x     0.344    1.721     0.20x
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.072    0.055     1.30x
```

A third of `typical`'s bytes, bytes holding everywhere else — bought with
`widegroup` wall time at 1.30x, reproducible at `--rounds 9` (1.25x). Its 519
files are true copies sharing the whole head, so they always need both tiers
and pay a second open and seek each for nothing. That is a cross-scenario
trade, and §4 says an optimizer does not get to make that call alone.

**Verdict:** flat cut REJECTED on the reasoning above; two-tier NOT MERGED,
committed to `opus-head-twotier` (`b7ef1a3`) for the user to judge. Whether it
is worth taking depends on the device and this RAM disk cannot answer it: with
reads near-free it charges the extra open at full weight while charging
nothing for the seeks a real disk would.

## 2026-08-22 — Cheaper per-file reads in stage 3 (rejected, below the noise floor)

**Hypothesis:** `hash_head()` is the only reader in the file that opens
*buffered*; `hash_full` and `hash_sample` both use `buffering=0`. On `needle`,
stage 3 is 3.66 s of a 4.60 s scan and moves 255 MB at 70 MB/s, which is
nowhere near what a RAM disk or SHA-256 can do, so per-file overhead is the
cost. Dropping the `io.BufferedReader` layer should cut it.

**Change:** measured three readers over the real 100,000-file `needle` corpus
— the current buffered `open`, `buffering=0` with `read_block()` to close the
short-read hole, and raw `os.open`/`os.read`. All three verified to produce
identical digests on 300 files first.

**Numbers:** single-threaded the hypothesis looks right — 21.7 µs/file
buffered, 19.2 µs unbuffered (0.88x), 18.5 µs raw fd (0.85x). At the reader
count the tool actually uses it evaporates:

```text
  buffered (current)       2.994s   29.9 us/file   1.00x
  unbuffered + read_block  2.955s   29.5 us/file   0.99x
  raw fd                   3.235s   32.3 us/file   1.08x
```

At `jobs=2` the bottleneck is the GIL, not the read path, and the raw-fd
version is worse because its loop holds the GIL for longer.

**Verdict:** REJECTED, 0.99x is far below the noise floor. Recorded because
the single-threaded measurement is genuinely misleading here — anyone
micro-benchmarking this function on one thread will conclude there is a 12%
win waiting, and there is not.

Also measured and rejected while here: reader count. `hash_stage` on `needle`
runs 4.649s at `jobs=1`, 3.174s at 2, 3.333s at 4, 4.169s at 8. The current
`read_jobs()` default of 2 for a non-flash volume is already the optimum on
this corpus; there is nothing to win by raising it.

## 2026-08-22 — Give each reader a batch of files instead of one task per file

**Hypothesis:** With the read path cleared, what is left around each file is
`hash_stage`'s own bookkeeping. It submits one pool task per file — 100,000
futures on `needle` for 100,000 reads of about 2.5 KB each. Queueing a task
should not cost more than the read it carries, but at that size it does.

**Change:** `hash_stage` hands each reader a batch of files and gets back a
list of digests. The work per file is untouched; only the per-file future
disappears. Batch size adapts down on small inputs so a scan of a handful of
files still spreads across the pool instead of collapsing onto one thread.
Verified first in isolation that every batch size produces digest lists
identical to the current code, in identical order:

```text
  per-item futures (current)   3.249s   32.5 us/file   1.00x
  chunked n=16                 2.028s   20.3 us/file   0.62x
  chunked n=64                 1.991s   19.9 us/file   0.61x
  chunked n=256                2.016s   20.2 us/file   0.62x
```

**Numbers:** full suite, seed 1, tree, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.358    0.482     0.74x     35       53
photos       pass  150.8 MB   150.8 MB    1.00x     0.021    0.024     0.88x     148      149
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.033    0.032     1.02x     109      109
needle       pass  244.1 MB   244.1 MB    1.00x     2.950    4.222     0.70x     105      303
samesize     pass  348.4 MB   348.4 MB    1.00x     0.290    1.686     0.17x     284      219
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.052    0.057     0.92x     152      152
```

`--rounds 9` on the three fast scenarios: photos 0.97x, bigheader 0.99x,
widegroup 0.90x — the 1.02x `bigheader` reading above is noise. Seed 2, smoke:
needle 0.53x, typical 0.88x, photos 0.86x, widegroup 0.90x, samesize 0.93x,
bigheader 0.99x, bytes 1.00x throughout.

**Verdict:** KEPT (commit `df8f3f7`). 0.70x on the slowest scenario in the
suite and 0.74x on the most representative one, bytes identical everywhere,
and RSS on `needle` down from 303 MB to 105 MB — the 100,000 `Future` objects
that no longer exist.
