# REVIEW-M3.md — maintainer review of the M3 claim

**Verdict: APPROVED**

Claim commit `71de381f668e729577b5410608d76c081d2acf20` (cache in `9612376`),
reviewed at HEAD `ce9fbc5e510dd31f56af977cfc16dbc66537d092`.
Reviewed against REWRITE.md and `dedup.py` at oracle `b6dd81f`.

This is the cleanest milestone in the port. I line-read `src/cache.rs` against
`ScanCache` in `dedup.py` and every rule §3.3 names is implemented the way the
oracle implements it, including the two that are easy to get subtly wrong: the
racily-clean comparison is non-strict in the right direction, and a racy row is
dropped whole rather than re-stamped column by column. The property that makes
the cache safe to ship in a deletion tool — that it narrows and never proves —
is enforced structurally, and the re-proof before deletion is real, refuses the
whole group, and discards the rows it was wrong about.

I found no defects in M3. I have no binding items.

---

## Evidence — what I ran myself

M3 gate, prescribed template, from a clean rebuild at HEAD:

```
python3 /Users/dogaozden/ox/dedup-oracle/conformance/run.py --gate m3 \
  --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'
```

```text

M3 gate
  m3-cold                  pass (13.0 MB read)
  m3-warm                  pass (0.0 MB read)
  m3-poison-refusal        pass
  m3-same-second           pass

4 check(s), 0 failed
gate m3: green
EXIT=0
```

`diff` against `evidence/m3-gate.txt`: **identical**, character for character.

The warm scan reads 0.0 MB against the cold scan's 13.0 MB. The harness's
ceiling is a ratio, not an absolute — `WARM_CEILING = 0.05` applied as
`warm["bytes_read"] / cold["bytes_read"]` (`run.py:373-377`) — so 0.0 MB is
comfortably inside it, and `check_sets` is still applied in full to the warm
scan, meaning correctness does not relax when the cache is doing the work.

Independent cache behaviour check of my own, on a tree of files sitting on
every stage boundary, with `DEDUP_CACHE_DIR` pointed at a scratch directory:

```
cold (empty cache)          bytes_read=1019908  opens=60  groups=9
warm (same tree, same DB)   bytes_read= 856066  opens=38  groups=9
--no-cache                  bytes_read=1019908  opens=60  groups=9

groups identical cold==warm:     True
groups identical cold==nocache:  True
warm read fewer bytes than cold: True
```

Standards at HEAD: `cargo test` 39 passed / 0 failed, `cargo clippy
--all-targets -- -D warnings` exit 0, `cargo fmt --check` exit 0.

Fence: the oracle clone at `/Users/dogaozden/ox/dedup-oracle` has an **empty
`git status` and sits at `b6dd81f`**; `conformance/` and `dedup.py` are
untouched. Nothing in `src/` reads outside the scanned root except the cache
database itself, under `DEDUP_CACHE_DIR` or `~/Library/Caches/dedup`, which is
where the oracle puts it too.

---

## Line-read against the oracle

Each of these was checked against `dedup.py` at `b6dd81f`, not assumed.

**Schema and versioning.** `cache.rs:285-293` creates the same nine columns in
the same order as `dedup.py:1680-1687`, and stamps `PRAGMA user_version` with
the same `CACHE_SCHEMA = 3`. On a version mismatch `read_rows`
(`cache.rs:295-304`) drops the table, recreates it, and returns no rows —
matching `dedup.py:1689-1704`, including the detail that a brand-new database
takes the same branch because SQLite defaults `user_version` to 0. Like the
oracle, it does this silently; that is parity, and I would not add a warning.

**Keying and per-volume separation.** Rows are keyed by inode alone, with
`size` and `mtime_ns` as validity columns checked in code rather than in SQL
(`cache.rs:168-173` against `dedup.py:1747`). One database per volume, named by
`diskutil`'s `VolumeUUID` with `sha256(mount point)[..32]` as the fallback
(`device.rs:100-116`), matching `dedup.py:1615-1635`. `cache_home` honours
`DEDUP_CACHE_DIR`, then `~/Library/Caches/dedup` on macOS, then
`$XDG_CACHE_HOME/dedup` or `~/.cache/dedup` — matching `dedup.py:1604-1612`.
`open_for` (`cache.rs:130-136`) additionally opens a database for any further
volume the scan turned out to reach, which the oracle also does lazily.

**The racily-clean rule — the one I checked hardest.** `cache.rs:174`:

```rust
if record.mtime_ns / CACHE_TICK_NS >= row.cached_at {
```

against `dedup.py:1749`:

```python
if rec.mtime_ns // CACHE_TICK_NS >= entry[CACHE_CACHED_AT]:
```

Same direction, same non-strict comparison, same whole-second tick
(`CACHE_TICK_NS = 1_000_000_000` in both), and the same consequence — the row
is trusted only when the file's mtime second is *strictly* older than the
second the writing scan started in. `cached_at` is read once in `ScanCache::new`
(`cache.rs:108`) before anything is read, matching the oracle's
`__init__`-time stamp, so every row a run writes shares one tick.

**A racy row is dropped whole.** `cache.rs:179-182` removes the entire row from
the map, marks the inode touched so `close` will `DELETE` it, and clears it from
`seen`. No column survives. That is `dedup.py:1751-1755` exactly, and it is the
half of §3.3 that a port could plausibly get wrong by clearing only the field
being asked for. `a_racily_clean_row_is_dropped_whole_rather_than_re_stamped`
proves it by writing `Ends` and `Full` and then asking for `Ends`, and
`a_row_one_tick_older_than_the_scan_is_trusted` pins the boundary from the
other side with an mtime of 99.999999999s against a row written at 100.

