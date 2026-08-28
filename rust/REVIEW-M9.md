# REVIEW-M9.md — maintainer review of the M9 claim

**Verdict: CHANGES — four binding items, none of them a design objection.**

The law holds. I attacked the no-materialization rule from every direction I
could construct and did not break it: every path in this program that opens a
file for its contents funnels through one function, that function is only ever
handed a path built from a record, and a dataless entry is dropped before a
record exists. The fence holds on my own corpora, chosen for what M9 touched
rather than what M8 touched. The 1/t regression is dead and guarded.

What the four items are about is the *second* mechanism and the written record.
The milestone's backstop — the I/O policy — verifies itself against the same
constants it set, so I was able to turn it into a shield over nobody, and into
a request that the kernel *download*, with all 276 tests green and every
behavioural case of the branch's own end-to-end harness still passing. The
shipped values are correct; I checked all four against the SDK header myself.
But the one property that would be catastrophic to get wrong is the one nothing
checks, and it costs eight lines to check, which I wrote and proved.

M9 is claimed at `962fa3c` on `m9-cloud`, off `main@644c66f` — both verified by
`git rev-parse` and `git worktree list`, not taken from the hand-off. The
binding spec is `M9-CLOUD.md`. I read the spec, the M9 section of
`rust/MILESTONES.md`, the M9 entries in `rust/LOG.md`, `REVIEW-M8.md` for
precedent and method, and `tools/cloud-validate.sh` as code, and then verified
everything independently in a worktree of my own at
`/Users/dogaozden/AI_Projects/dedup-m9-review`, with builds of both sides made
from fresh `git archive` extractions beside it; **no number below is taken from
the builder's scripts.** Where I re-ran a builder script I say so and treat it
as a reproduction, not as evidence.

---

## The binaries I tested

```text
$ echo "CARGO_TARGET_DIR=[${CARGO_TARGET_DIR:-unset}]"
CARGO_TARGET_DIR=[unset]

$ shasum -a 256 .../builds/base/rust/target/release/dedup-rs
b4b8f70834bc141e1010f802349d8c267b8f89df7c6bb80cd299b213fb027fff   main@644c66f
$ shasum -a 256 .../builds/cand/rust/target/release/dedup-rs
4544f690c1cfc53ba4830d3f278b593befd48c9a59a99132ff3e6d3b708aaae3   m9-cloud@962fa3c
$ shasum -a 256 .../mut/rust/target/release/dedup-rs
4891929c23bb6e0ae1bc37c72436b58d616660611f83d6e2e2746e39b9293c95   m9-cloud@962fa3c
```

Three build directories, three hashes. The last two are the same source: a
release build embeds its own absolute path, so a hash is only comparable
*within* one directory, which is what makes the staleness check meaningful
rather than ritual. Every mutant below was applied to the third — an isolated
`git clone` detached at `962fa3c`, never this worktree and never the builder's
— and after every one the source was restored and the binary rebuilt to
`4891929c…` byte for byte. That is printed beside each mutant.

---

## The law — hand-traced, then attacked

### Every path that opens a file

The spec's law is absolute, so I enumerated rather than sampled. Every call in
`src/` that reads file *contents* is `Effort::open` (`effort.rs:102`), reached
from exactly five sites: `stages.rs:120` (`take_or_open`), `stages.rs:600`,
`:608`, `:615`, and `compare.rs:271`. Every one of them opens
`root.join(record.path())` — a path built from a `FileRec`. `FileRec`s are
produced by `gather_bulk` and `gather_portable` and nowhere else. `gather_bulk`
drops a dataless entry at `walk.rs:344`, *before* `files_seen` is incremented
and before a record exists.

So grouping, sampling, the cache, `verify_attested` and the proof at apply
cannot reach a dataless file, because none of them can reach anything that is
not a record. `verify_attested` (`compare.rs:1268`) takes `&[FileRec]` and
reaches the disk only through `split_identical` → `lockstep` → `Effort::open`;
there is no second door. `cloud::dataless` appears in exactly one production
site, which is that line in the walk.

The remaining filesystem calls on the candidate are `stat`-family only —
`cloud::flags_of` (`symlink_metadata`), `trash.rs:366`, `walk.rs:550`,
`device.rs:237`. None of them materializes a placeholder; reading `st_flags`
without the bytes is the entire point of a dataless file, and it is the same
call the spec nominates as the detector.

**Cache.** "No verdicts are ever recorded for files that were not read" is
structural rather than defended: `ScanCache::remember` (`cache.rs:245`) takes a
`&FileRec`, so a file that never became a record has nothing to be remembered
about. There is no mutant for this because there is no line to break.

### The two gather paths, and the one that has no detector

`gather_portable` has no `cloud` parameter and treats nothing as dataless. That
is what the spec allows and the code says so in as many words. It is also
reachable on macOS, and more reachable than before: `WANT_COMMON` now includes
`ATTR_CMN_FLAGS`, and `parse_entry` (`sys.rs:1088`) sends **the whole
directory** down the portable path if the kernel declines any requested
attribute. A filesystem that will not report `ATTR_CMN_FLAGS` therefore loses
walk-time detection for every file in that directory.

I could not test that shape — the only filesystems I may legally reach here are
APFS on the local SSD, and the constraint on `/Volumes` and on
`~/Library/CloudStorage` is absolute — so I record it as reasoning, not as a
measurement. What I can say is that **the law still holds there**, and holds by
design rather than by luck: those files become records, their reads are refused
by the I/O policy, the existing read-error path drops them from their groups,
and `cloud::in_cloud` names them from the same flag. I proved that whole
sequence end to end below. The failure mode is a worse *experience* — a file
that should have been skipped silently is instead grouped, attempted, and
reported as evicted — not a download. The builder's comment says a filesystem
that will not answer an attribute request is not one that has cloud
placeholders on it; that is a judgment, and a reasonable one, but it is stated
as a fact. See non-binding note 1.

### The constants, checked against the header rather than the comment

`cloud.rs` takes five numbers on trust from a comment saying they came from the
SDK. I read the SDK on this machine:

```text
$ grep -n "SF_DATALESS\|SF_SYNTHETIC\|SF_SUPPORTED" $(xcrun --show-sdk-path)/usr/include/sys/stat.h
335:#define SF_SUPPORTED    0x009f0000      /* mask of superuser supported flags */
337:#define SF_SYNTHETIC    0xc0000000      /* mask of system read-only synthetic flags */
359:#define SF_DATALESS     0x40000000     /* file is dataless object */

$ grep -n "IOPOL_" $(xcrun --show-sdk-path)/usr/include/sys/resource.h
509:#define IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES 3
512:#define IOPOL_TYPE_VFS_IGNORE_CONTENT_PROTECTION 6
520:#define IOPOL_SCOPE_PROCESS   0
521:#define IOPOL_SCOPE_THREAD    1
540:#define IOPOL_MATERIALIZE_DATALESS_FILES_OFF     1
542:#define IOPOL_MATERIALIZE_DATALESS_FILES_ORIG    4
544:#define IOPOL_MATERIALIZE_DATALESS_FILES_BASIC_MASK 3
```

All five are right, including the mask, and including `_ORIG` being 4 which is
why the mask exists. The comment's "3, not 6" is exactly the confusion worth
warning about — 6 is `IGNORE_CONTENT_PROTECTION`. `SF_SUPPORTED` is `0x009f0000`
and does not contain `0x40000000`, which independently confirms the claim that
no userland process can set `SF_DATALESS`, and therefore that the flag-swap
harness is necessary rather than lazy.

### The scope — the brief's specific worry, settled three ways

A thread-scoped policy set on the main thread would be a false shield, because
the scan reads on worker threads. The branch uses `IOPOL_SCOPE_PROCESS`, and 0
really is `PROCESS` (THREAD is 1, above). The man page is explicit:

```text
$ man 3 setiopolicy_np
     IOPOL_SCOPE_PROCESS  The I/O policy of all I/Os issued by the current
                          process is get or set.
```

And measured, in a C probe of my own — a fresh `pthread` asking the kernel
after the main thread set the policy:

```text
A. nothing set yet
  worker thread sees scope=PROCESS policy=2 (masked 2) -> refusing=no
B. PROCESS-scoped OFF set on the MAIN thread (what dedup-rs does)
  worker thread sees scope=PROCESS policy=1 (masked 1) -> refusing=YES
   main thread, asked at THREAD scope: 0
  worker thread sees scope=THREAD policy=0 (masked 0) -> refusing=no
```

Two things worth keeping. The system default is **2 — `..._ON`**, so the policy
is doing real work, not restating a default. And the last line is the false
shield made visible: at THREAD scope a worker sees `0`, covered by nothing.

I also tried to prove the scope behaviourally with `IOPOL_TYPE_VFS_ATIME_UPDATES`,
whose effect is observable where materialization is not. **It failed to
discriminate** — atime did not move in the control case either — so I report it
as inconclusive rather than dressing it up as evidence.

### Where the policy is installed

`cloud::install_refusal` is called once, at `main.rs:206`, *after* the pre-flight
menu (`main.rs:164-170`) and before both the live path (`main.rs:225`) and the
frozen one. So a `--cloud` chosen at the menu is honoured, and there is no
window in which a file is opened before the policy is in force. There is no
second call site and so no stale state: `--cloud` declines to install rather
than lifting, which is a deliberate departure from the spec's word "lift", is
argued for in `LOG.md` and in the function's own doc comment, and I **ratify**
it. Overriding a policy the surrounding environment set for its own reasons is
not this tool's business, and within one process the two readings are
indistinguishable.

---

## The fence — my own derivation

`rv9_fence.py` is mine, written from `M9-CLOUD.md`'s fence sentence. I did not
reuse the branch's corpora, and I deliberately did not reuse M8's either,
because M9 touched something M8 did not: the walk now reads `ATTR_CMN_FLAGS`
out of every bulk entry and one bit of it decides whether a file exists. So my
corpora are the **near misses** — files wearing real, non-zero, non-dataless
flags (`UF_HIDDEN`, `UF_NODUMP`, `UF_OPAQUE`, `UF_IMMUTABLE`), and files that
legitimately under-allocate (sparse files, and `ditto --hfsCompression` files
carrying `UF_COMPRESSED`) — beside an empty tree, thirty zero-byte files,
unicode and space-bearing names, twelve-deep nesting, symlinks to a file, to a
directory, dangling and looping, hardlink clusters beside a true copy, and a
same-ends/different-middles tree that forces the comparison stage. Eleven
invocations each, every one non-interactive: 66 cases, each comparing stdout,
stderr, exit code and every field of the report including every effort counter.

```text
$ python3 rv9_fence.py compare fence/base1 fence/cand
66 cases compared, 0 differing
FENCE HELD: every non-interactive output and every effort counter is byte-identical
FENCE_EXIT=0
```

**My first pass did not hold, and the reason was mine.** Nine cases differed
*base against base* by exactly one `seek`. That is the nondeterminism
`REVIEW-M8` documented in `Effort`'s cross-reader seek counting, reproduced on
my corpora where M8's reviewer could not reproduce it on theirs. I pinned the
reader count to `-j 1`, as the brief instructed, and re-derived. The control now
says the harness is sound:

```text
$ python3 rv9_fence.py compare fence/base1 fence/base2
66 cases compared, 0 differing
BASE_SELF_EXIT=0
```

I also dropped `--cloud` from the differential and say so plainly: `main`
answers it with `dedup-rs: unknown argument: --cloud` and exit 2, so comparing
that against the candidate measures the flag's existence, not the fence.
What is worth proving is proved separately, and it is a stronger statement than
the fence asks for — on a dataless-free tree `--cloud` changes *nothing*:

```text
$ candidate --cloud   vs   candidate --list, same six corpora
6 corpora compared, 0 differing
--cloud changes NOTHING on a dataless-free tree
```

**And the gate has teeth, which I proved rather than assumed.** I took the
falsification the branch nominates — emit the skip count unconditionally — and
ran it against my corpora:

```text
mutant: `skipped_cloud: Some(skipped_cloud)` in place of `(> 0).then_some(..)`
$ python3 rv9_fence.py compare fence/base1 fence/mut1
  DIFFER  empty.auto   ['report.skipped_cloud=None vs 0']
  ...
66 cases compared, 66 differing
```

Every case. The branch reports 56 of its 84 for M8's corresponding mutant; I
get 66 of 66 for this one, because this key is in every report.

The conditional is guarded twice — `#[serde(skip_serializing_if = "Option::is_none")]`
on the field (`main.rs:66`) and `(skipped_cloud > 0).then_some(..)` at the
construction (`main.rs:299`) — and I checked both the serializer and real
outputs, as the brief asked. On real output the key is simply absent:

```text
skipped_cloud: (absent — the scan met no cloud file)
```

