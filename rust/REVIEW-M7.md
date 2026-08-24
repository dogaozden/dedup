# REVIEW-M7.md — maintainer review of the M7 claim

**Verdict: APPROVED at `cf62e3c`.** See the re-review at the end of this file.
All three binding items below are discharged and all five non-binding notes
were taken. The first round's verdict is kept unedited beneath, because the
record of what was wrong is the reason the fixes can be checked.

---

**First round — verdict: CHANGES — three binding items, none of them a design
objection**

Claimed at `479ccb0` on branch `m7-pivot`, reviewed at that tip (8 commits atop
`main@cc654c4`). Binding specs are `M7-PIVOT.md`, and `SPIN.md`'s Laws, which
bind M7 too. I read `M7-PIVOT.md`, the M7 sections of `MILESTONES.md` and
`LOG.md`, and `evidence/` first, and then verified everything independently in
a detached checkout of my own at `/Users/dogaozden/AI_Projects/dedup-m7-review`
with a build of `main@cc654c4` beside it; **no number below is taken from the
builder's scripts.** Where I re-ran a builder script I say so and treat it as a
reproduction, not as evidence.

**The sweep is correct.** I hand-derived the never-read-again inference, then
attacked it at every boundary I could construct, and I could not break it. The
classes it settles are the byte-equality classes. Nothing in this review says
otherwise, and nothing below risks user data.

What must change is the **record**: the milestone's headline bytes result is
stated as a universal and is not one, the `PIVOT_GROWTH` constant's doc comment
carries a number that contradicts both the branch's own evidence and `LOG.md`,
and the one test named for the window boundary no longer tests the window
boundaries it names. Each is a few lines. Item 3 is the one I care most about,
because it is a test that has quietly stopped guarding the hottest surface in
the program.

---

## The load-bearing proof — my own derivation

I did not take the doc comment's word for it.

**The claim.** `A` parts from the pivot in run `i` (having agreed with it
through run `i-1`); `B` parts in run `j > i` (having agreed with the pivot
through run `i` inclusive). Then `A ≠ B`, and neither is ever read again to
find out.

**The derivation.** Runs are the pivot's reads. `split_pivot_windows` builds
`held` from the pivot's own `read_block` calls (`compare.rs:759-781`), and every
other member is then asked for exactly those lengths in exactly that order
(`compare.rs:805-827`). So the run boundaries are the same boundaries for every
member of one sweep, and a run comparison is the same `memcmp` on the same
bounds that lockstep performs.

Let run `i` cover the byte range `[x, x+L)`. `B` matched the pivot on the whole
of run `i`, so `B[x..x+L) == P[x..x+L)`. `A` did not, so `A[x..x+L) ≠
P[x..x+L)`. Therefore `A[x..x+L) ≠ B[x..x+L)`, so `A ≠ B`. Byte equality is an
equivalence relation and the disagreement is exhibited on real bytes read this
scan, so no later evidence can overturn it. `A` and `B` are filed under
different keys, are never rejoined, and never need to be. ∎

The two places this could be true on paper and false in the code, and why they
are not:

- **The key is an absolute file offset, not a window-relative one.**
  `Verdict::Apart(offset + done)` (`compare.rs:817`) adds the sweep's running
  `offset` — advanced by `filled` at the end of every window
  (`compare.rs:835`) — to the within-window `done`. Two members that part in
  the same run get the same key wherever that run sits; two that part in
  different runs get different keys even when those runs are in different
  windows. **This is the answer to "is run indexing continuous across
  windows": yes, because it is never window-relative in the first place.** The
  pivot's buffer is rebuilt per window but the key is not derived from the
  buffer.

- **Same-run leavers are re-read from the start of the run they parted in, not
  after it and not from zero.** `residue()` (`compare.rs:593-602`) returns each
  bucket paired with its own `at`; the class is pushed back onto `pending` with
  that offset and every member is seeked back to it
  (`compare.rs:718-733`). So the parting byte is compared again, by the members
  that disagreed over it, against each other.

**Termination.** Each `pending` class of size `n` yields children that exclude
its pivot, so every child has size `≤ n-1`; a class of one is dropped as
settled (`compare.rs:734-736`), and `residue()` filters buckets of one
(`compare.rs:598`). Class size strictly decreases and offsets never go
backwards, so the recursion is finite. A dropped singleton is safe for the same
reason the pairwise argument is: it agreed with the pivot up to its own parting
and disagreed inside it, so it differs from every other member of that sweep.

**A pivot lost mid-sweep** settles nothing. `abandoned()` (`compare.rs:610-617`)
keeps the live class together *at `matched_to`* rather than settling it,
because half the sweep was never measured. Correct — and it is the case no real
file can stage, which is why it is unit-tested through three method calls.

### What I did to try to break it

Eight probes of my own, written against the sweep's **real** geometry, appended
to a scratch copy of the tree (never to the checkout) and run at `--release`:

```
running 8 tests
test compare::tests::rv_the_real_window_geometry ... ok
test compare::tests::rv_different_run_leavers_cost_nothing_more ... ok
test compare::tests::rv_everyone_parts_in_window_two ... ok
test compare::tests::rv_partings_across_a_window_boundary_are_not_confused ... ok
test compare::tests::rv_three_way_split_in_one_run_plus_a_later_one ... ok
test compare::tests::rv_parting_in_the_very_last_run ... ok
test compare::tests::rv_parting_at_the_true_window_boundaries ... ok
test compare::tests::rv_property_all_three_comparisons_agree ... ok

test result: ok. 8 passed; 0 failed; ... 224 filtered out
```

The ones that carry weight:

- `rv_the_real_window_geometry` **measures** the window ramp instead of reading
  it off a comment: two identical files of `40C` cost `2 x 4` seeks, so the
  windows are `1C, 4C, 16C, 64C` — growth four, four windows. This is what
  exposes binding item 3.
- `rv_parting_at_the_true_window_boundaries` walks the parting across `C±1`,
  `5C±1`, `21C±1` and the last byte of the file — the **real** boundaries under
  growth four — and requires the sweep to agree with lockstep at every one.
- `rv_partings_across_a_window_boundary_are_not_confused` puts one pair parting
  at `5C-1` and two more pairs parting at `5C`, i.e. leavers on opposite sides
  of a window boundary plus two distinct classes inside one run on the far
  side. All three classes come back correct; nothing is merged across the
  boundary and nothing is lost.
- `rv_different_run_leavers_cost_nothing_more` states the inference as an
  assertion about bytes: with leavers in runs 1 and 3 of a `40C` file, the
  sweep reads exactly `5C + 2C + 4C`. Each leaver stops at the end of its own
  run and is never read again, and the pivot stops the moment its class is
  empty rather than reading to EOF.
- `rv_property_all_three_comparisons_agree` — 250 random shapes (widths 2-13,
  sizes 64-70,000 B, chunks 32-9,000 B, up to six content classes), each
  requiring `split_pivot_windows`, `split_identical` and
  `split_identical_capped` to reach the same partition. All 250 agreed.

Two of my probes failed on their first run. **Both were my errors, not the
code's**, and I record them because one is instructive: I asserted the pivot
reads its whole file, and it does not — it stops the window loop as soon as
`sweep.live` is empty. The sweep is cheaper than I assumed, not more expensive.
The other was a fill-byte collision in my own fixture (`b'Z'` is the filler).

### Boundaries I checked and found already handled

- **A member shorter than one run or window** cannot arise from a size
  mismatch, but a file whose head stage 3 read entirely never reaches the sweep
  at all: `prepare` answers it from the retained bytes (`compare.rs:225-253`).
  That is also what makes **zero-length files** safe — they are settled before
  any sweep. Confirmed end to end on a tree of empty and one-byte files.
- **A short read is genuine EOF, never a partial read.** `Effort::read_block`
  (`effort.rs:79-100`) loops until `want` bytes or a zero-length `read`. Had it
  not, two equal files could have been declared apart by a truncated buffer.
  This is the single quietest dependency in the whole design and it holds.
- **`at_tail`** is unreachable in production under `spinning` (no tail probe,
  so `tail == 0` and `limit == None`); it is defensive, correct, and unit
  tested. Also: a regrouped class can never arrive with `from >= limit`,
  because a parting offset is always strictly inside the readable span.
- **Bucket ordering is deterministic.** `Ordered` keeps first-seen order
  (`ordered.rs:54-56`), so the residue classes come back in the order their
  first member parted, which is `members` order.

---

## Evidence — what I ran myself

Clean release build of the tip in my own checkout, and of `main@cc654c4` beside
it. Test counts re-derived by building, not read from the claim: **194 on
`main@cc654c4`, 224 at the tip** — the claim's numbers, confirmed. Base clippy
is red at `compare.rs:898` exactly as the claim reports and the tip is green,
`cargo fmt --check` clean on both. The freshness proof re-done at the tip:
`shasum` → `touch src/compare.rs src/consts.rs` → rebuild (real 12 s of work,
not a cache replay) → `shasum`, byte-identical. The builder's worktree has zero
uncommitted changes, so nothing measured is missing from the commit.

### The gate, with no profile flag, and the fence either side

```
$ ls /Volumes   (before)          $ hdiutil info | grep -c dedupconf  ->  0
\]
dedupbench

$ python3 conformance/run.py --gate all \
    --candidate '<my frozen tip binary> {root} --report-json {json} {flags} {stage}'
...
33 check(s), 0 failed
gate all: green
GATE_EXIT=0

$ ls /Volumes   (after)           $ hdiutil info | grep -c dedupconf  ->  0
\]
dedupbench
```

33/33, exit 0, run from my review checkout with a binary frozen outside the
tree so no concurrent rebuild could swap it under me. Nothing in the harness
passes `--profile`, so all 33 ran the flash pipeline. The two mounted volumes
are unchanged and the `m4-trashes` image detached itself.

### My own adversarial differential

