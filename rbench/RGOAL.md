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

## 2026-08-23 — ROUND TWO, OPUS: STOP SUMMARY

**Refereed score 0.6669**, against the `94739ad` pin, on branch `opus-r2` at
`db6b8c7`. Unmerged, as §10 asks.

```
run                                        load   score
self,     r2, seed 1                       5.41  0.6717
self,     r2, seed 2                       7.96  0.6829
referee,  r2, seed 1  (clean main)         5.34  0.6833
self,     scale default, seed 1            4.63  0.6722
```

Self and referee agree to 1.7%, inside the floor. Noise floor measured twice
that day on the same machine: **±7.4%** in the afternoon (geomean against
itself 1.0044) and **±3.7%** in the evening (0.9931). Every gate green in
every run; `--selftest` 5 of 5 each time.

Referee's per-scenario table, which is the one to read:

```
scenario     base s  cand s   ratio  spread     bytes   ceiling     floor
needle       0.7354  0.6030  0.8051   ±3.9%  122.1 MB  122.1 MB  133.3 KB
typical      0.1713  0.1036  0.6034   ±3.5%   70.6 MB   70.6 MB   33.0 MB
samesize     0.0269  0.0233  0.8685  ±10.4%   32.8 MB   32.8 MB   22.5 MB
widegroup    0.1879  0.1523  0.8150  ±11.9%  451.3 MB  451.3 MB  451.3 MB
photos       0.0234  0.0167  0.7123   ±2.6%  147.9 MB  147.9 MB  147.8 MB
bigheader    0.0233  0.0175  0.7439   ±1.1%  138.2 MB  146.2 MB  112.0 MB
rescan       0.0290  0.0116  0.3817   ±1.5%       0 B    5.2 MB    5.2 MB
prefixtrap*  0.0069  0.0068  1.0036   ±3.2%   16.2 MB   16.2 MB   16.0 MB
```

### Kept, in the order they landed

| # | change | commit | what it bought |
|---|---|---|---|
| 1 | `getattrlistbulk(2)` for the walk | `be5be78` | 1.0000 → 0.9587 |
| 2 | a disk image does not charge for a seek | `6319d35` | 0.9587 → 0.8272 |
| 3 | stop running SipHash over every small file | `039242d` | 0.8272 → 0.7900 |
| 4 | a reader per performance core, small-file stage only | `ae7b453` | 0.7900 → 0.7824 |
| 5 | read sibling directories concurrently | `3019856` | → 0.6833 refereed |
| 6 | compare a group against its pivot in parallel | `db6b8c7` | → **0.6669** refereed |

Two of those want the maintainer's eye before anything is taken:

- **#2 is bought where §7 says this bench is blind.** The corpus volume is a
  RAM disk. The change is confined to media the registry positively identifies
  as a disk image, so an unreporting USB enclosure keeps the conservative
  path — but nothing here tests that, and it is the largest single term in the
  score. It also moves `bigheader`'s bytes, 146.2 MB → 138.2 MB: below the
  baseline, above the floor, deterministic, legal under §1, and still a
  behaviour change.
- **#4 and #5 are the only new constants**, and both are confirmed at
  `--seed 2` and `--scale default` per §6.5.

### Rejected, with the measurement

- **`openat` on the walk's directory fd** — 1.04x at one thread, 1.00x at
  four. Path resolution is not what an open costs here.
- **The seek counter's global mutex** — twice. 0.15 → 0.15 the first time;
  and again after the compare stage went from one reader to eight, which is
  the condition that would have made it matter: 136.59 → 137.90 ms.
- **One reader pool sized from the performance cores** — 0.7936 against
  0.7900, and a §8 trade: `needle` and `typical` won, `widegroup` and
  `samesize` lost. Kept only in the narrowed form, #4.
- **A parallel sort in `candidates()`** — 1.5%, under the floor.
- **`strip = true`** — the binary loses 276 KB and startup does not move.

### Where the wall is, and it is not where the honeypot says

`typical` now runs **0.1036 s for 13,372 opens — 7.9 µs each**, which is
exactly the per-file open cost measured in C over the same tree, at the
thread count where that cost bottoms out (0.4429 s serial, 0.1526 s at four
readers, flat to worse out to ten). **`typical` is on the syscall floor.**
And on `needle` the tool completes the whole scan in **0.5520–0.6030 s**
against **0.5718 s** for a bare C loop that only opens and reads the same
50,000 files — so `needle` is on it too. Startup is
3.4 ms — 1.4 ms of it bare `fork`/`exec`, 1.7 ms dyld on a 3.1 MB binary —
and is now 20–50% of five of the seven scenarios.

**The 0.20 target is below what a byte-exact scan can reach on this machine,
and `floor.py` is why.** Its opens term counts the members of the planted
duplicate groups: 45 on `needle`. A scan that does not already know the answer
must open every file sharing a size with another — 50,000 on `needle` — because
§9 forbids ruling a file out on anything but its bytes. Re-running `floor.py`'s
own arithmetic with the honest count puts the bulk-walk honeypot at **0.4001**
with its own serial per-open figure, **0.3416** at the fastest per-open this
machine was measured to do, and **0.2533** at a hypothetical 4.0 µs nobody has
observed. Full working in the entry below. The fix is one line: count the files
in non-singleton size classes, which the generators already know.