**Where the fence is real and where it is structurally blind.** Every one of my
66 cases is non-interactive, so none of them exercises the current-file line or
the windowed rate at all. That is not a gap in the fence — it is the fence — but
it means the fence cannot be cited as evidence that the new display stays out of
non-interactive output. That is guarded by construction instead: `Pulse::start`
returns `Pulse::off()` before it ever calls `effort.watch_reads()` when stderr
is not a terminal or `--quiet` is given (`pulse.rs:264-267`), so `Effort::reading`
early-returns on one relaxed load and no name is ever recorded. `Pulse::live` is
reached only from `live_scan`, which is behind `live_wanted`. I checked the
`live_wanted` parameterisation from `REVIEW-M8`'s note 4 landed: it takes
terminal-ness as an argument (`main.rs:128`) and the test runs **both** values
(`main.rs:608`), which is the shape that note asked for.

---

## Mutants

Eleven of mine, each applied to the isolated clone, built, run, reverted, and
the binary rebuilt to `4891929c…` before anything else touched it.

```text
mutant                                    cargo test   flag-swap walk   verdict
detection-line-deleted                    SURVIVED     CAUGHT           caught
cloud-flag-ignored (--cloud is a no-op)   SURVIVED     CAUGHT           caught
detector-inverted (== 0)                  caught by the_dataless_flag_is_the_detector…
rate-window-one-beat (100 ms)             caught by a_stall_reads_zero_within_one_window…
rate-window-a-minute (60 s)               caught by a_stall_reads_zero_within_one_window…
rate-is-a-lifetime-average-again          caught by a_stall_reads_zero_within_one_window…
current-file-never-recorded               caught by a_read_from_a_carried_descriptor…
menu-row-for-cloud-removed                caught by every_flag_in_the_table_has_been_decided…
iopolicy-call-site-deleted                SURVIVED     SURVIVED         SURVIVED (disclosed)
IOPOL_SCOPE_PROCESS = 1  (THREAD)         SURVIVED     SURVIVED         SURVIVED  → item 1
IOPOL_..._OFF = 2  (i.e. ON)              SURVIVED     SURVIVED         SURVIVED  → item 1
```

The first two confirm the branch's own claim, and they are the interesting
shape: deleting the one line the whole law rests on leaves `cargo test` green at
276, and only the flag-swap harness notices. `rate-is-a-lifetime-average-again`
is mine and it is the one I most wanted to see fail — deleting the eviction loop
from `Rate::observe` so the span becomes the stage's age — and it is caught. The
M9 regression is guarded.

**Two of my survivors needed a correction I nearly missed.** My first run
reported the two `IOPOL` constant mutants as CAUGHT by the flag-swap harness.
They were not. That harness edits `cloud.rs` and proves its own restore with
`git diff -- rust/src/cloud.rs`; my mutants live in the same file, so the only
thing that failed was *its restore check*, not any of its cases. Re-run in
isolation:

```text
IOPOL_SCOPE_PROCESS = 1        cargo test: 276 passed; every walk case held
IOPOL_..._OFF = 2              cargo test: 276 passed; every walk case held
```

That is item 1, and it is the one thing in this review I would not merge
without.

### The flag-swap harness is faithful

I checked the substitution at the layer it is injected, because a swap at the
wrong layer tests a parallel path. It replaces one constant —
`SF_DATALESS: u32` in `cloud.rs` — and nothing else. `dataless()`, the walk's
exclusion, the counter, the report field, the notice and `--cloud` are all the
production code, unmodified, one level above the swap. `UF_HIDDEN` means nothing
else anywhere in `src/` (I grepped), so setting it changes no other behaviour.
The one proposition the swap cannot test — that `0x40000000` is the right number
— I verified against the header above, and the offset it depends on is checked
against `lstat` field by field by `the_bulk_read_agrees_with_lstat_field_by_field`.

Reproduced, and it is the honest kind of evidence:

```text
=== 2. one member of the group is a placeholder ===
  groups:        [3]          files_seen: 5 (was 6)     bytes_read: 9453568 (was 12599296)
  skipped_cloud: 1            unreadable: []
  stderr:        '1 file in cloud — not scanned (--cloud to include)'
=== 3. --cloud puts it back ===
  groups:        [4]          skipped_cloud: (absent)   stderr: ''
=== 5. the whole tree is placeholders ===
  groups: []   files_seen: 0   bytes_read: 0   skipped_cloud: 6   exit: 0
  restored -> 4891929c…  identical to the pre-mutant binary
```

### The offset

`ATTR_CMN_FLAGS` (`0x00040000`) is read between `MODTIME` (`0x400`) and
`FILEID` (`0x2000000`), which is bit order, which is the order
`getattrlistbulk` packs them in. A misplacement would not fail — it would shift
the inode, and hardlink folding would start joining unrelated files. The
branch's test compares inode, size, nlink, mtime *and* flags against `lstat` on
a real directory with a real `chflags`-set flag, which is what makes that
checked rather than plausible. It is the right test and I could not improve on
it.

---

## The eviction path, fault-injected

Nothing in `cargo test` runs the spec's mid-scan-eviction sentence end to end:
the unit tests prove `EDEADLK` is an ordinary `io::Error` and that `in_cloud`
filters on the flag, but neither drives the binary over a tree where a file that
was readable at walk time is unreadable at read time. So I built it, out of the
branch's own two seams — the flag swap so a flag can be set, `--cloud` so the
file really enters a group, and then the file made unreadable, which is what an
eviction looks like to every stage:

```text
exit:           0
groups:         [['media/reel_0.mov', 'media/reel_2.mov']]
unreadable:     ['media/reel_1.mov']
skipped_cloud:  1
stderr:
  dedup-rs: media/reel_1.mov lives in the cloud and was evicted while this scan
  was reading it; it was left out of its group rather than downloaded. --cloud
  to download and compare.
  1 file in cloud — not scanned (--cloud to include)

the evicted file left its group, was named, was counted, and the scan finished cleanly
```

The file left its group, the group survived as a real pair rather than a half
group, nothing crashed, the exit is 0, and the message names the file and says
where it went. The claim that the graceful degradation needed no new code is
true, and it is the right finding to have written down.

The second line of that stderr is item 3: it tells a user who has just passed
`--cloud` to pass `--cloud`.

---

## The windowed rate

