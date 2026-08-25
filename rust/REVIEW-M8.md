# REVIEW-M8.md — maintainer review of the M8 claim

**Verdict: APPROVED at `4810ffd`.** See the re-review at the end of this file.
All four binding items below are discharged and all three non-binding notes
were taken.

---

**First round — verdict: CHANGES — four binding items, none of them a design
objection.** The design is right, the fence holds under a harder corpus than
the branch's own, and the deletion path is guarded the way the claim says it
is. Three of the four items are one-line fixes; the fourth is a test that does
not exist yet for the milestone's one absolute fence.

M8 is claimed at `be65b8a` on `m8-live`, off `main@a362ffa` — both verified by
`git merge-base`, not taken from the claim. The binding spec is `M8-LIVE.md`.
I read the spec, the M8 section of `rust/MILESTONES.md`, the M8 entry in
`rust/LOG.md`, `REVIEW-M7.md` and `REVIEW-M5_2.md` for precedent, and the nine
`evidence/m8-*.txt` files, and then verified everything independently in a
worktree of my own at `/Users/dogaozden/AI_Projects/dedup-m8-review`, with
builds of `main@a362ffa` and `m8-live@be65b8a` made from fresh `git archive`
extractions beside it; **no number below is taken from the builder's scripts.**
Where I re-ran a builder script I say so and treat it as a reproduction, not as
evidence.

The branch also carries `cc23a43`, which is `main@636554f`'s `claim_audit.py`
fix under a different sha. I checked with `git patch-id --stable`: both sides
produce `f2f61c1c…`, so it is the same change cherry-picked and it will merge
without a conflict. Noted, not an item.

---

## The binaries I tested

`REVIEW-M5_1` warned that a shared `CARGO_TARGET_DIR` can hand back a
false-identical binary, and `REVIEW-M5_2`'s own closure was bitten by a stale
`target/release`. So, explicitly:

```text
$ echo "CARGO_TARGET_DIR=[${CARGO_TARGET_DIR:-unset}]"
CARGO_TARGET_DIR=[unset]

$ shasum -a 256 .../builds/base/rust/target/release/dedup-rs
6430d7dda5d326fed8e2e25ae0f75d72062fae36774ac7fdc4702846665dbe51   main@a362ffa
$ shasum -a 256 .../builds/cand/rust/target/release/dedup-rs
38a65a86951292bb22967b4f7dd146f663babf3eda2cc2c60a8d479da5063a83   m8-live@be65b8a
$ shasum -a 256 dedup-m8-review/rust/target/release/dedup-rs
cc85f177e334bfb6f5f4e8925a2b80a1ccf8c7f10aada150471a3130cd4ff35f   m8-live@be65b8a
```

Three separate build directories, three distinct hashes. The last two are the
same source: a release build here embeds its own absolute path, so a hash is
only comparable **within** one build directory. That is worth writing down,
because it is what makes the claim's own staleness check
(`touch && rebuild && compare`) the right method rather than a ritual — it
compares within one directory. My own runs of that check agreed: after every
mutant below, restoring the source and rebuilding returned
`38a65a86…` byte for byte.

---

## The absolute fence — my own derivation

The fence is the item the spec says to attack first, so it is what I attacked
first, and I did not reuse the branch's corpora to do it.

`rv_fence.py` is mine, written from `M8-LIVE.md`'s fence sentence rather than
from `m8_freeze.py`. Ten corpora, chosen to include the shapes the branch's six
do **not** have: an empty tree, symlinks (to a file, to a directory, dangling,
and a directory loop), thirty zero-byte files, unicode and space-bearing names,
a twelve-deep nesting, files astride the **10 MiB default floor** so a run with
no `--min-size` is itself exercised, and hardlink clusters beside a true copy.
Seventeen invocations each plus a cold/warm pair: 190 cases. Every case
compares stdout, stderr, exit code and the whole report including every effort
counter.

```text
$ python3 rv_fence.py compare fence/base1 fence/cand
base-vs-candidate (the fence)
190 cases compared, 0 differing
FENCE HELD: every non-interactive output and every effort counter is byte-identical
FENCE_EXIT=0
```

**And the gate has teeth, which I proved rather than assumed.** I took the
branch's own falsification and re-ran it against my corpora: change
`pipeline.rs`'s `if live.is_some()` to `if true`, so the deferred stage 5 leaks
onto the frozen path, rebuild, recapture:

```text
$ python3 rv_fence.py compare fence/base1 fence/mut
  DIFFER  onegiant/warm-1   [stdout, report.bytes_read=59314176 vs 2969600, report.groups, report.seeks=163 vs 55]
  DIFFER  onegiant/warm-2   [stdout, report.bytes_read=57147392 vs 0, report.groups, report.opens=5 vs 0, ...]
  DIFFER  sameends/list     [report.bytes_read=663552 vs 626688, report.seeks=36 vs 27]
  DIFFER  symlinks/list     [report.bytes_read=131094 vs 131072, report.seeks=8 vs 4]
  ...
190 cases compared, 86 differing
```

The branch reports 56 of its 84; I get 86 of my 190. Different corpora, same
verdict: the fence is falsifiable, and it was falsified.

**Where the fence is real and where it is merely untested.** These are not the
same thing and the claim does not separate them. Every one of the 190 cases is
a run with no terminal, so `live_wanted`'s first clause — `tui::has_terminal()`
— is already false and the remaining three are never evaluated. The freeze
proof therefore proves a great deal about *shared pipeline code* and nothing at
all about `live_wanted`'s own guard. I checked that directly by deleting two of
its clauses:

