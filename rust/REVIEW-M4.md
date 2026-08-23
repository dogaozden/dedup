# REVIEW-M4.md — maintainer review of the M4 claim

> **CURRENT VERDICT: APPROVED** — 2026-08-22 re-review at `9df8017`, appended at
> the end of this file. All four binding items below are fixed and verified,
> including the `--stage size --auto` data-loss path, which is now closed by the
> type system rather than by a mode check. Everything from here to the
> re-review is the original CHANGES review, kept intact as the audit trail.

## Original review (verdict superseded)

**Verdict: CHANGES**

Claim commit `5bb7cf3c1d088c0b61c8cf98a21004bba3f90cc0` (deletion in `9612376`),
reviewed at HEAD `ce9fbc5e510dd31f56af977cfc16dbc66537d092`.
Reviewed against REWRITE.md and `dedup.py` at oracle `b6dd81f`.

**This binary must not be run outside scratch trees until item 1 is fixed.**

The M4 gate is green and I reproduced it, `m4-trashes` included. The deletion
machinery a user reaches through documented flags is, as far as I can find,
correct: the survivor guard holds, hardlink clusters go as a unit, records
pinned from outside the tree are never touched, both Trash branches are right,
the TTY refusal works, and cache-attested groups are re-proven from bytes before
anything is removed. I tried to break each of those and could not.

But there is one path that permanently destroys files that are not duplicates,
it is reachable from the public command line, and no gate can see it. The
`--stage size` switch added for the M1 gate reports size buckets as if they were
duplicate groups, and `--auto` will act on them. The harness's own oracle driver
guards exactly this case and the port does not. I reproduced it and lost two
files whose contents provably differ.

That is the one unrecoverable failure this milestone exists to prevent, so M4
does not pass, notwithstanding a green gate. Three smaller deletion-path
divergences follow it.

---

## Evidence — what I ran myself

