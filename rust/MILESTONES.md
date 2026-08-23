# Milestones

## M1 — walk parity — RE-CLAIMED after REVIEW-M1 CHANGES

Implementation provides repository scaffolding, the M1 CLI skeleton,
portable directory traversal with oracle-ordered exclusions, size grouping,
hardlink collapse, extension/name filters, and the JSON conformance report.
macOS `getattrlistbulk(2)` remains explicitly deferred in `LOG.md`.

- Implementation commit addressing all binding review items:
  `06a5f640c11d7cdde9b98842f0f383960d36de00`
- Gate command: `python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'`

Final gate output, captured to `/tmp/dedup-rs-m1-gate.txt` by redirecting
stdout of the command above:

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

13 check(s), 0 failed
gate m1: green

```

## M2 — pipeline parity — RE-CLAIMED after REVIEW-M2 CHANGES

Stages 3 to 5 complete: the two-tier small-file stage, the one-open end probe,
the deep-head remainder, sample windows past `SAMPLE_SPAN`, and a wave-based
lockstep byte comparison that reuses the head and tail bytes stage 3 kept,
revalidated by `fstat` before they are trusted. `-j` is honoured by both pools.

- Implementation commit addressing all three binding items: `9c3c5a8c9879198843282ff5f1ab1bcf3b4b13b6`
  (pipeline in `56b13e8`, review fixes in `9c3c5a8`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m2-gate.txt` by redirecting that command's stdout, and
quoted from the file:

```text

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

13 check(s), 0 failed
gate m2: green
```

The command exited 0.

### The three binding items

**1. Memory was 280 B/file against §6's 230.** Three per-file costs went, all
of them real waste rather than accounting: the walk built an `Entry` list and
then a `FileRec` list beside it, so every surviving file was represented twice
at peak — the walk now builds records directly and `candidates` compacts them
in place; the report cloned every reported path, so it now borrows them and
serialises straight to the file; and the two retained-byte vectors, empty on
every record that never reaches stage 3, became one optional box, which is 8
bytes instead of 48. Measured the way REVIEW-M2 measured it — same corpus shape
at two file counts so fixed buffers cancel, minimum of three runs:

```text
minimum of three runs each, marginal cost over 50,000 extra files

names              50,000 files  100,000 files     B/file
16 characters          19906560       27672576      155.3
44 characters          21463040       30425088      179.2

REWRITE.md §6 budget: 230 bytes per file record
```

The reviewer's sentence about the entry list is corrected in LOG.md: it was
O(files), not a constant, and it was the largest single reason the figure was
280.

**2. The glob engine backtracked exponentially.** `-x '*a*a*a*a*a*a*a*a*a*a*a*b'`
against one 40-character name took 121 seconds. The `*` branch is now a
bottom-up fill over (token index, character index), so each pair is decided once
and the whole match is O(tokens x characters). The comment that claimed this was
already handled is gone.
`filters::tests::a_pathological_pattern_still_finishes_promptly` runs the
reviewer's three patterns and fails if they take over a second; they take
microseconds.

**3. The quoted gate commands were unrunnable.** A scripted find-and-replace had
expanded `evidence/m4-flags.txt` into the literal `{flags}` token in four gate
commands across MILESTONES.md and LOG.md. All four are restored. I copied the
command block above out of this file and ran it unmodified to check.

### Effort counters against the oracle

Required: `bytes_read` within +/-10% of the oracle's instrumented count and not
below the tree's floor, `opens` no greater than the oracle's. Measured: both are
*identical* to the oracle on all 13 cases. The table is in LOG.md.

### Performance against REWRITE.md §6

Alternated timed runs (no hyperfine on this machine) on a generated corpus of
50,000 files, 512 B to 4 KiB, across 250 directories, re-taken at this commit:

- `-j 8`: oracle 5.057s, dedup-rs 1.710s — **2.96x** (target >= 1.3x)
- `-j 1`: oracle 10.506s, dedup-rs 8.112s — **1.30x** (target >= 1.0x)
- record memory: **155 B/file** (16-char names), **179 B/file** (44-char names),
  against a 230 budget
- bytes read: never more than the oracle on any tree — exactly equal on all 13

Absolute times are higher than the first measurement because other agents share
this machine; the ratios are what alternated runs are for. Full output in
`evidence/perf-j8.txt` and `evidence/perf-j1.txt`.

### Standards

`cargo test` 45 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0, all from a clean rebuild
(`evidence/final-standards.txt`).

### Note on `--stage size`

The M1 gate grades candidates against size buckets, on the harness premise that
an M1 binary has no content stages yet. Once stage 3 landed, the finished
pipeline fails that gate 9 of 13 — by reporting only the files whose bytes
match — so the binary carries a `--stage size` switch, mirroring the harness's
own oracle driver, used only by the m1 line of `--gate all` below. The M1
transcript above was captured at the M1 commit, when the prescribed template
still passed. Full write-up in LOG.md.

REVIEW-M2 ruled the switch acceptable and flagged that shipping it unguarded on
a deletion tool was not. That is binding item 1 of REVIEW-M4 and is fixed; see
the M4 claim below.

## M3 — cache — CLAIMED

One sqlite database per volume UUID under `DEDUP_CACHE_DIR` (falling back to
`~/Library/Caches/dedup`), schema version 3, rows keyed by inode and qualified
by (size, mtime_ns), with the columns the oracle's stages fill: ends, deep,
sample, full. `--no-cache` opens nothing and writes nothing.

The cache narrows and never proves. A group whose members all carry remembered
whole-file digests is reported as attested and read again before anything in it
is deleted; on mismatch the whole group is refused and its rows discarded. A row
is trusted only while the file's mtime second is strictly older than the second
the row was written, and a racy row is dropped whole rather than re-stamped
column by column.

- Implementation commit: `71de381f668e729577b5410608d76c081d2acf20` (cache and deletion in `9612376`)
- Gate command, run with cwd = the `dedup-rs` repo after `cargo build --release`:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m3 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m3-gate.txt` by redirecting that command's stdout, and
quoted from the file:

```text

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