```text
mutant: live_wanted loses `&& options.report_json.is_none() && options.stage.is_empty()`
$ cargo test
test result: ok. 250 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Nothing noticed. That is binding item 3. To be clear about what I am and am not
saying: **the fence holds** — I verified the real binary's behaviour on a real
terminal, below — but two of its four clauses are currently guarded by nothing
at all, and the milestone whose whole premise is one absolute fence should not
leave half of it untestable.

I also confirmed the conformance harness really does pass `--report-json` on
every invocation, which is the claim's defence-in-depth argument:
`conformance/run.py:158` and `:718` both hard-code it into the template. That
part of the claim is exactly true.

### What a terminal actually gets

The freeze proof cannot see the terminal side, so I measured it. `rv_tty_matrix.py`
drives each combination on a real pty (winsize set, `ESC[6n` answered) and asks
whether a group is ever labelled `candidate` — which only the live browser does.

```text
                             m8-live@be65b8a          main@a362ffa
case                         exit alt cand report     exit alt cand report
bare interactive             0    2   True  False     0    2   False False
--list                       0    0   False False     0    0   False False
--list --interactive         0    2   True  False     0    2   False False
--report-json                0    2   False True      0    2   False True
--stage size                 2    1   False False     2    1   False False
--auto shortest --yes        0    0   False False     0    0   False False
```

`--list`, `--auto`, `--report-json` and `--stage size` all stay on the frozen
path, on a terminal, with the report still written. That is the fence, held,
measured on the thing itself.

`--list --interactive` is the exception, and it is binding item 4 — not because
the behaviour is wrong, but because the code says something about it that is
not true.

---

## Prove-on-delete — hand-traced, then sabotaged

I traced every path from a mark to a removed file.

`Mark` is `{ group, record }` (`model.rs:29`) — a stable group id and an index
into the scan's records, never a screen position. `Browser::selection`
(`model.rs:356`) resolves marks through `self.at`, the id→position map. On the
scan side, `serve` (`pipeline.rs:639`) proves exactly the marked groups, and
only ids whose verdict is `whole` reach `standing`; `keeping` is then the marks
filtered to `standing`, so **a mark on a group that failed its proof is dropped
before `run_selected` is called at all**. `resplit` replaces the failed group
with its first real byte-equality class and pushes the rest as new ids, every
one of them flagged proven, because the comparison that produced them is the
proof. A class of one is dropped, which is the honest thing: a file that
matches nothing is not a duplicate.

`ProvenGroups` did not move, and the "lend the records to a real `Scan`" trick
is sound: `Scan::proven` is still the only constructor, `serve` mints the token
from a real `Scan` built by `std::mem::take` on the records and takes them back
on the next line. `run_selected` cannot tell it from `--auto`'s. I checked the
`proven: true` on that lent scan is honest and it is: every group it carries is
either compared this scan or flagged `attested`, and `run_selected`
(`trash.rs:434`) sends every attested group through `verify_attested` before
removing anything from it.

Two further guards I confirmed by reading rather than trusting: `run_selected`
refuses the **whole** request if any mark's record is not in its group
(`trash.rs:403`), and the survivor guard counts copies that will still be on
disk afterwards via `still_there`, not rows in a list (`trash.rs:411-422`).

Then I sabotaged it. `prove_marked`'s `whole` is the single line that decides
whether a candidate may be deleted from, so I inverted it both ways:

```text
mutant                                        cargo test   m8_live_end_to_end.py
the-proof-always-passes  (whole = true)       SURVIVED     CAUGHT
the-proof-always-fails   (whole = false)      SURVIVED     CAUGHT
```

Both survived `cargo test`. Following `REVIEW-M5_2`'s precedent — a survivor is
escalated to a higher-fidelity harness before any conclusion is drawn — I built
each as a release binary and ran the pty battery:

```text
=== MUTANT A — the proof always PASSES
FAIL  a failed proof deletes nothing  (1.3s)
        THE PROOF DID NOT HOLD: liar files went: ['liar/l1.bin', 'liar/l2.bin']
        a liar file reached the Trash: ['l1.bin', 'l2.bin']
        the screen never said the group was refused

=== MUTANT B — the proof always FAILS
FAIL  mark-and-apply during a running scan  (2.4s)
        nothing was removed at all
```

So the deletion-honesty guard is real and it bites in both directions; it
simply lives at the pty level rather than in `cargo test`. That is the same
shape M5.2 found for `pulse-is-never-off`, and I rule it **accepted, no
rework** — with a non-binding note (1) that the milestone's most
consequential line has no terminal-free guard.

Both harnesses `sys.exit(main())` on their failure count, so their exit codes
are load-bearing. My first pass reported `E2E_EXIT=0` beside a failing run;
that was my own error — `PIPESTATUS` after a subshell measures the subshell —
and not the harness's. Recorded because the transcript above would otherwise
look like evidence of a defect that does not exist.

---

## What the proof does after it refuses — binding item 1

This is the one real defect I found, and it is on the deletion path.

`serve` has three ways to answer. Two of them — `Applied` on success, and
`Applied` with `refused: broke` when nothing was removable — reach
`Browser::absorb`, which ends with `self.marked.clear()` (`model.rs:598`,
"forget every mark, including the ones a refused group still holds"). The third
does not:

```rust
// pipeline.rs:732-737
Err(why) => {
    if broke > 0 {
        live.forget(records, &[]);      // republishes the RESPLIT list
    }
    crate::live::Answer::Refused(why)
}
```

```rust
// tui/mod.rs:221-224
crate::live::Answer::Refused(why) => {
    browser.flash = why;                // marks are NOT cleared
    Step::Stay
}
```

So on a refusal that also broke a candidate, the group list is rebuilt with
**shorter** membership while the marks against it survive. `absorb_found`
prunes marks only by group id (`model.rs:638-640`) — "a mark on a group that is
gone is not a mark on anything" — and a resplit group is not gone, it is
smaller. `selection()` then indexes `members[member]` directly
(`model.rs:363`). I wrote the reproducer as a model-level test:

```text
$ cargo test rv_a_stale_member_position_survives_a_refusal
thread '...' panicked at src/tui/model.rs:363:58:
index out of bounds: the len is 2 but the index is 3
test result: FAILED. 0 passed; 1 failed; 0 ignored; 250 filtered out
```

**And a panic on the browser thread does not end the process — it hangs it.**
`live_scan` calls `watch.cancel()` *after* `tui::run_live(...)` returns, so a
panic skips it, and `std::thread::scope` waits for the scan thread before
resuming the panic — a thread whose serving loop is `while !live.cancelled()`.
I injected a panic on the browser thread and watched:

```text
=== B. probe ON — browser panics once the scan has finished ===
    exited=None  after 25.01s  still_alive_at_limit=True
    saw: thread 'main' panicked at src/tui/mod.rs:180:13
    saw: reviewer probe: browser panic after the scan finished
    RESULT: THE PROCESS NEVER LEFT
```

The panic hook does restore the terminal, so the user gets a shell back — and a
process that never exits. That turns any browser-thread panic, this one
included, from a crash into a hang.

What I proved and what I did not: the panic is proved at the model level and
the code path into it is exact. I did not build the full end-to-end, which
needs a survivor-guard refusal (a marked group whose other copies have left the
disk) to land in the same apply as a failed candidate proof. I take the
co-occurrence as established because `serve` itself is written for it — the
`if broke > 0` inside the `Err(why)` arm exists for no other case.

Neither half of this is a design objection. `absorb`'s comment already states
the intended rule; the `Refused` arm is the one path that does not follow it.

---

## Cache under kill — reproduced, then taken further

I reproduced the branch's script first:

```text
$ python3 rust/evidence/scripts/m8_cache_kill_resume.py <my build> <scratch>
pass  killed by SIGTERM      cold_opens 24060  resumed_opens 60  groups_identical True
pass  killed by SIGINT       cold_opens 24060  resumed_opens 60  groups_identical True
pass  killed by SIGKILL      cold_opens 24060  resumed_opens 60  groups_identical True
3 case(s), 0 failed
```

The kill timing is honest. It waits for the cache to hold a committed row
before signalling — not a fraction of a stopwatch — and it fails the case if
the scan finished first (`m8_cache_kill_resume.py:198-199`). The vacuous
kill-at-half-of-0.2s draft the builder self-reports is genuinely fixed.

**But that script runs `--list`, which is the frozen path.** Three of the four
kills `M8-LIVE.md` names — `q`, Ctrl+C, terminal death — only exist on the live
path, where the cache is written by a scan thread behind a browser that owns
the terminal. So I wrote `rv_live_kill.py`, which opens the live browser on a
real pty, kills it once the cache has committed, opens every database directly
and asks it whether it is well:

```text
uninterrupted scan: 6000 group(s), 24000 opens, 110394000 bytes

pass   q typed at the live browser
        exit=0   rows committed before the kill=12000
        integrity_check: ...db=ok (12000 rows)
        resumed: 0 opens vs 24000 cold; groups identical: True
pass   SIGINT (Ctrl+C)      exit=-2   integrity_check: ok (12000 rows)   0 opens vs 24000
pass   SIGTERM              exit=-15  integrity_check: ok (12000 rows)   0 opens vs 24000
pass   SIGKILL              exit=-9   integrity_check: ok (12000 rows)   0 opens vs 24000

4 case(s), 0 failed
RV_LIVE_KILL_EXIT=0
```

No corruption on the live path either, under the signal that cannot be handled,
and total reuse on the resume. The durability argument is also honestly scoped
rather than oversold: `open_database` sets `journal_mode=WAL` and
`synchronous=NORMAL`, and its comment concedes in as many words that a machine
losing power mid-flush can still lose the last commit. That is the correct
bargain for the failure the spec names, and saying so is better than claiming
more.

On the flagged inherited issue — "a long stage 5 cannot flush mid-way" — the
characterisation is honest and I would go further in the branch's favour: on
the live path stage 5 is deferred, so the long comparison happens at apply
time, where the cache is not being written at all. A kill there **loses**, and
cannot **corrupt**, because `write_rows` opens and commits one transaction
synchronously and nothing is in flight between them.

One property worth recording, which is a consequence of the design rather than
a defect: `settle()` is called only at the four stage boundaries, so a live
scan that finishes every stage inside `FLUSH_EVERY` (2s) while owing fewer than
`FLUSH_ROWS` (4096) rows commits nothing until `cache.close()` — which runs
only on a clean exit. I hit this on my first attempt with a 900-pair corpus:
integrity `ok`, but `0 rows`, and the resume reused nothing. The loss is
bounded by exactly the two triggers the branch chose, and both are needed; see
non-binding note 3.

---

## Concurrency

The apply-pauses-scan call is stated in `LOG.md` with its one reason, as the
spec required, and the reason is correct: a stage mutates the records a
deletion walks (`release` drops retained bytes, a split rewrites bucket
membership). I traced the pause and it excludes the mutation windows properly —
`serve()` appears at exactly four sites, all of them inside the `'stages:`
labelled block immediately after `cache.settle()`, and never inside
`compare_stage` or a hash stage.

I **ratify** the refusal to implement idle-time promotion. It is the builder's
choice under the spec, correctness does not depend on it, and the stated reason
is the right one: a second reader on the platter during the narrowing stages is
the interleaving M7 spent a milestone removing. The claim's own suggestion —
run it only once the scan thread has no stages left, where it is the sole
reader — is the safe shape if it is ever wanted.

The per-volume cache is shared by proof and scan, and I looked for the race the
brief asked about. There isn't one, and the reason is structural rather than
lucky: the scan thread is the only thread that ever touches the `ScanCache`,
because the browser asks and the scan answers. `Live` holds a `RefCell` and is
deliberately not `Sync`, which is how the compiler is told so.

`Watch::fresh()` is correct — but its doc comment is not; see non-binding note 2.

---

## Terminal death, and the pulse behind the browser

```text
$ python3 rust/evidence/scripts/m8_live_death.py <my build>
pass  the terminal closes and there is no SIGHUP to warn anyone   exit 1   scan still running: True
pass  the terminal closes and SIGHUP arrives                      exit -1  scan still running: True
pass  Ctrl+C typed at the live browser                            exit 0   terminal afterwards: intact
pass  SIGHUP while the browser is up and the scan is running       exit -1  LEFT RAW  (M5's, reported not fixed)
pass  SIGTERM while the browser is up and the scan is running      exit -15 LEFT RAW  (M5's, reported not fixed)
5 scenario(s), 0 failed

$ python3 rust/evidence/scripts/tui-terminal-death.py <my build>
  keys/menu        PASS   keys/browser     PASS
  hangup/menu      PASS   hangup/browser   PASS  (0.05s after the terminal died, 0.00 CPU-seconds)
all scenarios passed

$ python3 rust/evidence/scripts/m5_2_signals.py <my build> <scratch>
All four behave as recorded.

$ python3 rust/evidence/scripts/m5_2_pulse.py <my build> <scratch> <main build>
a piped run says nothing at all   ok (stdout byte-identical to main, 5188 bytes; 5 counters equal at -j1)
...eight cases...
every case behaves
PULSE_EXIT=0
```

The SIGHUP/SIGTERM-leaves-terminal-raw result is M5's, `m5_2_signals.py` records
it as M5's, and it is unchanged here. Not graded as M8's, and the branch is
right not to grade it either.

The `paints` fix is correct and I checked it by construction rather than by
observation alone: `Pulse::live` builds a pulse with `ticker: None` and
`paints: false`, `stage()`'s boundary frame is behind `if live.paints`
(`pulse.rs:262`), and `finish()` returns early when there is no ticker
(`pulse.rs:279`) — so even `Drop` cannot write. The live pulse has no way to
reach stderr. Meanwhile the non-browser TTY path that M5.2 built still passes
its whole battery, including twelve repaints and a cleared line before the
browser.

---

## Live UI

Streaming order and cursor stability are right, and for the right reason.
`Browser::insert` (`model.rs:283`) records the current group's stable `index`,
inserts by `order()` via `partition_point`, reindexes, and puts the cursor back
on the same id — so a group arriving above the cursor renumbers the screen and
moves nothing the user was looking at. Marks are keyed by stable id, which is
the load-bearing change the claim highlights; the mutant that reverts it is
caught (below).

**Time-to-first-group is not gamed, and I hand-traced what "found" means.** The
`stream` closure publishes a bucket only when `final_now(size)` holds, and the
three predicates are principled rather than convenient:

```text
after stage 3 (ends)        size <= HEAD_BYTES        the deep head only looks at bigger files
after stage 4 (deep head)   !sampling || size <= SAMPLE_SPAN   the sampler only looks at bigger files,
                                                               and not at all on spinning
after all narrowing         |_| true                  nothing is left that could split it
```

So a group appears at the first moment no remaining narrowing stage could
change it. That is the earliest a candidate honestly exists, which is exactly
what the spec's closing paragraph asks for.

The candidate label's polarity is honest — `if group.proven { Span::raw("") }
else { "  candidate" }` (`view.rs:218`), only the guess is labelled — and the
emptied-list flash fix is real: `an_emptied_list_still_says_why_it_emptied` is
a new test and it does not touch any non-interactive output, so it cannot
disturb the fence.

---

## Standards, re-derived

Counts taken from my own builds of both sides, not copied:

```text
main@a362ffa      $ cargo test
                  test result: ok. 226 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
                  TEST_EXIT=0
m8-live@be65b8a   $ cargo test
                  test result: ok. 250 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
                  TEST_EXIT=0
```

226 before, 250 after, twenty-four new: confirmed. Exactly one test binary on
each side — no integration binary and no doc-test pass — so the figure is the
whole count and not a unit-test subset. `cargo clippy --all-targets -- -D
warnings` and `cargo fmt --check` are clean on both sides
(`CLIPPY_EXIT=0`, `FMT_EXIT=0`).

```text
$ python3 rust/evidence/scripts/claim_audit.py M8
M8: 12 block(s), 0 untraced
AUDIT_M8_EXIT=0
$ python3 rust/evidence/scripts/claim_audit.py --all
...every section M1-M8...  0 untraced anywhere
AUDIT_ALL_EXIT=0
```

The gate, run in my own worktree with my own binary:

```text
$ ls /Volumes   (before)      \]  dedupbench
33 check(s), 0 failed
gate all: green
   exit 0
