# conformance — the referee

This directory decides whether an implementation of `dedup` is correct. It
drives your binary as a subprocess, drives the Python oracle (`../dedup.py`)
beside it on the same trees, and compares. It is maintainer-owned: read it,
run it, never edit it. If a gate looks wrong, write the case down and stop —
do not adjust the referee to agree with you.

Nothing here trusts what your program says about itself. Group listings are
checked against the oracle *and* against a record of how each tree was built.
Byte counts are checked against the oracle's instrumented count *and* against a
floor the tree implies. Deletions are judged by what is left on disk.

---

## 1. Run it

```
python3 conformance/run.py --candidate '<command template>' --gate <m1|m2|m3|m4|all>
```

The template is one command line with four substitutions:

| token     | becomes                                                       |
|-----------|---------------------------------------------------------------|
| `{root}`  | the absolute path of the directory to scan                     |
| `{json}`  | the absolute path where your protocol report must be written   |
| `{flags}` | the scan flags this case calls for, already split into words   |
| `{stage}` | nothing, for you. Ignore it; it exists for the oracle (§6)     |

The template is split with shell word-splitting rules once, then each token is
substituted, so a path containing a space stays one argument.

**The exact command for each milestone**, assuming your binary is at
`target/release/dedup-rs`:

```
# M1 — walk parity
python3 conformance/run.py --gate m1 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'

# M2 — pipeline parity
python3 conformance/run.py --gate m2 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'

# M3 — cache
python3 conformance/run.py --gate m3 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'

# M4 — deletion
python3 conformance/run.py --gate m4 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'

# everything at once, once M4 is in
python3 conformance/run.py --gate all \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

The template is identical for every milestone. Nothing about it changes as you
progress; what changes is which gate you ask for.

**Exit codes.** `0` the gate is green. `1` the candidate is wrong — the report
names the tree, the check and the first difference. `2` the machine or the
harness is wrong (your binary did not run, wrote no report, timed out). A `2`
is never a verdict on your code; fix the plumbing and run again.

Trees are built on first use under `conformance/.trees/` (about 180 MB, on the
local disk, gitignored). They are deterministic: the same seed always writes
the same bytes. Delete that directory to force a rebuild.

---

## 2. The protocol

After any scan, write this JSON object to the path given by `--report-json`:

```json
{
  "groups": [["a.bin", "copies/a.bin"], ["big.iso", "backup/big.iso"]],
  "bytes_read": 12953176,
  "files_seen": 51,
  "bytes_seen": 17629184,
  "unreadable": ["locked.bin"],
  "opens": 89
}
```

Every path is **relative to the scanned root**, with `/` separators and no
leading `./`.

**`groups`** — the duplicate groups. One entry per *record*, not per name: a
hardlinked inode wearing four names contributes exactly one path, because it is
one file. That path is the record's **canonical name — the shortest of its
names, ties broken lexicographically.** (The oracle internally keeps whichever
name its walk met first, which is not stable between runs; the protocol pins it
down so the report is reproducible, and it is the same rule that picks a keeper
under `--auto shortest`, so the reported name is the name that survives.)

- Members within a group: sorted by (length of path, then path).
- Groups: sorted by (`-size`, basename of the first member, first member).
- A group has at least two records. A hardlink cluster that matches nothing
  else is **not** a group.

**`files_seen` / `bytes_seen`** — every directory entry the walk accepted and
its size, counted *after* `--ext` / `--skip-ext` / `-x` filtering and *before*
the `--min-size` filter. Symlinks are never followed and never counted. Each
name of a hardlinked inode counts separately here — this is a count of
directory entries, not of files.

**`unreadable`** — bare relative paths of files that could not be read, sorted,
no error text. Error strings differ between languages and are not compared. A
file is only discovered to be unreadable if something tried to open it, so a
file alone at its size never appears here.

**`bytes_read`** — bytes of file content actually read from inside the root
this scan. **`opens`** — the number of times a file inside the root was opened.
Directory opens (the bulk-metadata walk) are not counted; the oracle does not
count them either.

Both are self-reported, because the harness cannot wrap your syscalls. They are
still checked, twice over (§4), and maintainers spot-audit them with a
dyld-interpose shim. Misreporting them is the one unforgivable failure.

**Flags you must accept**, spelled as the oracle spells them:
`--list`, `--min-size BYTES`, `-j N`, `-x GLOB` (repeatable), `--ext LIST`
(repeatable, comma-separated), `--skip-ext LIST`, `--auto [RULE]`, `-y`,
`--hard`, `--no-cache`. The harness also relies on `DEDUP_CACHE_DIR` choosing
where the cache lives — it always sets it, and never lets a scan near a real
one.

---

## 3. What each gate checks

**M1 — walk parity.** `{stage}` puts the oracle in size-grouping mode, so what
it reports is the candidate set: every size bucket holding two or more records,
after hardlink collapse and the filters. Your M1 binary has no content stages
yet, so what it naturally reports *is* that candidate set — no special flag on
your side. Checked: identical group listing, identical `files_seen` and
`bytes_seen`. Byte and open counts are not checked here.

**M2 — pipeline parity.** Identical groups on every tree and every flag
combination; `files_seen`, `bytes_seen` and `unreadable` identical;
`bytes_read` within ±10% of the oracle's instrumented count *and* not below the
floor; `opens` no greater than the oracle's. Then the whole scan runs a second
time and both reports must agree exactly — groups, counters and all.

**M3 — cache.** Four checks. A cold scan with an empty cache reads at least the
floor. A warm scan of an unchanged tree reads under 5% of what the cold scan
read. A file changed inside the same wall-clock second its cache row was
written is re-read, not trusted — the row still matches on inode, size and
mtime-to-the-nanosecond, and must be distrusted anyway. And a poisoned file
(content changed, original mtime restored, so the row is legitimately trusted)
may be *reported* as a duplicate but must be **refused at deletion**: every
name in that group survives, while the rest of the tree is still cleaned up.

**M4 — deletion.** Your binary and the oracle each get their own freshly built
copy of the tree, their own `HOME`, and their own cache directory. Checked:
nothing outside the expected victim set is removed, no expected victim is left
behind, every removed name lands in the Trash, and — on a temporary disk image
the harness creates and mounts — deletions on a non-boot volume go to that
volume's `.Trashes/<uid>/` and **not** to the home Trash. The expected victims
are worked out from the tree's construction record, not from the oracle:
keeper is the shortest path in the group, and a record pinned by a hardlink
outside the tree is never a victim.

---

## 4. Reading a failure

Failures are one line naming the check and the first difference. What they mean:

| message | what went wrong |
|---|---|
| `wrong groups — group N differs — expected [...] but got [...]` | The Nth group is not the one expected. Compare the two listings; usually a member is missing, extra, or spelled with a non-canonical hardlink name. |
| `... is out of order — ... and both appear elsewhere in the listing` | You found the right groups but sorted them wrong. The key is (`-size`, basename of first member, first member). |
| `N groups are missing, the first being [...]` | You reported fewer groups than exist. Something was narrowed away that should have reached the byte comparison. |
| `N groups too many, the first being [...]` | You reported a group that is not one. Almost always a digest treated as proof, or a hardlink cluster reported as duplicates of itself. |
| `wrong files_seen / bytes_seen` | The walk is admitting or rejecting the wrong entries. Check filter order, symlink handling, and that hardlink names are counted individually. |
| `wrong unreadable list` | Either a file you failed to open was not reported, or you reported one nothing should have tried to open. |
| `bytes_read N is below the floor of M` | You claim to have read less than proving your own groups requires. Every byte of every member of every reported group has to be compared this scan — so either the number is wrong, or a group was reported on a digest. |
| `bytes_read N is ±X% against the oracle's instrumented M` | Reading materially more than the oracle is a pipeline that narrows too late; materially less, with the groups still right, means the number is not real. |
| `opens N exceeds the oracle's M` | More than one open per file somewhere — most likely a small file whose head and tail were read in two passes. |
| `two runs over the same tree reported different ...` | Non-determinism. Usually a set or hash-map iteration order leaking into output, or a first-seen hardlink name used instead of the canonical one. |
| `warm scan ... read N bytes ... nothing was reused` | The cache is not being consulted, or every row is being rejected. |
| `a file changed inside the second its cache row was written was trusted anyway` | The racily-clean rule is missing. Trust a row only when the file's mtime second is **strictly** older than the row's `cached_at`; a racy row is dropped whole, never re-stamped column by column. |
| `'X' was deleted. The cache said its group was proven; the bytes said otherwise` | A cache-attested group was deleted without being re-proven. Re-read and re-compare before removing anything, and refuse the whole group on mismatch. |
| `nothing at all was deleted — refusing the poisoned group is right, refusing every other group along with it is not` | One bad group must not abort the run. |
| `deleted N file(s) it had no business deleting` | The survivor guard failed, or a hardlink-pinned record was treated as deletable. |
| `left N duplicate(s) behind` | Victims were identified and not removed. |
| `the Trash holds N name(s), the tree lost M` | Every removed name goes to the Trash; all names of a hardlinked record go together. |
| `N name(s) went to the home Trash from a file on another volume` | Trash goes to the volume the file lives on. A move across volumes is a copy. |
| `nothing was written to <mount>/.Trashes/<uid>` | Same rule, other direction. |
| `skipped — ...` | A check could not run here (no second volume, no scan short enough to fit inside one second). Skips are printed and never counted as passes. |
| `conformance: the oracle disagrees with the <tree> manifest` | Exit 2. The harness or the oracle is broken, not you. Report it. |