I did not reuse `spinning_differential.py` or `pivot_seeks.py`. I wrote
`m7rev_differential.py` (in scratch) with **fifteen** trees built against the
sweep's real constants — `SLOW_CHUNK` = 16 MiB windows growing by four, so the
boundaries are at `+16 MiB` and `+80 MiB` past the 64 KiB retained head, with
`PIVOT_RUN` = 128 KiB runs inside them — and compared **six** ways on each
tree: oracle (`conformance/oracle_driver.py`), flagless `-j1`, flagless `-j8`,
`--profile flash`, `--profile auto`, `--profile spinning -j1`, `--profile
spinning -j8`. Groups were compared as **ordered lists of ordered members**, so
`M7-PIVOT.md`'s split-order law is checked and not merely membership; the
`unreadable` array was compared against the oracle's on every tree and checked
for duplicates.

Every run passes `--no-cache`. My first pass did not, and it silently
corrupted the comparison — the scan cache is per-**volume**, not per-tree, so
the earlier binary's digests made the later one read zero bytes on four trees.
Every number below is from the cold re-run.

```
tree                       groups  verdict
zerolen                         2  OK   (empty files, and one-byte classes)
nothing-matches                 0  OK   (12 same-size strangers)
three-way                       3  OK   (three classes parting in one run)
same-run-different-bytes        0  OK   (leavers 900 B apart inside one run)
same-run-equal-pair             1  OK   (leavers equal to each other)
last-run                        3  OK   (parting inside the final run)
run-boundaries                  4  OK   (either side of run boundaries)
cohort-boundary                 4  OK   (16 x 2 MiB, 4 classes, last KiB)
tails                           2  OK   (parting only in the last 2 KiB)
wide-equal                      1  OK   (40 identical)
hardlinks                       1  OK   (a hardlink cluster in a group)
unreadable                      1  OK   (chmod 000 inside a group)
everyone-in-window-2            3  OK   (every member parts in window 2)
window-boundaries               4  OK   (8 x 88 MiB parting at +16 MiB - 1,
                                         at +16 MiB, and at +80 MiB)
mixed                           4  OK   (several sizes, groups and strangers)

FAILURES: 0
```

Fifteen trees x six ways = 90 runs, every one byte-identical to the oracle in
groups, member order, group order and `unreadable`. No duplicate `unreadable`
entries anywhere.

`window-boundaries` is the tree that matters: one class parts at the last byte
of window 1, one at the first byte of window 2, one at the first byte of
window 3, at the real 16 MiB/80 MiB geometry. All four classes come back exact.

### Deletion, under both profiles

An 11-member group (wider than `SPIN_INTERLEAVE`), a pair, and a stranger.
Cold scan to populate the cache, then `--auto shortest -y --hard` **warm**, so
the attested re-proof actually runs:

```
--- flash    (warm cache, --auto shortest --hard) exit=0 ---
      11 files deleted · 33554432 bytes reclaimed
      survivors: ['p0.bin', 'unique.bin', 'w00.bin']
--- spinning (warm cache, --auto shortest --hard) exit=0 ---
      11 files deleted · 33554432 bytes reclaimed
      survivors: ['p0.bin', 'unique.bin', 'w00.bin']
```

One survivor per group, identical under both profiles.

### Flash inertness — the fact, separated from the timing

The contention-immune half, on my own fifteen trees, tip against
`main@cc654c4`, **no profile flag**, at `-j1` and `-j8`:

```
trees where any of bytes_read, opens, files_seen, bytes_seen, groups,
unreadable differed:  0     (30 paired runs)
keys only in tip report: none
```

Then the builder's `pivot_flash_inert.py`, reproduced:

```
scenario       files   bytes_read         opens    groups ours/base
needle         10000     24682520          9635        10   1.110x
typical         1600      2812459           430        64   0.995x
samesize         142      3702784           142         4   1.014x
widegroup         70     14501888            84         8   1.018x
photos            13     47846337            11         3   0.994x
bigheader          8     32677888             8         1   0.953x
prefixtrap        32     16973824            32         4   0.969x

geomean over 7 scenarios: 1.007x
noise floor (base against itself, same corpus, same rounds): 0.862x
load average: 4.10 4.58 4.15
```

Every counter cell — `bytes_read`, `opens`, `groups` — is identical to the
committed evidence on all seven scenarios. My geomean is 1.007x against the
claim's 1.000x, and my noise floor is 0.862x against the claim's 0.988x,
because I measured at load average 4.1 where the builder measured at 1.1. Every
per-scenario figure I got (0.953x-1.110x) sits inside the noise floor I
measured. The conclusion holds under worse conditions than the claim was taken
under.

### Determinism

Five identical `--profile spinning -j1` runs on the same tree produced exactly
one distinct `(bytes_read, seeks, opens)` tuple. The spinning counters repeat,
as the claim says.

---

## Rulings — verified as implemented

