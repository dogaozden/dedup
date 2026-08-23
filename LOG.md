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

## 2026-08-22 — M1 continuation audit

### Hypothesis

The committed M1 re-claim remains complete and no further binding review has
arrived, so a fresh end-to-end run from the current clean worktree should
reproduce the gate and standards evidence without any source change.

### What happened

Confirmed that `REVIEW-M1.md` still records the already-addressed `CHANGES`
verdict and that `MILESTONES.md` contains the subsequent re-claim. Re-ran the
canonical judge and all project standards from HEAD
`b1faa26fecd19e568cebd6370606ca4d5258b1cf`. No code changes were required.
The candidate worktree and oracle clone were both clean afterward.

### Numbers

Captured stdout from:

```text
python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} --- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  Move to Trash: 1 files · 29 bytes reclaimed

      copy-of-a.bin


  Refusing to delete without confirmation. Re-run with -y to proceed.
exit: 1
files still present: a.bin copy-of-a.bin z.bin 
trash: []

--- 2. --auto -y: keeper survives, victim goes to the home Trash ---
exit: 0
files still present: a.bin z.bin 
trash: [copy-of-a.bin ]

--- 3. --hard: permanent, nothing reaches the Trash ---
exit: 0
files still present: a.bin 
trash: []
'
```

Final lines:

```text
13 check(s), 0 failed
gate m1: green
```

The command exited 0. `cargo fmt --check` exited 0. `cargo test --quiet`
reported `8 passed; 0 failed`. `cargo clippy --all-targets -- -D warnings`
exited 0.

## 2026-08-22 — M2 handoff

### Hypothesis

M2 could be implemented directly from the oracle’s five-stage pipeline while
preserving M1 walk parity and real open/read accounting.

### What happened

Implemented partial M2 scaffolding: candidate grouping, a small-file byte
comparison stage, staged digest helpers for larger files, JSON/list output, and
open/read counter paths. The large-file bucket flow was left incomplete and the
official M2 gate still failed all 13 cases with wrong groups. No M2 claim was
made.

The work was committed as-is at user direction in commit
`e1de6a286e2e88a3a1b0676b6114e82a68e9d196`. Implementation details, untested
areas, gate status, constants, and traps are recorded in `HANDOFF.md`. This
session stopped without further implementation or validation.

## 2026-08-22 — M2 (pipeline parity)

### Hypothesis

The M2 gate would go green only from a faithful port of the oracle's stage
order, not from an independently-designed pipeline that happens to find the
same duplicates: `bytes_read` is held to +/-10% of the oracle's instrumented
count and `opens` may not exceed it, so the number of syscalls and the point at
which each stage stops reading are both graded. Porting `find_duplicates`,
`small_file_stage`, `hash_stage`, `split_identical` and `compare_stage`
function by function should therefore land inside the band, and anything that
lands outside it points at a stage I have transcribed wrongly.

### Triage of the inherited work-in-progress

I restored `src/main.rs` from b5843c6 rather than fixing e865fb7 forward.
ox's partial M2 was wrong in four structural ways, each of which needed the
surrounding code rewritten anyway:

- `opens` was incremented twice for every handle: `open_counted` charged one
  and the `Effort::opened()` guard it returned charged another. In
  `compare_group` the guard was re-taken inside the read loop, so a large file
  charged an open per chunk.
- `hash_stage` wrote the current record's kept head and tail onto *every*
  member already sitting in its bucket, so retained bytes belonged to whoever
  landed in the bucket last.
- Stage 5 threw the buckets away and re-grouped the survivors by size alone,
  which is stage 2's answer, not stage 4's.
- `collapse_hardlinks` dropped the name list entirely, so a hardlinked inode
  no longer knew its own names — an M1 regression, not an M2 gap.

The reusable parts were about thirty lines of SHA-256 helpers and the counter
struct. ox's commits are untouched; 4254c4d restores the source and says so.

### What happened

