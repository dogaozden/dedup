# REVIEW-M5_2.md — maintainer review of the M5.2 claim

**Verdict: CHANGES**

Claimed at `a86af00` on branch `m5_2-ux`, reviewed at that tip in my own detached
checkout. Binding spec is `M5_2-UX.md`; `M5-TUI.md` and `M5_1-MENU.md` apply
unchanged, and REWRITE.md §0/§3 under both. I read `M5_2-UX.md`, the two
inherited laws, `REVIEW-M5_1.md`, the M5.2 claim in `MILESTONES.md`, the new
`LOG.md` entries and the seven new evidence files first, then verified
independently. Every number below is one I produced. `ls /Volumes` before the
gate run showed only `dedupbench` and a stray `\]` — no passport, and I touched
neither.

**Two binding items**, both in the evidence harnesses rather than in the
program. Neither is in the deletion path, the grammar, or the pulse — I tried
hard to break all three and could not. Item 1 is a regression suite this branch
silently broke and did not notice; item 2 is the half-finished half of the
repair the milestone is otherwise right to be proud of.

The milestone itself is in good shape. The grammar is tight enough that twenty
thousand adversarial strings and my own ninety-case probe through the real
binary could not make it lie or fall over; the pulse really is inert on a piped
run, by construction and by measurement; the humaniser split is the right call
and I am accepting it, with reasons; and the `m5_end_to_end.py` repair is real —
I built a mutant of the keep-rule law and watched the repaired script catch it.

---

## The binaries I tested

Per REVIEW-M5_1's own warning about a shared `CARGO_TARGET_DIR` handing back
four identical binaries:

```
worktree build      010d027ab92c13522030819ae87c13839811c5033d196fd5ef731f4bcd8127ea
independent build   c9ce62f7f89746e9564b321baa3e40afc3c024fcd8f652e55a4a2ed66e873c0c
main@fbc196d        19e6f02abd2639382179f14c44d3d7cb0326c33caec11d4475fed4fae9d08d56
```

The second is `git archive a86af00` extracted to scratch and built with its own
target directory; the third is `git archive fbc196d` the same way. The
independent build gives 181 tests, byte-identical `--help`, 8/8 on the pulse pty
suite and 0 differing on `m5_end_to_end.py` — the same as the worktree build,
which is what rules staleness out.

---

## Binding items

### 1. This branch broke `menu_mutants.py`, and nobody noticed

