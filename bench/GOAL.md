# GOAL — autonomous optimization of `dedup`

You are an optimizer. You have a shell on this Mac and write access to this
repository, and you have no prior context about it. This file is the whole
briefing: the mission, the machine, the harness, the rules, and the map of the
code. (Round one was run by two of you, on branches `opus` and `luna`; both
journals are on `main` in `bench/LOG.md`. Those are names in the history now,
not names for you.)

Read it all before you touch anything.

If this document ever contradicts the code or the harness, the code is right and
this document is stale — log the discrepancy in `bench/LOG.md` and trust what
runs.

The repository is at `/Users/dogaozden/AI_Projects/dedup`. All paths below are
relative to it unless written out in full.

---

## 1. Mission

`dedup` finds byte-exact duplicate files and moves the extras to the Trash. It
is a deletion tool, so it must never be wrong. It is also a tool people point at
slow external disks holding hundreds of gigabytes, so it must be fast.

Your job is to make it faster, measurably, without weakening a single guarantee.

This is open-ended maximization, not a fixed target. There is no number to hit.
Keep finding wins until you run out of ideas that beat the noise floor.

Three metrics, in this priority order:

1. **Bytes read.** What `dedup` pulls off disk to reach its answer.
   Deterministic — same corpus, same answer, same count, every time. It predicts
   the slow-disk reality where this tool is disk-bound rather than CPU-bound. A
   bytes win is the most valuable thing you can produce.
2. **Wall time**, as measured by the harness's interleaved min-of-N rounds.
   Real, but noisy on this machine. See section 2.
3. **Peak RSS.** Worth improving, never worth a regression in the other two.

A win in bytes beats a win in time. A win in time beats a win in RSS.

The robustness gates (correctness, deletion safety, adversarial cases, the
pytest suite) are not metrics. They are gates. They are always green or the run
did not happen.

`python3 bench/run.py --score` folds the first two metrics into a single number
for the whole run — section 3 defines it. It is a way of reporting where you
have got to, not a target to aim at: there is still no number to hit, and a
score is worthless the moment a gate is red or bytes have regressed anywhere,
which is why the harness refuses to print one in either case. `main` currently
scores **~0.85**.

---

## 2. The machine

- macOS on Apple Silicon (Apple M4, 10 cores, 16 GB RAM).
- `python3` is **3.9.6**. Not 3.11, not 3.12. Code that only parses on a newer
  interpreter will not run here. `dedup.py` relies on
  `from __future__ import annotations` for its `list[FileRec]` and
  `bytes | None` annotations — keep that import.
- The boot disk is nearly full. The data volume sits at ~94% (840 GB used of
  926 GB). Free space moves around and can disappear without warning.
  **Never write anything sizable outside the RAM disk.** Before writing more
  than a few hundred megabytes anywhere, run `df -h /` and look.
- `/tmp` is purged by the OS under disk pressure. Do not put a corpus there, do
  not put results there, do not assume anything you left there is still there.
- The RAM disk lives at `/Volumes/dedupbench` and the harness creates and
  manages it. Everything large — corpora, scratch files, work-in-progress
  copies of the tool under test — belongs there.
- 16 GB of RAM total, and the RAM disk consumes it. An oversized RAM disk pushes
  the machine into swap, and swap lands on the nearly-full boot disk. If you
  ever change corpus scale, watch memory.
- The ten corpora leave roughly 150 MB spare on the 2816 MB volume, so anything
  else living there matters. `.fseventsd` — macOS's own event spool, which
  every corpus build feeds — reached 88 MB across one day of runs and was
  enough on its own to fail `ensure_space`. It is regenerable log data, not
  corpus and not results: clear it (`rm -rf /Volumes/dedupbench/.fseventsd/*`)
  before concluding a run cannot fit. Do not resize the RAM disk instead.
- **Absolute timings on this machine drift by 2–3× depending on what else is
  running.** A number you measured an hour ago is not comparable to a number you
  measure now. Only the harness's interleaved min-of-N comparison is
  trustworthy, and even then, for anything under ~5% prefer the bytes number.
- The RAM disk may or may not report itself as flash. `dedup` asks the volume
  (`diskutil info -plist`) and picks its defaults from the answer: flash gets 8
  readers, 4 parallel directory walkers, and 4 MB compare chunks; anything else,
  including "don't know", gets 2 readers, 1 walker, and 16 MB chunks. Run
  `diskutil info /Volumes/dedupbench | grep -i solid` once and know which code
  path your benchmark is actually exercising.

---

## 3. The benchmark harness

`bench/run.py` is the only measurement that counts. Do not invent your own
timing scripts and do not quote numbers from anything else.

### The full run

```
python3 bench/run.py
```

That does all of this, in order:

1. Ensures the RAM disk exists at `/Volumes/dedupbench`.
2. Generates the corpora (deterministic, from the seed).
3. Runs the pytest suite as a gate.
4. Runs every scenario, A/B against the baseline pinned in `bench/baseline.lock`.
5. Writes `bench/state/results.json`.

Exit codes:

- `0` — every gate passed. Only then do the numbers mean anything.
- `1` — a gate failed. Your change is wrong. The numbers are void.
- `2` — environment error (RAM disk, corpus generation, missing baseline, an
  open-file limit under 2048). Not your change's fault. See section 9 for
  what to do.

### Flags

- `--scenario NAME` — run just this scenario. Repeatable.
- `--seed N` — corpus seed, default `1`.
- `--scale smoke|default` — `smoke` is the small, fast corpus for the inner
  loop; `default` is the real one.
- `--rounds N` — timing rounds, default `3`.
- `--no-tests` — skip the pytest gate. Inner loop only. Never for a result you
  intend to keep.
- `--no-baseline` — measure the current tree alone, no A/B.
- `--dedup PATH` — benchmark a work-in-progress copy of `dedup.py` instead of
  the one in the tree. This is how you try an idea without committing it.
- `--selftest` — proves the gates still catch sabotage. Run it after any change
  you make to anything under `bench/`.
- `--score` — reduce the whole run to one number. Section 3's "The score" has
  the definition. Implies `--rounds 9`, and refuses `--no-tests` and
  `--no-baseline`.
- `--root PATH` — override the RAM disk location. Any existing writable
  directory will do, including one on the boot disk: the gates are entirely
  valid there and it is the quickest way to check them. Timings taken there are
  not, and the run says so on its first line.