### Where I would look next

1. **Fix `floor.py`'s opens term** and re-derive round three's target. Anything
   else here is downstream of that number being wrong.
2. **`widegroup` opens each file about three times** — 5,442 opens for 1,827
   files, one per stage. At 7.9 µs that is 43 ms of its 138 ms. Merging tier A
   and tier B is not available (it would read past the ceiling on `prefixtrap`),
   so this needs the staged pipeline to keep a descriptor, which the 256-fd
   limit forbids at this file count. It is the largest single removable term
   left and I do not have a safe way to remove it.
3. **The unique-small-file cache gap is still open and this bench still cannot
   see it.** `stages.rs:391` guards both `cache.remember` calls with
   `if group.len() > 1`, so a small file found unique is never remembered and
   a warm re-scan reads it again. Contrary to the note I was handed, the
   reshaped `rescan` does **not** exercise it: its `min_size` is 65537, above
   `DEEP_HEAD`, so `small_file_stage` never runs there — which is exactly what
   the round-two prep entry says that `min_size` was chosen for. Fixing it is a
   real win for real trees and worth nothing to this harness, so it wants a
   scenario before it wants a patch.
4. **The volume cannot hold the scored set** while round-one `-default-`
   corpora are resident: 2590 MB wanted of 2739 MB, with 424 MB already spoken
   for, and `ensure_space` evicts alphabetically so it takes `bigheader` and
   `needle` — the two costliest to rebuild — and never reaches the stale ones.
   `needle` was rebuilt before nearly every run of this round, and twice it was
   evicted out from under a measurement in progress.

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

## 2026-08-23 — round two, opus: the noise floor of the day

`--calibrate` at `r2`, nine rounds, load average 4.70 3.37 3.23, another
agent's `--scale default` corpora resident on the volume:

```
geomean of a baseline against itself: 1.0044
NOISE FLOOR: +/-7.4%   (worst scenario: widegroup at 1.0745)
  per-round dispersion, worst scenario: photos at +/-5.8%
```

Three times looser than the +/-2.4% of record taken at pin time, and the
machine is the reason: the box was carrying the user's own applications and a
second optimizer all afternoon. **+/-7.4% per scenario is the floor every
number below is judged against**, and the geomean's own floor is about
+/-0.4%. Nothing here is claimed on a per-scenario move smaller than 7.4%.

**A note on the volume, which cost me two measurements before I understood
it.** Five of the eight corpora were rebuilt during that calibration, `needle`
among them, which RGOAL already warns is the signature of a set that does not
fit. The cause is not the r2 sizing: 424 MB of round-one `-default-` corpora
were resident, `ensure_space` evicts alphabetically and so takes `bigheader`
and `needle` first and never reaches them. They are another contestant's live
working set — they were created at 15:13 today — so they are not mine to
delete and I left them alone. The consequence for anyone reading these
numbers is that `needle` was rebuilt before most runs, and that ad-hoc timings
taken outside `run.py` are worthless while a second harness is churning the
volume: I lost one set of `needle` figures to a corpus that was evicted out
from under the measurement mid-loop.

**Verdict:** of record, for this round only.

---

## 2026-08-23 — the walk reads a directory in one call

**Hypothesis:** the walk pays one `lstat(2)` per file and REWRITE.md §4 wants
that gone. `getattrlistbulk(2)` returns a whole directory's names *and*
attributes in one call, and on HFS+ and APFS that is one pass over the catalog
rather than a lookup per entry. Round one left this unclaimed; ox's attempt
scored 1.0148 and RGOAL asked that the next person not treat that as a
ceiling.

**Change:** `sys::BulkDir` — the `getattrlistbulk` FFI, plus a parser that
carries the kernel buffer, a name arena and the parsed entries across
directories, so a directory of 50,000 entries costs one allocation and not
50,000. `walk()` uses it and falls back to `read_dir` per directory for
anything the platform will not describe completely; `BulkDir::read` answers
`false` without ever having reported a partial directory, so the fallback
cannot double count. The relative path is now built by pushing onto the
parent's prefix rather than by stripping the root off a fresh `PathBuf`,
which also removes `relative_path`'s `.replace('\\', "/")` — a call that
allocates a second `String` on every entry whether or not there is a
backslash in it.

Two things worth writing down for whoever touches this next:

- **The kernel packs the attribute buffer with no padding.** `ATTR_FILE_DATALENGTH`
  lands on a 4-byte boundary behind `ATTR_FILE_LINKCOUNT`, so almost nothing in
  an entry is naturally aligned and every field has to be read unaligned. A
  parser that assumes alignment will read plausible-looking rubbish rather than
  crash.
- **`attr_dataoffset` counts from the `attrreference_t` itself**, not from the
  entry and not from the cursor. I had this wrong first and the bug survived a
  clean build; what caught it was checking the parse against `lstat` field by
  field rather than eyeballing the offsets.

**Numbers.** Full scored run, nine rounds, load average 9.93 7.21 5.55 — a
busy machine, and the spreads say so:

```
scenario     base s  cand s   ratio  spread     bytes   ceiling     floor
needle       0.6925  0.6733  0.9826   +/-2.7%  122.1 MB  122.1 MB  133.3 KB
typical      0.1643  0.1570  0.9478   +/-1.4%   70.6 MB   70.6 MB   33.0 MB
samesize     0.0239  0.0234  0.9886   +/-6.0%   32.8 MB   32.8 MB   22.5 MB
widegroup    0.2178  0.2246  1.0157  +/-14.6%  451.3 MB  451.3 MB  451.3 MB
photos       0.0309  0.0287  0.9570  +/-12.1%  147.9 MB  147.9 MB  147.8 MB
bigheader    0.0321  0.0386  1.0530  +/-26.0%  146.2 MB  146.2 MB  112.0 MB
rescan       0.0321  0.0245  0.7898   +/-4.8%       0 B    5.2 MB    5.2 MB
prefixtrap*  0.0085  0.0080  0.9255  +/-21.1%   16.2 MB   16.2 MB   16.0 MB

SCORE: 0.9587
```

Noise floor that day +/-7.4%. Only two scenarios moved outside their own
spread and both are the walk-heavy ones: `rescan` at **0.7898 (+/-4.8%)`,
whose warm scan reads nothing at all and is therefore almost pure walk, and
`typical` at **0.9478 (+/-1.4%)**. Everything else is inside the noise of a
load-9.93 machine. `--selftest` 5 of 5. Output is byte-identical to the
baseline on every corpus and every reported field, checked before timing.

**This is a smaller win than the honeypot implies, and the reason matters.**
`floor.py` costs the walk at one `lstat` (1.07 us) against `readdir` (0.25 us)
and puts the bulk-walk floor at 0.1431. Measured, the whole walk went from
about 1.56 us to about 1.05 us per file — roughly 1.5x, not 4x. `getattrlistbulk`
is not free: it does the same catalog work, just once per directory instead of
once per file, and on a volume whose catalog is entirely in RAM the per-file
lookup it removes was never the 1.07 us the model charges it.

**Verdict:** KEPT (`be5be78`). Real, gates green, output identical — but the
walk is only about a fifth of `typical` and a sixteenth of `needle`, so a
1.5x on it cannot be worth more than it was.

---

## 2026-08-23 — a disk image does not charge for a seek

**Hypothesis:** `sequential_jobs()` returns **1** for any volume whose medium
the registry will not name, and `all_solid_state()` gives that volume the
16 MB spinning-disk compare chunk. The bench volume is a RAM disk, and
`solid_state()` answers `None` for it — the doc comment on that function says
so in as many words. So the stage that streams whole files in step has been
running single-threaded on every scored scenario. RGOAL parked this after
round one as unproven, measured inside a +/-2.9% floor; at `r2` scale
`widegroup` is 3.6x larger, and §12.1 says rejections expire.

**Change:** `sys::disk_image()` asks the registry a *different* question —
is this device provided by `IODiskImageBlockStorageDeviceOutKernel` — and
`device::is_solid_state` falls back to it when `solid_state()` says `None`.
`solid_state()` itself is untouched, because an existing test asserts it
agrees with `diskutil`'s `SolidState`, and `diskutil` says "Info not
available" here; widening it would have broken that agreement.

**This is deliberately not "treat unknown media as flash".** That is the
version §7 warns about, and it is wrong for the USB enclosure that reports
nothing and may genuinely spin — eight sequential readers on real heads is a
seek storm. The registry separates the two cases: a disk image has no heads,
nothing with heads is provided by that class, and an unreporting enclosure is
left exactly where it was.

**Numbers.** Full scored run, nine rounds, load average 3.97 6.62 6.08:

```
scenario     base s  cand s   ratio  spread     bytes   ceiling     floor
needle       0.8166  0.7875  0.9644  +/-19.4%  122.1 MB  122.1 MB  133.3 KB
typical      0.1723  0.1641  0.9483  +/-19.0%   70.6 MB   70.6 MB   33.0 MB
samesize     0.0275  0.0236  0.8696   +/-4.5%   32.8 MB   32.8 MB   22.5 MB
widegroup    0.1922  0.1439  0.7831   +/-5.5%  451.3 MB  451.3 MB  451.3 MB
photos       0.0251  0.0179  0.7153   +/-4.7%  147.9 MB  147.9 MB  147.8 MB
bigheader    0.0231  0.0174  0.7552   +/-1.7%  138.2 MB  146.2 MB  112.0 MB
rescan       0.0269  0.0213  0.7879   +/-2.1%       0 B    5.2 MB    5.2 MB
prefixtrap*  0.0072  0.0077  1.0232   +/-9.4%   16.2 MB   16.2 MB   16.0 MB