4 check(s), 0 failed
gate m3: green
```

The command exited 0. M2 was re-run at the same commit and is still green
(`evidence/m2-gate.txt`, `13 check(s), 0 failed`).

Note on ordering: this gate cannot pass without deletion. `m3-poison-refusal`
runs `--auto shortest -y` and judges the result by which files are left on
disk, so M4's machinery had to land first. It is recorded in LOG.md as a
harness/spec discrepancy rather than a change of plan.

### Standards

`cargo test` 39 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
exit 0. `cargo fmt --check` exit 0.

## M4 — deletion — RE-CLAIMED after REVIEW-M4 CHANGES

Trash placement per volume, the survivor guard, `--auto` with all three keep
rules, `-y`, `--hard`, the TTY refusal, hardlink clusters removed as a unit,
records pinned from outside the tree never removed, and byte re-verification of
cache-attested groups before any removal.

- Implementation commit addressing all four binding items: `9c3c5a8c9879198843282ff5f1ab1bcf3b4b13b6`
  (deletion in `9612376`, review fixes in `9c3c5a8`)
- Gate command:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m4 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

Captured to `evidence/m4-gate.txt`, quoted from the file:

```text

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

3 check(s), 0 failed
gate m4: green
```

The command exited 0.

### 1. `--stage size --auto` no longer reaches the deletion path — structurally

The reviewer's reproduction, re-run against this commit
(`evidence/m4-stage-guard.txt`):

```text
=== REVIEW-M4 item 1 reproduction: --stage size --auto must not delete ===
before:
  alpha.bin    791235c06f572a70cb55f829507b4926
  bravo.bin    64caede5d77a72aaaf89add5cd1defde
  charlie.bin  5db22b7a73818e9172cd65da96a9fb8c
dedup-rs: --auto cannot act on a --stage size scan. Size buckets are files that merely share a length; nothing has read their contents, so removing one would not be removing a duplicate. Re-run without --stage.
EXIT: 2
after:  alpha.bin bravo.bin charlie.bin 

=== same tree, no --stage: normal behaviour, three distinct files, nothing to remove ===

  Nothing to remove.

EXIT: 0
after:  alpha.bin bravo.bin charlie.bin 

=== and a real duplicate still deletes, with the oracle's lowercase wording ===

  Keeping the shortest copy in each of 1 groups.
  Move to Trash: 1 files · 16 bytes reclaimed

after:  alpha.bin bravo.bin charlie.bin 
trash:  zz-copy.bin 
```

Three files with three different contents; all three survive, the exit is 2, and
the message says why. A real duplicate in the same tree still deletes normally.

The fix is the one REVIEW-M4 asked for rather than the minimal one.
`Scan::groups` is private. It comes out two ways: `reported()`, used to build
the protocol report, where a size bucket is an honest answer; and `proven()`,
which returns `Option<ProvenGroups>` and yields `None` for a size-only scan.
`run_auto` takes a `ProvenGroups`, whose field is private to `pipeline`, so no
other module can construct one. A future caller cannot reintroduce this by
forgetting a string comparison, because there is no string comparison — the
deletion path will not type-check with unproven groups.

### 2 and 3. The keep rules now pick the copy the oracle picks

`newest` used `max_by_key`, which returns the *last* maximum where Python's
`max` returns the first, so a tie kept the longest-named copy instead of the
shortest-named one. And the rules compared `mtime_ns` where the oracle compares
`st_mtime` as a float64, which resolves about 240 ns — so files a nanosecond
apart were ordered here and tied there. Both fixed: the comparator is reversed
and minimised to get the first maximum, and `oracle_mtime` reproduces
`seconds + 1e-9 * nanoseconds` exactly. `mtime_ns` remains the cache key and the
retained-bytes revalidation, where being stricter than the oracle only ever
means reading more; that split is recorded in LOG.md.

Every rule on every tie case the reviewer named, oracle beside port on identical
trees (`evidence/m4-keep-rules.txt`):

```text
case                     rule       oracle keeps   dedup-rs keeps 
all mtimes equal         shortest   c.bin          c.bin          same
all mtimes equal         oldest     c.bin          c.bin          same
all mtimes equal         newest     c.bin          c.bin          same
two tied newest          shortest   c.bin          c.bin          same
two tied newest          oldest     c.bin          c.bin          same
two tied newest          newest     bb.bin         bb.bin         same
sub-microsecond oldest   shortest   c.bin          c.bin          same
sub-microsecond oldest   oldest     bb.bin         bb.bin         same
sub-microsecond oldest   newest     c.bin          c.bin          same
plainly ordered          shortest   c.bin          c.bin          same
plainly ordered          oldest     aaa.bin        aaa.bin        same
plainly ordered          newest     c.bin          c.bin          same

0 rule check(s) differing
```

### 4. The plan prints the rule's own name

```text
  Keeping the shortest copy in each of 1 groups.
