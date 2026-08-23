# RGOAL — the rules for optimizing the Rust tool

Successor in spirit to `bench/GOAL.md`, which governed the Python bench and
retired with it. That file's law was *bytes first*. This one's is **time
first** — the port is fast enough that wall clock is now the thing worth
buying, and bytes have become a constraint you may not spend.

Written alongside `rbench/run.py`, which is the only measurement that counts.
Where this file and the code disagree, the code is right and this file is
stale; say so and fix it.

---

## 1. Mission

Make `rust/` faster without ever making it wrong.

Three things are tracked, and they are not peers:

- **time** is the score. Wall clock of the whole process, spawn to exit.
- **bytes** are a constraint. You may read less than the oracle did. You may
  never read more, and you may never read less than proving the answer costs.
- **correctness** is a gate. A wrong answer has no score at all — not a bad
  one.

A gate is not a metric. You do not trade against a gate, and there is no
number good enough to buy one off.

---

## 2. The machine

An Apple-silicon Mac with 16 GB of RAM, shared with whatever else its owner
is running. Three facts follow from that and all three have bitten someone:

- **Absolute timings drift by a factor of two or more** depending on what else
  is awake. A number measured an hour ago is not comparable with one measured
  now. Only ratios taken inside the same interleaved round are comparable, and
  that is the only kind of number this harness reports.
- **Corpora live on a RAM disk at `/Volumes/dedupbench`**, which is the only
  volume this harness may touch. Never `/Volumes/My New Passport` — that drive
  belongs to the user. No `sudo`, ever.
- **Other agents share this box.** `run.py` refuses to score while another
  harness is live and takes an advisory lock on the volume. Both checks exist
  because two runs on one RAM disk evict each other's corpora mid-scan, and
  the wreckage arrives looking like a gate failure in your change.

The RAM disk is 2816 MB and is routinely near full. Corpora are regenerable;
`ensure_space` evicts the ones this run does not need, but only ever from
`corpora/`. Everything else on that volume belongs to somebody else.

---

## 3. The harness

```
python3 rbench/run.py                        # full scored run, 9 rounds
python3 rbench/run.py --candidate <sha|path> # measure something else
python3 rbench/run.py --calibrate            # baseline vs itself: the noise floor
python3 rbench/run.py --selftest             # sabotage the candidate, check the gates
python3 rbench/run.py --scenario needle      # one scenario, prints no SCORE
python3 rbench/run.py --scale smoke          # tenth-size corpora, for development
python3 rbench/floor.py                      # what the machine costs before you do
```

**Scales.** `r2` is the scored one and the default. `smoke` is a tenth of it and
is for development — it prints a SCORE line but nobody should quote one.
`default` is the round-1 set, kept so that round-1 corpora still build, and
**not** comparable with anything measured at `r2`. A scale's sizes never move
once numbers exist against it; when they need to move, the next scale gets the
next name. See the round-2 journal entry for how `r2`'s sizes were chosen.

Exit `0` when every gate passed, `1` on VOID, `2` when the machine or the
harness is wrong and it is not your fault.

**The two sides.** `rbench/baseline.lock` names a commit. The harness extracts
that commit's `rust/` straight out of git, builds it at `--release` into a
target directory it owns, and times it against the candidate. The candidate is
this checkout's `rust/` unless you name another. Both sides are built the same
way from clean sources, so no build-cache state is ever part of the
comparison.

**The scored set** is `needle`, `typical`, `samesize`, `widegroup`, `photos`,
`bigheader`, `rescan` — the Python bench's six, plus the warm-cache scenario
the cache earned when it became first-class. That list is fixed from the day
it was written. A scenario added later earns its keep as a gate, not as a
term: a score is only worth something if today's number compares with one
recorded months from now.

**The gate set** is `prefixtrap`, the first scenario to come through that door.
It runs in every scored run, at smoke scale, and it voids on the same gates
everything else does — but it contributes no term and appears in the table
marked `*`. It is there because all seven scored shapes are blind to one idea:
reading less of the head. On every one of them a shorter head probe reads
fewer bytes and separates just as much, so the bytes fall, which is legal, and
the clock falls with them. `prefixtrap` is same-size files that agree for a few
kilobytes and then part company — the shape real trees are full of and the
scored seven contain nowhere — and a head too short to separate a band there
sends it to the byte-for-byte stage as one bucket, far past anything the
baseline paid. It is a gate and not a term because what it detects is a wrong
turn, not a slow one.

**Corpora** come from `bench/corpus.py`, imported and not forked, so scenario
names and shapes stay continuous with everything in `bench/LOG.md`. Python is
allowed at generation time and nowhere else.

---

## 4. The score