SCORE: 0.8272
```

Against the walk change alone at 0.9587, on the same harness: `widegroup`
1.0157 -> **0.7831**, `photos` 0.9570 -> **0.7153**, `bigheader` 1.0530 ->
**0.7552**, `samesize` 0.9886 -> **0.8696**. All four outside their spreads.
`rescan` did not move, which is the check that this is the read stages and
not something else: `rescan`'s scored half reads nothing. `--selftest` 5 of 5.

**Two disclosures, both §8.**

- **It moves bytes.** `bigheader` reads **138.2 MB** where the baseline read
  146.2 MB — below the ceiling, above the 112.0 MB floor, deterministic across
  all nine rounds. The compare chunk changed from `SLOW_CHUNK` to `CHUNK`, so
  a mismatch is found after less of the file. Reading less is legal under §1
  and it is still a behaviour change.
- **This bench cannot see the cost of getting it wrong.** §7 names the corpus
  volume as a RAM disk that reports no medium, which is exactly the case that
  is free here and is not free on a real external drive. The class check is
  what confines the change to media that genuinely has no heads, but nothing
  in this harness tests that claim, and nobody should take the score as
  evidence for it.

**Verdict:** KEPT on this branch (`6319d35`), and flagged for the maintainer
rather than claimed. It is the single largest term in this round's score and
it is bought in the one place §7 says the bench is blind.

---

## 2026-08-23 — two ideas measured and dropped before they cost a scored run

Both were killed with a throwaway build and a C probe rather than a nine-round
run, which is the only reason there was time for the ones above.

**The seek counter's mutex.** `Effort::read_block` takes a global
`Mutex<Option<At>>` twice per read — once in `charge_seek`, once to store where
the read stopped. With eight readers and 50,000 files that looked like the
obvious serialiser. A build with the mutex removed entirely (seeks left wrong
on purpose, purely to size the prize) moved `typical` 0.15 -> 0.15 and
`widegroup` 0.23 -> 0.21, both inside noise. **The lock is not the
bottleneck.** Not pursued, and `effort.rs` is untouched.

Worth recording beside it: `seeks` is *already* nondeterministic under the
parallel readers, 2045 or 2046 on the same corpus from run to run, on the
pinned baseline as much as on this branch. `bytes_read` is exactly
deterministic on both, which is what gate 7 actually checks.

**`openat` on a held directory fd.** The walk now has a directory descriptor
in hand, so every later `open(root.join(name))` could have been
`openat(dirfd, name)` and skipped resolving the parent components. Measured in
C over `typical`'s 19,200 files: **1.04x at one thread, 1.00x at four, 0.98x
at ten.** Path resolution is not what an open costs here. Dropped.

What the same probe did establish, and what shaped everything after it:
**the open path saturates at four threads.** 0.4429 s serial, 0.1526 s at
four, and flat to worse out to ten — about 7.9 us per file no matter how many
readers ask. That is the wall `needle` and `typical` are against, and it is
why neither of them moved much for any of the work above.

**Verdict:** both REJECTED, on measurement, before a scored run was spent.

---

## 2026-08-23 — the small-file stage was hashing every byte it read, with SipHash

**Hypothesis:** `Ordered` indexes its slots by a hash of the key, and in the
small-file stage the key *is* a whole file's contents. So every byte that
stage reads is also a byte it hashes, through `DefaultHasher`, which is
SipHash. That is 122 MB of SipHash on `needle` and 70 MB on `typical`, none
of which the profile of that stage should be spending on hashing.

**Change:** a multiply-and-rotate mixer in `ordered.rs` in place of
`DefaultHasher`. This is safe for a reason worth stating plainly rather than
assuming: **the index is a hint and never an answer.** `push` compares the
key itself before it believes a match, so a weaker hash costs one extra
comparison and cannot produce a wrong grouping. It also cannot reorder
anything — `slots` stays in insertion order, which is what the `RETAIN_BUDGET`
walk depends on and what `ordered.rs`'s own doc comment says is observable.
And the digest goes on to be a `HashMap` key, where that map hashes it again
with its own SipHash, so this mixer does not have to spread its output well.
It only has to depend on every byte and on the length, which it is tested for
— including on the `prefixtrap` shape, a long shared prefix and a short
parting.

**Numbers.** Full scored run, load average 5.61 5.09 5.48:

```
needle       0.7065  0.6689  0.9393   +/-3.9%
typical      0.1809  0.1570  0.8665  +/-10.6%
samesize     0.0300  0.0239  0.9048  +/-12.2%
widegroup    0.2071  0.1432  0.7476   +/-6.2%
photos       0.0249  0.0179  0.7171   +/-3.8%
bigheader    0.0292  0.0174  0.6424   +/-6.4%
rescan       0.0272  0.0212  0.7571   +/-3.3%
prefixtrap*  0.0085  0.0084  1.0515   +/-6.1%

SCORE: 0.7900   (from 0.8272)
```

The gain lands where the hypothesis says it should — `typical` 0.9483 ->
0.8665 and `needle` 0.9644 -> 0.9393, the two scenarios made of small files.
`--selftest` 5 of 5, output byte-identical.

**Verdict:** KEPT (`039242d`).

---

## 2026-08-23 — reader counts: one pool sized wrong, then two sized right

**Hypothesis:** `cpu_bound(8)` puts eight readers on a machine with four
performance cores and six efficiency ones. A C probe over `typical`'s 19,200
files said the open path saturates at four: 0.4429 s serial, 0.1526 s at
four, and flat to slightly worse out to ten. So eight readers are queueing,
not working.

**First change, and it was wrong.** Sizing `random_jobs` from the performance
cores. Measured with `--jobs` before writing anything, the two stages want
opposite things:

```
                      j3      j4      j5      j6      j8      j10