**(a) `verify_attested` stays on `split_identical_capped`. Confirmed.**
`compare.rs:1284-1286` still routes `spinning` to the capped cohort comparison
on the deletion path. The agreement harness is real and it really runs all
three: `Bench::partitions` (`compare.rs:1519-1554`) calls `split_identical`
under `Flash`, `split_identical_capped` under `Spinning` and
`split_pivot_windows` under `Spinning` on the same tree and returns all three
partitions, and every shape test in the file compares them. I exercised it
myself over 250 randomly generated shapes and all three agreed on every one.
The older splitter is a live second opinion, not dead weight. Accepted as
ruled.

**(b) `PIVOT_GROWTH = 4`. The decision is right; the record is not.**
`pivot_run_size.py` reproduces exactly — but it varies `PIVOT_RUN` only
(`WRITTEN`/`MUTATED` at lines 38-39 swap `MIN_CHUNK` for `SLOW_CHUNK`). **No
committed script varies `PIVOT_GROWTH` at all**, so the ruling's stated
verification does not reach the constant it was about. I supplied the missing
measurement: I built a `PIVOT_GROWTH = 2` variant in scratch and ran it against
the tip, cold, one reader.

```
one group of N identical files, --profile spinning -j1, cold
shape                   growth2 seeks  growth4 seeks  fewer at 4  bytes equal
3 x 256 MiB                        21             15       1.40x  True
2 x 512 MiB                        16             12       1.33x  True
4 x 64 MiB                         20             16       1.25x  True
```

Groups identical, bytes identical, 1.25x-1.40x fewer whole-scan head movements
at four. The stage-5 arithmetic is exactly what `consts.rs` claims: a 256 MiB
file costs three windows at growth four and five at growth two (21 - 15 = 6 =
2 extra windows x 3 members). **Four is justified.** See binding item 2 for
what is wrong with how it is written down.

