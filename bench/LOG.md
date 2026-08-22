# Optimization log — round one

Both optimizers' journals, merged onto `main`. Branch `opus` first, then
branch `luna` under its own heading at the end.

## Stop summary — 2026-08-22

Six ideas processed. Three kept, two rejected, one rejected and then reopened
and kept when a later change removed the reason it was rejected.

**Cumulative result — branch tip against the pinned baseline `012acc4`, full
suite, seed 1, exit 0, 119 passed:**

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  73.1 MB    108.7 MB    0.67x     0.428    0.467     0.92x     33       51
photos       pass  150.3 MB   150.8 MB    1.00x     0.026    0.027     0.97x     149      146
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.040    0.039     1.03x     109      110
needle       pass  244.1 MB   244.1 MB    1.00x     2.860    4.207     0.68x     108      281
samesize     pass  348.4 MB   348.4 MB    1.00x     0.328    1.916     0.17x     284      219
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.055    0.071     0.78x     152      150
```

**Bytes:** `typical` 0.67x on seed 1 and 0.71x on seed 2 at default scale,
holding at 1.00x on all five others. **Time:** `samesize` 5.8x faster, `needle`
1.47x, `typical` 1.09x here and 0.76x at `--rounds 9`, nothing slower
(`bigheader` 1.03x on a 40 ms scenario is 1.02x at 9 rounds). **RSS:** `needle`
281 MB → 108, `typical` 51 → 33. The `samesize` RSS column swings 219↔284 on
identical code and is noise; the baseline run at the top of this log shows the
same swing.

**Kept**

- `227827e` — deterministic tie-break on the final group sort. Not a
  performance change; it closes the known nondeterminism bug so every
  comparison after it reads against a stable ordering.
- `7b7b77b` — `partition_blocks` hands off to grouping-on-the-block when a
  compare pass strips off nothing but its own pivot. `samesize` 1.627s →
  0.302s, seed 2 5.2x, everything else flat.
- `df8f3f7` — `hash_stage` gives each reader a batch instead of one task per
  file. `needle` 0.70x, `typical` 0.74x, RSS on `needle` down 65%.
- `bf627d9` — the stage-3 head in two tiers: one page for everyone, the
  remaining 60 KiB only for files that page left crowded. `typical` 0.67x
  bytes with discrimination unchanged.

**Rejected, and why**

- A flat smaller `HEAD_BYTES`. Passes every gate and every written rule — 4 KiB
  gives `typical` 0.54x and `samesize` 0.76x with no regression anywhere — and
  is still wrong. Stage-5 fall-through is byte-identical from 64 KiB down to 1
  KiB on these corpora, so the benchmark rewards shrinking the head without
  limit; no scenario contains files sharing a 1–64 KiB prefix, which is the
  case that would charge for it, and real camera raws do exactly that. The
  two-tier head above is the version that captures the win without the risk.
- Unbuffered and raw-fd reads in `hash_head`. 12–15% faster on one thread,
  0.99x at the two readers the tool actually uses, because the bottleneck there
  is the GIL. A cautionary entry more than a result.
- Raising the reader count. `jobs=2` measured optimal on `needle` (1: 4.649s,
  2: 3.174s, 4: 3.333s, 8: 4.169s). Nothing to win.

**One rejection that was wrong, and how it was caught**

The two-tier head was first measured before `hash_stage` was batched, cost
`widegroup` 1.30x, and went to branch `opus-head-twotier` as a cross-scenario
trade §4 says an optimizer may not decide alone. The regression was real; my
explanation of it — an extra `open` and seek per crowded file — was not. It was
per-file future overhead in the second pass, and batching removed it. Re-tested
against the tip, `widegroup` measures 1.01x at `--rounds 9` and the change
landed as `bf627d9`. **A rejection is only valid against the tree it was
measured on**; re-test rejected ideas after anything lands near them.

**Where I would look next**

1. `samesize` and `widegroup` still spend 91.9 MB and 34.0 MB in stage 3 on
   heads that separate nothing, because every file in those buckets shares all
   64 KiB. The two-tier head cannot help there — both tiers are needed. What
   would is noticing *during* the stage that a head is not discriminating.
2. Stage 5 re-reads from offset 0 bytes stage 3 and stage 4 already read. The
   only legal way to exploit that is to keep those bytes and *compare* them
   rather than trust the digest, trading RSS for I/O — worth measuring, since
   `COMPARE_BUDGET` already bounds what a group may hold.
3. Group-level parallelism in stage 5. `typical` reaches it with ~640 groups of
   about 2.4 members, so most of a 2-reader pool sits idle; `bigheader` must
   keep its ability to give one group the whole pool.
4. Not stage-4 sample geometry. `bigheader` is 276.8 MB of stage 5 against 33.6
   MB of stage 4, and its stage 5 is the byte floor — six genuinely identical
   56 MiB files that must be read end to end. There is little there.

**Two notes on the harness, for whoever maintains it**

- `bench/_measure.py` passes `reporter=None` and starts its clock *after* the
  module is imported. So two ideas the briefing offers in §7 — Reporter
  throttle overhead in the hot loops, and lazy-loading `curses` — cannot be
  measured by this harness at all: the throttle branch never executes, and
  import cost lands in `setup_seconds`, which is a 2-second gate and not a
  reported metric. §4's "no unmeasured wins" rule disqualifies both ideas that
  §7 recommends. (The other branch, `luna`, spent an idea confirming the curses
  half empirically and reverted it.)
- §4 tells you to confirm on seed 2 and, to avoid eviction, to use `--scale
  smoke`. For anything touching `partition_blocks` that advice is a trap:
  `samesize` runs 0.036 s at smoke against 1.6–1.9 s at default and does not
  reproduce the behaviour at all. The two pieces of guidance pull against each
  other whenever the volume is tight, and it was 83–98% full throughout.

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

## 2026-08-22 — The two-tier head, revisited: the trade was never a trade

**Hypothesis:** The entry above left the two-tier head unmerged because it cost
`widegroup` 1.30x wall time, and I attributed that to the second pass paying an
extra `open` and seek on 519 files that always need both tiers. That attribution
was wrong, and the ordering of my own work is what exposed it: the two-tier
measurement was taken on the tree *before* `hash_stage` was batched (`df8f3f7`).
If the cost were syscalls it would survive batching; if it were per-file future
overhead in the second pass, batching would absorb it. Worth one measurement.

**Change:** none to the two-tier logic. Rebuilt exactly the same change on top
of the batched `hash_stage` and re-measured.

**Numbers:** it was future overhead. `widegroup` goes from 1.30x to 1.02x at 3
rounds and 1.01x at 9. Full suite, seed 1, tree, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  73.1 MB    108.7 MB    0.67x     0.428    0.467     0.92x     33       51
photos       pass  150.3 MB   150.8 MB    1.00x     0.026    0.027     0.97x     149      146
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.040    0.039     1.03x     109      110
needle       pass  244.1 MB   244.1 MB    1.00x     2.860    4.207     0.68x     108      281
samesize     pass  348.4 MB   348.4 MB    1.00x     0.328    1.916     0.17x     284      219
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.055    0.071     0.78x     152      150
```

`--rounds 9` on the four scenarios worth re-timing: typical 0.76x, photos
0.98x, bigheader 1.02x, widegroup 1.01x. Seed 2, smoke: typical 5.1 MB vs 5.5
MB (0.93x), everything else 1.00x bytes. Seed 2 at default scale on `typical`,
because smoke understates a head change:

```text
typical   pass  86.3 MB    121.8 MB    0.71x     0.354    0.472     0.75x     41       59
```