typical            0.1415  0.1201  0.1408  0.1460  0.1515  0.1547
widegroup          0.2682  0.2237  0.1565  0.1639  0.1456  0.1448
```

Scored, that came out **0.7936 against 0.7900** — no gain, and a
cross-scenario trade underneath it: `needle` 0.9393 -> 0.8170 and `typical`
0.8665 -> 0.7220, against `widegroup` 0.7476 -> 0.8700 and `samesize`
0.9048 -> 1.0374. §8 says a trade like that is not mine to keep, and the
geomean did not move anyway. Reverted.

**Second change, kept.** `random_jobs` drives two different things: the
small-file stage, which opens a file, reads all of it and moves on, and the
head/tail probes, which read one page out of a much larger file. Only the
first is pure latency on a shared catalog. So the small-file stage gets its
own pool sized from the performance cores, built lazily inside
`if !small.is_empty()` so a tree with nothing under `DEEP_HEAD` never
allocates it, and the probes keep `cpu_bound`.

```
needle       0.7076  0.5797  0.8190   +/-2.5%
typical      0.1692  0.1169  0.6920   +/-3.0%
samesize     0.0256  0.0231  0.8658   +/-8.4%
widegroup    0.2005  0.1498  0.7995   +/-8.4%
photos       0.0245  0.0196  0.8230   +/-4.7%
bigheader    0.0231  0.0173  0.7452   +/-3.1%
rescan       0.0279  0.0213  0.7459   +/-1.9%
prefixtrap*  0.0070  0.0068  0.9582   +/-3.0%

SCORE: 0.7824   (from 0.7900),  load average 11.40 8.61 7.00
```

**Read the last three rows as noise and here is the proof.** `widegroup`,
`photos` and `bigheader` have no file below `DEEP_HEAD` — 247 KB, 3.9 MB and
56 MB per file — so `small.is_empty()` holds and their code path is
byte-for-byte what it was in the previous run. `photos` still moved 0.7171 ->
0.8230 between the two. **That is what a run-to-run per-scenario comparison
is worth on this machine at load 11.4: about +/-15%, twice the +/-7.4% the
calibration measured at load 4.7.** Only `needle` and `typical`, which are
made of small files, moved for a reason.

**Verdict:** first REVERTED (no gain, and a §8 trade); second KEPT
(`ae7b453`).

---

## 2026-08-23 — the honeypot counts the opens needed to check the answer, not to find it

This is not an optimization. It is a correction to `floor.py`, offered because
this file's own preamble says that where it and the code disagree the code is
right and the file is stale. Here I think it is the other way round, in a way
that set this round's target below what any correct tool can reach.

`floor.py:294` costs the opens term as

```python
opens = 0 if warm else sum(len(g) for g in ceiling["groups"])
```

— the members of the planted duplicate groups, and nothing else. That is the
number of opens needed to **verify a known answer**. A scan does not know the
answer. Under §9's byte-exactness rule a file can only be ruled *out* of a
group by reading it, so every file that shares a size with another has to be
opened, and only files whose size is unique can be skipped. On `needle` the
floor charges **45 opens**; every correct implementation must do **50,000**,
which is what both the tool and the frozen oracle actually do.

Re-running `floor.py`'s own arithmetic, with its own constants, changing only
the open count to the honest one — and costing the walk as a bulk directory
read throughout, which is the *generous* direction:

```
  scenario     floor opens  honest opens   published   honest
  needle            45        50000          6.6%      82.5%
  typical         1853        13372         23.9%      93.5%
  samesize          91         1402         17.9%      74.9%
  widegroup       1820         1827         21.2%      20.7%
  photos            49          100         32.2%      35.1%
  bigheader          8            8         23.7%      24.1%
  rescan             0            0         44.0%      16.3%

  published bulk-walk honeypot : 0.1431      (0.2131 with an lstat walk)
  same arithmetic, honest opens: 0.4001
