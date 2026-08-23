# REWRITE.md — dedup-rs

> **STATUS (2026-08-23): port COMPLETE.** All milestones M1-M4 claimed and
> APPROVED (re-reviews at `rust/REVIEW-M2.md` and `rust/REVIEW-M4.md`). Code
> merged into this repo at `rust/`. Python (`dedup.py`) is now frozen as the
> correctness oracle.

You are an autonomous implementer. Your job: port `dedup` — a duplicate-file
finder that deletes files — from Python to Rust, as a new project `dedup-rs`,
implementing the design lessons measured against fclones. This document is
your whole briefing. Read all of it before writing anything.

The Python tool is the **oracle**. It lives at
`/Users/dogaozden/AI_Projects/dedup` (branch `main`) and it stays authoritative
for behavior forever: where this document and the oracle disagree, the oracle
wins — log the discrepancy, don't guess. It is battle-tested: 171 tests, a
gated benchmark, three optimization campaigns, and a forensic audit stand
behind its current shape. You are porting a *settled* design, not designing.

---

## 0. The fence — read this before anything else

You inherit no other rules, so these are absolute:

- Work ONLY in `/Users/dogaozden/AI_Projects/dedup-rs` (create it) and read
  ONLY from a clone you make of the oracle repo. Never write to
  `/Users/dogaozden/AI_Projects/dedup` or any of its worktrees.
- NEVER touch `/Volumes/My New Passport` or any external volume. Never scan
  a directory outside your repo, your scratch corpora, and your oracle clone
  — not even read-only.
- Deletion code paths run ONLY against scratch corpora you generated
  yourself. Never against real data, ever, in any milestone.
- No sudo. No network calls from the tool. No telemetry.
- Never modify the conformance harness (`conformance/` — maintainer-owned,
  see §2). If a gate seems wrong, write the case in your LOG.md and stop
  touching that area.

## 1. Mission and honest expectations

