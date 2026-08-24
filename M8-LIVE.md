# M8 — the live scan

Commissioned 2026-08-24. The maintainer watched a spinning-drive scan
crawl and asked for the obvious product: see results WHILE it runs,
judge them while it runs, pay the expensive proof only for what gets
deleted, and never lose what a half-finished scan learned. Claim as M8
in `rust/MILESTONES.md`; the maintainer review is BLOCKING
(`rust/REVIEW-M8.md`). This is the largest milestone since the port —
land it as reviewable commits, not one avalanche.

## The one absolute fence

`--list`, `--auto`, `--report-json`, piped runs, and every conformance/
bench invocation keep TODAY'S pipeline and TODAY'S bytes, byte for
byte: full proving, same counters, same output, same exit codes. Every
gate in `conformance/` and every RBENCH/XBENCH number depends on it.
The live scan is the INTERACTIVE browser path only.

## The live browser

- On an interactive launch (menu accepted or bare terminal run), the
  browser opens IMMEDIATELY. The scan runs on worker threads behind it.
- The browser draws the scan's progress itself — stage, bar, counts,
  rate, from the same shared state the pulse reads. The stderr pulse
  line never fights the alternate screen: in live mode the browser IS
  the progress display (the pre-scan stderr line remains only for
  non-browser TTY runs, e.g. --list on a terminal).
- Duplicate groups appear in the list AS THEY ARE FOUND, stably
  ordered (largest reclaimable first, insertion without reshuffling
  under the user's cursor — a new group may insert above, the cursor
  must stay on the group it was on).
- Every browsing action works during the scan: navigate, mark, unmark,
  bulk rules, help. Quitting mid-scan is clean: workers stopped or
  detached safely, terminal restored (all M5 laws), cache flushed.

## Prove-on-delete

- Groups stream in at SAMPLED-MATCH confidence (survivors of the
  head/tail/deep-head/sample stages) and are labeled honestly in the
  UI: candidate, not yet proven.
- Stage 5 becomes deferred for the live path. The full byte proof for
  a group runs when it is needed: at APPLY, for exactly the groups the
  user marked, before anything is trashed. `ProvenGroups` stays the
  only key to deletion; nothing about the type-level guard weakens. A
  candidate group that fails its proof at apply time splits/settles
  honestly on screen and nothing from the failed portion is deleted.
- Idle-time upgrading is the builder's choice (background stage 5
  continuing to promote candidates to proven while the user browses is
  welcome, if cleanly cancellable); correctness never depends on it.
- The apply flow may run while the walk/stages continue or pause them —
  builder's design call, stated in LOG.md with one reason.

## The cache learns from partial scans

- Per-file facts (digests, proofs, attestations) are persisted AS THEY
  ARE LEARNED, transactionally. Killing the scan at any point — q,
  Ctrl+C, SIGTERM, terminal death — leaves a valid cache holding
  everything settled before the kill. No corruption, ever: a torn
  write must be impossible by construction (sqlite transaction/WAL
  discipline), and a cache the next run cannot open is a failed
  milestone.
- Evidence: a scripted run that scans a corpus, is killed at ~half,
  then re-runs — the second run's bytes_read/opens must be materially
  lower, and its answer identical to an uninterrupted scan's.

## --force

- New flag in the FLAGS table (menu and --help inherit it from the one
  table): ignore everything the cache knows for this scan — read no
  verdicts from it — but WRITE fresh learnings as the scan proves
  things. Distinct from `--no-cache`, which neither reads nor writes.
  Help text must state that distinction in one line each.

## Verification

- Model-level tests (terminal-free) for the streaming list: insertion
  order, cursor stability under insertions, candidate/proven labels,
  apply-triggers-proof, proof-failure splitting.
- TestBackend snapshots: live screen with progress + partial list;
  candidate label visible; post-proof state.
- pty end-to-end in `rust/evidence/scripts/` (lane-prefixed names,
  m8_*): mark-and-apply DURING a running scan deletes exactly the
  proven marks and nothing else (build on the m5 end-to-end harness's
  screen-awareness); quit mid-scan restores the terminal (death tests
  inherited and extended to the live screen); the partial-cache
  kill/resume script above; a --force run re-reads what a cached run
  would skip (counters prove it).
- Non-interactive freeze proof: --list/--auto/--report-json outputs and
  all effort counters byte-identical to main on the standard corpora;
  conformance 33/33; RBENCH flash scenarios within noise.
- 194+ tests stay green; clippy -D warnings; fmt.

## For the record

Time-to-first-group is the metric this milestone creates. Future bench
eras may score it; nothing in this milestone may game it beyond
honestly finding groups sooner.
