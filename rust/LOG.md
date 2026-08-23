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
python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
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
  by me, but I am keeping it. The oracle hashes with `hashlib.sha256`, so a
  vetted SHA-256 crate is the parity-preserving choice; hand-rolling the hash
  to avoid a dependency would be the actual violation of §8's spirit, since it
  re-derives something the oracle already settled and risks diverging from it. The stage-3 and stage-4 digests must be a real
  cryptographic hash to narrow safely, and sha2 compiles to the ARMv8 SHA-256
  instructions this machine has; a hand-rolled implementation would be several
  times slower for no behavioural gain. Its full transitive set is `cfg-if`,
  `cpufeatures` (which pulls `libc`), `digest`, `block-buffer`,
  `generic-array`, `typenum`, `crypto-common` and the `version_check` build
  dependency — eight crates, all long-standing RustCrypto infrastructure, and
  `libc` is on §7's allowlist in its own right. (An earlier draft of this entry
  named only three of them; REVIEW-M2 was right that the list was short.)
  Nothing is ever reported or deleted on the strength of one of its digests —
  §3.1 is enforced by stage 5, not by the hash.

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

## 2026-08-22 — final verification sweep

### What happened

Rebuilt from scratch (`cargo clean` first) and re-ran everything at HEAD, so
the numbers quoted in the claims describe the binary that is actually
committed rather than an earlier one.

```text
$ cargo clean && cargo build --release
     Removed 4123 files, 464.5MiB total
   Compiling dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-rs)
    Finished `release` profile [optimized] target(s) in 25.95s
=== BUILD EXIT: 0 ===

$ cargo test

test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-rs)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.17s
clippy exit: 0
$ cargo fmt --check
fmt exit: 0
```

Gates and checks, all from the clean build:

| check | command | exit |
|---|---|---|
| M2 gate | `run.py --gate m2` with the prescribed template | 0 |
| M3 gate | `run.py --gate m3` with the prescribed template | 0 |
| M1 gate | `run.py --gate m1`, template with `{stage}` appended | 0 |
| M4 local | `gate_deletion`'s two local checks, reproduced | 0 |
| differential trees | 35 checks, oracle vs port, whole report | 0 |
| differential cache | 4 trees, both directions, shared databases | 0 |
| M4 gate | **not run** — see MILESTONES.md | — |

### One divergence I went looking for and did not find

`relative_path` converts an `OsStr` with `to_string_lossy`, which would replace
invalid UTF-8 with U+FFFD and hand a later stage a path that does not exist —
where the oracle's Python strings round-trip arbitrary bytes through
surrogateescape. That would be a silent mis-handling rather than a loud one,
so it was worth testing. It cannot arise on the target platform: APFS rejects a
non-UTF-8 filename at creation with `Errno 92, Illegal byte sequence`, so the
name never reaches the walk. It could still matter on a mounted exFAT or NTFS
volume or a network share, and it is written down here rather than fixed,
because fixing it means carrying the raw `PathBuf` alongside the reported name
on every record and that costs §6's memory budget.

### Smaller divergences from the oracle, none of them gated

- The oracle takes several roots (`paths` is `nargs="*"`); this takes one, which
  is all the conformance protocol's `{root}` ever supplies. The name-dedup in
  `fold_inode` that exists for overlapping roots is therefore unreachable here,
  and kept because a second root is the obvious next feature.
- `--min-size` is parsed as `u64`, so a negative value is an error; the
  oracle's `type=int` accepts one and lets every file through.
- The curses browser (M5) does not exist and is out of scope.
- `getattrlistbulk(2)` is still deferred, as recorded at M1. The walk is
  portable `read_dir`. The protocol excludes directory opens from `opens`, so no
  gate can see it, and §6's wall-time targets are already met without it.
  **Intended milestone: M5**, alongside the TUI, or in a dedicated performance
  pass if M5 is never commissioned — it is the only remaining item from
  REWRITE.md §4 that this port does not implement, and naming a milestone for it
  is REVIEW-M1 item 6, which I answered vaguely the first time.

### Fence

Verified rather than asserted. The oracle clone at `/Users/dogaozden/ox/
dedup-oracle` has an empty `git status` and sits at `b6dd81f`, unchanged.
`/Users/dogaozden/AI_Projects/dedup-rs` is the only tree I wrote to, apart from
the session scratchpad. Nothing under `/Volumes` was touched, read or scanned.

