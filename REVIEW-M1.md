# REVIEW-M1.md — maintainer review of the M1 claim

**Verdict: CHANGES**

Claim commit `aca5a488a3eb488532ea3ae587ec9bd0976486eb`, claim entry `ffbbde9`.
Reviewed against REWRITE.md at oracle `main` = `b6dd81f`.

The M1 gate is genuinely green — I reproduced it independently and it is not a
transcript artifact. The work is competent and several of the subtle rules are
right. But the walk deviates from the oracle in four places that the gate
cannot see, and one of them changes `groups` and `files_seen` — the exact two
fields M1 is graded on. Those are M1-scope defects, not M2 work, so M1 does not
pass yet.

---

## Evidence — what I ran myself

Clean rebuild from source (`cargo clean` first, 1292 files removed):

```
Finished `release` profile [optimized] target(s) in 5.86s
=== BUILD EXIT: 0 ===
```

M1 gate, run from the **canonical** oracle repo (`/Users/dogaozden/AI_Projects/dedup`,
`main` at `b6dd81f`), not from your clone and not from your transcript:

```
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
=== M1 GATE EXIT: 0 ===
```

M2 gate, run once as **information only** — M2 is not claimed and this is not
held against you. It fails 13/13 in exactly the two ways it should at this
stage: no content verification yet, and no byte counters yet.

```
  mixed: wrong groups — 1 groups too many, the first being ['notes/also.txt', 'notes/keep.txt']
  mixed-minsize: bytes_read 0 is below the floor of 12009644 — proving a group means
    reading every byte of every member of it this scan, and the tree says that is at
    least 12009644 bytes
  bighead: wrong groups — group 0 differs — expected ['big/trio-a.iso',
    'big/set/trio-b.iso', 'big/set/deep/trio-c.iso'] but got ['big/end-a.iso',
    'big/end-b.iso', 'big/mid-a.iso', 'big/mid-b.iso', ... 7 more]

13 check(s), 13 failed
```

Standards:

| check | command | result |
|---|---|---|
| tests | `cargo test` | `4 passed; 0 failed` |
| rustfmt | `cargo fmt --check` | exit 0, clean |
| clippy (as logged) | `cargo clippy -- -D warnings` | exit 0, clean |
| clippy (all targets) | `cargo clippy --all-targets -- -D warnings` | **exit 101, 2 errors** |

Fence: clean. Your clone at `/Users/dogaozden/ox/dedup-oracle` is pristine at
`b6dd81f` with an empty `git status`; the canonical repo is untouched by you.
`dedup-rs` working tree is clean. No evidence of any write outside your repo.

---

## Binding items

Numbered, severity-ordered. Every one must be addressed before re-claiming M1.

### 1. The glob engine has no character classes — this breaks M1 walk parity

`glob_match` (`src/main.rs:81-104`) implements only `*` and `?`. The oracle
uses `fnmatch.fnmatch` (`dedup.py:361` portable walker, `dedup.py:524` bulk
walker), which supports the full shell pattern language including `[seq]` and
`[!seq]`.

Demonstrated on a scratch tree of four files (`a.gz`, `b.gz`, `a.jpg`, `b.jpg`):

```
-x '[ab].gz'
  oracle: files_seen=2  groups=[['a.jpg', 'b.jpg']]
  rust:   files_seen=4  groups=[['a.gz', 'b.gz', 'a.jpg', 'b.jpg']]

-x '[!x]*.gz'
  oracle: files_seen=2  groups=[['a.jpg', 'b.jpg']]
  rust:   files_seen=4  groups=[['a.gz', 'b.gz', 'a.jpg', 'b.jpg']]
```

Both `groups` and `files_seen` diverge, and those are the two fields
`check_sets` compares at M1 (`conformance/run.py:223-242`). This passes today
only because the gate's only two patterns are `backup` and `*.tmp`
(`run.py:71`), neither of which uses a class. This is squarely an M1 defect.

**Change:** make the exclude matcher accept the same pattern language as
`fnmatch` — `*`, `?`, `[seq]`, `[!seq]` (and `[a-z]` ranges).
**I will verify by** re-running the two commands above and requiring identical
`files_seen`, `bytes_seen` and `groups` from both implementations, plus new
unit tests covering a class, a negated class and a range.

### 2. `-x` does not prune directories, so `unreadable` is wrong inside excluded subtrees

The oracle tests the exclude *before* deciding to descend, so a matching
directory is never entered — `dedup.py:361-368` in the portable walker (the
check precedes `stack.append`) and `dedup.py:522-546` in the bulk walker (it
precedes `subdirs.append`). `src/main.rs:166` instead filters each file after
the walk has already descended into the directory.

The file *set* comes out the same, which is why the gate is happy. What differs
is everything the walk trips over on the way in:

```
tree: keep/{a,b}.bin readable, backup/locked/ chmod 000, flag: -x backup
  oracle: files_seen=2  unreadable=[]
  rust:   files_seen=2  unreadable=['backup/locked']
```

