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

**Where the win is, and where it is not.** These ratios belong to the corpus
SPIN.md commissioned the profile for: wide groups of large files, which is what
a USB disk full of media looks like and what the real scan that prompted this
was stuck on. On small-file trees `spinning` issues slightly MORE seeks than
`flash`, because re-reading a pivot costs more than the skipped tail probes
save — the differential table above shows it (`tails` 18 → 19,
`cohort-boundary` 64 → 72). Read "3.0x-3.6x fewer" as a statement about large
wide groups, not about every tree.

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

test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s


$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s

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

Strictly there are two tables, not one: `HELP_FLAGS` sits beside `FLAGS` and is
matched by hand before the `FLAGS` lookup, because `--help` has to win before a
missing root can be an error. It is not a drift hole — `help_text()` renders it
from that same constant and the golden test chains it into the set it diffs —
but "a single source of flag truth" is a sentence about `FLAGS`, and this is
the exception to it.

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

Captured to `evidence/m5_1-mutants.txt`, the six that attack the table (the
same script's two terminal-death mutants are quoted under the re-claim below):

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

test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s


$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s

$ cargo fmt --check
(no output: clean)
```

---

## M5.1 — RE-CLAIMED after REVIEW-M5_1.md

Binding item 1 is fixed. It was two defects in the same eight lines, and the
review found the first; running the fix down found the second.

**The spin.** `menu.rs`'s loop read one of `wait_for_terminal_input`'s three
answers. On `Waited` — nothing typed inside 250 ms, which is the ordinary state
of a form somebody is reading — it fell through to a blocking `event::read()`,
and from there the hangup check was unreachable. A terminal that dies while the
process sits in that read is readable forever and empty forever, and crossterm
answers by looping on `read` inside `try_read`: no event, no error, one core at
100%. The browser has consumed all three answers since `09599c6`; the menu,
added one screen in front of it, did not.

Reproduced before it was touched, with the review's own deterministic driver
(kept as `evidence/scripts/hangup_deterministic.py`; the delay before the
terminal dies is a parameter, so the process is provably past the poll). Both
builds identified by sha256, because a shared `CARGO_TARGET_DIR` will otherwise
hand you the wrong binary and a clean result you did not earn. Captured to `evidence/m5_1-hangup-before-and-after.txt`:

```text
b102625 (the claimed tip REVIEW-M5_1 read), sha256 a78699545e1f789d:
$ python3 rust/evidence/scripts/hangup_deterministic.py <b102625 build> 1.5 3
  run 1: SPINNING(menu, 5.03 CPU-s in 5s)
  run 2: SPINNING(menu, 5.04 CPU-s in 5s)
  run 3: SPINNING(menu, 5.06 CPU-s in 5s)
SPUN 3/3
exit=1

and the same script against this branch, sha256 d7002edcf24a6b5c:
$ python3 rust/evidence/scripts/hangup_deterministic.py rust/target/release/dedup-rs 1.5 3
  run 1: LEFT(menu, exit=1, 0.01 CPU-s)
  run 2: LEFT(menu, exit=1, 0.01 CPU-s)
  run 3: LEFT(menu, exit=1, 0.01 CPU-s)
SPUN 0/3
exit=0
```

The fix is the browser's own `match`, all three arms, so `event::read()` is
entered only when poll has just said bytes are waiting.

**The panic on the way out.** The review's step 2 asked why its patched build
left with 101 where the browser leaves with 1. It is ratatui's `Terminal::drop`,
which shows the cursor and `eprintln!`s when it cannot — onto the descriptor
that just died. The browser already carries the answer (`std::mem::forget` on
the terminal-gone path) and the menu did not. Confirmed by building the
in-between version rather than reasoning about it: three arms and no
`mem::forget` leaves with 101 on every run; with it, 1.

`ask` now returns `Answer::Scan | Quit | TerminalGone`, and `main` gives
`TerminalGone` exit 1 — the same ending the browser gives the same event.

### The regression test, and what it was missing

`tui-terminal-death.py` now runs four scenarios instead of two, each of which
**asserts which screen it reached**:

```text
  keys/menu        PASS  'q' left the menu in 0.64s, exit 0
  keys/browser     PASS  'q' left the browser in 0.64s, exit 0
  hangup/menu      PASS  the menu left 0.05s after the terminal died, exit code 1, 0.00 CPU-seconds
  hangup/browser   PASS  the browser left 0.05s after the terminal died, exit code 1, 0.00 CPU-seconds

all scenarios passed
```

Three changes, all of them things the old script's shape allowed to go wrong:

- **A fixed delay past the poll** before the terminal is taken away. Closing
  the master as soon as the paint pauses lands inside the 250 ms window about
  half the time, which is why the review warns not to read one green run as a
  fix.
- **Each screen is driven and named separately.** This is the coverage that
  quietly disappeared when M5.1 landed: `-i` used to reach the browser, so the
  hangup case tested the browser; after the menu went in front, the same script
  tested the menu instead and never said so. A test that does not assert which
  screen it is on stops covering the screen it was written for the moment
  somebody adds another.
- **A panic exit is a failure**, not a pass. Leaving is not enough; leaving on
  purpose is the requirement.

The `keys` cases are kept and extended to both screens, because a view that
ignored input altogether would also not spin and would pass a hangup test on
its own.

### Both halves of the fix are guarded

`menu_mutants.py` gained the two whole-program mutants, built and driven
through a pty. Each is caught, each with its own symptom, and each **only on
the menu** — the browser stays green, which is the per-screen pinning working:

```text
mutant one-of-three-answers         enters the blocking read when poll said nothing was typed
  caught: hangup/menu      FAIL  the menu was still running 5s after the terminal died, 5.00 CPU-seconds in that window — SPINNING

mutant drop-the-dead-terminal       lets ratatui print to a descriptor that is gone, panicking on the way out
  caught: hangup/menu      FAIL  the menu left in 0.05s but by panicking (exit 101)

every mutant was caught
```

### Standards and the gate

```text
$ cargo test
test sys::terminal::a_live_terminal_with_something_typed_at_it_is_ready ... ok

test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s


$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s

$ cargo fmt --check
(no output: clean)
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

The end-to-end pty suite is unchanged and still passes all seven cases
(`evidence/m5_1-end-to-end.txt`).

### Non-binding notes taken

- The M6 claim now says where the seek win lives — wide groups of large files —
  and that small-file trees seek slightly more, which its own differential table
  already showed.
- `LOG.md`'s three stale prose numbers are corrected: 158 and 159 tests to 163,
  and "six mutants" to seven for M6's script. The evidence files were right
  throughout; only the prose had drifted.
- The one-table section now says plainly that there are strictly two tables,
  `HELP_FLAGS` beside `FLAGS`, and why that is not a drift hole.

## M5.2 — sizes for humans, and a live pulse — CLAIMED

Branch `m5_2-ux`, off `main@fbc196d`. Binding spec is `M5_2-UX.md`; M5-TUI.md
and M5_1-MENU.md apply unchanged, and nothing here touches the deletion path —
the size grammar runs before any scan exists and the pulse only reads counters.

Gate command, unchanged from the M5.1 and M6 claims, `{stage}` and all:

```text
$ python3 conformance/run.py --gate all \
    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

### Sizes for humans

`rust/src/size.rs` holds the grammar, and `Flag::apply` is its only caller, so
the command line, the pre-flight form and `--help` are three doors onto one
function. Plain digits stay plain bytes — the conformance templates are written
in them and none of this moves them. A suffix multiplies: `k`, `m`, `g`, `t`,
case-insensitive, alone or dressed as `kb` or `kib`, at most one space in
front, decimals floored to whole bytes, all multiples binary. `p` and `e` are
read too, though nothing teaches them, so that the whole ladder the browser
prints is a ladder the tool can be told.

On the real binary, `evidence/m5_2-sizes.txt`:

```text
$ dedup-rs <tree> --list --min-size '3145728'              exit=0  1 duplicate group(s)
$ dedup-rs <tree> --list --min-size '3m'                   exit=0  1 duplicate group(s)
$ dedup-rs <tree> --list --min-size '3mb'                  exit=0  1 duplicate group(s)
$ dedup-rs <tree> --list --min-size '3MiB'                 exit=0  1 duplicate group(s)
$ dedup-rs <tree> --list --min-size '2.9g'                 exit=0  No duplicates found.
$ dedup-rs <tree> --list --min-size '512 k'                exit=0  1 duplicate group(s)
$ dedup-rs <tree> --list --min-size '0'                    exit=0  2 duplicate group(s)
$ dedup-rs <tree> --list --min-size '12qb'                 exit=2  dedup-rs: not a size: "12qb" — plain bytes or 512k, 100mb, 1.5g
$ dedup-rs <tree> --list --min-size '-1'                   exit=2  dedup-rs: not a size: "-1" — plain bytes or 512k, 100mb, 1.5g
$ dedup-rs <tree> --list --min-size '1.2.3'                exit=2  dedup-rs: not a size: "1.2.3" — plain bytes or 512k, 100mb, 1.5g
$ dedup-rs <tree> --list --min-size '18446744073709551616' exit=2  dedup-rs: not a size: "18446744073709551616" — plain bytes or 512k, 100mb, 1.5g
$ dedup-rs <tree> --list --min-size '1€'                   exit=2  dedup-rs: not a size: "1€" — plain bytes or 512k, 100mb, 1.5g
```

That last line is a bug I put in and found reading my own diff. The suffix was
split with `split_at(1)` — one *byte* — so `--min-size 1€` panicked inside
`core::str` instead of being refused. A flag value is whatever came off the
command line, and a program that deletes files may refuse but may not fall
over. It splits by character now, and
`size::tests::nothing_a_person_can_type_makes_the_parser_fall_over` throws
twenty thousand words built from digits, dots, signs, spaces, unit letters,
NUL, Arabic-Indic and fullwidth digits, `€` and an emoji at it.

The `--help` line, from `evidence/m5_2-help.txt`:

```text
      --min-size SIZE     ignore files smaller than this; plain digits are
                          bytes, or say 512k, 100mb, 1.5g, 2TiB — all binary
                          multiples
                          (default: 10 MiB)
```

**The display round-trip law, and where it made me split the humaniser in
two.** M5_2-UX.md asks for one humaniser, shared with the TUI's `human_size`,
and for every displayed size to parse back to exactly the bytes it came from.
One function cannot do both: `human_size` rounds, because the browser shows
arbitrary file sizes and `1468006 B` is not a size a person reads. But a
rounded size in the menu's min-size field is not a display bug, it is a scan
nobody agreed to — the form hands its own words back to the parser, so a field
showing `1.4 MiB` for 1500000 would run the scan at 1468006. That is this
repository's display-path/record-path divergence, one screen earlier.

So there are two, on one ladder, in one file, bonded by test:

- `human_size` — rounded, for reading. Relabelled `KB`/`MB` → `KiB`/`MiB`,
  which is what it was already dividing by, and its inherited `EB` branch —
  which printed 1024× the truth past a pebibyte — replaced by an `EiB` rung.
- `exact_size` — the largest unit that divides to one decimal place, plain
  bytes when none does. `1500000` stays `1500000`.

`exact_size` is what the help default and the menu prefill use, reached through
`Flag::show`, which is `Flag::apply`'s inverse. `--min-size 10485760` on the
command line reaches the form as `10 MiB` and leaves it as `10 MiB`. Tests:
`size::tests::every_size_the_tool_would_show_as_a_setting_parses_back_to_itself`
(the ladder, its neighbours, and a few thousand deterministic scatter values),
`size::tests::one_ladder_is_read_and_written`,
`cli::tests::what_the_form_shows_is_what_the_parser_reads`, and
`cli::tests::the_defaults_the_help_text_prints_are_the_defaults_the_code_has`,
which now asserts the help default *is* `exact_size(DEFAULT_MIN_SIZE)` and
parses back to it.

`--quiet` is a setting rather than a swallowed word, and says what it does.
`--verify` keeps its no-op arm and its reason.

### The live pulse

Ported from the oracle's `Reporter` (dedup.py:2149-2200): its stage names, its
two body shapes, its `\r\033[2K` repaint, its 24-cell bar. What is new is who
paints. The oracle ticks from the scan thread; here a ticker thread reads the
counters and the scan path only ever adds to one. `Pulse` is `Option<Live>` —
off, every method is a branch that falls through, and there is no thread, no
lock and no write.

The rate is `Effort::bytes_read`, the same counter the honesty fields are made
of, so the number on the line is bytes that really moved and cannot drift from
what the report says. That is why `main` now holds `Effort` in an `Arc`: the
ticker reads it while the readers write it.

Counted so that no file is counted twice inside a stage. Tier B of the
small-file stage is not counted, because those files were counted when their
head was read; cache hits are, because a warm stage that showed nothing would
look stuck rather than fast. Every stage's total is the very slice handed to
it, so the count can only ever fall short of the total, never run past it — and
it does fall short in one place: small-file candidates that the retention
budget declines to hold are never read there, so a spilling small-file stage
stops before the end of its bar and hands those files on to the head stage,
where they are counted again under a total that includes them. Short is the
honest direction; the bar is clamped at its total either way
(`pulse::tests::the_bar_never_runs_past_its_own_end`).

`finish` joins the ticker before wiping the line, so nothing can paint after
it. `Drop` calls `finish` too, so a scan that panics still gives the line back.
The line is cut a column short of the terminal width: a wrapped line leaves its
first row behind at the next repaint.

Eight cases through a real pty, `evidence/m5_2-pulse.txt`:

```text
$ python3 rust/evidence/scripts/m5_2_pulse.py rust/target/release/dedup-rs \
    <scratch> <a build of main@fbc196d>
a piped run says nothing at all                ok (stdout byte-identical to main, 5188 bytes; stderr empty on both; 5 counters equal at -j1)
a terminal gets a line, and gets it back       ok (6 repaints, stages Reading small files+Comparing, bar Reading small files 2402→12002/12002; Comparing 0→0/14, cleared before the browser)
the walk counts files out loud                 ok (2 repaints, counts 54472…54472 against 100000 files seen, never ahead and never backwards)
--quiet means quiet on a terminal too          ok (nothing between the menu and the browser, 9 groups found)
--list down a pipe, the line on the terminal   ok (37729 bytes of list down the pipe, 1 repaints on the terminal, neither in the other)
the menu takes 100mb, 1m and 1k                ok (100mb→0 group(s), 1m→7 group(s), 1k→9 group(s))
bytes on the command line, a size on screen    ok (shown as 2 MiB, and 2 MiB is what the scan used)
a size that is not one is refused in the field ok (refused in the field, in words that say what to type instead)

every case behaves
exit=0
```

The handover case is the one that matters most: it asserts the bytes written
between the menu giving the screen back and the browser taking it, and that the
last of them is the clear. Nothing of the line survives into the browser's
screen, and `--list`'s stdout and the line never appear in each other's stream.
The bar is checked for movement, forwards, and never past its own end, per
stage — a stage whose counter is never told anything paints a bar frozen at
nought and looks exactly like one that has just started.

### Inertness

`evidence/m5_2-inertness.txt` — a 30,516-file tree with work in every stage,
stderr piped, alternating builds so a warming disk drifts through both sides,
15 runs each:

```text
== stderr piped, -j1 ==
  opens                 51052          51052   same
  bytes_read        647872512      647872512   same
  files_seen            30516          30516   same
  bytes_seen        640221184      640221184   same
  seeks                 51260          51260   same
  groups                  254            254   same
  m5.2         4.601s median, 4.373–4.900 over the fastest 14 of 15
  main         4.531s median, 4.362–4.952 over the fastest 14 of 15
  difference   +1.6% (+0.070s), against a 0.590s trimmed spread within a build

== stderr piped, -j8 ==
  opens                 51052          51052   same
  bytes_read        647872512      647872512   same
  files_seen            30516          30516   same
  bytes_seen        640221184      640221184   same
  groups                  254            254   same
  seeks                 51260          51260   not compared: nondeterministic above one reader
  m5.2         1.114s median, 0.948–1.250 over the fastest 14 of 15
  main         1.164s median, 0.984–1.260 over the fastest 14 of 15
  difference   -4.3% (-0.050s), against a 0.302s trimmed spread within a build

the pulse costs a piped run nothing that can be measured
exit=0
```

`seeks` is diffed at one reader only, and that is a measured decision rather
than a convenience: eight repeats of `main` against itself at the default `-j`
gave 348 and 349 on the same tree, and this build gave the same two numbers.
Concurrent readers interleave and count against each other, which is the honest
answer for one set of heads and is what M6 scores; a comparison there would be
measuring the scheduler.

This machine has other agents on it, and the noise floor now drops the slowest
tenth of each build's runs before taking the spread — REVIEW-M5_2's non-binding
note 4, taken, because a floor set by the worst outlier is a gate that cannot
fail. Only the slow tail goes: a run cannot be spuriously *fast*, so the fast
end is real and the trim cannot hide a regression.

The two halves disagree about the sign — `+1.6%` at one reader and `-4.3%` at
eight, this build slower on one and faster on the other, on the same tree in
the same minutes. A cost that appears and disappears with the job count is not
a cost. The counters, which are the evidence that does not have a spread, are
identical on both.

(The absolute times are higher than the first capture because that one ran on a
warm page cache and this one on a freshly written tree. Both halves of each
comparison were run alternately against the same tree, which is what the
comparison rests on.)

### Also

`tools/passport-measure.sh` takes `--min-size 1m`, and gives the scan's own
stderr to the terminal so the line is on while the maintainer sits through an
hour of platter — only the timings go into the log, because a repainted line
logged verbatim is a page of carriage returns. Guarded: where `/dev/tty` cannot
be opened the line stays off and everything goes to the log exactly as before.
An unguarded redirection there does not merely lose the line, it fails the
whole scan, which is how I found it — under a pty with no controlling terminal.
That guard is a step past the letter of the spec, which asks only for the human
forms; it is here because this script is the thing the maintainer runs on the
drive that started M5.2.

### Standards and the gate

```text
$ cargo test
test sys::terminal::a_live_terminal_with_something_typed_at_it_is_ready ... ok

test result: ok. 182 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s

$ cargo clippy --all-targets -- -D warnings
Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-m52/rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.95s

$ cargo fmt --check
(no output: clean)

FMT_EXIT=0
```

164 tests on `main@fbc196d`, 182 here — counted by building that commit and
running its own suite, not by reading the last claim, which said 163 before
`main` gained one. (181 at the first claim; the fix round added one, taking
REVIEW-M5_2's non-binding note 2.)

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
exit=0
```

### A defect found on the way, in M5's evidence rather than in M5.2

`evidence/scripts/m5_end_to_end.py` — the browser's end-to-end suite, the only
proof in the repository that the browser deletes what it says it deletes — has
not been testing the browser since M5.1 landed. It launches with `-i`, which
now reaches the pre-flight menu, and then types browser keys at a form that
ignores them. Every pty case timed out and was killed:

```text
18 check(s) differing
```

...on `main@fbc196d` exactly as on this branch, so the suite was reporting the
same wreckage whatever it was pointed at. This is REVIEW-M5_1's own third
finding — "a test that does not assert which screen it is on stops covering the
screen it was written for the moment somebody adds another" — which was fixed
in `tui-terminal-death.py` and not looked for anywhere else.

Three harness bugs, no program change:

- It now waits for the menu by name, presses Enter, waits for the browser's
  alternate screen, and prints `NOT REACHED` where that never happens. A case
  that is not on the browser must say so instead of printing numbers.
- The two cases that run from inside the scanned tree passed a relative `HOME`,
  so the run's own report, cache and Trash were created *inside the tree* and
  counted among the survivors. Absolute now. This is the case that guards
  REVIEW-M5's binding item 1, the keep rules ranking the scan's own name.
- The too-small-terminal case can no longer start small: the menu refuses
  below 54×16 before the browser is ever reached, and correctly so. The pty
  starts roomy and is shrunk once the browser has it, which is a window being
  resized — the only way a browser now reaches a screen too small to read.

`evidence/m5_2-m5-suite-restored.txt`:

```text
=== 2. the repaired script, both builds, unmodified ===
this branch    0 check(s) differing   EXIT=0   NOT REACHED lines: 0   'reached the browser' checks: 9
main@fbc196d   0 check(s) differing   EXIT=0   NOT REACHED lines: 0   'reached the browser' checks: 9
```

Byte-identical output on both builds, so nothing in M5.2 moved the browser; and
eighteen checks that were dead are alive, including the deletion laws, the
keep-rule law, and the refusal on a screen nobody can read.

`tui-terminal-death.py` (4/4) and `m5_1_menu_end_to_end.py` (7/7) were re-run
unchanged and are green. Two more scripts are **not**, on this branch and on
`main@fbc196d` alike, and I have left them alone because what they turn up is
not a harness bug:

- `m5_screens.py` dies with `OSError: [Errno 5]` partway through — the same
  menu-in-front cause, one screen further on.
- `m5_terminal_restore.py` reports 5 checks differing. Its `quitting with q`
  case is clean — raw mode entered and released, alternate screen entered and
  left — but its `interrupting with ctrl-c` case is not, and running that down
  found a program gap rather than a script one.

**Ctrl-C does not leave the pre-flight menu.** The browser answers it before
anything else (`tui/mod.rs`'s `key_for` checks `KeyModifiers::CONTROL` first);
the menu's `key_for(editing: bool, code: KeyCode)` is never given the
modifiers, so Ctrl-C arrives as `Char('c')` and falls through to `Key::Ignored`.
Raw mode means the terminal raises no SIGINT of its own, so nothing happens at
all. Driven on a pty, at both screens, on both builds:

```text
== menu / keyboard ==
   reached menu before interrupting: True
   (canonical, echo, ISIG) before : (True, True, True)
   (canonical, echo, ISIG) during : (False, False, False)
   (canonical, echo, ISIG) after  : (False, False, False)
   exit: IGNORED — still running after 5s, then killed
   alternate screen entered 1, left 0
   terminal came back as it was: False (after our SIGKILL, not after the interrupt — the interrupt did nothing)

== browser / keyboard ==
   reached browser before interrupting: True
   (canonical, echo, ISIG) before : (True, True, True)
   (canonical, echo, ISIG) during : (False, False, False)
   (canonical, echo, ISIG) after  : (True, True, True)
   exit: 0
   alternate screen entered 2, left 2
   terminal came back as it was: True
```

The `menu / keyboard` row says the terminal did not come back, and says why in
the same breath: the keystroke did nothing, so the probe had to `SIGKILL` the
run, and it is the `SIGKILL` those modes describe. The interrupt itself is not
what wrecked anything — nothing at all happened.

`q` and Esc still leave, so nobody is trapped, and M5_1-MENU.md specifies only
those two — which is why I am reporting this rather than changing another
milestone's keymap inside this one. It is one line (take the `KeyEvent` rather
than its `KeyCode`, as the browser does) and it belongs to whoever owns M5.1.

### What a reviewer should look at hardest

- **Two humanisers where the spec says one.** Argued above and in LOG.md. The
  bond between them is `size::tests::one_ladder_is_read_and_written`; if the
  reviewer wants one function, the price is either a rounding menu field or a
  browser that prints raw byte counts, and I would rather be told which.
- **A wide group shows a still bar and a moving rate.** The compare stage
  counts as each group settles, so a single group of forty files reports
  nothing until it finishes. That is the oracle's behaviour in the same shape —
  its `tick` inside `split_identical` moves `hashed_bytes` and not
  `hashed_files` — and the rate is the sign of life there.
- **Ctrl-C mid-scan leaves one line of text behind.** The pulse writes text and
  never a mode change, so an interrupt while it is painting cannot wreck
  anything — but `Drop` does not run on a default SIGINT, so the last line
  painted stays where it is and the shell's next prompt lands after it. Driven
  on a pty, interrupt sent while the line was provably up:

  From `evidence/m5_2-signals.txt`, which the fix round turned from a
  terminal probe I pasted from into a script that fails if any of this changes:

  ```text
== scan / keyboard ==
   reached scan before interrupting: True
   (canonical, echo, ISIG) before : (True, True, True)
   (canonical, echo, ISIG) during : (True, True, True)
   (canonical, echo, ISIG) after  : (True, True, True)
   exit: -2
   alternate screen entered 0, left 0
   terminal came back as it was: True
  ```

  The oracle clears it, because Python hands it a `KeyboardInterrupt` to catch.
  Matching that here means a signal handler, which means `libc::signal` and an
  `unsafe` block, which REWRITE.md §7 keeps inside `sys.rs` — new machinery for
  one stale line on a terminal that is otherwise exactly as it was found. I
  left it, rather than decide that quietly.

  Chasing that question one step further turned up something worse, and older.
  A **signal from anywhere but the keyboard, while the browser holds the
  terminal, wrecks it** — and that is M5's, not M5.2's. Ctrl-C typed at the
  browser is safe, and safe for a reason worth stating: raw mode clears `ISIG`,
  so no signal is raised at all and the keystroke arrives as a key event that
  `key_for` maps to `Key::Quit`. But `kill -INT`, a supervisor, or a shell's
  `kill %1` skips the terminal entirely, and a default signal disposition
  terminates without unwinding — so neither the `Restore` drop guard nor the
  panic hook runs. Driven on a pty, `(canonical, echo, ISIG)` sampled before,
  during and after:

  ```text
ctrl-c at the keyboard, by screen:
  scan/keyboard    exit -2     terminal intact  canonical mode, so it is a real SIGINT; nothing to restore
  menu/keyboard    exit killed ignored          raw mode clears ISIG and the menu's key map drops the modifiers, so nothing happens — M5.1's, reported not fixed
  browser/keyboard exit 0      terminal intact  raw mode clears ISIG; read as a key and mapped to Quit

a signal from anywhere but the keyboard, at the browser:
  browser/signal   exit -2     TERMINAL WRECKED no unwinding, so neither the drop guard nor the panic hook runs — M5's, reported not fixed
  ```

  Byte-identical on `main@fbc196d` for the three rows that exist there — the
  fourth is the scan, which `main` has no progress line to be interrupted
  through. That is what makes the wrecked terminal M5's and the ignored ctrl-c
  M5.1's rather than this milestone's.

  Byte-for-byte the same on `main@fbc196d`, so it predates this branch. The
  terminal is left in raw mode with the alternate screen still on — the M5.4
  law, failed, by the one exit path M5's own drop guard and panic hook cannot
  cover. Terminal *death* is covered and passes (`tui-terminal-death.py`, 4/4);
  it is a signal, specifically, that goes unhandled, because the crate installs
  no signal handler anywhere. Reported and not fixed: it is another milestone's
  screen, and the fix is the same `libc::signal` machinery I declined above.

- **Nothing here is deletion-adjacent**, and the `ProvenGroups` guard, the
  survivor guard and `verify_attested` are untouched. `--gate all` is 33/33 and
  the piped stdout is byte-identical to `main`'s.

### Fence conditions this run was made under

Worktree `/Users/dogaozden/AI_Projects/dedup-m52` on branch `m5_2-ux`, off
`main@fbc196d`. Nothing under `/Volumes` was read or written. Every corpus was
generated under this worktree's `rust/target/`, including the copy of
`main@fbc196d` used for the differentials, which was extracted with `git
archive` and built into a target directory of its own rather than by touching
any other worktree. No `conformance/` file, no `rbench/`, `bench/` or `xbench/`
file, and no other branch was touched.

---

## M5.2 — fix round after REVIEW-M5_2 — 2026-08-23 — RE-CLAIMED

`REVIEW-M5_2.md` returned **CHANGES** on two binding items, both in the evidence
harnesses. Neither touches the program, and the program has not moved: the only
`rust/src` change in this round is inside a `#[cfg(test)]` module, taking
non-binding note 2.

### Binding 1 — `menu_mutants.py`, broken by this branch and missing from the claim

The reviewer is right, and right about the part that stings: this branch edits
the flag table more than anything since M5.1, `menu_mutants.py` is the script
that exists to guard the flag table, and I ran the suites I remembered instead
of the ones that touch what I changed. It aborted at mutant 3 of 8, so five
mutants never ran — including both whole-program terminal-death mutants M5.1 was
approved on.

The retarget is the reviewer's, one line: the `default:` line is unique in
`cli.rs` on its own, and the `help:` line above it was never what the mutant
needed. `evidence/m5_2-menu-mutants.txt`:

```text
$ python3 rust/evidence/scripts/menu_mutants.py
Baseline: the tree as written.
  cli::tests and tui::menu::tests all pass

mutant help-misses-a-flag           renders every flag but the first
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags, the_help_text_says_something_about_every_flag

mutant help-invents-a-flag          hand-writes a line for a flag the parser does not accept
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags

mutant help-default-drifts          says a default the code does not have
  caught by the_defaults_the_help_text_prints_are_the_defaults_the_code_has, clearing_a_field_puts_the_default_back, the_min_size_field_shows_a_size_a_person_can_read_and_type

mutant menu-writes-its-own-default  shows a default of its own instead of the table's
  caught by clearing_a_field_puts_the_default_back, the_form_shows_the_root_every_field_and_every_default, the_min_size_field_shows_a_size_a_person_can_read_and_type

mutant menu-drops-a-field           leaves a field out of the argument list it hands back
  caught by a_switch_toggles_and_reaches_the_options, the_command_line_prefills_the_fields_it_names

mutant menu-skips-validation        accepts whatever is typed and lets the scan find out
  caught by a_bad_profile_is_refused_the_same_way, a_field_is_refused_in_the_field_with_the_parsers_own_words, a_refusal_is_shown_beside_the_field_it_belongs_to

Baseline: the same tree, built and run through tui-terminal-death.py.
  every scenario passed

mutant one-of-three-answers         enters the blocking read when poll said nothing was typed
  caught: hangup/menu      FAIL  the menu was still running 5s after the terminal died, 5.02 CPU-seconds in that window — SPINNING

mutant drop-the-dead-terminal       lets ratatui print to a descriptor that is gone, panicking on the way out
  caught: hangup/menu      FAIL  the menu left in 0.06s but by panicking (exit 101)

every mutant was caught
MENU_MUTANTS_EXIT=0
```

Eight named, eight caught, exit 0 — and `help-default-drifts` and
`menu-writes-its-own-default` are now caught by M5.2's own
`the_min_size_field_shows_a_size_a_person_can_read_and_type` as well. The guard
got stronger while it was not being run, which is the part worth worrying about.

**The process fix, so this does not recur:** before claiming, run every script
under `evidence/scripts/` that names a file the branch touched. `git diff
--name-only` against the base, grepped through the scripts, finds it in a
second. Written into `LOG.md` as a rule rather than as an apology.

### Binding 2 — `NOT REACHED` now counts

`drive` hands the flag back, and all nine of its call sites turn it into
`failures += not check("reached the browser", reached, True)`. Proved the way
the reviewer proved it — suppress only the menu `Enter`, change nothing else,
and require the suite to fail. From `evidence/m5_2-m5-suite-restored.txt`, which
now carries all three states end to end:

```text
=== 1. the script as of main@fbc196d, against this branch's binary ===
18 check(s) differing
EXIT=1
(every pty case timed out and was killed; none of them was on the browser)

=== 2. the repaired script, both builds, unmodified ===
this branch    0 check(s) differing   EXIT=0   NOT REACHED lines: 0   'reached the browser' checks: 9
main@fbc196d   0 check(s) differing   EXIT=0   NOT REACHED lines: 0   'reached the browser' checks: 9

=== 3. the same script with only the menu Enter suppressed ===
26 check(s) differing
EXIT=1   NOT REACHED lines: 9
```

and the two cases the review named, on that suppressed run:

```text
=== cancelling the confirmation removes nothing ===
    expect: all three files survive and the Trash is empty
    NOT REACHED: the browser screen never appeared
    reached the browser: False (DIFFERENT)
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== quitting with copies marked removes nothing ===
    expect: all three files survive
    NOT REACHED: the browser screen never appeared
    reached the browser: False (DIFFERENT)
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)
```

26 differing where the unrepaired script gave 17. Nine of the ten cases now
report a differing check; the tenth is `-i without a terminal refuses instead of
guessing`, the only case in the file that never opens a pty — a plain subprocess
with no terminal, which suppressing the menu `Enter` cannot reach. It is honest
for that one to stay green, and the evidence file says so in as many words.

### The non-binding notes, answered

**1. Stale quote — taken, and then taken further.** Two of the eight lines the
claim quoted from `evidence/m5_2-pulse.txt` were from a run before the last
re-capture. Corrected from the file. The cause is worth naming: I hand-copied
timing-dependent output into the claim instead of pasting the file, then
re-captured the file and never re-synced the quote.

So I checked the rest of the claim the same way the reviewer would have to —
mechanically, every fenced line against every file in `evidence/` — and found
fifteen more lines with no evidence file behind them at all. All of them were
from ad-hoc terminal probes I ran, pasted from the screen, and never saved.
Those probes are now `evidence/scripts/m5_2_signals.py`, and it is a gate rather
than a print-out: it names the four behaviours it records and exits 1 if any of
them changes. Every fenced line in this claim now comes from a file under
`evidence/`.

**2. The `16.0 EiB` boundary — taken.** The fuzz shifted its values by 4 bits to
stay under the one string `human_size` can produce that the grammar refuses. It
now sweeps the whole u64 range and asserts the boundary from both sides, in
`what_the_browser_shows_is_at_least_readable_back` and a new
`the_one_size_the_browser_can_show_that_the_grammar_refuses`. The boundary is
*searched for* by bisection rather than written down, so a change to the
rounding moves it instead of breaking the arithmetic that names it — and the
search standing on `human_size` being non-decreasing is itself checked at the
boundary.

**3. `m5_2_pulse.py` wants a quiet machine — taken.** Said in the docstring,
with the failure it actually produces under load (`exit -9 … [the run did not
exit; killed]`) so a reader can recognise it rather than chase it.

**4. The inertness gate could not fail at `-j1` — taken.** The noise floor now
drops the slowest tenth of each build's runs before taking the spread. Only the
slow tail goes, and deliberately: a run cannot be spuriously *fast*, so the fast
end is real and the trim cannot hide a genuine regression.

**5. `passport-measure.sh`'s `/dev/tty` guard — accepted as written.** Nothing
to do; recorded here so the audit is not lost.

**6. Not this branch's — left alone, as instructed.** `menu_mutants.py`
hardcodes one session's scratchpad path at lines 23-25. `m5_screens.py` passed
for the reviewer at both tips and dies with `EIO` for me, so it is
machine-dependent; either way it is M5.1's.

---

## M7 — the pivot-window compare — CLAIMED

Branch `m7-pivot`, off `main@cc654c4`. Binding spec `M7-PIVOT.md`; the
maintainer review is BLOCKING (`rust/REVIEW-M7.md`).

Under `--profile spinning` ONLY, stage 5 stops interleaving. Per group the
pivot is read a window at a time and held; every other member then reads the
same range in one run of its own and is compared, in memory, against the bytes
in hand. A member that parts from the pivot leaves its class and is filed under
the run it parted in; members filed under the same run are compared among
themselves from the start of that run, the first of them becoming the next
pivot, recursively, until every class is settled.

The flash profile is untouched — same parallel lockstep, same waves, same
descriptors — and the gate below is run with no profile flag at all, which is
the proof of that. `verify_attested` is untouched too: the deletion path keeps
the capped cohort comparison it was reviewed on at M6. Nothing outside
`rust/src/compare.rs` and `rust/src/consts.rs` changed.

**HDD wall-clock unvalidated.** No rotational drive is available to an agent —
the only one on this machine is fenced — so no speed claim is made here. What is
claimed is correctness, which is proved against the oracle, and the access
pattern, which is counted. A user-supervised run on a real spinning disk is
still owed. The wall this was commissioned from is the maintainer's, measured on
real spinning media (`measurements/passport/`, and a live scan): stage 5 moving
at ~44 MB/s against a ~110–130 MB/s sequential ceiling. That figure is cited,
not reproduced; nothing here measured it.

### Why the answer is the same answer

A *run* is one read the pivot asked for. Every member is asked for the same
lengths at the same offsets, so the runs are the same runs for everybody, and
comparing a run against the pivot's is the same `memcmp` on the same boundaries
lockstep does.

- A member kept in the pivot's class matched every run to the end of the
  pivot's file, so the class the sweep settles is exactly the members equal to
  the pivot.
- Two members that parted in DIFFERENT runs cannot be equal, and are never read
  again to find out: the one that parted in the earlier run differs from the
  pivot somewhere inside it, and the one that parted later matched the pivot
  through the whole of that run.
- Two members that parted in the SAME run agree with the pivot — and so with
  each other — everywhere before it, and are compared among themselves from
  exactly there. Recursively, so every pair either meets or is told apart by the
  rule above.

It is M6's reasoning at a finer grain. Where the capped comparison rests on
byte-equality being transitive between cluster representatives, this rests on
the same transitivity plus one extra fact — that agreeing with a third file over
a range and disagreeing with it inside that range cannot both be true of two
equal files. Every pair in a reported group is still joined by a chain of
comparisons that each read real bytes this scan. No digest settles anything.

**Two sizes, for two reasons.** A window is what a head movement buys: it starts
at the comparison chunk and grows by `PIVOT_GROWTH` to `PIVOT_WINDOW`, so a
group that parts early costs exactly what the lockstep round it replaces cost,
and one that keeps agreeing pays a movement per member per 256 MiB. A run is
what a parting is placed to, and runs inside a window carry straight on from
each other, so their length costs no movement at all — `PIVOT_RUN` is
`MIN_CHUNK`, and a member that parts is read barely past the byte it parted at.
Three constants, all in `consts.rs` beside the other budgets.

**Memory.** One window and one member's run, `held_ceiling(chunk)`, however wide
the group. There is no term for the group's width, which is what M7-PIVOT.md
asks for by name: a 1,800-member group holds one window, not 1,800. A
`debug_assert` in the sweep says so on every window, and
`the_window_budget_is_the_cap_however_wide_the_group_is` runs it over a group of
200.

### The gate, with no profile flag

Run from the repository root:

```
rust/evidence/scripts/m7_gate.sh > rust/evidence/m7-gate.txt
```

which runs

```
python3 conformance/run.py --gate all \
  --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
```

Quoted from `evidence/m7-gate.txt`:

```text
$ ls /Volumes   (before)
\]
dedupbench

$ python3 conformance/run.py --gate all \
    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'

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
   exit 0

$ ls /Volumes   (after)
\]
dedupbench

$ hdiutil info | grep -c dedupconf   (0 = the check detached its image)
0
```

33 checks, 0 failed, exit 0. Nothing in the harness passes `--profile`, so all
33 ran the flash pipeline. The `ls /Volumes` lines either side are the fence
condition the maintainer set for running `m4-trashes`, whose check makes,
mounts, uses and detaches an 80 MB image of its own at `/Volumes/dedupconf`:
the two volumes already mounted are unchanged and were never in any path this
branch used, and `hdiutil info` afterwards shows the check's image gone.

### Groups are profile-independent

```
python3 rust/evidence/scripts/spinning_differential.py
```

M6's eight scratch trees, unchanged, each shaped at something the profile
removes. On each the oracle, `--profile flash`, `--profile spinning` and a
flagless run are compared. `evidence/m7-differential.txt`:

```text
tree                  files  groups   flash seeks  spinning seeks   flash bytes  spinning bytes  verdict
tails                     5       2            18              18        319488          425984  same
middles                   3       1            63               9      27856896        26279936  same
last-chunk                3       1            60              12      38805504        56623104  same
cohort-boundary          16       4           110              76      33554432        35389440  same
wide                     24       1           693              72     239075328       226492416  same
links                     6       2            16              12       1048576         1048576  same
unreadable                5       1            70               9      29884416        28311552  same
mixed                    46      10           132              75      20717575        45826056  same

8 trees, every profile identical to the oracle
```

The tree to read is `cohort-boundary` — sixteen files in four content classes,
each file shorter than one comparison chunk, which is the shape a pivot sweep is
worst at because every class needs a pass of its own, and the one that decided
how long a run is.

### Why a run is `MIN_CHUNK` and not the chunk

A run costs no head movement, so its length is free to choose; what it decides
is how many bytes a member reads past a parting and how many it reads again when
its class is compared to itself. That is an argument, so it was measured rather
than asserted: the same corpus read by `main`, by this branch, and by this
branch built with `PIVOT_RUN = SLOW_CHUNK` — the obvious implementation, and
the one the constant exists to reject. The variant is built from a copy of the
tree with a target directory of its own, so no variant binary is ever produced
in the working checkout.

```
python3 rust/evidence/scripts/pivot_run_size.py <main-build>/rust/target/release/dedup-rs
```

`evidence/m7-run-size.txt`:

```text
16 x 2 MiB in 4 classes, parting in the last kilobyte, --profile spinning, one reader.

read as                    bytes_read       MiB   seeks
main, capped cohorts         49807360      47.5      73
PIVOT_RUN = SLOW_CHUNK       82313216      78.5      72
PIVOT_RUN = MIN_CHUNK        35127296      33.5      72

all three report the same groups
a run as long as the chunk reads 2.34x what this branch reads, and 1.65x what
main's capped comparison reads; at MIN_CHUNK it is 0.71x main
```

A run as long as the chunk would have made this milestone a bytes REGRESSION on
this shape — 78.5 MiB against the 47.5 MiB `main` reads. At `MIN_CHUNK` it is an
improvement, at 33.5 MiB. `main`'s 49,807,360 bytes here is byte-for-byte the
number M6's own claim reports for `cohort-boundary`, from a differently written
generator in a different script — which is two harnesses agreeing about what the
old sweep does on this shape, and is the reason to believe the third row.

### Seeks, against the sweep this replaces

```
python3 rust/evidence/scripts/pivot_seeks.py <main-build>/rust/target/release/dedup-rs
```

Seven generated corpora, `--profile spinning`, one reader, every count checked
for determinism across two runs and every shape checked for the same groups from
all three of `main`, this branch, and `--profile flash`.
`evidence/m7-seeks.txt`:

```text
Seeks, one reader (-j 1). main is the capped cohort sweep under --profile spinning;
`ours` is this branch under the same flag. `flash` is the profile the gate grades,
for comparison with M6, whose table read flash against spinning.

Whole-scan counts: stages 1-4 are untouched by this branch and cost a head movement
per file between them, so the stage-5 ratio is higher than the column below.

shape               groups  flash   main   ours vs main vs flash     opens
one-wide-group           1   1668     89     48    1.9x    34.8x    25->12
several-groups           3   3210    192    120    1.6x    26.8x    63->30
few-and-long             2   3138    114     30    3.8x   104.6x     12->6
one-long-pair            1   2070     70     12    5.8x   172.5x      4->2
parting-groups           3   2504    166    108    1.5x    23.2x    47->19
small-and-many           4    816     80     64    1.2x    12.8x    32->16
classes-in-one-run       4    112     73     72    1.0x     1.6x    40->16

shape                what it is
  one-wide-group     one group of 12 x 64 MiB
  several-groups     3 groups of 10 x 48 MiB
  few-and-long       2 groups of 3 x 256 MiB
  one-long-pair      one group of 2 x 512 MiB
  parting-groups     3 bands of 6 x 64 MiB sharing 40 MiB, plus one unique
  small-and-many     4 groups of 4 x 20 MiB
  classes-in-one-run 16 x 2 MiB in 4 classes, parting in the last kilobyte

bytes. SPIN.md lets `spinning` read MORE than the profile the gate grades — it trades bytes for seeks by design — so what matters here is this branch against main, which is the sweep it replaces:
  shape                  flash MiB      main MiB       our MiB ours/main
  one-wide-group             774.0         831.9         768.0     0.92x
  several-groups            1455.0        1583.8        1440.0     0.91x
  few-and-long              1539.0        1536.0        1536.0     1.00x
  one-long-pair             1025.0        1024.0        1024.0     1.00x
  parting-groups            1161.0        1695.8        1194.6     0.70x
  small-and-many             328.0         320.0         320.0     1.00x
  classes-in-one-run          32.0          47.5          33.5     0.71x

seeks: between 1.0x and 5.8x fewer than main on every shape above
groups identical across main, this branch, and --profile flash, on every shape
HDD wall-clock unvalidated: no rotational drive is available here.
```

**Bytes are equal or lower on every shape** — 0.70x to 1.00x of what `main`
reads — which is more than M7-PIVOT.md asked for; it asked for equal. Two shapes
read strictly less, and both for the same reason: `main`'s capped comparison
re-reads a pivot once per cohort, and this sweep opens and reads each member
once per pass instead. The `opens` column is the same fact from the other side —
at least halved on every shape, and better than halved where `main` re-opened a
pivot in several cohorts — and it is where the descriptor table shows up
(below).

### Where the win is, and where it is not

`few-and-long` and `one-long-pair` are the shape SPIN.md commissioned the
profile for — a USB disk full of video — and they are where the ratio lives:
3.8x and 5.8x fewer head movements than `main`, 104.6x and 172.5x fewer than
the flash pipeline. The ratio grows with the file, because a sweep pays a movement
per member per window and lockstep pays one per member per chunk.

`classes-in-one-run` is the floor: 1.0x. Sixteen 2 MiB files in four classes,
every file shorter than one chunk, so the cost is the number of passes rather
than the length of the reads and there is nothing for a longer window to buy.
`small-and-many` at 1.2x is the same story less sharply. Read "1.0x to 5.8x"
as a statement about large files, not about every tree — and note that the
counts are whole-scan: stages 1 to 4 are untouched here and spend a head
movement per file between them, so stage 5's own ratio is higher than the
column.

The `vs flash` column is there to be comparable with M6's table, which read
flash against spinning and reported 3.0x to 3.6x. It is not comparable with
M6's *numbers*: flash's own seek counts moved on `main` when the round-2
optimiser landed, and the corpora are not M6's.

### What the default profile does, which is nothing new

RBENCH scores the default profile and never passes a profile flag, so the claim
it is owed is that its scenarios do not move.

**This is not RBENCH.** The harness keeps its corpora on the `dedupbench` RAM
disk under `/Volumes`, which this agent's fence forbids it to touch at all. So
the corpora here are built by `bench/corpus.py` — the same seeded generators,
the same names, the same shapes — under `rust/target/` in this worktree, and
timed on the local SSD. The score of record is still the refereed harness's;
this is evidence that there is nothing for it to find.

```
python3 rust/evidence/scripts/pivot_flash_inert.py <main-checkout>
```

`evidence/m7-flash-inert.txt`:

```text
RBENCH-shaped, on the local SSD in this worktree. Corpora from bench/corpus.py at
scale smoke, seed 20260824. No profile flag on any run — this is the profile the harness scores.
load average: 1.14 1.71 2.30  (shared machine: a figure above about 1 makes every
timing below indicative only)

scenario       files   bytes_read         opens    groups ours/base
needle         10000     24682520          9635        10   1.008x
typical         1600      2812459           430        64   0.995x
samesize         142      3702784           142         4   1.012x
widegroup         70     14501888            84         8   0.987x
photos            13     47846337            11         3   0.992x
bigheader          8     32677888             8         1   0.997x
prefixtrap        32     16973824            32         4   1.008x

geomean over 7 scenarios: 1.000x
noise floor (base against itself, same corpus, same rounds): 0.988x
load average at the end: 2.57 2.00 2.40

bytes_read, opens, files_seen, bytes_seen and the groups are identical between the two
builds on every scenario — that is the part of this that is a fact. The wall clock is not
claimed: see the noise floor above.
```

`bytes_read`, `opens`, `files_seen`, `bytes_seen` and the groups are identical
between the two builds on all seven scenarios — that is the part of this that is
a fact, and it is what RBENCH's byte-determinism gate and correctness gate read.
The wall clock is not claimed: the geomean is 1.000x and the noise floor, the
base build against itself over the same corpus and the same rounds, is 0.988x,
so every per-scenario figure between 0.987x and 1.012x is inside it. The machine
was at load average 1.1 rising to 2.6 and is shared with other agents.

### Where `--profile` meets the other flags

```
python3 rust/evidence/scripts/spinning_meets_the_rest.py
```

M6's combination suite, unchanged, re-run because this branch edits the stage
`--auto` deletes on. `evidence/m7-combinations.txt`:

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

### The tests are guards, not decoration

```
python3 rust/evidence/scripts/pivot_mutants.py
```

Twelve deliberate breaks, each a plausible implementation of the same feature,
built and run in a copy of the tree so no mutant binary is ever left in the
checkout. `evidence/m7-mutants.txt`:

```text
Baseline: the tree as written.
  compare::tests all pass

mutant merge-the-residue                calls everything that left the pivot in one run equal, without reading again
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_group_taking_several_rounds_is_partitioned_the_same_way, a_group_where_nothing_matches_reports_nothing, a_member_that_matches_nobody_is_carried_to_every_later_pass, a_residue_class_that_parts_later_parts_at_the_right_byte, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, a_residue_that_is_two_classes_is_split_again_rather_than_merged, equal_members_split_across_cohorts_are_still_found, members_that_left_in_one_run_at_different_bytes_are_not_merged, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape, the_two_comparisons_are_the_same_function

mutant drop-the-residue                 never offers a member that left the pivot to any later pivot
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_divergence_in_a_middle_window_splits_the_group, a_divergence_in_the_first_window_splits_the_group, a_divergence_in_the_last_window_splits_the_group, a_divergence_on_a_window_boundary_splits_the_group, a_group_taking_several_rounds_is_partitioned_the_same_way, a_member_that_cannot_be_read_leaves_the_rest_of_its_group_alone, a_pivot_that_cannot_be_read_hands_its_class_to_the_next_one, a_residue_class_that_parts_later_parts_at_the_right_byte, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, a_residue_that_is_two_classes_is_split_again_rather_than_merged, a_window_longer_than_a_run_is_still_split_at_the_right_byte, equal_members_split_across_cohorts_are_still_found, members_that_left_in_different_runs_are_never_compared_again, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape, the_sweep_finds_the_same_classes_wherever_the_files_part, the_two_comparisons_agree_where_a_window_holds_several_runs, the_two_comparisons_are_the_same_function, two_members_that_left_the_pivot_in_different_runs_still_find_their_own, two_members_that_left_the_pivot_together_are_compared_to_each_other

mutant regroup-past-the-parting         restarts a residue AFTER the run it parted in, so the parting byte is never re-read
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_group_taking_several_rounds_is_partitioned_the_same_way, a_group_where_nothing_matches_reports_nothing, a_member_that_matches_nobody_is_carried_to_every_later_pass, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, a_residue_that_is_two_classes_is_split_again_rather_than_merged, equal_members_split_across_cohorts_are_still_found, members_that_left_in_one_run_at_different_bytes_are_not_merged, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape, the_two_comparisons_are_the_same_function

mutant regroup-at-the-window            restarts a residue at the window rather than at the run it parted in
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start

mutant one-bucket-for-every-parting     files every leaver under one offset, so two that parted in different runs meet again
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start

mutant hash-the-run-that-parted         lets a leaver's last run reach its digest, which it will read again
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_group_read_here_is_learned_as_the_digest_of_the_whole_file

mutant only-the-first-run-of-a-window   judges a member on the first run of each window and skips the rest
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_divergence_in_a_middle_window_splits_the_group, a_divergence_in_the_last_window_splits_the_group, a_divergence_on_a_window_boundary_splits_the_group, a_group_read_here_is_learned_as_the_digest_of_the_whole_file, a_group_taking_several_rounds_is_partitioned_the_same_way, a_group_where_nothing_matches_reports_nothing, a_member_that_cannot_be_read_leaves_the_rest_of_its_group_alone, a_pivot_that_cannot_be_read_hands_its_class_to_the_next_one, a_residue_class_that_parts_later_parts_at_the_right_byte, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, a_residue_that_is_two_classes_is_split_again_rather_than_merged, a_window_longer_than_a_run_is_still_split_at_the_right_byte, members_that_left_in_one_run_at_different_bytes_are_not_merged, the_kept_head_is_compared_rather_than_read_a_second_time, the_kept_tail_settles_the_end_without_reading_it, the_sweep_finds_the_same_classes_wherever_the_files_part, the_sweep_opens_each_member_once_and_reads_it_in_one_run_per_window, the_two_comparisons_agree_where_a_window_holds_several_runs, the_two_comparisons_are_the_same_function, two_members_that_left_the_pivot_in_different_runs_still_find_their_own, two_members_that_left_the_pivot_together_are_compared_to_each_other

mutant settle-on-a-lost-pivot           settles the pivot's class even though the pivot could not be read
  caught by a_pivot_that_cannot_be_read_hands_its_class_to_the_next_one

mutant strand-a-lost-pivots-class       drops the members a lost pivot had already proved equal to each other
  caught by a_pivot_that_cannot_be_read_hands_its_class_to_the_next_one, a_pivot_that_goes_away_mid_sweep_settles_nothing_and_carries_its_class_on

mutant flat-windows                     never lets a window grow, so every window costs a head movement per member
  caught by the_sweep_opens_each_member_once_and_reads_it_in_one_run_per_window

mutant no-seek-back                     leaves a regrouped class's handles wherever the last comparison stopped
  caught by a_digest_survives_the_regrouping_that_reads_a_run_twice, a_group_taking_several_rounds_is_partitioned_the_same_way, a_group_where_nothing_matches_reports_nothing, a_member_that_matches_nobody_is_carried_to_every_later_pass, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, a_residue_that_is_two_classes_is_split_again_rather_than_merged, equal_members_split_across_cohorts_are_still_found, members_that_left_in_one_run_at_different_bytes_are_not_merged, the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape, the_two_comparisons_are_the_same_function

mutant runs-as-long-as-the-chunk        reads a window in chunk-long runs, so a parting is placed a chunk wide
  caught by a_member_that_parts_is_read_to_the_end_of_its_run_and_no_further, a_residue_is_read_again_from_its_own_run_rather_than_from_the_start, the_window_budget_is_the_cap_however_wide_the_group_is

every mutant was caught
```

The three worth naming are `merge-the-residue`, which calls everything that
parted in one run equal without reading it again — the exact "two files that
differ, reported as one group" this stage exists to prevent;
`regroup-past-the-parting`, the off-by-one that restarts a residue after the run
it parted in, so the parting byte is never compared; and
`hash-the-run-that-parted`, which is invisible to every group assertion and
shows up only as a whole-file digest of a file that does not exist — the kind
that would be remembered and matched against on the NEXT scan.

### The descriptor table, and what the sweep does with it

The held-`File` table composes rather than being bypassed. `compare_stage`'s
spinning branch now lifts a group's carried descriptors exactly as the flash
branch does, and the sweep opens each member once and keeps it for every window
— so a carried descriptor saves exactly the open it saves on the flash path. The
capped comparison never did this: it re-opened the pivot in every cohort.

End to end, and as a gate rather than a print-out:

```
python3 rust/evidence/scripts/pivot_descriptors.py <main-build>/rust/target/release/dedup-rs
```

`evidence/m7-descriptors.txt`:

```text
three 6 MiB copies, --profile spinning, one reader:
main@cc654c4    "bytes_read": 18874368, "opens": 6, "seeks": 12
this branch     "bytes_read": 18874368, "opens": 3, "seeks": 9

one open per member instead of two, the same bytes, fewer seeks
```

What the sweep does NOT carry over from the capped comparison is
`SPIN_INTERLEAVE`. That cap existed to bound how many files a round-robin read
at once; holding a descriptor costs nothing on a disk that is only ever asked to
read from one of them at a time, and the flash path has always opened a whole
group at once. `the_sweep_opens_each_member_once_and_reads_it_in_one_run_per_window`
holds the count to one per member.

### What was not touched, and why

- **`verify_attested`.** M7-PIVOT.md says "Deletion path untouched.
  `verify_attested` untouched", so it still calls `split_identical_capped` under
  `spinning`. That is a deliberate literal reading: the function is on the
  deletion path, it was reviewed at M6 on that comparison, and re-proving an
  attested group is not where this milestone's seeks are. The consequence is
  that two spinning splitters now exist. Both are exercised by
  `bench.partitions`, which runs lockstep, the capped sweep and the pivot sweep
  over every shape in the file and requires all three to agree — so the older
  one is now a second opinion on the newer one rather than dead weight. Say the
  word and it moves.
- **The flash path.** The only edit inside it is that its reading preliminaries
  — opening the group, comparing what stage 3 kept, seeding the hashers,
  positioning the handles — moved into a `prepare` both splitters call. The code
  is the same code; duplicating the trickiest twenty lines in the program into a
  second copy was the alternative.
- **`COMPARE_BUDGET`, `SLOW_CHUNK`, `SPIN_INTERLEAVE`.** Unchanged. REWRITE.md
  §8 forbids re-tuning settled measurements, and the sweep's budget is a new
  constant rather than a new value for an old one.

### Standards

```
rust/evidence/scripts/m7_standards.sh <main-checkout>/rust > evidence/m7-standards.txt
```

The before-count is re-derived by building `main@cc654c4` from a `git archive`
into a target directory of its own, not copied from the previous claim.
`evidence/m7-standards.txt`:

```text
=== main@cc654c4, for the before-count and the clippy verdict that predates this branch ===
$ cargo test

test result: ok. 194 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.64s


$ cargo clippy --all-targets -- -D warnings
error: this assertion has a constant value
   --> src/compare.rs:898:9
    |
898 |         assert!(COMPARE_BUDGET / 512 < CHUNK, "512 members must shrink it");
    |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    |
    = help: consider moving this into a const block: `const { assert!(..) }`
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.92.0/index.html#assertions_on_constants
    = note: `-D clippy::assertions-on-constants` implied by `-D warnings`
    = help: to override `-D warnings` add `#[allow(clippy::assertions_on_constants)]`

error: could not compile `dedup-rs` (bin "dedup-rs" test) due to 1 previous error
   exit 101

=== this branch ===
$ cargo test

test result: ok. 224 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.82s


$ cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-m7/rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.94s
   exit 0

$ cargo fmt --check
(no output: clean)

=== the binary the evidence was measured with is the committed source ===
    Finished `release` profile [optimized] target(s) in 0.09s
$ shasum -a 256 target/release/dedup-rs
d61455257d5e735bbed1b947e439760293eb1af9750900791c938146eef11382  target/release/dedup-rs
$ touch src/compare.rs src/consts.rs && cargo build --release
    Finished `release` profile [optimized] target(s) in 12.22s
$ shasum -a 256 target/release/dedup-rs   (identical = it was not stale)
d61455257d5e735bbed1b947e439760293eb1af9750900791c938146eef11382  target/release/dedup-rs
```

194 tests to 224. Thirty new, and five that existed already now guard the new
sweep as well: `bench.partitions` returns all three comparisons and every shape
test asserts the pivot sweep agrees with lockstep.

**The clippy line is not this branch's.** `cargo clippy --all-targets -- -D
warnings` fails on `main@cc654c4` itself — clippy 1.92's `assertions_on_constants`
rejects M6's `assert!(COMPARE_BUDGET / 512 < CHUNK, ...)`, whose operands became
const-foldable when the round-2 optimiser merged. Reported rather than fixed;
the maintainer fixed it on `main` at `06548c2` and it is cherry-picked here as
`6f032ef`, which is why the branch runs green above while the base does not. The
side-by-side capture is kept deliberately: it is the proof the red predates this
work.

**No stale binary.** Every measurement above was taken with
`rust/target/release/dedup-rs` built from the committed source. Proved rather
than assumed, in the same capture: `shasum -a 256` of the binary, then `touch
src/compare.rs src/consts.rs && cargo build --release` — a real twelve-second
recompile, not a cache replay — then `shasum` again. Byte-identical, so it was
not stale. A stale mutant binary cost the maintainer a day yesterday, which is
why this is a line in the standards script rather than a sentence here.

### Defects found and not fixed

1. **A wide group of all-distinct files is quadratic, on both spinning
   comparisons.** The pivot loop strips one class per pass, so N unique files of
   the same size cost N passes. `partition_blocks` has `PARTITION_FANOUT` for
   exactly this on the flash path and neither spinning path has an equivalent.
   It is not new and this branch improves it — `main`'s capped sweep re-reads a
   whole cohort per pass where this reads one run per member after the first —
   but it is still there, and a `widegroup`-sized tree of genuinely unique files
   would be very slow under `--profile spinning`. Not fixed: it is not in
   M7-PIVOT.md, and the fix (digesting the run a member parted in, to pick a
   slot the way `partition_blocks` does) belongs with a measurement of how often
   the shape occurs.
2. **`/Volumes` has a volume named `\]`** on this machine, present before and
   after every run here. Nothing this branch did created or touched it; recorded
   because the gate evidence shows it and a reader should not have to wonder.

### Fence conditions this run was made under

Worktree `/Users/dogaozden/AI_Projects/dedup-m7` on branch `m7-pivot`, off
`main@cc654c4`, plus the cherry-pick of the maintainer's `06548c2`. Nothing
under `/Volumes` was read or written, with the single exception the maintainer
ruled in: the ephemeral APFS image the conformance `m4-trashes` check creates,
attaches at `/Volumes/dedupconf` and detaches itself, recorded either side by
`ls /Volumes` in `evidence/m7-gate.txt`. The `dedupbench` RAM disk was not
touched, mounted, unmounted or read. Every corpus was generated under this
worktree's `rust/target/m7/` and removed after use, including the copy of
`main@cc654c4` used for the differentials, which was extracted with `git
archive` and built into a target directory of its own rather than by touching
any other worktree. No `conformance/`, `rbench/`, `bench/` or `xbench/` file was
modified, and no other branch was touched. No `sudo`. Free space on the volume
was 13 GiB throughout, which is why the largest corpus here is 1.5 GiB.
