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

## 2026-08-22 — M1 review response

### Hypothesis

All eight binding REVIEW-M1 items can be addressed without starting M2 by
porting the oracle's filter semantics exactly, adding real read/open counters as
M2 scaffolding, correcting the walk journal, and re-claiming only from captured
gate output.

### What happened

- Replaced the two-token glob matcher with fnmatch-style `*`, `?`, `[seq]`,
  `[!seq]`, ranges, and an unterminated `[` treated literally.
- Moved exclusion before metadata/type dispatch so matching directories are not
  entered; a failed directory-entry iteration now records the parent path.
- Normalized extension tokens with whitespace folding, lowercasing, and at most
  one removed leading dot; rejected empty tokens and identical normalized tokens
  in both whitelists and blacklists with the oracle's messages.
- Added `Effort`, `OpenedFile`, and a short-read loop that counts one open when
  a file is opened and bytes returned by each real read. M1 has no file-content
  reads, so both remain factually zero; a unit test exercises the actual helper.
- Moved the test module to the end of the source file, adopted
  `cargo clippy --all-targets -- -D warnings`, expanded tests for classes,
  normalization/filter errors, hardlink ordering, and effort counting, and made
  `--auto` fail loudly because deletion is out of M1 scope.

The prior statement that there were no spec discrepancies was wrong: portable
walking was used while REWRITE.md §4 specifies macOS `getattrlistbulk(2)` with
a fallback. That FFI remains deferred; it should land with the parallel walk /
performance work after M2, since content-stage parity must be established first
and the conformance protocol does not compare directory-walk syscall counts.

### Differential evidence against oracle clone `b6dd81f`

Scratch-tree comparisons produced identical `groups`, `files_seen`,
`bytes_seen`, `unreadable`, `opens`, and `bytes_read` for:

- `-x '[ab].gz'`
- `-x '[!x]*.gz'`
- `-x '[a-z].bin'`
- `--ext '..gz'`
- `--ext jpg --skip-ext tmp`
- readable files beside a mode-000 directory inside `-x backup`

Rust exits non-zero for `--ext gz --skip-ext gz` and `--ext 'jpg,,gz'`; legal
different extensions continue to scan. The exact stderr text is:

```text
Error: FilterError { message: "in both --ext and --skip-ext: gz" }
Error: FilterError { message: "--ext: empty extension in 'jpg,,gz'" }
```

### Standards

```text
cargo test: 8 passed, 0 failed
cargo clippy --all-targets -- -D warnings: clean
```