**Verdict:** KEPT (commit `bf627d9`). Bytes strictly improve on `typical` —
0.67x on seed 1, 0.71x on seed 2 — and hold at 1.00x on every other scenario;
time improves or holds everywhere. Branch `opus-head-twotier` (`b7ef1a3`) is
superseded and left in place only as the record of the rejected trade.

**The lesson, for whoever reads this next:** I measured a cross-scenario trade
correctly, reasoned about its cause plausibly, and got the cause wrong. The
number was real; the story I attached to it was not, and the story is what sent
the change to a branch instead of into the tree. Two independent changes
interacted, and measuring one before the other existed made a clean win look
like a trade. When an optimization is rejected for a regression, it is worth
re-testing it against the tip after anything else lands nearby — a rejection is
only valid against the tree it was measured on.

---

## Round one, branch `luna` — appended verbatim

Two optimizers ran the `GOAL.md` loop against the pinned baseline `012acc4`
at the same time, on separate branches and separate worktrees; the preamble
above has the arrangement. Everything before this line is `opus`. Everything
after it is `luna`'s log, moved here unchanged (its headings demoted one
level to sit under this one) when `main` took `luna`'s `a77e106` and stopped
`luna` being a branch anyone would think to read.

It is kept for the rejections as much as the wins. `luna` measured and threw
away dictionary grouping in `partition_blocks`, lazy-loading `curses`, and a
256 KiB `BULK_BUF` — and the first of those was later reopened on `opus` and
kept, once a guard was found that made it win. Read section 4 of `GOAL.md`
on why a rejection only holds against the tree it was measured on before
re-running any of these.

### Stop summary — 2026-08-22

Three consecutive ideas after the first kept win were tested and rejected, so
the optimization loop stops here.

**Kept:** `a77e106` (`perf: skip stage-3 head during sampling`) removes the
stage-3 64 KiB head reread from the first deep sample. On the full seed-1
measurement it holds bytes on typical, photos, needle, and widegroup, and
reduces bigheader from 296.5 MB to 296.0 MB and samesize from 348.4 MB to
348.2 MB. Its recorded seed-1 time/RSS changes were mixed and below the
decision threshold overall; seed 2 reproduced the byte reductions. The
cumulative kept result is therefore a deterministic byte reduction with no
byte regression, no broad time claim, and mixed RSS movement.

**Rejected:** dictionary grouping for stage-5 blocks made default-scale
samesize 0.13x the baseline but made photos 3.13x, bigheader 3.67x, and
widegroup 1.76x slower; lazy curses loading produced no meaningful time win;
and a 256 KiB bulk-directory buffer produced no smoke-scale time win while
making needle 1.06x slower. All three passed correctness and deletion gates,
were reverted, and are retained below with their measurements.

**Next place to look:** stage-4 sample geometry or adaptive head sizing, then
stage-5 scheduling across many small groups. Flash-specific walk and reader
paths remain unmeasured on this conservative benchmark profile and would need
coverage before being treated as wins.

### 2026-08-21 — Baseline and briefing log filename discrepancy

**Hypothesis:** Establish an unmodified reference before testing optimizations.

**Change:** The briefing requires `bench/LOG.md`, but this checkout contained
only `bench/GOAL.md`; this journal was added to resolve that discrepancy. The
tree was verified on branch `luna`, and the stale factor-1.0 generated corpus
was rebuilt by the harness for the current factor-1.0 run.

**Numbers:**

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.449    0.478     0.94x     53       51
photos       pass  150.8 MB   150.8 MB    1.00x     0.029    0.043     0.68x     145      146
bigheader    pass  296.5 MB   296.5 MB    1.00x     0.031    0.031     1.00x     109      112
needle       pass  244.1 MB   244.1 MB    1.00x     4.042    4.092     0.99x     277      308
samesize     pass  348.4 MB   348.4 MB    1.00x     1.619    1.663     0.97x     219      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.062    0.064     0.97x     152      152
```

**Verdict:** BASELINE — no optimization tested yet; full harness exited 0.

### 2026-08-21 — Skip the stage-3 head in the first deep sample

**Hypothesis:** Stage 3 already reads the first 64 KiB of every candidate, but
stage 4 rereads that region as part of its first 1 MiB sample. Starting that
sample at `HEAD_BYTES` and reading 64 KiB less should reduce disk bytes without
changing the exact stage-5 proof.

**Change:** `hash_sample()` now reads 960 KiB from offset 64 KiB for its first
deep point, keeps the other three 1 MiB points unchanged, and subtracts the
saved 64 KiB from `sampled_bytes()`.

**Numbers:** Smoke seed 1 passed all gates; `bigheader` was 112.0 MB current vs
112.5 MB baseline and `samesize` was 75.8 MB vs 75.9 MB. Full seed 1:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.463    0.456     1.02x     53       52
photos       pass  150.8 MB   150.8 MB    1.00x     0.024    0.024     0.97x     149      149
bigheader    pass  296.0 MB   296.5 MB    1.00x     0.036    0.042     0.86x     112      109
needle       pass  244.1 MB   244.1 MB    1.00x     4.095    4.140     0.99x     312      301
samesize     pass  348.2 MB   348.4 MB    1.00x     1.695    1.683     1.01x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.056    0.055     1.01x     152      153
```

Full seed 2:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  121.8 MB   121.8 MB    1.00x     0.539    0.544     0.99x     57       57
photos       pass  152.0 MB   152.0 MB    1.00x     0.024    0.025     0.96x     159      158
bigheader    pass  296.0 MB   296.5 MB    1.00x     0.032    0.035     0.92x     112      109
needle       pass  244.2 MB   244.2 MB    1.00x     4.194    4.340     0.97x     312      301
samesize     pass  348.5 MB   348.6 MB    1.00x     1.724    1.728     1.00x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.057    0.055     1.03x     152      152
```

**Verdict:** KEPT (commit `a77e106`); all gates passed, bytes strictly improved
or held on every scenario, and the reduction reproduced on seed 2.

### 2026-08-21 — Dictionary grouping for stage-5 block partitions

**Hypothesis:** The `samesize` corpus sends about 1,400 members through the
pivot-based `partition_blocks()` loop. Grouping by the complete block bytes in
a dictionary should avoid its O(n²) comparisons while remaining exact because
dictionary equality verifies byte strings after hashing.

**Change:** Replaced the pivot/rest partition loop with dictionary grouping in
the WIP copy only. No repository code from this attempt was retained.

**Numbers:** Smoke seed 1 passed all gates:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  7.2 MB     7.2 MB      1.00x     0.031    0.029     1.05x     25       25
photos       pass  33.5 MB    33.5 MB     1.00x     0.020    0.008     2.55x     50       50
bigheader    pass  112.0 MB   112.5 MB    1.00x     0.057    0.022     2.60x     104      102
needle       pass  23.4 MB    23.4 MB     1.00x     0.333    0.353     0.95x     46       46
samesize     pass  75.8 MB    75.9 MB     1.00x     0.046    0.034     1.34x     84       84
widegroup    pass  18.1 MB    18.1 MB     1.00x     0.017    0.012     1.39x     31       31
```