```
SCORE = geomean over the seven scored scenarios of median(wall_cand / wall_base)
```

Lower is better. `1.0000` is the pinned baseline exactly.

Per scenario the harness runs at least nine interleaved rounds — baseline,
candidate, baseline, candidate — and takes the ratio **within each round**,
then the median of those ratios. Pairing inside a round is what makes this
survive a drifting machine: drift that moves both sides in a round divides out
of that round's ratio, and the median discards the rounds where it did not.

A geometric mean because these are ratios. Halving one scenario and doubling
another has to come out where it started, and an arithmetic mean does not.

**VOID is not a bad score. It is no score.** Any gate failure prints
`SCORE: void (<reason>)`, reports no timings at all, and exits nonzero. There
is no partial credit and no penalty term — a candidate that got the wrong
answer has not been measured, it has been disqualified.

---

## 5. The gates

Every one of these voids the run.

1. **Correctness.** The answer key is derived from corpus construction — the
   generators recorded every duplicate group they planted — and the frozen
   oracle is made to agree with it before a single timing is taken. Every run,
   every round, both cold and warm, the candidate's `--report-json` groups
   must match. Group membership is compared as a set of sets of NFC names;
   the order groups are printed in is the conformance suite's business, not
   this one's.

2. **Bytes ceiling.** At generation time the harness runs the frozen oracle
   with its `open()` wrapped, and stores what it actually read beside the
   corpus. The candidate's self-reported `bytes_read` may never exceed it.
   Reading *less* is allowed and is exactly where the remaining algorithmic
   wins are. Reading more is a regression. This is the gate `prefixtrap` was
   added to arm: on that corpus a probe too short to separate a band does not
   read less, it reads the band to the end.

3. **Byte floor.** Nothing is a duplicate on the strength of a hash, so every
   member of every group has to be read end to end at least once. The floor is
   `sum(size × members)` over the planted groups. A candidate reporting fewer
   bytes than that either did not prove what it claimed or is lying about its
   bytes — the ceiling only looks upward, and this is the direction it cannot
   see. Off for the warm half of `rescan`, where reading nothing is the point,
   and nowhere else.

4. **Counter plausibility.** One sampled run per scenario holds the
   self-reported byte count against `proc_pid_rusage`'s physical-read figure.
   This is a tripwire, not a proof, and on a RAM disk it is close to vacuous —
   see §7.

5. **Setup ceiling.** The candidate scanning an empty directory, three times,
   fastest kept. Above two seconds is fixed overhead, not a scan.

6. **Manifest tripwire.** Every file's size and mtime under the corpus, taken
   before the scored rounds and again after. A corpus that moved during the
   run means two different things were measured and called one.

7. **Byte determinism.** A side that reads a different number of bytes from
   one round to the next is not running the same experiment nine times.

---

## 6. Rules of measurement

1. **`rbench/run.py` is the only measurement that counts.** Do not invent your
   own timing scripts and do not quote numbers from anything else.

2. **A delta smaller than the noise floor is not a win.** Run `--calibrate`
   and read the number it prints. On this machine, on 2026-08-23, with another
   agent's build running alongside, a baseline scored against itself gave a
   geomean of `1.0062` and a worst per-scenario median of `1.0341` — a noise
   floor of about **±3.4% per scenario, ±1% on the geomean**. Recalibrate
   whenever the machine's circumstances change; do not inherit that figure as
   a constant. **Do not keep a change on the strength of a 2% score
   improvement.**

3. **Every number you write down is copied from harness output.** Never
   estimate, never round from memory, never quote a figure you did not watch
   print.

4. **One change at a time.** Two changes measured together are one
   measurement of neither.

5. **Confirm at `--seed 2` before claiming a win**, and at `--scale default`
   for anything whose effect could depend on corpus size — thresholds,
   fan-outs, batch sizes, buffer sizes, any new constant.

6. **A win the harness cannot see is a claim, not a result.** If the path you
   optimized is not exercised by any scored scenario, this bench has no
   opinion about your change and neither should you.

---

## 7. What this bench is blind to

Every one of these is a real cost that this harness charges nothing for.

- **Spinning-media seek cost.** The corpus volume is a RAM disk: no heads to
  move, no rotational latency, no queue depth, no readahead worth the name. An
  idea aimed at seek behaviour will look flat here by construction, not
  because it failed. The Python bench learned this the hard way — its tail
  probe was measured as a large win on a volume that charged nothing for the
  one thing it would cost most on real slow media, and nobody has ever
  measured it on a real device.
- **Real cold-cache device I/O.** There is no cold cache to be had. `rescan`
  measures a warm dedup-cache hit against a populated one, not a cold disk.