$ ls /Volumes   (after)       \]  dedupbench
$ diff <(before) <(after)
(no output: the mounted volumes are the ones that were there before)
$ hdiutil info | grep -c dedupconf
0
gate green, mount table unchanged, no image left attached
```

The `\]` volume was present before and after, unchanged, and nothing on this
branch touched it. `dedupbench` was never read, written or mounted by me.

### Flash parity

`m8_flash_inert.py` against my own `git archive` build of the base:

```text
scenario       files   bytes_read         opens    groups ours/base
needle         10000     24682520          9635        10   0.986x
typical         1600      2812459           430        64   0.982x
samesize         142      3702784           142         4   0.967x
widegroup         70     14501888            84         8   0.986x
photos            13     47846337            11         3   1.006x
bigheader          8     32677888             8         1   1.005x
prefixtrap        32     16973824            32         4   0.995x
rescan          1600       351680             2         1   0.976x

geomean over 8 scenarios: 0.988x
noise floor (base against itself, same corpus, same rounds): 0.991x
load average at the end: 3.09 3.23 2.91
```

Every counter column above is identical to the claim's table, digit for digit,
which is the part that is a fact. My wall-clock geomean (0.988x) is not the
claim's (0.997x) and neither is a claim: mine sits inside my noise floor of
0.991x, the claim's sits inside its 0.978x, and I measured at load average 3.0
on a machine shared with a dozen other agents. Counters over timings, as M7
ruled.

### Determinism — is the pinning fair, or is it hiding something?

The brief asked whether the run-to-run nondeterminism the builder pinned away
is new in `m8-live`. **It is not**, and I checked it two ways.

Structurally: `effort.rs`, which counts `seeks` across concurrent readers, is
**unchanged by this branch** (`git diff --stat main..HEAD -- src/effort.rs` is
empty), and the racily-clean rule at `cache.rs:231` is a context line in the
diff, not an added one. Both mechanisms predate M8.

Empirically, `rv_nondet.py` ran each binary six times in three regimes —
warm-cache counters on an un-aged corpus, the same aged, and a cold `-j4` scan:

```text
                              main@a362ffa     m8-live@be65b8a