`Rate::observe` keeps `(when, total)` readings at most every 100 ms, evicts from
the front while the *second* reading is older than `RATE_WINDOW`, and divides
`total - readings[0].total` by the age of `readings[0]`. So the span is bounded
in `[RATE_WINDOW, RATE_WINDOW + SPACING)` once the window is full, a stall makes
numerator and denominator disagree only until the last moving reading falls out,
and then the answer is exactly 0. `Stage::started` really is deleted, so there
is nowhere left to put a lifetime — which, as `LOG.md` says, is worth more than
the test.

I mutated it three ways and all three are caught (table above), including the
one that matters: remove the eviction loop and the span becomes the stage's age,
and `a_stall_reads_zero_within_one_window_instead_of_decaying_like_one_over_t`
fails. The branch's own account of finding that test vacuous is honest and the
fix is real.

**But the width is not pinned from both directions, and the claim says it is.**
`MILESTONES.md:259` reads "The tests now pin the width from both directions." I
mutated `RATE_WINDOW` across a range and the result is not monotonic:

```text
RATE_WINDOW = 100 ms   FAILED  — but on `attempt to subtract with overflow` at
                                 pulse.rs:671, which is the test's own
                                 `window_ms - 400` underflowing, not an assertion
RATE_WINDOW = 500 ms   FAILED
RATE_WINDOW = 900 ms   ok. 276 passed   <- SURVIVED
RATE_WINDOW = 1 s      FAILED
RATE_WINDOW = 2 s      ok. 276 passed   <- SURVIVED
RATE_WINDOW = 3 s      ok. 276 passed   (the shipped value)
RATE_WINDOW = 60 s     FAILED  — "half a window into a stall should read about
                                 half of 3145728, got 778565"
```

The upper pin is real and fails for exactly the right reason. The lower one is
not a pin: the test computes every expectation *from* `RATE_WINDOW`, so it
checks the window is internally consistent, never that it is the width the
design chose, and two of the values I tried sail through. The 100 ms case — the
very mutant the claim says it fixed — is caught by an arithmetic underflow in
the test's own expression, which is a catch in a debug `cargo test` and would
wrap rather than panic under `--release`.

To be exact about what I am and am not saying: **the property the milestone
promises is guarded.** A stall reads zero, a lifetime average is unrepresentable,
and the mutant that reintroduces 1/t is caught. Only the specific choice of
three seconds is unguarded, and a 2-second window would still satisfy the spec.
The defect is that the claim states coverage it does not have, which is the same
class as `REVIEW-M8`'s item 4. `LOG.md`'s own description of the fix is careful
and accurate; it is the one-sentence summary in `MILESTONES.md` that overreaches.

### The current file

Recorded where a read *starts*, not where a file is opened, and the reason in
`LOG.md` is the best thing on this branch: stages 3 and 4 hand descriptors
forward, so the comparison — the long stage, the one a placeholder stalls in —
opens nothing, and the earlier one-line version was blank for exactly the case
the feature exists for. Found by running the thing on a pty, not by reading it.
`a_read_from_a_carried_descriptor_still_names_its_file` pins it on
`Effort::opens() == 1`, which is what the code *did* rather than what it says
about itself, and my `current-file-never-recorded` mutant is caught by it.

Middle-truncation keeps the identifying tail (`right = room - 1 - left`, with
`left = (room-1)/3`), drops the name entirely rather than showing a useless stub
below `LEAST_NAME`, and the same rule is spelled the same way in `pulse.rs` and
`view.rs`. Reproduced on a real pty:

```text
Comparing…  ░░░░░░░░░░░░░░░░░░░░░░░░   0%  0/4 · 3.1 GiB/s  ← media/holidays/IMG_20190711_183012_reel.MOV
--quiet paints nothing, terminal or not: ok
a pipe is silent and says nothing about the cloud: ok
the pre-flight menu offers --cloud, off: ok
all 4 cases held
```

---

## `--cloud`

`FLAGS` entry present with `default: "skip them, and say how many"`; hand-coded
menu row present, `MENU_EXPOSURE` grown to 18, `LEAST_HEIGHT` raised 16 → 17 so
the ninth row is not off the bottom — which is the kind of thing that gets
forgotten. My `menu-row-for-cloud-removed` mutant is caught by
`every_flag_in_the_table_has_been_decided_about_for_the_form`, so the guard is
real in this direction too. The help text says it plainly, as the spec demands:

```text
      --cloud             include cloud placeholder files — files whose bytes
                          live on a provider's server, not this disk.
                          Comparing one DOWNLOADS it in full, which costs
                          bandwidth and can be slow; without this they are
                          skipped and counted
```

---

## `tools/cloud-validate.sh`, reviewed as code

It is read-only — `--list`, no deletion flag, the JSON report written to
`mktemp` outside the tree and the transcript to the CWD, nothing written inside
the tree. `du -sk` before and after is the right measure and the comment says
why apparent size would be the wrong one. The independent `find -flags +dataless`
count before and after is a genuinely good second proof that does not depend on
`du` at all. It does **not** require dataless files to exercise its printing —
it prints a NOTE and carries on — which the brief asked me to check, and I
confirmed by running it against a scratch tree of my own (never real cloud
storage):

```text
== the default scan: no --cloud, no deletion flag, --list only ==
   exit 0
skipped_cloud: (absent — the scan met no cloud file)
before: 9216 KiB
after:  9216 KiB
PASS: not one block was fetched. The scan read no cloud file.
```

One defect, item 4: line 67's `log "   exit $?"` follows a pipeline into `tee`,
so it reports `tee`'s status and never the scan's.

```text
$ zsh -c 'false | tee /dev/null; echo "exit $?"'
exit 0                     <- the failing command exited 1
$ zsh -c 'false | tee /dev/null; echo "${pipestatus[1]}"'
1                          <- what the line means to report
```

This is the same trap `REVIEW-M8` caught itself in, which is why I looked for
it.

---

## Standards, re-derived

Counts from my own builds of both sides:

```text
main@644c66f      $ cargo test    test result: ok. 258 passed; 0 failed
m9-cloud@962fa3c  $ cargo test    test result: ok. 276 passed; 0 failed
                  $ cargo clippy --all-targets -- -D warnings   CLIPPY_EXIT=0
                  $ cargo fmt --check                           FMT_EXIT=0
```

258 → 276 reconciles exactly. The diff adds eighteen `#[test]` attributes and
removes none — I checked both by counting attributes across the diff and by
`git grep -c` on each side, and the added test *functions* are eighteen named
tests, not helpers.

