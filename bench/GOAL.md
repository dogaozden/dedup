# GOAL — autonomous optimization of `dedup`

You are luna. You have a shell on this Mac and write access to this repository.
You have no prior context about it. This file is the whole briefing: the
mission, the machine, the harness, the rules, and the map of the code.

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
- `--root PATH` — override the RAM disk location.

### Scenarios

`typical`, `photos`, `bigheader`, `needle`, `samesize`, `widegroup`,
`adversarial`, `deletion`.

The last two — `adversarial` and `deletion` — are untimed. They are gate-only:
they prove the tool is still correct and still safe, and they produce no
performance numbers.

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
  "verdict": ...
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
  cached answer, a sample, or a read that never happened.
- **The setup ceiling.** More than 2 seconds between process start and the
  scan actually starting fails the run. Work done before the timer starts
  does not get to disappear from the number.

None of these three has its own line in the gates dict — they surface as a
`correctness` (or `adversarial`) failure whose message says which one fired.

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
   against one particular corpus, not against the problem.
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

**Keep the byte accounting honest.** `dedup` counts what it reads in
`ScanStats.hashed_bytes`, and that is what the "read X% of them" summary line
reports. Any read you add must be counted there. Reading bytes without counting
them does not make the tool faster, it makes the benchmark lie, and it will be
treated as sabotage.

### Measurement caveats on this machine

- **`bigheader`, `photos` and `widegroup` finish in tens of milliseconds.**
  At the default `--rounds 3` the min-of-N is thin. Use `--rounds 9` on these
  three before trusting a time verdict either way.
- **Switching `--seed` at default scale evicts the other seed's corpora** to
  make room (section 3 has the eviction mechanics) — the next run on the old
  seed pays a regeneration cost again. `--scale smoke` for a quick seed check
  sidesteps this.
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

**No persistent cache between runs.** No mtime/inode digest cache, no
`~/.cache/dedup`, no sidecar index, nothing that carries a conclusion from one
scan to the next. A deletion tool must prove identity fresh on every scan. Be
clear-eyed about this one: a cache **would score enormously** on this benchmark,
because the benchmark runs the same corpus repeatedly. It is **forbidden**. If
you find yourself reasoning toward "we could remember that these matched last
time", stop and write the idea in the log as rejected. The harness backs this
up mechanically: the byte floor (section 3) requires every reported group's
members to be read in full, in this process, this scan. A cache that skipped
a read to score better fails that check before it gets the chance to look
fast.

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

One file: `dedup.py`, about 1,700 lines. Entry point is `main()`; the pipeline
is `find_duplicates()`. Everything below names real functions and real
constants — go read them.

### The constants (top of the file)

| Constant | Value | Meaning |
|---|---|---|
| `HEAD_BYTES` | 64 KiB | stage-3 sample size |
| `SAMPLE_BYTES` | 1 MiB | bytes per sample point in stage 4 |
| `SAMPLE_POINTS` | 4 | head, two middles, tail |
| `SAMPLE_SPAN` | 8 MiB | `SAMPLE_BYTES * SAMPLE_POINTS * 2` — below this, sampling isn't worth it |
| `CHUNK` | 4 MiB | stage-5 comparison chunk on flash |
| `SLOW_CHUNK` | 16 MiB | ...and on a spinning disk |
| `COMPARE_BUDGET` | 128 MiB | ceiling on bytes held in memory for one group |
| `MIN_CHUNK` | 128 KiB | floor the chunk shrinks toward in wide groups |
| `BULK_BUF` | 128 KiB | `getattrlistbulk` buffer, per `DirReader` |

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

**Stage 3 — hash the head.** `hash_stage(candidates, hash_head, ...)`.
`hash_head()` SHA-256s the first `HEAD_BYTES` of each file. Buckets are keyed
`(size, digest)` — size is part of the key because a partial digest says nothing
about files of different lengths. Cheap, and it kills most of what stage 2 left.
Anything in a bucket of one is done.

**Stage 4 — sample across the file.** `hash_stage(survivors, hash_sample, ...)`,
but only for groups whose size is greater than `SAMPLE_SPAN`. `hash_sample()`
seeds the digest with the file's size, then reads `SAMPLE_POINTS` blocks of
`SAMPLE_BYTES` at evenly spaced offsets: head, two middles, tail. Files at or
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

