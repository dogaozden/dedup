# REVIEW-M5.md — maintainer review of the M5 claim

**Verdict: CHANGES**

Claim commit `67c7e65`, reviewed at tip `ec0c9a6`. Implementation is `020b73c`
(the browser) and `df03dde` (two fixes). Binding specs are `M5-TUI.md` and
REWRITE.md §0/§3; the UX oracle is `dedup.py`'s curses `Browser`
(`dedup.py:2203-2596`). I read the oracle before reading the port.

Two binding items, numbered below. Neither is in the deletion core — I tried
hard to break that and could not. Item 1 is a silent behavioural divergence
from both the oracle and this program's own `--auto`; item 2 is a hard-rule
violation in a corner. Everything else I checked came back correct, including
the parts I expected to fail.

---

## Evidence — what I ran myself

Standards at tip, from `rust/`:

```
$ cargo fmt --check
EXIT=0

$ CARGO_TARGET_DIR=<scratch>/m5rev-target cargo clippy --all-targets -- -D warnings
    Checking dedup-rs v0.1.0 (/Users/dogaozden/AI_Projects/dedup-m5/rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.48s
EXIT=0

$ cargo test
test result: ok. 101 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s
EXIT=0
```

The clippy run is deliberately out-of-tree so nothing was replayed from a warm
cache — it compiled all 24 crates from scratch and emitted no warning.

The gate, from the worktree root:

```
$ python3 conformance/run.py --gate all --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
M1 gate   13 pass      M2 gate   13 pass
M3 gate   m3-cold pass (13.0 MB read) · m3-warm pass (0.0 MB read)
          m3-poison-refusal pass · m3-same-second pass
M4 gate   m4-deletion pass · m4-links pass · m4-trashes pass

33 check(s), 0 failed
gate all: green
EXIT=0
```

The "core untouched" claim is true by construction, not just by a green gate.
Every scanning file is byte-identical to pre-M5 (`c238b30`):

```
$ git diff --stat c238b30..ec0c9a6 -- rust/src/
 rust/src/main.rs      |  55 ++-
 rust/src/pipeline.rs  |   1 +
 rust/src/trash.rs     | 528 +++++++++++++++++++++++++-
 rust/src/tui/{mod,model,view}.rs  | 2108 +++++++++++++++

$ git diff --stat c238b30..ec0c9a6 -- rust/src/walk.rs rust/src/stages.rs \
      rust/src/compare.rs rust/src/cache.rs rust/src/effort.rs \
      rust/src/filters.rs rust/src/device.rs rust/src/ordered.rs rust/src/consts.rs
(no output)
```

`pipeline.rs`'s one line is `#[derive(Debug)]` on `DupGroup`. `Cargo.toml` gains
`ratatui` only, default features off, `crossterm` on — §7's allowlist exactly.

Then my own adversarial harness, which is independent of your evidence scripts:
eleven cases driving the real release binary through a real pty against trees it
generates, asserting on the disk afterwards. Nine passed outright; the two that
differed are binding items 1 and 2.

```
=== A: bulk 'keep the shortest path' vs --auto shortest, cwd inside root ===
    browser survivors: ['photos/aaaa.bin']
    --auto survivors: ['b.bin']
    browser and --auto keep the same copy: DIFFERENT
=== C: marking every copy in a group is refused at the keystroke ===
    exactly one copy was removed: ['b.bin'] ok
=== D: two rounds — a mark must name a record, not a screen row ===
    the second round removed the row it was pointing at: ['c.bin'] ok
    both removals reached the Trash: ['a.bin', 'b.bin'] ok
=== E: a hardlink cluster is one row and goes as one unit ===
    only the cluster went: ['z.bin'] ok
    both names of the inode reached the Trash: ['a.bin', 'also-a.bin'] ok
=== F: a record pinned from outside the tree is never offered, never taken ===
    the pinned copy survives: ['p.bin'] ok
    the outside name is untouched: True ok
    only the free copy reached the Trash: ['q.bin'] ok
=== G: bulk keep-shortest never marks a pinned copy ===
    only the middle copy reached the Trash: ['mm.bin'] ok
=== H: quitting and refusing the confirmation both remove nothing ===
    quit with a live mark removes nothing: ['a.bin', 'b.bin'] ok
    answering 'n' removes nothing: ['a.bin', 'b.bin'] ok
    terminal: raw released / alt left / left last: True ok
=== I: --hard puts nothing in the Trash ===
    one copy gone for good: ['b.bin'] ok    the Trash is empty: [] ok
=== J: a terminal too small to lay out ===
    nothing was deleted from a screen nobody could read: ['b.bin'] DIFFERENT
=== K: no terminal — the browser refuses rather than guesses ===
    -i into a pipe exits 2: 2 ok
    a plain run into a pipe exits 0: 0 ok
    neither run touched the tree: ['a.bin', 'b.bin'] ok
```

