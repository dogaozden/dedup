# dedup-rs work log

## 2026-08-22 — M1

### Hypothesis

The M1 gate could be met with a deterministic portable walk, the oracle's
extension and basename-glob filters, size counting before `--min-size`,
hardlink collapse by `(device, inode)`, and shortest/lexicographic canonical
names.

### What happened

Implemented the M1 CLI skeleton and conformance protocol report. The first
official run passed 12 of 13 cases; `mixed-exclude` failed because the first
glob implementation matched only file basenames and therefore did not prune a
directory named by `-x backup`. Aligned exclusion with the oracle's walk order:
a pattern matches the entry name or any directory component already being
descended. Added tests for extensions, directory-component exclusions, glob
semantics, and canonical hardlink names. Fixed formatting and clippy issues.

### Numbers

- `cargo test`: 4 passed, 0 failed.
- `cargo clippy -- -D warnings`: clean.
- Official gate: 13 checks, 0 failures, `gate m1: green`.
- Post-claim gate rerun against implementation commit
  `aca5a488a3eb488532ea3ae587ec9bd0976486eb`: 13 checks, 0 failures,
  `gate m1: green`.

No discrepancies between REWRITE.md and the oracle clone were observed during
M1. The oracle remains authoritative.