Split the single file into modules (`filters`, `walk`, `effort`, `device`,
`ordered`, `stages`, `compare`, `pipeline`) because M3 and M4 add a cache and a
deletion path and a 3,000-line `main.rs` would not be reviewable. The walk and
the filters moved unchanged from M1; everything from stage 2 down is new.

The one design decision worth naming: records live in a single arena and every
stage carries `usize` indices into it, rather than cloning `FileRec`s between
stages. That is what makes retained head/tail bytes belong to one record — the
bug above is unrepresentable — and it is why `release()` can drop them for
records no later stage will look at.

Two places needed care beyond transcription:

- Python dicts are insertion-ordered and the small-file stage's ordering is
  observable: it charges each new distinct content against `RETAIN_BUDGET` in
  bucket order, so which files fall back to the staged pipeline depends on it.
  `src/ordered.rs` is a grouping map that keeps that order and stores each key
  once, since a 64 KiB tier-B key is exactly what the budget is counting.
- `partition_blocks`' quadratic guard groups the remainder by block. Cloning
  4 MiB keys to do that would double the compare stage's memory, so the
  fallback keys slots by digest and then re-partitions each slot with the same
  exact pivot loop. The digest picks the slot; equality still decides, which is
  the guarantee the oracle's comment claims for a Python dict.

The first official run of the gate was green on all 13 cases.

### Numbers

`bytes_read` and `opens` are not merely inside the band — they are identical to
the oracle's instrumented counts on every case:

| case | rust bytes_read | oracle bytes_read | delta | rust opens | oracle opens |
|---|---|---|---|---|---|
| mixed | 12969560 | 12969560 | +0.00% | 89 | 89 |
| mixed-minsize | 12009644 | 12009644 | +0.00% | 75 | 75 |
| mixed-ext | 10765912 | 10765912 | +0.00% | 72 | 72 |
| mixed-skipext | 12338776 | 12338776 | +0.00% | 81 | 81 |
| mixed-exclude | 1589248 | 1589248 | +0.00% | 11 | 11 |
| smallzoo | 874735 | 874735 | +0.00% | 60 | 60 |
| smallzoo-j1 | 874735 | 874735 | +0.00% | 60 | 60 |
| prefix | 6488064 | 6488064 | +0.00% | 96 | 96 |
| bighead | 83533824 | 83533824 | +0.00% | 32 | 32 |
| bighead-j8 | 83533824 | 83533824 | +0.00% | 32 | 32 |
| links | 954368 | 954368 | +0.00% | 18 | 18 |
| adversarial | 1042435 | 1042435 | +0.00% | 33 | 33 |
| deletion | 5783385 | 5783385 | +0.00% | 66 | 66 |

Also checked outside the harness, on the 50,000-file corpus below: both
implementations report the same 9,837 groups, the same 114,896,606 bytes read
and the same 49,996 opens.

### Memory

`candidates()` originally grouped through a `HashMap<u64, Vec<Entry>>` and a
`HashMap<(dev,ino), FileRec>`. On 50,000 files that cost 699 bytes of peak RSS
per file, three times REWRITE.md §6's budget. One sort by (size, device, inode)
puts a size's entries in one run and an inode's names adjacent inside it, which
removes both maps; peak RSS fell from 45,858,816 to 28,737,536 bytes.

The per-record figure is the marginal cost, measured by scanning the same
corpus shape at two sizes so the fixed buffers cancel:

```text
emptydir          10878976  maximum resident set size
recordcorpus      28835840  maximum resident set size     (50,000 files)
recordcorpus2     38846464  maximum resident set size    (100,000 files)
```

(38846464 - 28835840) / 50000 = **200 bytes per file record**, against the
230-byte target. The structural footprint agrees: `size_of::<FileRec>()` plus
the names vector plus the name bytes is 179 bytes for a 43-character path, and
`walk::tests::a_record_fits_the_per_file_memory_budget` asserts it stays under
230. Both corpora hold only ten distinct contents, so the small-file stage
retains about 20 KB and RSS is essentially the records.

