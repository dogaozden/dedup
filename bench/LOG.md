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
