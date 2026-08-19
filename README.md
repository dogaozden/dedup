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
| 4 | hash 4 MB spread through the file | settles files that share a header |
| 5 | hash in full | only for what's left, so the answer stays exact |

Stage 4 matters on big files that begin identically — disk images, backups,
container formats. Reading four megabytes from across the file settles them
without reading all of it. On a set of 300 MB images sharing a 100 MB header
it cut reads from 3.15 GB to 0.67 GB, 79% less, for the same answer.

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
    --verify        byte-compare matches before reporting them
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