A. un-aged, warm, -j1         A_STABLE=True    A_STABLE=True
B. aged 10s, warm, -j1        B_STABLE=True    B_STABLE=True
C. aged, cold, -j4            C_STABLE_ALL=True  C_STABLE_ALL=True
```

I did not reproduce the flakiness at all — my corpora build faster than one
second and my `-j4` files are small enough not to interleave — so I cannot
confirm the builder's specific observation. What I can say is the thing that
matters: **the two binaries are equally deterministic in every regime I could
construct**, so the pinning is fair harness hygiene and not concealment of a
regression. My own base-against-base capture agrees:

```text
$ python3 rv_fence.py compare fence/base1 fence/base2
190 cases compared, 0 differing
BASE_SELF_EXIT=0
```

### Mutants

```text
mutant                                        verdict    caught by
menu-guard-a-new-flag-skips-the-form          caught     every_flag_in_the_table_has_been_decided_about_for_the_form
                                                         + every_flag_in_the_table_is_accepted_and_nothing_else_is
menu-drops-force-from-the-form                caught     every_flag_in_the_table_has_been_decided_about_for_the_form
                                                         + the_form_offers_force_and_it_reaches_the_options
marks-keyed-by-screen-position-again          caught     marks_survive_a_group_being_inserted_above_them
the-proof-always-passes                       caught     m8_live_end_to_end.py (SURVIVED cargo test)
the-proof-always-fails                        caught     m8_live_end_to_end.py (SURVIVED cargo test)
deferred-stage-5-leaks-onto-the-frozen-path   caught     my fence proof, 86 of 190 cases
live_wanted loses its --report-json/--stage clauses   SURVIVED   nothing, at any level  → item 3
```

The `MENU_EXPOSURE` guard is genuinely bidirectional — it fails both for a
`FLAGS` entry with no menu row and for a menu row with no `FLAGS` entry — and
the first mutant above is exactly the one the claim says it added the guard
for. Two of the four self-reported vacuous tests are spot-checked here
(mark-keying, and `--force` in the form); both now fail for the right reason.

---

## `--force`

Reproduced exactly, and the script is right to look in the database rather than
believe the counters:

```text
run                                  bytes_read    opens   groups
cold (nothing remembered)              67108864       16        8
warm (the cache answers)                      0        0        8
--force (believes nothing)             67108864       16        8
--no-cache (reads and writes nothing)  67108864       16        8