`ls /Volumes` immediately before each gate run showed only `dedupbench` (another
project's, untouched) and the stray `]` entry, which is a symlink to `/` and
appears in no mount table. No external drive was mounted at any point, and no
command of mine ever named a `/Volumes` path.

M4 gate, prescribed template, from a clean rebuild at HEAD:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m4 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

```text

M4 gate
  m4-deletion              pass
  m4-links                 pass
  m4-trashes               pass

3 check(s), 0 failed
gate m4: green
EXIT=0
```

`diff` against `evidence/m4-gate.txt`: identical. `/Volumes` after the run held
exactly what it held before — the harness detached and unlinked its image as
`run.py:667-675` says it does.

Whole-project gate, template with `{stage}`:

```text

M1 gate    13 cases, all pass
M2 gate    13 cases, all pass
M3 gate    m3-cold pass (13.0 MB read) · m3-warm pass (0.0 MB read) ·
           m3-poison-refusal pass · m3-same-second pass
M4 gate    m4-deletion pass · m4-links pass · m4-trashes pass

33 check(s), 0 failed
gate all: green
EXIT=0
```

Identical to `evidence/all-gate.txt`. `/Volumes` clean afterwards.

Fence: the oracle clone has an **empty `git status` at `b6dd81f`** —
`conformance/run.py`, `conformance/oracle_driver.py`, `conformance/trees.py` and
`dedup.py` are unmodified, so the referee that produced these greens is the one
the maintainers wrote. Every deletion I performed ran either through the harness
or against trees I generated under the session scratchpad, and I used `--hard`
for the destructive checks so nothing of mine reached the user's Trash except
one deliberately-named file in the placement check, which I removed again.

---

## Binding items

### 1. `--stage size --auto` permanently deletes files that are not duplicates

**Severity: data loss. This is the failure the milestone exists to prevent.**

`--stage size` makes `size_stage` (`pipeline.rs:53-86`) report **size buckets**
as `scan.groups`. `main.rs:196-207` then hands those groups to `run_auto`
whenever `--auto` is present, with no check on `options.stage`. The groups carry
`attested: false`, so `verify_attested` waves them through untouched
(`compare.rs:458-461`), `plan_auto` keeps one member per bucket and dooms the
rest, and `remove_record` removes them. Nothing anywhere on that path has read a
single byte of any file's contents.

Reproduced on a scratch tree of three 16-byte files with three different
contents:

```
before:  alpha.bin    d8a73157ce10cd94a91c2079fc9a92c8
         bravo.bin    232555f36a0bb24f621267ffeed5c06e
         charlie.bin  80fda7545b1fc3f70af956e7cbdf33bf

$ dedup-rs <tree> --min-size 1 --stage size --auto -y --hard

  Keeping the Shortest copy in each of 1 groups.
  Delete permanently: 2 files · 32 bytes reclaimed

      bravo.bin
      charlie.bin

  2 files deleted · 32 bytes reclaimed
EXIT=0

after:   alpha.bin
```

Two files with distinct contents, permanently gone, reported as reclaimed. The
same tree and the same flags through the harness's own driver:

```
$ python3 oracle_driver.py <tree> --min-size 1 --stage size --auto shortest -y --hard
EXIT=0
after:   alpha.bin  bravo.bin  charlie.bin      (nothing deleted)
```

The oracle driver is safe because it guards the deletion call on the stage
(`oracle_driver.py:277`):

```python
if args.auto and args.stage == "full":
    return mod.run_auto(groups, args.auto, args.hard, args.yes, cache, ...)
```

The port copied the switch and not the guard.

This violates REWRITE.md §3.1 in the strongest available way. §3.1 says nothing
may be deleted "on the strength of a hash or sample alone" and that every
reported group must trace to bytes compared this scan; here the deletion traces
to nothing but `st_size`. It also defeats §3.2 in spirit: one survivor per
*bucket* is not one survivor per group when the bucket was never a group.

No gate can catch this. `gate_walk` passes `--stage size` but never `--auto`;
`gate_deletion` and `gate_trashes` pass `--auto` but always with `{stage}`
empty (`run.py:564`, `run.py:641`). The two never meet in the harness. They meet
on the command line.

**Change:** refuse to delete when the reported groups are not content-proven.
The minimal fix mirrors the driver — only dispatch to `run_auto` when
`options.stage != "size"` — but I want the stronger form: make it impossible to
express. Have `size_stage` return a `Scan` that is structurally incapable of
reaching the deletion path (a distinct type, or a flag on `Scan` that
`plan_auto` asserts on), so that a future caller cannot reintroduce this by
forgetting a string comparison. If `--stage size` is combined with `--auto`,
exit non-zero with a message saying why, rather than silently listing and not
deleting.
**I will verify by** re-running the three-file reproduction above and requiring
all three files to survive, by running the same with `--auto` and no `--stage`
and requiring normal behaviour, and by reading the guard to confirm it is not a
second string comparison that the next `--stage` value would slip past.

### 2. `--auto newest` keeps the wrong copy when mtimes tie

`Rule::keeper` uses `max_by_key` for `Newest` (`trash.rs:53`). Rust's
`max_by_key` returns the **last** maximum; Python's `max` returns the **first**
(`dedup.py:2226`). Group members are pre-sorted shortest-name-first in both
implementations, so on a tie the oracle keeps the shortest-named copy and the
port keeps the longest-named one. Measured on identical trees, `--hard`, keeper
shown:

```
three files, all mtimes equal
  --auto newest     oracle kept ['c.bin']    rust kept ['aaa.bin']    DIFFER

two tied newest (aaa.bin, bb.bin), one older
  --auto newest     oracle kept ['bb.bin']   rust kept ['aaa.bin']    DIFFER
```

`--auto shortest` and `--auto oldest` agree in both cases, because `min_by_key`
returns the first minimum and so matches Python. Only `Newest` inverts.

This is not data loss — exactly one survivor remains either way — but which copy
survives is the entire user-visible contract of a keep rule, and REWRITE.md §0
makes the oracle authoritative for behaviour. The gate cannot see it: `m4`
only ever runs `--auto shortest` (`run.py:564`), and `doomed_names`
(`run.py:533-547`) derives its answer key from the shortest-path rule alone.

**Change:** make `Newest` select the first maximum, e.g. by taking the minimum
of `Reverse(mtime_ns)` or by folding with a strict `>` comparison.
**I will verify by** re-running the tie cases above and requiring the same
survivor as the oracle for all three rules.

### 3. `--auto oldest` diverges on mtime differences finer than the oracle can see

`Rule::keeper` compares `mtime_ns` (`trash.rs:45`); `KEEP_RULES` compares
`rec.mtime`, the coarse float `st_mtime` (`dedup.py:2225-2227`). At current
epoch values a float64 resolves roughly 240 ns, so two files a nanosecond apart
are *tied* for the oracle and *ordered* for the port, and the tie-break then
picks different files:

```
aaa.bin @ …001.000000000, bb.bin @ …001.000000001, c.bin @ …009
  --auto oldest     oracle kept ['bb.bin']   rust kept ['aaa.bin']    DIFFER
```

Using nanoseconds is the better engineering choice in isolation, which is why I
am not asking you to make the port worse. But it is a divergence from the
authority, in the deletion path, and it is unrecorded.

**Change:** either compare the same quantity the oracle compares for the
`oldest`/`newest` rules, or record this in LOG.md as a deliberate, named
divergence with its user-visible consequence spelled out. I will accept either;
I will not accept it being undocumented.
**I will verify by** re-running the sub-microsecond case and requiring either
parity or a LOG.md entry describing it.

### 4. The deletion plan prints the rule's Debug spelling

`main.rs:257` formats the rule with `{rule:?}`:

```
dedup-rs:  Keeping the Shortest copy in each of 1 groups.
dedup.py:  Keeping the shortest copy in each of 1 groups.
```

Commit `71de381` is titled "fix: print the keep rule by its own name rather than
its Debug spelling" and did add `Rule::name()` and a `Display` impl
(`trash.rs:35-63`) — but the call site was never changed, so `Display` is
reachable only from a unit test and the user-facing line is still wrong. A
commit whose message describes a fix that is not in effect is worth correcting
on its own account.

**Change:** use `{rule}` at `main.rs:257`.
**I will verify by** running `--auto shortest` and requiring the oracle's
lowercase wording.

---

## What I checked and found correct

Each verified by running it, on scratch trees, not by reading alone.

- **Survivor guard.** `plan_auto` (`trash.rs:72-83`) never pushes the keeper's
  index into `doomed`, so no caller can arrange a group to lose every copy —
  structurally the same guarantee as `dedup.py:2602-2611`, and per-group. I also
  confirmed no record can appear in two groups: `small` and `staged` partition
  the record indices, and a record handed to `fallback` is never also in
  `proven`, so a record cannot be kept in one group and doomed in another.
- **Hardlink clusters go as a unit.** `remove_record` (`trash.rs:172-190`)
  removes every name of the inode. Verified: a two-name cluster and a separate
  copy, keeper `k.bin`, both cluster names removed together.
- **Records pinned from outside the tree are never removed.** `deletable()` is
  `names.len() >= nlink` (`walk.rs:60-62`), matching `dedup.py:171-179`.
  Verified: a file hardlinked to a name outside the scanned root survived, the
  outside link survived, and — correctly — nothing at all was deleted from that
  group, because the pinned record was the non-keeper and the keeper is never a
  victim.
- **Trash placement.** `trash_dir_for` (`trash.rs:91-97`) sends boot-volume files
  to `~/.Trash` and everything else to `<volume>/.Trashes/<uid>`, matching
  `dedup.py:2086-2096`, with `mount_point` walking up on `st_dev` exactly as
  `volume_root` does. I verified the home-Trash branch by hand and the
  non-boot branch is what `m4-trashes` exercises on the harness's own image; it
  passed in both my `--gate m4` and my `--gate all` runs. A cross-volume "move"
  cannot silently become a copy in normal operation because the destination is
  chosen on the file's own volume; the `rename` → `copy` + `remove_file`
  fallback (`trash.rs:155-161`) mirrors `shutil.move`, and a failed copy returns
  before the source is removed, so the file is never lost.
- **Collision handling.** `unique_destination` (`trash.rs:126-142`) tries the
  plain name, then `name 2.ext` … `name 9999.ext`, then a timestamped name —
  matching `dedup.py:2099-2109`, including the dotfile behaviour, since the
  `!stem.is_empty()` guard makes `.bashrc` split the way `os.path.splitext`
  splits it.
- **TTY refusal, `-y`, `--hard`, and the default.** Verified all four:
  `--auto` with no `-y` and stdin not a terminal prints the plan, refuses on
  stderr with the oracle's wording, exits 1 and deletes nothing; `--auto -y`
  deletes; `--hard` removes permanently with nothing reaching the Trash; and an
  invocation with no deletion flag removes nothing. The check is on **stdin**
  (`main.rs:272`), which is what `dedup.py:2656` checks.
- **Cache-attested groups are re-proven before deletion.** `verify_attested`
  runs before the plan is printed (`main.rs:241`), refuses the whole group on
  mismatch, and forgets its rows — covered in detail in REVIEW-M3.md and
  exercised end-to-end by `m3-poison-refusal`.
- **Error handling.** The delete loop continues past a failure and reports it
  (`main.rs:289-311`), matching `dedup.py:2673-2681`; a failure cannot reduce a
  group below its keeper, since the keeper was never in `doomed`.
- **Reporting.** `done` counts names and `freed` counts bytes once per record
  (`main.rs:293-294`), which is the oracle's accounting at `dedup.py:2677`.
- **Keep rules.** Exactly `shortest`, `oldest`, `newest`, defaulting to
  `shortest`, with an unknown rule exiting 2 (`main.rs:154-160`) — matching
  `dedup.py:2223-2233` and `dedup.py:2792-2796`. The `shortest` rule uses code
  points, not bytes, so emoji names order the way Python orders them.

---

## Note on the harness's coverage, for the record

While checking what the gate can and cannot see, two things are worth writing
down, neither of them a defect in this port:

- **M4 has no poison-refusal check.** REWRITE.md §5 lists "poison refusal
  end-to-end" under M4, but the harness puts it in m3 (`gate_trust` →
  `poison_refusal`, `run.py:494-522`); `gate_deletion` and `gate_trashes`
  contain no poison logic. The behaviour is tested — just under the other gate.