One thing to flag rather than act on: `/Users/dogaozden/AI_Projects/dedup`, the
live checkout I am forbidden to touch, holds an untracked `NOTE-FROM-OPUS.md`
that I did not create. I looked only at `git status` there, to prove I had not
written to it, and left the file alone.

## 2026-08-22 — M4 claimed, and the whole gate green

### What happened

The maintainer ruled the harness's own disk image inside the fence, narrowly
and with conditions, having read `gate_trashes` independently. Both gate runs
were made under those conditions: `ls /Volumes` immediately before each, no
manual command ever pointed at a `/Volumes` path, and `/Volumes/dedupconf`
touched only through the harness.

Both went green on the first run.

```text

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

3 check(s), 0 failed
gate m4: green
```

```text

M1 gate
  mixed                    pass
  mixed-minsize            pass
  mixed-ext                pass
  mixed-skipext            pass
  mixed-exclude            pass
  smallzoo                 pass
  smallzoo-j1              pass
  prefix                   pass
  bighead                  pass
  bighead-j8               pass
  links                    pass
  adversarial              pass
  deletion                 pass

M2 gate
  mixed                    pass
  mixed-minsize            pass
  mixed-ext                pass
  mixed-skipext            pass
  mixed-exclude            pass
  smallzoo                 pass
  smallzoo-j1              pass
  prefix                   pass
  bighead                  pass
  bighead-j8               pass
  links                    pass
  adversarial              pass
  deletion                 pass

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

33 check(s), 0 failed
gate all: green
```

**33 checks, 0 failed, across all four milestones.** `m4-trashes` is the one
that matters most here: it is the only check that exercises `trash_dir_for`'s
non-boot branch, the thing I flagged as unverified while the gate was blocked.
It passes, so that gap is closed.

### The pre-flight check, and one oddity worth naming

`ls /Volumes` before and after both runs returned exactly two entries:
`dedupbench`, which belongs to somebody else's work on this machine and which I
never touched, and a stray directory entry named `]`. The maintainer's
condition was to stop on an unexpected *volume*; `]` is not one — it does not
appear in the mount table, so nothing is mounted on it. It looks like a
leftover mountpoint stub or the result of an unquoted shell glob. I did not
touch it, did not read it, and did not remove it: it is not mine, and the fence
covers it.

`/Volumes` is byte-identical before and after both runs. The harness detached
and unlinked its image in its `finally`, as it says it does.

### Milestone ordering, recorded as the maintainer asked

The spec's order is M2 → M3 → M4, and the claims follow it. The *build* order
could not: `gate_trust` → `poison_refusal` (`run.py:494-522`) runs
`--auto shortest -y` and judges M3 by which files are left on disk, so M3's
gate cannot go green until M4's deletion machinery exists. Deletion was
therefore built second and the cache third. This is a property of the harness,
not a change of plan, and the claims are in spec order regardless.

## 2026-08-22 — REVIEW-M2 and REVIEW-M4 fix round

### Hypothesis

Seven binding items across two reviews. Six were straightforward; one was the
kind of defect that should change how I work, not just what the code does.

### The one that mattered

REVIEW-M4 item 1: `--stage size --auto` deleted files that were not duplicates,
permanently, having read no bytes of any of them. The reviewer reproduced it and
lost two files with provably different contents.

The mechanism is embarrassing in how ordinary it is. I added `--stage size` to
satisfy the M1 gate, mirrored `oracle_driver.py`'s `size_stage` faithfully, and
did not read the next four words of the driver — `and args.stage == "full"` on
the line that dispatches deletion. I checked what the flag did to the *report*.
I never asked what it did to the *other flags*, on a tool whose job is removing
files.

REWRITE.md §3.1 says nothing is deleted on the strength of a hash or a sample.
This was weaker than a hash: it was `st_size`. And no gate could see it, because
`gate_walk` passes `--stage size` but never `--auto`, and `gate_deletion` passes
`--auto` but always with `{stage}` empty. The two only meet on a command line a
person types.

I took the structural fix rather than the string comparison, because the string
comparison is exactly the thing I had already proved I would forget.
`Scan::groups` is private; it leaves the struct through `reported()` for the
protocol report, where a size bucket is an honest answer, or through `proven()`,
which returns `None` for a size-only scan. `run_auto` takes a `ProvenGroups`
token whose field is private to `pipeline`, so no other module can mint one. The
deletion path does not type-check with unproven groups. `--stage size --auto`
exits 2 and says why.

The lesson generalises past this bug: when a new mode changes what a *group*
means, the deletion path is the first place to look, not the last.

### The other six