One correction to my own method, because it would otherwise look like two more
findings: my first pass also reported that the `--hard` confirmation never said
"This cannot be undone" and that the poisoned run never said "refused". Both
were artefacts of my harness, not of your program. ratatui repaints only changed
cells, so a word can reach the pty in pieces across frames, and my escape
stripping then joined the fragments without their spaces. Re-checked with
whitespace-insensitive matching against captured frames, the text is all there:

```
┌ Confirm ──────────────────────┐
│Deletepermanently:1files││Reclaims1.8KB││││Thiscannotbeundone.│
└ y  confirm      n  cancel ────┘
```

Finally, the one requirement neither your evidence nor your tests cover — a
group the cache vouched for. A disk outcome alone cannot prove this, since
"nothing was deleted" looks identical whether the group was refused after a byte
check or never formed. So I poisoned a single byte in the *middle* of a 24 MB
copy and put the mtime back to the nanosecond, leaving head, tail and every
sample point identical, so the group could only come from a remembered digest:

```
=== larger (25165824 bytes each) ===
    poisoned run's scan still grouped them: [['a.bin', 'b.bin']]
    poisoned run read 0 bytes during the scan
    survivors: ['a.bin', 'b.bin']        trash: []
    said 'Refusing to touch' after handing the terminal back: True
    said 'no longer match': True
```

Zero bytes read during the scan is the proof the group was believed, not seen.
The browser then read it, disbelieved it, removed nothing, and said so. Correct.

---

## The deletion path, and what `df03dde` fixed

There is one entry point and it is the right one. `run_selected`
(`trash.rs:386`) is `run_auto`'s sibling and takes the same `ProvenGroups`
token, whose field is private and which only `Scan::proven` can mint
(`pipeline.rs:54-72`) — unweakened, and the browser re-mints it every round
rather than carrying one across the loop (`tui/mod.rs:172-175`). The TUI
constructs no removal call of its own: `remove_record`, the Trash placement and
the pinned-record skip are all reached through `run_selected`, unchanged.

I reconstructed `df03dde` rather than taking the commit message for it. It
fixed two bugs, and **both were shippable data loss**:

1. **A mark was a screen position.** Before the fix, `run_selected` did
   `let index = proved.members[member]` on a position handed in by the UI. The
   browser renumbers its rows after every round (`model.rs:363-389` drops
   removed records and the emptied group), while the scan's own member list
   never shrinks. From the second round on, a mark named a *different file*, not
   a missing one. My case D is the shape of it: round one takes row 0, and in
   round two the rows are `[b, c]` while the records are `[1, 2]`. On the fixed
   code the second round removes `b.bin` and leaves `c.bin`, which is what I
   observed; on the old code the same keystrokes reach for record 0, which is
   the file already deleted.
2. **The survivor guard counted rows, not copies.** `if chosen.len() >=
   groups[group].members.len()` compares a selection against the membership the
   scan recorded, which after one round of deletion is no longer the question
   worth asking. Three copies, delete one, then select the remaining two: two is
   less than three, the guard passes, and the group loses every copy. That is
   the §3.2 invariant failing in the exact scenario the browser exists to
   create.

The fix is structural on both counts. A mark is now `(group in the scan, record
in the scan)` and is checked for membership (`trash.rs:396-405`); the guard
counts members that are unmarked *and still on disk* (`trash.rs:406-421`), which
is the real question; and the whole check loop completes before any removal
begins, so a request that would empty any group is refused entire rather than
half-executed. Both have regression tests (`model.rs:779`, `trash.rs:911`).

