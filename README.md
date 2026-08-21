# dedup

Find byte-exact duplicate files and move the extras to the Trash.

```
dedup                 # scan the current directory, open the browser
dedup ~/Downloads     # scan somewhere else
dedup --list          # print a report, don't open the browser
```

## What counts as a duplicate

Two files are duplicates when their **contents are identical**. Names and
extensions are ignored, so `photo.jpg`, `photo.jpeg`, and `photo.backup` all
match if the bytes are the same — which is what you want for folders full of
rescued or re-downloaded files.

Files under 10 MB are ignored, because they're rarely worth the screen space.
Lower the floor with `--min-size` if you want them.

## Speed

The scan avoids reading files it doesn't have to:

| Stage | Work | Effect |
|-------|------|--------|
| 1 | list every file's size | one bulk syscall per directory, no reads |
| 2 | discard unique sizes | a file with a unique size can't have a twin |
| 3 | hash the first 64 KiB | cheap, eliminates most survivors |
| 4 | hash 4 MB spread through the file | settles big files that share a header |
| 5 | compare the bytes, in step | only for what's left, and it stops early |

Stage 4 matters on big files that begin identically — disk images, backups,
container formats. Reading four megabytes from across the file settles them
without reading all of it. On a set of 300 MB images sharing a 100 MB header
it cut reads from 3.15 GB to 0.67 GB, 79% less, for the same answer. Files
too small to be worth sampling skip it: below 8 MB there is nothing to sample
without reading the whole file, and stage 5 would then read it all again.

Stage 5 reads the survivors in step and splits them apart the moment their
bytes differ, rather than hashing each one to the end and comparing digests.
That is faster twice over. Files that turn out not to match stop being read
where they diverge instead of being read in full. And comparing blocks costs
essentially nothing, where hashing them was the entire cost of a scan on a
fast disk — BLAKE2b managed 1.5 GB/s per core, so a handful of cores hashing
was the ceiling on an SSD that could deliver far more.

Measured against the previous version, same trees, same answers:

| Tree | Readers | Before | After | |
|---|---|---|---|---|
| /Applications, defaults | 2 | 0.33s | 0.13s | 2.6× |
| /Applications, defaults | 8 | 0.20s | 0.08s | 2.4× |
| /Applications, `--min-size 1M` | 8 | 0.59s | 0.35s | 1.7×, 27% fewer bytes |
| 1.29 GB of large files | 2 | 0.79s | 0.15s | 5.4× |
| 1.29 GB of large files | 8 | 0.34s | 0.12s | 2.9× |

Those are the reading stages; the directory walk is unchanged. On a slow
external disk the CPU was never the limit, so expect the fewer bytes rather
than the multiple.

There is deliberately no "fast mode" that stops at stage 4. Sampled agreement
is not proof: two files can match at every sampled point and still differ in
between. A tool that deletes files should not guess.

On a typical tree this reads a low single-digit percentage of the bytes on
disk. The summary line tells you exactly how much it read.

Stage 1 uses `getattrlistbulk()` on macOS, which returns metadata for a whole
batch of directory entries in one syscall instead of one `stat()` per file —
33k syscalls instead of 284k on a /Applications-sized tree. Elsewhere it falls
back to a plain `scandir` walk.

