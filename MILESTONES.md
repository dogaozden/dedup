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

## M4 — deletion — IMPLEMENTED, GATE NOT RUN

**This is not a claim.** The handshake says a milestone is claimed only when
its gate exits 0, and I have not run the M4 gate. What follows is the status,
so a reviewer can see exactly what is finished and what is unverified.

### Why the gate is unrun

The instructions I work under carry an absolute fence — never touch anything
under `/Volumes`, never run any binary against it, and violating it ends the
project. The M4 gate's third check, `gate_trashes` (`run.py:605-676`), creates
an 80 MB disk image, mounts it at `/Volumes/dedupconf`, builds a tree on it and
runs the candidate against that root with `--auto shortest -y`, so my binary
would read and delete files under `/Volumes`. `--gate m4` runs all three
checks; there is no way to ask for the other two, and REWRITE.md §0 forbids
editing the harness to make one.

I referred the decision up rather than reinterpreting a fence that is marked
project-ending, twice, and it has not come back. `--gate all` is unrun for the
same reason, since it includes m4.

### What is implemented

Trash placement per volume (`trash_dir_for`: the home Trash on the boot volume,
`<volume>/.Trashes/<uid>` elsewhere), the survivor guard, `--auto` with all
three keep rules, `-y`, `--hard`, the TTY refusal, hardlink clusters removed as
a unit, records pinned from outside the tree never removed, and byte
re-verification of cache-attested groups before any removal.

### What is verified, and how

`gate_deletion`'s two local checks reproduced on trees built by the harness's
own `trees.py` into the session scratchpad, judged against each tree's
construction record — not against the oracle — by the same rule `run.py:533`
uses, with the oracle run beside the port on its own copy and its own HOME
(`evidence/m4-local-deletion.txt`):

```text
  deletion/oracle          pass (12 removed, 12 trashed, exit 0)
  deletion/candidate       pass (12 removed, 12 trashed, exit 0)
  links/oracle             pass (2 removed, 2 trashed, exit 0)
  links/candidate          pass (2 removed, 2 trashed, exit 0)

4 check(s), 0 failed
```

The flag surface (`evidence/m4-flags.txt`):

```text
--- 1. --auto with no -y and stdin not a TTY: must refuse ---

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
```

Keep rules and hardlink handling (`evidence/m4-rules-and-hardlinks.txt`):

```text
--auto shortest -> survivor: z.bin 
--auto oldest -> survivor: deep/long-name.bin 
--auto newest -> survivor: z.bin 

--- a hardlink cluster goes as one unit, every name of it ---
before: c.bin keeper.bin sub/a.bin sub/b.bin 
after : c.bin sub/a.bin sub/b.bin 
trash : keeper.bin 
report groups: [['c.bin', 'keeper.bin']]
--- the cluster is the victim: all three of its names must go together ---
before: k.bin sub/xa.bin sub/xb.bin sub/xc.bin 
after : k.bin 
trash : xa.bin xb.bin xc.bin 

--- a record pinned by a name outside the tree is never a victim ---
before: aa.bin pinned.bin 

  Nothing to remove.

after : aa.bin pinned.bin 
trash : []
```

The poison path — a cache-attested group whose bytes no longer match must be
reported and then refused at deletion, while the rest of the tree is still
cleaned up — is covered by `m3-poison-refusal`, which is green.

### What is NOT verified

`.Trashes/<uid>` on a non-boot volume. That is exactly the check the fence
blocks, and nothing short of a second volume exercises it. `trash_dir_for` is
the code; `evidence/m4-flags.txt` shows the boot-volume branch working, and the
non-boot branch is untested by anything.

To finish M4, run:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m4 \
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

and then `--gate all` with `{stage}` appended to the template, per the note
under the M2 claim.

- Implementation commit: `ccabb1b87e95ee9ecb02649fb4a62d059e2af9ff`