- **The effort counters are self-reported and the harness does not instrument
  the candidate.** `oracle_driver.py`'s `instrument()` wraps `builtins.open`
  inside the oracle's own process only. I therefore checked the counters
  structurally instead: `opens` and `bytes_read` move only inside `Effort`
  (`effort.rs:22-52`), at a real `File::open` and from the length a real `read`
  returned, and there is no `unsafe`, no mmap and no FFI anywhere in `src/` for
  an uncounted read to hide in. On every corpus I generated the two numbers came
  out *exactly* equal to the oracle's instrumented counts — 59,993 opens and
  137,402,414 bytes on 60,000 small files; 81,338 opens and 1,883,732,233 bytes
  on a 2.5 GiB mixed tree. Nothing here is being marked up.

---

## How to clear this review

Fix items 1-4, re-run `--gate m4` and `--gate all`, and re-claim M4 per §5.2
with captured output and a runnable command line.

Item 1 is the one that matters, and the lesson in it is worth naming, because it
is the same lesson REVIEW-M1 closed with. `--stage size` was added to satisfy a
gate, and it was added faithfully — I ruled it acceptable in REVIEW-M2.md and I
stand by that. What was not done was to ask what the new flag means when it
meets the *other* flags on a tool whose job is deleting files. The harness's own
driver had already answered that question, four words of it, on the line right
after the one that was copied. When you add a mode that changes what a group
means, the deletion path is the first place to go looking, not the last.