```

`71de381` added `Rule::name()` and a `Display` impl and never changed the call
site. It does now.

### Fence conditions this run was made under

`ls /Volumes` immediately before each gate run: `dedupbench`, which is another
project's and which I never touched, and a stray `]` entry that appears in no
mount table — the reviewer independently identified it as a symlink to `/`. No
drive named "My New Passport" and no unexpected external volume at any point.
`/Volumes/dedupconf` was touched only through the harness invocation; no command
of mine ever named a `/Volumes` path. `/Volumes` held exactly the same two
entries after both runs.

### Also verified, beyond the gate

- `gate_deletion`'s two local checks reproduced against each tree's construction
  record with the oracle running beside the port (`evidence/m4-local-deletion.txt`).
- The flag surface (`evidence/m4-flags.txt`) and hardlink handling
  (`evidence/m4-rules-and-hardlinks.txt`).
- The poison path is `m3-poison-refusal`, which is green.

## M5 — the interactive TUI — CLAIMED

The browser is back: the group list, the member paths, the keeper marking, the
selection, the bulk keep rules, the confirmation and the summary, in ratatui,
with the oracle's keymap and the oracle's information architecture. Deletion
goes through the machinery `--auto` uses and nothing else.

- Implementation commit: `df03dde8eb50a4dc870ae5f8a74e4b79caae7d56`
  (the browser in `020b73c`, the two guard fixes below in `df03dde`)
- Gate command (the TUI must not disturb the core), run from the repo root:

```
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/m5-all-gate.txt`, quoted from the file:

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

The command exited 0.

### One deletion path

`trash::run_selected` is the entry point the spec asks for: beside `run_auto`,
taking the same `ProvenGroups` token, whose field is private to `pipeline` and
which only `Scan::proven` can mint. A browser over a `--stage size` scan does
not type-check, and `main::browse` refuses one for the same reason `--auto`
does. The TUI constructs no removal call of its own — it hands
`run_selected` a list of marks and `run_selected` does the rest through
`verify_attested` and `remove_record`, unchanged.

The survivor guard inside that entry point does not trust the UI:

- A mark is `(group in the scan, record in the scan)`. Screen positions never
  leave `tui::model`, because a browser that has already deleted something
  renumbers what is left while the scan's own list does not — a stale position
  would name a *different file*, not a missing one.
- The whole request is validated before anything is removed, and a request that
  would leave any group with no copy is refused entire rather than partly
  applied.
- What the guard counts is copies that will still be on disk afterwards, not
  members the scan recorded. A group half-emptied by an earlier round has fewer
  survivors left than its membership suggests.
- A record pinned from outside the tree is never removed, however it came to be
  marked, exactly as in `plan_auto`.

Both of the first two properties are there because the scripted runs below
caught me getting them wrong; both bugs and their regression tests are written
up in LOG.md.

### End-to-end: the real binary, a real terminal, real trees

`evidence/scripts/m5_end_to_end.py` opens a pty, spawns the release binary
with `-i`, writes the bytes a keyboard would write, and then looks at the disk.
`HOME` points at a scratch directory so Trash placement is checked without
touching the user's own Trash.

```
python3 rust/evidence/scripts/m5_end_to_end.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5-end-to-end.txt`, quoted from the file:

```text
=== confirmed deletion moves exactly the marked copies to the Trash ===
    expect: a.bin and x.dat survive; the other three are in the scratch Trash
    exit: 0 (ok)
    survivors: ['a.bin', 'x.dat'] (ok)
    trash: ['b.bin', 'c.bin', 'y.dat'] (ok)

=== cancelling the confirmation removes nothing ===
    expect: all three files survive and the Trash is empty
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== quitting with copies marked removes nothing ===
    expect: all three files survive
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== --hard deletes permanently and puts nothing in the Trash ===
    expect: a.bin survives, b.bin is gone, the Trash stays empty
    exit: 0 (ok)
    survivors: ['a.bin'] (ok)
    trash: [] (ok)

=== space on every copy in a group cannot empty it ===
    expect: the last copy refuses to be marked, so one survives the confirmation
    exit: 0 (ok)
    survivors: ['c.bin'] (ok)
    trash: ['a.bin', 'b.bin'] (ok)
    refusal shown on screen: True (ok)

=== removing a hardlinked record removes every name it wears ===
    expect: both names of the cluster go together; the other copy survives
    exit: 0 (ok)
    survivors: ['z.bin'] (ok)
    trash: ['a.bin', 'also-a.bin'] (ok)

=== -i without a terminal refuses instead of guessing ===
    expect: exit 2, a message naming --list and --auto, and nothing removed
    exit: 2 (ok)
    stderr: dedup-rs: the interactive browser needs a terminal on stdin and stdout. Use --list to see the groups or --auto to clean up.
    survivors: ['a.bin', 'b.bin'] (ok)

0 check(s) differing
```

The command exited 0.

### The terminal comes back, on every way out

`evidence/scripts/m5_terminal_restore.py` reads the pty's own termios before,
during and after a session, and looks for the alternate-screen sequences in
what the program wrote.

```
python3 rust/evidence/scripts/m5_terminal_restore.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5-terminal-restore.txt`, quoted from the file:

```text
=== quitting with q ===
    modes before  (canonical, echo): (True, True)
    modes during  (canonical, echo): (False, False)
    modes after   (canonical, echo): (True, True)
    raw mode was entered: True
    raw mode was released: True
    alternate screen entered: True
    alternate screen left: True
    left after it entered: True

=== interrupting with ctrl-c ===
    modes before  (canonical, echo): (True, True)
    modes during  (canonical, echo): (False, False)
    modes after   (canonical, echo): (True, True)
    raw mode was entered: True
    raw mode was released: True
    alternate screen entered: True
    alternate screen left: True
    left after it entered: True

=== exiting because nothing is left to browse ===
    modes before  (canonical, echo): (True, True)
    modes during  (canonical, echo): (False, False)
    modes after   (canonical, echo): (True, True)
    raw mode was entered: True
    raw mode was released: True
    alternate screen entered: True
    alternate screen left: True
    left after it entered: True

0 check(s) differing
```

The command exited 0. The fourth path — a panic — is covered by the drop guard
and the panic hook rather than by this script, and the guard firing during
unwinding is unit-tested
(`the_guard_puts_the_terminal_back_on_every_way_out_of_its_scope`).

### What the screen looks like

`evidence/scripts/m5_screens.py` drives a session and reconstructs each frame
from the escape sequences the binary wrote, so the layout can be read rather
than inferred from a buffer assertion.

```
python3 rust/evidence/scripts/m5_screens.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5-screens.txt`; the opening frame, the refusal and the
keep-this-copy frame, quoted from the file:

```text
----- the group list as it opens; the second copy is pinned from outside -----
 dedup    …ts-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/m5-shots/m5-screens/tree
 ──────────────────────────────────────────────────────────────────────────────────────────
  2 groups · 1.1 KB reclaimable

  Group 1 of 2                                                           1.0 KB × 3 copies
 ──────────────────────────────────────────────────────────────────────────────────────────
   › ○ photos/beach.jpg                                                        2026-08-23
     ⊘ old/beach copy.jpg                                                      2026-08-23
     ○ backup/photos/beach.jpg                                                 2026-08-23










 ──────────────────────────────────────────────────────────────────────────────────────────
  ↑↓ file   ←→ group   space mark   a keep-this   u clear
  b bulk    ENTER apply   ? help    q quit