Only one harness run may use a volume at a time. `run.py` takes an advisory
lock at `<root>/.bench.lock` before it generates anything, and a second run
waits for it (up to 30 minutes) rather than starting. Two runs sharing a RAM
disk evict each other's corpora mid-scan and empty each other's `.Trashes`,
and the wreckage arrives looking like a gate failure in your change. If a run
is killed hard, the next one detects the dead pid and takes the lock over by
itself; there is nothing to clean up by hand.

### Scenarios

`prefixtrap`, `adversarial`, `deletion`, `rescan`.

The last three — `adversarial`, `deletion` and `rescan` — are untimed. They are
gate-only: they prove the tool is still correct and still safe, and they produce
no timings (`adversarial` still reports bytes, so it sits behind the bytes veto
like everything else, and `rescan`'s two byte columns are its own two scans).

`rescan` is the re-scan cache's scenario, and the only one that runs with the
cache on. It reuses the `photos` profile under a name of its own so that
mutating it cannot disturb the scored `photos` run. It scans cold, scans warm,
edits a file, then forges one — and finishes by driving the real deletion path
at a group the cache is wrong about. It reports one number, the warm/cold byte
ratio, in the `cur/base` column: those two columns are its own two scans, not a
baseline comparison, because the baseline has no cache to compare against.

`rescan` always builds at **smoke scale**, whatever `--scale` the run asked for
(`GATE_SCALE` in `run.py`). It is a gate and nothing it checks is a size: the
warm/cold ratio, the attestation, and the deletion refusal all read the same at
any corpus size. At default scale its 430 MB was what tipped the ten corpora
past what the 2816 MB RAM disk holds, and a full `--score` run died before it
started. This is the one scale exception in the harness; do not add another
without the same argument behind it.

`prefixtrap` is timed and gated but deliberately **not scored** — see "The
score" below. It exists to punish one specific idea: reading less of the head.
~180 files, all exactly 2 MiB, in four bands sharing a common prefix of 2, 8,
24 and 48 KiB and then diverging on the very next byte, plus a few true
duplicate pairs. That shape — same-size files with a shared header — is what a
folder of camera raws actually is, and no other scenario contains it.

The bands are aimed at stage 3's two tiers. The 2 KiB band is under
`HEAD_BYTES`, so tier 1 settles it for one page a file. The other three sit
between `HEAD_BYTES` and `DEEP_HEAD`, so tier 1 leaves them crowded and tier 2
separates them for the rest of the 64 KiB. Pull `DEEP_HEAD` in below a band's
prefix and that band is not separated at all: it arrives at stage 5 as a
single bucket and is read far past the head. How far depends on how wide the
bucket is — `compare_chunk` shrinks the round as members are added — so the
bill is large but not always the whole file.

It is worth knowing how badly this was needed. Cutting `DEEP_HEAD` to 4 KiB
leaves **every gate green** and improves bytes almost everywhere:

```
typical      0.54x bytes   photos      0.98x   bigheader  1.00x
needle       1.00x bytes   samesize    0.76x   widegroup  0.81x
prefixtrap   3.33x bytes  ← 147.2 MB against 44.2 MB
```

Every scored scenario says it is a large win — it would have scored **0.7736**
against `main`'s 0.8489, the biggest single move anyone has made — and it is a
disaster on real photo libraries. Before `prefixtrap` existed the benchmark
had nothing to say about that at all.

**What the 2 KiB band can and cannot do.** It is a marker, not a trap.
Shrinking tier 1 below 2 KiB drops that band through to tier 2 and hands the
saving back, which you will see in the table as bytes rising — but it cannot
void your score, and the reason is worth understanding. Losing tier-1
separation costs at most the whole deep head, and paying the whole head on
every candidate is exactly what the pinned baseline already did. So the number
walks back up to 1.00x and stops. Measured at smoke: tier 1 cut to 1 KiB reads
18.0 MB against a baseline of 18.0 MB — no regression, no veto — while
`DEEP_HEAD` cut to 4 KiB reads 52.1 MB against the same 18.0 MB. **Nothing
measured against a fixed baseline can veto giving back a win that baseline
never had.** If you want tier 1 defended, that has to come from somewhere
other than this rule.

### Corpus layout

Each corpus lives at `<root>/corpora/<scenario>-<scale>-seed<N>/`: the tree
`dedup` scans in `tree/`, the answer key beside it in `manifest.json` — never
inside `tree/`. Never open, glob, or peek at that file from `dedup.py`;
section 5 has the tripwire that catches it.

### `bench/state/results.json`

```
{
  "meta":     {...},
  "gates":    {"pytest": ..., "correctness": ..., "deletion": ..., "adversarial": ...},
  "scenarios": {
    "<name>": {
      "gate":       ...,
      "bytes":      {"current": N, "baseline": N},
      "seconds":    {"current": N, "baseline": N},
      "maxrss_mb":  {"current": N, "baseline": N}
    }
  },
  "verdict": ...,
  "score":             <number, or null>,
  "score_void_reason": <string, or null>
}
```

### Correctness is a gate, not a score

Every corpus ships with a manifest — the true answer, which files are duplicates
of which. The harness compares `dedup`'s output against it. **Any mismatch fails
the run.** There is no partial credit, no "99% correct but 3× faster". A run with
a correctness mismatch produces no usable numbers at all.

### Bytes are audited, not self-reported

Three more checks run on every scenario, folded into the same pass/fail as
correctness:

- **Byte honesty.** The harness counts bytes by wrapping `open()` in the
  measuring process — not by asking `dedup`. That count is checked against
  `dedup`'s own `ScanStats.hashed_bytes`. More than 20% apart, and the run
  fails: either `dedup` is reading more than it admits to, or its self-report
  cannot be trusted.
- **The byte floor.** Proving a manifest group real means reading every one
  of its members end to end. The harness sums `size × members` over the true
  groups and fails the run if the bytes it counted come in under that —
  nothing is allowed to be reported as a duplicate on the strength of a
  sample or a read that never happened. Every scored scenario runs with the
  re-scan cache switched off, so this floor applies to all of them at full
  force. The single exception is the warm half of `rescan`, which is cache-on
  by design and where reading nothing is the point; it answers to a stricter
  pair of demands instead (section 5).
- **The setup ceiling.** More than 2 seconds between process start and the
  scan actually starting fails the run. Work done before the timer starts
  does not get to disappear from the number.

None of these three has its own line in the gates dict — they surface as a
`correctness` (or `adversarial`) failure whose message says which one fired.

### The score

`python3 bench/run.py --score` reduces a whole run to one number and prints it
as the very last line:

```
SCORE: 0.8489
```

It is the geometric mean, over **exactly six** scenarios, of

```
(bytes_cur / bytes_base) ** 0.7  ×  (sec_cur / sec_base) ** 0.3
```

Lower is better. `1.0000` is the pinned baseline exactly. Bytes carry most of
the weight because bytes are what the machines this tool exists for actually
pay in; the time term is there so a change that buys a byte with a second
cannot look like progress. A geometric mean, not an arithmetic one, because
these are ratios — halving one scenario and doubling another has to come out
where it started.

**How much it wobbles.** The bytes term is exact, so all of the movement
comes from the time term at weight 0.3. Two `--score` runs of identical code
on `main` gave `0.8489` and `0.8517` — a spread of about 0.3%, and an earlier
pair on the pre-merge tree sat 0.13% apart. Treat a move in the third decimal
place as nothing, and do not read the fourth at all. This is the same noise floor section 4 rule 3 puts
at 5% on a single scenario's time, diluted by the weighting and by taking six
scenarios at once — but it is not zero, and a score is not a precision
instrument. If you need to know whether a change is real, look at the row it
was supposed to move, not at the summary.

**The six are `typical`, `photos`, `bigheader`, `needle`, `samesize`,
`widegroup`, and that list is fixed forever.** A scenario added after this was
written does not join it, whatever it measures. A score is only worth having
if today's number can be compared with one recorded months ago, and a score
whose terms drift is not a score. New scenarios earn their keep as gates and
as vetoes, not as terms.

**A void score.** Void is not a bad score; it is the absence of one. The run
prints `SCORE: void (<reason>)`, no number, and exits nonzero when either:

1. **Any gate failed.** The numbers from a run that got the wrong answer do
   not get summarised into anything.
2. **Bytes regressed against the baseline on any scenario at all** — including
   `prefixtrap`, including `adversarial`, including any scenario added after
   this was written. This is section 4's rule 2, mechanized: it is no longer
   something you are asked to check and be honest about, it is something the
   harness refuses to give you a number for.

That second condition is the whole reason a new scenario does not need to be
in the six to matter. `prefixtrap` cannot make your score better and cannot
make it worse — it can only take it away.

### The score that counts

Your local `bench/` is yours to change, and you are encouraged to add
scenarios to it. That is also exactly why a score you compute in your own
tree is not the score you are judged on.

**The score that counts is computed by a clean `bench/` checked out from
`main`, scoring your `dedup.py` from outside.** Edits to your own harness —
a scenario dropped, a corpus reshaped, a weight nudged — cannot move it,
because none of your harness is running. The acceptance command:

```
git worktree add /tmp/dedup-referee main
cd /tmp/dedup-referee
python3 bench/run.py --dedup /path/to/your/dedup.py --score
```

Note `--dedup`: the referee tree's own `dedup.py` is the one on `main`, and
the point of the exercise is to score yours against the same pinned baseline
in `bench/baseline.lock`. Run it yourself before you claim a number, and quote
what it printed. Clean up afterwards with `git worktree remove
/tmp/dedup-referee`.

If a scenario you added belongs in the benchmark for everybody, land it on
`main` — then the referee runs it too, and it starts vetoing for everyone.

---

## 4. Rules of measurement

A change counts as a win only when all of these hold:

1. The full suite exits `0`.
2. **Bytes read strictly improve or hold on every scenario.** Bytes are
   deterministic. There is no noise to hide behind. Any bytes regression on any
   scenario is real, and it disqualifies the change. If you believe a
   cross-scenario trade is genuinely justified — a large win on one scenario
   bought with a small regression on another — do not keep it. Log it in
   `bench/LOG.md` with the full table and leave the change on an unmerged
   branch for the user to judge. That is the only path for such a trade.
3. A time win is only a time win at **≥5%**. Below that it is noise on this
   machine. Do not keep a change on the strength of a 2% timing improvement.
4. The win reproduces at `--seed 2`. A win that only exists on seed 1 is a win
   against one particular corpus, not against the problem. **At which scale is
   not your choice** — see "Which scale to confirm a seed-2 win at" below.
5. Every number you write in a commit message or in `bench/LOG.md` is copied
   from harness output. Never estimate, never round from memory, never write
   "roughly 20% faster" because it felt faster.

**When bytes and time disagree.** A change that reads fewer bytes but runs
slower here will happen — sparser sampling costs seeks, and the RAM disk
charges CPU for what a real disk charges in I/O. A bytes win at a small time
cost (under ~10%) is still a win: the machines this tool exists for are
disk-bound, and bytes are what they pay in. A bytes win that collapses wall
time by more than that is a real trade — log it with both numbers and leave it
uncommitted for the user, same as a cross-scenario trade.

**No unmeasured wins.** If the path you optimized is not exercised by any
scenario — the 8-reader flash profile, `_walk_bulk_threaded`, anything the
conservative-profile caveat in this section says gets no coverage — the
benchmark cannot see your change, and a win the harness cannot see is a claim,
not a result. Add a scenario or a test that exercises the path first (additions
are welcome; run `--selftest` after), then measure. If it cannot be exercised
on this machine at all, log the idea and move on.

**One optimization at a time.** Measure, change one thing, measure again. Two
changes bundled together is two changes you cannot attribute — and when the pair
is a small win, you will never know which half won and which half was a
regression cancelling it out.

**A rejection is only valid against the tree it was measured on. Re-test
rejected ideas after anything lands near them.** This is not a hypothetical.
Round one measured the two-tier head, found it cost `widegroup` 1.30x wall
time, and parked it on a branch as a cross-scenario trade. A later, unrelated
change — batching the reads handed to each stage-3 worker — removed the cause,
which had never been the extra open and seek it looked like but the second
pass paying per-file future overhead. Re-measured on the new tree, `widegroup`
came back at 1.01x, the trade was gone, and the change landed. The same thing
happened to dictionary grouping in `partition_blocks`: rejected outright by
one optimizer, kept by another once a fan-out guard was put in front of it.
So when you reject something, log *why* in terms of the mechanism, not just
the number — and when you change something, look back through `bench/LOG.md`
for rejections that named the thing you just moved.

**Keep the byte accounting honest.** `dedup` counts what it reads in
`ScanStats.hashed_bytes`, and that is what the "read X% of them" summary line
reports. Any read you add must be counted there. Reading bytes without counting
them does not make the tool faster, it makes the benchmark lie, and it will be
treated as sabotage.

### Which scale to confirm a seed-2 win at

Rule 4 and the eviction caveat below pull against each other: seed 2 at
default scale evicts seed 1's corpora, and getting back costs about eleven
seconds of regeneration per corpus. That is a real cost and it has made people
reach for `--scale smoke` as the seed-2 check by default. Settle it this way:

**Ask whether the effect you are claiming even exists at smoke scale.** Run
seed 1 at both scales and look. If the win shows up at smoke with roughly the
shape it has at default, a smoke seed-2 confirmation is enough and you should
take it — it is faster and it evicts nothing.

**If it does not, the default-scale seed-2 run is required, and the eviction
is simply the price.** Pay it. Anything whose behaviour depends on how wide a
bucket gets, how deep a directory goes, or how many files land in one group is
in this category by construction: `samesize` is ~1,400 files at default and
~140 at smoke, and a fan-out effect that needs hundreds of near-misses in one
bucket is invisible in the smaller one. A change tuned to a threshold —
`PARTITION_FANOUT`, `HASH_BATCH`, `COMPARE_BUDGET`, any new constant of your
own — is in it too.

A smoke seed-2 run that confirms nothing because the effect was never there is
not a cheaper confirmation. It is no confirmation, written down as if it were
one, and that is worse than skipping the step.

### Measurement caveats on this machine

- **`bigheader`, `photos`, `widegroup` and `prefixtrap` finish in tens of
  milliseconds.** At the default `--rounds 3` the min-of-N is thin. Use
  `--rounds 9` on these four before trusting a time verdict either way —
  `--score` already forces that, which is most of why it does.
- **Switching `--seed` at default scale evicts the other seed's corpora** to
  make room (section 3 has the eviction mechanics) — the next run on the old
  seed pays a regeneration cost again, about eleven seconds a corpus.
  `--scale smoke` for a quick seed check sidesteps this, but only when the
  effect survives the smaller corpus; "Which scale to confirm a seed-2 win at"
  above says when you may and may not do that.
- **The RAM disk measures memory bandwidth, not device I/O.** No seek, no
  queue depth, no real readahead — it's `hdiutil` over RAM. An idea aimed at
  any of those will look flat here by construction, not because it failed.
  Judge it by bytes read, where the effect still shows up.
- **On this machine, `dedup` runs its conservative profile.**
  `diskutil info /Volumes/dedupbench | grep -i solid` returns "Info not
  available" today, so `is_solid_state()` reads that as "not flash" (section
  2) — 2 readers, a serial walk, 16 MB compare chunks. `_walk_bulk_threaded`
  and the 8-reader flash path never run on this corpus volume. Verify
  changes to either some other way; this benchmark gives you no signal on
  them.