cache after the --force run:    16 rows, 16 with a whole-file digest
cache after the --no-cache run: 0 rows, 0 with a whole-file digest
```

The help text carries the distinction in one line each, as the spec demands:

```text
      --no-cache          neither read nor write the scan cache
      --force             read nothing the cache remembers, but write what
                          this scan learns
```

`--force` counters prove re-reading on a warm cache: the middle row reads
nothing, the `--force` row reads everything a cold run reads. Accepted as
written.

---

## Binding items

Item 1 is the one I care most about. Items 3 and 4 are about the written
record, which `REWRITE.md` §8 treats as the thing the next person reads.

### 1. A refusal that also breaks a candidate leaves marks that name files that are no longer there, and the panic that follows hangs the process

Traced and reproduced above. Two independent halves, and both should be closed.

`serve`'s `Err(why)` arm republishes a resplit list when `broke > 0`
(`pipeline.rs:732-735`) while `tui/mod.rs:221-224` keeps the marks. Every other
answer clears them through `absorb`. `absorb_found` prunes marks by group id
only, so a mark against a group that merely got *smaller* survives and
`selection()` indexes past the end (`model.rs:363`).

Separately, `live_scan` reaches `watch.cancel()` only after `tui::run_live`
returns, so any panic on the browser thread leaves `std::thread::scope` waiting
on a scan thread looping `while !live.cancelled()`. Measured: the process was
still alive after 25 seconds and had to be killed.

*Required:* clear the marks on the `Refused` path the way `absorb` does — or
prune them by member as well as by group in `absorb_found`, whichever you judge
truer to the model. And make the scan's cancellation survive a panicking
browser, so a browser fault ends the process instead of hanging it; the drop
guard idiom the scan thread already uses for `Live::leaving` is the obvious
shape, applied to the browser side.

*I will verify by:* running the model-level reproducer — mark member 3 of a
four-member candidate, absorb a replaced list in which that group has two
members, then call `selection()` — and requiring it to return marks rather than
panic; and by re-running my browser-thread panic probe (a `panic!` injected
into `live_loop` after `fresh.finished`) on a pty and requiring the process to
exit on its own within a few seconds rather than being killed at the limit. A
regression test for at least the first, terminal-free, in `model.rs`.

### 2. The milestone's most consequential line has no terminal-free guard

`prove_marked`'s `whole` decides whether a candidate group may be deleted from.
Inverted in either direction, all 250 tests pass; only the pty battery notices.
The pty battery is real and it does bite — I proved that above, and I am not
asking for the guard to be moved. I am asking for a second one that a
`cargo test` run can fail, because that is the suite anyone editing this file
will run first.

*Required:* one terminal-free test that fails when `whole` is forced true and
one that fails when it is forced false. `prove_marked` takes plain
`&[(Vec<usize>, bool)]` runs and a `&[FileRec]`, so this needs a scratch
directory and no terminal at all; the "same ends, different middles" corpus the
claim already describes is the fixture.

*I will verify by:* applying both mutants to `pipeline.rs` in a scratch
checkout and requiring `cargo test` to fail on each, naming a test that is
about the proof rather than about something incidental — and then, per the
stale-mutant rule, rebuilding and confirming the suite is green again.

### 3. Two clauses of the one absolute fence are guarded by nothing

Deleting `&& options.report_json.is_none() && options.stage.is_empty()` from
`live_wanted` leaves `cargo test` at 250 passed and the freeze proof blind,
because every freeze case is non-interactive and `has_terminal()` short-circuits
first. A gate that cannot fail proves nothing about the clause it does not
reach, and the claim presents `live_wanted` as where the fence "is enforced in
code rather than promised in a comment".

*Required:* make `live_wanted` decidable without a terminal — take the
terminal-ness as an argument, or split the flag half into a helper — and test
the flag half directly: `--report-json`, `--stage size`, `--auto`, `--list`,
and bare, each asserted against the answer the fence requires.

*I will verify by:* deleting each clause of the flag half in turn in a scratch
checkout and requiring `cargo test` to fail on every one; and by re-running my
tty matrix against the rebuilt binary and requiring `--report-json` on a
terminal still to write its report and still to show no `candidate` label.

### 4. `main.rs` says `--list` is excluded from the live path, and it is not

`main.rs:80-81`: "`--list` and `--auto` are already excluded by
`browsing_flags`". `--auto` is. `--list` is excluded only when `--interactive`
is absent, because `browsing_flags` is `auto.is_none() && (interactive ||
!list)`. Measured on a real terminal: `--list --interactive` reaches the live
browser on this branch and shows `candidate` labels, where `main` browses the
same run fully proved.

I am **not** asking for the behaviour to change. `-i` means "browse them", main
already browses for `--list -i`, and `M8-LIVE.md` gives the interactive browser
path to M8. The defect is that the comment states a guard the code does not
have, on the one subject the milestone said was absolute — and that a reader of
`MILESTONES.md` would conclude no run carrying `--list` can defer stage 5. One
can.

*Required:* correct the comment to say what `browsing_flags` actually does, and
add one line to the M8 claim recording that `--list --interactive` on a
terminal is an interactive run and takes the live path.

*I will verify by:* `grep -n "browsing_flags" -A 12 rust/src/main.rs` and
checking the comment matches the expression; and by re-running my tty matrix
and checking the claim's new sentence against the `--list --interactive` row.

---

## Non-binding notes

1. The two proof mutants surviving `cargo test` is item 2. Separately, it is
   worth a sentence in `LOG.md` that the deletion-honesty guard lives at the
   pty level, so the next person does not read a green `cargo test` as covering
   it. M5.2 learned the same lesson about `pulse-is-never-off` and writing it
   down is what made it cheap the second time.

2. `Watch::fresh()`'s doc comment describes a design the code does not
   implement. It says the published list "holds `None` where the browser has
   already taken one" and that "the published list is kept whole so that a
   `republish` can rebuild it from nothing" — but `groups` is `Vec<Group>` with
   no `Option`, `drain(from..)` empties it, and `taken` is therefore
   permanently 0, which makes the `.min()` on the next line dead. The behaviour
   is correct, because `republish` rebuilds from `Live::runs` rather than from
   `body.groups`. Either drop `taken` and the comment, or implement the cursor
   the comment describes. I would drop it.

3. `live.rs` says `cancel` is "read in the scan's innermost per-file loop"
   (:19-21, :273-276) and that `taken_ask` is called "between files in a reading
   stage and between stages" (:280-282). Neither is so: both appear only at the
   four stage boundaries of the `'stages:` block, plus the post-scan serving
   loop. I looked for harm and did not find any worth an item — on a 3 GiB
   corpus, `q` mid-scan took 0.21s to exit against 0.05s after the scan — so
   this is a wording fix, not a behaviour one. Worth fixing because the comment
   is the reason a reader would believe cancellation is prompt on a tree far
   larger than mine.