- **NTFS, exFAT, and network volumes.** Never exercised. The corpus volume is
  HFS+ and the boot volume is APFS.
- **The physical-read tripwire, mostly.** `proc_pid_rusage` reports real device
  reads, and reads from a RAM disk are not device reads. Gate 4 therefore sits
  near zero on both sides and will only ever catch a candidate pulling
  gigabytes off the boot disk. It is not, and is not claimed to be, proof that
  a self-reported byte count is honest. Gates 1, 2 and 3 plus review are what
  actually hold that line.
- **RAM ceilings.** Peak RSS is not measured and not scored.
- **Sampling misses the last milliseconds.** The physical-read counter can only
  be read from a living process, so the final sample lands slightly before
  exit. That biases the figure downwards, which is the safe direction for a
  tripwire but means it under-reports.
- **Whatever else was awake.** Load average is recorded with every run.
  Read it before believing a small number.

---

## 8. Cross-scenario trades

A change that wins on one scenario and loses on another **is not yours to
keep**. Log it with the full per-scenario table and leave it on an unmerged
branch for the maintainer to judge. That is the only path for such a trade,
and it is the same rule the Python bench had.

The score is a geometric mean, so it will happily average a real regression
into an apparent win. Read the per-scenario column, every time, before you
read the last line.

The same applies to a bytes-for-time trade: if a time win costs bytes anywhere
— even bytes still under the ceiling — say so with both numbers and let the
maintainer decide.

---

## 9. Inviolables

Breaking one of these does not produce a bad result. It produces no result.

- **Never weaken a gate to pass it.** Not the tolerance, not the ceiling, not
  a scenario's scale, not a case. Adding scenarios and adding gates is welcome;
  loosening one is forbidden. A benchmark you have repaired to make your own
  change look good is worth nothing, and from the outside it is
  indistinguishable from an honest repair.
- **Never edit `rbench/` to change your own score.** The harness ignores it
  anyway — see §10 — so an edit here can only ever mislead you.
- **Never read the corpus manifest from tool code.** It sits beside the
  scanned tree, never inside it, and it is the answer key.
- **Byte-exactness stands.** Nothing is reported or deleted as a duplicate on
  the strength of a hash or a sample. A hash may narrow a candidate set; it may
  never prove one. There is no fast mode that stops at a sample.
- **The cache may narrow, never prove.** Deletion byte-verifies every group,
  including one the cache vouched for. The six non-`rescan` scenarios run
  cache-off forever; do not "improve" one by letting it remember an answer the
  harness is about to ask for nine more times.
- **The filesystem fence.** Read and write only inside the repository and
  `/Volumes/dedupbench`. No `sudo`. Never run the tool's deletion modes
  anywhere but a RAM-disk corpus.
- **Never commit to `main`, never merge to `main`, never force-push, never
  rewrite history.** The maintainer merges.

---

## 10. The referee protocol

Your own copy of `rbench/` does not judge you.

The harness stages the candidate by extracting **`rust/` and only `rust/`**
from the commit or path you name. Your branch's edits to `rbench/run.py`, to
`rbench/baseline.lock`, to the corpus generators, cannot reach the thing being
measured, because the thing being measured is a directory taken by name. That
is the referee protocol, mechanized rather than promised.

For a number anyone will quote, run it from a clean checkout too:

```
git worktree add /tmp/rbench-referee main
python3 /tmp/rbench-referee/rbench/run.py --candidate <your-sha>
```

Then quote what it printed, not what you remember.

**The claim/review handshake.** Land nothing on the strength of your own run:
say what you changed, quote the referee's per-scenario table and its score,
name the noise floor you measured that day, and leave the branch unmerged.

---

## 11. The journal

Append an entry for **every** attempt, including the failures. Especially the
failures. Reverted ideas stay in the log permanently, and before starting a
new idea you check whether the log already killed it.

```
## YYYY-MM-DD — <one-line title>

**Hypothesis:** what you expected to get faster, and why.
**Change:** what you actually did, in a sentence or two.
**Numbers:** the per-scenario table and the score, pasted from harness output.
              The noise floor you measured the same day. The load average.
**Verdict:** KEPT (commit <sha>) or REVERTED, and the reason.
```

---

## 2026-08-23 — RBENCH built and proved

**Change:** `rbench/run.py`, `baseline.lock` pinning `c238b30`, this file.
Corpora and generators reused from `bench/corpus.py` by import. The oracle is
`dedup.py` at `c238b30`, driven through `conformance/oracle_driver.py`.

**Numbers**, all at `--scale smoke` on a machine where another builder (M5)
was working at load average ~3.4:

| run | result |
|---|---|
| `--calibrate --rounds 15` | geomean **1.0062**, noise floor **±3.4%** (worst scenario `samesize` at 1.0341) |
| `--candidate c238b30 --rounds 9` | **SCORE 0.9890**, every gate green |
| default candidate, separately built, `--rounds 9` | **SCORE 1.0085**, every gate green |
| same again after the last refactor | **SCORE 1.0061**, every gate green |
| `--selftest` | 4 of 4 sabotages caught, honest control clean |
| a candidate patched in Rust to drop one group, rebuilt from source | **VOID** on the first scenario, no timings printed, exit 1 |

The four sabotaged candidates and the gate that caught each: `drop-group` →
correctness; `inflate-bytes` → bytes ceiling; `underreport-bytes` → byte
floor; `touch-corpus` → manifest tripwire.

Two things the build turned up that are worth remembering:

- **A stale corpus outlives the run that made it.** The Python bench's
  `rescan` gate rewrites corpus files in place and leaves them rewritten, so
  `rescan-smoke-seed1` on the shared RAM disk no longer matched its own
  manifest. The generation-time cross-check between construction and the
  oracle caught it. `ensure_corpus` now spends one rebuild on a disagreement
  before treating it as real. Do not remove that check to make a run go
  through; it is the reason a poisoned corpus cannot quietly become the
  answer key.
- **A liveness check that is too broad is a check that is off.** The first
  "is another harness running" test excluded this process's whole process
  group, which also hid any rival launched from the same shell — the exact
  case it exists to catch. It only surfaced because the check was tested in
  both directions, with a fake harness deliberately started. Test a tripwire
  by making it fire.

Once fixed, that check earned its keep the same afternoon: it refused a run
because the M5 builder was mid-scan with its own `dedup-rs`, on a machine
whose load average had climbed to 5.5. That is a run that would otherwise
have been timed against another agent's work and quoted as a number.

**These are smoke-level proof that the harness functions. They are not
records.** They were taken at a tenth scale, on a busy shared machine, against
a candidate that is byte-identical to the baseline. The first real optimizer
round should recalibrate on a quiet machine and at `--scale default` before
quoting any figure as a starting point.

**Verdict:** KEPT.

---

## 2026-08-23 — optimizer round one, closed

Three contestants ran the loop above against baseline `c238b30`, each in its
own worktree, each scored by the referee protocol of §10 rather than by itself.

| entrant | score | verdict |
|---|---|---|
| opus | **0.3173** | CONFIRMED, and merged — `823e87c` |
| sol | 0.4808 | CONFIRMED clean, and strictly dominated |
| ox | 1.0000 | no change; stopped honestly at three strikes |

**What won.** One finding: `dedup-rs` spent about 60 ms of every run on a
`diskutil` subprocess — twice over with the cache on — to learn two facts the
kernel and the IO registry hand over in microseconds. Six of the seven scored
scenarios finish in under 200 ms, so on five of them that subprocess was not
overhead around the work, it *was* the work. Replacing it with FFI in
`rust/src/sys.rs` took the geomean to a third of the baseline and made every
scenario faster; nothing traded against anything. The branch's own referee run
recorded 0.3161 against the verifier's 0.3173 — the same commit, 0.4% apart,
comfortably inside the ±4.3% floor measured that day.

**Sol found the same lever and got two thirds of the way down it.** 0.4808 is a
real, clean, confirmed win by any standard except the one that mattered: it
removed one `diskutil` call and left the one inside `volume_uuid` standing.
Same idea, same afternoon, half the prize, and nothing to merge because the
winner is a superset of it.

### What the round is evidence of

- **Grep every subprocess spawn on the hot path before optimising any of
  them.** This cost Sol the round outright. `Command::new` is four tokens and
  the whole finding was a list of where they appear; the work that followed was
  the easy part. Count them first, then choose.
- **The referee protocol earns its cost.** Ox's second attempt measured 0.96 in
  its own tree and 0.9999 under the referee — self-measurement is optimistic in
  the direction you are hoping, reliably enough that a bench without an
  independent scorer is a bench that flatters whoever is holding it.
- **`getattrlistbulk` is not settled.** One naive attempt (ox) scored 1.0148 —
  slower. That is a data point about that attempt, not a verdict on the idea;
  REWRITE.md §4 still wants it and `needle` is still the scenario where the
  walk dominates. Whoever picks it up should read that attempt first and not
  treat 1.0148 as the ceiling.
- **Parked, unproven:** Sol's idea of classifying virtual media as flash-like.
  It landed inside the ±2.9% floor, which means the bench could not see it
  either way — and §7 already names why: the corpus volume is a RAM disk that
  reports no medium, so this is the one case the bench charges nothing for.
  Not rejected, not kept. If anyone measures it, measure it on top of the
  merged tip, and read §7 and §8 before quoting the result.