```

`per_open` is measured serially and the tool opens in parallel, so that 0.4001
is an overestimate. Substituting the fastest per-file open this machine was
measured to do — 7.9 us, four threads, open plus read plus close — gives
**0.3416**. Even a hypothetical 4.0 us, faster than anything observed here,
gives **0.2533**.

**So the round-two target of 0.20 sits below the floor of any byte-exact
implementation on this machine, and 0.1431 is roughly a factor of two below
it.** The error is concentrated in `needle`, whose 6.6% is the single term
that pulled the published geomean down, and which is really about 60–80%.

Nothing here touches the gates, and none of it makes a slow candidate look
fast — the floor is a target-setting tool, not a scoring one. But RGOAL's own
account of why the floor exists is that "the predecessor bench set a target
below what the hardware charges once; a target like that can only be hit by a
candidate that stopped doing the work." That is exactly the failure mode this
row reproduces, one level up: the hardware charge is right and the *work* is
undercounted.

**Suggested fix**, for whoever owns `floor.py`: count the opens as the number
of files sharing a size with at least one other file, which the generators
already know and which is what both the oracle and the tool do. Then the
`needle` row reads about 82% instead of 6.6% and the honeypot lands near 0.34.

**Verdict:** reported, not acted on. `rbench/` is not mine to change (§9),
and this is a claim about the target rather than about any candidate's score.

---

## 2026-08-23 — the walk goes wide, and rescan halves

**Hypothesis:** once the bulk read landed, the walk *was* `rescan`. Measured
with `--min-size` set above every file, so the tool walks and reads nothing:
`rescan` spends **86.8%** of its scan in the walk, and `typical` 24.1%. That
is the shape of the scored half of `rescan` by construction — it reads no
bytes, so all it can do is walk and ask the cache. And the walk was one
thread.

**Change:** one task per directory, on a pool of its own sized like the
small-file pool and for the same reason — these are directory reads against a
single catalog. Each task builds its own directory's records into a local
vector and appends the whole of it under one lock, so the shared state is
touched once per directory and never per file. The kernel buffer and name
arena moved to a thread-local, because `BulkDir` carries 128 KiB and a walk
visits thousands of directories; one per worker rather than one per
directory.

**Why the order does not matter, checked rather than argued.** `candidates`
sorts by size and then by `(device, inode)`, and after hardlink folding that
is a unique key, so the records it hands on are in the same order whatever
order the tasks finished in. `main` sorts and dedups `unreadable` before
reporting it. That is the argument; the check is 20 runs per corpus, each
producing exactly **one** distinct answer over `groups`, `bytes_read`,
`files_seen`, `opens` and `unreadable` — on every corpus and on a tree of
symlinks, hardlinks, a fifo, unicode and backslash names.

**Numbers.**

```
walk only (min-size above every file)   old        new
  typical                            67.30 ms   23.17 ms
  rescan                             42.20 ms   17.94 ms
  widegroup                          10.43 ms   10.01 ms   (too few directories)

scored, rayon's global pool | own pool sized to the performance cores
  rescan       0.3817 (+/-7.7%)    |  0.4711 (+/-4.6%)
  typical      0.8312 (+/-1.1%)    |  0.8105 (+/-3.9%)
  SCORE        0.7542              |  0.7661
```

**Both scored runs are worth less than they look and the reason is in the
`base s` column.** They were taken at load average 12.06 and 13.84, with a
headless Chromium belonging to something else on this machine taking 616% of
the CPU. `needle`'s own baseline read 1.1293 s and then 1.6144 s against the
0.6925–0.7076 s it reads on a quiet box — the machine was running at half
speed and drifting while it did. The two forms of the change cannot be told
apart at that noise, and `needle` at 1.0394 +/-11.2% says nothing at all. The
one thing that is far outside it is `rescan`, which halved.

The pooled form is kept over the global-pool form on grounds other than
score: the global pool would have been a third set of threads alive beside
the two the read stages already build, on a machine with ten cores.

**Verdict:** KEPT (`3019856`), on `rescan`'s move. The rest of the table from
these two runs should not be quoted, and is not.

---

## 2026-08-23 — the floor, proved; three ideas that did not clear it; the stop

**A correction to my own strike count first.** I wrote a stop summary one idea
early. The rule is three *consecutive* ideas inside the floor, and a win resets
the count. My order was: bulk walk (win), disk image (win), hash (win), one
pool sized from the performance cores (fail), small-file pool (win), parallel
walk (win), and only then the failures below. The pool-sizing failure had two
wins after it and never counted toward a stop. Recorded because a
miscounted stop is a contestant quitting with ideas on the table, which is
exactly what §12 exists to prevent.

### First, the two scenarios that dominate the score are provably finished

Not argued — measured, with a C program that does nothing but `open`,
`read`, `close` over the same trees, at the thread count where that cost
bottoms out.

```
                     bare C open+read+close       the whole tool
  typical  19,200 files   0.1526 s @ 4 threads     0.1036 s   (13,372 opens, 7.9 us each)
  needle   50,000 files   0.5718 s @ 8 threads     0.5520-0.6030 s
