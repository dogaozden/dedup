# XBENCH — the competitive benchmark

Commissioned 2026-08-23. Lives in `xbench/`. Purpose: an HONEST, repeatable
comparison of our Rust tool against the best open-source duplicate finders —
primarily **fclones** (the fastest widely-known one), plus others where
trivially installable (jdupes, czkawka-cli). This is a measurement
instrument, not an RL environment: nobody optimizes XBENCH itself, and its
credibility rests on being scrupulously fair to the competitors. Any number
this suite produces must be one we'd be comfortable publishing next to
fclones' own benchmarks.

## Fairness laws

1. **Same corpus, same volume, alternated runs.** All tools run on the
   identical generated tree, interleaved (A,B,C,A,B,C…, ≥5 rounds each),
   medians reported with dispersion. One untimed prime run per tool first,
   so every tool sees the same page-cache state. Load discipline as in
   RBENCH: refuse to measure while another harness or heavy process is
   live; record load average with every table.
2. **Flag alignment, documented.** Each tool runs with flags that make the
   TASK as identical as the tools allow — same min-size, no symlink
   following, same hardlink treatment, hidden files included — and the
   exact mapping lives in one table in `xbench/tools.py` with a comment per
   choice. Where a tool cannot express our semantics, the difference is
   recorded in the report, not silently ignored.
3. **Correctness is reported next to speed.** Every tool's output is parsed
   and checked against the corpus's construction-derived ground truth.
   The report shows groups-exact / groups-differ (with counts) beside the
   times. A fast wrong answer is reported as exactly that. (This is also
   where our byte-verification guarantee becomes visible instead of
   invisible cost.)
4. **Lanes are never conflated.** Lane A: cold-ish single scan (fresh
   page-cache prime, no persistent caches — our tool runs `--no-cache`).
   Lane B: repeat scan (each tool re-run as a user would re-run it; our
   cache ON, labeled as such). fclones has no persistent cache — that
   contrast is legitimate and must be labeled, never smuggled into lane A.
5. **Versions pinned.** The report records every tool's version, our
   commit, macOS version, volume type, and generation seed. Competitors
   installed via Homebrew at current stable; never patched or forked.

## Scenarios

The seven RBENCH shapes (reuse `bench/corpus.py` — continuity), PLUS:
- `photolib`: a large media-library shape modeled on fclones' own published
  benchmark scenario (many large files, some duplicated) — their home
  field, so a win there means something.
- `backup`: two near-identical copies of a source tree (the classic
  backup-dedup use case), thousands of small files — the walk-speed field.

Scales: the report's headline tables use corpora big enough that per-tool
times are ≥1 s (fixed startup <5%); a smoke scale exists for harness
development only and its numbers are never published.

## Output

`python3 xbench/compare.py` → a Markdown report (`xbench/reports/<date>.md`)
with one table per scenario (tool, median wall, dispersion, correctness,
peak RSS if cheaply measurable) and a JSON sidecar with everything. The
report leads with the fairness table (flags per tool) and the environment
block. No geomean headline across tools — per-scenario tables only; a
single crowned number invites gaming and cross-shape averaging hides
losses. State plainly per scenario who won.

## Fence

Corpora on `/Volumes/dedupbench` for real runs (same lock protocol as
RBENCH — never while a round is being scored); harness development and
function-proof runs on a local scratch directory. Never `/Volumes/My New
Passport`. Competitor tools are run ONLY against generated corpora, never
against any real user data, and never with their deletion modes — scan and
report only, every tool.