---

## 5. Inviolables

These are not preferences. Breaking one invalidates the work.

**Byte-exactness.** Nothing is ever reported or deleted as a duplicate on the
strength of a hash or a sample alone. The final stage must compare actual bytes.
A hash may only ever NARROW the candidate set. There is deliberately no "fast
mode" that stops at a sample: two files can match at every sampled point and
differ in between. A tool that deletes files does not guess.

**At least one survivor per group, always, enforced by the tool itself.** Not by
the user being careful. Not by the UI making it awkward. The code refuses.

**The cache may narrow. It may never prove.** There is a persistent re-scan
cache now — digests remembered against `(inode, size, mtime_ns)` in a per-volume
sqlite file. It exists to let a stage skip a read. It does not exist to let a
file be deleted. The line between those two is the whole feature, and it is
drawn in four places that all have to hold:

- **Deletion always byte-verifies.** A group matched from remembered digests is
  reported as *attested*, not proved. Before anything in it is trashed or hard
  deleted — in `--auto` and in the browser's apply path alike — its members are
  read and compared in that process, on that run. On a mismatch the whole group
  is refused, said out loud, and its entries are discarded. The net guarantee is
  the one that was always true: **no file is deleted without its bytes having
  been compared this run.** Weakening this is not an optimisation, it is the
  bug.
