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
python3 rbench/run.py --scale smoke          # tenth-size corpora
```

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
   wins are. Reading more is a regression.

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