---
---

# Re-review — 2026-08-22, at `9df8017`

**Verdict: APPROVED**

Re-reviewed at `9df8017495cf0430e937626a96aecff42a83e342` (fixes in `9c3c5a8`),
against REWRITE.md and `dedup.py` at oracle `b6dd81f`. Working tree clean.

All four binding items are fixed. The one that mattered — permanent deletion of
files that are not duplicates — was not patched where I suggested the minimum
fix; it was closed the stronger way I asked for, so the deletion path no longer
type-checks with unproven groups. I re-ran every verification recipe written
above, and I wrote fresh adversarial cases for the parts of the fix that a
recipe could not anticipate.

**The restriction in the original review is lifted: this binary may now run
outside scratch trees.**

## Item 1 — `--stage size --auto` deletes non-duplicates: **fixed, structurally**

Recipe: *re-run the three-file reproduction and require all three to survive;
run the same with `--auto` and no `--stage` and require normal behaviour; read
the guard and confirm it is not a second string comparison that the next
`--stage` value would slip past.*

The original reproduction, plus a genuine duplicate pair added to the same tree
so I could check the fix did not simply disable deletion:

```
before:  alpha.bin       d8a73157ce10cd94a91c2079fc9a92c8
         bravo.bin       232555f36a0bb24f621267ffeed5c06e
         charlie.bin     80fda7545b1fc3f70af956e7cbdf33bf
         dup-one.bin     d7e3bcfee5ebf7e16f43d4a31b5d11f6
         dup-twoooo.bin  d7e3bcfee5ebf7e16f43d4a31b5d11f6

$ dedup-rs <tree> --min-size 1 --stage size --auto -y --hard
dedup-rs: --auto cannot act on a --stage size scan. Size buckets are files that
merely share a length; nothing has read their contents, so removing one would
not be removing a duplicate. Re-run without --stage.
EXIT=2
after:   all five files present

$ dedup-rs <tree> --min-size 1 --no-cache --auto -y --hard
  Keeping the shortest copy in each of 1 groups.
  Delete permanently: 1 files · 16 bytes reclaimed
      dup-twoooo.bin
EXIT=0
after:   alpha.bin bravo.bin charlie.bin dup-one.bin
```

