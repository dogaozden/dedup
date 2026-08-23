# M2 handoff

## Implemented and believed working

- The existing M1 walk remains in place, including extension filtering,
  name-based exclusion pruning, size counting before `--min-size`, hardlink
  collapse by `(device, inode)`, deterministic report ordering, unreadable-path
  collection, fnmatch-style globs, and the conformance JSON protocol.
- Size candidates are grouped after hardlink collapse.
- A small-file stage exists for candidates at or below `DEEP_HEAD`. It reads
  whole tiny files directly, groups them by exact bytes, reads the first
  `HEAD_BYTES` of larger small files, then reads their remainder from offset
  `HEAD_BYTES` for byte-exact grouping.
- Larger-file scaffolding has staged SHA-256 helpers for both end pages, the
  remainder of a deep head, and sampled middles. Real open/read counters are
  attached to file opens and returned-byte paths.
- `--list` prints either “No duplicates found.” or duplicate group members.

## Written but untested

- The large-file end-page, deep-head-rest, sampling, and final byte-comparison
  stages are incomplete and have not been validated against oracle output.
- Group propagation between these stages is not complete: some intermediate
  buckets are discarded instead of being carried into the next stage.
- Final compare currently opens every member again rather than reusing kept
  head/tail data as the oracle does. Its counters are therefore expected to be
  wrong until that path is rebuilt.
- No unit tests were added or adapted for the new M2 structures. The old tests
  may no longer compile because they reference removed helper functions.

## Not started

- Exact parity with the oracle’s five-stage narrowing.
- Parallelism and `-j` behavior; `-j` is parsed but ignored.
- Cache schema, warm-scan reuse, attested groups, and racily-clean handling.
- Deletion flows, trash placement, survivor guards, TTY checks, and `--auto`.
- macOS `getattrlistbulk(2)` FFI, which was already deferred from M1.

## Current gate status

- The last official run of this partial state failed all 13 M2 cases with wrong
  groups. It was captured in `/tmp/m2-current.txt`, but `/tmp` must not be
  treated as durable evidence.
- No M2 claim exists in `MILESTONES.md`.

## Design decisions and traps

- Paths stored in records are relative to the canonical scan root. They work
  for ordinary relative scans but need review before deletion support.
- `Effort::opened()` increments `opens`; only actual read loops increment
  `bytes_read`. Do not derive counters from file sizes.
- The current implementation intentionally uses SHA-256 to mirror the oracle.
- The oracle’s important constants are: head 4 KiB, deep head 64 KiB, tail
  4 KiB, sample window 64 KiB, eight samples, sample span 8 MiB, minimum
  comparison chunk 128 KiB, comparison budget 128 MiB, retention budget
  128 MiB.
- Small-file handling is special: files at or below the deep-head boundary do
  not go through normal digest stages. Do not accidentally send them there.
- The current source contains unused compatibility helpers and dead stats
  methods left over from refactoring. Clippy will fail until they are removed
  or used.
- The next implementer should read the oracle pipeline around
  `find_duplicates`, `small_file_stage`, `hash_stage`, and `split_identical`
  before changing bucket flow. Preserve stage order exactly.