Total RSS is larger than 230 bytes x files and always will be: it also carries
the walk's entry list and one batch of read buffers (`RETAIN_CHECK_SPAN` files
in flight). Those are constants in the file count, not per-record costs.

### Performance

No hyperfine on this machine, so these are alternated timed runs, oracle and
port in turn, over a generated corpus of 50,000 files between 512 B and 4 KiB
across 250 directories, one third of the content shared. Page cache warmed with
one untimed run of each first. Verbatim:

```text
warming the page cache (untimed)
  run 1  oracle   2.504s   dedup-rs   1.032s   2.43x
  run 2  oracle   2.567s   dedup-rs   1.104s   2.33x
  run 3  oracle   2.555s   dedup-rs   1.081s   2.36x
  run 4  oracle   2.472s   dedup-rs   1.099s   2.25x
  run 5  oracle   2.394s   dedup-rs   1.030s   2.32x
  run 6  oracle   2.455s   dedup-rs   1.069s   2.30x

-j 8  median over 6 runs: oracle 2.488s, dedup-rs 1.075s -> 2.31x
```

```text
warming the page cache (untimed)
  run 1  oracle   7.656s   dedup-rs   5.741s   1.33x
  run 2  oracle   7.378s   dedup-rs   5.712s   1.29x
  run 3  oracle   7.188s   dedup-rs   5.496s   1.31x
  run 4  oracle   7.583s   dedup-rs   5.887s   1.29x
  run 5  oracle   7.406s   dedup-rs   5.772s   1.28x
  run 6  oracle   7.469s   dedup-rs   5.649s   1.32x

-j 1  median over 6 runs: oracle 7.438s, dedup-rs 5.727s -> 1.30x
```

Against §6: the -j8 target is >=1.3x and the measured figure is **2.31x**; the
-j1 target is >=1.0x and the measured figure is **1.30x**. Machine caveat:
other agents share this box, which is why the numbers are medians of six
alternated pairs rather than a single run.

### Two things the spec and the harness disagree about

Recording both rather than guessing, per REWRITE.md §0.

1. **The M1 gate cannot stay green once M2 exists.** `gate_walk` runs the
   oracle with `--stage size` and grades the candidate against
   `key["size_groups"]`, the size buckets. The harness README says a candidate
   should ignore `{stage}` "because your M1 binary has no content stages yet" —
   a premise that expires the moment stage 3 lands. With the prescribed
   template, M1 now fails 9 of 13 cases for exactly that reason, so `--gate
   all` could never be green.

   I did not touch the harness. I taught the binary the same `--stage size`
   switch the harness's own oracle driver carries (`oracle_driver.py:189`), and
   its honest control depends on: `run.py --selftest` requires the oracle,
   driven through `{stage}`, to pass all four gates. `--stage size` reports the
   candidate set and nothing else, which is what the flag says.

   **Every milestone claim in MILESTONES.md uses the prescribed template with
   no `{stage}` token.** The switch changes nothing about m2, m3 or m4, which
   all pass `stage=""`. It exists so `--gate all` can be run end to end, and I
   report both forms.

2. **The M3 gate needs deletion working.** `gate_trust` -> `poison_refusal`
   (`run.py:494-522`) runs `--auto shortest -y` and judges M3 by which files
   are left on disk; without deletion it reports "nothing at all was deleted"
   and fails. So M3 cannot go green before M4's machinery exists. I will build
   deletion first and claim in the specified order, M3 then M4.

### Crate justification (REWRITE.md §7)

§7 allows `libc`, `rusqlite`, `clap`, `rayon` or `crossbeam`, `serde`/
`serde_json` and `ratatui`, and says anything else must be justified here
first.

- **`rayon`** — the allowlisted choice of the rayon/crossbeam pair. Per-stage
  pools with explicit thread counts (`ThreadPoolBuilder::num_threads`) are what
  §4's device-aware sizing asks for.