Nothing is destroyed on the size-only path, the refusal explains itself, and
ordinary deletion still removes exactly the one genuine duplicate and keeps the
shortest-named copy. Both halves of the recipe pass.

**The guard is the type, not a mode check.** I read it rather than taking the
claim:

- `Scan::groups` and `Scan::proven` are private (`pipeline.rs:32-40`), so a
  `Scan` literal can only be written inside `pipeline`. I grepped: the only two
  construction sites are `size_stage` (`proven: false`) and `find_duplicates`
  (`proven: true`).
- `ProvenGroups<'a>(&'a [DupGroup])` (`pipeline.rs:53`) has an unexported tuple
  field, so it can only be built inside `pipeline`. Its only construction site
  is `Scan::proven`, `self.proven.then_some(ProvenGroups(&self.groups))`
  (`pipeline.rs:70`).
- `run_auto` takes `ProvenGroups` (`trash.rs:238-242`) and has exactly one
  caller, inside the `Some(proven)` arm at `main.rs:202-215`. `plan_auto` is no
  longer `pub`.

So a hypothetical future `--stage whatever` cannot slip past: any value other
than `"size"` routes to `find_duplicates`, which does real content work and
earns `proven: true` honestly. The guard keys on whether bytes were compared,
not on how the mode was spelled. That is what I asked for and it is better than
the minimal fix.

## Item 2 — `--auto newest` on a tie: **fixed**

## Item 3 — keep rules compared a finer clock than the oracle: **fixed**

Recipe for both: *re-run the tie cases and require the same survivor as the
oracle for all three rules; for item 3, require parity or a LOG.md entry.*

I re-ran my own battery, extended from 15 combinations to 21 with two new
shapes chosen because the fix changed the comparison quantity — a gap just under
float64's resolution at current epoch values and one just over it. Fresh tree
per (rule, implementation), `--hard` so nothing reaches the Trash:

```
case                                   rule       oracle keeps  dedup-rs keeps
tied mtimes (all equal)                shortest   c.bin         c.bin      SAME
tied mtimes (all equal)                oldest     c.bin         c.bin      SAME
tied mtimes (all equal)                newest     c.bin         c.bin      SAME
distinct mtimes                        shortest   c.bin         c.bin      SAME
distinct mtimes                        oldest     bb.bin        bb.bin     SAME
distinct mtimes                        newest     aaa.bin       aaa.bin    SAME
two tied newest, one older             shortest   c.bin         c.bin      SAME
two tied newest, one older             oldest     c.bin         c.bin      SAME
two tied newest, one older             newest     bb.bin        bb.bin     SAME
two tied oldest, one newer             shortest   c.bin         c.bin      SAME
two tied oldest, one newer             oldest     bb.bin        bb.bin     SAME
two tied oldest, one newer             newest     c.bin         c.bin      SAME
sub-microsecond apart                  shortest   c.bin         c.bin      SAME
sub-microsecond apart                  oldest     bb.bin        bb.bin     SAME
sub-microsecond apart                  newest     c.bin         c.bin      SAME
200ns apart (under float resolution)   shortest   c.bin         c.bin      SAME
200ns apart (under float resolution)   oldest     aaa.bin       aaa.bin    SAME
200ns apart (under float resolution)   newest     c.bin         c.bin      SAME
2us apart (above float resolution)     shortest   c.bin         c.bin      SAME
2us apart (above float resolution)     oldest     aaa.bin       aaa.bin    SAME
2us apart (above float resolution)     newest     c.bin         c.bin      SAME

ALL SAME
```

Both previously failing cases now agree: "two tied newest" keeps `bb.bin` on
both sides where the port used to keep `aaa.bin`, and "sub-microsecond apart"
keeps `bb.bin` on both sides where the port used to keep `aaa.bin`. The two new
shapes agree too, and they agree *differently* from each other — at 200 ns the
float can still separate the files and both keep `aaa.bin`, at 1 ns it cannot
and both fall through to the tie-break — which is the behaviour a faithful port
of a float comparison should show.

