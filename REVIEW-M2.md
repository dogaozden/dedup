# REVIEW-M2.md — maintainer review of the M2 claim

**Verdict: CHANGES**

Claim commit `ce97445eadd49bac5fd56a3a794831d3b4458c58` (pipeline in `56b13e8`),
reviewed at HEAD `ce9fbc5e510dd31f56af977cfc16dbc66537d092`.
Reviewed against REWRITE.md and `dedup.py` at oracle `b6dd81f`.

The pipeline itself is the strongest part of this port. I line-read all five
stages against `dedup.py` and could not find a semantic divergence in the
narrowing logic — the constants, the two-tier small-file stage, the one-open
end probe, the deep-head remainder, the sample geometry, the wave-based
compare and the retained-bytes revalidation are all faithful, including the
subtle points that are easy to get wrong. The gate is genuinely green and the
transcript is genuinely captured. Byte and open counts are not merely inside
the tolerance, they are *exactly equal* to the oracle on every tree I built.

M2 does not pass yet for three reasons, none of them in the pipeline: the §6
memory budget is exceeded and the claimed figure does not reproduce, a
pathological-input defect that REVIEW-M1 flagged was carried forward under a
comment asserting it was fixed, and the claim record in MILESTONES.md is
corrupted to the point that the gate commands it quotes cannot be run.

The `--stage size` arrangement is **ruled acceptable** — see the section below.

---

## Evidence — what I ran myself

Clean rebuild from HEAD (`rm -rf target` first):

```
   Compiling dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-rs)
    Finished `release` profile [optimized] target(s) in 22.56s
[exited with code 0]
```

M2 gate, prescribed template, run from this repo:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

```text

M2 gate
  mixed                    pass
  mixed-minsize            pass
  mixed-ext                pass
  mixed-skipext            pass
  mixed-exclude            pass
  smallzoo                 pass
  smallzoo-j1              pass
  prefix                   pass
  bighead                  pass
  bighead-j8               pass
  links                    pass
  adversarial              pass
  deletion                 pass

13 check(s), 0 failed
gate m2: green
EXIT=0
```

`diff` against `evidence/m2-gate.txt`: **identical**, character for character.
REVIEW-M1 item 8 is cleared for the output blocks — this one was captured, not
retyped. (It is not cleared for the command lines; see item 3.)

Standards at HEAD:

| check | command | result |
|---|---|---|
| tests | `cargo test` | `39 passed; 0 failed` |
| clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| rustfmt | `cargo fmt --check` | exit 0 |

Fence: the oracle clone at `/Users/dogaozden/ox/dedup-oracle` has an **empty
`git status` and sits at `b6dd81f`** — `conformance/` and `dedup.py` are
unmodified. `dedup-rs` had a clean working tree at HEAD before I wrote this
file. No candidate code reads anything outside the scanned root: I grepped
`src/` for `manifest`, `.trees`, `seed`, `20260822`, `oracle_driver` and
`expected` and found no hit that was not ordinary prose in a doc comment.

---

## Differential testing beyond the gate

32 comparisons, oracle driver vs port, on trees I generated with a different
shape and seed from anything in the conformance corpus — sizes sitting exactly
on `HEAD_BYTES`, `DEEP_HEAD`, `DEEP_HEAD+TAIL_BYTES` and `SAMPLE_SPAN` with
first-byte and last-byte variants; files past `SAMPLE_SPAN` differing only
mid-file; emoji and CJK names; hardlink clusters including one pinned from
outside the root; `chmod 000` files and directories; 1,200 tiny files. Each at
`-j1`, `-j4` and `-j8`, comparing `groups`, `files_seen`, `bytes_seen`,
`unreadable`, `opens` and `bytes_read`.

**30 of 32 identical on every field.** The two that differed were exit codes
only, on `--ext gz --skip-ext gz` and `--ext 'jpg,,gz'`, and that is a harness
artifact, not a defect: `oracle_driver.py:260-270` wraps every exception and
returns 1, while `dedup.py` itself exits 2. Run against the oracle directly,
both implementations exit 2 with the same message:

```
dedup:    in both --ext and --skip-ext: gz          exit 2
dedup-rs: in both --ext and --skip-ext: gz          exit 2
dedup:    --ext: empty extension in 'jpg,,gz'       exit 2
dedup-rs: --ext: empty extension in 'jpg,,gz'       exit 2
```