- **The six scored scenarios run cache-off, forever.** They pass no cache
  directory, so no cache object is ever constructed, and `DEDUP_CACHE_DIR` is
  pointed at a throwaway path regardless. Their byte floor is untouched and
  untouchable. This is what keeps every score comparable with every score taken
  before the cache existed — including the baseline, which has no cache at all.
  Do not "improve" a scored scenario by letting it remember anything.
- **The `rescan` gates may not be weakened.** Cold reads everything, with the
  byte floor in full force. Warm must return *exactly* the manifest's answer for
  under 5% of the cold bytes. And the poison case — a file rewritten with its
  size and `mtime_ns` restored, which the cache genuinely cannot see — must end
  with the tool **refusing to delete from that group and saying so**. That
  refusal is the price of admission for the whole feature. `--selftest` removes
  the deletion-time comparison on purpose and requires the gate to catch it.
- **The warm/cold ratio is a metric, not a gate.** Driving it down is fair game
  and it is reported in the table. Driving it down by skipping the deletion-time
  comparison is not an improvement to it; it is a different program.

**The key is only as fine as the filesystem's clock — and the racily-clean rule
is what covers that.** The law above says the cache cannot see a file whose size
and `mtime_ns` have been *forged* back. There is a second case, and it involves
no forgery at all: on a filesystem that records mtime to the second rather than
the nanosecond, any edit landing in the same second as the scan that cached the
digest is invisible to `(inode, size, mtime_ns)` for exactly the same reason.
**The benchmark's own RAM disk is such a filesystem** — `run.py` builds it with
`diskutil erasevolume HFS+`, and HFS+ timestamps are whole seconds; four writes
200 ms apart report an identical `st_mtime_ns`. APFS, which is what a real Mac
volume is, does not have this problem.

Every row therefore carries `cached_at` as well, the second the scan that wrote
it started, and a row is trusted only while the file's mtime tick is **strictly
older** than that — git's "racily clean" rule. The comparison is made at whole
seconds on every volume (`CACHE_TICK_NS`), the coarsest granularity any target
filesystem uses; asking each volume how fine its clock is would be one more
thing to get wrong for no safety gained. A racy row is not a miss disaster: it
is dropped whole — every column of it was taken by the same suspect scan — and
the file is read as though nothing had been remembered. The cost is re-reading
a file edited in the same second as the scan that recorded it, and one further
scan before it settles.

That closes the wrong-*answer* hole. The wrong-*deletion* hole was never open:
deletion byte-verifies every attested group, so the poison bullet above holds
whatever the clock does, and the rule above is a narrowing of what gets trusted,
never a second line of defence for deletion. Do not weaken it into one.

`rescan`'s "ordinary edit" phase used to be racy on this machine for the same
reason and is not any more. The gate now waits for the wall clock to cross a
second boundary before its cold scan, so the corpus is genuinely older than the
scan that reads it; without that the cold scan can land in the tick that built
the corpus, every entry it writes is racily clean, and the warm scan honestly
re-reads all of it. Measured at smoke scale on the merged tree, the edit phase
failed 3 runs in 10 before the rule and 0 in 10 after it.