The mechanism is right in the source as well. `Rule::Newest` is now
`min_by(|l, r| time(r).total_cmp(&time(l)))` (`trash.rs:66-70`) — reversing the
comparator and taking the minimum yields the *first* maximum, because Rust's
`min_by` returns the first minimum where `max_by_key` returns the last maximum.
`oracle_mtime` (`trash.rs:100-103`) reconstructs `st_mtime` as
`seconds as f64 + 1e-9 * nanoseconds as f64`, which is how CPython builds the
float the oracle compares.

Item 3 asked for parity *or* documentation and got both: the deliberate
remaining divergence is stated at `trash.rs:89-98` and again in LOG.md — the
finer `mtime_ns` is still what the cache key and the retained-bytes
revalidation compare, "where being stricter than the oracle only ever means
reading more, never deleting more". That reasoning is correct and I accept it.

## Item 4 — the plan printed the Debug spelling: **fixed**

Recipe: *run `--auto shortest` and require the oracle's lowercase wording.*

```
dedup-rs:  Keeping the shortest copy in each of 1 groups.
dedup.py:  Keeping the shortest copy in each of 1 groups.
```

`main.rs` now formats through `Display` (`{}`), so `Rule::name()` is reachable
from the path it was written for.

## Independent re-verification

Clean rebuild (`rm -rf target`), exit 0. `cargo test` 45 passed / 0 failed,
`cargo clippy --all-targets -- -D warnings` exit 0, `cargo fmt --check` exit 0.

`ls /Volumes` immediately before each gate run showed only `dedupbench`, which
belongs to another project and which I did not touch, and the stray `]` entry,
which is a symlink to `/` and appears in no mount table. No Passport and no
unexpected external volume at any point. Gate commands copied verbatim out of
MILESTONES.md and run unmodified:

```
--gate m4     3 check(s), 0 failed    gate m4: green     EXIT=0
--gate all   33 check(s), 0 failed    gate all: green    EXIT=0
```

`/Volumes` was byte-identical after both runs — the harness detached and
unlinked its image as it says it does.

Fence: the oracle clone has an **empty `git status` at `b6dd81f`**, and
`git diff HEAD -- dedup.py conformance/ REWRITE.md` produces no output, so the
referee is byte-identical to the maintainers' own.

The deletion behaviours I verified by hand in the original review — survivor
guard, hardlink clusters removed as a unit, records pinned from outside the tree
never touched, both Trash branches, TTY refusal, `--hard`, the no-flags default,
continue-on-error, and cache re-proof before removal — are unchanged by this
round except for the keep-rule comparator, and `m4-deletion`, `m4-links` and
`m4-trashes` all pass.

## Residual findings

Neither is blocking and neither is a defect in this port; both are recorded in
REVIEW-M2.md's re-review in full.

1. **The harness's own effort counters can lose updates under `-j8`.** One
   oracle run in 21 reported five fewer `opens` than the other twenty on an
   identical tree, with every other field identical. `opens[0] += 1` and
   `self._tally[0] += len(block)` (`oracle_driver.py:82`, `:42`) are non-atomic
   read-modify-writes called from the oracle's thread pool. It biases against
   candidates, since `check_effort` fails on `opens > oracle["opens"]`. For the
   harness maintainers, not for this repo.
2. **A dangling cross-reference in MILESTONES.md**, where the `--gate all`
   section points at an explanation the rewrite removed from the M2 claim.
   Cosmetic.

## Ruling

The data-loss path is closed by construction rather than by a check that a
future edit could forget, the three behavioural divergences are gone and
covered by tests, and everything else about the deletion machinery still holds
up under both the harness and my own trees. **M4 is APPROVED**, and with it the
project as a whole per REWRITE.md §5.

One thing worth saying plainly, because the original review was blunt about the
mistake: the fix chosen here was the harder one. Making `run_auto` refuse a
mode string would have satisfied the letter of the item in one line. Making the
deletion path impossible to reach with unproven groups required moving
`run_auto` between modules, making `Scan`'s groups private, and inventing a
token type to carry the proof — and it means the next person to add a scanning
mode cannot reintroduce this bug by forgetting anything. That is the right
instinct for a tool that deletes files.
