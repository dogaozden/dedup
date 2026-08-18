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

## Speed

The scan avoids reading files it doesn't have to:

| Stage | Work | Effect |
|-------|------|--------|
| 1 | `stat()` every file | no reads at all |
| 2 | discard unique sizes | a file with a unique size can't have a twin |
| 3 | hash the first 64 KiB | cheap, eliminates most survivors |
| 4 | hash in full | only for files that got this far |

On a typical tree this reads a low single-digit percentage of the bytes on
disk. The summary line tells you exactly how much it read.

Hashing is BLAKE2b-256. A collision is far less likely than a disk returning
the wrong bytes, but `--verify` adds a byte-for-byte comparison if you want it.

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

Groups are sorted by reclaimable space, so the biggest wins come first.

Bulk actions (`b`) mark every group at once — keep the oldest, the newest, or
the shortest path — which is how you deal with hundreds of groups without
stepping through them one at a time.

**At least one copy in every group always survives.** The tool won't let you
mark them all.

## Deletion

Files go to the Trash, on the correct volume — a file on an external drive
lands in that drive's `.Trashes`, not your home folder, so emptying the Trash
doesn't mean dragging gigabytes back across a USB cable first.

Pass `--hard` to delete permanently instead.

## Options

```
-l, --list          print a report instead of opening the browser
-j, --jobs N        parallel readers (default: CPU count, capped at 8)
    --min-size N    ignore files smaller than N bytes (default 1, skips empty)
-x, --exclude GLOB  skip names matching GLOB (repeatable)
    --verify        byte-compare matches before reporting them
    --hard          delete permanently instead of using the Trash
-q, --quiet         no progress output
```

**On a spinning external drive, use `-j 2`.** Parallel reads make an SSD
faster but cause a mechanical disk to thrash between seek positions.

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