```text
$ python3 rust/evidence/scripts/claim_audit.py M9      M9: 21 block(s), 0 untraced
$ python3 rust/evidence/scripts/claim_audit.py --all   0 untraced anywhere

$ ls /Volumes   (before)   \]
$ python3 conformance/run.py --gate all --candidate '…'
  33 check(s), 0 failed
  gate all: green
$ ls /Volumes   (after)    \]
$ hdiutil info | grep -c dedupconf     0
```

The `\]` volume was there before and after and nothing here touched it. The
only `/Volumes` path anything in this review reached is the ephemeral
`dedupconf` image the `m4-trashes` check attaches and detaches itself.

### Flash parity

`m8_flash_inert.py` against my own `git archive` build of the base:

```text
scenario       files   bytes_read         opens    groups ours/base
typical         1600      2812459           430        64   0.984x
samesize         142      3702784           142         4   1.000x
widegroup         70     14501888            84         8   1.000x
photos            13     47846337            11         3   1.011x
bigheader          8     32677888             8         1   1.010x
prefixtrap        32     16973824            32         4   1.008x
rescan          1600       351680             2         1   1.019x

geomean over 8 scenarios: 1.004x
noise floor (base against itself, same corpus, same rounds): 0.998x
load average at the end: 3.29 3.49 3.92
```

Every counter column is identical to the claim's table, digit for digit, which
is the part that is a fact. My geomean (1.004x) sits inside my noise floor
(0.998x) and I measured at load average 3.3 on a shared machine. Counters over
timings, as M7 ruled.

---

## Binding items

### 1. The I/O policy verifies itself against its own constants, so a false shield and an inverted policy both pass everything

This is the one I care about. `refuse_materialization` (`cloud.rs:86`) sets the
policy and then asks `materialization_refused` whether it took — but both use
the *same* symbols for scope and for value, so any wrong value is
self-consistent and invisible. Two one-character mutants, each with all 276
tests green and every behavioural case of `m9_dataless_walk.py` still passing:

```text
IOPOL_SCOPE_PROCESS: 0 -> 1     the policy binds the calling thread only. The scan
                                reads on rayon's workers, which my C probe shows see
                                `refusing=no`. A shield over nobody.
IOPOL_..._OFF: 1 -> 2           2 is IOPOL_MATERIALIZE_DATALESS_FILES_ON. The process
                                asks the kernel to DOWNLOAD, and reports success.
```

The shipped values are correct — I verified all five against the SDK header
above — so nothing is broken today. What is missing is any guard at all on the
milestone's backstop mechanism, and `SF_DATALESS` shows the discipline that is
absent here: it is pinned to its literal with
`assert_eq!(SF_DATALESS, 0x4000_0000, "the value in <sys/stat.h>")`. The three
`IOPOL` constants get no such line, and the scope — the subtlest of them —
additionally deserves a behavioural check, because pinning a literal can be
"fixed" by editing the literal.

I wrote the guard to be sure it is cheap and that it bites:

```rust
assert_eq!(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, 3);
assert_eq!(IOPOL_SCOPE_PROCESS, 0);
assert_eq!(IOPOL_MATERIALIZE_DATALESS_FILES_OFF, 1);
assert_eq!(IOPOL_MATERIALIZE_DATALESS_FILES_BASIC_MASK, 3);
assert!(refuse_materialization());
let on_a_worker = std::thread::spawn(materialization_refused).join().expect("the probe thread");
assert_eq!(on_a_worker, Some(true), "a worker thread is not covered by the refusal");
```

```text
A. clean tip + my guard:                       test result: ok. 1 passed
B. scope -> THREAD (the false shield):         test result: FAILED. 1 failed
B. OFF -> 2 (ask the kernel to download):      test result: FAILED. 1 failed
B. type -> 6 (the neighbour the comment warns about): test result: FAILED. 1 failed
```

*Required:* pin the three `IOPOL` constants to their header values the way
`SF_DATALESS` is pinned, and add one assertion that the refusal is visible
**from a spawned thread** — that second half is what makes the scope a tested
property rather than a remembered one.

*I will verify by:* applying each of the three constant mutants above in turn in
a scratch clone and requiring `cargo test` to fail on every one, naming a test
that is about the policy rather than about something incidental; and by
confirming the worker-thread assertion alone fails under the scope mutant, then
rebuilding and requiring the suite green and the release binary back to its
pre-mutant hash.

### 2. `MILESTONES.md` says the window is pinned from both directions, and it is not

`MILESTONES.md:259`: "The tests now pin the width from both directions."
`RATE_WINDOW` can be changed to 900 ms or to 2 s with all 276 tests green
(table above). The test derives every expectation from `RATE_WINDOW` itself, so
it can only ever check internal consistency; the 100 ms case is caught by an
underflow in the test's own arithmetic rather than by an assertion, and would
wrap instead of panicking under `--release`.

I am **not** asking for the window to be defended to the millisecond — 2 s would
satisfy the spec, and the property that matters is genuinely guarded. The defect
is a sentence claiming coverage that does not exist, on the milestone's other
headline change. `LOG.md`'s own account of the same fix is accurate; only the
summary overreaches.

*Required:* either add an assertion that fixes the width against a literal
(`assert_eq!(RATE_WINDOW, Duration::from_secs(3))` with a sentence saying why
three), or correct `MILESTONES.md` to say what the tests actually pin — the
shape of the decay and the upper bound — and say that the exact width is a
tuning choice guarded by neither.

*I will verify by:* re-running my `RATE_WINDOW` sweep — 100 ms, 500 ms, 900 ms,
1 s, 2 s, 60 s — and requiring either that every one of them fails `cargo test`,
or that the claim no longer says "both directions"; and by re-reading
`MILESTONES.md` against whichever was chosen.

### 3. A `--cloud` run that meets an eviction tells the user to pass `--cloud`

`main.rs:305` prints `cloud::notice(skipped_cloud)` unconditionally. Under
`--cloud` the walk-time skip count is zero, but `Cloud::evicted` adds every
evicted file to the same counter (`cloud.rs:234`, deliberately, and the reason
given is sound), so the notice fires. Measured in my eviction run above:

```text
  dedup-rs: media/reel_1.mov lives in the cloud and was evicted … --cloud to
  download and compare.
  1 file in cloud — not scanned (--cloud to include)
```

The second line is wrong twice on this path: the file was not "not scanned" by
choice, and `--cloud` is already on. The line above it has already said the true
thing better. Reachable in production whenever a provider evicts a file during
a `--cloud` run.

