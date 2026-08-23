# REVIEW-M6.md — maintainer review of the M6 claim

**Verdict: APPROVED**

Claimed at `b102625` on branch `features`, reviewed at that tip (7 commits atop
`main@baf9e25`). Binding specs are `SPIN.md` and REWRITE.md §0/§3. I read
`LOG.md` and `evidence/` first and then verified everything independently; no
number below is taken from the builder's scripts.

M6's own code — `compare.rs`, `cache.rs`, `device.rs`, `effort.rs`,
`stages.rs`, `pipeline.rs` — is correct as far as I could break it, and I tried
hard. The capped comparison is exact, the cache column fix is real and live,
and the default path is byte-identical to `main`.

> **This branch must not merge yet.** `REVIEW-M5_1.md` carries one binding item
> against the M5.1 menu that ships on this same branch: the pre-flight menu
> spins on a core when its terminal dies. That is an M5.1 defect, not an M6
> one, but it is on the tip that would merge. See the trailing-review section
> at the end of this file, which is where I found it.

---

## The fence breach — RULING: recorded and closed

The builder disclosed that it ran the binary read-only against `/dev` and
`/System/Volumes/Data` before getting the coverage properly. I checked all
three things that matter.

**(a) It is recorded honestly.** `rust/LOG.md`, section "The one thing that
could not be observed, and what was done instead":

```
Looking for a real volume that reports unknown was the wrong way to get this,
and I started down it before stopping: a scan of `/dev` and
`/System/Volumes/Data` is outside the fence REWRITE.md §0 draws around this
repo, its scratch corpora and the oracle clone. It was read-only and it was
killed by its own timeout, and it should not have been started.
```

That names the paths, names the rule it broke, and does not excuse it.

**(b) No artifact survives.** Over the whole diff `baf9e25..b102625`:

```
$ git diff baf9e25..b102625 | grep -cE '/System/Volumes|/dev/disk|/dev/rdisk'
1
```

The single hit is the disclosure quoted above. Zero `/dev/disk`, zero
`/dev/rdisk`. Grepping `rust/evidence/` for `/System/Volumes`, `/dev/disk`,
`/dev/rdisk` and home-directory paths returns nothing in any file this branch
added — every `/Users/dogaozden` hit is in a pre-existing evidence file from an
earlier milestone and is a worktree path, not a scanned path. No report JSON,
no evidence file and no test fixture carries a real system or user path.

**(c) The proper fix covers what the breach was hunting.** It does. The breach
was after "does `auto` on rotational media actually RUN the spinning pipeline",
and `VolumeProbe::answered` (test-only, `device.rs`) answers it on real scans
rather than in isolation. `pipeline.rs` gains four tests that build a scratch
tree, run the real `find_duplicates` against each of the three device answers,
and assert on the `Scan` itself — not on `Wanted::resolve` returning the right
word:

```rust
fn auto_runs_the_spinning_pipeline_on_a_disk_that_says_it_spins() {
    let tree = Tree::new("rotational");
    let scan = tree.scan(Some(false), Wanted::Auto);
    assert_eq!(scan.profile, Profile::Spinning);
    assert_eq!(scan.hint, None);
    assert_eq!(scan.reported().len(), 1);
    assert_eq!(scan.reported()[0].members.len(), 2);
}
```

That is the stronger claim, and it is the one the breach could not have
produced anyway. Closed.

---

## Evidence — what I ran myself

Clean release build at the tip, then standards from `rust/`:

```
$ cargo test
test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.16s

$ cargo fmt --check
FMT_EXIT=0
```

The gate, with **no profile flag**, reproduced from the repository root:

```
$ python3 conformance/run.py --gate all \
    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
...
M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

33 check(s), 0 failed
gate all: green
[exited with code 0]
```

33/33, exit 0. Nothing in the harness passes `--profile`, so this is the proof
the default changed nothing.

### My own adversarial differential

