# Milestones

## M1 — walk parity — RE-CLAIMED after REVIEW-M1 CHANGES

Implementation provides repository scaffolding, the M1 CLI skeleton,
portable directory traversal with oracle-ordered exclusions, size grouping,
hardlink collapse, extension/name filters, and the JSON conformance report.
macOS `getattrlistbulk(2)` remains explicitly deferred in `LOG.md`.

- Implementation commit addressing all binding review items:
  `06a5f640c11d7cdde9b98842f0f383960d36de00`
- Gate command: `python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'`

Final gate output, captured to `/tmp/dedup-rs-m1-gate.txt` by redirecting
stdout of the command above:

```text

M1 gate
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
gate m1: green

```

## M2 — pipeline parity — RE-CLAIMED after REVIEW-M2 CHANGES

Stages 3 to 5 complete: the two-tier small-file stage, the one-open end probe,
the deep-head remainder, sample windows past `SAMPLE_SPAN`, and a wave-based
lockstep byte comparison that reuses the head and tail bytes stage 3 kept,
revalidated by `fstat` before they are trusted. `-j` is honoured by both pools.

- Implementation commit addressing all three binding items: `9c3c5a8c9879198843282ff5f1ab1bcf3b4b13b6`
  (pipeline in `56b13e8`, review fixes in `9c3c5a8`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m2-gate.txt` by redirecting that command's stdout, and
quoted from the file:

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
```

The command exited 0.

### The three binding items

**1. Memory was 280 B/file against §6's 230.** Three per-file costs went, all
of them real waste rather than accounting: the walk built an `Entry` list and
then a `FileRec` list beside it, so every surviving file was represented twice
at peak — the walk now builds records directly and `candidates` compacts them
in place; the report cloned every reported path, so it now borrows them and
serialises straight to the file; and the two retained-byte vectors, empty on
every record that never reaches stage 3, became one optional box, which is 8
bytes instead of 48. Measured the way REVIEW-M2 measured it — same corpus shape
at two file counts so fixed buffers cancel, minimum of three runs:

```text
minimum of three runs each, marginal cost over 50,000 extra files

names              50,000 files  100,000 files     B/file
16 characters          19906560       27672576      155.3
44 characters          21463040       30425088      179.2

REWRITE.md §6 budget: 230 bytes per file record
```

The reviewer's sentence about the entry list is corrected in LOG.md: it was
O(files), not a constant, and it was the largest single reason the figure was
280.

**2. The glob engine backtracked exponentially.** `-x '*a*a*a*a*a*a*a*a*a*a*a*b'`
against one 40-character name took 121 seconds. The `*` branch is now a
bottom-up fill over (token index, character index), so each pair is decided once
and the whole match is O(tokens x characters). The comment that claimed this was
already handled is gone.
`filters::tests::a_pathological_pattern_still_finishes_promptly` runs the
reviewer's three patterns and fails if they take over a second; they take
microseconds.

**3. The quoted gate commands were unrunnable.** A scripted find-and-replace had
expanded `evidence/m4-flags.txt` into the literal `{flags}` token in four gate
commands across MILESTONES.md and LOG.md. All four are restored. I copied the
command block above out of this file and ran it unmodified to check.

### Effort counters against the oracle

Required: `bytes_read` within +/-10% of the oracle's instrumented count and not
below the tree's floor, `opens` no greater than the oracle's. Measured: both are
*identical* to the oracle on all 13 cases. The table is in LOG.md.

### Performance against REWRITE.md §6

Alternated timed runs (no hyperfine on this machine) on a generated corpus of
50,000 files, 512 B to 4 KiB, across 250 directories, re-taken at this commit:

- `-j 8`: oracle 5.057s, dedup-rs 1.710s — **2.96x** (target >= 1.3x)
- `-j 1`: oracle 10.506s, dedup-rs 8.112s — **1.30x** (target >= 1.0x)
- record memory: **155 B/file** (16-char names), **179 B/file** (44-char names),
  against a 230 budget
- bytes read: never more than the oracle on any tree — exactly equal on all 13

Absolute times are higher than the first measurement because other agents share
this machine; the ratios are what alternated runs are for. Full output in
`evidence/perf-j8.txt` and `evidence/perf-j1.txt`.

### Standards

`cargo test` 45 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0, all from a clean rebuild
(`evidence/final-standards.txt`).

### Note on `--stage size`

REVIEW-M2 ruled the switch acceptable and flagged that shipping it unguarded on
a deletion tool was not. That is binding item 1 of REVIEW-M4 and is fixed; see
the M4 claim below.

## M3 — cache — CLAIMED

One sqlite database per volume UUID under `DEDUP_CACHE_DIR` (falling back to
`~/Library/Caches/dedup`), schema version 3, rows keyed by inode and qualified
by (size, mtime_ns), with the columns the oracle's stages fill: ends, deep,
sample, full. `--no-cache` opens nothing and writes nothing.

The cache narrows and never proves. A group whose members all carry remembered
whole-file digests is reported as attested and read again before anything in it
is deleted; on mismatch the whole group is refused and its rows discarded. A row
is trusted only while the file's mtime second is strictly older than the second
the row was written, and a racy row is dropped whole rather than re-stamped
column by column.

- Implementation commit: `71de381f668e729577b5410608d76c081d2acf20` (cache and deletion in `9612376`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m3 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m3-gate.txt` by redirecting that command's stdout, and
quoted from the file:

```text

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

4 check(s), 0 failed
gate m3: green
```

The command exited 0. M2 was re-run at the same commit and is still green
(`evidence/m2-gate.txt`, `13 check(s), 0 failed`).

Note on ordering: this gate cannot pass without deletion. `m3-poison-refusal`
runs `--auto shortest -y` and judges the result by which files are left on
disk, so M4's machinery had to land first. It is recorded in LOG.md as a
harness/spec discrepancy rather than a change of plan.

### Standards

`cargo test` 39 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0.

## M4 — deletion — RE-CLAIMED after REVIEW-M4 CHANGES

Trash placement per volume, the survivor guard, `--auto` with all three keep
rules, `-y`, `--hard`, the TTY refusal, hardlink clusters removed as a unit,
records pinned from outside the tree never removed, and byte re-verification of
cache-attested groups before any removal.

- Implementation commit addressing all four binding items: `9c3c5a8c9879198843282ff5f1ab1bcf3b4b13b6`
  (deletion in `9612376`, review fixes in `9c3c5a8`)
- Gate command:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m4 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m4-gate.txt`, quoted from the file:

```text

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

3 check(s), 0 failed
gate m4: green
```

The command exited 0.

### 1. `--stage size --auto` no longer reaches the deletion path — structurally

The reviewer's reproduction, re-run against this commit
(`evidence/m4-stage-guard.txt`):

```text
=== REVIEW-M4 item 1 reproduction: --stage size --auto must not delete ===
before:
  alpha.bin    791235c06f572a70cb55f829507b4926
  bravo.bin    64caede5d77a72aaaf89add5cd1defde
  charlie.bin  5db22b7a73818e9172cd65da96a9fb8c
dedup-rs: --auto cannot act on a --stage size scan. Size buckets are files that merely share a length; nothing has read their contents, so removing one would not be removing a duplicate. Re-run without --stage.
EXIT: 2
after:  alpha.bin bravo.bin charlie.bin 

=== same tree, no --stage: normal behaviour, three distinct files, nothing to remove ===

  Nothing to remove.

EXIT: 0
after:  alpha.bin bravo.bin charlie.bin 

=== and a real duplicate still deletes, with the oracle's lowercase wording ===

  Keeping the shortest copy in each of 1 groups.
  Move to Trash: 1 files · 16 bytes reclaimed

after:  alpha.bin bravo.bin charlie.bin 
trash:  zz-copy.bin 
```

Three files with three different contents; all three survive, the exit is 2, and
the message says why. A real duplicate in the same tree still deletes normally.

The fix is the one REVIEW-M4 asked for rather than the minimal one.
`Scan::groups` is private. It comes out two ways: `reported()`, used to build
the protocol report, where a size bucket is an honest answer; and `proven()`,
which returns `Option<ProvenGroups>` and yields `None` for a size-only scan.
`run_auto` takes a `ProvenGroups`, whose field is private to `pipeline`, so no
other module can construct one. A future caller cannot reintroduce this by
forgetting a string comparison, because there is no string comparison — the
deletion path will not type-check with unproven groups.

### 2 and 3. The keep rules now pick the copy the oracle picks

`newest` used `max_by_key`, which returns the *last* maximum where Python's
`max` returns the first, so a tie kept the longest-named copy instead of the
shortest-named one. And the rules compared `mtime_ns` where the oracle compares
`st_mtime` as a float64, which resolves about 240 ns — so files a nanosecond
apart were ordered here and tied there. Both fixed: the comparator is reversed
and minimised to get the first maximum, and `oracle_mtime` reproduces
`seconds + 1e-9 * nanoseconds` exactly. `mtime_ns` remains the cache key and the
retained-bytes revalidation, where being stricter than the oracle only ever
means reading more; that split is recorded in LOG.md.

Every rule on every tie case the reviewer named, oracle beside port on identical
trees (`evidence/m4-keep-rules.txt`):

```text
case                     rule       oracle keeps   dedup-rs keeps 
all mtimes equal         shortest   c.bin          c.bin          same
all mtimes equal         oldest     c.bin          c.bin          same
all mtimes equal         newest     c.bin          c.bin          same
two tied newest          shortest   c.bin          c.bin          same
two tied newest          oldest     c.bin          c.bin          same
two tied newest          newest     bb.bin         bb.bin         same
sub-microsecond oldest   shortest   c.bin          c.bin          same
sub-microsecond oldest   oldest     bb.bin         bb.bin         same
sub-microsecond oldest   newest     c.bin          c.bin          same
plainly ordered          shortest   c.bin          c.bin          same
plainly ordered          oldest     aaa.bin        aaa.bin        same
plainly ordered          newest     c.bin          c.bin          same

0 rule check(s) differing
```

### 4. The plan prints the rule's own name

```text
  Keeping the shortest copy in each of 1 groups.
```

`71de381` added `Rule::name()` and a `Display` impl and never changed the call
site. It does now.

### Fence conditions this run was made under

`ls /Volumes` immediately before each gate run: `dedupbench`, which is another
project's and which I never touched, and a stray `]` entry that appears in no
mount table — the reviewer independently identified it as a symlink to `/`. No
drive named "My New Passport" and no unexpected external volume at any point.
`/Volumes/dedupconf` was touched only through the harness invocation; no command
of mine ever named a `/Volumes` path. `/Volumes` held exactly the same two
entries after both runs.

### Also verified, beyond the gate

- `gate_deletion`'s two local checks reproduced against each tree's construction
  record with the oracle running beside the port (`evidence/m4-local-deletion.txt`).
- The flag surface (`evidence/m4-flags.txt`) and hardlink handling
  (`evidence/m4-rules-and-hardlinks.txt`).
- The poison path is `m3-poison-refusal`, which is green.

## Whole-project gate — `--gate all`

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate all \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/all-gate.txt`, quoted from the file:

```text

M1 gate
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

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

33 check(s), 0 failed
gate all: green
```

The command exited 0. **33 checks, 0 failed, across all four milestones.**

The `{stage}` token is in this template and only this one, for the reason set
out under the M2 claim: the M1 gate grades the size-bucket candidate set, which
a finished pipeline no longer reports by default. Each individual milestone
claim above uses the prescribed template without it.