**Verdict:** opus KEPT (merged at `823e87c`); sol and ox REVERTED, branches
`sol-r1` and `ox-r1` left standing as the record.

---

## 2026-08-23 — round two, prepared: a new scale, a trap gate, and a floor

**Hypothesis:** round one measured process startup more than it measured the
algorithm, so round two needs bigger corpora. That turned out to be half right,
and the half that was wrong is the more useful finding.

### What round one was actually measuring

Round one's diagnosis was "six of seven scenarios finish under 200 ms, so a
78 ms subprocess kill scored 3.15×". True — but that 78 ms *was* the
`diskutil` overhead, and removing it is what round one bought. Measured today
against `main`, the tool starts in **3.6 ms** — against the pinned `c238b30`'s
52 ms on the same volume. The startup problem round two inherits is fifteen
times smaller than the one round one solved.

So the question is not "are the corpora too small" in the abstract. It is
"what fraction of each scenario is still startup". Measured at the old
`default` scale, against `main`:

| scenario | baseline s | spawn share |
|---|---|---|
| needle | 1.6047 | 0.2% |
| typical | 0.1281 | 3.1% |
| widegroup | 0.0711 | 5.5% |
| samesize | 0.0252 | 15.6% |
| photos | 0.0225 | 17.5% |
| bigheader | 0.0224 | 17.5% |
| rescan | 0.0060 | **66%** |

### The arithmetic that decides what re-scaling can buy

Wall clock per megabyte of RAM disk, same measurements:

```
needle 6.58   widegroup 0.56   typical 0.43        ms per MB
samesize 0.061   photos 0.057   bigheader 0.050
rescan ~0
```

A hundredfold spread, and it is not arbitrary. A file costs about 11 µs to
open and 1 µs to stat; the same 11 µs spent reading buys roughly 300 KB. **On
this volume, file count buys wall clock and bytes do not.** Measured RAM-disk
throughput is 26–29 GB/s, so spending one second on reading needs 26 GB and the
whole disk is 2816 MB. No byte-bound scenario on this machine can reach one
second at any scale that fits. That is the answer to the "≥1 s per scenario"
target: it is reachable for `needle` and arithmetically impossible for the
other six, and no amount of scaling changes it.

The scenarios therefore fall into three classes:

- **responds to scale** — `needle`, `typical`, `widegroup`. Enough files that
  per-file cost dominates.
- **capped by physics** — `samesize`, `photos`, `bigheader`. Few files, many
  bytes, read at 26 GB/s. Every one of them sits in the twenties of
  milliseconds and stays there. Giving all three another 240 MB of RAM disk
  was measured to buy 13 ms between them, and would have cost the volume the
  headroom it needs to not evict a corpus every run. Not spent.
- **responded to nothing** — `rescan`, as it was. Its warm scan reads nothing by
  design and walked thirteen files, so it was startup and a rounding error. It
  has since been reshaped rather than rescaled; see below.

### The `r2` scale

`bench/corpus.py` gains a scale named for the round that cut it, plus
per-scenario multipliers, because one scalar cannot size seven shapes to a
common wall clock. `smoke` is untouched and is still what development runs on.
`default` is kept so round-one corpora still build and is not comparable with
anything here.

| scenario | factor | files | on disk | baseline s | startup share |
|---|---|---|---|---|---|
| needle | **0.5** | 50,000 | 196 MB | 0.6934 | 0.5% |
| typical | **1.2** | 19,200 | 386 MB | 0.1664 | 2.2% |
| widegroup | **3.6** | 1,820 | 452 MB | 0.1710 | 2.1% |
| samesize | 1.0 | 1,402 | 415 MB | 0.0246 | 14.6% |
| photos | 1.0 | 100 | 394 MB | 0.0227 | 15.9% |
| bigheader | 1.0 | 8 | 451 MB | 0.0232 | 15.5% |
| rescan | **0.7** | 11,200 | 231 MB | 0.0277 | 13.0% |
| `prefixtrap` (gate) | 0.1 (smoke) | 32 | 65 MB | 0.0072 | 50% |

`baseline s` for `rescan` is its **warm** scan, which is the half that is scored;
its cold half is the harness's untimed priming run and has no figure of its own.

Startup share is against `main`'s own measured startup of 3.6 ms, of which
2.2 ms is work it chooses to do and 1.3 ms is bare process creation.

`baseline s` is `main` at `baf9e25`, which is what the pin is about to become,
**not** the currently pinned `c238b30`. Generation is 0.1–4.2 s per corpus and
the oracle pass 0.1–2.4 s, both far inside the two-minute budget.

### Size these against disk, not against bytes