I did not reuse `spinning_differential.py`. I wrote
`featrev-differential.py` (in scratch) with fourteen trees aimed at the
cohort-partitioning boundaries, and compared **seven** ways on each tree:
oracle (via `conformance/oracle_driver.py`), flagless `-j1`, flagless `-j8`,
`--profile flash`, `--profile spinning` `-j1`, `--profile spinning` `-j8`, and
`--profile auto`.

```
tree                         groups verdict
exact-at-cap                 1      OK        (a group of exactly K=8)
one-over-cap                 1      OK        (exactly K+1)
pair-straddles-boundary      1      OK        (only f00 and f08 equal — the
                                               partner sits past the first
                                               cohort boundary)
pairs-never-meet-the-pivot   2      OK        (17 files; two classes that never
                                               share a cohort with their own
                                               partner during the first sweep)
last-chunk-differs           2      OK        (21 MiB files parting at the very
                                               last byte, past chunk one)
tail-only-difference         4      OK        (four classes parting only inside
                                               the last 4 KiB — exactly where
                                               the head-only probe stops
                                               separating)
sample-span-crossers         1      OK        (> SAMPLE_SPAN: flash samples,
                                               spinning does not)
wide-plus-singletons         1      OK        (12 equal among 18 same-size
                                               strangers, 30 wide)
hardlinks-in-a-wide-group    1      OK
unreadable-in-a-wide-group   1      OK
many-classes-wide            8      OK        (24 files, 8 classes of 3)
two-classes-alternating      2      OK        (20 files, two classes of 10)
deep-head-boundary           8      OK        (sizes either side of DEEP_HEAD)
multi-round-wide             2      OK        (12 copies spanning three
                                               SLOW_CHUNKs, diverging inside
                                               the second)

FAILURES: 0
```

Groups were compared as ordered lists of lists, so member order had to agree
too, not just membership. The `unreadable` array was compared against the
oracle's on every tree and checked for duplicate entries — none anywhere.

### Deletion and the attested re-proof, under both profiles

`verify_attested` routes through `split_identical_capped` under `spinning`, so
the capped sweep is on the deletion path, not only the reporting path. On a
tree with an 11-member group (wider than the cap) plus a two-member group, cold
scan to populate the cache, then `--auto shortest -y --hard` with a **warm**
cache so the attested re-proof runs:

```
--- flash (warm cache, --auto shortest --hard) exit=0 ---
      11 files deleted · 3328000 bytes reclaimed
--- spinning (warm cache, --auto shortest --hard) exit=0 ---
      11 files deleted · 3328000 bytes reclaimed

SURVIVORS IDENTICAL across profiles:
    ./a.bin
    ./unique.bin
    ./w00.bin
```

One survivor per group, identical under both profiles.

### The cache column fix, and the 3 → 4 migration

I tested the migration with two real binaries, not by reading. A build of
`main@baf9e25` in scratch wrote a schema-3 cache; the features build then ran
against it:

```
=== 1. cold scan with MAIN (writes a schema-3 cache) ===
user_version after main: 3
columns after main:      ino size mtime_ns ends deep sample full cached_at last_seen
rows after main:         4

=== 2. warm scan with FEATURES against that schema-3 DB ===
user_version after features: 4
columns after features:      ino size mtime_ns ends deep sample full head cached_at last_seen
groups reported:             [['a.bin', 'b.bin', 'e.bin']]

=== 3. and back again: MAIN against the now-schema-4 DB ===
user_version after main again: 3
groups reported:               [['a.bin', 'b.bin', 'e.bin']]
```

Dropped and rebuilt cleanly in both directions, correct groups each time. The
cost is one cold scan per binary switch, which is the right trade.

The nastier case — a schema-3-shaped table **lying** about its version, which
is the "misread old rows as new columns" hazard — is caught and reported
rather than misread. Planting a 9-column table stamped `user_version = 4` under
the real database name:

```
dedup-rs: the scan cache could not be read (no such column: head in SELECT ino,
size, mtime_ns, ends, deep, sample, full, head, cached_at FROM files at offset
54); reading everything and rebuilding it
  groups: [['a.bin', 'b.bin', 'e.bin']]
  columns now: ino size mtime_ns ends deep sample full head cached_at last_seen
```