The targeted default-scale `samesize` run passed with 348.2 MB current vs
348.4 MB baseline, 0.215 seconds current vs 1.663 seconds baseline, and 278 MB
current vs 220 MB baseline RSS. The complete default seed-1 run passed all
gates:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  108.7 MB   108.7 MB    1.00x     0.489    0.485     1.01x     52       51
photos       pass  150.8 MB   150.8 MB    1.00x     0.078    0.025     3.13x     146      146
bigheader    pass  296.0 MB   296.5 MB    1.00x     0.123    0.034     3.67x     112      108
needle       pass  244.1 MB   244.1 MB    1.00x     4.047    4.097     0.99x     305      304
samesize     pass  348.2 MB   348.4 MB    1.00x     0.221    1.723     0.13x     284      284
widegroup    pass  158.8 MB   158.8 MB    1.00x     0.103    0.058     1.76x     152      152
```

**Verdict:** REVERTED — the targeted `samesize` win was outweighed by large
time regressions on `photos`, `bigheader`, and `widegroup`, and there was no
new byte reduction beyond the already-kept stage-3 head skip.

### 2026-08-22 — Lazy-load curses for noninteractive scans

**Hypothesis:** Every benchmark subprocess imports `curses` even though the
noninteractive harness path never opens the browser. Deferring that import
until immediately before `curses.wrapper()` might reduce startup time without
changing scan behavior or bytes.

**Change:** Removed the module-level import and loaded `curses` only in the
interactive branch in the WIP copy. No repository code from this attempt was
retained.

**Numbers:** Smoke seed 1 passed all gates:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  7.2 MB     7.2 MB      1.00x     0.028    0.029     1.00x     25       25
photos       pass  33.5 MB    33.5 MB     1.00x     0.008    0.008     1.00x     50       50
bigheader    pass  112.0 MB   112.5 MB    1.00x     0.018    0.017     1.03x     104      101
needle       pass  23.4 MB    23.4 MB     1.00x     0.326    0.316     1.03x     46       46
samesize     pass  75.8 MB    75.9 MB     1.00x     0.032    0.032     1.02x     84       84
widegroup    pass  18.1 MB    18.1 MB     1.00x     0.011    0.011     1.04x     31       31
```

**Verdict:** REVERTED — no scenario produced a meaningful time win and bytes
held on every scenario.

### 2026-08-22 — Double the bulk directory metadata buffer

**Hypothesis:** The macOS `getattrlistbulk` walker uses a 128 KiB buffer. A
256 KiB buffer might reduce metadata syscalls for the 100k-file `needle`
scenario without changing file contents or byte accounting.

**Change:** Changed `BULK_BUF` to 256 KiB in the WIP copy only. No repository
code from this attempt was retained.

**Numbers:** Smoke seed 1 passed all gates:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  7.2 MB     7.2 MB      1.00x     0.029    0.029     1.00x     25       25
photos       pass  33.5 MB    33.5 MB     1.00x     0.008    0.008     0.96x     51       50
bigheader    pass  112.0 MB   112.5 MB    1.00x     0.018    0.018     1.01x     104      101
needle       pass  23.4 MB    23.4 MB     1.00x     0.330    0.311     1.06x     46       46
samesize     pass  75.8 MB    75.9 MB     1.00x     0.034    0.034     0.99x     86       83
widegroup    pass  18.1 MB    18.1 MB     1.00x     0.011    0.011     1.02x     31       31
```

**Verdict:** REVERTED — no scenario produced a meaningful time win and bytes
held on every scenario.

---

# Round two — branch `opus-round2`

Target: `SCORE ≤ 0.50` at seed 1 and seed 2. Starting point, measured on this
branch before any change (`python3 bench/run.py --score`, unmodified tree at
`main` `0661a5e`): **SCORE: 0.8522**, exit 0, 119 passed.

## Summary — 2026-08-22

**Target met, and checked from outside.** Five changes kept, two ideas
measured and rejected, two more declined with reasons. Every run below exited
0 with 119 tests passing and no scenario's bytes regressed.

| `--score` run | seed 1 | seed 2 |
|---|---|---|
| in this tree | 0.4820 | 0.4913 |
| **referee** — clean `bench/` from `main` (`81fa031`), `--dedup` this branch's `dedup.py` | **0.4790** | **0.4845** |

Starting point on this branch, unmodified: **0.8522**.

**Cumulative, seed 1, against the pinned baseline `012acc4`:**

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.286    0.467     0.61x     103      53
photos       pass  147.9 MB   150.8 MB    0.98x     0.018    0.022     0.83x     64       149
bigheader    pass  146.2 MB   296.5 MB    0.49x     0.016    0.030     0.52x     97       109
needle       pass  244.1 MB   244.1 MB    1.00x     2.725    3.929     0.69x     110      303
samesize     pass  32.8 MB    348.4 MB    0.09x     0.047    1.565     0.03x     57       284
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.057    0.054     1.06x     161      152
prefixtrap   pass  33.4 MB    44.2 MB     0.75x     0.012    0.014     0.83x     24       50
adversarial  pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
SCORE: 0.4820
```

**One idea, applied four times.** Every kept change is the same sentence: *do
not read a byte twice, and probe where files actually differ before reading
more of where they agree.*

- `d9e260c` — keep the head stage 3 reads and **compare** those bytes in stage
  5 instead of reading them again. A digest may narrow, never prove, so the
  bytes themselves are carried. `typical` 0.67x → 0.48x, `widegroup` 1.00x →
  0.80x, which is its floor to the byte.
- `ac69219` — probe one page of the **tail** before extending the head. Files
  of one size that survive a page of header usually share that header;
  reading more of it settles nothing. `samesize` 348.4 → 32.8 MB, `bigheader`
  296.5 → 160.1 MB.
- `ee3d8c2` — take **both end pages in one open**. `open` is 22 µs on this
  volume against ~10 for a page read, so a second pass for the tail was
  costing more than it read. Cost 0.4 MB of bytes across two unscored
  scenarios; bought 1,900 opens.
- `0a24651` — **eight 64 KiB sample windows** instead of four of a megabyte.
  A window separates two files only when it lands on something they do not
  share, so a sample buys coverage of the file's length, not weight.
  `bigheader` 160.1 → 146.2 MB.
- `aed953d` — do not open a file whose bytes are already in hand. `typical`
  0.77x → 0.63x wall.

**How close to the floor this leaves things.** The harness's own floor — Σ
size × members over the true groups — beside what the tool now reads, seed 1:

```text
scenario     floor      reads      what the gap is
typical      24.2 MB    52.5 MB    one 4 KiB page for each of ~7,500 candidates
                                   that turn out to be nothing
photos      155.0 MB   147.9 MB    at the floor (the two disagree because they
                                   count partial reads differently)
bigheader   117.4 MB   146.2 MB    the blind pair, which must be read to the
                                   byte where it differs, plus 2 MB of sample
needle        0.3 MB   244.1 MB    the real floor, not the manifest's: 100,000
                                   files of 1-4 KB, one page each, and a page
                                   is the smallest thing a disk can return
samesize     24.1 MB    32.8 MB    two pages for each of 1,402 candidates
widegroup   132.5 MB   126.3 MB    at the floor: every file is a true copy and
                                   is now read exactly once
```

There is no large bytes win left in this pipeline. What remains is wall time,
and the two biggest levers are both blocked — see "Two ideas not attempted"
at the end of this log.

**Where I would look next.** (1) The 100,000 `open` calls in `needle`: 22 µs
each on this volume and 2.2 s of CPU, unreachable without either
multiprocessing or a harness that can count bytes read in a child. (2)
`widegroup` is the one scenario slower than baseline, 1.06x, and the reason
is structural: it now compares 33 MB of kept head bytes in memory instead of
reading 34 MB off a RAM disk, which is a wash here and a large win on a real
device. (3) Retaining the sample windows the way the head and tail are
retained would give `bigheader` back another ~10 MB, and needs `split_identical`
to compare around holes rather than around one prefix and one suffix.