The first cut of `r2` was sized from the manifests' `total_bytes` and overflowed
the volume without saying so. The volume is HFS+ with a **4 KiB allocation
block**, and a tree of small files costs appreciably more than it contains:
`needle`'s files average 2.6 KB and each occupies a whole block, which is 136 MB
of difference on that corpus alone. The set that was supposed to occupy 2508 MB
of 2816 actually wanted about 2860, and the symptom was quiet — `needle` and
`bigheader` were evicted and rebuilt on **every single run**, which the logs
show plainly once you look for `built` lines and which no gate has any reason to
mention.

Two lessons, both cheap to act on and both easy to miss:

- **`du`, never `total_bytes`.** `ensure_space` compares against real free
  space, so a plan made in data bytes is a plan made in the wrong unit.
- **Check the `built` lines after a run.** A corpus that is rebuilt every time
  is a corpus that did not fit, and the run still passes every gate while it
  happens.

`ensure_space` evicts in alphabetical order and reserves 1.4× its estimate, so
under pressure it will drop `bigheader` and `needle` — the two most expensive
things to rebuild — before anything else, and the two of them then take turns
being missing. That is existing behaviour and not worth changing for this; the
fix is to leave enough room that it never runs.

Sizes are therefore set from measured disk usage, with `needle` and `typical`
held well below what would fit because they have the slack to spare: both still
run far above any startup effect, and their megabytes buy much more on `rescan`,
which had none. The eight corpora a scored run touches occupy **2599 MB** of the
2739 MB the volume has for corpora, leaving about 140 MB — enough that a run
which rebuilds nothing evicts nothing.

### `rescan` reshaped: off `photos`, onto `typical`

`rescan`'s warm scan reads nothing — that is the point of it — so the only work
it can possibly do is walk the tree, stat every file and ask the cache about
each one. All of that is per-file, and over `photos`' thirteen large files it
came to about a millisecond. The scenario scored **0.0448**, the largest single
term in the geomean, and essentially all of it was the startup cost round one
had just removed. A cache test that cannot see the cache measures nothing.

It now borrows `typical`'s shape instead. It keeps its own directory for the
reason it always had one. `WARM_CEILING`, the warm correctness gate and the cold
half's gates are all untouched, and the cold half runs like any other cold scan.

**Its `min_size` is one byte above `DEEP_HEAD`, and that is the whole trick.**
The first attempt used `min_size` 1 and failed `WARM_CEILING` outright, reading
**69%** of its cold bytes back on every warm scan. The cause is in the tool, and
it is worth writing down because no scored scenario prices it:

> `small_file_stage` settles everything at or below `DEEP_HEAD` in one pass, and
> it calls `cache.remember(…, Field::Full, …)` **only for groups it proved to be
> duplicates**. A small file found unique is never remembered. A size class is
> then settled from the cache only when *every* member of it carries a digest,
> so a single unique file sends its whole class back to disk. `typical` is ~96%
> unique small files, so nearly every class was re-read.

Above `DEEP_HEAD` the staged pipeline caches a digest for every file it opens,
unique ones included, and the warm scan reads exactly **0 B**. The whole tree is
still walked and stat'd — `min_size` decides what is *considered*, not what is
*visited* — so the file count, which is the measurement, is undiminished.

**That finding is the more valuable half of this change.** A real warm re-scan
of an ordinary tree re-reads every unique small file it has ever seen, every
time. Caching the unique verdict for small files looks like a genuine win, and
none of the scored seven can see it.

**A sharp edge, recorded deliberately.** Few files are considered and their
sizes are drawn from a continuous distribution, so no two of them share a size
and the oracle reads nothing but the duplicate groups. `rescan`'s cold legal
band is therefore **zero bytes wide** — floor equal to ceiling. That is the
ordinary ceiling and floor gates on a corpus where the optimal read *is* the
floor, not a weakened or special-cased gate, and §5.2 says reading more than the
oracle is a regression. But it does mean any candidate reading one extra byte on
`rescan`'s cold pass voids, and an eager or batched head read would do it. Met
by surprise it looks like a broken harness, so it is written down here.

### The trap gate

`prefixtrap` joins as the first gate scenario, through the door §3 leaves open.
It runs at smoke scale in every scored run, gates on the same correctness,
ceiling and floor checks as everything else, contributes no term, and prints
marked with `*`.

It is there because all seven scored shapes are blind to one idea: reading less
of the head. On every one of them a shorter probe reads fewer bytes and
separates just as much — bytes fall, which is legal, and the clock falls with
them. `prefixtrap` is same-size files that agree for a few kilobytes and then
part company, and a probe too short to separate a band sends that band to the
byte-for-byte stage as one bucket.