- **`--auto newest` kept the wrong copy on a tie.** Rust's `max_by_key` returns
  the last maximum; Python's `max` returns the first. Members arrive
  shortest-name-first, so the oracle kept the shortest-named copy and this kept
  the longest-named one. Reversing the comparator and taking the minimum gets
  the first maximum. `min_by_key` already matched, which is why only `newest`
  was wrong — the sort of asymmetry that survives casual reading.
- **The keep rules compared a finer clock than the oracle can see.** The oracle
  compares `st_mtime`, a float64, which resolves about 240 ns at current epoch
  values; this compared `mtime_ns`. Files a nanosecond apart were tied there and
  ordered here, so the tie-break picked different survivors. I chose parity over
  the better-in-isolation nanosecond comparison: which copy survives is the
  whole user-visible contract of a keep rule and §0 makes the oracle
  authoritative. **The divergence that remains, deliberately:** `mtime_ns` is
  still what the cache key and the retained-bytes revalidation compare, where
  being stricter than the oracle only ever means reading more, never deleting
  more.
- **Memory: 280 B/file against §6's 230.** Three genuine per-file wastes, not
  an accounting dispute. The walk built an `Entry` list and then a `FileRec`
  list beside it, so every surviving file existed twice at peak; the walk now
  builds records directly and `candidates` compacts them in place with a write
  index. The report cloned every reported path into a `Vec<Vec<String>>` and
  then serialised that into a second `Vec<u8>`; it now borrows the paths and
  writes straight to the file. And `head`/`tail` were two always-present
  `Vec<u8>`s costing 48 bytes on every record, when only records reaching stage
  3 ever carry any; they are one `Option<Box<Retained>>`, 8 bytes.

  ```text
  minimum of three runs each, marginal cost over 50,000 extra files

names              50,000 files  100,000 files     B/file
16 characters          19906560       27672576      155.3
44 characters          21463040       30425088      179.2

REWRITE.md §6 budget: 230 bytes per file record
```

  **REVIEW-M2 was right that LOG.md said the opposite of the truth.** The
  earlier entry claimed total RSS above the budget was "the walk's entry list
  and one batch of read buffers ... constants in the file count". The read
  buffers are constant. The entry list was O(files) and was the single largest
  reason the figure was 280. That sentence was wrong and I have not left it
  standing.
- **The glob engine backtracked exponentially.** REVIEW-M1 raised it as
  non-binding; I carried it forward and, worse, wrote a comment asserting it was
  handled. It was not: `(0..=text.len()).any(|i| inner(&tokens[1..], &text[i..]))`
  *is* the exponential backtrack. 121 seconds on one 40-character name against
  fnmatch's third of a second. It is now a bottom-up fill over (token index,
  character index) — each pair decided once, O(tokens x characters) — with the
  reviewer's three patterns as a test that fails over a second. A comment
  claiming a property the code does not have is worse than no comment, and this
  one cost a review cycle.
- **The quoted gate commands could not be run.** A scripted find-and-replace
  expanded `evidence/m4-flags.txt` into the literal `{flags}` token in four gate
  commands. Restored, and I copied the M2 command out of MILESTONES.md and ran
  it unmodified rather than eyeballing it. The hazard is general: writing
  documentation by templated substitution puts every brace-delimited token in
  the document at risk, and the tokens in this project's commands are
  brace-delimited by design.
- **Non-binding notes, all taken.** `sample_points` asserts the precondition
  that keeps its subtraction in range; the `sha2` dependency list is corrected
  from three crates to the actual eight; and the `getattrlistbulk` deferral now
  names M5, which REVIEW-M1 item 6 asked for and I answered vaguely the first
  time.

### Verification

Everything re-run from a clean rebuild at this commit.

```text
$ cargo clean && cargo build --release
     Removed 2799 files, 389.3MiB total
   Compiling dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-rs)
    Finished `release` profile [optimized] target(s) in 53.68s
build exit: 0

$ cargo test

test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s


$ cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-rs)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.60s
clippy exit: 0
$ cargo fmt --check
fmt exit: 0
```

| check | exit |
|---|---|
| `--gate m2` | 0 |
| `--gate m3` | 0 |
| `--gate m4` | 0 |
| `--gate all` | 0 — 33 checks, 0 failed |
| differential trees, 42 checks | 0 differing |
| differential cache, 4 trees both directions | 0 differing |
| keep rules, 12 rule/tie combinations | 0 differing |
| local deletion checks | 0 failed |