*Required:* suppress the skip notice, or word it differently, when
`options.cloud` is set — the eviction sentence already covers that case.

*I will verify by:* re-running my `rv9_evicted.py` (flag swap + `--cloud` + an
unreadable member) and requiring the eviction sentence to remain and the
`(--cloud to include)` line to be absent; and by re-running
`m9_dataless_walk.py` case 3 to confirm a clean `--cloud` run still says nothing
at all.

### 4. `cloud-validate.sh` reports `tee`'s exit status as the scan's

`tools/cloud-validate.sh:66-67` pipes the binary into `tee` and then prints
`$?`, which in zsh is the last element of the pipeline. The scan's status is
never shown, and this is the script the maintainer runs on real cloud storage
where the transcript *is* the record.

*Required:* use `${pipestatus[1]}` (captured immediately after the pipeline),
or drop the `tee` and redirect.

*I will verify by:* running the script against a scratch tree with a binary that
exits non-zero and requiring the transcript to show that status.

---

## Non-binding notes

1. **The portable path's justification is stated as a fact and is a judgment.**
   `walk.rs`'s comment says "a filesystem that will not answer an attribute
   request is not one that has cloud placeholders on it." That is plausible and
   I could not test it under my constraints. But adding `ATTR_CMN_FLAGS` to
   `WANT_COMMON` widened the door into `gather_portable` — any directory whose
   bulk read declines one attribute now loses walk-time detection entirely — and
   the sentence reads as though the case cannot arise. Worth one clause saying
   the I/O policy is what holds the law there, and that such files surface as
   evictions rather than skips. The behaviour is right; the description is more
   confident than the evidence.

2. **`m9_dataless_walk.py` cannot be composed with another mutant in `cloud.rs`.**
   It proves its own restore with `git diff -- rust/src/cloud.rs`, so any other
   uncommitted change to that file reads as a failed restore. It cost me a
   wrong result — I recorded two mutants as caught when nothing had caught them
   — and I only found it because the failure list said "the source was NOT
   restored" rather than naming a case. `LOG.md` records fixing exactly this
   defect in `m9_mutants.py`; the same fix has not reached this script. Comparing
   the file's bytes against the bytes read at entry would close it.

3. **The system default is `..._ON` (2), which is worth writing down.** My probe
   found that a process which never calls `setiopolicy_np` has materialization
   *enabled*. That is the strongest single argument for the policy existing, it
   is not stated anywhere in the claim, and it is the sentence that tells the
   next reader the call is load-bearing rather than belt-and-braces.

4. **`--cloud` declines to install rather than lifting — ratified, and worth a
   line in `MILESTONES.md`.** The spec says "lift"; the code declines to install,
   and `LOG.md` argues the difference well. I agree with the choice. The
   observable consequence — a `--cloud` run inside an environment that has
   already set the policy will still refuse to download — is not written down
   anywhere a reader of `MILESTONES.md` would find it.

---

## Rulings — checked, and found right

* The detector is the flag and only the flag; the allocated-vs-logical heuristic
  is correctly refused, and my `thin` corpus (sparse and `UF_COMPRESSED` files)
  passes the fence untouched.
* The exclusion sits upstream of everything — grouping, sampling, the cache, and
  any proof at apply — because it sits upstream of the record.
* `ProvenGroups` is untouched by this branch.
* Dropping skipped files before `files_seen` is the right call and consistent
  with what the protocol's honesty fields mean.
* Counting an eviction into the same total as a walk-time skip is right: from a
  user's point of view they are one thing.
* Asking the flag rather than the errno at `in_cloud` is right, and the
  `#[cfg(test)]` on `refused_materialization` with a comment saying the scan
  must not branch on it is better than either alternative.
* The ring over an EWMA, for the reason given: an EWMA has the same shape as the
  bug.
* Deleting `Stage::started` so a lifetime average is unrepresentable rather than
  merely unwritten.
* Recording the current file at the read rather than at the open — and finding
  that by running the binary rather than by reading it.
* The two-place skip notice: the short form in the always-present row, the
  sentence where there is a line for it.
* `claim_audit.py` catching the builder twice on this branch is the process
  working.

## Defects confirmed as pre-existing, and not M9's

SIGHUP and SIGTERM leave the terminal raw — M5's, recorded by M8, unchanged
here. Ctrl+C at the menu is ignored — M5.1's. `menu.rs`'s `LEAST_HEIGHT`
comment drift predates this branch and the builder cleaned it up while adding
the ninth row, which is the right amount of tidying.

## Fence conditions this review was made under

Worktree `/Users/dogaozden/AI_Projects/dedup-m9-review` on `m9-review`, created
from `m9-cloud` at `962fa3c`. The builder's worktree
`/Users/dogaozden/AI_Projects/dedup-m9` was neither read from nor written to. I
edited no branch but `m9-review`, pushed nothing, deleted no branch, killed no
process I did not start, and used no `sudo`.

Every mutant was applied to an isolated `git clone` under my scratch root,
detached at `962fa3c`, never to this worktree and never the builder's. Each was
reverted, the revert proved by `git status --short` being empty, and the release
binary rebuilt to `4891929c…` before the next thing ran — printed beside each
mutant above, and confirmed once more at the end.

Nothing in this review went near `~/Library/CloudStorage`,
`/Volumes/My New Passport`, or any real user data. `cloud-validate.sh` was run
**only** against a scratch tree I generated, never against real cloud storage.
`/Volumes` held `\]` before and after every run and
`hdiutil info | grep -c dedupconf` reported `0`; the only `/Volumes` path
anything touched is the conformance gate's own ephemeral image. Every corpus
lives under my scratch root and every `--auto --yes` deletion went to a scratch
`HOME`'s Trash.

The machine was shared throughout; load average ran between 3.0 and 3.9 and is
printed beside the timings. The only figures I claim as facts are counters.

## How to clear M9

Close the four items. Item 1 is the only one that adds a guard rather than
correcting a sentence, and it is eight lines that I have already written and
proved bite. Items 2, 3 and 4 are a sentence, a condition on a print, and a
`${pipestatus[1]}`.

Nothing here is a design objection. The shape of this milestone — detect at the
walk from a flag the kernel owns, refuse at the process from a policy the kernel
enforces, count what was skipped where a person will see it, and replace the
lifetime average with a window that can actually reach zero — is right, and the
parts of it I tried hardest to break did not break. The law held every way I
came at it.

---