Structurally this is safe by construction as well: `ScanCache::load`
(`cache.rs:148-175`) only sets `volume.database = Some(..)` when `read_rows`
succeeded, so `write_rows` can never reach a table `read_rows` has not already
validated or replaced.

And the columns really are disjoint and really do persist — this is the fix
being live rather than merely declared:

```
--- after --profile flash ---      ino,ends,head,deep,full: ...|1|0|1|1
--- after --profile spinning ---   ino,ends,head,deep,full: ...|1|1|1|1
--- after --profile flash ---      ino,ends,head,deep,full: ...|1|1|1|1
                                   groups identical throughout, bytes 0 on the
                                   third run (attested)
```

A spinning-only scan writes `head` and never `ends`; a flash-only scan writes
`ends` and never `head`; a switch adds the other column without disturbing the
first.

### The seek counter does not perturb what is scored

Against the `main@baf9e25` build on the same tree, flagless, at `-j1` and
`-j8`:

```
-j 1
   groups          main 6, features 6  -> IDENTICAL
   bytes_read      main 72433200       features 72433200       same
   opens           main 526            features 526            same
   files_seen      main 440            features 440            same
   bytes_seen      main 69287472       features 69287472       same
   keys only in features report: ['seeks']
-j 8
   (identical in every field; same extra key)

MISMATCHES: 0
```

Flagless stdout is byte-identical to `main`'s (`cmp -s` clean) and stderr is
empty on both — no hint leaks into a normal run.

### Mutants

`spinning_mutants.py` re-run: **7 mutants, 7 caught, 0 survived**, exit 0
(the script carries 6 in `MUTANTS` plus `one-cache-column` in `WHOLE_PROGRAM`).
The one that matters most:

```
mutant one-cache-column           files the head-only probe and the two-ends probe in the same column
  caught: warm flash then spinning reported [('a.bin', 'b.bin')], oracle [('a.bin', 'b.bin', 'e.bin')]
```

`settle-on-a-lost-pivot` is caught only by
`a_pivot_that_goes_away_mid_sweep_settles_nothing_and_goes_back_in`, which is
the test the `Round` extraction was done to make possible. I read it and drove
the logic by hand; it asserts the right thing — nothing settles, and the
pivot's proved members go back to the pool led by a survivor:

```rust
round.fold(&[vec![2], vec![3]], &[vec![1, 2], vec![3]], &[]);
round.fold(&[vec![4], vec![5]], &[vec![4, 5]], &[1]);
...
assert!(matched.is_empty());
assert_eq!(pool, vec![vec![2], vec![3], vec![4, 5]]);
```

---

## Why the capped comparison is exact — my own reading

I did not take the doc comment's word for it. The invariant that carries it:
after a round, `merged` is a **complete** equivalence class, because every
leftover was compared against the pivot on real bytes this scan and lost, and
byte equality is transitive. Settling it and dropping it from the pool
therefore cannot lose a pair. Each round removes exactly one cluster, so every
pair eventually shares a cohort with a pivot that speaks for it.

The two places this could have gone wrong, and why they do not:

- `compare_cohort` reconstructs the **whole** partition, not just the runs
  worth reporting. `split_identical` drops singletons (`members.len() < 2 =>
  continue`), so a member that matched nobody would have vanished; the
  read-but-unmatched members are put back as `vec![index]` and only the members
  that appear in `reported` are treated as lost. My `pair-straddles-boundary`
  and `pairs-never-meet-the-pivot` trees exist to catch exactly that mistake.
- The lost-pivot branch refuses to settle, because half the sweep never
  measured anything against the pivot. `close` retains `merged` without the
  pivot and pushes it back at the front of the pool. I checked the failure
  mode it avoids: settling there would strand `merged`'s members while a file
  equal to them sits in the pool they have just left.

