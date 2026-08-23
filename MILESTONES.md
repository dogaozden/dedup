# Milestones

## M1 — walk parity — RE-CLAIMED after REVIEW-M1 CHANGES

Implementation provides repository scaffolding, the M1 CLI skeleton,
portable directory traversal with oracle-ordered exclusions, size grouping,
hardlink collapse, extension/name filters, and the JSON conformance report.
macOS `getattrlistbulk(2)` remains explicitly deferred in `LOG.md`.

- Implementation commit addressing all binding review items:
  `06a5f640c11d7cdde9b98842f0f383960d36de00`
- Gate command: `python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} --- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  Move to Trash: 1 files · 29 bytes reclaimed

      copy-of-a.bin


  Refusing to delete without confirmation. Re-run with -y to proceed.
exit: 1
files still present: a.bin copy-of-a.bin z.bin 
trash: []

--- 2. --auto -y: keeper survives, victim goes to the home Trash ---
exit: 0
files still present: a.bin z.bin 
trash: [copy-of-a.bin ]

--- 3. --hard: permanent, nothing reaches the Trash ---
exit: 0
files still present: a.bin 
trash: []
'`

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

## M2 — pipeline parity — CLAIMED

Stages 3 to 5 complete: the two-tier small-file stage, the one-open end probe,
the deep-head remainder, sample windows past `SAMPLE_SPAN`, and a wave-based
lockstep byte comparison that reuses the head and tail bytes stage 3 kept,
revalidated by `fstat` before they are trusted. `-j` is honoured by both pools.
`--list` and `--report-json` are implemented; `--auto` still refuses.

- Implementation commit: `ce97445eadd49bac5fd56a3a794831d3b4458c58`
  (pipeline in `56b13e8`, stage-2 memory and `--stage size` in `ce97445`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} --- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  Move to Trash: 1 files · 29 bytes reclaimed

      copy-of-a.bin


  Refusing to delete without confirmation. Re-run with -y to proceed.
exit: 1
files still present: a.bin copy-of-a.bin z.bin 
trash: []

--- 2. --auto -y: keeper survives, victim goes to the home Trash ---
exit: 0
files still present: a.bin z.bin 
trash: [copy-of-a.bin ]

--- 3. --hard: permanent, nothing reaches the Trash ---
exit: 0
files still present: a.bin 
trash: []
'
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

### Effort counters against the oracle

Required: `bytes_read` within +/-10% of the oracle's instrumented count and not
below the tree's floor, `opens` no greater than the oracle's. Measured: both
are *identical* to the oracle on all 13 cases. The table is in LOG.md.

### Performance against REWRITE.md §6

Alternated timed runs (no hyperfine on this machine) on a generated corpus of
50,000 files, 512 B to 4 KiB, across 250 directories:

- `-j 8`: oracle 2.488s, dedup-rs 1.075s — **2.31x** (target >= 1.3x)
- `-j 1`: oracle 7.438s, dedup-rs 5.727s — **1.30x** (target >= 1.0x)
- record memory: **200 bytes per file** marginal peak RSS (target <= 230)
- bytes read: never more than the oracle on any tree — exactly equal on all 13

Full output, verbatim, is in LOG.md and `evidence/perf-j8.txt` /
`evidence/perf-j1.txt`. Re-taken against the shipped binary after M3 and M4
landed, since the first measurement predates them: 2.33x at -j8, 1.32x at -j1,
unchanged within noise. The evidence files hold the later run.

### Standards

`cargo test` 27 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0. Captured in `evidence/m2-standards.txt`.

### One thing the reviewer should know before reading the M1 line above

The M1 gate grades the *candidate set*, so a finished pipeline fails it by
construction. See LOG.md, "Two things the spec and the harness disagree about".
This claim, and every claim below it, uses the prescribed template with no
`{stage}` token.

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
  --candidate 'target/release/dedup-rs {root} --report-json {json} --- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  Move to Trash: 1 files · 29 bytes reclaimed

      copy-of-a.bin


  Refusing to delete without confirmation. Re-run with -y to proceed.
exit: 1
files still present: a.bin copy-of-a.bin z.bin 
trash: []

--- 2. --auto -y: keeper survives, victim goes to the home Trash ---
exit: 0
files still present: a.bin z.bin 
trash: [copy-of-a.bin ]

--- 3. --hard: permanent, nothing reaches the Trash ---
exit: 0
files still present: a.bin 
trash: []
'
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

## M4 — deletion — CLAIMED

Trash placement per volume (`trash_dir_for`: the home Trash on the boot volume,
`<volume>/.Trashes/<uid>` elsewhere), the survivor guard, `--auto` with all
three keep rules, `-y`, `--hard`, the TTY refusal, hardlink clusters removed as
a unit, records pinned from outside the tree never removed, and byte
re-verification of cache-attested groups before any removal.

- Implementation commit: `5bb7cf3c1d088c0b61c8cf98a21004bba3f90cc0` (deletion landed in `9612376`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m4 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m4-gate.txt` by redirecting that command's stdout, and
quoted from the file:

```text

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

3 check(s), 0 failed
gate m4: green
```

The command exited 0. `m4-trashes` is the check that mounts a temporary disk
image and requires deletions on a non-boot volume to land in that volume's
`.Trashes/<uid>` and *not* in the home Trash — the one part of M4 no scratch
tree can exercise. It passed.

### Fence conditions this run was made under

The maintainer ruled the harness's own self-created, self-detached image inside
scope, narrowly, with conditions I followed exactly:

- `/Volumes/dedupconf` was touched only through the conformance harness
  invocation. I never pointed the binary, or any command, at a `/Volumes` path
  myself.
- `ls /Volumes` immediately before each gate run. Before and after both runs it
  read `dedupbench` (somebody else's, untouched) and a stray directory entry
  named `]` that is **not a mounted volume** — it is absent from the mount
  table, so nothing is mounted on it. No drive named "My New Passport" and no
  unexpected external volume at any point.
- The harness cleaned up after itself: `/Volumes` is byte-identical before and
  after, with no `dedupconf` left mounted.

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