----- space on the pinned copy is refused, and says why -----
 dedup    …ts-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/m5-shots/m5-screens/tree
 ──────────────────────────────────────────────────────────────────────────────────────────
  2 groups · 1.1 KB reclaimable

  Group 1 of 2                                                           1.0 KB × 3 copies
 ──────────────────────────────────────────────────────────────────────────────────────────
     ○ photos/beach.jpg                                                        2026-08-23
   › ⊘ old/beach copy.jpg                                                      2026-08-23
     ○ backup/photos/beach.jpg                                                 2026-08-23










 ──────────────────────────────────────────────────────────────────────────────────────────
  hardlinked outside this tree — deleting it frees nothing
  b bulk    ENTER apply   ? help    q quit

----- a keeps this copy and marks the rest that can actually be freed -----
 dedup    …ts-tib/620866d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/m5-shots/m5-screens/tree
 ──────────────────────────────────────────────────────────────────────────────────────────
  2 groups · 1.1 KB reclaimable                                  1 marked · 1.0 KB to free

  Group 1 of 2                                                           1.0 KB × 3 copies
 ──────────────────────────────────────────────────────────────────────────────────────────
   › ○ photos/beach.jpg                                                        2026-08-23
     ⊘ old/beach copy.jpg                                                      2026-08-23
     ● backup/photos/beach.jpg                                                 2026-08-23










 ──────────────────────────────────────────────────────────────────────────────────────────
  ↑↓ file   ←→ group   space mark   a keep-this   u clear
  b bulk    ENTER apply   ? help    q quit
```

### Model and paint, separated and tested

`src/tui/model.rs` holds the state and every decision — cursor movement,
marking, the two refusals, the bulk rules, the rebuild after a round of
deletion — and imports nothing from a terminal. `src/tui/view.rs` reads it and
writes to a frame, and decides nothing. The spec's three named model cases are
`a_group_may_never_have_every_copy_marked`,
`a_hardlink_cluster_is_one_row_and_goes_as_one_unit` and
`a_record_pinned_from_outside_the_tree_is_not_offered`; the render tests
snapshot the whole drawn buffer for a seeded scan and assert that the paths are
visible and the keeper is marked, in shape and in colour.

Captured to `evidence/m5-standards.txt`, quoted from the file:

```text
$ cargo test

test result: ok. 101 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s


$ cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-m5/rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s

$ cargo fmt --check
(no output: clean)
```

### How a run reaches the browser, and what a piped run still does

Adding an interactive mode to a tool the harness drives as a subprocess is a
chance to change non-interactive behaviour by accident, so this is checked
rather than assumed. The browser opens on the oracle's own condition — a
terminal, and neither `--list` nor `--auto` — and `-i` asks for it outright,
which is the only case where a pipe is an error rather than a fall-through.
A piped run with no flags still writes its report and prints nothing, exactly
as before M5.

```
python3 rust/evidence/scripts/m5_dispatch.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5-dispatch.txt`, quoted from the file:

```text
=== no flags: the oracle's default is the browser ===
    terminal: True   flags: (none)
    exit: 0   entered the browser: True
    said: dedup…d6-546b-4ac1-8468-9ebe21560ca6/scratchpad/m5-dispatch/m5-dispatch/case-0/tree─────────────

=== --list: prints the groups and stops ===
    terminal: True   flags: ['--list']
    exit: 0   entered the browser: False
    said: 1 duplicate group(s) keep a.bin b.bin

=== nothing duplicated: says so and stops ===
    terminal: True   flags: (none)
    exit: 0   entered the browser: False
    said: No duplicates found.

=== -i with nothing duplicated: not an error ===
    terminal: True   flags: ['-i']
    exit: 0   entered the browser: False
    said: No duplicates found.

=== no flags through a pipe: exactly as before M5 ===
    terminal: False   flags: (none)
    exit: 0   entered the browser: False
    said: 

=== --list through a pipe: exactly as before M5 ===
    terminal: False   flags: ['--list']
    exit: 0   entered the browser: False
    said: 1 duplicate group(s) keep a.bin b.bin

=== -i through a pipe: refuses, and says what to use ===
    terminal: False   flags: ['-i']
    exit: 2   entered the browser: False
    said: dedup-rs: the interactive browser needs a terminal on stdin and stdout. Use --list to see the gr

every case left both files where they were
```

The command exited 0.

### Fence conditions this run was made under

`ls /Volumes` immediately before and after the gate run: `dedupbench`, which is
another project's and which I never touched, and the stray `]` entry that
appears in no mount table. No drive named "My New Passport" and no unexpected
external volume at any point; no command of mine named a `/Volumes` path.
Every tree the browser deleted from was generated by these scripts under this
session's scratchpad with an `m5-` prefix, and every run pointed `HOME` at a
scratch directory, so nothing reached the real Trash.

### Not in this milestone

`getattrlistbulk(2)`. LOG.md had named M5 as the milestone that would land it;
M5-TUI.md scopes this run to the browser, so it is re-deferred to a dedicated
performance pass, with the reasoning in LOG.md rather than left unsaid.

## M5 — the interactive TUI — RE-CLAIMED after REVIEW-M5 CHANGES

Both binding items fixed, both verified the way REVIEW-M5 said it would verify
them, and the whole gate still green. The review's root-cause framing is right
and is what the fixes are shaped around: each item was a place where the
browser's *view* of the world reached a decision — a string built for drawing
ranked the copies, and the paint layer's early return decided whether a safety
dialog existed.

- Fix commit: `612d3dd541f639a4ec2ae87fc4da9715b7eb8baa`
- Gate command, run from the repo root:

```
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/m5-all-gate.txt`, quoted from the file:

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

The command exited 0.

### 1. The keep rules rank the scan's own name

`Member` now carries two strings and says which is which: `name`, the scan's
name relative to the root — the one `--auto` ranks — and `path`, what to draw.
`apply_bulk` passes `name` to `Rule::keeper_of`. Nothing but the painter reads
`path`, and the two comments that claimed the rule could not diverge (they were
false when written) now say what is actually true, in `trash.rs` and
`model.rs`.

The reviewer's own reproduction, now run as end-to-end case 7 — the same tree
built twice and cleaned two ways from the same directory inside it:

```text
=== bulk keep-shortest keeps the copy --auto shortest keeps ===
    expect: b.bin survives both, even run from inside the tree where the drawn paths rank the other way round
    exit: 0 (ok)
    browser survivors: ['b.bin'] (ok)
    --auto survivors: ['b.bin'] (ok)
    browser and --auto agree: ['b.bin'] (ok)
