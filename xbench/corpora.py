#!/usr/bin/env python3
"""The two shapes XBENCH adds, and the registration that makes them buildable.

bench/corpus.py is imported, never forked: the RBENCH scenarios keep their
exact meaning here, so a number from this suite sits next to a number from
that one without an asterisk. These two builders follow the same contract —
`(Corpus, Random, factor)`, write through `c.write`, and let the answer key
fall out of what was written — and are registered into the imported module's
tables at import time. bench/ is not edited.

Neither shape is designed to embarrass a competitor. XBENCH is a measurement
instrument and its credibility is the whole point: `photolib` is modelled on
the media-library case fclones benchmarks itself against, on the theory that
a win on somebody's home field is the only kind worth reporting.
"""
from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench"))
import corpus                                             # noqa: E402

# Bump when either builder changes what it lays down. Kept separate from
# corpus.GENERATOR_VERSION so that a change here cannot invalidate the seven
# inherited corpora, and a change there cannot silently rewrite these two.
XBENCH_GENERATOR_VERSION = 1

# A camera writes the same EXIF/JFIF preamble into every frame it shoots, so
# same-body photos really do share a first few kilobytes and then part company.
# 48 KiB is a plausible metadata-plus-embedded-thumbnail block. It is not aimed
# at any tool's head-read constant and no measurement here depends on where it
# falls relative to one.
CAMERA_HEADER = 48 << 10


def _scaled(base: int, f: float, floor: int) -> int:
    """A count or a size at this scale, with a floor that keeps the point.

    Everything in both builders goes through here. A fixed-size piece left
    unscaled does not shrink at smoke scale, and two or three of them are
    enough to make the smoke corpus larger than the thing it is standing in
    for — which is how a "small" scale ends up costing more than the run.
    """
    return max(floor, round(base * f))