The port exists for two reasons: thread-parallelism the Python GIL forbids,
and a codebase the owner wants in Rust. Measured reality, so you don't chase
phantoms: best-vs-best, Rust buys ~1.5× on per-file work at 8+ threads
(macOS syscalls dominate at 1 thread — it's a dead heat there), 1.28× on bulk
SHA-256, and nothing on byte comparison (Python's `==` is already memcmp).
The other 3× against fclones came from design, and the design is already in
the oracle. Your targets (§6) are calibrated to that. A port that is
byte-exact, conformant, and 1.3–1.5× faster at `-j8` is a complete success.

## 2. The referee: conformance harness

Maintainers are building `conformance/` inside the oracle repo — a
language-agnostic harness that drives ANY dedup implementation as a
subprocess and compares it against the oracle on identical trees:

- Your binary must implement the **conformance protocol**:
  `dedup-rs --report-json <path>` writes, after any scan, a JSON object:
  `{"groups": [[path, ...], ...], "bytes_read": N, "files_seen": N,
  "bytes_seen": N, "unreadable": [path, ...], "opens": N}`
  with groups sorted by (-size, name, keeper-path) and paths relative to the
  scanned root. Same flags as the oracle otherwise (`--list`, `--min-size`,
  `-j`, `-x`, `--ext`, `--skip-ext`, `--auto`, `-y`, `--hard`, `--no-cache`).
  Two details the harness pins down: a hardlinked inode contributes ONE path
  per group — its canonical name, the shortest, ties broken lexicographically
  (the same name `--auto shortest` keeps); and `unreadable` carries bare
  relative paths with no error text. `opens` counts file opens inside the
  scanned root only (the bulk walk's directory opens are excluded).
- The harness checks: identical groups vs the oracle, bytes within declared
  bounds, opens within declared bounds, deletion outcomes identical
  (survivors, `.Trashes` placement), cache behavior (warm ≈ 0 bytes, poison
  refusal), on the bench corpora plus adversarial trees you never see.
- `bytes_read`/`opens` are self-reported; maintainers spot-audit them with a
  dyld-interpose shim. Lying to the protocol is the one unforgivable failure.

Until `conformance/` lands you may still do M1 groundwork (repo scaffolding,
walk, unit tests), but no milestone is DONE until its conformance gate runs
green. Never substitute your own judgment of "matches the oracle" for the
harness's.

## 3. Inviolables (identical to the oracle's law)

1. **Byte-exact.** Nothing is ever reported or deleted as a duplicate on the
   strength of a hash or sample alone. A digest may only NARROW. The final
   decision for every reported group traces to actual bytes compared this
   scan — via lockstep comparison, or retained-bytes equality for content
   held whole in memory.
2. **At least one survivor per group**, enforced by the code, not the UI.
3. **The cache narrows, never proves.** Deletion always re-proves
   byte-identity of cache-attested groups before removing anything; on
   mismatch it refuses the whole group, loudly, and invalidates the entries.
   Cache entries are distrusted when racily clean: trust a row only if the
   file's mtime tick (whole seconds) is strictly older than the row's
   `cached_at`. A racy row is dropped whole — never re-stamped column by
   column.
4. **Trash goes to the correct volume** (`<volume>/.Trashes/<uid>/` on
   externals, home Trash on the boot volume); `--hard` is the only permanent
   deletion and is opt-in. Without a TTY, `--auto` refuses unless `-y`.
5. **Deterministic output.** Same tree → identical report, byte for byte,
   including group order (size desc, then name, then keeper path).
6. **Hardlinks**: names sharing (dev, ino) collapse to one record carrying
   all names; links outside the scanned tree mark the record undeletable.

## 4. Architecture you are porting (settled — do not re-derive)

Pipeline stages, with the oracle's current constants for orientation (read
the oracle source for exact values at your clone's commit; they win):

1. **Walk.** Bulk metadata: `getattrlistbulk(2)` on macOS (FFI), one syscall
   per directory batch; portable `read_dir` fallback. Only survivors of the
   min-size filter pay an `lstat`. `--ext`/`--skip-ext`/`-x` filtering
   happens here, before stat where possible.
2. **Size groups.** Hardlink collapse, discard unique sizes. Zero reads.
3. **Ends probe.** ONE open per file reads the first 4 KiB (`HEAD_BYTES`)
   and last 4 KiB (`TAIL_BYTES`) together; bucket on (size, ends-digest).
   Small files (≤ `DEEP_HEAD` = 64 KiB) are read whole in that single open
   and grouped on their retained bytes — no digest in the decision path,
   and they skip every later stage. One retained block per distinct
   content, bounded by `RETAIN_BUDGET` (128 MiB).
4. **Deep head** (64 KiB tier) for buckets the ends probe left crowded —
   reads only the part not already read. Then **sampling** for files >
   `SAMPLE_SPAN` (8 MiB): 8 windows × 64 KiB spread over
   `[DEEP_HEAD, size - TAIL_BYTES)`, digest seeded with size.
5. **Lockstep byte comparison** over surviving groups, in waves so `-j`
   spans many small groups while one huge group can still own the pool.
   Chunks: 4 MiB flash / 16 MiB spinning, floor 128 KiB, wave memory ≤
   `COMPARE_BUDGET` (128 MiB). Retained head/tail bytes are compared from
   memory (with fstat size+mtime revalidation before trusting them); files
   diverging stop being read at the diverging chunk. Short reads loop —
   `read(2)` returning less than asked is normal and MUST be retried to the
   requested size or EOF (a missed loop here silently loses duplicates).

**Cache** (SQLite): one DB per volume UUID under `~/Library/Caches/dedup/`
(fallback `~/.cache/dedup/`), `DEDUP_CACHE_DIR` override, schema-versioned;
row = (inode, size, mtime_ns) → ends/deep/sample/full digests + `cached_at`.
Full digests learned opportunistically when stages read full content anyway.
Groups whose members all carry equal trusted full digests are reported as
duplicates without reading ("attested") — and re-proven at deletion (§3.3).
`--no-cache` = read everything, write nothing, bit-identical results.

**Device awareness** (fclones lesson, improves on the oracle): classify each
stage's I/O as Random or Sequential and size pools separately per device
type. Unknown devices (USB enclosures rarely report) get a generous random
pool and a serial sequential pool — the asymmetry is the point: guessing
wrong about random reads on an SSD costs multiples, guessing wrong on
spinning rust costs ~30%. The oracle collapses this to one reader count;
you should not.

**fclones lessons adopted**: one open per small file; per-stage pools;
scriptable output (`--json`, `--csv`; group listing separated from removal);
~180–230 bytes of memory per file record. NOT adopted: fclones'
hash-only verdicts (violates §3.1), `--transform` (out of scope).

**Deferred**: the interactive curses browser is milestone M5 and may be
deferred indefinitely — `--list`, `--json`, and `--auto` are the complete
non-interactive surface.

## 5. Milestones — one at a time, each gated

Every milestone: PR-sized commits on `main` of `dedup-rs`, journal entry per
work session in `LOG.md` (hypothesis → what happened → numbers), rustfmt +
clippy clean, unit tests for every module, and the conformance gate green
before you call it done. A maintainer (Opus) line-reads each milestone's
diff before the next begins — leave it reviewable.

**The claim/review handshake — this is also your stop condition.** Each run
of you is scoped to exactly ONE milestone; you are never asked to "implement
the spec", only "implement M<n>". The protocol:

1. Start of every run: if `REVIEW-M<previous>.md` exists in the repo root,
   read it first. `CHANGES` items are binding — address every one and
   re-claim the previous milestone before touching the new one. Only an
   `APPROVED` verdict unlocks the next milestone.
2. When your milestone's conformance gate exits 0: append a claim entry to
   `MILESTONES.md` (milestone, commit SHA, the gate command and its final
   lines verbatim), commit everything, and **STOP — your goal is complete.**
   Never begin the next milestone in the same run.
3. If the gate will not go green after honest effort: exhaustion note in
   `LOG.md` per §8, and stop. That is also a completed run.

A maintainer reviews each claim (line-reads the diff, independently re-runs
the gate) and writes `REVIEW-M<n>.md` with `APPROVED` or `CHANGES`. The
project as a whole is done when M4 is APPROVED; M5 happens only if
separately commissioned.

- **M1 — walk parity.** Repo scaffold, CLI skeleton, bulk walk + fallback,
  size grouping, hardlink collapse, filters. Gate: identical candidate sets
  (paths, sizes, inode groupings) vs oracle on the conformance trees;
  `files_seen`/`bytes_seen` match.
- **M2 — pipeline parity.** Stages 3–5 complete, `--list`/`--json` output.
  Gate: identical groups on every conformance tree including the bench
  corpora and adversarial trees (symlink loops, unreadable files, unicode
  names, same-size traps, last-byte differences); bytes_read within ±10% of
  oracle; opens ≤ oracle.
- **M3 — cache.** Schema, attestation, racily-clean, `--no-cache`. Gate:
  warm scan of unchanged tree reads ~0 bytes; same-second edit re-read;
  poison (content changed, mtime restored) grouped-then-REFUSED at delete.
- **M4 — deletion.** Trash-per-volume, survivor guard, `--auto` flows,
  `--hard`, TTY refusal. Gate: identical survivors/removals vs oracle on
  scratch corpora; `.Trashes` placement correct; poison refusal end-to-end.
- **M5 — TUI (optional, last).** ratatui port of the browser with the same
  keymap. Gate: manual review; the survivor guard tested in code.

## 6. Performance targets (measured claims only)

On the conformance corpora, after M2: ≥1.0× oracle wall time at `-j1`;
≥1.3× at `-j8` on the many-small-files corpus; memory ≤ 230 bytes/file for
records; never more bytes read than the oracle on any tree. Numbers you
quote come from the harness or from `hyperfine` runs you show verbatim. No
claim without output.

## 7. Code standards

Rust 2021+, stable toolchain. Crate allowlist: `libc` (FFI), `rusqlite`,
`clap`, `rayon` or `crossbeam` (pick one), `serde`/`serde_json`, `ratatui`
(M5 only). Anything else: justify in LOG.md first. `unsafe` only inside the
`getattrlistbulk` FFI module, each block commented with its invariant.
Errors: a file that can't be read is DATA (`unreadable` list), never a
crash; a directory that can't be entered is skipped and reported. Match the
oracle's plain-prose comment voice: explain why, never what.

## 8. What not to do

- Don't redesign the pipeline, re-tune constants, or re-litigate settled
  measurements (SHA-256 vs BLAKE2b, chunk sizes, sample geometry — all
  measured; the numbers live in the oracle's `bench/GOAL.md`).
- Don't build your own verification and declare victory — the harness is
  the referee, and maintainers audit self-reported counters.
- Don't add features the oracle lacks (except `--json`/`--csv`, §4).
- Don't touch the oracle repo, its bench, or its cache directory.
- Don't proceed past a red gate. Honest stuck beats confident wrong: if a
  milestone won't converge, write the exhaustion note in LOG.md and stop.