REVIEW-M1 items 1-4 (glob classes, `-x` directory pruning, extension conflicts
and empty tokens, single-dot normalization) are all **cleared**, verified
differentially rather than by reading the diff.

---

## The `--stage size` arrangement — ruling

**Ruled: acceptable engineering, not a gate dodge.** Reasons, in order.

**The problem is real and I reproduced it exactly.** With the prescribed
template the M1 gate fails 9 of 13:

```
13 check(s), 9 failed

GATE FAILURE
  mixed: wrong groups — 1 groups are missing, ...
  smallzoo: wrong groups — group 0 differs — expected ['boundary/65537-a.bin',
    'boundary/65537-b.bin', 'boundary/65537-x.bin'] but got
    ['boundary/65537-a.bin', 'boundary/65537-b.bin']
  ...
```

The failure shape is diagnostic and exonerating: the port reports the two
`65537` files whose bytes match and the answer key wants all three files of
that size. It is failing by being *more* correct than the gate expects.
`check_sets` grades m1 against `key["size_groups"]` (`run.py:223-232`), on the
stated premise that "under M1 the candidate has no content stages yet". That
premise expires when stage 3 lands, so a finished pipeline cannot pass m1, and
`--gate all` could never be green.

**The switch is semantically faithful.** `size_stage` (`pipeline.rs:53-86`)
runs the real `walk`, the real `candidates` (hardlink collapse and
unique-size discard), keeps buckets of more than one record, and sorts with
the same comparator the full path uses. That is precisely what the harness's
own `oracle_driver.py:153-177` does to the oracle. It reports the same
protocol object; it fabricates nothing.

**It cannot leak into another gate.** `expand` (`run.py:110-124`) substitutes
`{stage}` with `"--stage size"` for m1 and `""` everywhere else, so m2, m3 and
m4 are unaffected by the token's presence. I verified this by running m2, m3
and m4 with the prescribed template (no token) and `--gate all` with it, and
getting green both ways.

**The LOG.md write-up is honest.** `LOG.md:321-343` states the harness README's
guidance against the flag, states the 9-of-13 failure, states that the harness
was not touched, and states that every milestone claim uses the prescribed
template. I checked each of those claims and each is true.

**The one thing against it, recorded rather than held against M2.** The
harness is explicit that this is not for candidates: `conformance/README.md:29`
calls `{stage}` "nothing, for you. Ignore it; it exists for the oracle", and
`README.md:137-138` says "no special flag on your side". So the switch is
mirroring a flag the harness defines for its reference implementation, not one
it offers candidates. Given the premise has demonstrably expired, adding it and
disclosing it is a better answer than a red `--gate all`, and I would rather
have this documented deviation than a silently unrunnable whole-project gate.

**Caveat that is not M2's problem but is the switch's.** Shipping `--stage size`
without the guard `oracle_driver.py:277` puts on the same flag opens a
deletion path that destroys non-duplicates. That is binding item 1 of
REVIEW-M4.md. The switch is fine; shipping it unguarded on a deletion tool is
not.

---

## Performance against §6

Alternated timed runs on a corpus I generated (60,000 files, 512 B – 4 KiB,
300 directories, half the content shared — the many-small-files shape §6 names).
Page cache warmed with one untimed run of each side. Verbatim:

```text
--- -j 8 ---
pass 1: oracle 3.46s (rss 141 MiB) | dedup-rs 1.34s (rss 100 MiB) | 2.58x
pass 2: oracle 3.21s (rss 141 MiB) | dedup-rs 1.37s (rss  98 MiB) | 2.34x
pass 3: oracle 3.08s (rss 141 MiB) | dedup-rs 1.25s (rss 100 MiB) | 2.46x
pass 4: oracle 2.49s (rss 142 MiB) | dedup-rs 1.22s (rss  97 MiB) | 2.04x
pass 5: oracle 2.93s (rss 141 MiB) | dedup-rs 1.84s (rss  98 MiB) | 1.59x
--- -j 1 ---
pass 1: oracle 11.05s (rss 139 MiB) | dedup-rs  8.59s (rss 97 MiB) | 1.29x
pass 2: oracle 10.13s (rss 141 MiB) | dedup-rs  8.40s (rss 98 MiB) | 1.21x
pass 3: oracle 10.04s (rss 140 MiB) | dedup-rs  8.53s (rss 96 MiB) | 1.18x
pass 4: oracle 11.13s (rss 139 MiB) | dedup-rs  9.60s (rss 97 MiB) | 1.16x
pass 5: oracle 12.18s (rss 139 MiB) | dedup-rs 10.74s (rss 94 MiB) | 1.13x
```