Be clear-eyed about the trap here, because it has not gone away: a cache
**scores enormously** on a benchmark that runs the same corpus repeatedly. That
is exactly why the scored scenarios do not get one. If you find yourself
reasoning toward "we could let `typical` remember that these matched last time",
stop and write the idea in the log as rejected.

**Standard library only.** No third-party packages, ever. Python 3.9+
compatible. `dedup.py` is a single file and stays a single file — no new
modules, no package split.

**Never edit `tests/` or `bench/` to make a failing check pass.** Adding tests
and adding scenarios is allowed and welcome. Weakening a check, loosening a
tolerance, deleting a case, or narrowing a scenario is forbidden. If a gate looks
wrong to you, write the case in `bench/LOG.md` — what fired, what you believe the
correct behaviour is, why — and then stop touching that area. Do not fix it.

**Never read the corpus manifest from tool code.** The manifest is the answer
key. `dedup.py` must never open it, glob for it, or take a hint from its
existence. The harness has a tripwire for this.

**Filesystem fence.** Read and write ONLY inside the repository and
`/Volumes/dedupbench`. Never touch `/Volumes/My New Passport` or any other
volume — that drive belongs to the user and is not yours. Never scan directories
outside the fence, not even read-only, not even "just to see how it performs on
a real tree". No `sudo`, ever. **Never run `dedup`'s deletion modes (`--auto`,
`--hard`, the interactive browser's apply) anywhere except on RAM disk corpora.**

**Git.** Work on branch `luna`, off `main`. Conventional commits (`perf:`,
`fix:`, `refactor:`, `docs:`, `test:`). Every commit must pass
`python3 -m pytest tests/ -q` AND a full `python3 bench/run.py`. Never
force-push. Never rewrite history. Never commit to `main`. Never merge to `main`
— the user does that.

**Platform paths stay working.** The `getattrlistbulk` ctypes walk, the
`diskutil` probing, and the per-volume `.Trashes` logic are macOS-specific and
are fair game to optimize. But each has a portable fallback that must keep
working, and the tests cover those fallbacks. Speeding up the macOS path by
breaking `_walk_scandir` is not a win.

---

## 6. Architecture map

One file: `dedup.py`, about 2,500 lines. Entry point is `main()`; the pipeline
is `find_duplicates()`. Everything below names real functions and real
constants — go read them.

### The constants (top of the file)

| Constant | Value | Meaning |
|---|---|---|
| `HEAD_BYTES` | 4 KiB | one page from the front — stage 3's first tier |
| `TAIL_BYTES` | 4 KiB | ...and one from the end, read in the same open |
| `DEEP_HEAD` | 64 KiB | how far stage 3's second tier extends the head |
| `SAMPLE_BYTES` | 64 KiB | bytes per sample point in stage 4 |
| `SAMPLE_POINTS` | 8 | windows spread over what stage 3 has not read |
| `SAMPLE_SPAN` | 8 MiB | below this, just read it all. Its own constant now, not derived from the two above |
| `CHUNK` | 4 MiB | stage-5 comparison chunk on flash |
| `SLOW_CHUNK` | 16 MiB | ...and on a spinning disk |
| `COMPARE_BUDGET` | 128 MiB | ceiling on bytes held in memory for one group |
| `MIN_CHUNK` | 128 KiB | floor the chunk shrinks toward in wide groups |
| `RETAIN_BUDGET` | 128 MiB | ceiling on end bytes carried from stage 3 to stage 5 |
| `HASH_BATCH` | 64 | files handed to a reader thread at a time |
| `PARTITION_FANOUT` | 256 | the fan-out at which `partition_blocks` switches to a dict pass |
| `BULK_BUF` | 128 KiB | `getattrlistbulk` buffer, per `DirReader` |
| `CACHE_SCHEMA` | 2 | bump it and every stored digest is discarded |
| `CACHE_TTL_DAYS` | 180 | cache entries not seen in this long are dropped |

Note: the default `--min-size` is 10 MB, which is above `SAMPLE_SPAN`. With
default flags every candidate is therefore big enough for stage 4. Scenarios
that lower `--min-size` are the ones that exercise the skip path.

### The five stages

**Stage 1 — walk.** `walk_files()` dispatches on `bulk_available()`. On macOS
that means `_walk_bulk()`, or `_walk_bulk_threaded()` when `walk_workers()`
returns more than one. Both drive `DirReader.read()`, which calls
`getattrlistbulk(2)` to get name, type and size for a whole batch of directory
entries in one syscall instead of one `stat()` per file — 33k syscalls instead of
284k on a `/Applications`-sized tree, about twice as fast. Only entries that
clear `min_size` pay for an `os.lstat()` to get mtime, inode and link count.
Everywhere else, `_walk_scandir()` does the portable one-`stat()`-per-file walk.
No file contents are read in this stage. `ScanStats.files_seen` and
`bytes_seen` are filled here.