---

## 7. Idea menu

Starting points, not instructions. Every one of them is a hypothesis to be
measured, not a known win. Several will turn out to be losses — log those too.

- **Stage-4 sample count and placement.** Is 4 points × 1 MB the right shape? Is
  it right for every size class? Fewer, larger samples? More, smaller? Different
  offsets — the current spread is uniform, and real file formats are not.
- **The head double-read.** Stage 3 reads the first 64 KiB. Stage 4's first
  sample point starts at offset 0 and reads 1 MiB, covering those same 64 KiB
  again. That is a real, countable duplicate read on every large candidate.
- **Adaptive `HEAD_BYTES` by size class.** 64 KiB is one number for a 10 MB file
  and a 10 GB file.
- **Chunk-size autotuning per device**, rather than the two hardcoded values.
- **Walk parallelism on flash.** `walk_workers()` caps at 4. Verify that cap
  still holds on this CPU, both by measurement and via `-p`.
- **Progress overhead in the tight loops.** The `Reporter` throttle is evaluated
  once per file in `hash_stage()` and once per round in `compare_stage()` — each
  iteration calls `time.monotonic()` just to decide it has nothing to do yet.
- **Interpreter startup.** `dedup.py` imports `curses` at module scope for a UI
  that a `--list` run never opens. Measure the import cost; on the smoke
  scenarios startup may be a visible fraction of the run.
- **`BULK_BUF` sizing.** 128 KiB per `DirReader`, one reader per walk thread.
  Bigger buffer, fewer syscalls, more memory.
- **Stage-5 group ordering by layout.** `find_duplicates()` already sorts
  `pending` by each group's minimum `(dev, ino)`. Verify that this actually
  holds through the whole pipeline before assuming there is anything to win.
- **Memory honesty in wide groups.** `COMPARE_BUDGET` bounds one group's round,
  but check what actually happens to RSS on the `widegroup` scenario.
- **KNOWN BUG — nondeterministic group ordering.** The final sort key is
  `(-size, basename)`. When two groups tie on both, the tie is broken by
  whatever order the pipeline happened to produce, which traces back to inode
  allocation order — so the same logical corpus, regenerated, can report its
  groups in a different order. This is pre-existing, not something you
  introduced. Fix it with a deterministic tie-break (the keeper's path is the
  obvious one). Cheap, and it makes benchmark output more comparable.
- **`samesize`'s O(n²) partition.** ~1,400 same-size files share one stage-3
  bucket (same size, same first 64 KiB) and reach `compare_stage` as a single
  group. `partition_blocks` picks a pivot and compares everyone else's 128
  KiB chunk against it, one pass per pivot — cheap when most files match, but
  this bucket is mostly unique content, so most passes strip off only the
  pivot. That's on the order of n²/2 block comparisons — roughly 10⁶ here —
  and it accounts for most of `samesize`'s ~1.8s scan time. Comparison-only,
  no I/O: a CPU cost riding on top of bytes already being read.
- **Group-level parallelism in stage 5.** `compare_stage()` runs groups one at
  a time with members read in parallel — deliberate, so a single group of two
  enormous files can use the whole reader pool. But a tree of thousands of
  two-member groups leaves most of the pool idle: each group can occupy only
  `len(members)` readers. Overlapping small groups (while keeping one big
  group able to saturate) is a real scheduling idea. Watch the memory budget
  — `COMPARE_BUDGET` is currently per-group.
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

**STOP. Log it in `bench/LOG.md` with the exact command and the exact output.
Do not "fix" the harness.** A benchmark that you have repaired to make your own
change look good is worth nothing, and from the outside there is no way to tell
that apart from an honest repair. The user will look at it.

---

## Before your first change

```
cd /Users/dogaozden/AI_Projects/dedup
git checkout -b luna
python3 -m pytest tests/ -q          # expect: 117 passed
python3 bench/run.py                 # expect: exit 0, a baseline table
```

If the pytest count is not 117, or `bench/run.py` does not exit 0 on an
unmodified tree, you have an environment problem, not an optimization problem.
Log it and stop.