Measured at smoke scale, the legal band is **[16.0 MB, 16.2 MB]** — floor to
ceiling, 1.4% wide, because almost every legal byte on that corpus is the
mandatory group proof. Reading every file to the end costs **64.0 MB**, four
times the ceiling. There is nowhere for a shrunk probe to hide.

`--selftest` gained a fifth sabotage, `shrunk-head`, which reports what that
failure costs on the corpus — every file at or above `min_size`, read end to
end — taken from the corpus rather than from a multiplier. An honest control
runs on the same corpus first, because a corpus that voided for everybody would
catch the sabotage and prove nothing by it. What a *real* shrunk-head candidate
does here was measured when the scenario was built and is recorded in
`_prefixtrap`'s docstring: `DEEP_HEAD` at 4 KiB read 147.2 MB against a
baseline of 44.2 MB.

### The floor

`rbench/floor.py` is new and exists because round two's targets should come out
of arithmetic. The predecessor bench set a target below what the hardware
charges once; a target like that can only be hit by a candidate that stopped
doing the work.

Measured against the pin `94739ad` at `r2`, 2026-08-23, load average 4.98 4.45
4.78, machine otherwise idle — of record:

```
process creation 0.0012 s   ·   open + close 10.96 µs   ·   readdir 0.25 µs

scenario     files   spawn    walk   opens    read   floor  base s  floor/base
needle      50,000  0.0012  0.0429  0.0005  0.0000  0.0447  0.6810        6.6%
typical     19,200  0.0012  0.0165  0.0203  0.0012  0.0392  0.1644       23.9%
samesize     1,402  0.0012  0.0012  0.0010  0.0008  0.0042  0.0237       17.9%
widegroup    1,820  0.0012  0.0016  0.0199  0.0166  0.0393  0.1857       21.2%
photos         100  0.0012  0.0001  0.0004  0.0054  0.0072  0.0222       32.2%
bigheader        8  0.0012  0.0000  0.0000  0.0041  0.0054  0.0226       23.7%
rescan      11,200  0.0012  0.0096  0.0000  0.0000  0.0108  0.0246       44.0%
prefixtrap      32  0.0012  0.0000  0.0001  0.0006  0.0019  0.0064       29.8%

HONEYPOT: targets below ≈0.2131 are not reachable honestly.
          ≈0.1431 if the walk became a bulk directory read.
```

Read that against round one's 0.3173: the room left below the new pin is roughly
a factor of five, not another factor of three, and the walk is where most of it
sits.

`rescan`'s row is worth a second look. Its floor is **44.0%** of its baseline
and almost all of that is the walk — which is the whole object of the reshape.
Where the old `rescan` was a startup measurement with a rounding error attached,
it is now a walk-and-cache-lookup measurement with a known irreducible half. It
is also the second scenario, after `needle`, whose number a bulk directory read
would move.

**These are of record**, taken at pin time on 2026-08-23 with the machine
otherwise idle. Three things about them are structural rather than incidental
and will survive.

**A floor may only contain what nobody can remove.** The first draft used the
tool's own startup as the spawn term. Held against the pinned `c238b30` — which
spends about 52 ms shelling out before it looks at anything — that produced a
honeypot of **0.4286**, and round one's confirmed, merged, independently
refereed winner scored **0.3173**. The floor would have called the round's
actual result cheating, and it would have been wrong for a reason worth
remembering: that 52 ms was work, and round one's entire win was proving it was
work rather than physics. The spawn term is now bare `fork`/`exec`/`exit`,
measured at about 1.3 ms; the binary's own startup is measured and printed
beside it, with the difference named as removable. Against `c238b30` the
honeypot then reads 0.0700, which is below 0.3173, which is the answer that
does not contradict a known result.

**The gap between the two honeypot figures is `getattrlistbulk`.** The walk term
is costed at one `lstat` per file, which is what the tool does and not what the
kernel charges at best: `readdir` answers at 0.24 µs an entry against `lstat`'s
1.07. A candidate landing between the two figures has most likely written the
bulk walk the round-one entry left open, not cheated. Read the diff before
believing either number.

**Every row must read under 100%.** A lower bound above a time that was actually
observed is wrong by inspection, and `floor.py` now refuses to print a honeypot
line when any row does it. Two separate bugs were caught this way while it was
being written: spawn measured on the pinned baseline while the wall clock came
from `main` (a floor of 0.1493 s for a scenario observed at 0.0054 s), and spawn
measured against an empty directory on the *boot* disk while the scenarios scan
the RAM disk, which for a binary that probes the volume it is pointed at is not
the same measurement.

While the pin is stale, run it as `floor.py --against candidate` after a run
whose candidate was the commit the pin is about to move to; the spawn term and
the wall clock then come from the same binary, which is the thing that has to be
true. Once the pin moves, the default is right again.