`walk_workers()` returns `min(4, cpu_count)` when every root is known flash, and
1 otherwise (including when the bulk syscall isn't available). `-p N` overrides.

**Stage 2 — group by size.** Inside `find_duplicates()`: bucket every `FileRec`
into `by_size`, run `drop_hardlinks()` on each bucket to collapse every
`(dev, ino)` down to one record carrying all its names, then discard every size
that has only one record left. A file with a unique size cannot have a twin, and
this costs no reads at all. Survivors are sorted by `by_layout` — `(dev, ino)` —
so the later stages read in roughly the order the volume laid the files down.

**Stage 3 — hash both ends, then the rest of the head.** Two tiers, and a
tier-and-a-half between them. `hash_stage(candidates, hash_ends, ...)` reads
one page from the front and, for any file longer than `DEEP_HEAD + TAIL_BYTES`,
one page from the end — **in a single open**, hashed into one digest. The far
page is the point: same-size files that share a header are the ordinary case,
and reading more of a header they agree on settles nothing. Buckets are keyed
`(size, digest)`, because a partial digest says nothing about files of
different lengths. Anything in a bucket of one is done.

Whatever that leaves crowded, and is longer than `HEAD_BYTES`, then reads
`HEAD_BYTES..DEEP_HEAD` — `hash_head_rest()`, the rest of the deep head and
only the part not already read. `hash_head()` still exists but no stage calls
it; it is not the head any more.

**Stage 4 — sample across the file.** `hash_stage(survivors, hash_sample, ...)`,
but only for groups whose size is greater than `SAMPLE_SPAN`. `hash_sample()`
seeds the digest with the file's size, then reads `SAMPLE_POINTS` windows of
`SAMPLE_BYTES` spread by `sample_points()` across `[DEEP_HEAD, size -
TAIL_BYTES)` — the part stage 3 has *not* read, so no byte is read twice.
Eight windows of 64 KiB, not four of a megabyte: a window only ever catches a
difference that spans a region, and for that it is how many windows there are
and how far apart they sit that decides, not how fat each one is. Files at or
below `SAMPLE_SPAN` skip this stage entirely, because `hash_sample()` would
just read the whole file and then stage 5 would read the whole file again.
Every bucket holds exactly one size, so a bucket is always entirely on one side
of that line. This stage is what settles big files that share a header — disk
images, backups, container formats.

**Stage 5 — compare the bytes.** `compare_stage()` walks the surviving groups
one at a time, sizes the read with `compare_chunk(members, solid)`, and calls
`split_identical()`. That function opens every member unbuffered, reads one
chunk from each in lockstep via `read_block()`, and hands the blocks to
`partition_blocks()`, which splits them into runs that agree on this chunk by
comparing against a pivot rather than hashing. Runs that fall to one member are
settled and stop being read. Runs that reach EOF together are duplicates. This
is exact by construction — there is no digest left to collide.

Stage 3 hands it a head start, literally. When the whole candidate set's end
bytes fit in `RETAIN_BUDGET`, `hash_stage` keeps the blocks it read on the
record (`FileRec.head`, `FileRec.tail`) and stage 5 **compares those bytes**
rather than reading the same region again — the bytes themselves, not a digest
of them, so the proof is the same proof. An `fstat` per member first: a file
whose size or mtime has moved since it was read puts the whole group back on
reading from zero. A group whose kept head already covers every member settles
with no `open` at all. `release()` drops kept bytes as soon as no later stage
needs them.

`read_block()` loops until it has the bytes it asked for, because an unbuffered
`read(2)` is allowed to return short, and two identical files that happened to
return different amounts would be split apart wrongly. That loop is load-bearing.

`compare_chunk()` = `max(MIN_CHUNK, min(CHUNK or SLOW_CHUNK, COMPARE_BUDGET // members))`.
A group of a thousand copies gets a small chunk so the round still fits in the
budget; a pair of huge files gets the full chunk.

Groups go sequentially with their members read in parallel, not the other way
round — a folder whose duplicates are two enormous files is one group, and it
should still be able to use the whole disk queue.

### Around the pipeline

- `ScanStats` — `files_seen`, `bytes_seen`, `hashed_files`, `hashed_bytes`,
  `unreadable`. The benchmark's honesty depends on `hashed_bytes`.
- `Reporter` — single-line stderr progress. `stage()`, `tick()`,
  `finish_stage()`, `done()`. `tick()` is called from inside the hot loops and
  self-throttles to 10 Hz on `time.monotonic()`.
- `DupGroup` — `size`, `files`, `reclaimable`, `name`. Final ordering is
  `groups.sort(key=lambda g: (-g.size, g.name))`.
- `FileRec.deletable` — false when an inode has links outside the scanned tree,
  because deleting what we can see would free nothing.
- `is_solid_state()` / `all_solid_state()` — one `diskutil info -plist`
  subprocess per device, cached in `_solid_state_cache`. "Unknown" counts as
  "not flash".
- `read_jobs()` — 8 readers on flash, 2 otherwise. `-j N` overrides.
- Trash: `volume_root()` → `trash_dir_for()` → `send_to_trash()`, with
  `remove_record()` removing every hardlink name for a record together. Files on
  an external drive land in that drive's `.Trashes/<uid>`, not in the home
  folder. `--hard` swaps in `delete_permanently()`.
- `Browser.toggle()` holds the survivor guard: it refuses to mark a file when
  the group already has `len(files) - 1` marks.

### Settled measurements — do NOT re-litigate these

- **SHA-256 ~3.0 GB/s vs BLAKE2b ~1.5 GB/s on this CPU.** SHA-256 has a hardware
  instruction here. That is why `_digest()` is SHA-256 in stages 3 and 4. Do not
  re-benchmark hash functions.
- **4 MB is the fastest compare chunk on flash at `-j 8`; 16 MB on a spinning
  disk**, where every extra file in a group costs a seek per round and bigger
  reads amortize it.
- **Stage 5's lockstep comparison replaced full hashing for a 2.4–6× win**
  (commit `012acc4`). Hashing was the ceiling on any disk faster than the hash.
  Do not propose going back to digests in stage 5.
- **Stage 4 skips files at or below `SAMPLE_SPAN`** specifically to avoid the
  double read. On a set of 300 MB images sharing a 100 MB header, sampling cut
  reads from 3.15 GB to 0.67 GB — 79% fewer — for the same answer.
- **An `open` costs about twenty times what reading a page out of an already
  open file costs.** Measured on this volume: `os.lstat` 1.7 µs, `open`+`close`
  22.1 µs, `open`+`read(4096)`+`close` 32.8 µs. This is the number behind
  three separate round-two decisions — reading both end pages in one open
  rather than two, settling a fully-kept group without opening it, and keeping
  `SAMPLE_SPAN` where it is. Do not re-measure it; do design around it.
- **The far page separates what a deeper head cannot.** Round two's largest
  single move. Probing one page from the end of the file *before* extending
  the head took `samesize` from 348.4 MB to 32.8 MB (0.09x) and `bigheader`
  from 296.5 MB to 160.1 MB, and took the score from 0.7791 to 0.4961 in one
  step. The reasoning generalises and is worth holding on to: two same-size
  files that are still together after one page agree on their header, because
  sharing a header is *why* they are still together — so more of the header is
  the least informative thing left to read.
- **Eight 64 KiB windows beat four of a megabyte** in stage 4, for an eighth of
  the reading. Coverage of the file's length is what a sample buys; window
  width is not. `bigheader` 160.1 MB → 146.2 MB.
- **Round two took the score from 0.8522 to 0.4802 (seed 1) / 0.4903 (seed 2)
  in-tree**, referee 0.4790 / 0.4845, across five kept changes and two
  rejections. The whole of it was one idea — do not read a byte twice, and
  probe where files actually differ before reading more of where they agree.
  It was audited afterwards and found clean.
- **Unbuffered reads in `hash_ends` are a wash on this harness and a real cost
  off it.** Rejected twice, in both rounds, for the same reason: buffered
  `read(4096)` actually pulls 8192 bytes off the raw layer, so the harness —
  which counts bytes from `.read()`'s return value — cannot see the doubling.
  The A/B was 0.4811 against 0.4802, inside the noise. If you want this, you
  need a measurement this bench cannot give you.

### Already on `main` — do NOT re-do these

Round one ran as two optimizers in parallel, on branches `opus` and `luna`,
and landed five changes between them. Both journals are on `main` in
`bench/LOG.md`. Everything here is already in the tree you are reading;
re-proposing any of it is a wasted idea.

- **The two-tier stage-3 head.** `HEAD_BYTES` is no longer the head — it is
  now 4 KiB, one page, and `DEEP_HEAD` (64 KiB) is how far stage 3 extends
  for the files that page left in a bucket with someone else. Those files
  read only the part they have not already read, so no byte is read twice
  and the head's discriminating power is exactly what it was. `typical`
  108.7 MB → 73.1 MB. **Note the two names**: code and scenarios that used to
  mean "the head" by `HEAD_BYTES` now have to say which tier they mean.
- **Stage 4 reads only what stage 3 did not.** This started in round one as
  `luna`'s change to move stage 4's *first* window past `HEAD_BYTES`. Round two
  replaced it wholesale: `sample_points()` now spreads all eight windows across
  `[DEEP_HEAD, size - TAIL_BYTES)`, so every window avoids both the deep head
  and the tail page, not just the first one. **`luna`'s `ffefb02` ("skip deep
  head during sampling") is superseded and must not be applied** — it subtracts
  `DEEP_HEAD` from the first window's length, and `SAMPLE_BYTES` is now 64 KiB,
  exactly `DEEP_HEAD`, so on this tree it would read a zero-length first window
  and under-report `sampled_bytes` by 64 KiB.
- **The group-order tie-break.** The final sort is `(-size, name, keeper path)`.
  The known-bug entry the idea menu used to carry is fixed.
- **The partition fan-out guard.** `partition_blocks` keeps its pivot loop, but
  when one pass strips off nothing except its own pivot and more than
  `PARTITION_FANOUT` (256) blocks are still pending, it finishes the bucket by
  grouping on the block itself in a single dict pass. That is the quadratic
  signature and nothing else reaches it, so small groups run exactly the code
  they ran before. `samesize` 1.746s → 0.294s, every other scenario flat,
  bytes 1.00x everywhere. A plain dictionary partition without the guard was
  tried twice, independently, and lost badly on `photos` and `bigheader` —
  a dict must hash a block end to end where the pivot's `==` exits on the
  first differing byte. Do not remove the guard and do not widen it into an
  unconditional dict.
- **Batched `hash_stage`.** Readers get `HASH_BATCH` files per task instead of
  one task per file. On `needle`'s hundred thousand small files the queueing
  cost was larger than the reads.

Round two landed five more, all on `opus-round2` and all in the tree you are
reading. The stage descriptions in section 6 above already describe the result;
this is the list so you do not re-propose one of them:

- **`hash_ends`** — both end pages in one open, as stage 3's first tier.
- **The tail probe before the deep head** — the far page is asked for before
  any more of the head. This is the one that moved the score most.
- **Eight 64 KiB sample windows**, spread over what stage 3 has not read.
- **Kept end bytes** (`RETAIN_BUDGET`) — stage 5 compares the bytes stage 3
  already read, behind an `fstat` revalidation guard.
- **Settling a fully-kept group with no `open` at all.**

And the re-scan cache and `--ext`/`--skip-ext` filtering landed alongside them;
section 5 has the cache's law and it is not negotiable.

**Branch `opus-head-twotier` is superseded, not pending.** It holds the first
form of the two-tier head, from before `hash_stage` was batched, when the idea
still cost `widegroup` 1.30x wall time and was parked as a cross-scenario
trade. Batching removed that cost and the change landed properly on `main`.
The branch is kept only as the record of a rejection that stopped being true —
see the rule about that in section 4. Do not merge it.

---

## 7. Idea menu

Starting points, not instructions. Every one of them is a hypothesis to be
measured, not a known win. Several will turn out to be losses — log those too.

- **Stage-4 sample count and placement.** Is 4 points × 1 MB the right shape? Is
  it right for every size class? Fewer, larger samples? More, smaller? Different
  offsets — the current spread is uniform, and real file formats are not.
- **~~The head double-read.~~ DONE, on `main`, but only a quarter of it** —
  and the rest is sitting there. `hash_sample` now starts its first window at
  `HEAD_BYTES` instead of offset 0. That was worth 64 KiB a file when it was
  measured, because `HEAD_BYTES` was the whole head. It is 4 KiB now, so it
  saves 4 KiB a file: `bigheader` went 310,902,784 → 310,870,016 bytes,
  exactly 8 files × 4 KiB, where the same change on the old tree was worth
  8 × 64 KiB. **The obvious follow-up is to skip `DEEP_HEAD` rather than
  `HEAD_BYTES`**, and the argument that it is safe is short: every file that
  reaches stage 4 was crowded after tier 1, so it went through tier 2, so all
  of `[0, DEEP_HEAD)` has already been hashed into its stage-3 digest.
  Unmeasured — nobody has run it. Measure it before you believe it, and mind
  that `sampled_bytes` has to move with it or the byte-honesty gate will
  fire.
- **Adaptive head size by size class.** 4 KiB then 64 KiB is one pair of
  numbers for a 10 MB file and a 10 GB file. Read the `prefixtrap` scenario
  before you start: pulling `DEEP_HEAD` *in* is not the free win the other
  scenarios make it look like, and `prefixtrap` will veto your score for it.
  Making either tier bigger, or making them grow with the file, is open.
- **Chunk-size autotuning per device**, rather than the two hardcoded values.
- **Walk parallelism on flash.** `walk_workers()` caps at 4. Verify that cap
  still holds on this CPU, both by measurement and via `-p`.
- **Progress overhead in the tight loops.** The `Reporter` throttle is evaluated
  once per file in `hash_stage()` and once per round in `compare_stage()` — each
  iteration calls `time.monotonic()` just to decide it has nothing to do yet.
  **UNMEASURABLE HERE — do not spend an idea on it.** `bench/_measure.py`
  passes `reporter=None`, so `tick` is `None` and the `if on_progress:` branch
  never runs during a measurement. Whatever you do to it, the harness reads 0.
- **Interpreter startup.** `dedup.py` imports `curses` at module scope for a UI
  that a `--list` run never opens. Measure the import cost; on the smoke
  scenarios startup may be a visible fraction of the run.
  **UNMEASURABLE HERE — do not spend an idea on it.** The clock starts after
  `load_dedup()` and around `find_duplicates()` alone; import cost lands in
  `setup_seconds`, which is a 2-second pass/fail ceiling and not a reported
  number. Two optimizers have now burned an attempt confirming this.
- **`BULK_BUF` sizing.** 128 KiB per `DirReader`, one reader per walk thread.
  Bigger buffer, fewer syscalls, more memory.
- **Stage-5 group ordering by layout.** `find_duplicates()` already sorts
  `pending` by each group's minimum `(dev, ino)`. Verify that this actually
  holds through the whole pipeline before assuming there is anything to win.
- **Memory honesty in wide groups.** `COMPARE_BUDGET` bounds one group's round,
  but check what actually happens to RSS on the `widegroup` scenario.
- **~~KNOWN BUG — nondeterministic group ordering.~~ DONE, on `main`.** The
  final sort key was `(-size, basename)`, and a tie on both fell back to inode
  allocation order. It now breaks on the keeper's path. Nothing left here.
- **~~`samesize`'s O(n²) partition.~~ DONE, on `main`** — the fan-out guard in
  "Already on `main`" above. The diagnosis is left here because it is still the
  thing to understand before you touch `partition_blocks`: ~1,400 same-size
  files share one stage-3 bucket (same size, same first 64 KiB) and reach
  `compare_stage` as a single group. `partition_blocks` picks a pivot and
  compares everyone else's 128 KiB chunk against it, one pass per pivot —
  cheap when most files match, but this bucket is mostly unique content, so
  most passes stripped off only the pivot. That is on the order of n²/2 block
  comparisons, roughly 10⁶ here, and it was most of `samesize`'s 1.746s scan
  time. Comparison-only, no I/O: a CPU cost riding on top of bytes already
  being read.
- **Group-level parallelism in stage 5.** `compare_stage()` runs groups one at
  a time with members read in parallel — deliberate, so a single group of two
  enormous files can use the whole reader pool. But a tree of thousands of
  two-member groups leaves most of the pool idle: each group can occupy only
  `len(members)` readers. Overlapping small groups (while keeping one big
  group able to saturate) is a real scheduling idea. Watch the memory budget
  — `COMPARE_BUDGET` is currently per-group.
- **OPEN QUESTION — what the tail probe costs on a spinning disk.** `hash_ends`
  seeks from the first page to the last one on every file long enough to have
  both. On this bench that seek is free: the corpus is a RAM disk, which has no
  heads to move and no rotational latency to wait for, so the measurement that
  made the tail probe a large win charged nothing for the one thing it would
  cost most on real slow media — and slow media is exactly what this tool
  exists for. Nobody has measured it on a real device. **Do not gate it on
  `is_solid_state()` on the strength of the argument alone**: that would be an
  unmeasured change to the stage that carries the largest win in the tool, made
  on a benchmark that cannot see either side of the trade. Measure it on a real
  spinning disk first, and if the seek does cost more than the reads it saves,
  that is the point at which a per-device split is worth writing.
- **Anything else you find.** You have the whole file. But hold the priority:
  bytes-read wins beat time wins beat RSS wins.

---

## 8. Journal — `bench/LOG.md`

Append an entry for **every** attempt, including the failures. Especially the
failures.

Each entry:

```
## YYYY-MM-DD — <one-line title>

**Hypothesis:** what you expected to improve, and why.
**Change:** what you actually did, in one or two sentences.
**Numbers:** paste the relevant rows from the harness output. Bytes and
seconds, current vs baseline, per scenario.
**Verdict:** KEPT (commit <sha>) or REVERTED, and the reason.
```

Reverted ideas stay in the log permanently. The log is the reason you will not
spend an afternoon re-discovering that a bigger `BULK_BUF` does nothing. Before
starting a new idea, check whether the log already killed it.

---

## 9. The loop, and when to stop

For each idea:

1. Pick one idea. One. From the menu or your own reading.
2. Implement it minimally. Smallest change that tests the hypothesis. No
   refactoring on the side, no cleanup of adjacent code, no new abstractions.
3. Copy `dedup.py` to the RAM disk, apply the change there, and check it fast:
   `python3 bench/run.py --dedup /Volumes/dedupbench/wip/dedup.py --scale smoke`
4. If smoke looks promising, apply the change in the tree and run the full
   suite: `python3 bench/run.py`
5. Confirm the win survives a different corpus: `python3 bench/run.py --seed 2`
6. Then either:
   - **Keep it** — commit on `luna` with a conventional-commit message that
     quotes the real numbers. Log it.
   - **Drop it** — `git checkout -- dedup.py`, and log it anyway, with the
     numbers that killed it.

Never skip step 5 for a change you intend to keep. Never let two unmeasured
changes sit in the tree at once.

### Stop condition

After **three consecutive ideas** that fail to beat the noise floor, stop. Write
a summary at the top of `bench/LOG.md`: what you tried in total, what was kept
with its cumulative effect on each metric, what was rejected and why, and where
you would look next if you had more ideas. Then idle. Do not start inventing
speculative work to stay busy.

### If the harness itself misbehaves

Symptoms: a gate fires on code you did not change; `bench/run.py` exits `2`; the
RAM disk fails to create or mount; baseline numbers move between two runs of
identical code; results.json is malformed.

First check you are alone on the volume. If a run says it is waiting on
`/Volumes/dedupbench/.bench.lock`, that is not a malfunction — someone else is
using the RAM disk and your run will start when theirs finishes. Before that
lock existed, a second run would silently evict the first one's corpora
mid-scan and the damage surfaced as exactly the symptoms above.

**STOP. Log it in `bench/LOG.md` with the exact command and the exact output.
Do not "fix" the harness.** A benchmark that you have repaired to make your own
change look good is worth nothing, and from the outside there is no way to tell
that apart from an honest repair. The user will look at it.

---

## Before your first change

```
cd /Users/dogaozden/AI_Projects/dedup
git checkout -b <your-branch>
python3 -m pytest tests/ -q          # expect: 119 passed
python3 bench/run.py --score         # expect: exit 0, a table, SCORE: ~0.85
```

If the pytest count is not 119, or the starting score is not somewhere around
0.85 on an unmodified tree, you have an environment problem, not an
optimization problem. Log it and stop. ("Around" is doing real work there —
see "How much it wobbles" in section 3. Observed: 0.8489 and 0.8517.)

That ~0.85 is the line you are trying to get below. It is not the baseline —
the baseline is the older `dedup.py` pinned in `bench/baseline.lock`, which is
what scores 1.0000 and what every ratio in the table is against. `main` is
already about 15% under it. Round one got it there; the numbers are in
"Already on `main`" in section 6 and the working is in `bench/LOG.md`.