On an SSD it reads several directories at once; on a spinning disk it reads one
at a time, because the heads would otherwise seek between them. dedup asks the
volume which it is and picks for itself. `-p N` overrides that — `-p 1` forces
one at a time, which is also what happens on any volume that won't say what it
is (USB enclosures usually won't).

The narrowing stages hash with SHA-256, which every CPU this runs on has an
instruction for — measured at twice BLAKE2b's speed here. Nothing is ever
called a duplicate on the strength of a hash, though: the last stage compares
the bytes, so a collision could only ever cost an extra comparison. `--verify`
used to add that comparison and is now the default, so the flag does nothing.

## Hardlinks

A hardlinked file is one set of bytes wearing several names, and the storage
is only released when the last name is gone. dedup groups those names onto a
single entry (shown as `+2 links`) and removes them together, so the space it
promises is the space you actually get back.

If a file has links **outside** the scanned tree, deleting the copies inside
the tree would free nothing. Those entries show `⊘` and can't be marked.

## The browser

```
↑ ↓  or  j k    move between copies
← →  or  h l    previous / next group
space           mark / unmark the highlighted copy
a               mark every copy except the highlighted one
u               clear marks in this group
g / G           first / last group
b               bulk actions across every group
ENTER           review and apply
?               help
q               quit without deleting
```

Groups are sorted by file size, largest first.

Bulk actions (`b`) mark every group at once — keep the oldest, the newest, or
the shortest path — which is how you deal with hundreds of groups without
stepping through them one at a time.

**At least one copy in every group always survives.** The tool won't let you
mark them all.

## Deletion

**Nothing is ever deleted on its own.** By default you highlight files, mark
them, press ENTER, and confirm on a screen showing the exact count and size —
four deliberate steps. Bulk actions only *mark*; they don't remove anything.

Files go to the Trash, on the correct volume — a file on an external drive
lands in that drive's `.Trashes`, not your home folder, so emptying the Trash
doesn't mean dragging gigabytes back across a USB cable first.

Pass `--hard` to delete permanently instead.

## --auto

For a fast pass with no browsing, `--auto` keeps one copy per group and removes
the rest:

```
dedup --auto              # keep the shortest path (default)
dedup --auto oldest       # keep the copy with the earliest mtime
dedup --auto newest       # keep the most recent
dedup --auto -y           # skip the confirmation prompt
```

It prints the full plan — how many files, how much space, and a sample of what
goes — then asks once before doing anything. Without a terminal to prompt on it
refuses outright rather than guessing, so a piped or scripted run can't delete
anything unless you passed `-y`.

## Options

```
-l, --list          print a report instead of opening the browser
    --auto RULE     keep one copy per group (shortest|oldest|newest), remove rest
-y, --yes           with --auto, don't ask for confirmation
-j, --jobs N        concurrent readers (default: 8 on an SSD, 2 on a
                    spinning disk)
-p, --parallel N    directories to scan at once (default: 4 on an SSD, 1 on a
                    spinning disk)
    --min-size N    ignore files smaller than N bytes (default 10485760, 10 MB)
-x, --exclude GLOB  skip names matching GLOB (repeatable)
    --verify        no-op; matches are always byte-compared now
    --hard          delete permanently instead of using the Trash
-q, --quiet         no progress output
```

dedup asks the volume whether it is flash and picks the reader count itself:
8 on an SSD, 2 on a spinning disk, where parallel reads only make the heads
seek. `-j N` overrides it.

Useful exclusions:

```
dedup -x '.DS_Store' -x 'node_modules' -x '.git'
```

## Install

```
./install.sh
```

Symlinks `dedup.py` into `~/.local/bin/dedup`. The symlink points back at this
repo, so edits take effect immediately — but moving the repo breaks it, in
which case just run the script again.

Needs Python 3.9 or newer. No third-party packages.

## Tests

```
python3 -m pytest tests/ -v
```

## Benchmarks

`bench/` measures a change to dedup against a fixed corpus, with correctness
as a gate rather than a score.

```
python3 bench/run.py                    # everything, ~2 GB of corpora
python3 bench/run.py --scale smoke      # a tenth the size, for iterating
python3 bench/run.py --selftest         # check that the gates still bite
```

The corpora live on a RAM disk the harness attaches at `/Volumes/dedupbench`,
because a 2 GB benchmark has no business on the boot disk and a real device
would make every number a story about the page cache. Each corpus is written
from a seeded PRNG, so the same `--seed` and `--scale` always produce the same
bytes, and the answer key is written beside the tree — never inside it.

Eight scenarios, each aimed at a different part of the pipeline: `typical`
(a mixed tree), `photos` (few files, all large), `bigheader` (long shared
prefixes, where stage 4 earns its keep), `needle` (100k tiny files, so mostly
the walk), `samesize` (every file the same length with the same first 64 KiB,
so nothing before stage 5 separates anything), `widegroup` (one group of 500,
plus the hardlink cases), `adversarial` (symlink loops, unreadable files, odd
names) and `deletion` (the real command line, checked against the Trash).

### What the gates mean

Nothing is reported as faster until it is first shown to be right. A failure
in any of these fails the whole run and prints no timings at all:

- **pytest** — the existing suite, run against the `dedup.py` under test.
- **correctness** — every group found must match the manifest exactly, down
  to which names belong to which inode and whether the entry can be deleted.
  A scan that reads anything outside the tree it was given fails here too,
  as does one whose byte count moves between identical rounds.
- **adversarial** — the scan finishes, finds the right groups, and reports
  the unreadable file as unreadable instead of quietly skipping it.
- **deletion** — `--auto shortest -y` leaves exactly one copy per group, with
  its bytes intact, every removed file in the volume's `.Trashes`, and
  nothing for a second pass to find.

Bytes are counted by wrapping `open()` in the measuring process, not by asking
the tool: `stats.hashed_bytes` is read as well, and a gap of more than 20%
between the two is itself a failure. There is also a floor. dedup's promise is
that nothing is called a duplicate on the strength of a hash, so every member
of every group must have been read to the end — a scan that reports the right
groups having read less than that did not prove them, and fails. Between the
two, the byte count cannot be improved by reading somewhere the harness
cannot see.

Timings are the scan alone, measured in a
fresh process, three rounds a side with the two sides interleaved, reporting
the fastest of each — the part of a measurement that a drifting machine load
cannot inflate. Results land in `bench/state/results.json`.

`--dedup PATH` points the harness at a different file, which is how a
work-in-progress is measured and how the gates themselves get tested.