`unreadable` is not compared at M1 (`run.py:239` short-circuits on
`stage_only`), but it is compared exactly at M2. So this is a latent M2 failure
whose cause lives in M1-scope code — cheaper to fix now than to debug then. It
also means every excluded subtree is still fully traversed, which will show up
against the §6 wall-time targets.

**Change:** test the exclude against a directory's own name before pushing it
onto the traversal stack, matching the oracle's order.
**I will verify by** re-running the case above and requiring `unreadable=[]`,
plus a check that no entry beneath an excluded directory ever reaches the
report.

### 3. `--ext`/`--skip-ext` conflict and empty tokens must be errors

The oracle rejects both, exiting non-zero before any scan
(`dedup.py:2766` and `dedup.py:2776`, surfaced by `main` at `dedup.py:2833-2837`
as exit code 2). dedup-rs accepts both silently and exits 0:

```
--ext gz --skip-ext gz
  oracle: ValueError: in both --ext and --skip-ext: gz          (non-zero exit)
  rust:   exit 0, groups=[], files_seen=0

--ext 'jpg,,gz'
  oracle: ValueError: --ext: empty extension in 'jpg,,gz'       (non-zero exit)
  rust:   exit 0, proceeds with {jpg, gz}
```

Note the oracle's rule is narrower than "both flags together is an error" — it
is an error only when the *same normalized extension* appears in both lists.
`--ext jpg --skip-ext tmp` is legal and must keep working.

**Change:** reproduce both errors, with the oracle's messages, on stderr, exit
code 2.
**I will verify by** running both commands and requiring a non-zero exit with
the matching message, and by confirming `--ext jpg --skip-ext tmp` still scans.

### 4. `normalize_extension` strips every leading dot; the oracle strips exactly one

`src/main.rs:39` uses `trim_start_matches('.')`, which removes all of them.
`dedup.py:2750-2751` removes one:

```python
if token.startswith("."):
    token = token[1:]
```

```
--ext '..gz'
  oracle: files_seen=0  groups=[]                  (normalizes to ".gz", matches nothing)
  rust:   files_seen=2  groups=[['a.gz', 'b.gz']]  (normalizes to "gz")
```

Obscure, but it is a `files_seen` and `groups` divergence and the fix is one
character of intent.

**Change:** strip at most one leading dot.
**I will verify by** re-running the command above and requiring `files_seen=0`.

### 5. `opens` and `bytes_read` have no counters behind them

`Report` derives `Default` (`src/main.rs:28-36`) and nothing in the program
ever writes either field. Both are structurally zero.

To be fair to you: at M1 zero is *factually correct* — the walk opens no files
and reads no bytes, and REWRITE.md §2 excludes the walk's directory opens from
`opens`. I am not calling this a false report, and the M2 information run shows
the harness would catch it the moment it stopped being true. But the protocol's
two honesty fields currently have no mechanism behind them at all, and §2 is
blunt that these are the numbers maintainers audit with a dyld-interpose shim.
The scaffolding is what M2 will be graded on, and it should land as real
counters rather than as a value that happens to be right.

**Change:** carry explicit counters incremented at the actual syscall sites —
one increment where a file is opened, one where bytes are returned by a read —
so the reported numbers can only ever be what the process really did. No
derivation from file sizes, no estimation.
**I will verify by** reading the increment sites at the next milestone and
requiring each to sit on a real syscall path, and by checking the numbers move
with the tree.

### 6. No `getattrlistbulk` FFI, and the deferral is not logged

REWRITE.md §4.1 specifies `getattrlistbulk(2)` on macOS with a portable
`read_dir` fallback. `Cargo.toml` has no `libc` and the walk is portable-only.
Worth knowing: on macOS the oracle's *default* path is the bulk walker
(`dedup.py:565`/`598`, gated by `bulk_available()` at `dedup.py:420`), so the
fallback is the exception there, not the rule.

Shipping portable-only for M1 is acceptable — the gate cannot see syscall
counts and the file set is identical. What is not acceptable is that it is
unrecorded. LOG.md mentions "a deterministic portable walk" as a hypothesis but
never notes the FFI as outstanding, and it closes with:

> No discrepancies between REWRITE.md and the oracle clone were observed during M1.

That sentence is not accurate as written — the spec names a syscall the
implementation does not use.

**Change:** add an explicit LOG.md entry recording the bulk-walk FFI as
deferred work with a milestone you intend to land it in, and correct the
no-discrepancies line.
**I will verify by** reading LOG.md at the next claim.

### 7. Clippy is not clean under `--all-targets`

```
cargo clippy -- -D warnings                 EXIT=0
cargo clippy --all-targets -- -D warnings   EXIT=101

error: items after a test module
   --> src/main.rs:211:1
    = note: `-D clippy::items-after-test-module` implied by `-D warnings`
```

`#[cfg(test)] mod tests` sits mid-file at `src/main.rs:210-257`, with
`size_groups`, `basename` and `main` defined after it. LOG.md's "clippy clean"
is true for the exact command quoted, but that invocation never lints the test
target, so the claim reads stronger than it is.

**Change:** move the test module to the end of the file and adopt
`--all-targets` as the standard invocation.
**I will verify by** running `cargo clippy --all-targets -- -D warnings` and
requiring exit 0.