Fence: `ls /Volumes` before each gate run showed only `dedupbench`, which is
another project's, and the stray `]` entry that appears in no mount table.
`/Volumes/dedupconf` was touched only through the harness. `/Volumes` was
unchanged afterwards.

### A working-practice note worth recording

The session scratchpad at
`/private/tmp/claude-501/.../scratchpad` is **shared between agents on this
machine**. Part-way through this round I re-ran what I thought was my own
differential harness and got a report in a format I did not write: another
agent had written a file of the same name over mine. Nothing was corrupted in
the repo, and the replacement script's two differences turned out to be the
exit-code artifact REVIEW-M2 had already ruled benign — but the evidence in this
repo should not depend on a file anyone can overwrite. Every harness that
produces something quoted in a claim now lives in `evidence/scripts/` and is
re-runnable from the repo. The perf corpus is rebuilt under a name of my own for
the same reason.

## 2026-08-23 — M5 (the interactive browser)

### Hypothesis

The browser could be a thin frontend: read the oracle's curses mode for the
information architecture, keep the state machine free of any terminal so it
could be tested by calling functions, and route every deletion through the
machinery `--auto` already uses rather than writing a second removal path.

### What happened

Built `src/tui/` in three parts — `model` (state, no terminal), `view` (paint,
no decisions), `mod` (the terminal, the event loop, and the bridge to `trash`).
`trash::run_selected` is the new entry point beside `run_auto`; it takes the
same `ProvenGroups` token, so a browser over a `--stage size` scan will not
type-check, and it enforces the survivor guard itself instead of trusting the
UI. `Rule::keeper_of` is now the single definition of what each keep rule
means, called by both `plan_auto` and the browser's bulk menu.

**Two bugs the end-to-end runs caught that the unit tests had not.** Both were
mine, both were in the same place, and both would have deleted the wrong file.

1. *A mark named a screen position.* The first version passed
   `(group position, member position)` to `run_selected`. That is correct until
   the first deletion, after which the browser drops emptied groups and
   renumbers what is left while the scan's own list does not change. The second
   round of a session then named a different group. The scripted run showed it
   as a deletion that removed nothing; the same drift with a live record on the
   other end would have removed a file nobody chose. A mark is now
   `(group in the scan, record in the scan)` — record indices never move —
   and `run_selected` checks the record really is in the group it was filed
   under. Regression test:
   `a_mark_names_the_scans_group_even_after_the_list_has_renumbered`.
2. *The survivor guard counted rows, not copies.* It refused a selection only
   when it covered every member the scan recorded. Across two rounds that is
   not the same question: a group of three that already lost one has two copies
   left, and marking both passed a count against three. The guard now counts
   members that are unmarked **and** still on disk. Regression test:
   `the_guard_counts_copies_still_on_disk_rather_than_rows_in_the_group`.

The end-to-end script drives the real binary through a pty. Two things a bare
pty does not do for you: it has no window size until you set one with
`TIOCSWINSZ`, and it does not answer the cursor-position query (`ESC[6n`) that
the backend issues at startup. Without both the browser simply hangs, which
cost an hour before I read the raw bytes coming back off the master fd.

### Divergences from the curses oracle, and why

- **The browser needs a flag to be asked for outright.** The oracle browses
  whenever stdout is a terminal and neither `--list` nor `--auto` was given,
  and so does this — but `-i`/`--interactive` says it explicitly, and only then
  is a pipe an error (exit 2, `NO_TERMINAL`) rather than a silent fall-through.
  A default that refuses loudly would have printed a refusal on every piped
  run the harness makes; a flag that refuses silently would be useless to a
  script that meant it. This is the one flag the oracle does not have.
- **Overlays are `Block`s.** curses drew `┌─ TITLE ─┐` by hand and measured its
  own text. A bordered ratatui block does both, so the corners are rounded and
  the footer is a bottom title. Same information, native idiom.
- **The help text says what ENTER will actually do.** The oracle's help always
  reads "move marked files to Trash" even under `--hard`; here it reads
  "delete marked files permanently" when that is what will happen. The oracle's
  own confirmation dialog already made that distinction, so this is the help
  text catching up with it rather than a new behaviour.
- **The help text lists `n` / `p`.** They have always been group navigation in
  the oracle; its help never mentioned them.
- **The post-deletion flash says "deleted" under `--hard`.** The oracle says
  "moved N files" either way.
- **No summary flash is lost on the way out.** In the oracle, finishing the
  last group returns from `run()` before the next `draw`, so the "moved N
  files" message it just set is never painted. Here the same thing happens, and
  the summary the user actually reads is the one printed on the restored
  terminal — which the oracle also prints. No information is missing; it is
  just not written twice.