**Re-stamping on write.** `remember` (`cache.rs:188-213`) sets
`row.cached_at = cached_at` unconditionally after any digest is stored, which is
what stops a racy row whose key still matches from keeping its old timestamp
forever. Matches `dedup.py:1770-1772` including the reasoning.

**Attestation is all-or-nothing per bucket.** `attested_runs`
(`cache.rs:234-262`) sends the whole group back to be read if *any* member lacks
a remembered `Full` digest, then regroups the rest by digest value and keeps
only runs of more than one — so a member whose full digest differs despite
matching ends/deep/sample is silently dropped rather than reported. That is
`dedup.py:1788-1807` line for line. `small_file_stage` carries the equivalent
gate per size class (`stages.rs:304-322` against `dedup.py:1455-1466`).

**The cache narrows, never proves.** Attested groups are marked
`attested: true` (`pipeline.rs:307-310`) and carried to `verify_attested`
(`compare.rs:448-488`), which re-runs the real `split_identical` over the
group's bytes and accepts it only if one run covers every member with nothing
unreadable. On any mismatch it refuses the **whole group** and calls
`cache.forget` on **every** member, not just the odd one out — matching
`dedup.py:1852-1869`. `run_auto` calls it before the deletion plan is even
printed (`main.rs:241`), which is where `dedup.py:2632` calls it too, and
`report_refusals` (`main.rs:317-336`) says so on stderr in the oracle's words.
`m3-poison-refusal` exercises this end to end and passes.

**Opportunistic full digests are honest.** A `Full` digest is only recorded for
a file whose bytes were compared to EOF as part of a still-matching run
(`compare.rs:311-317` and `341-348`), and the hasher is seeded with the retained
prefix and closed with the retained tail so the digest genuinely covers the
whole file (`compare.rs:231-244`). The comment at `compare.rs:226-230` states
exactly why that matters, and it is right: a digest built over only the part
this call happened to read would be remembered as a whole-file digest without
being one. `hash_stage` records only `Ends`/`Deep`/`Sample`
(`stages.rs:187-191`), never `Full` — matching the oracle's `CACHED_FIELD` map
at `dedup.py:997-999`, which deliberately omits `hash_full`.

**Concurrency.** The oracle calls `remember` from inside its pool and leans on
the GIL. This port instead returns learned digests from `split_identical` and
applies them serially in `find_duplicates` (`pipeline.rs:295-299`), and
`hash_stage` looks every digest up before dispatching reads and remembers after
they return (`stages.rs:198-246`). Cache access is single-threaded everywhere,
so there is no lock to get wrong. This is a better answer than the oracle's for
a language without a GIL, and it changes no observable behaviour.

**`--no-cache`.** `main.rs:164` constructs no `ScanCache` at all, so no database
is opened, read, written or created, and an existing one is left untouched —
matching `dedup.py:2842`. My independent run above confirms the groups are
identical with and without it.

**Counters.** Cache hits go through `cache.get`, which is a map lookup and
touches neither `opens` nor `bytes_read`; those only move in `Effort`
(`effort.rs:22-52`), at the syscall. A warm scan therefore reports 0 bytes
because it genuinely read none, which is what `m3-warm` is checking.

**TTL and write-back.** `close` (`cache.rs:265-282`) writes the union of
`touched` and `seen` and prunes rows whose `last_seen` predates
`CACHE_TTL_DAYS`, so a tree nobody edits does not age out of its own cache.
Matches `dedup.py:1809-1849`, `CACHE_TTL_DAYS = 180` in both.

---

## Non-binding notes

- `evidence/m3-gate.txt` is authentic, but the gate *command* quoted above it in
  MILESTONES.md is corrupted — the literal `{flags}` token has been replaced by
  the whole contents of `evidence/m4-flags.txt`, so the command as printed
  cannot be run. This is not an M3 defect and I have made it binding item 3 of
  REVIEW-M2.md rather than holding M3 for it, since the same corruption hit the
  M1 and M2 blocks and is best fixed in one pass.
- MILESTONES.md:221-224 records that M3's gate cannot pass without M4's deletion
  machinery, because `poison_refusal` (`run.py:494-522`) runs
  `--auto shortest -y` and grades by what is left on disk. I confirmed that
  reading of the harness. Building deletion before the cache and claiming in
  spec order was the right call, and disclosing it was the right call.
- `ScanCache::get` takes `&mut self` because a racy row mutates on read. That is
  correct and necessary, and it is worth knowing it is what forces the
  single-threaded cache design described above — not an accident.
- Rust's `/` truncates toward zero where Python's `//` floors, so
  `mtime_ns / CACHE_TICK_NS` would differ for a pre-1970 mtime. Not reachable on
  any filesystem this tool targets, and not worth code.

---

## What this milestone got right that I want on the record

The hard part of M3 is not SQLite. It is that a cache in a deletion tool is a
standing invitation to delete something on the strength of a memory, and this
implementation refuses that invitation in three independent places: an attested
group carries a flag rather than a verdict, the verdict is re-derived from bytes
before any plan is printed, and a group that fails to re-prove takes its cache
rows down with it. Each of those is testable and each is tested. That is the
right shape.