```

Plus the model test REVIEW-M5 specified, over a group whose display order and
record order disagree: `a_bulk_rule_ranks_the_scans_name_not_the_one_on_screen`
asserts `apply_bulk(Rule::Shortest)` marks the record-order non-keeper, and
that swapping only the display strings changes nothing about which copy dies.
`keep_this_one_and_the_survivor_guard_do_not_read_the_display_string` covers
the two other ways a copy gets marked on that same group.

### 2. A screen nobody can read takes no orders but quit

`MIN_WIDTH`/`MIN_HEIGHT` moved into `model.rs`, because the moment they decide
what the browser will act on they are not only a layout number; the paint layer
reads them from there so the two cannot drift. The model carries `legible`, set
from the area of the frame that was actually painted, and below the minimum it
returns `Step::Stay` for every key but `Quit`.

REVIEW-M5 allowed drawing a clipped overlay instead. I did not take it: at 20×8
a box cannot fit "Move to Trash: 1 files", so that option would draw *a* dialog
while the numbers hard rule 3 exists to show stayed invisible.

End-to-end case 8, on the reviewer's own 20×8 pty with `space`, ENTER, `y`:

```text
=== a terminal too small to lay out will not confirm a deletion ===
    expect: both copies survive; the frame says to resize rather than asking y/n
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin'] (ok)
    trash: [] (ok)
    painted the resize message: True
    never painted a confirmation: True
```

Model tests `a_screen_too_small_to_read_takes_no_orders_but_quit` (including
the frame shrinking *underneath* a live confirmation, and the confirmation
coming back when it grows again) and
`the_legibility_floor_is_the_one_the_paint_layer_uses`.

### The re-read of a cache-attested group, now tested

REVIEW-M5's sharpest non-binding note: nothing anywhere exercised
`run_selected`'s `verify_attested` call, and deleting it left everything green.
End-to-end case 9 closes it — two 24 MB copies, the cache warmed, then one byte
changed in the *middle* and the mtime restored, so head, tail and every sample
point still agree and only a full re-read can tell:

```text
=== a group the cache vouched for is re-read, disbelieved and refused ===
    expect: the scan reads nothing and still groups them; the browser reads them, removes nothing, and says why
    cold scan exit: 0 (ok)
    exit: 0 (ok)
    the scan still grouped them: 1 (ok)
    the scan read 0 bytes to do it
    grouped without reading them: True
    survivors: ['a.bin', 'b.bin'] (ok)
    trash: [] (ok)
    said it was refusing, and why: True
```

The 0 bytes is the proof the group was believed rather than seen. **I checked
the test can fail**: with the `verify_attested` call removed it reports three
differing checks and sends `b.bin` — the copy that no longer matches — to the
Trash, while all 105 unit tests and the whole gate stay green, which is exactly
the hole the reviewer described.

### All ten browser scenarios

```
python3 rust/evidence/scripts/m5_end_to_end.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5-end-to-end.txt`, quoted from the file:

```text
=== confirmed deletion moves exactly the marked copies to the Trash ===
    expect: a.bin and x.dat survive; the other three are in the scratch Trash
    exit: 0 (ok)
    survivors: ['a.bin', 'x.dat'] (ok)
    trash: ['b.bin', 'c.bin', 'y.dat'] (ok)

=== cancelling the confirmation removes nothing ===
    expect: all three files survive and the Trash is empty
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== quitting with copies marked removes nothing ===
    expect: all three files survive
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== --hard deletes permanently and puts nothing in the Trash ===
    expect: a.bin survives, b.bin is gone, the Trash stays empty
    exit: 0 (ok)
    survivors: ['a.bin'] (ok)
    trash: [] (ok)

=== space on every copy in a group cannot empty it ===
    expect: the last copy refuses to be marked, so one survives the confirmation
    exit: 0 (ok)
    survivors: ['c.bin'] (ok)
    trash: ['a.bin', 'b.bin'] (ok)
    refusal shown on screen: True (ok)

=== removing a hardlinked record removes every name it wears ===
    expect: both names of the cluster go together; the other copy survives
    exit: 0 (ok)
    survivors: ['z.bin'] (ok)
    trash: ['a.bin', 'also-a.bin'] (ok)

=== bulk keep-shortest keeps the copy --auto shortest keeps ===
    expect: b.bin survives both, even run from inside the tree where the drawn paths rank the other way round
    exit: 0 (ok)
    browser survivors: ['b.bin'] (ok)
    --auto survivors: ['b.bin'] (ok)
    browser and --auto agree: ['b.bin'] (ok)

=== a terminal too small to lay out will not confirm a deletion ===
    expect: both copies survive; the frame says to resize rather than asking y/n
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin'] (ok)
    trash: [] (ok)
    painted the resize message: True
    never painted a confirmation: True

=== a group the cache vouched for is re-read, disbelieved and refused ===
    expect: the scan reads nothing and still groups them; the browser reads them, removes nothing, and says why
    cold scan exit: 0 (ok)
    exit: 0 (ok)
    the scan still grouped them: 1 (ok)
    the scan read 0 bytes to do it
    grouped without reading them: True
    survivors: ['a.bin', 'b.bin'] (ok)
    trash: [] (ok)
    said it was refusing, and why: True

=== -i without a terminal refuses instead of guessing ===
    expect: exit 2, a message naming --list and --auto, and nothing removed
    exit: 2 (ok)
    stderr: dedup-rs: the interactive browser needs a terminal on stdin and stdout. Use --list to see the groups or --auto to clean up.
    survivors: ['a.bin', 'b.bin'] (ok)

0 check(s) differing
```

The command exited 0.

### Standards

`evidence/m5-standards.txt` now has a generator beside it like the other four,
which was the last non-binding note:

```
cd rust && evidence/scripts/m5_standards.sh > evidence/m5-standards.txt
```

Quoted from the file:

```text
$ cargo test

test result: ok. 105 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.34s


$ cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-m5/rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.61s

$ cargo fmt --check
(no output: clean)
```

### The other non-binding notes

- A mark is a `Mark { group, record }` rather than a bare pair. This does not
  make a screen position unrepresentable — only a distinct type for record
  indices would, which is wider than a fix round should carry — but the two
  fields are both `usize`, nothing but their order distinguished them, and
  swapping them was a live failure mode.
- The second `put_the_terminal_back` in `run` is gone; the drop guard covers
  every path it covered, and two mechanisms for one guarantee only make a
  reader wonder which is load-bearing.
- The confirmation-counts-records against summary-counts-names wrinkle is
  recorded in LOG.md as inherited parity, so the next reader does not
  "fix" it into a divergence.

`evidence/m5-screens.txt` came back byte-identical, which REVIEW-M5 expected to
move. It did not, because that script runs the browser with its working
directory at the scanned root — where the display name and the scan name are
the same string — and because it only opens the bulk menu to photograph it and
leaves with `esc` without applying a rule. It was regenerated from the fixed
binary regardless.

### Fence conditions this round was made under

`ls /Volumes` immediately before and after the gate run: `dedupbench`, another
project's, and the stray `]` entry that appears in no mount table. No command
of mine named a `/Volumes` path. Every tree deleted from was generated under
this session's scratchpad with an `m5-` prefix, with `HOME` pointed at a
scratch directory, so nothing reached the real Trash.


## Whole-project gate — `--gate all`

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate all \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/all-gate.txt`, quoted from the file:

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

The command exited 0. **33 checks, 0 failed, across all four milestones.**

The `{stage}` token is in this template and only this one, for the reason set
out under the M2 claim: the M1 gate grades the size-bucket candidate set, which
a finished pipeline no longer reports by default. Each individual milestone
claim above uses the prescribed template without it.

---

## M6 — the spinning profile — CLAIMED

`--profile flash|spinning|auto`, default `auto`. `auto` picks `spinning` only
for a disk that positively reports rotational media; solid state and "no idea"
both get `flash`, which is today's behaviour byte for byte. `flash` is
untouched, and the gate below is run with no profile flag at all, which is the
proof of that.

Under `spinning` the pipeline drops the tail probe (the end stage reads the
head alone), drops the sample stage entirely, keeps the comparison run at
`SLOW_CHUNK` however wide the group, reads with one thread, and compares groups
wider than `SPIN_INTERLEAVE` (8) a cohort at a time, carrying the proved
partition between passes. Members are read more than once by design.

**HDD wall-clock unvalidated.** No rotational drive is available to an agent —
the only one on this machine is fenced — so no speed claim is made here. What
is claimed is correctness, which is proved against the oracle, and the access
pattern, which is counted. A user-supervised run on a real spinning disk is
still owed.

### The gate, with no profile flag

Run from the repository root:

```
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/m6-gate.txt`, quoted from the file:

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

The command exited 0. Nothing in the harness passes `--profile`, so every one
of those 33 checks ran the flash pipeline exactly as M5 left it.

### Groups are profile-independent

```
python3 rust/evidence/scripts/spinning_differential.py
```

Eight scratch trees, each shaped at something the profile removes — the tail
page it no longer probes, the middle it no longer samples, a divergence in the
last kilobyte, equal members either side of a cohort boundary, a group three
times the cohort width, hardlink clusters, unreadable members, and a seeded
jumble across every size class. On each tree the oracle, `--profile flash`,
`--profile spinning` and a flagless run are compared. Captured to
`evidence/m6-differential.txt`:

```text
tree                  files  groups   flash seeks  spinning seeks   flash bytes  spinning bytes  verdict
tails                     5       2            18              19        319488          389120  same
middles                   3       1            40              11      31002624        31457280  same
last-chunk                3       1            30              14      38805504        56623104  same
cohort-boundary          16       4            64              72      33554432        49807360  same
wide                     24       1           336             102     239075328       254607360  same
links                     6       2            15              16       1048576         1048576  same
unreadable                5       1            39              12      29884416        28311552  same
mixed                    46      10           102              83      20717575        45826056  same

8 trees, every profile identical to the oracle
```

### Seek accounting

```
python3 rust/evidence/scripts/spinning_seeks.py
```

`seeks` counts reads that did not continue where the previous read stopped. It
is incremented at the read, from the offsets the process asked for, and never
derived from a file's size — the rule `opens` and `bytes_read` are already
held to. It rides in the `--report-json` object beside them; the conformance
protocol neither asks for it nor reads it.

Every measured run is at `-j 1`: concurrent readers interleave, and that
interleaving belongs to the scheduler rather than to the pipeline. Captured to
`evidence/m6-seeks.txt`:

```text
One group of N identical files, one reader (-j 1).

files    MiB   flash seeks   spinning seeks   ratio      flash MiB   spinning MiB   groups
    4     20            64               20     3.2x        82.0           80.0        1
    8     20           128               40     3.2x       164.0          160.0        1
   16     20           256               86     3.0x       328.0          359.9        1
   32     20           512              172     3.0x       656.0          719.8        1
    6    100           216               60     3.6x       603.0          600.0        1

Same corpora at the default reader count, for scale only (threads interleave, so these move between runs):
files    MiB   flash seeks   spinning seeks
    4     20            59               20
    8     20           123               40
   16     20           253               86
   32     20           512              172
    6    100           192               60

spinning issues fewer seeks on every corpus: between 3.0x and 3.6x fewer
HDD wall-clock unvalidated: no rotational drive is available here.
```

The bytes column is the price: at 32 copies, 10% more bytes read for a third of
the seeks. That is the trade SPIN.md asks for by name.

### What the counter costs the profile that is scored

The counter takes a lock on every read block, and `read_block` is the default
profile's hot path. So the default profile was run against a build of `main` at
`baf9e25` on the same tree:

```
python3 rust/evidence/scripts/seek_counter_cost.py \
  <main-build> rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m6-counter-cost.txt`:

```text
corpus: 156 MiB, 4012 files
load average: 5.14 5.49 5.26  (this machine is shared; a figure above about 1 makes the timings below indicative only)

-j 1   (7 runs each, page cache warm, medians)
  main            92.4 ms  (min 90.4, max 113.9)   bytes_read 170599168  opens 4048  groups 10
  this branch     91.2 ms  (min 90.1, max 92.9)   bytes_read 170599168  opens 4048  groups 10
  -> this branch / main = 0.987x wall clock (indicative); bytes_read, opens, files_seen, bytes_seen and the groups all identical (exact)

-j 8   (7 runs each, page cache warm, medians)
  main            61.5 ms  (min 59.0, max 68.3)   bytes_read 170599168  opens 4048  groups 10
  this branch     58.2 ms  (min 57.6, max 59.8)   bytes_read 170599168  opens 4048  groups 10
  -> this branch / main = 0.946x wall clock (indicative); bytes_read, opens, files_seen, bytes_seen and the groups all identical (exact)

nothing the harness measures moved
```

`bytes_read`, `opens`, `files_seen`, `bytes_seen` and the groups are identical
between the two builds — that is the part of this that is a fact, and it is
what RBENCH's byte-determinism gate and the conformance effort check read.

The wall clock is not claimed. This machine is shared with other agents and was
at load average 5.1 throughout; an earlier run of the same script reported
0.18x and 1.29x on the two halves of one comparison, which is contention and
nothing else. Four runs at a load this reader can see put it between 0.92x and
1.03x — i.e. no effect anybody can measure here — and a real figure wants a
quiet machine.

### Where `--profile` meets the other flags

REVIEW-M4's first finding was a mode that changed what a group meant, in a
combination no gate ran. `--profile` changes which bytes a digest covers, so
the combinations were run rather than reasoned about:

```
python3 rust/evidence/scripts/spinning_meets_the_rest.py
```

```text
cache: flash first, then spinning          ok (1 groups, the new copy joined its twins)
cache: spinning first, then flash          ok (1 groups, the new copy joined its twins)
cache: spinning twice                      ok (1 groups, the new copy joined its twins)
--auto under either profile                ok (both kept a.bin, c.bin, d.bin)
the browser under either profile           ok (both removed b.bin, nested/a-copy.bin and kept a.bin, c.bin, d.bin)
--stage size --auto --profile spinning     ok (exit 2, nothing removed): dedup-rs: --auto cannot act on a --stage size scan
--profile with a name it does not know     ok (exit 2): dedup-rs: --profile takes flash, spinning or auto

every combination behaves
```

The cache case is the one that matters. The `spinning` end probe reads the head
alone; the `flash` one reads both ends. Those digests cover different bytes, so
they get different cache columns (`CACHE_SCHEMA` 4 adds `head`). Sharing one
column would let a scan compare a remembered head-and-tail against a freshly
read head, and two identical files would land in different buckets — a
duplicate lost, silently, with the gate still green.

### The tests are guards, not decoration

```
python3 rust/evidence/scripts/spinning_mutants.py
```

Six deliberate breaks, each one a plausible implementation of the same feature,
built and run. Captured to `evidence/m6-mutants.txt`:

```text
mutant cohort-width               reads SPIN_INTERLEAVE + 1 files at once instead of SPIN_INTERLEAVE
  caught by the_cohorts_never_hold_more_than_the_cap_open_at_once

mutant drop-leftovers             never offers a non-matching member to a later pivot
  caught by a_group_taking_several_rounds_is_partitioned_the_same_way, a_member_that_cannot_be_read_joins_no_group_and_stops_nobody_else, a_pivot_alone_and_unreadable_leaves_nothing_behind, a_pivot_that_goes_away_mid_sweep_settles_nothing_and_goes_back_in, a_run_holding_the_pivot_brings_whole_clusters_with_it, equal_members_split_across_cohorts_are_still_found, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape

mutant forget-the-partition       carries only each cluster's representative, losing what a pass proved
  caught by a_group_taking_several_rounds_is_partitioned_the_same_way, a_member_that_cannot_be_read_joins_no_group_and_stops_nobody_else, a_run_holding_the_pivot_brings_whole_clusters_with_it, equal_members_split_across_cohorts_are_still_found, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape

mutant settle-on-a-lost-pivot     settles the pivot's class even though a cohort could not read it
  caught by a_pivot_that_goes_away_mid_sweep_settles_nothing_and_goes_back_in

mutant strand-a-dead-cluster      drops the other members of a cluster whose representative went away
  caught by a_cluster_whose_representative_goes_away_keeps_its_other_members

mutant merge-on-the-first-cohort  stops sweeping once the pivot has matched somebody
  caught by a_group_taking_several_rounds_is_partitioned_the_same_way, a_group_wider_than_the_cap_still_comes_back_whole, a_member_that_cannot_be_read_joins_no_group_and_stops_nobody_else, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape, the_cohorts_never_hold_more_than_the_cap_open_at_once

Baseline: the same tree, built and run through spinning_meets_the_rest.py.
  every combination behaves

mutant one-cache-column           files the head-only probe and the two-ends probe in the same column
  caught: warm flash then spinning reported [('a.bin', 'b.bin')], oracle [('a.bin', 'b.bin', 'e.bin')]

every mutant was caught
```

The last one is the cache column, and its symptom is the exact silent data loss
described above.

### Standards

Both milestones go to one review, so this is the branch tip and its test count
includes M5.1's. At the M6 implementation commit alone it read 131.

```text
$ cargo test
test sys::terminal::a_live_terminal_with_something_typed_at_it_is_ready ... ok

test result: ok. 159 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s


$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.06s

$ cargo fmt --check
(no output: clean)
```

---

## M5.1 — the pre-flight menu, and --help — CLAIMED

`--help` and `-h` print real usage and exit 0; they used to exit 2 with
"unknown argument". An interactive launch gets a form of the tool's real
options before the scan starts. Both read one table.

### One table

`rust/src/cli.rs` holds `FLAGS`. The parser walks it to decide what each
argument means, `help_text()` renders it, and the menu asks it for its fields,
their prompts and their defaults. No flag is spelled twice anywhere, and a word
becomes a setting in exactly one function — `Flag::apply` — so the menu
validating a field and the parser reading an argument are the same code.

```
$ dedup-rs --help
```

Captured to `evidence/m5_1-help.txt`, quoted from the file:

```text
dedup-rs — find byte-exact duplicate files.

usage: dedup-rs <root> [options]

Groups of files whose bytes are identical, largest first. Nothing is
ever called a duplicate on the strength of a hash: every group is
read and compared. Without --list or --auto, a terminal gets the
interactive browser.

options:
      --min-size BYTES    ignore files smaller than this
                          (default: 10485760 bytes (10 MiB))
      --ext LIST          only these extensions, comma-separated; repeatable
                          (default: every extension)
      --skip-ext LIST     skip these extensions, comma-separated; repeatable
                          (default: skip nothing)
  -x, --exclude GLOB      skip paths matching this glob; repeatable
                          (default: exclude nothing)
  -j, --jobs N            readers to use
                          (default: as many as the disk is worth)
      --profile NAME      access patterns to use: flash, spinning or auto
                          (default: auto)
      --no-cache          read everything, remembering nothing
                          (default: use the scan cache)
  -l, --list              print the duplicate groups and stop
                          (default: browse them, given a terminal)
  -i, --interactive       browse them, and fail if there is no terminal
                          (default: browse only when a terminal is there)
      --auto [RULE]       delete without asking, keeping shortest, oldest or
                          newest
                          (default: delete nothing)
  -y, --yes               do not ask before deleting
                          (default: ask)
      --hard              delete permanently instead of moving to the Trash
                          (default: move to the Trash)
      --report-json PATH  write the scan report here
                          (default: a file in the temporary directory)
      --stage NAME        size, to stop after grouping by length and report
                          that; nothing there has been read, so --auto refuses
                          it
                          (default: full)
  -q, --quiet             accepted for the Python tool's sake; ignored here
                          (default: ignored either way)
      --verify            accepted for the Python tool's sake; ignored here,
                          which re-proves every group it deletes from anyway
                          (default: ignored either way)
  -h, --help              print this and stop
exit=0
```

### The rule, enforced

`cli::tests::the_help_text_and_the_parser_know_exactly_the_same_flags` scrapes
every `-x`-shaped token out of the rendered help and asserts the set equals the
table's, in both directions;
`the_defaults_the_help_text_prints_are_the_defaults_the_code_has` compares each
printed default against what a bare command line actually produces.

A golden test that passes proves nothing on its own, so each way of breaking
the rule was built and run:

```
python3 rust/evidence/scripts/menu_mutants.py
```

Captured to `evidence/m5_1-mutants.txt`:

```text
mutant help-misses-a-flag           renders every flag but the first
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags, the_help_text_says_something_about_every_flag

mutant help-invents-a-flag          hand-writes a line for a flag the parser does not accept
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags

mutant help-default-drifts          says a default the code does not have
  caught by the_defaults_the_help_text_prints_are_the_defaults_the_code_has, clearing_a_field_puts_the_default_back

mutant menu-writes-its-own-default  shows a default of its own instead of the table's
  caught by clearing_a_field_puts_the_default_back, the_form_shows_the_root_every_field_and_every_default

mutant menu-drops-a-field           leaves a field out of the argument list it hands back
  caught by a_switch_toggles_and_reaches_the_options, the_command_line_prefills_the_fields_it_names

mutant menu-skips-validation        accepts whatever is typed and lets the scan find out
  caught by a_bad_profile_is_refused_the_same_way, a_field_is_refused_in_the_field_with_the_parsers_own_words, a_refusal_is_shown_beside_the_field_it_belongs_to

every mutant was caught
```

### The menu

It appears under exactly the browser's own condition — a terminal, no `--auto`,
and either `-i` or no `--list` — checked by `main::browsing`, which is the same
question `main::browse` asks after the scan. A piped or flagged run never
reaches it.

What it hands back is a command line. `main` reads that command line with
`parse_arguments_from`, the same parser it read the real one with, so the form
cannot say anything the flags cannot say and cannot quietly drop a flag it does
not show — anything outside its seven fields rides through untouched. A field
is checked by `Flag::apply` on a scratch `Options`, so it is refused in the
field, in the parser's own words.

Twelve model tests and six `TestBackend` snapshots cover it without a terminal.
End to end, through a real pty against the real binary:

```
python3 rust/evidence/scripts/m5_1_menu_end_to_end.py \
  rust/target/release/dedup-rs <scratch-dir>
```

Captured to `evidence/m5_1-end-to-end.txt`:

```text
--help exits 0 and lists the flags       ok (exit 0, 47 lines, -h identical)
a piped run never sees the menu          ok (2 groups printed, no menu in the output)
enter-through equals the piped run       ok (2 groups, identical to the piped run)
enter-through with no flags at all       ok (('huge-a.bin', 'huge-b.bin'), the 10 MiB default honoured on both sides)
typing a min-size changes the groups     ok (1 group; the 4 KiB pair was filtered out by the typed value)
space toggles the cache switch           ok (cold 4206592 bytes, warm 12288, switch on 4206592 — the toggle reached the options)
esc scans nothing                        ok (exit 0, no report, nothing read)

every case behaves
```

The cache switch is asserted on the bytes the scan read rather than on the
screen: ratatui repaints only the cells that changed, so a phrase drawn into a
later frame never reaches the pty whole — the harness mistake REVIEW-M5 caught
in its own tooling before it caught anything in the program.

### Too small to read is too small to agree to

REVIEW-M5's second finding was a confirmation dialog that was never drawn and
still acted on. Nothing in the menu deletes — it runs before a scan exists —
but starting a scan on settings nobody could see is the same mistake, so below
54x16 the form draws a resize line and `Form::on_key` refuses everything but
the way out. `the_paint_and_the_model_agree_on_what_is_too_small_to_read`
renders at both sides of each boundary and asserts the drawing layer and the
model reach the same verdict, which is what stops the two rules drifting apart.

### The gate

Conformance templates are non-interactive, so the menu never appears in one.

```
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Captured to `evidence/m5_1-gate.txt`:

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

The command exited 0.

### Standards

```text
$ cargo test
test sys::terminal::a_live_terminal_with_something_typed_at_it_is_ready ... ok

test result: ok. 159 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s


$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.06s

$ cargo fmt --check
(no output: clean)
```