I also went looking for duplicate `unreadable` entries — a pivot that fails in
every cohort of its round would be reported once per cohort. It does not arise:
a file that cannot be opened is dropped at the ends/head probe, long before it
can be a compare-stage pivot. Verified on a 15-member group whose lowest-inode
member (the pivot candidate) was `chmod 000`:

```
--- --profile flash ---     unreadable: ['aaa-denied.bin']  has duplicates: False
--- --profile spinning ---  unreadable: ['aaa-denied.bin']  has duplicates: False
                            group sizes: [14]   (oracle: 14)
```

---

## SPIN.md's honesty law

Satisfied, plainly. `MILESTONES.md`:

> **HDD wall-clock unvalidated.** No rotational drive is available to an agent
> — the only one on this machine is fenced — so no speed claim is made here.
> What is claimed is correctness, which is proved against the oracle, and the
> access pattern, which is counted. A user-supervised run on a real spinning
> disk is still owed.

`evidence/m6-seeks.txt` repeats it in the last line of its own output. The
claim's quoted gate block is byte-identical to `evidence/m6-gate.txt` (I
diffed every quoted line against the file: none missing).

---

## Non-binding notes

None of these blocks the milestone. I would take 1 and 2.

1. **The headline seek ratio is corpus-specific, and the claim could say so
   once.** "between 3.0x and 3.6x fewer" is measured on the wide big-file
   corpus SPIN.md asked for, and it is true there. On small-file trees
   `spinning` issues *more* seeks than `flash`, because the re-reads cost more
   than the skipped tail probes save. From my own runs at `-j1`:

   ```
   one-over-cap             flash 36   spinning 38
   tail-only-difference     flash 48   spinning 61
   deep-head-boundary       flash 108  spinning 116
   two-classes-alternating  flash 80   spinning 84
   ```

   The builder's own differential table already shows this (`tails` 18 → 19,
   `cohort-boundary` 64 → 72), so it is disclosed rather than hidden — but a
   reader who sees only the seeks table will take "3.0x–3.6x fewer" as
   universal. One sentence naming where the win lives would fix it.

2. **Two stale numbers in `LOG.md` prose.** The M5.1 section's closing line
   says "`cargo test` 159 passed"; the evidence file and my own run both say
   163. The M6 section says "All six mutants are caught now" where the script
   carries seven. The evidence files are right in both cases; only the prose
   drifted.

3. **`VolumeProbe`'s stale doc comment.** The builder flagged this itself under
   "Noticed, not touched" — the comment still justifies the probe thread by the
   cost of a `diskutil` subprocess that round one replaced with IOKit calls.
   Correctly left alone; it is not this branch's code.

4. **Not this branch's, noted in passing:** the attached short-option form
   `-j1` is rejected while the oracle accepts it (`--min-size=1` and `-x*.tmp`
   likewise). I checked `main@baf9e25` and it behaves identically, so M5.1's
   parser rewrite did not cause it and the gate does not exercise it.

5. **`rbench --selftest` was NOT run, and I am claiming no result for it.**
   `rbench/run.py:56` sets `DEFAULT_ROOT = "/Volumes/dedupbench"`, and
   `selftest()` calls `ensure_corpus(volume, ..., rebuild=True)` — it rebuilds
   a corpus on that volume. That is outside this review's fence, and
   `/Volumes/dedupbench` is mounted and in active use by other agents in this
   session; rebuilding its corpus would corrupt their runs, not merely mine.
   The report-JSON question it was meant to answer is covered from the other
   side: the extra `seeks` key breaks nothing, because `--gate all` is green
   33/33 at this tip and `conformance/run.py` checks only that the six protocol
   keys are present. Someone with the bench volume to themselves should still
   run it before merge.

---

## What I checked and found correct

- `auto` never selects `spinning` on unknown media — `Wanted::resolve` gives
  `(Flash, Some(UNKNOWN_MEDIUM))` for `None`, and the pipeline tests prove the
  scan that ran was the flash one. My differential's `--profile auto` column
  matched the oracle on all fourteen trees.