Median **2.34x at -j8** (target ≥1.3x) and **1.18x at -j1** (target ≥1.0x).
Both targets met on every single pass. The claimed 2.33x at -j8 reproduces
almost exactly; the claimed 1.32x at -j1 is at the top of my range rather than
the middle, which I attribute to a shared machine and do not hold against you.
The port also uses consistently *less* memory than the oracle here.

Effort parity on the same run:

```
  files_seen   oracle=60000        rust=60000        same
  bytes_seen   oracle=137416182    rust=137416182    same
  opens        oracle=59993        rust=59993        same
  bytes_read   oracle=137402414    rust=137402414    same
  groups       oracle=40           rust=40           same
```

On a second, deliberately unfavourable corpus (60,006 files, 2.5 GiB, sizes
spanning every stage boundary plus 9–12 MiB files) the port is 1.19–1.58x at
-j8 and 1.12–1.14x at -j1, dipping to **0.86x on one -j1 pass**. That is not a
§6 failure — §6's target names the many-small-files corpus — and it is exactly
what §1 predicts, since Rust buys nothing on byte comparison. I record it so the
2.33x is not read as a universal figure. Effort parity held there too:
`opens` 81,338 and `bytes_read` 1,883,732,233 on both sides, all 414 groups
identical.

---

## Binding items

### 1. Per-file memory is over §6's budget, and the claimed 200 B/file does not reproduce

I measured marginal peak RSS the same way you did — same corpus shape at two
file counts so fixed buffers cancel, and only a handful of distinct contents so
the small-file stage's retention stays constant. Minimum of three runs each:

```
20,000 files   14,974,976 bytes
60,000 files   26,198,016 bytes      slope 20k→60k:  280.6 B/file
120,000 files  42,991,616 bytes      slope 60k→120k: 279.9 B/file
```

Linear across a six-fold range at **280 bytes per file**. Name length moves it
but never under the budget — with 13-character relative names it is 264 B/file
(20,234,240 → 33,439,744 over 50k files), and with 45-character names 350 B/file
(26,787,840 → 44,269,568). REWRITE.md §6 allows 230.

The structural cause is identifiable. `candidates` (`walk.rs:210-243`) takes
`entries: Vec<Entry>` **by value and keeps it alive for the whole function**
while pushing into `out: Vec<FileRec>`, so at peak the process holds one
`Entry` (64 bytes plus its name allocation) *and* one `FileRec` (112 bytes plus
its names vector) for every surviving file at once. That is not a fixed cost.

This matters because LOG.md:281-283 says the opposite:

> Total RSS is larger than 230 bytes x files and always will be: it also carries
> the walk's entry list and one batch of read buffers (`RETAIN_CHECK_SPAN` files
> in flight). Those are constants in the file count, not per-record costs.

The read buffers are constant in the file count. The walk's entry list is not —
it is O(files) and it is the largest single reason the marginal figure is 280
rather than 200.

**Change:** either bring marginal peak RSS to ≤230 B/file (draining `entries`
as records are built, so the two vectors do not both hold every file, is the
obvious route), or — if you believe §6's "230 bytes/file for records" means the
`FileRec` struct alone and not the per-file peak — say so explicitly in LOG.md,
state which quantity your number measures, and correct the "constants in the
file count" sentence either way.
**I will verify by** re-running the two-corpus slope at 50k/100k and requiring
either ≤230 B/file or a LOG.md entry that makes the definition and the
structural cost explicit.

### 2. The glob engine backtracks exponentially — 121 seconds on one 40-character name

REVIEW-M1 raised this as a non-binding note ("worth not carrying forward"). It
was carried forward, and the code now carries a comment claiming it was fixed
(`filters.rs:229-234`):

```rust
(Some(Token::Any), _) => {
    // Only the longest run of a leading `*` needs trying, and only
    // from each position once: without the early exit a pattern
    // like `*a*a*a*` backtracks exponentially over a long name.
    (0..=text.len()).any(|index| inner(&tokens[1..], &text[index..]))
}
```

