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
`evidence/perf-j1.txt`.

### Standards

`cargo test` 27 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0. Captured in `evidence/m2-standards.txt`.

### One thing the reviewer should know before reading the M1 line above

The M1 gate grades the *candidate set*, so a finished pipeline fails it by
construction. See LOG.md, "Two things the spec and the harness disagree about".
This claim, and every claim below it, uses the prescribed template with no
`{stage}` token.