- The hint is one line, on stderr, and does not fire on a normal run here
  (stderr empty, byte-identical to `main`).
- `compare_chunk` under `spinning` returns `SLOW_CHUNK` at every width, and
  `SLOW_CHUNK * SPIN_INTERLEAVE == COMPARE_BUDGET` is asserted, so the width
  cap is what keeps the budget rather than shrinking the run.
- `Probe::Head` is a separate variant with its own `Field::Head`, and
  `hash_stage` maps them one-to-one. `record.tail()` is empty under `spinning`,
  so `split_identical`'s kept-tail path is correctly inert there.
- The learned full digest covers the whole file under both profiles (kept head
  + everything read + kept tail), so a digest learned under one profile is
  valid under the other. Confirmed live: the third run in the column trace
  reads 0 bytes, attested from digests learned under the other profile.
- Seek accounting is counted at the read from the offsets asked for, never
  derived from size, and `At` is keyed on `(dev, ino, offset)` so descriptor
  reuse cannot fake a continuous run. Short reads advance the position by what
  came back, not by what was requested.
- Deletion inviolables hold under `spinning`: byte-proof (the capped sweep is
  `split_identical` on each cohort), survivor guard, `ProvenGroups` untouched.

---

# Trailing review — `main`'s commit `09599c6`

`fix(tui): leave when the terminal dies instead of spinning on a core` merged
to `main` unreviewed. Read against the M5 laws and re-run.

**The commit itself is sound.** `sys::wait_on` (`sys.rs:364`) polls the
descriptor, checks `POLLHUP | POLLERR | POLLNVAL` **before** `POLLIN` (correct
— macOS reports both at end-of-file and it is the hangup that decides), treats
`EINTR` as `Waited` so `SIGWINCH` does not evict a live browser, and treats any
other poll failure as a hangup. The `unsafe` block carries its invariant, and
it lives in `sys.rs`, which is where REWRITE.md §7 says volume/terminal probing
belongs. The browser's loop consumes it three ways —
`Ready => {}`, `HungUp => break`, `Waited => continue` — so `event::read()` is
only ever entered when poll has just said bytes are waiting. Terminal restore
is unchanged and unconditional. No behaviour change for a live terminal.

Re-run on a `main@baf9e25` build I made in scratch:

```
$ python3 rust/evidence/scripts/tui-terminal-death.py <main build>
  keys    PASS  'q' quit the browser in 0.64s, exit 0
  hangup  PASS  left 0.05s after the terminal died, exit code 1, 0.01 CPU-seconds
all scenarios passed
```

Normal interactive input is unregressed: the `keys` case passes on every build
I tested, including the branch tip.

**But the same script fails on this branch's tip**, and that is how I found the
M5.1 defect:

```
$ python3 rust/evidence/scripts/tui-terminal-death.py rust/target/release/dedup-rs
  keys    PASS  'q' quit the browser in 0.64s, exit 0
  hangup  FAIL  still running 5s after the terminal died, 5.05 CPU-seconds in that window — SPINNING
1 scenario(s) failed
```

The regression is not in `09599c6` and not in M6 — it is M5.1's pre-flight
menu, which is now the screen that is up when `-i` starts, and which enters
`event::read()` without poll having said `Ready`. Full diagnosis, the stack
trace, the deterministic reproduction and the fix are binding item 1 of
`REVIEW-M5_1.md`. I raise it there rather than here because the code is M5.1's,
but it is on the tip that would merge, which is why the banner at the top of
this file says the branch must not merge yet.

---

## How to clear the M6 side

Nothing is binding on M6. The two things worth doing before merge, neither of
them gating:

- Add the one sentence to the seeks claim saying where the 3.0x–3.6x holds and
  that small-file trees can go the other way (non-binding note 1).
- Fix the two stale numbers in `LOG.md` prose (non-binding note 2).

And someone with `/Volumes/dedupbench` to themselves should run
`python3 rbench/run.py --selftest` before this merges, since I could not.