### The pin

`baseline.lock` is **not** touched here. It still names `c238b30`, and the
maintainer re-pins to the post-`features`-merge tip. Every number in this entry
that says "baseline" means `main` at `baf9e25`, which is what that tip will
contain, and the run below reports a score against the *old* pin because that
is what the harness was still holding.

**Update, pin time (2026-08-23):** `baseline.lock` now names `94739ad` —
`main` after the `features` and `r2prep` merges. The paragraph above describes
how the run and calibration below were taken, against the *old* pin; every
score from here forward is against this one.

**Numbers.** Full scored run, `--scale r2 --candidate baf9e25`, nine rounds,
baseline the stale pin `c238b30`, load average 4.73, another builder working.
**This run rebuilt nothing** — every corpus was already resident, which is the
check that the set fits:

```
scenario     base s  cand s   ratio  spread     bytes   ceiling     floor
needle       0.7556  0.6934  0.9177   ±4.0%  122.1 MB  122.1 MB  133.3 KB
typical      0.2262  0.1664  0.7400   ±0.8%   70.6 MB   70.6 MB   33.0 MB
samesize     0.0814  0.0246  0.3013   ±1.0%   32.8 MB   32.8 MB   22.5 MB
widegroup    0.2573  0.1710  0.7145   ±3.6%  451.3 MB  451.3 MB  451.3 MB
photos       0.0791  0.0227  0.2874   ±0.3%  147.9 MB  147.9 MB  147.8 MB
bigheader    0.0810  0.0232  0.2918   ±1.3%  146.2 MB  146.2 MB  112.0 MB
rescan       0.1422  0.0277  0.1961   ±0.2%       0 B    5.2 MB    5.2 MB
prefixtrap*  0.0636  0.0072  0.1140   ±0.3%   16.2 MB   16.2 MB   16.0 MB

SCORE: 0.4225
```

That 0.4225 is not a result — it is the round-one win being re-measured at a new
scale against a pin that has not moved yet. It is here as proof that all seven
scored scenarios and the new gate run to completion with every gate green at
`r2`. The immediately preceding run of the same pair scored 0.4219, 0.14% away.

`rescan`'s row is the reshape working: **0 B** read on the warm side, against a
cold scan of 5.2 MB, over 11,200 files instead of thirteen.

`--calibrate --rounds 9` at `r2`, pin time on 2026-08-23, load average 5.59
4.31 4.79, machine otherwise idle: geomean **0.9936**, **noise floor ±2.4%**
(worst scenario `rescan` at 0.9759), worst per-round dispersion `widegroup` at
±8.8%. Round one's equivalent at smoke scale was ±3.4%, so the larger corpora
still bought a tighter floor, which is what they were for — and the floor did
not loosen when `needle` and `typical` were cut back to fit the volume, which is
the evidence that those cuts were slack rather than signal. **This is the noise
floor of record**, against the `94739ad` pin.

`--selftest`: 5 of 5 sabotages caught (`drop-group`, `inflate-bytes`,
`underreport-bytes`, `touch-corpus`, `shrunk-head`), both honest controls
clean.

`rescan`'s two halves, timed directly rather than by the harness, which never
times the priming scan — fastest of five, `main` at `baf9e25`:

| half | seconds | bytes | opens |
|---|---|---|---|
| cold | 0.0398 | 5,443,825 | 63 |
| warm | 0.0259 | **0** | **0** |

Zero opens on the warm side is the reshape's whole claim in one number: every
considered file was answered from the cache without being touched, and what
remains on the clock is the walk over 11,200 files.

**Verdict:** KEPT — this is preparation, not an optimization. Nothing in
`rust/` was touched and no score was chased.

---

## 12. The loop, and when to stop

For each idea:

1. Read the journal. If it was already tried, do not try it again unless
   something has landed near it since — rejections do expire, and the Python
   bench has at least one idea that was rejected once and kept later after an
   unrelated change removed the real cause.
2. Make one change.
3. `python3 rbench/run.py --selftest` still passes.
4. `python3 rbench/run.py` — full scored run, nine rounds.
5. If it beat the noise floor, confirm at `--seed 2`, then have the referee
   score it and quote that.
6. Write the journal entry either way, with the numbers.

**Stop after three consecutive ideas that fail to beat the noise floor.**
Write a summary at the top of the journal: what you tried, what was kept and
its cumulative effect, what was rejected and why, and where you would look
next. Then idle. Do not invent speculative work to stay busy.

**If the harness itself misbehaves** — a gate fires for a reason that is
plainly not your change, a corpus will not build, the volume is wedged — stop
and report it. Do not fix it to get your run through.