**(c) `SPIN_INTERLEAVE` is not applied on the new path, and the code says why.
Confirmed.** The comment is at `compare.rs:1163-1169`: "What the sweep does not
carry over from the capped comparison is its width cap — holding a descriptor
costs nothing on a disk that is only ever asked to read from one of them at a
time, and `SPIN_INTERLEAVE` existed to bound the round-robin this replaces."
Descriptor *use* really is one-at-a-time: `split_pivot_windows` calls `prepare`
with `pool: None` (`compare.rs:688`, under the comment "No pool: one disk, one
queue"), the window loop is single-threaded, and every read takes and drops one
mutex before the next. Descriptor *holding* is now group-wide rather than
capped at eight — see non-binding note 4.

---

## Binding items

### 1. The bytes result is stated as a universal and is not one

`MILESTONES.md:2722-2724` says, with no hedge anywhere in the claim:

> **Bytes are equal or lower on every shape** — 0.70x to 1.00x of what `main`
> reads — which is more than M7-PIVOT.md asked for; it asked for equal.

It is true of the seven shapes in `pivot_seeks.py`. I reproduced those and they
hold. It is **not** true generally. On my own corpora, cold, `--profile
spinning -j1`, tip against `main@cc654c4`:

```
shape                        main bytes    our bytes  ours/main
three-way                      18874368     19660800      1.04x
same-run-equal-pair             9437184      9699328      1.03x
last-run                       18874368     19267584      1.02x
run-boundaries                 50331648     51118080      1.02x
tails                          26214400     26411008      1.01x
everyone-in-window-2          176160768    177209344      1.01x
...
cohort-boundary                49807360     35127296      0.71x
wide-equal                     46858240     41943040      0.90x
```

Six of fifteen shapes read **more** than `main`, by 1% to 4%. The mechanism is
the design working as intended and is fully explained by it: a member that
parts is read to the end of its 128 KiB run, and that run is read once more
when its class is regrouped. On `three-way` the excess is 786,432 B = exactly
six runs. Two shapes also issue **more** seeks than `main`
(`same-run-different-bytes` 9 → 11, `everyone-in-window-2` 34 → 36), and an
all-distinct group of 8 costs 24 → 51.

None of this violates a spec. `SPIN.md` says in terms that the bytes ceiling
does not apply to `spinning` and that the profile "trades bytes for seeks by
design". The defect is that the claim asserts the opposite of a spec allowance
as a general result, and takes credit against `M7-PIVOT.md`'s "bytes equal"
verification line for exceeding it everywhere. A reader of the permanent record
would conclude the profile can never cost bytes. It can.

This is the same defect `REVIEW-M6.md` raised as non-binding note 1 — a
corpus-specific ratio written as universal. It recurred in a stronger form, so
it is binding this time. The seeks ratio in the same claim **is** hedged
("Read '1.0x to 5.8x' as a statement about large files, not about every tree",
`MILESTONES.md:2742`); the bytes sentence needs the same treatment.

*Required:* scope the sentence to the corpus it was measured on, and record
that shapes exist where the sweep reads a few per cent more and issues more
seeks, with the reason. One or two sentences. Do not re-tune a constant for
this; the trade is correct.

*I will verify by:* re-reading `MILESTONES.md`'s bytes paragraph for a scope
qualifier and a stated counter-direction, then re-running my
`m7rev_differential.py` and confirming that a shape it reports above 1.00x is
consistent with what the amended text now says. The claim must not assert
"every shape" for a range measured on seven.

### 2. `PIVOT_GROWTH`'s doc comment carries a number the branch's own evidence contradicts

Two statements of the same measurement disagree, and neither is reproducible
from anything committed:

- `consts.rs:87` — "worth 1.5-2.7x fewer seeks at two and **2.5-4.8x** at four"
- `LOG.md:2412` — "1.5-2.7x fewer seeks at two against **1.0-5.8x** at four"

`LOG.md`'s figure is the branch's own headline vs-`main` range from
`m7-seeks.txt` (1.0x-5.8x). `consts.rs`'s 2.5-4.8x matches nothing in the
branch — not the seeks table, not `LOG.md`, and not my own measurement.
`REWRITE.md` §8 treats these comments as the record of settled measurements, so
a wrong one is not cosmetic: it is what the next person tuning this constant
will read.

*Required:* make the two agree, and make the growth claim reproducible — either
commit the growth-2 comparison as a script beside `pivot_run_size.py`, or state
the ramp arithmetic (three windows against five for a 256 MiB file) as the
argument it is and stop calling it a measured seek ratio. My numbers in ruling
(b) are available if useful.

*I will verify by:* `grep -n "at two\|at four" rust/src/consts.rs rust/LOG.md`
and checking the two agree; and, if a script is added, running it and checking
it reports a growth-2 build against the tip on a corpus with files large enough
for the ramps to differ (≥ 256 MiB), with bytes and groups asserted identical.

### 3. The window-boundary test no longer tests the window boundaries

`compare.rs:1780-1783`:

```rust
/// The chunk every shape below is built around. A window starts there and
/// doubles, so with a group read from offset zero the windows are
/// `[0, C)`, `[C, 3C)`, `[3C, 7C)`, `[7C, 15C)`, and the runs inside them
/// are every C bytes.
```

Windows do not double. `PIVOT_GROWTH` is 4, so the boundaries are `C`, `5C`,
`21C`. I measured this rather than inferring it — `rv_the_real_window_geometry`
above. `compare.rs:2036` has the same stale phrase ("and a window doubles"),
though the arithmetic two lines below it at 2052-2056 is correctly growth-four.

The consequence is not cosmetic.
`a_divergence_on_a_window_boundary_splits_the_group`
(`compare.rs:1854-1879`) — whose stated job is "either side of every boundary a
doubling window has in this file" — walks `C±1`, `3C±1`, `7C±1`, `15C±1` over a
file of `SPAN = 16C`. At growth four that file has exactly two window
boundaries, `C` and `5C`. So the test covers the first one and then spends
three quarters of its offsets on `3C`, `7C` and `15C`, which are ordinary run
boundaries, while **`5C` is never tested either side.**
`the_sweep_finds_the_same_classes_wherever_the_files_part` does hit `5C`
exactly, via its `run * C` chain, but its `step_by(311)` walk does not land on
`5C±1` — 311 x 65 = 20,215 and 311 x 66 = 20,526 straddle 20,480 — so a fault
confined to the first or last byte of a window would be caught by nothing in
the committed suite.

There is no such fault: my `rv_parting_at_the_true_window_boundaries` and
`rv_partings_across_a_window_boundary_are_not_confused` both pass, and the
`window-boundaries` tree passes end to end at the real 16 MiB geometry. This is
a guard that has silently stopped guarding, on the surface where a false merge
deletes user data — which is exactly the kind of thing that should not be left
for the next reader to notice.

*Required:* fix both comments to describe growth four, and extend the boundary
test to the real boundaries — `5C±1` in the file it already uses, and, if the
file is lengthened past `21C` (mine is `40C`), `21C±1` as well. Keeping the run
boundaries it already walks is fine; they are worth testing, just not under
that name.

*I will verify by:* reading the two comments; then, in a scratch copy, setting
`PIVOT_GROWTH` back to 2 and confirming the amended boundary test **fails** —
a boundary test that passes at both growth factors is not testing boundaries.
Then restoring 4 and confirming it passes, and re-running my eight probes.

---

## Non-binding notes

1. **"Nothing outside `compare.rs` and `consts.rs` changed" overstates
   slightly.** True for source and for every harness directory —
   `git diff cc654c4..479ccb0 -- rust/src conformance rbench bench xbench tools`
   shows exactly those two files. The whole-repo diff is 21 files; the other 19
   are pure additions (`LOG.md`, `MILESTONES.md`, `evidence/*.txt`,
   `evidence/scripts/*`). Substance holds; the wording could say "no source or
   harness file outside these two".

2. **There is no claim-versus-evidence audit script, and the claim's process
   note implies there is.** I checked every fenced block in the M7 claim against
   its named `evidence/` file: **all ten are byte-for-byte faithful, whole-file
   quotes, with zero omissions and zero altered numbers.** The claim is honest.
   But nothing in the branch mechanically enforces that — no committed script
   reads `MILESTONES.md`. A general
   `evidence/scripts/claim_audit.py` (extract the fences from an M-section, diff
   against `evidence/`) would make this discipline permanent rather than
   per-builder, and would have caught M5.2's unbacked lines. Worth doing once,
   for every future milestone.

3. **`m7_gate.sh` prints both `/Volumes` listings but does not assert they are
   equal.** The "unchanged" conclusion is prose over two printed blocks. A
   `diff` between them and a non-zero exit would make it a gate. I verified the
   listings match, before and after, in my own run.

4. **Descriptor holding under `spinning` went from capped to group-wide.** M6's
   capped sweep held at most `SPIN_INTERLEAVE` = 8 descriptors per group; the
   pivot sweep opens the whole group in `prepare` and keeps it. This is
   deliberate, disclosed, and matches what the flash path has always done, and
   on this machine it is nowhere near a limit (`ulimit -n` is 1,048,576).
   On a host with a small `ulimit -n`, a very wide group under `spinning` would
   now fail opens where M6 would not. The failure mode is safe and visible —
   the file lands in `unreadable` and is reported, never silently merged — so
   this is a note, not a defect. Worth one line in the claim.

5. **The `at_tail` path in `split_pivot_windows` is unreachable in production.**
   Under `spinning` there is no tail probe, so `tail == 0` and `limit == None`.
   The code is correct and unit-tested and I would keep it, but a reader should
   know it is defensive rather than live.

---

## Defects confirmed as pre-existing, for the backlog

**The wide all-distinct same-size group is super-linear on both spinning
comparisons.** Reported by the builder and not fixed; I reproduced it against
`main@cc654c4` to confirm it is not this branch's. Files identical except their
last 8 bytes, so every one is its own class and each pass strips exactly one,
cold, `--profile spinning -j1`:

```
    N     size    main bytes     tip bytes  main/N*sz  tip/N*sz  main seeks  tip seeks
    8  1048576       8388608      10158080       1.0x      1.2x          24         51
   16  1048576     142606336      24576000       8.5x      1.5x         176        167
   32  1048576     573243392      65994752      17.1x      2.0x         645        591
   64  1048576    2295660544     199163904      34.2x      3.0x        2459       2207

  bytes at N=64 over bytes at N=8, for an 8x rise in file count:
    main@cc654c4    273.7x
    this branch      19.6x
```

It is real, it predates M7, and **this branch improves it by an order of
magnitude** — 2.3 GB down to 199 MB at N=64, and a growth rate of 19.6x against
273.7x. At N=8 the sweep costs slightly more than the capped cohort (1.2x
bytes, 51 seeks against 24), which is the small-N price of one pass per class.
Backlog, not M7's, and the builder's proposed fix (a `PARTITION_FANOUT`
equivalent on the spinning path) is the right shape. It should be filed with a
measurement of how often genuinely-unique same-size wide groups occur.

**The `\]` volume** was present, unchanged, before and after every run I made,
including the gate. Nothing on this branch created or touched it. Noted only.

---

## Fence conditions this review was made under

Detached checkout at `479ccb0` in `/Users/dogaozden/AI_Projects/dedup-m7-review`
and a detached `main@cc654c4` in `/Users/dogaozden/AI_Projects/dedup-m7-base`;
no branch was written to except `review-m7`, which carries this file. The
builder's worktree was read and never written. Nothing under `/Volumes` was
read or written, with the single ruled-in exception of the ephemeral
`dedupconf` image the `m4-trashes` check creates and detaches itself, captured
by `ls /Volumes` and `hdiutil info` either side. `dedupbench` and the Passport
were never touched, mounted, unmounted or read — they appear only in listings.
All scratch corpora, the `PIVOT_GROWTH = 2` variant build and the probe copy of
the tree live under the session scratchpad and were removed after use; no
variant binary was ever produced in a checkout. Free space was 12-14 GiB
throughout, which is why my largest corpus is 768 MiB. No `sudo`.

---

## How to clear M7

Three items, all in the record and one test's offset list. Nothing about the
sweep needs to change: it is correct, it is well tested, the gate is green with
no profile flag, all nineteen mutants are caught, and the deletion path is
untouched and behaves identically under both profiles. Fix items 1-3 and this
is an approval.

I would also take non-binding notes 1 and 2.

---

# Re-review — 2026-08-24, at `cf62e3c`

**Verdict: APPROVED.**

Six commits atop the `479ccb0` I reviewed. All three binding items are
discharged, all five non-binding notes were taken, and two of the fixes are
better than what I asked for. Everything below I ran myself in my own checkout;
where I re-ran a builder script I say so and treat it as a reproduction.

## The headline fact, checked my own way

The claim is that the release binary did not move. I could not confirm that by
matching the builder's SHA — `d614552…f11382` is built at
`/Users/dogaozden/AI_Projects/dedup-m7`, and a release build embeds its path,
which is why my build at `479ccb0` was `bcc295bc…e46b88e1` and never could have
matched. So I checked it two better ways.

**Structurally.** `#[cfg(test)]` sits at `compare.rs:1315`, and every hunk in
the `479ccb0..cf62e3c` diff of that file begins at line 1781 or later:

```
@@ -1781,3 +1781,8 @@ mod tests {      @@ -1856,20 +1915,9 @@ mod tests {
@@ -1786,3 +1791,23 @@ mod tests {     @@ -1880,0 +1929,12 @@ mod tests {
@@ -1843,2 +1868,3 @@ mod tests {     @@ -1883,4 +1943,14 @@ mod tests {
@@ -1849,2 +1875,4 @@ mod tests {     @@ -2036 +2106 @@ mod tests {
@@ -1853,0 +1882,31 @@ mod tests {
```

Every change to `compare.rs` is inside the test module. The `consts.rs` diff is
a doc comment and nothing else. No shipping code was touched.

**Empirically.** I rebuilt at `cf62e3c` in the same checkout, at the same path,
so the comparison means something:

```
my sha at cf62e3c   bcc295bc305b1aa72854f1e0f2aef254e31f0f2d5ebd29954be59e6ee46b88e1
my sha at 479ccb0   bcc295bc305b1aa72854f1e0f2aef254e31f0f2d5ebd29954be59e6ee46b88e1
```

Byte-identical. The builder's own SHA is likewise unchanged across the round
(`m7-standards.txt` carries `d614552…` at both tips, and the diff of that file
shows no change to those lines). Two independent builds from two different
paths, each unmoved. **My correctness derivation from the first review carries
over wholesale** — the sweep, its proof, and every boundary I attacked are the
same code.

It carries over in practice too: my eight probes, re-applied to the fix-round
source, still pass 8/8, and `cargo test` at the tip is **226 passed, 0 failed**,
confirming 224 → 226.

## Binding item 1 — discharged, and the corpus was fixed, not just the prose

`MILESTONES.md:2741` now reads "**On these nine shapes** bytes run from 0.70x
to 1.05x of what `main` reads — so mostly equal or lower, and on two of them
higher. That is a statement about this corpus and not a general result", and a
new subsection states plainly that shapes exist which read more and seek more,
that SPIN.md permits it in terms, and that "nothing here rules out shapes worse
than the ones measured." That is the honest epistemic position, which is more
than I asked for.

The part that matters more than the prose: **the corpus absorbed my
counter-examples.** `pivot_seeks.py` now carries nine shapes. I re-ran it
against my own `main@cc654c4` build and reproduced the committed table exactly,
including both new rows:

```
shape                  groups  flash   main   ours vs main vs flash     opens
three-way-in-one-run        3     90     24     24    1.0x     3.8x     12->6
strangers-of-one-size       0     16     24     51    0.5x     0.3x     16->8

  three-way-in-one-run            36.0          36.0          36.4     1.01x
  strangers-of-one-size            0.1          32.0          33.7     1.05x

  reads more than main: three-way-in-one-run, strangers-of-one-size
  seeks more than main: strangers-of-one-size
```

`strangers-of-one-size` reproduces my own 24 → 51 figure from an independently
written corpus. The summary now prints both directions and names the shapes in
each, and the script's gate no longer requires every shape to improve — a gate
that could only ever be satisfied by a friendly corpus. My verification recipe
asked that the claim not assert "every shape" for a range measured on seven; it
now asserts a range for nine and says so twice.

## Binding item 2 — discharged; my recipe run verbatim

My recipe was `grep -n "at two\|at four" rust/src/consts.rs rust/LOG.md` and
check the two agree. They do: both now carry **1.25x-1.40x fewer head
movements**, both sourced to the new `evidence/scripts/pivot_growth.py`. The
contradictory `2.5-4.8x` survives in exactly one place — `MILESTONES.md:3178`,
inside the paragraph *describing the defect I raised* — and `1.5-2.7x` is gone
from the tree entirely. That is the right disposition: the wrong number is
preserved as history, not as a live claim.

The new script's numbers are the numbers I measured independently before the
builder wrote them, from my own `PIVOT_GROWTH = 2` build:

```
shape               growth2  growth4 fewer at 4  bytes equal
3 x 256 MiB              21       15      1.40x         True
2 x 512 MiB              16       12      1.33x         True
4 x 64 MiB               20       16      1.25x         True
```

Identical in every cell to my own first-round table. Four is the better factor
and it is now a measurement rather than an assertion.

## Binding item 3 — discharged, in a stronger form than I specified

I asked for the stale comments to be fixed and the offset list extended to the
real boundaries. The builder did something better: the geometry is now
**derived and pinned** rather than written down. `window_edges()`
(`compare.rs:1798-1812`) computes the boundaries from `PIVOT_GROWTH` and
`PIVOT_WINDOW`, `SPAN` is raised to `40C` so three real boundaries exist, the
boundary test walks `edge-1, edge, edge+1` over the derived edges, and the old
run-boundary walk survives under the honest name
`a_divergence_on_a_run_boundary_splits_the_group`.

The pin is `the_windows_are_where_the_growth_factor_puts_them`, which
*measures* the window count via seeks at three file sizes, holds it against
`window_edges`, and then asserts `PIVOT_GROWTH == 4` and
`window_edges(0, SPAN) == [C, 5C, 21C]`.

My recipe said: set `PIVOT_GROWTH` back to 2 and confirm the amended boundary
test **fails**, because a boundary test that passes at both growth factors is
not testing boundaries. I ran that myself on a scratch copy of the fix-round
source:

```
test compare::tests::the_windows_are_where_the_growth_factor_puts_them ... FAILED
  panicked at src/compare.rs:1909: assertion `left == right` failed:
  the offsets below belong to this factor
test compare::tests::the_sweep_opens_each_member_once_and_reads_it_in_one_run_per_window ... FAILED
test result: FAILED. 224 passed; 2 failed
```

Note what does **not** fail: the boundary test itself passes at growth 2 —
correctly, because `window_edges` retargets it onto the growth-2 boundaries, so
it is still testing real boundaries. The silent retargeting I was worried about
is now impossible because the pin fails loudly instead. That satisfies the
intent of my recipe better than the letter of it, and I accept the stronger
form. `pivot_growth.py` carries this same check as a required failure, so it is
in the suite and not just in this review.

## The other checks

- **Gate.** `m7_gate.sh` now diffs its two `/Volumes` listings and exits
  non-zero on a difference, and gates on the leftover-image count too — note 3
  taken properly. My run: 33/33, `gate all: green`, `exit 0`, "gate green,
  mount table unchanged, no image left attached". `/Volumes` held `\]` and
  `dedupbench` before and after, unchanged.
- **Re-captured batteries.** The differential moved only in the `flash seeks`
  column (696→695, 16→15) — the one column documented as scheduler-dependent;
  every byte column and every group identical. `m7-flash-inert.txt`'s counters
  are identical to the previous capture; only wall-clock moved, geomean 0.999x
  against a 1.015x noise floor at load 6.1. That is consistent with my own
  reproduction last round (1.007x against a 0.862x floor at load 4.1).
- **`claim_audit.py`.** M7 audits clean: 11 blocks, 0 untraced.

## Non-binding notes from this round

1. **`pivot_growth.py` takes a main-binary argument it never uses.** It is
   bound at line 108, existence-checked at 109-110, and never referenced again;
   the comparison is tip-at-growth-2 against tip-at-growth-4. **The experiment
   is the right one** — `PIVOT_GROWTH` exists only at the tip, `main`'s spinning
   path is the capped cohort sweep with no growth factor at all, so a
   main-relative growth measurement is a category error. My own binding item 2
   recipe asked for "a growth-2 build **against the tip**", which is exactly
   what the script does, and it is what I did myself. So this is cosmetic and
   **not** a re-open. It is worth fixing anyway because it already misled one
   careful reader into expecting a baseline comparison: drop the parameter, or
   use it only to print a context line.

2. **`claim_audit.py` is worth keeping but it cries wolf, and I would fix that
   before anyone relies on it.** All four blocks its `--all` pass reports are
   benign, and I checked each myself:

   - Three of the four *do* have their content committed —
     "confirmed deletion moves exactly…" is in `m5-end-to-end.txt`,
     "ctrl-c at the keyboard, by screen" is in `m5_2-signals.txt`, and
     "18 check(s) differing / main@fbc196d" is in
     `m5_2-m5-suite-restored.txt`. The runs happened and their output is in the
     repository; the claim quotes an excerpt or an elided version rather than a
     contiguous block, so the matcher misses it. Reporting that as
     "NOTHING IN evidence/ CONTAINS THIS" is wrong and alarming.
   - The fourth, `cargo test … 101 passed`, has no evidence anywhere — but it
     sits at `MILESTONES.md:662`, inside
     "## M5 — the interactive TUI — **CLAIMED**", the *superseded* original
     claim. The live "RE-CLAIMED after REVIEW-M5 CHANGES" section quotes
     `105 passed` at line 997, and that one traces cleanly to
     `m5-standards.txt`. Four tests were added during the M5 fix round, the
     evidence was re-captured, and the archived original kept its own
     contemporaneous number — which is what an archive should do.

   **Nothing here smells worse than staleness. No fabricated measurement.** My
   recommendation is backlog, with the fix being to the tool rather than to the
   old claims: report an excerpt of a known file as "excerpt of X" instead of
   "nothing contains this", and either skip superseded sections or bucket them
   separately. A claim-auditor whose only four findings are false alarms will be
   ignored the first time it finds something real.

## Disposition

M7 is approved at `cf62e3c`. The shipping binary is byte-identical to the one I
proved correct, the record now says what was measured and on what, the growth
constant is a measurement with a script behind it, and the window-boundary
guard cannot silently retarget again. The two backlog items stand where they
were: the super-linear wide all-distinct group on both spinning comparisons —
which this branch improves by an order of magnitude and does not introduce —
and a user-supervised wall-clock run on real spinning media, which is still
owed and which nothing here claims.
