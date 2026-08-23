# Handoff — dedup vs fclones

Written at the end of RBENCH optimizer round one, for whoever picks up the
fclones comparison. Everything here is either measured or flagged as unverified.

## State

Branch `opus-r1` @ `7da1ecc`, pushed, **unmerged**, working tree clean.
RBENCH refereed **0.3161** against pinned baseline `c238b30`. 108 tests,
clippy and fmt clean, `--selftest` green before every scored run.

Full reasoning lives in `rust/LOG.md` — summary at the top, five entries below
it, including the ideas that were rejected and why. Read that before
re-deriving anything.

## Where dedup's time actually is now

Candidate seconds at `--scale default`, from the referee run. Measured, not
estimated:

| scenario | seconds |
|---|---|
| needle | 1.6629 |
| typical | 0.1348 |
| widegroup | 0.0511 |
| bigheader | 0.0249 |
| samesize | 0.0243 |
| photos | 0.0231 |
| rescan | 0.0054 |

**`needle` is roughly thirty times everything else combined, and is the only
scenario the scan itself dominates.** It is 100,000 files of about 2.4 KB each.
Every one costs an `fstatat` in the walk plus three allocations per entry:
`entry.path()`, `to_string_lossy()`, and a `replace('\\', "/")` that allocates
even when there is no backslash to replace. The walk is single-threaded.

`getattrlistbulk(2)` (REWRITE.md §4) has been deferred at M1, at M2 and again at
M5. `rust/src/sys.rs` now exists as its home, `libc` is already a dependency,
and the unsafe-FFI justification REWRITE.md §7 asks for is already written in
`rust/LOG.md`. That groundwork is done.

This is the likely battleground against fclones, whose parallel walk is a known
strength.

Round one removed a ~60 ms `diskutil` subprocess from every run — `strings` on
the binary now finds zero references to it, where the baseline has one. Startup
is about 2 ms of spawn. **Do not go looking for another one; that lever is
spent.**

## The trap that could waste the whole round

dedup's inviolable: **nothing is called a duplicate on the strength of a hash.**
A hash may only narrow a candidate set. Stage 5 compares actual bytes, in
lockstep, and splits a group the moment they differ.

If fclones' default path concludes from a digest instead, then "fclones is
faster" may only mean "fclones asserts where dedup proves" — and no honest
optimization closes that gap.

Settle this from **fclones' source**, not from anyone's recollection of it, mine
included: I have impressions of its architecture and they are exactly the sort
of thing that is confidently wrong. Then either match the workloads, or name the
flags that make both tools do the same job — **in the bench's rules, not in
mid-round judgment.**

The same question applies to:

- whether small files skip full verification;
- whether its cache can settle a group without reading it;
- whether hardlinks are folded. dedup treats one inode wearing several names as
  a single record, because deleting fewer than all of them frees nothing.

## The machine

Apple M4, 10 cores (4 performance + 6 efficiency), 16 GB, macOS.

Corpora live on a 2816 MB RAM disk at `/Volumes/dedupbench`, routinely near
full, evicted and regenerated as needed.

**That volume reports no `SolidState`**, so dedup sees an unknown device: 8
random readers, 1 sequential reader, 16 MB comparison chunk. A RAM disk charges
nothing for seeks, so any idea aimed at seek behaviour will look flat there by
construction rather than because it failed.

Per-scenario `--min-size`: needle, typical, samesize and widegroup use 1;
photos uses 1 MB; bigheader uses 10 MB. Scenario shapes come from
`bench/corpus.py`.

## Contention

Other agents optimize on this box — round one raced `sol-r1` and `ox-r1`.
Harnesses refuse to score while a rival `dedup-rs` or `run.py` is live, and take
an advisory lock on the volume.

**Wait, never kill** — including a process that looks hung. One stuck
`dedup-rs` from another session burned eight hours at 100% CPU and blocked all
scoring. Waiting was still the correct call.

Two things worth rebuilding immediately:

- A ~15-line shell wrapper that polls `ps` for rivals and retries on the
  "another harness is live" exit. It turns contention into latency instead of
  failure.
- Pin measurements to a commit (`--candidate <sha>`), so the working tree stays
  free to edit while a run is queued behind someone else's.

## The method that actually produced round one

1. **Read the absolute column before theorizing.** The brief pointed at the
   walk's per-file syscalls. The calibration's absolute times said an empty
   directory took 87 ms and five of seven scenarios finished under 115 ms. The
   algorithm was not the story; the startup was.
2. **Price an idea with a throwaway build before investing in it.** A stubbed
   variant scored 0.2915 and proved the subprocess was worth chasing. It was
   never a candidate for keeping, and the journal says so.
3. **Test a replacement against what it replaced.** The first IO-registry query
   looked for a property that does not exist and returned "don't know" where
   `diskutil` said "yes" — a faster route to a *different answer*. A test that
   ran `diskutil` and demanded agreement caught it. Nothing else would have.
4. **Be most suspicious of wins only this bench can see.** A one-line change
   would have bought most of the same speedup, and was declined: the corpus
   volume reports "unknown", so the single case being tuned is the single case
   the bench charges nothing for — while real USB enclosures, which also report
   nothing, would quietly get worse.

## Unrelated, still outstanding

`python3 conformance/run.py --gate all` fails **9 of 33 checks on `main`
itself** — identical with and without every change on this branch, verified by
stashing and rebuilding. LOG.md records that gate green at M5; it is not green
now. It is not an RBENCH gate and it did not block round one. Reported, not
touched.