```

**On `needle` the tool does the entire scan — walk, 50,000 opens, 122 MB read,
content hashing, grouping, the byte-for-byte proof and the JSON report — in
the time a bare C loop takes to open and read the same files and do nothing
with them.** On `typical` its per-open cost is exactly the saturated rate.
There is no headroom left in either, and between them they are two of the
seven terms.

Note also that `needle`'s opens are dearer than `typical`'s — 26.23 us serial
against 23.19 us, 11.4 us against 7.9 us at best parallelism — on the same
volume, with the same code. A bigger catalog costs more per lookup. That is
the "4 us per file of overhead" I spent an hour looking for in the profile:
it was never overhead, it was the open.

### Then three ideas, none of which cleared ±3.7%

**1. The seek counter's global mutex, re-tested.** Rejected earlier this round
on a 0.15 -> 0.15. Re-tested on purpose because the condition changed
underneath it — the disk-image fix took the compare stage from **one** reader
to eight, which is when a shared lock would start to matter. It still does
not: `widegroup` 136.59 ms with it against 137.90 ms without, `typical` 97.90
against 98.66. Rejected twice, the second time deliberately.

**2. `par_sort_unstable_by_key` in `candidates()`.** It sorts every walked
record twice, 50,000 of them on `needle` at ~200 bytes each. Safe here: the
first sort's only ties are hardlinks of one inode, whose names `fold_names`
sorts afterwards, and the second sort's key is unique once folding has
happened. Measured on `typical`: **96.41 ms against 97.90 ms**, 1.5%. Under
the floor, and §6.2 says not to keep a change on a 2% improvement.

**3. `strip = true` on the release profile.** Startup is 3.4 ms and is now
20-50% of five of the seven scenarios; 1.65 ms of it is dyld on a 3.1 MB
binary, so a smaller binary looked like the last shared lever. It shrinks to
2,898,544 bytes from 3,174,512 and buys nothing:

```
              --help    empty tree   rescan
  stripped    2.80 ms     3.04 ms    10.39 ms
  unstripped  2.89 ms     2.98 ms    10.36 ms
```

Differences of 1-3% in both directions. macOS does not page a symbol table in
at load, so the bytes removed were never on the critical path.

**Three consecutive ideas inside the floor. This is the stop under §12.**

## 2026-08-23 — the pivot comparison was serial, and three more that were not worth keeping

**Where this came from.** I had already written a stop summary saying the tool
was at its floor. It was not, and the thing that showed it was arithmetic
rather than a hunch: `widegroup` was spending 153 ms against about 83 ms of
work I could account for. Reading its 452 MB takes **33.5 ms** — measured, in
C, at the thread count where it bottoms out, and **14.2 GB/s** rather than the
27 GB/s `floor.py` measures with 48 MB probe files, because widegroup's files
are 247 KB. Seventy milliseconds were unexplained on a scenario worth a
seventh of the score, and a scenario I had just called finished.

**Change:** `partition_blocks` picks the first block as a pivot and `memcmp`s
every other block against it — one at a time. Those comparisons are
independent, and on a wide group of large blocks they are the whole stage:
`widegroup` puts 227 blocks of a quarter megabyte through a pass, per group,
eight groups of them. The verdicts now go into a `Vec<bool>` in parallel and
the split happens afterwards, so a run holds its members in exactly the order
it always did and no block moves — only the `memcmp` is spread.

**Numbers**, referee, clean checkout of `main`, load average 4.49:

```
scenario     base s  cand s   ratio  spread     bytes   ceiling     floor
needle       0.6911  0.5174  0.7486   ±3.7%  122.1 MB  122.1 MB  133.3 KB
typical      0.1710  0.1018  0.5952   ±1.1%   70.6 MB   70.6 MB   33.0 MB
samesize     0.0262  0.0234  0.9100   ±5.2%   32.8 MB   32.8 MB   22.5 MB
widegroup    0.1719  0.1252  0.7353   ±6.5%  451.3 MB  451.3 MB  451.3 MB
photos       0.0227  0.0178  0.7384   ±3.6%  147.9 MB  147.9 MB  147.8 MB
bigheader    0.0228  0.0163  0.7172   ±2.8%  138.2 MB  146.2 MB  112.0 MB
rescan       0.0268  0.0100  0.3715   ±0.7%       0 B    5.2 MB    5.2 MB
prefixtrap*  0.0067  0.0067  1.0083   ±2.8%   16.2 MB   16.2 MB   16.0 MB

SCORE: 0.6669   (referee, from 0.6833)
```

Confirmed at `--seed 2` and `--scale default`, gates green in all of them.
Re-profiled after: `split_identical` fell from 10.6% of `widegroup` to 6.1%
and `memcmp` is 1.5%, which is the change doing what it claims.

**§8 disclosure: this trades `samesize` away.** Across every run before and
after, on both scorers and both scales:

```
  before: 0.8433  0.8684  0.8685  0.9186        (mean 0.875)
  after : 0.9100  0.9437  0.9551  0.9575  0.9614  0.9655   (mean 0.949)