One thing worth stating because it is not obvious and is what makes the rewrite
safe: `run_selected` now ignores the verified groups `verify_attested` returns
(`_proved`, `trash.rs:432`) and deletes the record indices it was given. That is
only sound because `verify_attested` is all-or-nothing — a group survives only
if `settled.matched.len() == 1 && settled.matched[0].len() == group.members.len()
&& unreadable.is_empty()`, otherwise the whole group is refused
(`compare.rs:472-481`). If it could ever *split* a group instead, the guard
would have been computed against a membership that no longer exists and a
selection could empty a surviving subgroup. It cannot, today. If anyone ever
teaches it to split, `run_selected` must be revisited in the same commit.

I tried to break the guard from the outside and could not: every copy in a group
(case C, refused at the keystroke, `model.rs:207-210`), a hardlink cluster
(case E, one row, both names gone together, one unit), a record pinned from
outside the root (cases F and G — never offered, refused when toggled, skipped
by the bulk rules, and skipped again inside `run_selected` at `trash.rs:443` for
good measure), a group with only a pinned copy left as survivor (the guard
counts it, `trash.rs:980`), quitting with marks live, and answering `n`.

---

## Binding items

### 1. The bulk "keep the shortest path" ranks the wrong string

`Browser::apply_bulk` feeds the *display* path into the shared rule:

```rust
// model.rs:237-242
let members: Vec<(&str, f64)> = group.members.iter()
    .map(|member| (member.path.as_str(), member.mtime)).collect();
let keeper = rule.keeper_of(&members);
```

`Member.path` is built by `display_path` (`tui/mod.rs:261-267`), which is
relative to the working directory when that strips and absolute when it does
not. `--auto` feeds the record path instead (`trash.rs:88-94`,
`records[index].path()`, relative to the scanned root). When the working
directory sits *inside* the scanned tree the two disagree, because then some
members strip their prefix and others do not — the strings stop sharing a common
prefix, and `name_order` is no longer order-preserving between them.

Reproduced, same tree, same directory, one keystroke sequence apart:

```
tree/b.bin  and  tree/photos/aaaa.bin   (identical bytes), run from tree/photos

browser, b then 3 then ENTER then y  →  survivor: photos/aaaa.bin
--auto shortest -y --hard            →  survivor: b.bin
```

The oracle does not have this split: `KEEP_RULES["shortest"]` is
`min(..., key=lambda i: (len(fs[i].path), fs[i].path))` (`dedup.py:2226`) and
`fs[i].path` is the same field the non-interactive path uses — `.display` is a
separate property built *from* it (`dedup.py:150-161`) and is used only for
drawing. So this port disagrees with the oracle and with itself, and the user is
told "Keep the shortest path in every group" while the longer path is kept.