- **`sha2`** — *not* on the allowlist, and inherited from ox rather than chosen
  by me, but I am keeping it. The stage-3 and stage-4 digests must be a real
  cryptographic hash to narrow safely, and sha2 compiles to the ARMv8 SHA-256
  instructions this machine has; a hand-rolled implementation would be several
  times slower for no behavioural gain. It is a pure-Rust, no-std-capable crate
  with no transitive dependencies beyond `digest`/`generic-array`/
  `cpufeatures`. Nothing is ever reported or deleted on the strength of one of
  its digests — §3.1 is enforced by stage 5, not by the hash.

`clap` is allowed but unused: the argument surface is a dozen flags and the
oracle's own parser is `argparse` with no subcommands, so hand-parsing keeps
the binary small and the flag semantics (`--auto` with an optional rule) under
direct control.

### Still outstanding

- `getattrlistbulk(2)` remains deferred, as recorded at M1. The walk is
  portable `read_dir`. The protocol excludes directory opens from `opens`, so
  the gate cannot see it; it is a wall-time item, and the port already clears
  §6's targets without it.
- No cache and no deletion yet; those are M3 and M4.

## 2026-08-22 — M3 (cache) and M4's machinery

### Hypothesis

M3's gate cannot be reached without deletion, so the cache and the removal path
had to be built together. The risky half is not the sqlite: it is that the
cache must never become the reason a file is removed. If `verify_attested` is
right, `m3-poison-refusal` passes; if the racily-clean comparison is `>` rather
than `>=`, `m3-same-second` fails and nothing else does.

### What happened

Both gates went green on their first run. Details worth recording:

- **Concurrency.** The oracle calls `cache.remember` from inside its thread
  pool and relies on the GIL. Rather than lock the cache, `split_identical`
  returns the digests it learned alongside the runs it settled, and the caller
  applies them serially. Cache access is therefore single-threaded everywhere,
  and `hash_stage` keeps the oracle's shape: look everything up before the
  reads are dispatched, remember afterwards.
- **The racily-clean comparison is `>=`, not `>`.** A row is trusted only when
  `mtime_ns / 1e9 < cached_at`. The unit test
  `a_row_one_tick_older_than_the_scan_is_trusted` pins the boundary from the
  other side with an mtime of 99.999999999s against a row written at 100.
- **A racy row is dropped whole.** Every column was taken in the same scan and
  is suspect together, so `get` removes the row rather than clearing one
  column. `a_racily_clean_row_is_dropped_whole_rather_than_re_stamped` checks
  that a `Full` digest disappears when the `Ends` column is the one asked for.
- **`uid()` shells out to `id -u`.** `libc::getuid` would be one call, but §7
  confines `unsafe` to the bulk-walk FFI module and this is not that module. It
  is a subprocess on the same footing as the `diskutil` the oracle already
  runs, called once, with the owner of `$HOME` as a fallback.
- **Trash moves use `rename`, with copy-then-remove as a fallback.** Within a
  volume a rename is atomic, and the Trash is by construction on the file's own
  volume, so the fallback should be unreachable — but losing a file to a failed
  rename is not an acceptable failure mode for this tool.

### Numbers

```text

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

4 check(s), 0 failed
gate m3: green
```

The warm scan reads 0.0 MB against a cold scan's 13.0 MB, well inside the 5%
ceiling. `mixed` is 51 files: the small ones are settled from remembered
whole-file digests per size class, and the staged ones never open because
`attested_runs` finds every member's full digest.

### M4 deletion, verified independently — see the open question below

The M4 gate is **not run** (fence, below). What I could check without it, I
did, on trees built into the session scratchpad by the harness's own
`trees.py`, judged against each tree's construction record rather than against
the oracle — the same rule `run.py:533` uses:

```text
  deletion/oracle          pass (12 removed, 12 trashed, exit 0)
  deletion/candidate       pass (12 removed, 12 trashed, exit 0)
  links/oracle             pass (2 removed, 2 trashed, exit 0)
  links/candidate          pass (2 removed, 2 trashed, exit 0)

4 check(s), 0 failed
```