```

About eight percent, consistently, against wins on `widegroup`, `needle`,
`typical` and `rescan` that net a refereed 0.6833 → 0.6669. `samesize`'s
absolute times are 0.0234 s against 0.0262 s, so a single millisecond is four
percent of that ratio and the scenario is the noisiest of the seven — but the
sign is the same in nine runs out of nine and I do not think it is noise.
**This is a cross-scenario trade and it is the maintainer's to judge, not
mine to keep.** It stays on this unmerged branch with the table above.

I tried twice to keep the win without the trade and failed both times; both
attempts are strikes below.

---

## 2026-08-23 — three more inside the floor, and the stop

**1. Sizing the parallel comparison by bytes per task** (`with_min_len`).
The theory was that `samesize` regressed because it dispatches many tiny
comparisons. It does not: its compare blocks are whole ~296 KB files, not the
4 KB head pages I had assumed. **0.6772 against 0.6537** — worse. Reverted.

**2. Raising `PARTITION_SPREAD` to 8 MB**, to leave `samesize` serial while
`widegroup` still spreads. Two scored runs, both contaminated beyond use — a
headless Chromium belonging to something else on this machine at 277% with
two more instances behind it, plus a VPN client at 83%. `needle`'s own
baseline read 0.98 s and then 1.38 s against 0.687 s, and `rescan` moved
0.3752 → 0.4611 on a code path the change cannot reach: its warm half never
enters the compare stage at all. **No demonstrated benefit, so not kept.**
Recorded as a failure rather than as an unknown, which is what an
unmeasurable change is worth.

**3. One allocation instead of three in `read_batched`.** It built a vector
per chunk, collected those into a vector of vectors, then flattened into a
third — on `needle`, eight hundred allocations and a full extra pass over
fifty thousand results, to arrive at the order it already had.
`with_min_len` batches identically and collects straight into one vector.
Measured on comparable baselines (`needle` 0.6903 against 0.6865):
**0.6746 against 0.6537**, and every scenario moved the same direction,
including `needle` whose spread was ±0.5%. Neutral at best. Reverted.

**Three consecutive ideas inside the floor. This is the stop under §12** —
this time with the count kept properly: a win resets it, and the last win was
the pivot comparison above.

### What is left, for round three

- **`widegroup` still has about 60 ms over a hand-rolled floor** (33.5 ms to
  read its 452 MB, ~22 ms for the two extra opens per file, ~7 ms of `memcmp`,
  3.4 ms of startup). Half its profile sits in rayon's `bridge_producer_consumer`
  and `join_context` frames, and with LTO inlining the closures into them I
  could not attribute it further with `sample`. That is where I would point a
  real profiler first.
- **The three opens per file** are worth 10.8 ms at four threads and 21.7 ms
  at eight, measured. Removing them needs the staged pipeline to hold a
  descriptor across stages, which the 256-descriptor limit forbids at this
  file count without raising it.
- **`samesize` is the worst scored scenario at 0.91** and the one this round
  made worse.

---

## 2026-08-23 — a measurement correction, then three more inside the floor

**First, a correction to my own probes, because it changed a conclusion.**
The C programs I used to establish "the tool is at the syscall floor" handed
work out through a mutex, one file per lock pair — 50,000 of them on `needle`.
The tool uses rayon with 64-file batches. Re-run with a batched handout and no
per-file lock, `widegroup`'s 452 MB reads in **0.0133 s (35.6 GB/s)** where my
earlier probe said 0.0337 s (14.2 GB/s). **My floor was 2.5x too pessimistic**,
which is how "the tool is faster than a bare C loop" happened — it was not;
my C loop was slow. `typical` survives the correction (13,372 opens in
0.1018 s, 7.61 µs each, against 7.80 µs from the batched probe) and is on the
floor. `widegroup` did not, and the parallel pivot compare above came out of
noticing that.

**A probe is a claim too.** It gets the same scepticism as the tool.

### 1. Clamping the compare's allocation to what the file can give

`read_block` allocates what it is asked for, and the compare asks for `span`.
I expected that to be `CHUNK`, four megabytes, against a `widegroup` member of
a quarter of one — and measured in C that allocating 4 MB per 247 KB file
costs **2.45x** (0.0346 s against 0.0849 s). It is not four megabytes.
`compare_chunk` is `MIN_CHUNK.max(CHUNK.min(COMPARE_BUDGET / members))`, so a
227-member group gets **564 KB**, and the over-allocation is 2.3x rather than
16x. Clamped it anyway and measured: candidate times **0.1247 s against
0.1258 s** on `widegroup` and 0.0987 against 0.0977 on `typical` — identical.
Reverted.

Worth keeping the arithmetic in mind: the budget divides the chunk by the
group width, so wide groups already read in small pieces.

### 2. More readers for the compare

The jobs curve on `widegroup`, current branch: 352.55 ms at one reader,
123.82 at eight, 120.23 at twelve, 118.37 at sixteen, 117.19 at
twenty-four. **Five percent for three times the threads**, which is under the
floor on one scenario and worth about 0.7% of the geomean. Not taken.

That curve says something more useful than its own answer. By Amdahl, one
reader at 352.55 ms against twenty-four at 117.19 puts **about 30% of
`widegroup`'s work on one thread**, and that serial third is not an oversight:
the groups are processed one at a time because doing eight at once would hold
8 × 227 × 564 KB — a gigabyte — against a `COMPARE_BUDGET` of 128 MB. The
memory budget forces it.

### 3. A wider pool for the walk

`rescan` is walk-bound, so the walk pool's width is worth checking rather than
assuming. Sweeping it:

```
  walk pool     rescan     typical
      4        10.20 ms    97.01 ms
      6        11.53 ms    95.97 ms
      8        12.94 ms    98.49 ms
     10        13.49 ms    97.72 ms
```

Four — a reader per performance core — is best, and it degrades steadily from
there. The existing sizing was already right, which is the answer even though
it is not a win.

*(The first attempt at this sweep produced four identical numbers because a
regex broke the build each time and every run measured the same stale binary.
Four results that agree perfectly are a reason to check the build, not to
believe them.)*

**Three consecutive ideas inside the floor. This is the stop under §12.**

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