4. A live scan that finishes every stage inside `FLUSH_EVERY` while owing fewer
   than `FLUSH_ROWS` rows commits nothing until `close()`, which runs only on a
   clean exit — so a kill after a *fast* live scan loses everything it learned.
   The loss is bounded exactly by the two triggers, both of which the claim
   defends well, and a fast scan by definition has little to lose. Recorded
   because I hit it before I sized my corpus up, and the next person will too.

5. The claim's fourth open question says time-to-first-group is unscored. I
   agree with leaving it unscored and I confirmed nothing games it (the three
   `final_now` predicates above). No action.

---

## Defects confirmed as pre-existing, and not M8's

* **SIGHUP and SIGTERM leave the terminal raw.** Neither the drop guard nor the
  panic hook runs, because neither signal unwinds. `m5_2_signals.py` records it
  as M5's and I confirmed it reproduces identically on `main@a362ffa`.
  `m8_live_death.py` prints it per scenario and declines to grade it, which is
  the right call: failing on it would be claiming a defect it did not find.
* **Ctrl+C at the menu is ignored** — M5.1's, recorded by the same script,
  unchanged.
* **A long stage 5 cannot flush mid-way.** Inherited M7 territory, honestly
  characterised by the claim, and — as ruled above — a loss rather than a
  corruption, and one the live path largely sidesteps by deferring stage 5.

---

## Rulings — checked, and found right

* `ProvenGroups` is untouched and remains the only key to deletion; the lending
  trick mints the token the same way `--auto` does.
* The deferral's reuse of `attested` is the right economy, not a shortcut: it
  is already precisely "matched without reading the bytes this scan" and
  already the flag `run_selected` keys on.
* Apply pauses the scan: correct call, correctly reasoned, correctly placed at
  stage boundaries.
* Declining idle-time promotion: **ratified**, for the reason given.
* `--force` and its help line: accepted as written.
* `MENU_EXPOSURE`: a real bidirectional guard, and the mutant it was written
  for is caught.
* The conformance harness really does pass `--report-json` on every invocation.

---

## Fence conditions this review was made under

Worktree `/Users/dogaozden/AI_Projects/dedup-m8-review`, branch `m8-review`,
created from `m8-live` at `be65b8a`. The builder's worktree
`/Users/dogaozden/AI_Projects/dedup-m8` was not read from and not written to. I
did not edit `m8-live` or `main`, pushed nothing, deleted no branch, and used
no `sudo`.

Every mutant was applied to a scratch copy under my own scratch root, never to
this worktree or the builder's, and every one was reverted and proved reverted
by `diff -r` against the committed source before the next thing ran. After the
last mutant the release binary rebuilt to `38a65a86…`, byte for byte what it
was before the first — the stale-mutant rule, observed and checked rather than
asserted.

`/Volumes` was `\]` and `dedupbench` before and after every run, and the gate
script diffs its own listings. The only `/Volumes` path anything here touched
is the ephemeral image the `m4-trashes` check attaches at `/Volumes/dedupconf`
and detaches itself; `hdiutil info | grep -c dedupconf` reported `0`
afterwards. `dedupbench` was never read, written or mounted — the flash parity
run used `bench/corpus.py` on the local SSD. Nothing went near
`/Volumes/My New Passport`.

Every corpus I built lives under my scratch root, every `--auto` and every
browser deletion went to a scratch `HOME`'s Trash, and the largest corpus
(3 GiB) was removed after use. Free space on `/` fell to 11 GiB at its
tightest, which is worth saying beside the flash numbers.

The machine was shared with a dozen other agents throughout; load average was
between 2.0 and 5.5 and is printed beside every timing. The only figures I
claim as facts are counters.

---

## How to clear M8

Close the four binding items. Item 1 is the only one that touches behaviour and
it is two small fixes; items 2 and 3 are tests that do not exist yet; item 4 is
a comment and a sentence. I would also take non-binding notes 2 and 3, which
are two stale comments and one piece of dead state.

Nothing here is a design objection. The shape of this milestone — defer stage 5,
reuse `attested`, lend the records, one reader on the disk, commit at stage
boundaries — is right, and the parts of it I tried hardest to break did not
break.

---

# Re-review — 2026-08-25, at `4810ffd`

**Verdict: APPROVED.**

Four commits on top of `be65b8a`, no history rewrite (`be65b8a` is still an
ancestor) and the base is still `a362ffa` — both checked with `git merge-base`
rather than taken from the hand-off. Everything below I ran myself in a fresh
`git archive` extraction of `4810ffd` built beside my `main@a362ffa` build;
where I re-ran a builder script I say so and treat it as a reproduction. My
build of the tip hashes `dedd36e9…`, and after every mutant in this round the
source was restored, proved identical to `git show 4810ffd:…`, and rebuilt back
to that same hash.