**Where the bytes actually are.** Before touching anything I instrumented the
pipeline per stage against the default corpora and put the harness's own byte
floor (Σ size × members over the manifest's true groups) beside it. Every
number below is MB = 10⁶ bytes, self-reported `hashed_bytes`, seed 1:

```text
scenario   cand    tier-1  tier-2  sample  compare   total    FLOOR
typical    9071      37.2    15.3       –     24.2     76.6     24.2
photos       47       0.2     2.4       –    155.0    157.6    155.0
bigheader     8      0.03     0.5    33.5    276.8    310.9    117.4
needle   100000     255.6       –       –      0.3    255.9      0.3
samesize   1402       5.7    86.1     8.4    265.0    365.3     23.6
widegroup   519       2.1    31.9       –    132.5    166.5    132.5
prefixtrap  196       0.8     9.3       –     33.6     43.6     33.6
```

Two things fall out of that table. `photos` and `widegroup` are already at or
near the floor and have nothing to give. `needle` reads 255.6 MB as one page
per file over 100,000 files of 1–4 KB — a page is the smallest thing a disk
can hand back, so its bytes are at the floor too even though the manifest
floor says 0.3 MB. **Shrinking the first-page read below 4 KiB would look like
an enormous win here and would not save a single real byte of I/O on any
device; it is not on the table.** What is on the table is `samesize`,
`bigheader` and `typical`, where the tool reads regions twice or reads far
past the point where a file has been told apart from its bucket.

## 2026-08-22 — Compare the head stage 3 already read instead of reading it again

**Hypothesis:** Stage 5 opens at offset 0 and re-reads every byte stages 3 and
4 have already read. Round one's log flags this and names the only legal way to
exploit it: keep the bytes and *compare* them, since a digest may narrow but
may never prove. Keeping the head should remove that re-read wherever a file is
read to EOF — which is exactly the true duplicates, i.e. the floor.

**Change:** `keep_head` / `keep_head_rest` return the block alongside the
digest; `hash_stage(..., keep=True)` appends it to `FileRec.head`, so a record
carries one contiguous run from offset 0. `split_identical` compares those
bytes as its first round and seeks past them, after an `fstat` per member
confirming size and mtime still match what was read — a file that moved under
us goes back to reading from zero. `release()` drops kept bytes for records no
later stage will look at. Retention is skipped entirely when the projected
cost exceeds `RETAIN_BUDGET` (128 MB), which is what keeps `needle`'s 100,000
files from being held in memory for a 0.3 MB gain.

**Numbers:** full suite, seed 1, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.338    0.467     0.72x     106      53
photos       pass  147.9 MB   150.8 MB    0.98x     0.022    0.022     0.99x     148      149
bigheader    pass  296.2 MB   296.5 MB    1.00x     0.032    0.033     0.98x     119      109
needle       pass  244.1 MB   244.1 MB    1.00x     2.837    4.064     0.70x     104      283
samesize     pass  341.4 MB   348.4 MB    0.98x     0.220    1.683     0.13x     342      219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.060    0.055     1.08x     175      152
prefixtrap   pass  40.6 MB    44.2 MB     0.92x     0.015    0.016     0.97x     36       51
adversarial  pass  1.2 MB     4.2 MB      0.30x     -        -         -         -        -
SCORE: 0.7791
```

Seed 2, default scale, exit 0:

```text
typical      pass  64.8 MB    121.8 MB    0.53x     0.336    0.467     0.72x     111      59
photos       pass  149.1 MB   152.0 MB    0.98x     0.023    0.023     1.02x     155      156
bigheader    pass  296.2 MB   296.5 MB    1.00x     0.032    0.032     1.02x     119      109
needle       pass  244.2 MB   244.2 MB    1.00x     2.780    4.188     0.66x     106      304
samesize     pass  341.5 MB   348.6 MB    0.98x     0.280    1.968     0.14x     328      219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.068    0.063     1.09x     173      152
prefixtrap   pass  40.6 MB    44.2 MB     0.92x     0.015    0.015     1.00x     36       50
adversarial  pass  1.2 MB     4.2 MB      0.30x     -        -         -         -        -
SCORE: 0.7919
```

`widegroup` lands on 126.3 MB, which is its floor to the byte: every file in
that corpus is a true copy, so once the head is not read twice there is
nothing left to remove. The saving only ever appears where a member is read to
EOF — a file settled by its first compare round reads one chunk either way,
just starting later — which is why `samesize` moves 0.98x here and not more.
That is the next entry's problem.

**Verdict:** KEPT. Bytes improve on five scenarios and hold on two, no
regression anywhere, 0.8522 → 0.7791 (seed 1) and 0.7919 (seed 2). RSS rises
where bytes are kept (typical 53→106, samesize 219→342): metric 3 paying for
metric 1, which is the order the brief asks for.

## 2026-08-22 — One page from the far end, asked before more of the head

**Hypothesis:** Stage 3's answer to "these two files of one size are still
together" is to read more of the head. But the ordinary reason two same-size
files are still together is that they share a header — one camera, one
encoder, one container format — and reading more of a header they agree on
settles nothing. The page at the *other* end is where they part. On
`samesize` that is 86 MB of deep head buying nothing; one 4 KiB page should
settle almost the whole corpus.

**Change:** `hash_tail` / `keep_tail` read the last `TAIL_BYTES` (4 KiB), as a
stage between the two head tiers, for buckets whose size is over
`DEEP_HEAD + TAIL_BYTES` so the two kept runs cannot overlap. The block is
kept the same way the head is; `split_identical` reads up to where the kept
tail begins and lets the tail settle the last round, so a file read to EOF
pays for that page once, not twice.

**Numbers:** full suite, seed 1, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.341    0.471     0.72x     103      53
photos       pass  147.9 MB   150.8 MB    0.98x     0.019    0.026     0.76x     64       147
bigheader    pass  160.1 MB   296.5 MB    0.54x     0.020    0.032     0.62x     101      109
needle       pass  244.1 MB   244.1 MB    1.00x     2.746    4.077     0.67x     108      305
samesize     pass  32.8 MB    348.4 MB    0.09x     0.055    1.689     0.03x     57       219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.061    0.055     1.11x     161      152
prefixtrap   pass  33.2 MB    44.2 MB     0.75x     0.012    0.015     0.82x     24       50
adversarial  pass  1.2 MB     4.2 MB      0.30x     -        -         -         -        -
SCORE: 0.4961
```

Seed 2, default scale, exit 0: typical 0.53x, photos 0.98x, bigheader 0.54x,
needle 1.00x, samesize 0.10x, widegroup 0.80x, prefixtrap 0.75x, adversarial
0.30x, `SCORE: 0.5064`.

Per-stage, seed 1: `samesize` 365.6 → 34.8 MB against a floor of 24.1 — the
deep head drops from 86.1 MB to 7 MB and the compare from 265.0 to 17.7,
because 1,300 files that share their whole 64 KiB head do not share their last
page. `bigheader` 310.9 → 167.9: its `lastbyte` pair differs in the final byte
and used to cost two 60 MiB reads to prove it. `widegroup` stays at 126.3 MB —
the probe costs 519 pages and the kept tail gives all 519 back, so a corpus
that is nothing but true copies pays nothing for it.

**Verdict:** KEPT. Bytes improve on six scenarios and hold on one, no
regression anywhere, 0.7791 → 0.4961 (seed 1) / 0.5064 (seed 2). `widegroup`
wall time is 1.11x/1.15x: 519 files that are all true copies pay one extra
`open` each for a probe that cannot settle them, and on this volume an `open`
is 22 µs against a 4 KiB read that is nearly free. That is the next entry.

## 2026-08-22 — Both end pages in one open

**Hypothesis:** The entry above cost `widegroup` 1.11x/1.15x wall time, and the
mechanism is measurable rather than guessable. On this volume, over the real
`needle` corpus: `os.lstat` 1.7 µs/file, `open`+`close` **22.1 µs**, and
`open`+`read(4096)`+`close` 32.8. The read is not what a probe costs — the
`open` is. So a second pass over the same files to fetch their tail page costs
one open each and cannot be worth it: fetch the far page in the *same* open as
the head page, with a seek in between.

**Change:** `hash_ends` / `keep_ends` replace the separate head and tail
stages with a single pass that reads the head page and, when
`size > DEEP_HEAD + TAIL_BYTES`, seeks and reads the tail page, hashing both
into one digest. `hash_stage`'s `keep` becomes a tuple of attribute names so
one reader can hand back both blocks.

**Numbers:** full suite, seed 1, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.343    0.463     0.74x     103      49
photos       pass  147.9 MB   150.8 MB    0.98x     0.019    0.024     0.83x     64       149
bigheader    pass  160.1 MB   296.5 MB    0.54x     0.021    0.035     0.59x     103      104
needle       pass  244.1 MB   244.1 MB    1.00x     2.754    4.073     0.68x     104      303
samesize     pass  32.8 MB    348.4 MB    0.09x     0.048    1.706     0.03x     57       219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.060    0.057     1.05x     161      152
prefixtrap   pass  33.4 MB    44.2 MB     0.75x     0.012    0.015     0.80x     24       50
adversarial  pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
SCORE: 0.4925
```

Seed 2, default scale, exit 0: typical 0.53x, photos 0.98x, bigheader 0.54x,
needle 1.00x, samesize 0.10x, widegroup 0.80x, prefixtrap 0.75x, adversarial
0.34x, `SCORE: 0.5037`.

Interleaved A/B on the corpora directly, two passes each, min of three runs:
`samesize` 0.045 → 0.035 s, `widegroup` 0.049 → 0.045 s, `photos` flat.

**The cost, stated plainly.** A file that the head page alone would have
settled now reads a tail page it does not need. That is +0.2 MB on
`prefixtrap` (33.2 → 33.4) and +0.2 MB on `adversarial` (1.2 → 1.4), both far
under baseline, and nothing measurable elsewhere — the scenarios where most
candidates are settled by their first page are the ones whose files are too
short to have a tail page at all. Bytes bought time here, which is the wrong
way round for this brief; it is 0.4 MB against 1,900 fewer `open` calls, both
seeds agree, and no scenario moved toward its baseline.

**Verdict:** KEPT. 0.4925 (seed 1) / 0.5037 (seed 2), `widegroup` 1.11x →
1.05x, `samesize` 0.055 → 0.048 s.

## 2026-08-22 — Sample points, not sample megabytes

**Hypothesis:** Stage 4 reads 4 MB of every big candidate — four windows of a
megabyte. But a window separates two files only when it lands on something
they do not share, so what a sample buys is *coverage of the file's length*,
and that is decided by how many windows there are and how far apart they sit,
not by how fat each one is. A difference small enough to fall between windows
is missed at 1 MB a window just as it is missed at 64 KiB; stage 5 is what
catches that. Eight windows of 64 KiB should therefore separate everything
four megabyte windows separate, for an eighth of the reading.

**Change:** `SAMPLE_BYTES` 1 MiB → 64 KiB, `SAMPLE_POINTS` 4 → 8, and
`SAMPLE_SPAN` pinned at 8 MB as its own number instead of being derived from
those two — the threshold is "is this file worth an open of its own", which
has not changed. `sample_points()` now spreads the windows over
`[DEEP_HEAD, size - TAIL_BYTES)`: every file that reaches stage 4 has been
through both end probes, so the sample no longer re-reads the 60 KiB of deep
head it used to overlap. The window read also moved from `fh.read()` to
`read_block()`; an unbuffered read is allowed to come back short, and two
identical files that got different amounts back would hash differently and be
missed — the same hole `read_block` exists to close in stage 5.

**Numbers:** full suite, seed 1, exit 0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.345    0.448     0.77x     103      53
photos       pass  147.9 MB   150.8 MB    0.98x     0.018    0.024     0.78x     64       149
bigheader    pass  146.2 MB   296.5 MB    0.49x     0.016    0.032     0.52x     97       109
needle       pass  244.1 MB   244.1 MB    1.00x     2.739    3.953     0.69x     101      246
samesize     pass  32.8 MB    348.4 MB    0.09x     0.045    1.586     0.03x     57       284
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.058    0.053     1.09x     161      152
prefixtrap   pass  33.4 MB    44.2 MB     0.75x     0.011    0.015     0.76x     24       50
adversarial  pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
SCORE: 0.4851
```

Seed 2, default scale, exit 0: typical 0.53x, photos 0.98x, bigheader 0.49x,
needle 1.00x, samesize 0.10x, widegroup 0.80x, prefixtrap 0.75x, adversarial
0.34x, `SCORE: 0.4955`.

Only `bigheader` has files over `SAMPLE_SPAN`, and it moves 160.1 → 146.2 MB
with wall time 0.021 → 0.016 s. Its four sampled files are the true pair and
the `blind` pair, and sampling separates neither — the true pair because they
are identical and the `blind` pair by construction — so on this corpus the
change is 14 MB of pure waste removed. What it does *not* do is give up the
case sampling exists for: `bigheader`'s `prefix` pair shares three quarters of
itself and then diverges, and windows five through eight still land in the
divergent part.

**Verdict:** KEPT. 0.4925 → 0.4851 (seed 1) and 0.5037 → 0.4955 (seed 2), no
bytes regression anywhere. **Both seeds are now under the 0.50 target.**

## 2026-08-22 — Reader count, re-tested against this tree (rejected)

**Hypothesis:** Round one measured `read_jobs()` = 2 as optimal on `needle`,
but that was before `hash_stage` was batched and before stage 3 grew a second
read per file. A rejection is only valid against the tree it was measured on,
and a micro-benchmark over the real corpus said 3 threads beat 2 (16.8 vs 18.8
µs/file) for open+read+digest.

**Change:** none kept. `jobs` overridden per run over the real corpora, min of
three runs each:

```text
needle      j=2 3.157s (1.00x)  j=3 3.147s (1.00x)  j=4 3.929s (1.24x)  j=6 4.532s (1.44x)
photos      j=2 0.016s (1.00x)  j=3 0.019s (1.14x)  j=4 0.018s (1.10x)  j=6 0.018s (1.10x)
samesize    j=2 0.047s (1.00x)  j=3 0.067s (1.43x)  j=4 0.069s (1.47x)  j=6 0.091s (1.94x)
typical     j=2 0.350s (1.00x)  j=3 0.362s (1.03x)  j=4 0.533s (1.52x)  j=6 0.618s (1.77x)
widegroup   j=2 0.057s (1.00x)  j=3 0.055s (0.96x)  j=4 0.049s (0.86x)  j=6 0.072s (1.26x)
```

**Verdict:** REJECTED, and the round-one number stands on the new tree. The
micro-benchmark was measuring one stage in isolation; the whole pipeline says
2 — the only scenario that wants more readers is `widegroup`, whose compare
stage is one group of 519 members, and it is 0.05 s. The mechanism, so the
next person does not have to re-run it: the extra thread wins in stage 3 where
every task is a syscall that drops the GIL, and loses more than that in the
compare stage, where `partition_blocks` holds the GIL for a whole block.

## 2026-08-22 — Do not open a file whose bytes are already in hand

**Hypothesis:** Once the head is kept, a group of short files arrives at stage
5 with every byte already in memory — `typical` sends 1,533 files there, all
of them under `DEEP_HEAD`. The stage still opens each one, seeks to EOF and
reads nothing. On this volume that open is 22 µs and the `stat` that says the
file has not changed is 1.7.

**Change:** `split_identical` settles a group without opening anything when
every member's kept head covers its whole length, after `unchanged()` confirms
size and mtime still match what was read.

**Numbers:** interleaved A/B over the `typical` corpus, two passes, min of
three runs each: 0.306 / 0.311 s → 0.269 / 0.271 s. Full suite, seed 1, exit
0, 119 passed:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.289    0.460     0.63x     103      52
photos       pass  147.9 MB   150.8 MB    0.98x     0.018    0.024     0.78x     64       149
bigheader    pass  146.2 MB   296.5 MB    0.49x     0.017    0.031     0.53x     97       109
needle       pass  244.1 MB   244.1 MB    1.00x     2.740    4.122     0.66x     106      304
samesize     pass  32.8 MB    348.4 MB    0.09x     0.046    1.553     0.03x     57       219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.058    0.055     1.05x     161      152
prefixtrap   pass  33.4 MB    44.2 MB     0.75x     0.011    0.015     0.77x     24       50
adversarial  pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
SCORE: 0.4802
```

Seed 2, default scale, exit 0, same bytes as the entry above on every
scenario, `typical` 0.63x wall: `SCORE: 0.4903`.

**Verdict:** KEPT. Bytes identical — this removes syscalls, not reads —
`typical` 0.77x → 0.63x, 0.4851 → 0.4802 (seed 1) and 0.4955 → 0.4903 (seed 2).

## 2026-08-22 — Unbuffered reads for the end pages (rejected)

**Hypothesis:** `hash_ends` opens buffered, and a buffered `read(4096)` does
not ask the device for 4096. Instrumenting the raw layer under
`io.BufferedReader` shows what it really asks for:

```text
buffered   open().read(4096)   -> raw reads: [8192]
buffered   head page + tail page -> raw reads: [8192, 8192]
```

Every candidate at least 8 KiB long therefore costs twice the device read its
head page needs. `hash_sample` and `split_identical` already open with
`buffering=0`; stage 3 was the one path that did not.

**Change:** `buffering=0` in `hash_ends` / `keep_ends`, with `read_block` around
both reads — an unbuffered read may come back short, and two identical files
that got different amounts back would hash differently and be missed.

**Numbers:** interleaved A/B over the corpora, two passes, min of three runs:
`needle` 2.644 / 2.616 → 2.599 / 2.565 s, `typical` 0.265 / 0.270 → 0.261 /
0.262 s — a consistent 2%. The full scored run disagreed: `SCORE: 0.4811`
against 0.4802 for the same tree without it, which is a wash inside the ~0.3%
the score wobbles by.

**Verdict:** REVERTED. Rule 3 puts the floor for a time win at 5% and this is
2% at best on a micro-benchmark and 0% on the harness. **The reason it looks
like nothing here is worth writing down:** the harness counts bytes by
wrapping `open()` and adding up what `.read()` returns, so 4096 is counted
whether the device was asked for 4096 or 8192, and the RAM disk charges
almost nothing for the difference. On a slow disk this is 4 KiB of real I/O
per candidate — 37 MB on `typical` alone — that no number in this benchmark
can see. Left out because the rules say a change is kept on a measurement,
and recorded here because the effect is real and the next harness may be able
to see it.

## 2026-08-22 — Two ideas not attempted, and why

Neither is a measurement, so neither is an entry above; both are places
someone will otherwise go.

**Reads in child processes are invisible to this harness.** `bench/_measure.py`
wraps `builtins.open` *in the measuring process* and counts what `.read()`
returns there. Anything read in a `multiprocessing` child — or through
`os.open`/`os.read`, or `io.FileIO` — is counted as zero, so the byte-honesty
check and the byte floor both fail the run. Stage 3 is GIL-bound at two reader
threads and multiprocessing is the obvious way out of that; it is not
available while the measurement works this way. `needle` spends 2.7 s doing
100,000 opens at 22 µs each, and that is the largest single number left in
the suite.

**Overlapping the walk with stage 3.** A size bucket is known to be a
candidate the moment it holds two files, so heads could be read while the walk
is still running, with no wasted read — worth up to 0.5 s of `needle` and
0.09 s of `typical`. It is not taken because `find_duplicates` sorts
candidates by `(dev, ino)` before reading them, and streaming gives that up:
the read order becomes walk order. On this RAM disk that costs nothing and
the benchmark would show a clean win, which is exactly the problem — the seek
cost it trades away is invisible here and real on the disks this tool exists
for.

## 2026-08-22 — Final verification of the branch tip

Run after merging `main` (its `bench/ROUND2.md` note) into this branch, so the
numbers below are the tip and the harness is byte-identical to `main`'s.

```text
python3 -m pytest tests/ -q              119 passed
python3 bench/run.py --selftest          selftest passed
                                         (control clean; truncate-compare caught;
                                          underreport-bytes caught at 50% against
                                          a 20% tolerance)
python3 bench/run.py --score             SCORE: 0.4822   exit 0
python3 bench/run.py --score --seed 2    SCORE: 0.4875   exit 0
```

Referee, on the tip's `dedup.py`, from a clean checkout of `main` (`81fa031`):

```text
git worktree add --detach /tmp/dedup-referee main
python3 bench/run.py --dedup .../dedup.py --score            SCORE: 0.4790
python3 bench/run.py --dedup .../dedup.py --score --seed 2    SCORE: 0.4845
```

The `--selftest` line is worth more here than it usually is. This branch moved
the proof of a duplicate out of stage 5's reads and into bytes kept from stage
3, so the question "would this harness still notice if the proof stopped
happening" is a live one rather than a formality. It notices: a compare stage
truncated to skip its last round is caught as five wrong groups, and a tool
under-reporting its own bytes by half is caught against the 20% tolerance.

---

# Integration — round two, the cache, and extension filtering onto `main`

This is not an optimization entry. It is the record of merging three branches
into `main`, what fought with what, and two things that came out of it that
somebody has to decide.

## 2026-08-22 — Merging `opus-round2`, `rescan-cache` and `ext-filter`

**What went in, in order:**

1. `opus-round2` (`07b7e05`) — the round-two winner. Merged clean; it branched
   from the current `main`.
2. `rescan-cache` (`fb81ee7`) — the persistent digest cache. Real conflicts,
   below.
3. `ext-filter` (`1a245fb`) — `--ext`/`--skip-ext` in both walkers. Small
   conflicts, all in signatures the cache had already widened.

**Why `rescan-cache` fought.** It branched from `93d7c11`, which predates the
round-one merge entirely. It had never seen the two-tier head, the batched
`hash_stage`, the `partition_blocks` fan-out guard or the group-order
tie-break, let alone round two. Every one of those had to win its conflict,
with the cache layered on afterwards rather than merged alongside.

**Two correctness bugs the textual merge produced.** Both would have compiled,
passed a casual reading, and been wrong:

- `split_identical` learned its whole-file digest from a hasher fed only what
  the call actually read — and round two makes that call seek past the kept
  head and stop before the kept tail. The digest of a file's *middle* would
  have been stored in the `full` column, and the next scan would have matched
  two files that agree only in the middle. Fixed by feeding the kept bytes into
  the hasher in their place: head first, then the streamed region, then the
  tail as each run finishes. Where that cannot be done the digest is not
  stored.
- `hash_stage` appended each stage's kept block to the record's run without
  checking that it *continued* that run. A file the cache answered at stage 3
  was never read and carries nothing, so the deep head's block would have been
  glued to the front of the gap, leaving `head` claiming to start at offset 0
  while starting 4 KiB in — and stage 5 compares that against another file's
  real prefix. Two files differing only in their first page could have been
  called duplicates. `keep` now pairs each attribute with the length its run
  must already have, and a block that does not continue the run is dropped.

Neither is hypothetical: the second is reached by an ordinary sequence — a file
settles alone at stage 3 on one scan, a new file with the same end pages
arrives on the next, and the old file hits the `ends` column while missing
`deep`.

**Remapping the cache onto the new stages.** `CACHED_FIELD` keyed on
`hash_head` and `hash_sample`; the pipeline calls neither of those at stage 3
any more. It now keys on `hash_ends`/`keep_ends` (a new `ends` column), on
`hash_head_rest`/`keep_head_rest` (a new `deep` column), and on `hash_sample`.
`hash_head` is deliberately absent: no stage calls it, and a digest of the
first page is not a digest of the two end pages. `CACHE_SCHEMA` went 1 → 2, so
every database written before this is dropped and rebuilt cold — a version-1
row's digests were taken over different bytes than the columns they sit in now
name. A group that settles from bytes already in hand (round two's zero-open
path) also learns its whole-file digest now, which is what lets a warm scan of
short files attest anything at all.

`CACHE_FREE` arrived unused, a list beside a string comparison that did the
real work. It selects the gate now, so the law is expressed once.

**luna's `ffefb02` — superseded, not applied.** It moved stage 4's first window
past `DEEP_HEAD`. Round two's `sample_points()` already spreads all eight
windows over `[DEEP_HEAD, size - TAIL_BYTES)`, which is strictly more, and
applying `ffefb02` on top would now be a bug: it subtracts `DEEP_HEAD` from the
first window's length, `SAMPLE_BYTES` is 64 KiB and `DEEP_HEAD` is 64 KiB, so
the first window would read nothing and `sampled_bytes` would under-report by
64 KiB. Superseded by the round-two restructure, recorded here by the
integration.

**Numbers.** 166 passed — 119 from `main`, 23 from the cache, 24 from
`ext-filter`, none dropped or weakened. Full suite, in-tree, seed 1, exit 0:

```text
scenario     gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical      pass  52.5 MB    108.7 MB    0.48x     0.291    0.449     0.65x     104      53
photos       pass  147.9 MB   150.8 MB    0.98x     0.019    0.023     0.83x     65       149
bigheader    pass  146.2 MB   296.5 MB    0.49x     0.016    0.031     0.52x     98       103
needle       pass  244.1 MB   244.1 MB    1.00x     2.843    4.151     0.69x     115      305
samesize     pass  32.8 MB    348.4 MB    0.09x     0.048    1.577     0.03x     57       219
widegroup    pass  126.3 MB   158.8 MB    0.80x     0.059    0.054     1.10x     162      152
prefixtrap   pass  33.4 MB    44.2 MB     0.75x     0.012    0.015     0.80x     25       51
adversarial  pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
SCORE: 0.4842
```

Seed 2, default scale, exit 0: `SCORE: 0.4901`, and `prefixtrap` measured
separately at seed 2 reads 33.4 MB against 44.2 MB (0.75x), no regression.
Bytes on every scored scenario are identical to round two's, to the byte, which
is the thing that had to be true: the cache is off in all of them and it moved
nothing.

**Verdict:** MERGED. `opus-round2` at `8f25e79`, `rescan-cache` at `c31b5ab`,
`ext-filter` at `aa0f611`.

## 2026-08-22 — Two things the merge exposed, neither of them fixed here

Logged rather than repaired, per section 9. Both need a decision.

**1. The ten corpora no longer fit on the RAM disk.** Round one added
`prefixtrap` (412 MB estimated at default scale) and `rescan-cache` added
`rescan` (430 MB). Neither branch could see the other. Together the ten
scenarios estimate 3171 MB against the 2816 MB `ram_disk_mb()` creates — a
355 MB shortfall before the 1.4x headroom `ensure_space` wants for the corpus
being built. A full default-scale run now dies:

```text
$ python3 bench/run.py --score
bench: /Volumes/dedupbench needs 574.1 MB free, has 118.3 MB
```

Exit 2, an environment error rather than a gate failure, and `ensure_space`
cannot help because every corpus it might evict is one this run needs. The
scores above were taken by naming the nine scenarios that do fit; the six
scored terms are unaffected and remain comparable with every score before them.
`rescan` was verified separately with `--selftest`. Raising the RAM disk is the
obvious repair and it is not mine to make: section 2 warns that an oversized
RAM disk pushes this 16 GB machine into swap, onto a boot disk that is 94%
full, and section 5 says not to edit `bench/` to make a failing check pass.

**2. `rescan`'s edit phase is racy on this volume, and the cache is why.**
The gate rewrites a file *without* restoring its mtime and requires the scan to
notice. On this volume it often does not. `run.py` builds the corpus volume
with `diskutil erasevolume HFS+`, and HFS+ records mtime to the second:

```text
$ python3 - <<'PY'   # on /Volumes/dedupbench, writing every 200 ms
mtime_ns=1787372023000000000  (frac=0)
mtime_ns=1787372023000000000  (frac=0)
mtime_ns=1787372023000000000  (frac=0)
mtime_ns=1787372023000000000  (frac=0)
PY
```

The cache key is `(inode, size, mtime_ns)`, so an edit landing in the same
second as the scan that cached the digest is invisible — no forgery required,
unlike the poison case the gate tests deliberately. The failure reads:

```text
  control, no sabotage        UNEXPECTED FAILURE: fail: after an edit — groups
  differ: 1 expected group(s) not found: 3224280B [album/set/five-1.jpg, ...] |
  1 unexpected group(s): 3224280B [album/set/five-0.jpg, ...]
```

Measured at smoke scale: `rescan-cache`'s own tip fails this 2 runs in 5. The
merged tree fails it 5 in 6 — not because anything broke, but because round
two's scans are fast enough that the corpus build, both scans and the edit now
land inside one tick. On a run that wins the race everything passes, including
the two that matter most: warm reads **0 B against a cold 32.8 MB (0.00%)**,
and `skip-attested-verify` is caught by the real deletion refusal
("deleted album/copies/pair000.jpg out of a group whose copies do not match").

This is a real limit of the tool, not an artifact of the bench — the same thing
happens on any volume with coarse timestamps. It costs a wrong answer, never a
wrong deletion, because deletion byte-verifies every attested group. The known
fix is git's "racily clean" rule: distrust an entry whose mtime falls in the
same tick as the scan that wrote it. It costs one extra warm-up scan, which the
gate's two-scan cold/warm shape cannot express, so taking it means changing the
gate — which section 5 puts out of an optimizer's hands. Left for the user.

## 2026-08-22 — Both of the merge's leftovers, repaired

The two things the last entry logged rather than fixed. The first is a product
bug and its fix is in `dedup.py`; the second is the harness's own capacity and
its fix is in `bench/`, authorised for that reason.

### 1. Racily-clean cache entries

`(inode, size, mtime_ns)` is only as fine as the filesystem's clock, and HFS+
records mtime to the second. An edit landing in the same second as the scan
that read the file leaves all three unchanged, so the next scan believes a
digest taken over bytes that are gone. No forgery anywhere in it — an ordinary
edit at an ordinary moment.

Every row now carries `cached_at`, the second the writing scan started, and a
row is trusted only while the file's mtime tick is **strictly older** than it:
git's racily-clean rule. The comparison is made at whole seconds on every
volume (`CACHE_TICK_NS`), the coarsest granularity any target filesystem uses;
asking each volume how fine its clock is would be one more thing to get wrong
for no safety gained. `CACHE_SCHEMA` went 2 → 3, so every database written
before this is dropped and rebuilt cold.

Three details that are not obvious from the rule:

- **The timestamp is read once, in `__init__`, before anything else is.** Every
  digest the scan stores comes from bytes read after that moment, so any file
  whose mtime reaches that tick could have changed behind the read. Stamping at
  store time instead would leave a hole open: a read at 10.9 and a store at
  11.1 are different ticks, and an edit at 10.95 would be trusted.
- **A distrusted row is dropped whole, not re-stamped.** Every column of it was
  taken by the same suspect scan. Re-reading stage 3's digest and leaving stage
  4's behind under a fresh timestamp would turn one racy row into a trusted
  lie — the file would be re-read where it settles and believed where it does
  not.
- **`remember` always writes this scan's timestamp.** A racy row's key still
  matches, which is the whole problem; without this it would keep its old
  timestamp and never become trustworthy again.

The cost is re-reading a file edited in the same second as the scan that
recorded it, plus one further scan before it settles. The poison guarantee is
untouched, and was never what this was about: a trusted row still proves
nothing, and deletion still byte-verifies every attested group.

**At the CLI, before and after.** Two 16 MB twins, both stamped to the exact
second the cold scan runs in; then one is rewritten to the same length and
stamped back to that same second. Size and `mtime_ns` come out of the edit
unchanged — `16777216 1787374177` before and after — and no forgery is
involved, only a clock too coarse to show what happened. On `main` at `9fb25e1`:

```text
--- re-scan after the same-second edit ---
  1 duplicate groups · 16.0 MB reclaimable
  0 byte-compared just now · 1 remembered from an earlier scan
  16.0 MB × 2  (reclaims 16.0 MB)
      .../tree/a.bin
      .../tree/b.bin
VERDICT: WRONG — the edited file is still reported as a duplicate
```

With the rule in place:

```text
--- re-scan after the same-second edit ---
  No duplicates found.
VERDICT: correct — the edit was seen, nothing is reported as duplicate
```

Five tests cover it (`TestRacilyClean`): an entry cached in the file's own tick
is distrusted, one cached a tick later is trusted, the same-second edit is no
longer reported as a duplicate, a racy row loses every column and not just the
one re-read, and a trusted-but-lying group is still refused at deletion. They
pin the clock rather than sleeping on it — `cached_at` is the scan's own
reading and the file's mtime is set relative to it — so none of them is timing
dependent. The suite's own corpus helper (`twins`) now backdates what it writes
by two ticks, because a tree written microseconds before the scan is one the
tool is now right to distrust, and every test that wants a warm hit has to
describe a tree that is genuinely older than the scan. 171 passed.

### 2. Bench capacity, and a gate that no longer races

**`rescan` builds at smoke scale now, at any `--scale`.** It is gate-only,
everything it checks is a ratio or a refusal, and none of that depends on
corpus size — while at default scale its 430 MB was what tipped the ten corpora
past the 2816 MB RAM disk and killed a full `--score` run before it started.
One entry in `GATE_SCALE`, read by `scenario_shape`. The RAM disk was not
resized; section 2's warning about swap on a 94%-full boot disk stands.

**The gate waits for a tick boundary before its cold scan.** With the rule
above in place, a cold scan starting in the second that built the corpus writes
nothing but racily-clean entries, and the warm scan honestly re-reads all of
it — a correct answer to a real ambiguity, but not what the gate measures.
`next_tick()` costs at most 1.1 s and makes the phase deterministic. The
ordinary-edit phase deliberately does *not* get one: the edit always lands at or
after the cold scan's own tick, so `mtime >= cached_at` holds by construction
and the re-read is already deterministic. Ten runs confirm it rather than
argument alone.

**Flake, measured the same way on both sides** — the `rescan` scenario at smoke
scale, ten consecutive runs, quiet machine:

```text
BEFORE (main 9fb25e1)                AFTER (this branch)
run 1: FAILED  after an edit         run 1..10: pass
run 2: pass                          [warm 0 B against cold 32.8 MB, 0.00%]
run 3: pass
run 4: pass
run 5: FAILED  after an edit
run 6: FAILED  after an edit
run 7..10: pass
BEFORE: 7 passed, 3 failed out of 10
AFTER: 10 passed, 0 failed out of 10
```

Three in ten rather than the five in six the last entry recorded, and the
difference is machine load, not code: the tighter the machine, the more of the
build, both scans and the edit fit inside one tick. Either way the failure mode
is gone, not made rarer — the rule removes the race rather than dodging it.

### Capacity, honestly

The ten corpora now estimate 2671 MB against the 2816 MB disk and land at about
2530 MB actual. That fits, but the margin is thin and it is worth writing down
where it went: `ensure_space` asks for 1.4× a corpus's estimate before building
it, and with nine corpora already on the volume the tenth's headroom is the
binding constraint. The first attempt at a full run still died —

```text
bench: /Volumes/dedupbench needs 627.5 MB free, has 598.3 MB
```

— on `bigheader`, 29 MB short, because 88 MB of accumulated `.fseventsd` spool
had built up on the volume across every agent's corpus churn. That is
regenerable OS log data on a scratch RAM disk, not corpus and not results;
clearing it freed 88 MB and the run went through. Nothing in `bench/` was
changed to make that check pass. If a future run dies the same way, clear the
spool before touching anything else — and note that the volume now finishes a
full run with roughly 150 MB free, so a scenario added at default scale will
need this arithmetic redone.

### Numbers

Full suite, in-tree, seed 1, default scale, `--score`, exit 0:

```text
scenario          gate  bytes cur  bytes base  cur/base  sec cur  sec base  cur/base  rss cur  rss base
typical           pass  52.5 MB    108.7 MB    0.48x     0.286    0.455     0.63x     104      53
photos            pass  147.9 MB   150.8 MB    0.98x     0.018    0.021     0.86x     65       149
bigheader         pass  146.2 MB   296.5 MB    0.49x     0.015    0.030     0.52x     98       109
needle            pass  244.1 MB   244.1 MB    1.00x     2.696    3.889     0.69x     115      304
samesize          pass  32.8 MB    348.4 MB    0.09x     0.047    1.512     0.03x     57       284
widegroup         pass  126.3 MB   158.8 MB    0.80x     0.057    0.053     1.08x     162      152
prefixtrap        pass  33.4 MB    44.2 MB     0.75x     0.011    0.015     0.77x     24       51
adversarial       pass  1.4 MB     4.2 MB      0.34x     -        -         -         -        -
deletion          pass  -          -           -         -        -         -         -        -
rescan warm/cold  pass  0 B        32.8 MB     0.00x     -        -         -         -        -
SCORE: 0.4847
```

Ten scenarios, which is the first full run since the merge — the last entry
could only name the nine that fit. Run twice: `0.4827` and `0.4847` against the
integrator's `0.4842`, the spread being the time terms on a machine that drifts
2–3× with load. **Every byte column is identical to the integrator's, to the
byte**, on all eight scenarios that report one, which is the thing that had to
be true: the six scored scenarios run cache-off and neither fix reaches them.

`--selftest`, exit 0, every sabotage still caught:

```text
  control, no sabotage        clean       byte gap 0.0%, 4 group(s) found
  truncate-compare            caught      groups differ: 5 unexpected group(s)
  underreport-bytes           caught      byte gap 50.0% against a 20% tolerance
  control, no sabotage        clean       warm read 0 B against a cold 32.8 MB
  skip-attested-verify        caught      fail: deletion — deleted
                                          album/copies/pair000.jpg out of a
                                          group whose copies do not match
```

**Verdict:** both fixed, on `main`.