# Re-review — 2026-08-28, at `500d9cd`

**Verdict: APPROVED.**

Two commits on top of `962fa3c`, no history rewrite (`962fa3c` is still an
ancestor, checked with `git merge-base --is-ancestor` rather than taken from the
hand-off) and the base is still `644c66f`. All four binding items are
discharged, each by the recipe I wrote in the first round rather than by the
builder's own script, and all four non-binding notes were taken.

Everything below I ran myself: a fresh `git archive` extraction of `500d9cd`
built beside my `main@644c66f` build, and a separate isolated `git clone`
detached at `500d9cd` for the mutants. The builder's worktree was neither read
from nor written to.

```text
$ shasum -a 256 .../builds/fix/rust/target/release/dedup-rs
27602f507c42b5d373ed8589b568590b2cc410efd77547dc720438e14696764d   (archive build)
$ shasum -a 256 .../mut2/rust/target/release/dedup-rs
2cac29ea2b63aa19da2d3dd4551bc0af17837a7ba91669354d3f9e2b5a0b50b1   (mutant lab)
```

After every mutant this round the source was restored and the binary rebuilt to
`2cac29ea…`, byte for byte, and the lab finished on an empty `git status`.

**Three of the four items were closed more thoroughly than I asked**, and I want
that on the record before the details, because in each case the builder found
something my recipe would have missed.

## Item 1 — discharged, and the guard is behavioural rather than remembered

My test was taken as written, with an attribution comment that states the two
mutants and why the self-consistency made them invisible. My recipe was to apply
each constant mutant and require `cargo test` to fail on each, naming a test
about the policy. Mine, in my own lab, with a fourth mutant I added this round:

```text
mutant                                    result                     failing test
scope 0 -> 1 (THREAD, the false shield)   FAILED. 276 passed, 1      the_policy_constants_are_the_
                                                                     headers_and_the_shield_covers_workers
..._OFF 1 -> 2 (ask it to DOWNLOAD)       FAILED. 276 passed, 1      (same)
type 3 -> 6 (IGNORE_CONTENT_PROTECTION)   FAILED. 274 passed, 3      (same, +2)
BASIC_MASK 3 -> 7 (swallows _ORIG)        FAILED. 276 passed, 1      (same)
```

All four caught, each naming a test that is about the policy rather than about
something incidental.

**The half I most wanted to check.** My review warned that pinning a literal can
be "fixed" by editing the literal, so I stripped the four `assert_eq!` lines out
and left only the behavioural assertion, then applied the false shield:

```text
thread '...the_policy_constants_are_the_headers_and_the_shield_covers_workers'
panicked at src/cloud.rs:365:9:
assertion `left == right` failed: a worker thread is not covered by the refusal
test result: FAILED. 0 passed; 1 failed
```

The behavioural half catches it on its own, with exactly the right message. The
scope is now a tested property, not a remembered one.

**And the builder built the mutant I only described.** `m9_policy_guard.py`
carries a fourth case — "scope -> THREAD, with its literal pin edited to agree"
— which is precisely the defeat I named in the item, and the worker assertion
catches it. That is better evidence than I asked for, and it is the difference
between a guard and a guard that has been attacked.

## Item 2 — discharged, and solved a third way

I offered two shapes: pin the width to a literal, or correct the sentence. The
builder did neither and did something better — derived a bracket from the spec's
own two requirements (`>= 1s`, because a shorter window cannot bridge the gaps
between 512 KiB reads and would report ordinary reading as a stall; `<= 5s`,
because `M9-CLOUD.md` asks that a stall read ~0 "within a few seconds") and
asserted *that*, while correcting the claim to say exactly what is and is not
pinned. `MILESTONES.md` now separates the shape of the decay, which is pinned,
from the width, which is bracketed and not pinned to a value, and says so in
those words.

My sweep, re-run against the fix — the same values as round one plus four more:

```text
RATE_WINDOW      round 1 (962fa3c)   round 2 (500d9cd)   caught by
100 ms           FAILED (underflow)  FAILED              the bracket
500 ms           FAILED              FAILED              the bracket
900 ms           ok — SURVIVED       FAILED              the bracket
999 ms           (not run)           FAILED              the bracket
1 s              FAILED              FAILED              half-a-window shape
2 s              ok — SURVIVED       ok  (admitted)
3 s              ok  (shipped)       ok  (shipped)
4 s              (not run)           FAILED              readings-count bound
5 s              FAILED              FAILED              readings-count bound
6 s              FAILED              FAILED              the bracket
60 s             FAILED              FAILED              the bracket
```

**My round-one survivor at 900 ms is now caught**, and the admitted set is
exactly `{2s, 3s}`. The 100 ms case no longer depends on an arithmetic underflow
— it fails on the bracket assertion, with a message naming the bracket — which
also closes the `--release` concern, and the evidence file records a
`cargo test --release` run rather than leaving me to wonder.

`m9-rate-window.txt` closes with "the two widths the spec allows are the two the
tests admit". I suspected that of being an overstatement and checked it rather
than writing it up: 4 s and 5 s are both refused by the full suite (by
`polling_faster_than_the_spacing_does_not_grow_the_window`, on the readings
count, not by the bracket). The sentence is precise. Recorded because my
suspicion was wrong and the check is what made the difference.

## Item 3 — discharged, and the builder found the half I missed

I named the skip notice. The builder fixed that and then found the same flaw one
sentence earlier, in `evicted_notice`'s own tail — "--cloud to download and
compare" was equally nonsense to a run that had already passed `--cloud` — and
also threaded `wants_cloud` through `run_live` into `summarise_live`, so the
**live browser's closing summary** is fixed too. I had only tested the frozen
path. The `LOG.md` note on why the `--cloud` eviction case is reachable in
production is correct: under `--cloud` the policy is not installed, so a dataless
read normally downloads and succeeds, and it is a provider or network failure
that leaves the file unread and still dataless.

My own fault injection, re-run with assertions updated for the corrected
wording:

```text
exit:           0
groups:         [['media/reel_0.mov', 'media/reel_2.mov']]
unreadable:     ['media/reel_1.mov']
skipped_cloud:  1
stderr:
  dedup-rs: media/reel_1.mov lives in the cloud and was evicted while this scan
  was reading it; it was left out of its group rather than downloaded. Its bytes
  could not be fetched.

the evicted file left its group, was named, was counted, and the scan finished cleanly
```

The eviction sentence survives and still names the file; `--cloud` appears
nowhere in the output of a run that passed `--cloud`; the file still leaves its
group and the group still survives as a real pair.