def _photolib(c: corpus.Corpus, rng, f: float) -> None:
    """A media library: many large files, a realistic slice of them duplicated.

    fclones publishes its own numbers against media libraries, so this is the
    shape to be beaten on rather than a shape chosen because we look good on
    it. Everything here is something a real library contains:

      * photos in a year/month tree, 1.5-5 MB, the bulk of the bytes;
      * `reimport/`, holding second copies of about a sixth of them — what an
        import tool leaves behind when it is pointed at a card twice, and the
        commonest real duplicate there is;
      * `Backup/`, a partial second copy of the earliest year;
      * one frame filed into several albums at once, so the answer contains a
        wide group and not only a heap of pairs;
      * a same-body burst: identical size AND identical first 48 KiB, then
        different frames. Nothing short of reading separates these, and they
        are here because cameras produce them, not because they are hard;
      * video, few and large, including one duplicated clip — the most
        expensive single thing a library asks a deduplicator to prove.

    min_size is 1 MiB, matching `photos`: below that a media library is
    thumbnails and sidecars, and no user of one of these tools cares.
    """
    photos = _scaled(100, f, 10)
    videos = _scaled(3, f, 1)
    burst = _scaled(12, f, 2)
    albums = _scaled(4, f, 2)
    video_size = _scaled(28 << 20, f, 6 << 20)
    clip_size = _scaled(30 << 20, f, 5 << 20)
    burst_size = 5 << 20
    reimport_share = 0.15

    def photo_size() -> int:
        return rng.randint(3 << 19, 5 << 20)          # 1.5 - 5 MB

    originals = []
    year, month = 2019, 1
    for i in range(photos):
        if i and i % 24 == 0:
            month += 1
            if month > 12:
                month, year = 1, year + 1
        rel = "photolib/{}/{:02d}/IMG_{:05d}.jpg".format(year, month, i)
        c.write(rel, corpus.rand(rng, photo_size()))
        originals.append(rel)

    def copy_of(rel: str, into: str) -> None:
        """A second copy of a file already written, byte for byte.

        Read back rather than kept in memory: which originals get duplicated
        is decided after they are written, and holding every candidate's bytes
        against that decision would cost hundreds of megabytes of RSS at
        generation time to save a few seconds of reading.
        """
        with open(os.path.join(c.tree, rel), "rb") as fh:
            c.write(into, fh.read())

    for i, rel in enumerate(originals):
        if rng.random() < reimport_share:
            copy_of(rel, "photolib/reimport/{:05d}.jpg".format(i))

    for rel in originals[:max(2, photos // 6)]:
        copy_of(rel, "photolib/Backup/" + os.path.basename(rel))

    hero = corpus.rand(rng, photo_size())
    for i in range(albums):
        c.write("photolib/albums/a{}/hero.jpg".format(i), hero)
    del hero

    # Same camera, same settings: one size, one 48 KiB preamble, different
    # frames. Size and head both settle nothing.
    header = corpus.rand(rng, CAMERA_HEADER)
    for i in range(burst):
        c.write("photolib/2022/burst/DSC{:04d}.jpg".format(i),
                header, corpus.rand(rng, burst_size - CAMERA_HEADER))
    # ...and one true duplicate inside that cluster, so it has an answer and
    # not only a cost.
    frame = corpus.rand(rng, burst_size - CAMERA_HEADER)
    c.write("photolib/2022/burst/DSC9998.jpg", header, frame)
    c.write("photolib/2022/burst/DSC9999.jpg", header, frame)
    del header, frame

    for i in range(videos):
        c.write("photolib/video/MVI_{:04d}.mov".format(i),
                corpus.rand(rng, video_size))
    clip = corpus.rand(rng, clip_size)
    c.write("photolib/video/holiday.mov", clip)
    c.write("photolib/Backup/holiday.mov", clip)


def _backup(c: corpus.Corpus, rng, f: float) -> None:
    """A source tree and a snapshot of it: thousands of small files, mostly
    identical, a few changed since.

    The walk-speed field and the group-count field at once. Almost every file
    is half of a two-member group, so the answer is thousands of small groups
    rather than a handful of large ones, and a tool that hashes quickly but
    keeps books slowly has nowhere to hide.

    The ways a snapshot differs from its source are all here, because they
    cost different amounts to rule out:

      * unchanged        identical bytes — the group to find;
      * edited in place  SAME SIZE, different bytes, so a size bucket hands
                         the pair straight through and something has to look
                         inside it;
      * rewritten        different size, separated for free;
      * deleted / added  present on one side only.
    """
    n = _scaled(12000, f, 300)
    edited = max(4, round(n * 0.04))
    rewritten = max(4, round(n * 0.03))
    deleted = max(2, round(n * 0.02))
    added = max(2, round(n * 0.01))
    dirs = corpus._dir_tree(rng, max(20, n // 60), 6)

    def size() -> int:
        # Source-tree shaped: mostly a few kilobytes, a thin tail of big ones.
        # Floored at 512 so nothing lands on a zero-length file, where the
        # three tools disagree by default and the scenario would stop being
        # about walking and start being about that disagreement.
        if rng.random() < 0.004:
            return rng.randint(256 << 10, 1 << 20)
        return 512 + int(rng.lognormvariate(8.5, 1.0))

    plan = []
    for i in range(n):
        d = dirs[rng.randrange(len(dirs))]
        leaf = "f{:06d}.dat".format(i)
        plan.append("{}/{}".format(d, leaf) if d else leaf)

    marked = sorted(rng.sample(range(n), min(n, edited + rewritten + deleted)))
    edit_set = set(marked[:edited])
    rewrite_set = set(marked[edited:edited + rewritten])
    delete_set = set(marked[edited + rewritten:])

    for i, rel in enumerate(plan):
        data = corpus.rand(rng, size())
        c.write("source/" + rel, data)
        if i in delete_set:
            continue
        if i in edit_set:
            c.write("snapshot/" + rel, corpus.rand(rng, len(data)))
        elif i in rewrite_set:
            c.write("snapshot/" + rel, corpus.rand(rng, size()))
        else:
            c.write("snapshot/" + rel, data)

    # Files the snapshot has and the source does not: a snapshot is older,
    # not smaller.
    for i in range(added):
        c.write("snapshot/attic/old{:05d}.dat".format(i), corpus.rand(rng, size()))


# ─────────────────────────────────────────────────────────────────────────────
# registration
# ─────────────────────────────────────────────────────────────────────────────
#
# corpus.build() dispatches through corpus.SCENARIOS, so the two shapes are
# added to the imported module's tables rather than to the file. bench/ stays
# exactly as RBENCH left it, and corpus_dir/build/load need no xbench-shaped
# special case.

SCENARIOS = {
    #              min_size,   timed, builder
    "photolib": (1 << 20,      True,  _photolib),
    "backup":   (1,            True,  _backup),
}

# (smoke floor, default) on-disk bytes, used only to refuse a corpus that will
# not fit. Both figures are measured, not guessed — see the manifest's
# total_bytes after a build at each scale.
ESTIMATE = {
    "photolib": (110e6, 700e6),
    "backup":   (30e6, 290e6),
}

for _name, _spec in SCENARIOS.items():
    corpus.SCENARIOS[_name] = _spec
    corpus.ESTIMATE[_name] = ESTIMATE[_name]

# The scenarios XBENCH reports on.
#
# RBENCH's scored seven, minus `rescan`, plus these two. `rescan` is left out
# because it is not a shape: corpus.SCENARIOS maps it to the `photos` builder,
# and it exists in RBENCH to gate the cache from inside a single-tool run.
# XBENCH measures the same thing far better — lane B re-runs EVERY scenario the
# way a user would re-run it — so including `rescan` here would build a second
# copy of `photos` and report the same corpus twice under two names. It stays
# buildable: `--scenario rescan` still works.
XBENCH_SCENARIOS = ("needle", "typical", "samesize", "widegroup", "photos",
                    "bigheader", "photolib", "backup")