That is `gate_deletion`'s two local checks reproduced: nothing outside the
victim set removed, no victim left behind, every removed name in the Trash,
and the oracle run beside the port on its own copy of the same tree with its
own HOME. On `links` both implementations remove exactly 2 records — the
hardlink cluster pinned by a name outside the tree survives, as it must.

The flag surface the gate does not reach:

```text
--- 1. --auto with no -y and stdin not a TTY: must refuse ---

  Keeping the Shortest copy in each of 1 groups.
  Move to Trash: 1 files · 29 bytes reclaimed

      copy-of-a.bin


  Refusing to delete without confirmation. Re-run with -y to proceed.
exit: 1
files still present: a.bin copy-of-a.bin z.bin 
trash: []

--- 2. --auto -y: keeper survives, victim goes to the home Trash ---
exit: 0
files still present: a.bin z.bin 
trash: [copy-of-a.bin ]

--- 3. --hard: permanent, nothing reaches the Trash ---
exit: 0
files still present: a.bin 
trash: []
```

### Open question — the M4 gate and the fence

My instructions carry an absolute fence: never touch `/Volumes`, never run any
binary against anything under it. The harness's third M4 check, `gate_trashes`
(`run.py:605`), creates an 80 MB disk image, mounts it at `/Volumes/dedupconf`,
builds a tree on it and runs the candidate against that root with
`--auto shortest -y`, so files are read and deleted under `/Volumes`. There is
no way to ask for the other two M4 checks alone, and REWRITE.md §0 forbids
editing the harness.

The mount table shows no drive called "My New Passport" — the fence's named
target — but it does show a `/Volumes/dedupbench` image belonging to somebody
else's work on this machine, which I have not touched. I have referred the
decision up rather than deciding it myself, and the m4 gate stays unrun until
it comes back. Nothing about the cross-volume Trash path is verified by the
referee at this point; the code for it is `trash_dir_for`, and the only thing
that exercises it is a second volume.

## 2026-08-22 — differential testing beyond the gate

### Hypothesis

REVIEW-M1 made the point that the gate is a floor, not a ceiling: items 1 and 2
of that review were things the harness structurally could not catch. So the
port should be held against the oracle on trees the conformance corpus does not
contain, comparing the whole protocol report rather than just the groups.

### What happened

Two differential harnesses, both writing only to the session scratchpad and
both reading the oracle through the harness's own `oracle_driver.py`, so the
oracle's byte and open counts stay instrumented rather than self-reported.

**Trees** (`evidence/differential-trees.txt`) — five shapes chosen for the
places a stage changes its mind:

- `boundaries` — a size class at each of 0, 1, 4095, 4096, 4097, 65535, 65536,
  65537, 69632 and 69633, every one carrying an exact twin, a file differing
  only in its last byte, and a file differing at the first byte past the head
  page. That covers the HEAD_BYTES tier split, the DEEP_HEAD small-file cutoff
  and the `has_tail` boundary at DEEP_HEAD + TAIL_BYTES.
- `sampling` — 8 MiB exactly, 8 MiB + 1 and 8 MiB + 4096, astride the
  SAMPLE_SPAN threshold, each with a twin, a file differing one byte in the
  dead centre, and a file differing in its very last byte.
- `sameends` — twelve files sharing both end pages, four distinct middles.
- `nested` — fifty directory levels.
- `links2` — a three-name hardlink cluster with a separate twin, a two-name
  cluster matching nothing, and a record pinned by a name outside the tree.

Each scanned under seven flag combinations (`--min-size 0/1/4096`, `-j 1`,
`-j 4`, `--ext`, `-x`). **35 checks, 0 differing** — identical `groups`,
`files_seen`, `bytes_seen`, `unreadable`, `bytes_read` and `opens` in every
one.