**Severity: high.** `menu_mutants.py` is the script that proves M5.1's one-table
rule is enforced by tests that *bite* — REVIEW-M5_1 leaned on it directly ("6
mutants, 6 caught") and M5.1 was only approved after two whole-program mutants
were added to it. This branch edits the flag table more than any branch since
M5.1, and it left that script unable to run.

**The defect.** `rust/evidence/scripts/menu_mutants.py:48` anchors the
`help-default-drifts` mutant on text this milestone rewrote:

```python
'        help: "ignore files smaller than this",\n        default: "10485760 bytes (10 MiB)",',
```

`cli.rs` now says:

```rust
        help: "ignore files smaller than this; plain digits are bytes, or say \
               512k, 100mb, 1.5g, 2TiB — all binary multiples",
        default: "10 MiB",
```

The script is well built and refuses to pretend — it aborts loudly rather than
skipping — but it aborts at mutant 3 of 8, so **five mutants never run at all**,
including `menu-drops-a-field`, `menu-skips-validation`, and both of the
whole-program terminal-death mutants M5.1 was approved on.

**Evidence.** Same script, my two builds:

```
$ python3 rust/evidence/scripts/menu_mutants.py        # in the m5_2-ux tree
mutant help-misses-a-flag           renders every flag but the first
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags, the_help_text_says_something_about_every_flag
mutant help-invents-a-flag          hand-writes a line for a flag the parser does not accept
  caught by the_help_text_and_the_parser_know_exactly_the_same_flags
mutant help-default-drifts: its anchor appears 0 times in src/cli.rs, not once — the source moved and this script is stale
MENU_MUTANTS_EXIT=1

$ python3 rust/evidence/scripts/menu_mutants.py        # in a main@fbc196d tree
  ... eight mutants ...
every mutant was caught
BASE_MENU_MUTANTS_EXIT=0
```

Green on `main`, broken here. That is this branch's doing, and the M5.2 claim
does not mention it: `MILESTONES.md:2135` says `tui-terminal-death.py` (4/4) and
`m5_1_menu_end_to_end.py` (7/7) were re-run, and `m5_screens.py` and
`m5_terminal_restore.py` are named as left broken. `menu_mutants.py` appears
nowhere in the M5.2 section. It was not run.

#### Verification recipe

Retarget the anchor onto the wording that is actually there. The anchor does not
need the `help:` line at all — the `default:` line alone is unique in `cli.rs`:

```python
    (
        "help-default-drifts",
        "says a default the code does not have",
        "src/cli.rs",
        '        default: "10 MiB",',
        '        default: "1 MiB",',
    ),
```

I applied exactly that to a scratch copy of `a86af00` and ran the script from
that copy:

```
retargeted the help-default-drifts anchor onto the M5.2 wording
EXIT=0
mutant help-default-drifts          says a default the code does not have
  caught by the_defaults_the_help_text_prints_are_the_defaults_the_code_has, clearing_a_field_puts_the_default_back, the_min_size_field_shows_a_size_a_person_can_read_and_type
mutant menu-writes-its-own-default  shows a default of its own instead of the table's
  caught by clearing_a_field_puts_the_default_back, the_form_shows_the_root_every_field_and_every_default, the_min_size_field_shows_a_size_a_person_can_read_and_type
mutant menu-drops-a-field           leaves a field out of the argument list it hands back
  caught by a_switch_toggles_and_reaches_the_options, the_command_line_prefills_the_fields_it_names
mutant menu-skips-validation        accepts whatever is typed and lets the scan find out
  caught by a_bad_profile_is_refused_the_same_way, a_field_is_refused_in_the_field_with_the_parsers_own_words, a_refusal_is_shown_beside_the_field_it_belongs_to
mutant one-of-three-answers         ...  caught: hangup/menu FAIL ... SPINNING
mutant drop-the-dead-terminal       ...  caught: hangup/menu FAIL ... (exit 101)
every mutant was caught
```

All eight caught, and M5.2's own new test
`the_min_size_field_shows_a_size_a_person_can_read_and_type` now catches two of
them as well — the guard got stronger, it just stopped being run.

To clear this item:

1. Retarget the anchor (or an equivalent that survives the new wording).
2. Run `menu_mutants.py` to completion and record `every mutant was caught` in
   the M5.2 evidence, beside the two suites already recorded there.
3. While you are in that file: it hardcodes `SCRATCH` to one session's
   scratchpad path (`menu_mutants.py:23-25`). That is M5.1's, not yours —
   mention it, do not fix it.

**I will verify by** running `python3 rust/evidence/scripts/menu_mutants.py`
from the branch tip and requiring exit 0 with eight mutants named and caught,
and by re-running it against a `main@fbc196d` tree to confirm both trees still
agree that every mutant is caught.

---

### 2. `m5_end_to_end.py` prints `NOT REACHED` and then counts the case as passing

**Severity: medium.** This is the file the milestone repaired, and the repair
stops one line short. The whole reason it needed repairing is that it silently
stopped testing the browser for a milestone and a half. The new detector is
right — it is the *reaction* that is missing.

**The defect.** `rust/evidence/scripts/m5_end_to_end.py:146-150`:

```python
    if output.decode("utf-8", "replace").count("\x1b[?1049h") < 2:
        # Said out loud, because this is the failure that hides: a screen in
        # front of the browser turns every key below into a test of something
        # else, and the case still prints numbers.
        print("    NOT REACHED: the browser screen never appeared")
```

The comment names the failure exactly and then lets it happen: `print` does not
touch `failures`. For the two cases whose expected outcome is *that nothing
happens* and which assert nothing about the screen, a run that never reaches the
browser reports every check `ok`.

**Evidence.** I copied the script to scratch and suppressed only the menu
`Enter`, so the browser is unreachable while everything else is untouched:

```
patched: the harness can no longer get past the menu
EXIT=1
NOT REACHED lines: 9
17 check(s) differing

=== cancelling the confirmation removes nothing ===
    expect: all three files survive and the Trash is empty
    NOT REACHED: the browser screen never appeared
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)

=== quitting with copies marked removes nothing ===
    expect: all three files survive
    NOT REACHED: the browser screen never appeared
    exit: 0 (ok)
    survivors: ['a.bin', 'b.bin', 'c.bin'] (ok)
    trash: [] (ok)
```

Two whole cases, six checks, green on nothing. The suite as a whole still fails
loudly (17 differing, exit 1), which is why this is medium and not high — but
"the suite failed somewhere else" is not the same as "this case is honest", and
these two cases are the ones a person would read to decide that cancelling and
quitting are safe.

#### Verification recipe

Make the detector a check:

```python
    if output.decode("utf-8", "replace").count("\x1b[?1049h") < 2:
        print("    NOT REACHED: the browser screen never appeared")
        reached_browser = False
```

and have `drive` return it alongside the exit code so each case can do
`failures += not check("reached the browser", reached, True)` — or, more simply,
raise, since no case in this file is supposed to stop before the browser.
Whatever shape you pick, the test is the same one I ran:

**I will verify by** suppressing the menu `Enter` in a scratch copy exactly as
above, and requiring that *every* case reports at least one differing check —
specifically that "cancelling the confirmation removes nothing" and "quitting
with copies marked removes nothing" no longer print all-`ok`. Then re-running
the unmodified script against both the branch build and a `main@fbc196d` build
and requiring `0 check(s) differing` and zero `NOT REACHED` lines on both.

---

## The spec deviation: **accepted**

`M5_2-UX.md` asks for the display round-trip "through one humanizer shared with
the TUI's `human_size`, and every string the tool displays must parse back to
exactly the bytes it came from". The builder split it into a rounding
`human_size` for display and an exact `exact_size` for settings.

**The spec asks for two things that cannot both be true.** `human_size` must
round — the browser shows `4.5 KiB reclaimable` for group sizes nobody chose —
and a rounded string cannot parse back to exactly the bytes it came from:
`human_size(1_500_000)` is `1.4 MiB`, which reads back as 1 468 006. A form
prefilled that way would be describing a scan the tool is not about to run,
which is a quieter version of the very bug this milestone was commissioned to
end. The spec's own worked example — `10 MiB`, not `10485760` — is what
`exact_size` produces. The builder resolved the contradiction in the direction
the spec's *purpose* points, and said so out loud rather than quietly. That is
the right call and I am accepting it without rework.

**Does the bond really stop them drifting?** Yes, and not only by assertion — I
built the drift. Twelve mutants, each one edit to a scratch copy of `a86af00`,
run through `cargo test`:

```
human-size-uses-1000             CAUGHT   (the_rounded_humaniser_still_reads_the_way_the_oracle_writes,
                                           the_walk_says_what_it_has_seen_and_the_rest_draw_a_bar, +3 view tests)
human-size-says-MB               CAUGHT   (a_size_is_shown_the_way_it_would_be_typed, +8 more)
settings-show-rounded            CAUGHT   (what_the_form_shows_is_what_the_parser_reads,
                                           clearing_a_field_puts_the_default_back,
                                           the_command_line_prefills_the_fields_it_names)
grammar-forgets-the-top-rung     CAUGHT   (one_ladder_is_read_and_written, +2)
multiplier-goes-decimal          CAUGHT   (one_ladder_is_read_and_written, +8)
suffix-drops-the-ib-dress        CAUGHT   (the_grammar_reads_what_the_table_says_it_reads, +8)
suffix-splits-by-byte-again      CAUGHT   (nothing_a_person_can_type_makes_the_parser_fall_over,
                                           junk_negatives_and_overflow_are_all_refused)
show-echoes-bytes                CAUGHT   (the_min_size_field_shows_a_size_a_person_can_read_and_type, +1)
help-default-hardcoded-bytes     CAUGHT   (the_defaults_the_help_text_prints_are_the_defaults_the_code_has, +2)
decimals-truncate-wrong          CAUGHT   (every_size_the_tool_would_show_as_a_setting_parses_back_to_itself, +10)
bar-may-run-past-its-end         CAUGHT   (the_bar_never_runs_past_its_own_end)
pulse-is-never-off               SURVIVED cargo test — see below
```

The third one is the deviation reversed: making settings use the rounding
humaniser is caught by three tests, so the split cannot silently collapse back.
`human-size-uses-1000` and `human-size-says-MB` are the drift in the other
direction — the display wandering off the grammar — and both are caught. So the
two functions are bonded on the ladder, on the base and on the spelling.

**Does every SETTING surface use `exact_size`?** I grepped every call site
rather than trusting the prose:

- `human_size` is reached from `pulse.rs:210` (bytes walked), `pulse.rs:246`
  (rate), `tui/mod.rs:414`, `tui/view.rs:117,124,175,237`. Every one of those is
  a number a person reads and never types back. None is a setting.
- `exact_size` has exactly one production call site: `cli.rs:153`, inside
  `Flag::show`.
- `Flag::show` has exactly one production call site: `tui/menu.rs:148-149`, the
  menu prefill, on both the first-value and the repeated-flag branch.
- The help default is the table literal `"10 MiB"`, bonded to the code by
  `assert_eq!(said("--min-size"), exact_size(bare.min_size))` and
  `assert_eq!(parse_size(said("--min-size")), Ok(bare.min_size))`; the menu's
  empty-field display (`menu.rs:66`) falls back to that same literal.

So the settings surfaces are the help default and the menu prefill, and both are
exact. `Flag::show` special-cases only `--min-size` and echoes everything else
verbatim, which the test pins deliberately — that is the minimal design, not an
oversight.

The only rounding the milestone still owns is the browser's, which is what the
spec wanted rounded. Ruling: **accept, no rework.**

---

## Evidence — what I ran myself

### Standards and the gate, at `a86af00`

```
$ cargo test
test result: ok. 181 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s
TEST_EXIT=0

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.06s
CLIPPY_EXIT=0

$ cargo fmt --check
FMT_EXIT=0

$ python3 conformance/run.py --gate all \
    --candidate 'rust/target/release/dedup-rs {root} --report-json {json} {flags} {stage}'
33 check(s), 0 failed
gate all: green
GATE_EXIT=0
```

`main@fbc196d`, built and tested by me: `164 passed`. The claim's 164→181 is
right.

### The grammar, probed through the real binary

Ninety words fed to `dedup-rs <tree> --min-size <word>` — not to a unit test —
with the effective byte value read back out of which size classes still formed
groups. Everything the table says it reads, it reads; everything it says it
refuses, it refuses, with one message shape:

```
'1k'/'1K'/'1kb'/'1KB'/'1kib'/'1KiB'/'1 KiB'/'1 kib'   ACCEPTED  min_size in [1024, 1024]
'1.25k'  [1025,1280]   '1.5k'  [1281,1536]   '1.9k'  [1537,1945]
'0.0009765625k'  [0,1]        '10 MiB' == '10485760' == '10m'
'15e' and '1.5e' accepted; '16e', '17e' refused — the line is where u64 stops
'18446744073709551616'        REJECTED  not a size: "…" — plain bytes or 512k, 100mb, 1.5g
'18446744073709551615.9k'     REJECTED  (decimal overflow, the case worth having)
'-1' '-1k' '- 1' '+1' '1e9' '0x10' '1,000' '1_000' '' ' ' 'k' 'mb' 'b' 'B'
'1.2.3' '.' '1..2' '1 2' '10 MB extra' '1k2' '1 kk' '10  mb' '1kib b' '12qb'   all REJECTED
'1€' '€' '1 €' '1k€' '١٢٣' '1２' '1kв' '🙂' '1 MiB🙂'                            all REJECTED
'1<TAB>k' '1<LF>k'                                                            REJECTED
```

The byte-vs-char fix holds: I rebuilt the pre-fix `split_at(1)` as a mutant and
`cargo test` caught it in two tests, one of which is the twenty-thousand-string
fuzz. Multi-byte input refuses rather than panics, everywhere I could reach it.

Two leniencies, both from `str::trim` and both harmless: a leading U+00A0
no-break space (`"\xa0100mb"`) and a trailing newline (`"1k\n"`) are accepted at
the right value. Worth knowing, not worth changing.

### The pulse pty suite, run alone, with a `main` build to diff against

```
$ python3 rust/evidence/scripts/m5_2_pulse.py <worktree build> <scratch> <main@fbc196d build>
a piped run says nothing at all                ok (stdout byte-identical to main, 5188 bytes; stderr empty on both; 5 counters equal at -j1)
a terminal gets a line, and gets it back       ok (5 repaints, stages Reading small files+Comparing, bar Reading small files 5685→12002/12002; Comparing 6→6/14, cleared before the browser)
the walk counts files out loud                 ok (3 repaints, counts 49443…95303 against 100000 files seen, never ahead and never backwards)
--quiet means quiet on a terminal too          ok (nothing between the menu and the browser, 9 groups found)
--list down a pipe, the line on the terminal   ok (37729 bytes of list down the pipe, 1 repaints on the terminal, neither in the other)
the menu takes 100mb, 1m and 1k                ok (100mb→0 group(s), 1m→7 group(s), 1k→9 group(s))
bytes on the command line, a size on screen    ok (shown as 2 MiB, and 2 MiB is what the scan used)
a size that is not one is refused in the field ok (refused in the field, in words that say what to type instead)
every case behaves — PULSE_EXIT=0
```

Same eight green on the independent build. The M5-TUI restore law is checked
where it matters: case 2 asserts that the bytes between the menu giving the
screen back and the browser taking it **end with `\r\x1b[2K`**, so the alternate
screen provably opens on a cleared line and nothing of the pulse is left in
scrollback.

`cargo test` could not catch a pulse that is never off — that mutant survived —
so I built it as a release binary and drove the pty suite with it:

```
$ python3 rust/evidence/scripts/m5_2_pulse.py <pulse-is-never-off mutant> …
a piped run says nothing at all                FAIL: stderr was not empty: b'\r\x1b[2K'
--quiet means quiet on a terminal too          FAIL: --quiet still wrote '\r\x1b[2K  Sampling deep…  ░░░░…   0%  0/12 · 223 MiB/s\r\x1b[2K'
2 case(s) wrong
```

Exactly the two laws that matter, and only those two. So the inertness and
`--quiet` guarantees do have a live regression guard; it just lives at the pty
level rather than in `cargo test`.

### Piped runs, diffed against `main@fbc196d` myself

Six non-interactive shapes on the same tree, my own comparison rather than the
script's:

```
  -j1  --list             exit 0/0  stdout identical  stderr identical  stderr-empty yes/yes  report identical
  -j1  (flagless)         exit 0/0  stdout identical  stderr identical  stderr-empty yes/yes  report identical
  -j1  --stage size --list exit 0/0 stdout identical  stderr identical  stderr-empty yes/yes  report identical
  -j8  --list             exit 0/0  stdout identical  stderr identical  stderr-empty yes/yes  report identical
  -j8  (flagless)         exit 0/0  stdout identical  stderr identical  stderr-empty yes/yes  report identical
  -j8  --stage size --list exit 0/0 stdout identical  stderr identical  stderr-empty yes/yes  report identical
```

`cmp -s` clean on stdout, on stderr, and on the whole report JSON — which
carries every counter — at both job counts. Nothing a pipe can see has moved.

### Inertness, re-measured

```
$ python3 rust/evidence/scripts/m5_2_inertness.py <worktree build> <main@fbc196d build> <scratch> 15
== stderr piped, -j1 ==
  opens                 51052          51052   same
  bytes_read        647872512      647872512   same
  files_seen            30516          30516   same
  bytes_seen        640221184      640221184   same
  seeks                 51260          51260   same
  groups                  254            254   same
  m5.2         1.288s median, 1.034–6.007
  main         1.229s median, 1.027–6.478
  difference   +4.8% (+0.059s), against a 5.452s spread within a build

== stderr piped, -j8 ==
  opens/bytes_read/files_seen/bytes_seen/groups   all same
  m5.2         0.820s median, 0.807–1.008
  main         0.824s median, 0.810–0.889
  difference   -0.4% (-0.004s), against a 0.201s spread within a build

the pulse costs a piped run nothing that can be measured — INERT_EXIT=0
```

**Every counter is identical at both job counts**, on a 30 516-file tree, which
is the part of this that is not a measurement at all and therefore the part that
carries the weight. My machine was not quiet — `uptime` showed load 4.07 with
other agents' `rustc` and `clang` at ~100% — so treat my `-j1` median as noise
(a 5.45 s within-build spread from one 6 s outlier makes it meaningless), and
the `-j8` figure, −0.4% against a 0.201 s spread, as the honest one. It agrees
with the builder's −0.2%.

The structural claim holds independently of any timing: `Pulse::off()` is
`Pulse(None)`, every method is one `if let Some(live) = &self.0` that falls
through, no thread is spawned, and the never-off mutant proves the off branch is
the one a piped run takes — it writes `\r\x1b[2K` to a piped stderr where the
real build writes zero bytes.

### `m5_end_to_end.py` — the critical audit

Repaired script, my build, run by me:

```
$ python3 rust/evidence/scripts/m5_end_to_end.py <worktree build> <scratch>
... ten cases ...
0 check(s) differing            EXIT=0     "NOT REACHED" lines: 0
```

Same script against my `main@fbc196d` build: `0 check(s) differing`, exit 0.
Same script against the independent build: `0 check(s) differing`, 0 `NOT
REACHED`. So the 0-differing is real on both builds and every one of the ten
cases genuinely reached the browser — that is what the claim says and it is
true.

**Are the checks still meaningful?** I did not take the green for an answer. I
built the keep-rule law's own defect as a release binary — `apply_bulk` ranking
`member.path` (what is drawn) instead of `member.name` (what the scan and
`--auto` rank), which is REVIEW-M5's binding item 1 — and ran the repaired
script against it:

```
=== bulk keep-shortest keeps the copy --auto shortest keeps ===
    expect: b.bin survives both, even run from inside the tree where the drawn paths rank the other way round
    exit: 0 (ok)
    browser survivors: ['photos/aaaa.bin'] (DIFFERENT)
    --auto survivors: ['b.bin'] (ok)
    browser and --auto agree: ['photos/aaaa.bin'] (DIFFERENT)
2 check(s) differing
```

It bites, and it bites on exactly the pair of checks it should: the browser kept
the wrong copy, `--auto` kept the right one, and the case that exists to say
they must agree said they did not. That case runs with `cwd` inside the tree,
which is precisely the case the absolute-`HOME` half of the repair made
possible. The repair is real; it is not vacuous green.

Reading the repair line by line, the three parts are each sound. Waiting for
`"before the scan"` on the ANSI-stripped stream and only then sending `Enter`
is the right condition (the terminal is canonical until the browser takes it,
so a key sent earlier is echoed at the scan and lost). Waiting for the *second*
`\x1b[?1049h` is the right way to know the browser rather than the menu is up.
`os.path.abspath(home)` matters for the two cases that run with `cwd` inside the
scanned tree — without it a relative scratch path puts the run's own report,
cache and Trash into the tree and then counts them among the survivors. And
starting the pty roomy and shrinking it only once the browser holds it is the
only honest way to reach a too-small browser now that the menu has a floor of
its own — a window being resized, which is what a real user does.

Its one shortfall is binding item 2.

### The rest of the M5 / M5.1 battery, at the tip

```
m5_dispatch.py            EXIT=0   every case left both files where they were
m5_screens.py             EXIT=0
tui-terminal-death.py     EXIT=0   keys/menu, keys/browser, hangup/menu, hangup/browser — 4 PASS
hangup_deterministic.py   EXIT=0   run 1-3: LEFT(menu, exit=1, ≤0.01 CPU-s)   SPUN 0/3
m5_1_menu_end_to_end.py   EXIT=0   every case behaves (7/7)
keeprules.py              EXIT=0   0 rule check(s) differing
verify_deletion.py        EXIT=0   4 check(s), 0 failed
m5_terminal_restore.py    EXIT=1   5 check(s) differing   ← pre-existing, see below
menu_mutants.py           EXIT=1   stale anchor            ← binding item 1
```

### `--help` and the one-table rule

`--help` and `-h` both exit 0 with byte-identical output. The `--min-size` line
documents the grammar as the spec asks and shows `(default: 10 MiB)`; `--quiet`
now says `no progress line while the scan runs` with default `a progress line,
on a terminal`, and the old "accepted for the Python tool's sake; ignored here"
has moved to `--verify` alone, where it belongs. With binding item 1's anchor
fixed, all four help/menu table mutants are caught, in both directions.

---

## Pre-existing defects — confirmed, and not M5.2's to fix

I built `main@fbc196d` myself and drove both defects on both binaries with my
own script.

**The SIGINT terminal wreck.** `kill -INT` while the browser holds the terminal:

```
### m5_2-ux @ a86af00 ###                      ### main @ fbc196d ###
A. kill -INT while the BROWSER holds the terminal
   exit: -2                                       exit: -2
   termios before/during/after:                   termios before/during/after:
     (True,True)/(False,False)/(False,False)        (True,True)/(False,False)/(False,False)
   raw mode released: False                       raw mode released: False
   alt screen enters/leaves: 2/1                  alt screen enters/leaves: 2/1
   left after it entered: False                   left after it entered: False
```

Identical. Raw mode kept, alternate screen still up, `ISIG` never gets a chance
because raw mode suppressed it and the default disposition terminates without
unwinding. For contrast, Ctrl-C *typed at the browser* is clean on both
(`exit 0`, raw mode released, 2/2 alt screens) — it is the signal, not the
keystroke, exactly as the branch's last commit message says.

**The menu's dropped Ctrl-C.**

```
B. ctrl-C typed at the pre-flight MENU
   exit: None, still running after 5s: True       exit: None, still running after 5s: True
   raw mode released: False                       raw mode released: False
   alt screen enters/leaves: 1/0                  alt screen enters/leaves: 1/0
```

Identical. And one the claim does not mention, which I found while checking:
`kill -INT` while the **menu** holds the terminal wrecks it the same way on both
builds (`exit -2`, raw mode kept, alt screen 1/0). Same root, same owner.

**The M5 restore suite.** `m5_terminal_restore.py` reports 5 checks differing at
the tip. I ran it against `main@fbc196d` and diffed the two transcripts:

```
$ diff <branch output> <main@fbc196d output>
DIFF_EXIT=0
```

Byte-identical. Both defects, and this suite's failure, **predate this branch**
and are backlog, not M5.2 blockers. I am saying so explicitly: they do not block
this milestone. They do want an owner — M5.1's — and the restore suite is a
third script that the menu's arrival broke and nobody repaired.

---

## Non-binding notes

1. **`MILESTONES.md` quotes a stale copy of its own evidence.** Line 1935
   introduces a fenced block as `evidence/m5_2-pulse.txt` and two of its eight
   lines are not in that file — "a terminal gets a line" (`5 repaints, stages
   Reading small files+Sampling+Comparing … 5469→12002/12002` in the doc vs `4
   repaints, stages Reading small files+Comparing … 7234→12002/12002` in the
   file) and "the walk counts files out loud" (`59320…59320` vs `62281…62281`).
   The other six are byte-identical, and my own runs produce a third and fourth
   set of numbers, so this is timing-dependent output pasted before the last
   re-capture — not a fabrication. Fix it the way REVIEW-M5_1's stale-number
   note was fixed: correct it and say in `LOG.md` that you did.

2. **`human_size` has one string the grammar cannot read back.** Above about
   15.95 EiB it rounds to `16.0 EiB`, and `16e` is 2⁶⁴, which `parse_size`
   correctly refuses. No tree will ever get there, and the round-trip law is
   `exact_size`'s, not `human_size`'s — but
   `what_the_browser_shows_is_at_least_readable_back` quietly shifts its fuzz by
   4 bits to stay under it. The comment says why; a reader would trust it more
   if the test asserted the boundary rather than avoided it.

3. **`m5_2_pulse.py` is load-sensitive.** Run alongside my own `cargo` work it
   failed the `--quiet` case with `exit -9 … [the run did not exit; killed]` —
   the 120 s patience ran out. Run alone it is 8/8 on both builds. Nothing is
   wrong with the program; the script deserves a line in its docstring saying it
   wants a quiet machine, since on this box it will not always get one.

4. **The inertness gate cannot fail at `-j1` on a busy machine.** The noise
   floor is `max(max(t) - min(t))` across both builds, so one 6 s outlier makes
   any difference "inside the noise". The builder's own `-j1` run had a 3.6 s
   spread and mine had 5.45 s. Consider a trimmed spread (interquartile, or drop
   the top decile) so the gate keeps its teeth; the counters, which are the real
   evidence, are unaffected either way.

5. **`tools/passport-measure.sh`'s `/dev/tty` guard is beyond spec and correct.**
   The spec only asked for `--min-size 1m`. I audited the extra guard with a
   stub binary and a scratch tree — nowhere near `/Volumes`. With a real
   controlling terminal (`pty.fork`), the scan's stderr is a terminal, the pulse
   line goes to the terminal, and the log gets the timings and none of the line.
   Without a controlling terminal (`setsid`, everything piped), `( : >/dev/tty )`
   fails, `LIVE` stays `/dev/stderr`, the scan's stderr is a pipe so the pulse is
   off anyway, and the measurement is still written. Both directions correct, no
   truncation risk, no lost measurement. Accepted as written.

6. **Not this branch's:** `menu_mutants.py` hardcodes one session's scratchpad
   path at lines 23-25; `m5_screens.py` passed for me at both tips but the claim
   reports it dying with `EIO` — machine-dependent, and M5.1's either way.

---

## What I checked and found correct

- 181 tests, clippy and fmt clean, `--gate all` 33/33 green, all re-run by me at
  `a86af00` on two independently built binaries.
- The grammar reads every form the table says it reads and refuses everything
  else, through `Flag::apply` — the single caller — so the command line, the
  form and `--help` cannot disagree. Ninety words through the real binary.
- Multi-byte input refuses rather than panics; the `split_at(1)` mutant is caught
  by two tests including the fuzz.
- Overflow is refused at exactly the u64 boundary, decimals included
  (`18446744073709551615.9k`), and `15e` still parses.
- The stage labels are the oracle's, verbatim, all six of them; the bar is the
  oracle's 24 cells; the repaint is the oracle's `\r\033[2K`; the beat is 100 ms.
- Every number the line shows comes from a counter the scan keeps anyway —
  `walk.rs` tells it the same `metadata.len()` it adds to `bytes_seen`, the
  reading stages tell it one file at a time from inside the pool, `compare.rs`
  tells it a group at a time as each settles, and totals are the real candidate
  vectors' lengths. The rate is `Effort::bytes_read` differenced from the stage
  start, which is the same counter the report prints. Nothing estimates. The
  `done.min(total)` clamp is deliberate, documented, and pinned by a test my
  mutant confirmed bites.
- The line is cleared before anything else is written: `main.rs` calls
  `pulse.finish()` before the hint, before the report, before `--list`'s stdout
  and before `browse()`, and `finish` joins the ticker before wiping so nothing
  can paint afterwards. `Drop` calls it too, so a panicking scan still gives the
  terminal back.
- A piped run is byte-for-byte `main` on stdout, stderr and the whole report
  JSON, at `-j1` and `-j8`, on three shapes each.
- `--quiet` is a real setting, documented, and its old no-op arm is gone from
  `--quiet` and left only on `--verify`.
- The menu prefills `4096` as `4 KiB` and `104857600` as `100 MiB`, hands those
  very words back to the parser, and the scan runs on them — proved end to end
  on a pty, not only in a snapshot.
- Nothing deletion-adjacent moved: `ProvenGroups`, `verify_attested`, the
  survivor guard and the hardlink-cluster rule are untouched, `keeprules.py` and
  `verify_deletion.py` are clean, and the browser still agrees with `--auto`.
- Diff hygiene is clean. Twenty-six files, twenty-five under `rust/`, and the
  twenty-sixth is `tools/passport-measure.sh`. No `bench/`, `rbench/`, `xbench/`,
  `conformance/`, `dedup.py` or `tests/` file is touched. Every changed line
  traces to the spec or to a named fix.

---

## How to clear this review

Fix binding items 1 and 2 with the recipes above, re-run `menu_mutants.py` to
completion and `m5_end_to_end.py` against both builds, take or decline the
non-binding notes in writing, and re-claim M5.2. The milestone needs nothing
else — the grammar, the humaniser split, the pulse and its inertness are all
sound, and I could not break any of them.

---

## Re-review closure — 2026-08-24, maintainer

The fix round landed as `285d381`: both binding items implemented with the
recipes above, notes 2–4 taken in writing, note 6 left alone as instructed,
and the builder's self-audit converted fifteen unbacked evidence lines into
`m5_2_signals.py`, now a gate.

The reviewer began the re-review and confirmed both fixes match its own
recipes verbatim, then was terminated mid-verification by a server-side API
error before it could run its independent sabotage pass. Rather than
restart the review, the maintainer completed the remaining verification on
the MERGED tree (`c3a8750`, main):

- `cargo test`: 182 passed, 0 failed. clippy `-D warnings` clean,
  `fmt --check` clean.
- `menu_mutants.py`: every mutant was caught, 8 of 8 — including both
  whole-program terminal-death mutants (hangup/menu SPINNING, exit-101).
- `m5_end_to_end.py`: 0 checks differing, browser reached in every case.
- `conformance --gate all` (templated `{stage}` form): 33 checks, 0 failed,
  green.

**M5.2 is APPROVED and merged.** The re-review's one unfinished step — the
reviewer re-running its own suppressed-Enter sabotage rather than trusting
the builder's capture — is recorded here as completed only in the builder's
captured form (26 differing, 9 NOT REACHED); the maintainer judged the
mutant battery and live e2e re-runs above sufficient corroboration.