- **Dates need a subprocess.** `strftime("%Y-%m-%d", localtime(...))` has no
  std equivalent that knows the local zone, and §7 keeps `unsafe` (and so
  `localtime_r`) inside the bulk-walk FFI module. `date +%z` runs once per
  session — the same footing as the `id -u` in `trash` — and the date is then
  pure arithmetic. UTC is the fallback; being wrong about the zone shifts a
  displayed date by at most a day and cannot affect what is deleted.

### `getattrlistbulk(2)`, re-deferred

LOG.md's "smaller divergences" entry named M5 as the milestone that would land
the bulk walk, "or a dedicated performance pass if M5 is never commissioned".
M5 has now been commissioned and it is not that milestone: M5-TUI.md scopes
this run to the browser and says to match the oracle's feature set and stop
there, and REWRITE.md §6's wall-time targets are already met without the FFI.
Landing an `unsafe` walk rewrite inside a milestone whose review is blocking
because it is deletion-adjacent would be the wrong thing to put in the same
diff. It belongs to a dedicated performance pass, which is where I am leaving
it, said plainly here rather than dropped quietly.

### Crate justification (REWRITE.md §7)

`ratatui` is on the allowlist for M5. `cargo add ratatui` with default features
pulls the termwiz, termion and termina backends as well — 110 crates including
`wezterm-*`, `uuid` and `time`. M5-TUI.md allows "`ratatui` + its default
`crossterm` backend", so the dependency is
`ratatui = { default-features = false, features = ["crossterm"] }`: 74 crates,
and `crossterm` comes in through `ratatui::crossterm` rather than as a
dependency of its own. Nothing in the scan, cache or deletion path links
against any of it.

### Numbers