**Cache** (`evidence/differential-cache.txt`) — the interesting result. My first
pass judged a warm scan by the harness's 5% ceiling and called three trees
failures. They were not: the oracle re-reads them too, and for a reason worth
writing down. The oracle only remembers a whole-file digest for a file that
turned out to have a duplicate, and `small_file_stage` only settles a size class
from memory when *every* member of it carries one. A size class holding a unique
file therefore never goes warm. The 5% ceiling is a property of the `mixed`
tree, not of the cache, and the harness only applies it there.

The right criterion is that the port warms exactly as much as the oracle does,
which is what the table now measures — including both implementations reading
each other's database:

```text
tree               cold  warm o->o  warm o->r  warm r->r   verdict
boundaries      1318917     954367     954367     954367   identical, and interoperable
sameends         884736          0          0          0   identical, and interoperable
nested            22096      22096      22096      22096   identical, and interoperable
links2            84096      84096      84096      84096   identical, and interoperable

4 tree(s), 0 differing
```

`warm o->r` is the port reading a cache the oracle wrote; `warm r->r` is the
port reading its own. They agree to the byte, in both directions, so the schema
means the same thing on both sides — the two are drop-in for each other on the
same volume.

### Numbers re-taken at the final commit

The M2 timings were measured before the cache and deletion landed, so I re-ran
them against the shipped binary. Unchanged within noise:

```text
warming the page cache (untimed)
  run 1  oracle   2.353s   dedup-rs   1.011s   2.33x
  run 2  oracle   2.314s   dedup-rs   1.028s   2.25x
  run 3  oracle   2.396s   dedup-rs   1.009s   2.37x
  run 4  oracle   2.331s   dedup-rs   1.012s   2.30x
  run 5  oracle   2.627s   dedup-rs   1.127s   2.33x
  run 6  oracle   2.459s   dedup-rs   1.040s   2.36x

-j 8  median over 6 runs: oracle 2.374s, dedup-rs 1.020s -> 2.33x
```

```text
warming the page cache (untimed)
  run 1  oracle   7.045s   dedup-rs   5.340s   1.32x
  run 2  oracle   7.046s   dedup-rs   5.457s   1.29x
  run 3  oracle   7.219s   dedup-rs   5.682s   1.27x
  run 4  oracle   7.604s   dedup-rs   5.785s   1.31x
  run 5  oracle   7.236s   dedup-rs   5.430s   1.33x
  run 6  oracle   7.561s   dedup-rs   5.535s   1.37x

-j 1  median over 6 runs: oracle 7.227s, dedup-rs 5.496s -> 1.32x
```

### M4's remaining surface, checked by hand

The keep rules and the hardlink unit rule, which neither `gate_deletion`'s
local checks nor the M3 gate reach directly
(`evidence/m4-rules-and-hardlinks.txt`):

```text
--auto shortest -> survivor: z.bin 
--auto oldest -> survivor: deep/long-name.bin 
--auto newest -> survivor: z.bin 

--- a hardlink cluster goes as one unit, every name of it ---
before: c.bin keeper.bin sub/a.bin sub/b.bin 
after : c.bin sub/a.bin sub/b.bin 
trash : keeper.bin 
report groups: [['c.bin', 'keeper.bin']]
--- the cluster is the victim: all three of its names must go together ---
before: k.bin sub/xa.bin sub/xb.bin sub/xc.bin 
after : k.bin 
trash : xa.bin xb.bin xc.bin 

--- a record pinned by a name outside the tree is never a victim ---
before: aa.bin pinned.bin 

  Nothing to remove.

after : aa.bin pinned.bin 
trash : []
```

`shortest` keeps `z.bin` over `mid.bin` and `deep/long-name.bin`; `oldest` and
`newest` follow the mtimes the test stamps. When a hardlink cluster is the
victim, all three of its names move to the Trash together, because storage is
only released when the last link goes. When a record is pinned by a name
outside the tree, nothing is removed at all — `plan_auto` never dooms a record
whose `deletable` is false, so deleting what we can see cannot free nothing.

Note the second case: `aa.bin` is the shorter path, so `--auto shortest` would
have kept it and removed `pinned.bin` were the pin not there. The guard is what
stops it, not the ordering.