All four binding items are discharged, each by the recipe I wrote in the first
round rather than by the builder's own script. One new observation is recorded
as a non-binding note; it does not hold up the merge, and I explain below why
not.

## Item 1 — discharged, both halves

**1a.** My own reproducer, re-run verbatim from my recipe — mark member 3 of a
four-member candidate, absorb a replaced list in which that group has two
members, call `selection()`:

```text
$ cargo test rv_recheck_stale_member_position_after_a_rebuild
running 1 test
test tui::model::tests::rv_recheck_stale_member_position_after_a_rebuild ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 258 filtered out
```

It returns instead of panicking, and my probe additionally asserts that no
surviving mark names a record absent from its group. It survives nothing at
all: `absorb_found` now clears every mark on every rebuilt list.

**The builder solved this differently from either option I offered, and the
choice is better than mine.** I suggested clearing on the `Refused` path *or*
pruning by member as well as by group. The second would have been a latent
bug: a rebuilt list only ever arrives after a proof or a deletion, and both
*rewrite* membership — a group that failed its proof comes back as its first
byte-equality class, whose position 1 need not be the record the user marked at
position 1. A member-pruned mark would have been **in range and naming a
different file**, which on this path is a file nobody chose being deleted.
Clearing everything, at the one place every rebuild passes through, is the
correct rule, and it is the rule `absorb` already stated two methods up. The
`members.get(member)?` backstop is documented as a backstop and not as the
guard, which is the right way round.

**1b.** My panic probe, re-run: a `panic!` injected into `live_loop` after
`fresh.finished`, on a pty.

```text
=== probe ON — browser panics once the scan has finished ===
    exited=101  after 0.55s  still_alive_at_limit=False
    saw: thread 'main' panicked at src/tui/mod.rs:178:13
    RESULT: the process left on its own with status 101
```

0.55s and exit 101, against "still alive after 25 seconds" in the first round.

I did not take the new guard's word for it either. Neutering `Stopping::drop`
to a no-op — the guard present, doing nothing — brings the hang straight back:

```text
=== guard neutered ===
    exited=None  after 25.03s  still_alive_at_limit=True
    RESULT: THE PROCESS NEVER LEFT
```

So `Watch::stopping()` is load-bearing rather than decorative. It is the exact
mirror of `Live::leaving()`, which is the right shape: the scope has two
threads and each now releases the other however it leaves.

## Item 2 — discharged

My recipe: apply both mutants, require `cargo test` to fail on each, naming a
test that is about the proof. Mine, in my own checkout:

```text
mutant                                        verdict   failing test
the proof always PASSES  (whole = true)       caught    pipeline::tests::the_proof_refuses_a_group_
                                                        whose_middles_differ_however_its_ends_look
the proof always FAILS   (whole = false)      caught    pipeline::tests::the_proof_accepts_a_group_
                                                        that_really_is_identical
an already-proven group is re-read            caught    pipeline::tests::a_group_already_proven_is_
                                                        not_read_a_second_time
```

Named for the proof, not for something incidental. The third is the builder's
own addition and it is the best of the three: it pins the `proven`
short-circuit on `Effort::bytes_read()` being zero — on what the code did,
rather than on what it says about itself. The fixture is the "same ends,
different middles" corpus the claim describes, built in a scratch directory
with a `Drop` that takes it away, and it needs no terminal.

After the mutants, `cargo test` is 258 green and the release binary is back to
`dedd36e9…`.

## Item 3 — discharged

My recipe: delete each clause of the flag half in turn, require a failure on
every one. I wrote my own mutant list rather than running theirs, and I added
clauses their script does not cover:

```text
mutant                                                  verdict    caught by
live_flags loses report_json.is_none()                  caught     the_fence_decides_the_live_path_by_flags_alone
live_flags loses stage.is_empty()                       caught     the_fence_decides_the_live_path_by_flags_alone
live_flags loses browsing_flags()                       caught     the_fence_decides_the_live_path_by_flags_alone
browsing_flags loses auto.is_none()                     caught     the_fence_decides… + the_menu_opens… (+1)
browsing_flags loses the list/interactive clause        caught     the_fence_decides… + the_menu_opens… (+1)
live_wanted stops consulting live_flags                 SURVIVED   nothing, in cargo test
```

Five of six caught where in the first round none were. The sixth is non-binding
note 4 below — and I escalated it rather than leaving it as a bare survival.

The fence itself, re-verified end to end. My own 190-case differential, ten
corpora, against my `main@a362ffa` build — re-run because `live_wanted` was
edited in this round and a fence proof from the previous tip would not have
covered it:

```text
$ python3 rv_fence.py compare fence/base1 fence/fix
base-vs-fix-tip (the fence, re-verified)
190 cases compared, 0 differing
FENCE HELD: every non-interactive output and every effort counter is byte-identical
```

And my own tty matrix on the clean tip, which is the half the freeze proof
structurally cannot see:

```text
case                        exit  alt  candidate?  report?  verdict
bare interactive            0     2    True        False    LIVE
--list                      0     0    False       False    frozen, no screen
--list --interactive        0     2    True        False    LIVE
--report-json               0     2    False       True     browser/menu only
--stage size                2     1    False       False    browser/menu only
--auto shortest --yes       0     0    False       False    frozen, no screen
```

Row for row what I measured in the first round, and row for row what the
branch's `m8-tty-matrix.txt` records.

## Item 4 — discharged

`main.rs` now quotes the expression rather than summarising it wrongly, and
names `--list --interactive` explicitly as an interactive run that takes the
live path. The claim carries the paragraph I asked for, and it says the thing
that matters in as many words: "a reader of `MILESTONES.md` cannot conclude
that no run carrying `--list` defers stage 5. One does." Accepted as written.

## Non-binding notes 1, 2 and 3 — all taken