The second half of my recipe — that a clean `--cloud` run still says nothing,
and that the *default* path keeps the advice that is actually useful:

```text
2. one member is a placeholder     stderr: '1 file in cloud — not scanned (--cloud to include)'
3. --cloud puts it back            stderr: ''      skipped_cloud: (absent)
5. the whole tree is placeholders  stderr: '6 files in cloud — not scanned (--cloud to include)'
6. an eviction, under --cloud      stderr: (the eviction sentence, no --cloud advice)
```

Case 6 is new: the builder added a regression test for this item to their own
harness. One check I made that the item did not ask for: the machine-readable
count is **not** suppressed — `skipped_cloud: 1` is still in the report under
`--cloud`. Only the human advice is dropped, which is the right split.

## Item 4 — discharged

```text
$ DEDUP_BIN=<a stand-in that writes a report and exits 3> ./tools/cloud-validate.sh <scratch tree>
   exit 3
NOTE: the scan itself failed. Everything below still measures the tree,

$ DEDUP_BIN=<the real binary> ./tools/cloud-validate.sh <scratch tree>
   exit 0
PASS: not one block was fetched. The scan read no cloud file.
```

`${pipestatus[1]}` captured on the line straight after the pipeline, with a
comment saying why `$?` is wrong there. The failing case now says so loudly; the
passing case is unchanged and raises no spurious note. At `962fa3c` the first
case printed `exit 0`.

## Non-binding notes 1-4 — all taken

Note 1: `gather_portable`'s doc comment is rewritten to separate **what is
proven** (a placeholder reaching that path becomes a record, its read fails under
the policy rather than downloading, and it surfaces as a named and counted
eviction) from **what is relied upon and not proven** (that a filesystem
refusing an attribute request is unlikely to host placeholders), and states in as
many words that neither of us could test the latter. It also records that adding
`ATTR_CMN_FLAGS` to `WANT_COMMON` widened the door into that function. That is
the honest shape.

Note 2: `m9_dataless_walk.py` compares the file's bytes against the bytes read at
entry; `git diff` is printed for information only. I verified it with the exact
mutant that cost me a wrong result — the scope mutant, in `cloud.rs`, the file
the harness itself edits:

```text
  (no output: identical, so the mutant is gone)
every case held: ...
```

No false failure. The harness now judges its own cases rather than the repository's
git state, and is composable with another mutant in the same file.

Note 3: the kernel default is recorded in `cloud.rs` — "REVIEW-M9 probed a process
that never calls `setiopolicy_np` and found materialization ENABLED. The kernel's
default is to download." That is the sentence that tells the next reader the call
is load-bearing.

Note 4: `MILESTONES.md` now records that `--cloud` declines to install rather than
lifting, and the observable consequence — a `--cloud` run inside an environment
that has already set the policy will still refuse to download.

## The fence and the standards, re-derived on the new tip

The fix touched `cloud.rs`, `main.rs`, `walk.rs` and `tui/mod.rs`, so a fence
proof from `962fa3c` would not have covered it. Re-run, my own 66 cases, my own
corpora, reader count pinned:

```text
$ python3 rv9_fence.py compare fence/base1 fence/fix
66 cases compared, 0 differing
FENCE HELD: every non-interactive output and every effort counter is byte-identical

$ candidate --cloud  vs  candidate --list, same six corpora
6 corpora compared, 0 differing
```

```text
$ cargo test        main@644c66f     258 passed
$ cargo test        m9-cloud@500d9cd 277 passed
$ cargo clippy --all-targets -- -D warnings    CLIPPY_EXIT=0
$ cargo fmt --check                            FMT_EXIT=0
$ python3 conformance/run.py --gate all --candidate '…'
  33 check(s), 0 failed        gate all: green
  /Volumes `\]` before and after; hdiutil info | grep -c dedupconf -> 0
$ python3 rust/evidence/scripts/claim_audit.py M9     11 block(s), 0 untraced
$ python3 rust/evidence/scripts/claim_audit.py --all  0 untraced anywhere
$ python3 rust/evidence/scripts/m9_pulse.py           all 4 cases held
```

276 → 277 reconciles exactly: the diff adds one `#[test]` and removes none, and
that one is the guard from item 1. My round-one mutants still behave: the
detection line deleted from the walk still leaves `cargo test` green at 277 and
is still caught by the flag-swap harness, which is the documented shape and not
a regression.

## Spot-check of the re-captured evidence

`m9-standards.txt`, `m9-policy-guard.txt` and `m9-rate-window.txt` agree with my
own independent runs case for case — 258 and 277, the same four policy mutants
with the same failing test, and the same sweep verdicts for the six widths we
both ran. The two new evidence scripts run and pass in my lab. Nothing in the
re-captured set describes a binary that no longer exists, which is the failure
the builder was bitten by once already and recorded.

## Defects still open, and still not M9's

SIGHUP and SIGTERM leave the terminal raw — M5's. Ctrl+C at the menu is ignored
— M5.1's. Both unchanged and correctly ungraded here.

## Fence conditions this round was made under

Same worktree, `/Users/dogaozden/AI_Projects/dedup-m9-review` on `m9-review`. All
verification in a fresh scratch extraction and an isolated clone of `500d9cd`.
The builder's worktree `/Users/dogaozden/AI_Projects/dedup-m9` was neither read
from nor written to. No pushes, no branch deleted, no `sudo`, no process killed
that I did not start, `main` and `m9-cloud` untouched.

Nothing went near `~/Library/CloudStorage`, `/Volumes/My New Passport`, or any
real user data; `cloud-validate.sh` was again run only against a scratch tree I
generated. `/Volumes` held `\]` before and after and no `dedupconf` image was
left attached.

## Disposition

**M9 is APPROVED at `500d9cd`.** The no-materialization law holds and is now
guarded at both of its mechanisms rather than one: the walk-time detector was
always attacked by the flag-swap harness, and the I/O policy — the backstop that
was invisible to every test in round one — now fails loudly if its scope, its
type, its value or its mask is wrong, and fails behaviourally from a worker
thread even if the literals are edited to agree. The fence holds on my own
corpora against the new tip. The 1/t regression is dead, the width is honestly
described, and the messages no longer advise a flag the run already passed.

Three of the four fixes are better than what I specified, and in each case
because the builder kept pulling on the thread after the item was technically
closed. That is the standard this project's reviews are supposed to produce.