| check | result |
| --- | --- |
| `cargo test` | 101 passed, 0 failed |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo fmt --check` | clean |
| `--gate all` | 0 — 33 checks, 0 failed |
| end-to-end browser runs, 7 scenarios | 0 differing |
| terminal restore, 3 exit paths | 0 differing |
| CLI dispatch, 7 flag and terminal combinations | 0 differing |

The suite went from 45 tests to 101. The 56 new ones are 27 on the model, 12
on the paint through `TestBackend`, 9 on `run_selected` against real scratch
trees, and 8 on the key map, the date arithmetic and the drop guard.

Fence: `ls /Volumes` before and after the gate run showed `dedupbench`, which
is another project's, and the stray `]` entry that appears in no mount table.
No command of mine named a `/Volumes` path. Every tree the browser deleted from
was generated under this session's scratchpad, under `m5-`-prefixed names, with
`HOME` pointed at a scratch directory so nothing reached the real Trash.

## 2026-08-23 — REVIEW-M5 fix round

### Hypothesis

Both binding items looked local — one wrong string handed to a shared
function, one early return in the wrong place — but REVIEW-M5 names a root
cause they share, and the fix is only worth having if it addresses that: the
browser's *view* of the world was leaking into decisions. So the question for
each was not "what is the smallest edit" but "what stops this kind of leak".

### 1. The bulk rules ranked a string built for drawing

`apply_bulk` passed `Member.path` — the display path — to `Rule::keeper_of`,
while `--auto` passes the record path. The two agree until the working
directory sits inside the scanned tree, and then some members strip their
prefix and others do not, the strings stop sharing one, and `name_order` stops
being order-preserving between them. Standing in `tree/photos`, "keep the
shortest path" kept `photos/aaaa.bin` (drawn as `aaaa.bin`) where
`--auto shortest` kept `b.bin`. The user was told the longer path would go and
the shorter one went.

The oracle has no such split: `KEEP_RULES["shortest"]` ranks `fs[i].path`, and
`.display` is a separate property built *from* it and used only for drawing
(`dedup.py:2226`, `dedup.py:150-161`). So this was a divergence from the oracle
and from this program's own `--auto` at once, and it was not in the logged list
of chosen divergences, which makes it the silent kind M5-TUI.md forbids.

`Member` now carries both strings, `name` (the scan's own, relative to the
root) and `path` (what to draw), with the doc comment on each saying which is
which and that they are not interchangeable. Every decision reads `name`;
nothing but the painter reads `path`. Two comments that asserted the opposite —
`trash.rs`'s "cannot come to mean one thing under `--auto` and another under a
menu" and its twin in `model.rs` — were false when written and now say the
thing that is actually true.

**The lesson, since it is the general one: sharing the function is not sharing
the rule if the inputs differ.** A shared `keeper_of` looked like it made the
two paths agree by construction. It made them agree only about *how* to rank,
and the argument decided *what*. `oldest` and `newest` were never affected
because they compare `mtime`, which is one number with one meaning.

### 2. Below 54×12 the confirmation was not drawn and `y` still deleted

`view::draw` returns early on a frame too small to lay out — correct for the
browse screen — but it returned before the overlay match, so in `Mode::Confirm`
the dialog was not drawn while the model still accepted `y`. A deletion could
proceed with the count, the bytes and the Trash-vs-permanent mode never on
screen, which hard rule 3 forbids outright.

REVIEW-M5 allowed either drawing the overlays before the early return (the
oracle's shape — its `_put` clips per line) or refusing `Key::Apply` below the
minimum. I took the second, and not only because it is smaller: at 20×8 a
clipped box cannot show "Move to Trash: 1 files" at all, so the first option
would satisfy the letter of "a dialog was drawn" while still deleting on
numbers nobody could read. Drawing something is not the requirement; the
numbers being legible is.

The minimum now lives in `model.rs`, because it stopped being only a layout
number the moment it decided what the browser would act on, and the paint layer
reads it from there so the two cannot drift apart. The model carries
`legible`, set from the area of the frame that was actually painted, and on a
frame below the minimum it takes no key but `Quit`. Movement and marking go
with it — acting on a list that was never drawn is the same mistake more
cheaply — and the resize message the user *can* see is the one thing on screen.
A `debug_assert` in the event loop makes the invariant loud if it is ever
inverted.

### Taken from the non-binding notes

- **Nothing tested the browser's re-read of a cache-attested group.** The
  reviewer showed that deleting the `verify_attested` call in `run_selected`
  left every test and the whole gate green, because `m3-poison-refusal` covers
  `run_auto` and nothing covered its sibling. Closed with an end-to-end case:
  warm the cache on two 24 MB copies, poison one byte in the *middle* and put
  the mtime back, so head, tail and every sample point still match and only a
  full re-read can tell. The scan reads 0 bytes and still reports the group —
  that zero is the proof it was believed rather than seen — and the browser
  reads it, disbelieves it, removes nothing and says why. I checked the test
  can fail: with the `verify_attested` call removed it reports three differing
  checks and `b.bin`, the copy that no longer matches, goes to the Trash.
  105 unit tests stay green through that same removal, which is the reviewer's
  point about where the hole was.
- A mark is now a `Mark { group, record }` rather than a bare `(usize, usize)`.
  This does not make a screen position unrepresentable — only a distinct type
  for record indices would, and that is a wider change than a fix round should
  carry — but the two fields are both `usize` and nothing except their order
  distinguished them, and swapping them was a live failure mode.
- Dropped the second `put_the_terminal_back` in `run`: the drop guard covers
  every path it covered, and two mechanisms for one guarantee make a reader
  ask which is load-bearing.
- `evidence/m5-standards.txt` now has a generator beside it like the others.

### The wrinkle inherited from the oracle, so nobody "fixes" it

The confirmation counts records ("1 files") while the exit summary counts names
("2 files moved to Trash") when a hardlink cluster is involved. The oracle does
exactly this (`dedup.py:2462` against `dedup.py:2511`), so it is parity, not a
bug. Changing either number would make it a divergence.

### Numbers

| check | result |
| --- | --- |
| `cargo test` | 105 passed, 0 failed (101 → 105) |
| `cargo clippy --all-targets -- -D warnings` | clean |
| `cargo fmt --check` | clean |
| `--gate all` | 0 — 33 checks, 0 failed |
| end-to-end browser runs, 10 scenarios | 0 differing |
| terminal restore, 3 exit paths | 0 differing |
| CLI dispatch, 7 flag and terminal combinations | 0 differing |

`m5-screens.txt` came back byte-identical, which REVIEW-M5 expected to move. It
did not because that script runs the browser with its working directory at the
scanned root, where the display name and the scan name are the same string, and
because it only opens the bulk menu to photograph it and leaves with `esc`
without applying a rule. The transcript was regenerated from the fixed binary
all the same.

Fence: `ls /Volumes` before and after the gate run showed `dedupbench`, which
is another project's, and the stray `]` entry that appears in no mount table.
No command of mine named a `/Volumes` path. Every tree deleted from was
generated under this session's scratchpad with an `m5-` prefix and `HOME`
pointed at a scratch directory.