Note 1 got its own heading in `LOG.md` ("Where the deletion-honesty guard
actually lives"), which is more than I asked for and the right call. Note 2:
the `taken` cursor is gone rather than implemented, and `fresh()` is now a
`std::mem::take` with a comment that describes what it does — the builder
records that the behaviour was right and the description was not, "which is the
worse of the two ways to be wrong". Note 3: the per-file claims in `live.rs`
are replaced with a module note stating the four stage boundaries and the
serving loop, and citing my 0.21s/0.05s measurement for what the latency
actually costs.

## The other checks

All re-run by me on the tip, in my own checkout:

```text
$ cargo test                                258 passed; 0 failed        TEST_EXIT=0
$ cargo clippy --all-targets -- -D warnings  clean                      CLIPPY_EXIT=0
$ cargo fmt --check                          clean                      FMT_EXIT=0
$ conformance/run.py --gate all              33 check(s), 0 failed      gate all: green
                                             mount table unchanged, 0 images left attached
$ m8_live_end_to_end.py                      5 case(s), 0 failed
$ m8_live_death.py                           5 scenario(s), 0 failed
$ m8_cache_kill_resume.py                    3 case(s), 0 failed   (24060 -> 60 opens)
$ rv_live_kill.py            (mine)          4 case(s), 0 failed   (live path; integrity ok
                                             under SIGKILL; 0 opens vs 24000; groups identical)
$ m8_force.py                                pass
$ m8_deletion_mutants.py     (new)           pass — 3 guards, each proved by breaking it
$ m8_fence_clauses.py        (new)           pass — 5 clauses, each proved by breaking it
$ m8_panic_probe.py          (new)           pass — exit 101 in 0.37s
$ claim_audit.py M8                          9 block(s), 0 untraced
```

250 → 258 reconciles exactly: the diff adds eight `#[test]` attributes and
nothing is removed. Three of the new scripts are the kind of evidence I like —
each one breaks the thing it is claiming to guard, and each checks the
stale-mutant rule on its own release binary rather than asserting it.

Flash parity, re-run against my own base build:

```text
scenario       files   bytes_read         opens    groups ours/base
needle         10000     24682520          9635        10   0.981x
typical         1600      2812459           430        64   1.009x
samesize         142      3702784           142         4   0.987x
widegroup         70     14501888            84         8   0.985x
photos            13     47846337            11         3   1.004x
bigheader          8     32677888             8         1   1.026x
prefixtrap        32     16973824            32         4   1.055x
rescan          1600       351680             2         1   1.036x

geomean over 8 scenarios: 1.010x
noise floor (base against itself, same corpus, same rounds): 1.003x
load average at the end: 1.87 4.78 9.41
```

Every counter column is identical to what I measured at `be65b8a` and to the
claim's own table. The wall clock is not claimed by anybody: my geomean sits
inside my noise floor, and the fifteen-minute load average of 9.48 says why a
floor was needed.

## Non-binding note from this round

4. **`the_live_path_is_the_flags_and_a_terminal_and_nothing_else` cannot fail
   under `cargo test`.** It asserts `live_wanted(&o) == tui::has_terminal() &&
   live_flags(&o)`. Under `cargo test` there is no terminal, so both sides are
   `false && …` and the assertion holds whatever `live_wanted` does with the
   flags. I proved it: replacing `live_wanted`'s body with `has_terminal() &&
   browsing_flags(options)` — dropping the `--report-json` and `--stage`
   exclusions back out of the deciding function — leaves all 258 green. That
   test's own comment says it exists so "the test above" is not "pinning a
   different function from the one that decides", and it does not do that.

   Following M5.2's precedent I escalated rather than stopping at the survival,
   and the escalation is why this is a note and not a fifth binding item. On a
   real pty the mutant is loud:

   ```text
   case                        exit  alt  candidate?  report?
   --report-json               0     2    True        False     <- live, and NO report written
   --stage size                0     2    True        False     <- live, exit 0 not 2
   ```

   So the link **is** guarded — by `m8_tty_matrix.py`, which is committed
   evidence, which the claim explicitly nominates for this job ("a unit test
   asks the function what it thinks, so `m8_tty_matrix.py` asks the binary what
   it does"), and which I independently confirmed catches it. The claim does
   not cite the vacuous unit test as covering the link, so the written record
   is not overstating anything; only that one test's comment is. That is the
   same class of defect as my first-round note 2, and it is not a coverage gap
   in the argument.

   It is also partly mine: my item 3 recipe offered two shapes — "take the
   terminal-ness as an argument, or split the flag half into a helper" — and
   the second, which the builder took faithfully, is the one that leaves this
   residue. If you want it closed, the first shape closes it: give `live_wanted`
   the terminal-ness as a parameter and assert both values. One line and one
   test, whenever that file is next open.

## Defects still open, and still not M8's

SIGHUP and SIGTERM leave the terminal raw — M5's, recorded by `m5_2_signals.py`
as M5's, confirmed by me on `main@a362ffa`, printed but ungraded by
`m8_live_death.py`. Ctrl+C at the menu is ignored — M5.1's. A long stage 5
cannot flush mid-way — inherited M7 territory, honestly characterised, and a
loss rather than a corruption.

## Fence conditions this round was made under

Same worktree, `/Users/dogaozden/AI_Projects/dedup-m8-review` on `m8-review`;
all verification in a fresh scratch extraction of `4810ffd`. The builder's
worktree was neither read from nor written to. No pushes, no branch deleted, no
`sudo`, `main` and `m8-live` untouched.

Every mutant this round was applied to the scratch extraction, reverted, and
proved reverted by diffing against `git show 4810ffd:…` before the next ran;
the release binary was rebuilt to `dedd36e9…` afterwards each time, and the
whole `src` tree was diffed against the committed source at the end.

`/Volumes` was `\]` and `dedupbench` before and after, `hdiutil info | grep -c
dedupconf` reported `0`, and `dedupbench` was never read, written or mounted.
Corpora under my scratch root only; every deletion went to a scratch `HOME`'s
Trash. Load average ran between 1.6 and 9.5 on a shared machine — counters are
the only figures claimed as facts.

## Disposition

**M8 is APPROVED at `4810ffd`.** The one absolute fence holds, measured on my
own corpora and on a real terminal, and it is now falsifiable in `cargo test`
as well as at the pty. `ProvenGroups` is still the only key to deletion and the
line that decides has a terminal-free guard in both directions. The cache
survives all four kills on the live path with `integrity_check` clean. The two
defects I found in the first round are fixed, and the fix to the first one is
better than what I proposed.

Non-binding note 4 is worth an hour whenever `main.rs` is next open, and is not
a condition of the merge.