It also falsifies two comments that assert the opposite — `trash.rs:64-66` ("The
browser calls this too … so 'keep the oldest' cannot come to mean one thing
under `--auto` and another under a menu") and `model.rs:232-233`. Sharing the
function is not sharing the rule if the inputs differ. And it is not in LOG.md's
list of chosen divergences, so it is silent, which `M5-TUI.md`'s mission
paragraph forbids.

`oldest`/`newest` are unaffected — they compare `mtime`, which is the same
number in both paths.

**Change:** rank on the record path, not the display path. Carry the scan's
`record.path()` on `Member` alongside the display string and pass that to
`keeper_of`, so the display string is used only for drawing.

**I will verify by** building `tree/b.bin` and `tree/photos/aaaa.bin` with
identical bytes, running the browser from `tree/photos` with `b`, `3`, ENTER,
`y`, and requiring the survivor to be the same file `--auto shortest` keeps from
that same directory (`b.bin`); and by a model unit test over a group whose
display order and record order disagree, asserting `apply_bulk(Rule::Shortest)`
marks the record-order non-keeper.

### 2. Below 54×12 the confirmation is never drawn, but `y` still deletes

`view::draw` returns early when the frame is smaller than `MIN_WIDTH × MIN_HEIGHT`
(`view.rs:84-85`), which is right for the browse screen. But it returns before
the overlay match at `view.rs:206-224`, so in `Mode::Confirm` the dialog is not
drawn at all — while the model is still in `Mode::Confirm` and `y` still returns
`Step::Apply` (`model.rs:296-301`). A deletion therefore proceeds with the count,
the byte total and the Trash-vs-permanent mode never having been shown, which is
exactly what `M5-TUI.md` hard rule 3 forbids.

Captured on a 20×8 pty, keys `space`, ENTER, `y`:

```
painted:   'Terminal too small'   True
           'Confirm'              False
           'Reclaims'             False
survivors: ['b.bin']        (both copies were there before)
```

The threshold itself is right — 54×12 matches the oracle (`dedup.py:2314`), and
I checked both sides of the boundary: 54×12 draws the full confirmation, 53×11
draws the too-small message. The oracle is better behaved in this corner than
the port: its `confirm_and_apply` calls `_overlay` directly rather than through
`draw`, and `_put` clips per line (`dedup.py:2302-2309`), so a cramped terminal
still shows a clipped confirmation instead of none.

This is the lesser of the two items — the guard still holds, the deletion went
to the Trash, and reaching it means typing blind on an unreadable screen. It is
binding anyway because "no deletion without the numbers on screen" is a hard
rule, and the fix is a few lines.

**Change:** either draw the overlays before the too-small early return (the
oracle's behaviour), or refuse `Key::Apply` while the frame is below the
minimum and flash something like "resize to at least 54×12 to confirm".

**I will verify by** driving the browser on a 20×8 pty with `space`, ENTER, `y`
and requiring that either both copies are still on disk, or the frame that was
painted contains the count, the byte total and the mode.

---

## Non-binding notes

Fix if convenient; none of these block a re-claim.

- Nothing anywhere in the repo tests the browser's re-read of an attested group.
  The nine `trash::selection` tests all build fresh scans, and the seven
  end-to-end scenarios (`evidence/m5-end-to-end.txt`) are confirm, cancel, quit,
  `--hard`, last-copy, hardlinks and no-terminal. Deleting the `verify_attested`
  call at `trash.rs:428-437` would leave 101 tests green and the gate green —
  `m3-poison-refusal` covers `run_auto`, not `run_selected`. I verified the
  behaviour by hand (above) and it is correct; the hole is in the net, not the
  code. A `selection` test that warms the cache, poisons the middle of a member
  and asserts a refusal would close it.
- A mark is a bare `(usize, usize)`. The membership check (`trash.rs:401-403`)
  is the only thing that distinguishes a record index from a screen position,
  and for the first group of a small scan those coincide numerically — group 0's
  members are often exactly `[0, 1, 2]` — so the check would not catch a
  regression that reintroduced positions. Given that `df03dde` is precisely that
  confusion, a newtype pair would make the fix type-level rather than
  validation-level, in the same spirit as `ProvenGroups`.
- `put_the_terminal_back` runs twice on the normal path: once from the `Restore`
  guard as `draw_and_read` returns, once explicitly at `tui/mod.rs:91`. Both are
  idempotent so nothing is wrong, but the second call is dead on every path the
  first one covers.
- The confirmation counts records ("1 files") while the summary counts names
  ("2 files moved to Trash") when a hardlink cluster is involved. The oracle
  does the same thing (`dedup.py:2462` against `dedup.py:2511`), so this is
  parity and I am not asking for a change — but it is the kind of inherited
  wrinkle worth a line in LOG.md so the next reader does not "fix" it into a
  divergence.
- `evidence/m5-standards.txt` is the one evidence file with no generator script
  beside it, unlike the other four. Its numbers are right — I reproduced 101 and
  the gate's 33 myself — so this is a consistency nit, not a doubt.

---

## What I checked and found correct

- **One deletion path.** No removal call outside `trash.rs`; `ProvenGroups`
  intact and re-minted per round; `--stage size` scans refused before the
  browser opens (`main.rs:270-276`) as well as before `--auto` runs.
- **TTY only.** `has_terminal()` requires both stdin and stdout
  (`tui/mod.rs:34-36`). `-i` into a pipe exits 2 with the full message; a plain
  run into a pipe exits 0 and touches nothing; `--list` on a terminal does not
  open the browser (`main.rs:260-262`). All four confirmed by running them.
- **Terminal restore.** The panic hook is installed *before* the alternate
  screen is entered and restores before delegating to the previous hook
  (`tui/mod.rs:84-88`), so a panic message lands on the normal screen; the
  `Restore` guard is created after raw mode and before the alternate screen, so
  a failure between the two still unwinds clean (`tui/mod.rs:110-113`). Taking
  the action as a closure to test the unwinding case without a terminal to wreck
  (`tui/mod.rs:47-53`, test at `tui/mod.rs:448-471`) is the right call — I could
  not force a genuine panic in the real binary from the outside, and that test is
  the honest substitute. On every session I drove, I checked the pty's termios
  directly: canonical and echo off during, both back on after, alternate screen
  left last.
- **Model/paint separation is genuine.** `model.rs` imports nothing from
  crossterm or ratatui — its only crate import is `trash::Rule`. Cursor,
  selection, the survivor guard, the bulk rules and the plan are all plain
  functions, and the 27 model tests call them directly. `key_for`
  (`tui/mod.rs:183-226`) is the only crossterm-aware decision and is itself
  tested per mode, including that `n` means "next group" while browsing and "no"
  in the confirmation.
- **TestBackend snapshots are meaningful, not decorative.** They assert whole
  rows including full paths, the keeper glyph and the link badge
  (`view.rs:491-545`), that the cursor row is the one in reverse video, that the
  confirmation carries count, bytes and reversibility (`view.rs:565-590`), and
  that a path too long keeps its filename.
- **Information architecture against the oracle.** Header with root label and
  "N groups · X reclaimable"; the marked counter; "Group i of n" with
  "size × n copies"; full paths elided from the left so the filename survives;
  `○` would-survive / `●` marked / `⊘` pinned, matching the oracle's glyphs and
  meanings; `+n links` badge; the date column; the "… n more" overflow hint; the
  bulk menu's four labels word-for-word; the confirmation's wording and `y`/`n`
  footer; the two refusal flashes quoted exactly from the oracle. The keymap is
  the oracle's, including the undocumented `n`/`p`, plus ctrl-c from any mode.
  `human_size`'s switch to whole numbers at 100 and `human_count`'s thousands
  separators match. The seven divergences LOG.md records are the real ones and
  each is backed by the line of code it describes.
- **Claim hygiene.** The claimed numbers are reproducible and the transcripts are
  captured, not retyped: the four scripted evidence files match their
  generators' format strings character-for-character, including a 96-character
  truncation boundary and hand-aligned column padding; `m5-screens.txt`'s
  four byte figures (1.0 KB, 1.1 KB, 40 B, 1.0 KB) are each correct against
  `human_size`'s rounding for a 1050-byte file; `101` is exactly the number of
  `#[test]` attributes in `src/`; the gate transcript matches
  `conformance/run.py`'s own `"  {:24s} {}"` format. Commands in MILESTONES.md
  run as written from the directory it names. LOG.md's account is honest,
  including the two bugs it says the scripted runs caught.

---

## How to clear this review

Fix items 1 and 2, re-run `cargo test`, `clippy --all-targets -- -D warnings`,
`fmt --check` and `--gate all`, re-run `m5_end_to_end.py` and `m5_screens.py`
(item 1 changes what the bulk menu marks, so the screens transcript will move),
and re-claim M5 in MILESTONES.md with captured output. If you keep any part of
the display-path ranking deliberately, that is a divergence and belongs in
LOG.md with its reason — but I do not think you should.

The two items share a root cause worth naming, and it is not carelessness: both
are places where the browser's *view* of the world leaked into a decision. Item
1 lets a string built for drawing decide which file dies; item 2 lets the
drawing layer's early return decide whether a safety dialog exists. The
architecture you built is explicitly designed to prevent exactly this — the
model has no terminal in it, and `run_selected` re-derives the guard instead of
trusting the UI — and that design is why neither item is a data-loss bug. The
lesson is narrower than "separate model from view", which you did: anything
crossing from the view side into a decision (a string, an early return, a
position) needs the same suspicion `df03dde` applied to screen positions. That
commit found two of these itself. These are the last two I can find.

The deletion core is the strongest part of this milestone. The `ProvenGroups`
token carried into a second entry point, the guard that counts what will be on
disk rather than what is in a list, and the refusal of a whole request rather
than most of it are all the harder choice over the easier one, and they hold up
under everything I could throw at them.