There is no memoization and no early exit in that expression — it *is* the
exponential backtrack the comment describes as avoided. Measured on a tree
holding exactly one file named `aaaa…aaaa.bin` (40 a's):

```
dedup-rs  -x '*a*a*a*a*a*a*a*a*a*a*a*b'   117.99s user   2:01.83 total
oracle    -x '*a*a*a*a*a*a*a*a*a*a*a*b'     0.07s user   0:00.32 total
dedup-rs  -x '*a*a*a*a*a*a*a*a*a*a*b'      35.01s user   0:35.77 total
```

Two minutes against the oracle's third of a second, and removing one `*a*`
divides the time by ~3.4, which is the signature of exponential growth. The
oracle is linear here because `fnmatch` compiles to a regex. This is a
user-supplied `-x` pattern on a tool people point at large trees.

**Change:** make the `*` branch linear — memoize on (token index, text index),
or match the trailing literal segments greedily. And fix the comment: a comment
that describes a property the code does not have is worse than no comment.
**I will verify by** re-running the three commands above and requiring the port
to finish each in under a second, plus a unit test with a pathological pattern.

### 3. The claim record's gate commands are corrupted and cannot be run

MILESTONES.md quotes the M2 gate command as:

```
python3 .../run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} --- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  ...
```

The literal token `{flags}` has been replaced by the entire contents of
`evidence/m4-flags.txt`. I confirmed the substitution byte for byte. It has
happened in **three places in MILESTONES.md** (the M1, M2 and M3 claim blocks)
and **two in LOG.md**; `{flags}` now survives at only two places in the repo,
MILESTONES.md:244 and :286. Mechanically this looks like a scripted
find-and-replace that expanded a file into the token, and I read it as an
accident, not an attempt to mislead — the output blocks below the commands are
authentic and I reproduced them exactly.

It still has to be fixed. REWRITE.md §5.2 asks the claim to record "the gate
command and its final lines verbatim", and as it stands the M2 claim records a
command that no shell could execute. REVIEW-M1 item 8 asked for captured
evidence rather than reconstructed evidence; the output half of that is now
right and the command half is not.

**Change:** restore `{flags}` in all five places and re-check that every quoted
command in MILESTONES.md and LOG.md is copy-pasteable as written.
**I will verify by** copying each quoted gate command out of MILESTONES.md and
running it unmodified.

---

## Non-binding notes

- `main.rs:257` prints the keep rule with `{rule:?}`, so the plan line reads
  "Keeping the **Shortest** copy" where the oracle prints "shortest". Commit
  `71de381` ("print the keep rule by its own name rather than its Debug
  spelling") added `Rule::name()` and a `Display` impl but did not change the
  call site, so the fix is not in effect. Carried as binding item 4 of
  REVIEW-M4.md since it is deletion-path output.
- REVIEW-M1 item 6 asked for the `getattrlistbulk` deferral to be logged "with
  a milestone you intend to land it in". It is logged (LOG.md:702-705) but no
  milestone is named — "the obvious next performance item" is not one. Minor,
  and the deferral itself remains reasonable.
- The `sha2` justification (LOG.md:359-369) is sound and I accept it: §7 requires
  a justification for anything off the allowlist, the oracle hashes with
  `hashlib.sha256`, Rust has no standard-library equivalent, and hand-rolling
  SHA-256 in a deletion tool would be a far worse call than one vetted crate.
  One correction: it says sha2 has "no transitive dependencies beyond
  `digest`/`generic-array`/`cpufeatures`", but the build also pulls `typenum`,
  `block-buffer` and `crypto-common`. Immaterial to the ruling, but the log
  should be accurate.
- `sample_points` (`stages.rs:56-63`) computes `size - TAIL_BYTES - SAMPLE_BYTES`
  on `u64`. It is unreachable for `size <= SAMPLE_SPAN` because
  `pipeline.rs:238` gates the stage, so this is correct today; a `debug_assert`
  on the precondition would keep it correct if that gate ever moves.
- `unchanged` and `still_the_same` (`walk.rs:248-261`) revalidate retained bytes
  against `mtime_ns`, where the oracle compares the coarse float `st_mtime`
  (`dedup.py:1129`, `dedup.py:1238`). Yours is strictly stricter — it falls back
  to reading in cases the oracle would trust — so it is safe, and I mention it
  only so it is not mistaken for parity.

---

## What I checked and found correct

Verified against `dedup.py` at `b6dd81f` or by differential run, not assumed.

- **Stage order and constants.** All of `HEAD_BYTES`, `DEEP_HEAD`, `TAIL_BYTES`,
  `SAMPLE_BYTES`, `SAMPLE_POINTS`, `SAMPLE_SPAN`, `CHUNK`, `SLOW_CHUNK`,
  `COMPARE_BUDGET`, `MIN_CHUNK`, `RETAIN_BUDGET`, `RETAIN_CHECK_SPAN`,
  `HASH_BATCH`, `PARTITION_FANOUT` match `dedup.py:47-101` exactly, and
  `DEFAULT_MIN_SIZE` matches `dedup.py:2805`'s 10 MiB.
- **`has_tail`.** `size > DEEP_HEAD + TAIL_BYTES` (`stages.rs:49-51`), which is
  the oracle's real threshold at `dedup.py:771-773` and not the
  `HEAD_BYTES + TAIL_BYTES` a careless port would write. The unit test pins both
  sides of it.
- **Small-file two-tier.** Files ≤ `DEEP_HEAD` never enter the digest stages
  (`pipeline.rs:148-153`); tier A reads the whole file in one open for
  `size <= HEAD_BYTES` and one page otherwise; tier B reads the remainder and
  groups on raw bytes, not a digest. `RETAIN_BUDGET` is checked every
  `RETAIN_CHECK_SPAN` candidates in all three tiers with the overflow going to
  `fallback` and back into the staged pipeline, matching `dedup.py:1474-1480`.
- **End probe.** One open, `HEAD_BYTES` then a seek to `size - TAIL_BYTES`,
  digest over head-then-tail with size *not* mixed in — matching
  `dedup.py:781-798`, and correctly different from the sample digest, which does
  mix size in as decimal ASCII (`stages.rs:123` vs `dedup.py:852`).
- **Sample geometry.** `first + i * ((size - TAIL - SAMPLE - DEEP_HEAD) / 7)`
  with integer division, matching `dedup.py:820-831` including the floor
  behaviour that makes the last window land short.
- **Deep head.** Reads only `HEAD_BYTES..min(size, DEEP_HEAD)`, and the retained
  block is glued on only when the existing run is exactly `HEAD_BYTES` long
  (`stages.rs:234-238`) — the contiguity guard `dedup.py:1996-1999` needs so a
  cache-answered file never gets a later block glued into an earlier one's gap.
- **Wave compare.** `wave_members = max(4*jobs, 16)`, solo groups get the pool,
  smaller groups are packed and each runs as its own task with the chunk sized
  on the whole wave — matching `dedup.py:1286-1336`. `compare_chunk` matches
  `dedup.py:1010-1011`, including treating an unknown device as spinning.
- **Retained-bytes reuse.** `prefix` and `tail` are the minima across opened
  members, `tail` is dropped wholesale if it would overlap `prefix` for any
  member, and an `fstat` on the live handle revalidates before either is
  trusted (`compare.rs:180-206`) — the same shape as `dedup.py:1122-1131`.
- **Short-read looping.** `Effort::read_block` loops to the requested size or
  EOF (`effort.rs:42-55`), with a unit test for the short-file case.
- **Counters.** `opens` increments only after a successful `File::open` and
  `bytes_read` only by the length a `read` actually returned (`effort.rs:22-52`).
  Nothing is derived from a file size. This is what `oracle_driver.py:59-95`
  measures on the oracle side, and the numbers come out exactly equal on every
  tree I tried. There is no mmap, no FFI and no `unsafe` anywhere in `src/`, so
  there is no uncounted read path to hide in.
- **Per-stage pools and `-j`.** `random_jobs`/`sequential_jobs` differ only when
  `-j` is absent; an explicit `-j n` sets both to n (`device.rs:41-61`), which is
  what keeps parity wherever the oracle has an opinion. §4 asks for exactly this
  split.
- **Canonical hardlink path.** Shortest then lexicographic, measured in code
  points rather than UTF-8 bytes (`walk.rs:69-71`), with an emoji test. Matches
  `dedup.py:710-712` and `oracle_driver.py:114-123`.
- **Unreadable.** Bare relative paths, sorted and deduplicated
  (`main.rs:179-180`), matching `oracle_driver.py:138-150`. Verified
  differentially with `chmod 000` files and directories.
- **Group and member ordering.** Members by (code points, name); groups by
  size descending, then keeper basename, then keeper path
  (`pipeline.rs:327-341`) — matching `dedup.py:2053`/`2060` and
  `oracle_driver.py:126-135`.
- **Crate list.** `rayon`, `rusqlite`, `serde`, `serde_json` are all on §7's
  allowlist; `sha2` is the one addition and is justified in LOG.md as §7
  requires. `clap` and `libc` are permitted but unused, which needs nothing.