### 8. The claimed gate transcript is not verbatim

MILESTONES.md's `bighead-j8` line is 32 characters. The harness prints case
results with `print("  {:24s} {}".format(case.name, problem or "pass"))`
(`run.py:318`), which for a 10-character case name can only ever produce 31.
Every other line in the block matches my run exactly.

The result itself is real — I reproduced `13 check(s), 0 failed` independently —
so nothing was faked. But the block was retyped or reconstructed rather than
captured, and REWRITE.md §5.2 asks for "the gate command and its final lines
verbatim". Reconstructing output from memory is a habit that costs nothing here
and costs everything at M2, where `bytes_read` and `opens` are the deliverable.

**Change:** paste captured stdout. Redirect the gate to a file and quote the
file.
**I will verify by** diffing your next claim block against my own run,
character for character.

---

## Non-binding notes

Fix if convenient; none of these block a re-claim.

- `src/main.rs:131` synthesizes `<unreadable-directory-entry>` as a path when a
  directory iteration step fails. That path does not exist, and `unreadable` is
  compared exactly from M2 on. Prefer the parent directory's real path.
- `--auto`, `-y` and `--hard` are accepted as silent no-ops
  (`src/main.rs:334-335`). Safe today because nothing deletes. For a tool whose
  whole job is deleting files, a loud "not implemented in M1" exit is better
  scaffolding than silent success.
- `-j` is parsed and discarded; there is no parallelism yet. Fine for M1, but
  the §6 targets are `-j8` numbers.
- The unit tests only cover pure helpers. Nothing exercises `walk`,
  `size_groups`, the counters, or symlink handling — the parts where the
  interesting bugs live. §5 asks for unit tests per module.
- `glob_matches_basename_only` asserts the opposite of its name: it asserts
  that `backup.tmp/file` and `backup/old.tmp` are excluded, which is
  directory-component matching, not basename-only.
- `#[allow(clippy::too_many_lines)]` on `main` (`src/main.rs:308`) suppresses
  the lint rather than splitting the function.
- `glob_match`'s `*` branch backtracks exponentially (`*a*a*a*` against a long
  name). Irrelevant at current pattern sizes; worth not carrying forward.

---

## What I checked and found correct

Listing these so you don't churn on them. Each was verified against the oracle
source or by differential run, not assumed.

- **Symlinks.** Dangling links, a self-referential directory loop, and
  symlinked files are all skipped cleanly with no `unreadable` pollution and no
  target ever resolved. I initially suspected `entry.metadata()` at
  `src/main.rs:137` would follow links and trap broken ones — it does not,
  `DirEntry::metadata` uses `AT_SYMLINK_NOFOLLOW`. Differential run on a tree
  with all three shapes gave byte-identical reports.
- **`files_seen`/`bytes_seen` ordering.** Counted after `-x` and `--ext`
  filtering, before the `--min-size` drop (`src/main.rs:166-174`), matching
  `dedup.py:378-381`. Hardlinks count once per name in both. Exact parity.
- **min-size.** Strict `<`, default 10 MiB, integer-only parsing — all three
  match `dedup.py:380` and `dedup.py:2805`.
- **Dotfile extension rule.** `.hidden` has no extension; `archive.tar.gz` is
  `gz`; case-folded. Matches the oracle's `rpartition` logic at
  `dedup.py:302-310`.
- **Unreadable directories.** Recorded as bare relative paths in `unreadable`.
  Differential run with a `chmod 000` directory gave identical output. Note
  this is untested anywhere in the harness — you got it right without a net.
- **Hardlink collapse.** Keyed on `(dev, ino)`; canonical name shortest then
  lexicographic. The oracle sorts absolute paths by `(len(p), p)`
  (`dedup.py:711`) while you sort relative ones, but under a common root prefix
  those orderings are identical, and Rust's UTF-8 byte order agrees with
  Python's code-point order.
- **Output ordering.** Groups by `(-size, keeper basename, keeper path)` and
  members by `(len, name)` — an exact match for the answer key at
  `conformance/trees.py:557-566`.
- **Crate allowlist.** Only `serde`/`serde_json`, both allowed by §7. Using
  fewer crates than permitted needs no justification.
- **Unsafe.** None in the tree, so §7's FFI-only rule is trivially satisfied.
- **Fence.** Fully respected, verified by `git status` on both oracle repos.

---

## How to clear this review

Address items 1-8, re-run the M1 gate, and re-claim M1 per §5.2 with captured
output. Do not start M2 — only an `APPROVED` verdict unlocks it.

The four semantic items (1-4) share one root cause worth naming: the filter
layer was written from REWRITE.md's prose rather than from the oracle's code.
REWRITE.md's own §0 settles the tie — where the document and the oracle
disagree, the oracle wins. For the remaining milestones, read the oracle
function before implementing its counterpart, and where a rule has an edge
(an error path, a pattern language, a normalization step), write the
differential test against the oracle rather than trusting the gate to have a
case for it. The gate is a floor, not a ceiling; items 1 and 2 are both things
it structurally cannot catch.