---

## 5. The trees

Built by `trees.py`, deterministic from one seed, each with a manifest written
*beside* the tree — never inside it. Reading that manifest during a scan is
cheating and the oracle driver trips a wire on it.

| tree | size | files | what it is for |
|---|---|---|---|
| `mixed` | 18 MB | 51 | the control: ordinary duplicates, a trio, several extensions for the filter cases |
| `smallzoo` | 3 MB | 104 | files ≤ 64 KiB — the single-open path. Boundary sizes 4095/4096/4097 and 65535/65536/65537, twins parting on their last byte, same-size strangers, empty files |
| `prefix` | 13 MB | 48 | same-size families sharing 4 KiB, sharing 64 KiB, and sharing *both ends* while differing in the middle |
| `bighead` | 138 MB | 11 | past the 8 MiB sampling threshold: pairs agreeing on both ends, differing one byte in the middle and one byte at the end |
| `links` | 2 MB | 11 | hardlinks: a cluster with a twin, a cluster with none, a cluster pinned by a name outside the tree |
| `adversarial` | 4 MB | 27 | symlink loop, broken symlink, `chmod 000` files (one with a size twin, one unreadable pair), emoji / spaces / quotes / embedded newline in names, empty files, `min_size` boundary |
| `deletion` | 8 MB | 32 | unambiguous keepers, a hardlink-pinned member, files that must survive untouched |

Each is scanned with `--min-size 1` unless the case says otherwise, and `mixed`
is scanned five more times under `--min-size`, `--ext`, `--skip-ext` and `-x`.

---

## 6. Proving the referee has teeth

```
python3 conformance/run.py --selftest
```

Runs the oracle against itself on every gate — all four must come back clean —
and then three deliberately doctored candidates from `selftest/`, each wrong in
exactly one way. The run passes only if each one is **caught**:

- `drop_last_group.py` — a perfect report with the last group deleted from it.
- `halve_bytes.py` — honest work, halved `bytes_read` and `opens`.
- `overdelete.py` — a correct deletion, plus one file too many.

A harness that has never failed anything is not a harness. If you change
nothing else here, you can still run `--selftest` to satisfy yourself that a
green gate means something.
