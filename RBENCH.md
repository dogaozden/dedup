# RBENCH — the wall-clock benchmark for the Rust tool

Commissioned 2026-08-23. Successor to `bench/` (the retired Python bytes
bench). Lives in `rbench/` of the dedup monorepo. Purpose: a gated fitness
function for autonomous optimizer rounds against `rust/` — **time is the
score, bytes are a constraint, correctness is a VOID gate.**

## The score

- `SCORE = geomean over the scored scenarios of (wall_cand / wall_base)`.
  Baseline 1.00 is a PINNED binary: `rbench/baseline.lock` names a commit;
  the harness builds both baseline and candidate at `--release` from clean
  checkouts. The pin moves only deliberately, between rounds, by the
  maintainer. Initial pin: `c238b30` (the monorepo merge).
- Interleaved A/B rounds (baseline, candidate, baseline, …), at least 9 per
  scenario, per-scenario median of ratios. Report per-scenario ratios and the
  geomean. A candidate that fails any gate is VOID — no score, never a
  penalty.
- Scenarios: reuse the Python bench's seeded generators (`bench/corpus.py`,
  Python allowed at *generation* time only) for continuity of names and
  shapes; the scored set and scales live in one place in `rbench/run.py`.
  Include at minimum: needle, typical, samesize, widegroup, photos,
  bigheader, plus a `rescan` (warm-cache) scenario now that the cache is
  first-class. Same-volume discipline as the old bench: corpora live on the
  `dedupbench` RAM disk (`/Volumes/dedupbench` — the ONLY `/Volumes` path the
  harness may touch, mounted and capacity-managed the way `bench/run.py`
  does), advisory lockfile plus live-process check, never `/Volumes/My New
  Passport`, never a tree another agent is using. The RAM disk keeps syscall
  costs real while removing device noise; its blindness to real cold-cache
  I/O is recorded below.

## Gates (all VOID on failure)

1. **Correctness.** Answers derive from corpus construction (the generators
   know every duplicate group they planted). The candidate's `--report-json`
   groups must match exactly, every scored run, both cold and warm.
2. **Bytes ceiling.** At corpus generation time the harness runs the FROZEN
   oracle (`dedup.py`, instrumented as in `conformance/oracle_driver.py`)
   once per scenario and stores `oracle_bytes_read` and `oracle_opens` in the
   manifest. The candidate's self-reported `bytes_read` must be `<=`
   oracle_bytes_read per scenario. Reading less is allowed and is where
   remaining algorithmic wins land; reading more is a regression, VOID.
3. **Counter plausibility.** Self-reported counters are cross-checked with
   `proc_pid_rusage`'s disk-I/O figures on at least one cold-cache run per
   scenario: physical reads wildly exceeding reported logical reads (beyond
   a documented tolerance) is a gate failure. This is a tripwire, not a
   proof — the real enforcement is item 1 plus review.
4. **Setup ceiling** (process start to first real work) and a **manifest
   tripwire** (corpus files' mtimes/sizes unchanged after a scored run),
   both inherited from the old bench's design.
5. **Referee protocol.** The score of record is computed by a clean
   `rbench/` checked out from `main` — a candidate branch's edits to
   `rbench/` are ignored by scoring, and `rbench/` selftests (sabotaged
   candidates: drop a group, lie about bytes, touch the corpus) must be
   caught. Optimizer rounds additionally get the claim/review handshake.

## Timing methodology

- The harness times the child process wall-clock (spawn to exit), never
  trusts self-reported timings.
- Warm the page cache identically for both sides (one untimed run each)
  except in the `rescan` scenario, whose cold/warm split is the point.
- Machine-idle discipline: refuse to score while another bench/agent process
  is live (process check), and record load average with every round.
- Publish the noise floor: the harness's `--calibrate` mode times baseline
  vs baseline and reports the spread; score claims within the noise floor
  are not wins, and `RGOAL.md` (the optimizer rules file, written with the
  harness) must say so.

## What this bench is blind to (record in RGOAL.md, inherited lessons)

Spinning-media seek costs (scratch volume is solid-state), real NTFS/exFAT
volumes, and RAM ceilings other than the §6 record budget. Cross-scenario
trades go to the maintainer unmerged, exactly as in the old bench's rules.
