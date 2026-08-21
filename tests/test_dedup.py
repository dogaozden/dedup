"""Tests for dedup's detection pipeline, trash handling, and marking rules."""
import os

import pytest

import dedup
from dedup import (
    Browser,
    DupGroup,
    FileRec,
    ScanStats,
    drop_hardlinks,
    elide_left,
    find_duplicates,
    human_size,
    send_to_trash,
    unique_destination,
    volume_root,
)


def scan(path, **kw):
    """Run the pipeline with test-friendly defaults."""
    opts = dict(excludes=[], min_size=1, jobs=2, verify=False)
    opts.update(kw)
    return find_duplicates([str(path)], stats=ScanStats(), reporter=None, **opts)


def write(path, content: bytes):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


# ─────────────────────────────────────────────────────────────────────────────
# detection
# ─────────────────────────────────────────────────────────────────────────────

class TestDetection:
    def test_finds_identical_files(self, tmp_path):
        write(tmp_path / "a.txt", b"hello world")
        write(tmp_path / "sub" / "b.txt", b"hello world")

        groups = scan(tmp_path)

        assert len(groups) == 1
        assert len(groups[0].files) == 2
        assert groups[0].size == 11

    def test_different_sizes_are_not_duplicates(self, tmp_path):
        write(tmp_path / "a.txt", b"short")
        write(tmp_path / "b.txt", b"a much longer piece of content")

        assert scan(tmp_path) == []

    def test_same_size_different_content_is_not_a_duplicate(self, tmp_path):
        write(tmp_path / "a.txt", b"aaaaaaaaaa")
        write(tmp_path / "b.txt", b"bbbbbbbbbb")

        assert scan(tmp_path) == []

    def test_wide_bucket_of_near_misses_finds_the_one_real_pair(self, tmp_path):
        # Same size and the same head all the way out to DEEP_HEAD, so all of
        # these reach stage 5 as one bucket, and every compare pass strips off
        # only its pivot.
        # Past PARTITION_FANOUT that hands off to grouping on the block, and
        # the real pair still has to come back out of it — and only it.
        head = b"h" * dedup.DEEP_HEAD
        for i in range(dedup.PARTITION_FANOUT + 2):
            write(tmp_path / f"u{i:04d}.bin", head + f"{i:08d}".encode())
        write(tmp_path / "twin_a.bin", head + b"XXXXXXXX")
        write(tmp_path / "twin_b.bin", head + b"XXXXXXXX")

        groups = scan(tmp_path)

        assert len(groups) == 1
        assert sorted(os.path.basename(f.path) for f in groups[0].files) == [
            "twin_a.bin", "twin_b.bin"
        ]

    def test_extension_is_ignored(self, tmp_path):
        """A renamed copy is still a duplicate — this is the documented rule."""
        write(tmp_path / "photo.jpg", b"\xff\xd8\xff" + b"x" * 200)
        write(tmp_path / "photo.jpeg", b"\xff\xd8\xff" + b"x" * 200)
        write(tmp_path / "photo.backup", b"\xff\xd8\xff" + b"x" * 200)

        groups = scan(tmp_path)

        assert len(groups) == 1
        assert len(groups[0].files) == 3

    def test_files_differing_only_past_the_head_sample(self, tmp_path):
        """Guards the stage-3 optimisation: identical heads, different tails."""
        head = b"z" * (dedup.HEAD_BYTES + 10)
        write(tmp_path / "a.bin", head + b"AAAA")
        write(tmp_path / "b.bin", head + b"BBBB")

        assert scan(tmp_path) == []

    def test_large_identical_files_past_head_sample(self, tmp_path):
        body = b"q" * (dedup.HEAD_BYTES * 2)
        write(tmp_path / "a.bin", body)
        write(tmp_path / "b.bin", body)

        groups = scan(tmp_path)

        assert len(groups) == 1

    def test_three_way_duplicate_reclaims_two_copies(self, tmp_path):
        for name in ("a", "b", "c"):
            write(tmp_path / f"{name}.txt", b"x" * 100)

        groups = scan(tmp_path)

        assert len(groups[0].files) == 3
        assert groups[0].reclaimable == 200

    def test_empty_files_skipped_by_default(self, tmp_path):
        write(tmp_path / "a.txt", b"")
        write(tmp_path / "b.txt", b"")

        assert scan(tmp_path) == []

    def test_empty_files_included_when_min_size_zero(self, tmp_path):
        write(tmp_path / "a.txt", b"")
        write(tmp_path / "b.txt", b"")

        groups = scan(tmp_path, min_size=0)

        assert len(groups) == 1
        assert groups[0].reclaimable == 0

    def test_min_size_filters_small_files(self, tmp_path):
        write(tmp_path / "a.txt", b"tiny")
        write(tmp_path / "b.txt", b"tiny")

        assert scan(tmp_path, min_size=1000) == []

    def test_default_min_size_is_10mb(self):
        args = dedup.build_parser().parse_args([])

        assert args.min_size == 10 * 1024 * 1024

    def test_groups_sorted_by_size_largest_first(self, tmp_path):
        write(tmp_path / "small_a", b"s" * 100)
        write(tmp_path / "small_b", b"s" * 100)
        write(tmp_path / "big_a", b"b" * 5000)
        write(tmp_path / "big_b", b"b" * 5000)
        write(tmp_path / "mid_a", b"m" * 900)
        write(tmp_path / "mid_b", b"m" * 900)

        groups = scan(tmp_path)

        assert [g.size for g in groups] == [5000, 900, 100]

    def test_groups_tying_on_size_and_name_order_by_keeper_path(self, tmp_path):
        # Both groups are 40 bytes and both keep a file called "same.bin", so
        # size and basename both tie. Only the keeper's full path separates
        # them, and it has to, or the order is whatever the pipeline emitted.
        write(tmp_path / "d2" / "same.bin", b"b" * 40)
        write(tmp_path / "d2" / "other2.bin", b"b" * 40)
        write(tmp_path / "d1" / "same.bin", b"a" * 40)
        write(tmp_path / "d1" / "other1.bin", b"a" * 40)

        groups = scan(tmp_path)

        assert [g.size for g in groups] == [40, 40]
        assert [g.name for g in groups] == ["same.bin", "same.bin"]
        assert [g.files[0].path for g in groups] == [
            str(tmp_path / "d1" / "same.bin"),
            str(tmp_path / "d2" / "same.bin"),
        ]

    def test_size_beats_reclaimable_in_ordering(self, tmp_path):
        # Five copies of a small file reclaim 1600 bytes; two copies of a
        # larger one reclaim only 1000. The larger file must still sort first.
        for name in ("many_a", "many_b", "many_c", "many_d", "many_e"):
            write(tmp_path / name, b"s" * 400)
        write(tmp_path / "few_a", b"b" * 1000)
        write(tmp_path / "few_b", b"b" * 1000)

        groups = scan(tmp_path)

        assert groups[0].size == 1000
        assert groups[0].reclaimable < groups[1].reclaimable

    def test_exclude_pattern_skips_matching_names(self, tmp_path):
        write(tmp_path / "keep.txt", b"same content here")
        write(tmp_path / "sub" / "keep.txt", b"same content here")
        write(tmp_path / ".DS_Store", b"noise noise noise")
        write(tmp_path / "sub" / ".DS_Store", b"noise noise noise")

        groups = scan(tmp_path, excludes=[".DS_Store"])

        assert len(groups) == 1
        assert all(".DS_Store" not in f.path for f in groups[0].files)

    def test_excluded_directory_is_not_descended(self, tmp_path):
        write(tmp_path / "a.txt", b"payload content")
        write(tmp_path / "node_modules" / "a.txt", b"payload content")

        assert scan(tmp_path, excludes=["node_modules"]) == []

    def test_symlinks_are_not_followed(self, tmp_path):
        real = write(tmp_path / "real.txt", b"content of the file")
        (tmp_path / "link.txt").symlink_to(real)

        assert scan(tmp_path) == []

    def test_nested_directories_are_searched(self, tmp_path):
        write(tmp_path / "x" / "y" / "z" / "deep.txt", b"buried treasure")
        write(tmp_path / "shallow.txt", b"buried treasure")

        assert len(scan(tmp_path)) == 1

    def test_verify_mode_still_finds_real_duplicates(self, tmp_path):
        write(tmp_path / "a.txt", b"verified content")
        write(tmp_path / "b.txt", b"verified content")

        assert len(scan(tmp_path, verify=True)) == 1

    def test_unreadable_file_is_recorded_not_fatal(self, tmp_path):
        write(tmp_path / "a.txt", b"readable content")
        write(tmp_path / "b.txt", b"readable content")
        blocked = write(tmp_path / "c.txt", b"readable content")
        blocked.chmod(0o000)
        try:
            stats = ScanStats()
            groups = find_duplicates([str(tmp_path)], excludes=[], min_size=1,
                                     jobs=2, verify=False, stats=stats, reporter=None)
            # the two readable copies still pair up
            assert len(groups) == 1
            assert len(groups[0].files) == 2
        finally:
            blocked.chmod(0o644)


class TestHardlinks:
    def test_hardlinks_collapse_to_one_entry(self, tmp_path):
        original = write(tmp_path / "a.txt", b"linked content")
        os.link(original, tmp_path / "b.txt")

        # deleting a hardlink frees nothing, so it must not be offered
        assert scan(tmp_path) == []

    def test_hardlink_plus_real_copy_reports_only_the_real_one(self, tmp_path):
        original = write(tmp_path / "a.txt", b"linked content")
        os.link(original, tmp_path / "hardlink.txt")
        write(tmp_path / "real_copy.txt", b"linked content")

        groups = scan(tmp_path)

        assert len(groups) == 1
        assert len(groups[0].files) == 2

    def test_drop_hardlinks_keeps_one_per_inode(self):
        files = [
            FileRec("/a", 10, 0.0, dev=1, ino=100),
            FileRec("/b", 10, 0.0, dev=1, ino=100),   # same inode
            FileRec("/c", 10, 0.0, dev=1, ino=200),
            FileRec("/d", 10, 0.0, dev=2, ino=100),   # same inode, other device
        ]

        assert len(drop_hardlinks(files)) == 3

    def test_collapsed_record_remembers_every_name(self):
        files = [
            FileRec("/long/path/b.txt", 10, 0.0, dev=1, ino=100, nlink=2),
            FileRec("/a.txt", 10, 0.0, dev=1, ino=100, nlink=2),
        ]

        [rec] = drop_hardlinks(files)

        assert sorted(rec.all_names) == ["/a.txt", "/long/path/b.txt"]
        assert rec.path == "/a.txt"          # shortest name shown
        assert rec.deletable                 # both names visible → space is freeable

    def test_record_is_undeletable_when_a_link_lives_outside_the_scan(self):
        # nlink says 3 names exist, but the scan only found one
        rec = FileRec("/seen.txt", 10, 0.0, dev=1, ino=100, nlink=3)

        assert not rec.deletable

    def test_reclaimable_ignores_copies_that_cannot_free_space(self):
        deletable = FileRec("/a", 100, 0.0, 1, 1, nlink=1)
        stuck = FileRec("/b", 100, 0.0, 1, 2, nlink=5)      # links outside scan

        group = DupGroup(100, [deletable, stuck])

        # only one of the two can actually be removed, and one must survive
        assert group.reclaimable == 100

    def test_reclaimable_is_zero_when_nothing_can_be_freed(self):
        stuck_a = FileRec("/a", 100, 0.0, 1, 1, nlink=5)
        stuck_b = FileRec("/b", 100, 0.0, 1, 2, nlink=5)

        assert DupGroup(100, [stuck_a, stuck_b]).reclaimable == 0

    def test_remove_record_deletes_every_name(self, tmp_path):
        first = write(tmp_path / "first.txt", b"shared inode")
        second = tmp_path / "second.txt"
        os.link(first, second)
        rec = FileRec(str(first), 12, 0.0, 1, 1, nlink=2,
                      links=[str(first), str(second)])

        removed = []

        def fake_remover(path):
            removed.append(path)
            os.unlink(path)
            return True, ""

        ok, err = dedup.remove_record(rec, fake_remover)

        assert ok, err
        assert sorted(removed) == sorted([str(first), str(second)])
        assert not first.exists() and not second.exists()

    def test_remove_record_reports_partial_failure(self, tmp_path):
        rec = FileRec("/a", 10, 0.0, 1, 1, nlink=2, links=["/a", "/b"])

        def flaky(path):
            return (path == "/a"), "nope"

        ok, err = dedup.remove_record(rec, flaky)

        assert not ok
        assert "/b" in err


# ─────────────────────────────────────────────────────────────────────────────
# trash
# ─────────────────────────────────────────────────────────────────────────────

class TestTrash:
    def test_unique_destination_avoids_collisions(self, tmp_path):
        (tmp_path / "file.txt").write_bytes(b"existing")

        result = unique_destination(tmp_path, "file.txt")

        assert result.name == "file 2.txt"
        assert not result.exists()

    def test_unique_destination_passes_through_when_free(self, tmp_path):
        assert unique_destination(tmp_path, "fresh.txt").name == "fresh.txt"

    def test_unique_destination_walks_past_several_collisions(self, tmp_path):
        (tmp_path / "f.txt").write_bytes(b"a")
        (tmp_path / "f 2.txt").write_bytes(b"b")
        (tmp_path / "f 3.txt").write_bytes(b"c")

        assert unique_destination(tmp_path, "f.txt").name == "f 4.txt"

    def test_volume_root_of_a_normal_path_is_a_mount_point(self, tmp_path):
        root = volume_root(str(tmp_path))

        assert root.exists()
        assert str(tmp_path).startswith(str(root))

    def test_send_to_trash_moves_the_file(self, tmp_path, monkeypatch):
        fake_trash = tmp_path / "trash"
        monkeypatch.setattr(dedup, "trash_dir_for", lambda p: fake_trash)
        victim = write(tmp_path / "goner.txt", b"delete me")

        ok, err = send_to_trash(str(victim))

        assert ok, err
        assert not victim.exists()
        assert (fake_trash / "goner.txt").read_bytes() == b"delete me"

    def test_send_to_trash_reports_failure_without_raising(self, tmp_path, monkeypatch):
        monkeypatch.setattr(dedup, "trash_dir_for", lambda p: tmp_path / "trash")

        ok, err = send_to_trash(str(tmp_path / "does_not_exist.txt"))

        assert not ok
        assert err


# ─────────────────────────────────────────────────────────────────────────────
# marking rules
# ─────────────────────────────────────────────────────────────────────────────

def make_browser(n_files=3, n_groups=1):
    groups = []
    for g in range(n_groups):
        files = [FileRec(f"/g{g}/f{i}.txt", 100, float(i), 1, g * 100 + i)
                 for i in range(n_files)]
        groups.append(DupGroup(100, files))
    return Browser(groups, "/root", hard=False)


class TestMarking:
    def test_toggle_marks_and_unmarks(self):
        b = make_browser()
        b.fi = 1

        b.toggle()
        assert (0, 1) in b.marked

        b.toggle()
        assert (0, 1) not in b.marked

    def test_cannot_mark_every_copy_in_a_group(self):
        """The core safety rule: one copy always survives."""
        b = make_browser(n_files=2)
        b.fi = 0
        b.toggle()
        b.fi = 1
        b.toggle()

        assert len(b.marked) == 1
        assert b.flash

    def test_keep_only_current_marks_the_rest(self):
        b = make_browser(n_files=4)
        b.fi = 2

        b.keep_only_current()

        assert b.marked == {(0, 0), (0, 1), (0, 3)}

    def test_clear_group_removes_marks(self):
        b = make_browser()
        b.keep_only_current()
        b.clear_group()

        assert b.marked == set()

    def test_marked_bytes_totals_correctly(self):
        b = make_browser(n_files=3)
        b.keep_only_current()

        assert b.marked_bytes() == 200

    def test_bulk_keep_oldest_leaves_one_per_group(self):
        b = make_browser(n_files=3, n_groups=2)

        n = b.apply_bulk(lambda fs: min(range(len(fs)), key=lambda i: fs[i].mtime))

        assert n == 4
        for gi in range(2):
            assert len(b.group_marks(gi)) == 2
            assert (gi, 0) not in b.marked      # mtime 0.0 is oldest

    def test_bulk_keep_newest_leaves_one_per_group(self):
        b = make_browser(n_files=3, n_groups=2)

        b.apply_bulk(lambda fs: max(range(len(fs)), key=lambda i: fs[i].mtime))

        for gi in range(2):
            assert (gi, 2) not in b.marked      # mtime 2.0 is newest

    def test_bulk_replaces_previous_marks(self):
        b = make_browser(n_files=3)
        b.fi = 0
        b.toggle()

        b.apply_bulk(lambda fs: 0)

        assert (0, 0) not in b.marked


class TestAutoPlan:
    """`--auto` picks keepers without a human in the loop, so the rules matter."""

    def _group(self):
        return DupGroup(100, [
            FileRec("/deep/nested/old.txt", 100, 10.0, 1, 1),
            FileRec("/new.txt", 100, 99.0, 1, 2),
            FileRec("/mid/middle.txt", 100, 50.0, 1, 3),
        ])

    def test_keeps_the_oldest(self):
        doomed = dedup.plan_auto([self._group()], "oldest")

        assert len(doomed) == 2
        assert "/deep/nested/old.txt" not in [r.path for r in doomed]

    def test_keeps_the_newest(self):
        doomed = dedup.plan_auto([self._group()], "newest")

        assert "/new.txt" not in [r.path for r in doomed]

    def test_keeps_the_shortest_path(self):
        doomed = dedup.plan_auto([self._group()], "shortest")

        assert "/new.txt" not in [r.path for r in doomed]
        assert len(doomed) == 2

    def test_always_leaves_one_copy_per_group(self):
        groups = [self._group(), self._group()]

        for rule in ("oldest", "newest", "shortest"):
            doomed = dedup.plan_auto(groups, rule)
            assert len(doomed) == 4          # 2 groups × (3 copies - 1 keeper)

    def test_never_plans_to_delete_an_undeletable_record(self):
        group = DupGroup(100, [
            FileRec("/a.txt", 100, 1.0, 1, 1, nlink=1),
            FileRec("/b.txt", 100, 2.0, 1, 2, nlink=9),   # links outside the scan
        ])

        doomed = dedup.plan_auto([group], "oldest")

        assert doomed == []                  # keeper is /a.txt, and /b can't be freed

    def test_every_documented_rule_exists(self):
        assert set(dedup.KEEP_RULES) == {"oldest", "newest", "shortest"}


# ─────────────────────────────────────────────────────────────────────────────
# formatting
# ─────────────────────────────────────────────────────────────────────────────

class TestFormatting:
    @pytest.mark.parametrize("n,expected", [
        (0, "0 B"),
        (512, "512 B"),
        (1024, "1.0 KB"),
        (1536, "1.5 KB"),
        (1024 ** 2, "1.0 MB"),
        (1024 ** 3, "1.0 GB"),
        (1024 ** 4, "1.0 TB"),
    ])
    def test_human_size(self, n, expected):
        assert human_size(n) == expected

    def test_elide_left_keeps_the_filename_visible(self):
        result = elide_left("/very/long/path/to/a/file/report.pdf", 20)

        assert len(result) == 20
        assert result.endswith("report.pdf")
        assert result.startswith("…")

    def test_elide_left_passes_short_strings_through(self):
        assert elide_left("short.txt", 20) == "short.txt"

    def test_elide_left_handles_degenerate_width(self):
        assert elide_left("anything", 1) == ""


class TestGroupModel:
    def test_reclaimable_excludes_the_kept_copy(self):
        files = [FileRec(f"/f{i}", 50, 0.0, 1, i) for i in range(4)]

        assert DupGroup(50, files).reclaimable == 150

    def test_name_comes_from_the_first_file(self):
        files = [FileRec("/dir/report.pdf", 10, 0.0, 1, 1),
                 FileRec("/other/copy.pdf", 10, 0.0, 1, 2)]

        assert DupGroup(10, files).name == "report.pdf"


class TestDisplayPath:
    def test_relative_when_inside_cwd(self, tmp_path, monkeypatch):
        monkeypatch.chdir(tmp_path)
        rec = FileRec(path=str(tmp_path / "sub" / "a.bin"), size=1,
                      mtime=0.0, dev=1, ino=1)

        assert rec.display == os.path.join("sub", "a.bin")

    def test_absolute_when_outside_cwd(self, tmp_path, monkeypatch):
        outside = tmp_path / "scan"
        outside.mkdir()
        here = tmp_path / "elsewhere"
        here.mkdir()
        monkeypatch.chdir(here)
        rec = FileRec(path=str(outside / "a.bin"), size=1,
                      mtime=0.0, dev=1, ino=1)

        assert rec.display == str(outside / "a.bin")
        assert not rec.display.startswith("..")


class TestBulkWalker:
    """The fast macOS walk must agree with the portable one exactly — it
    decides what gets deleted, so a parsing slip would be data loss."""

    def _tree(self, root):
        write(root / "big_a.bin", b"A" * 3000)
        write(root / "big_b.bin", b"A" * 3000)
        write(root / "small.bin", b"s" * 10)
        write(root / "sub" / "nested.bin", b"n" * 2000)
        write(root / "sub" / "deep" / "deeper.bin", b"d" * 4000)
        write(root / "sub" / ".DS_Store", b"noise")
        write(root / "unicode ✓ name.bin", b"u" * 1500)
        (root / "link.bin").symlink_to(root / "big_a.bin")
        os.link(str(root / "big_a.bin"), str(root / "hardlink.bin"))

    def _keys(self, recs):
        return {(r.path, r.size, r.dev, r.ino, r.nlink) for r in recs}

    @pytest.mark.parametrize("min_size,excludes", [
        (1, []),
        (2000, []),
        (1, [".DS_Store"]),
        (1, ["*.bin"]),
    ])
    def test_matches_portable_walk(self, tmp_path, min_size, excludes):
        self._tree(tmp_path)
        sa, sb = ScanStats(), ScanStats()

        a = dedup._walk_scandir([str(tmp_path)], excludes, min_size, sa)
        b = dedup._walk_bulk([str(tmp_path)], excludes, min_size, sb)

        assert self._keys(a) == self._keys(b)
        assert sa.files_seen == sb.files_seen
        assert sa.bytes_seen == sb.bytes_seen

    def test_symlinks_not_followed(self, tmp_path):
        self._tree(tmp_path)

        recs = dedup._walk_bulk([str(tmp_path)], [], 1, ScanStats())

        names = {os.path.basename(r.path) for r in recs}
        assert "link.bin" not in names          # the symlink is skipped
        assert "hardlink.bin" in names          # a hardlink is a real file

    def test_falls_back_when_unavailable(self, tmp_path, monkeypatch):
        write(tmp_path / "a.bin", b"x" * 100)
        write(tmp_path / "b.bin", b"x" * 100)
        monkeypatch.setattr(dedup, "bulk_available", lambda: False)

        groups = scan(tmp_path)

        assert len(groups) == 1

    def test_walk_files_dispatches_to_a_working_walker(self, tmp_path):
        self._tree(tmp_path)

        recs = dedup.walk_files([str(tmp_path)], [], 1, ScanStats())

        assert self._keys(recs) == self._keys(
            dedup._walk_scandir([str(tmp_path)], [], 1, ScanStats()))


class TestParallelWalk:
    """Threading here decides which files exist, so it must never lose one."""

    def _tree(self, root, dirs=12, per_dir=8):
        for d in range(dirs):
            for f in range(per_dir):
                write(root / f"d{d}" / f"sub{d}" / f"f{f}.bin", bytes([d]) * (100 + f))
        write(root / "top.bin", b"t" * 500)

    def _keys(self, recs):
        return {(r.path, r.size, r.dev, r.ino, r.nlink) for r in recs}

    @pytest.mark.parametrize("workers", [1, 2, 4, 8])
    def test_threaded_matches_serial(self, tmp_path, workers):
        self._tree(tmp_path)
        serial_stats, threaded_stats = ScanStats(), ScanStats()

        serial = dedup._walk_scandir([str(tmp_path)], [], 1, serial_stats)
        threaded = dedup._walk_bulk([str(tmp_path)], [], 1, threaded_stats,
                                    None, workers)

        assert self._keys(threaded) == self._keys(serial)
        assert len(threaded) == len(serial)          # no duplicated entries
        assert threaded_stats.files_seen == serial_stats.files_seen
        assert threaded_stats.bytes_seen == serial_stats.bytes_seen

    def test_no_files_lost_across_repeated_runs(self, tmp_path):
        """A racy exit condition shows up as an occasional short result."""
        self._tree(tmp_path)
        expected = len(dedup._walk_scandir([str(tmp_path)], [], 1, ScanStats()))

        counts = {len(dedup._walk_bulk([str(tmp_path)], [], 1, ScanStats(), None, 8))
                  for _ in range(15)}

        assert counts == {expected}

    def test_spinning_disk_stays_serial(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: False)
        assert dedup.walk_workers(["/somewhere"], None) == 1

    def test_unknown_device_stays_serial(self, monkeypatch):
        """USB enclosures often don't report it — don't gamble on a guess."""
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: None)
        assert dedup.walk_workers(["/somewhere"], None) == 1

    def test_ssd_goes_parallel(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: True)
        monkeypatch.setattr(dedup, "bulk_available", lambda: True)
        assert dedup.walk_workers(["/somewhere"], None) > 1

    def test_mixed_roots_stay_serial(self, monkeypatch):
        """One spinning root in the set is enough to keep it serial."""
        monkeypatch.setattr(dedup, "bulk_available", lambda: True)
        monkeypatch.setattr(dedup, "is_solid_state",
                            lambda p: p != "/spin")
        assert dedup.walk_workers(["/ssd", "/spin"], None) == 1

    def test_explicit_setting_overrides_detection(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: False)
        assert dedup.walk_workers(["/somewhere"], 6) == 6
        assert dedup.walk_workers(["/somewhere"], 0) == 1     # never zero

    def test_parallel_flag_defaults_to_auto(self):
        assert dedup.build_parser().parse_args([]).parallel is None
        assert dedup.build_parser().parse_args(["-p", "3"]).parallel == 3


class TestWalkRobustness:
    """Failures here must produce an error, never a hang."""

    def _with_timeout(self, fn, seconds=20):
        import threading
        box = {}
        t = threading.Thread(target=lambda: box.update(r=fn()), daemon=True)
        t.start()
        t.join(seconds)
        assert not t.is_alive(), "walk hung instead of returning"
        return box.get("r")

    def test_empty_roots_returns_instead_of_hanging(self):
        result = self._with_timeout(
            lambda: dedup._walk_bulk([], [], 1, ScanStats(), None, 4))

        assert result == []

    def test_reader_exception_does_not_hang(self, tmp_path, monkeypatch):
        write(tmp_path / "a.bin", b"x" * 100)
        monkeypatch.setattr(dedup.DirReader, "read",
                            lambda self, d: (_ for _ in ()).throw(ValueError("boom")))
        stats = ScanStats()

        result = self._with_timeout(
            lambda: dedup._walk_bulk([str(tmp_path)], [], 1, stats, None, 4))

        assert result == []
        assert stats.unreadable                    # reported, not swallowed


class TestOverlappingRoots:
    """The same file reachable twice must not be treated as two files."""

    def test_same_name_is_not_counted_twice(self):
        rec = FileRec(path="/x/a", size=10, mtime=0.0, dev=1, ino=1, nlink=1)
        dup = FileRec(path="/x/a", size=10, mtime=0.0, dev=1, ino=1, nlink=1)

        merged = drop_hardlinks([rec, dup])

        assert len(merged) == 1
        assert merged[0].all_names == ["/x/a"]

    def test_duplicate_root_does_not_invent_duplicates(self, tmp_path):
        write(tmp_path / "only.bin", b"u" * 4000)

        groups = find_duplicates([str(tmp_path), str(tmp_path)], excludes=[],
                                 min_size=1, jobs=2, verify=False,
                                 stats=ScanStats(), reporter=None)

        assert groups == []

    def test_duplicate_root_keeps_one_copy_and_reports_truthfully(self, tmp_path):
        write(tmp_path / "a.bin", b"A" * 4000)
        write(tmp_path / "sub" / "copy.bin", b"A" * 4000)
        groups = find_duplicates([str(tmp_path), str(tmp_path)], excludes=[],
                                 min_size=1, jobs=2, verify=False,
                                 stats=ScanStats(), reporter=None)
        assert len(groups) == 1
        victim = [f for f in groups[0].files if f.path.endswith("copy.bin")][0]

        removed = []
        ok, err = dedup.remove_record(
            victim, lambda p: (removed.append(p), (True, ""))[1])

        assert ok and err == ""
        assert removed == [victim.path]             # exactly once

    def test_symlinked_root_collapses_to_one_name(self, tmp_path):
        """Two spellings of one directory entry must not be deleted twice."""
        real = tmp_path / "real"
        real.mkdir()
        write(real / "a.bin", b"A" * 100)
        (tmp_path / "link").symlink_to(real)
        st = os.stat(real / "a.bin")
        recs = [FileRec(str(real / "a.bin"), 100, 0.0, st.st_dev, st.st_ino, 1),
                FileRec(str(tmp_path / "link" / "a.bin"), 100, 0.0,
                        st.st_dev, st.st_ino, 1)]

        merged = drop_hardlinks(recs)

        assert len(merged) == 1
        assert len(merged[0].all_names) == 1

    def test_real_hardlinks_are_still_kept_separate(self, tmp_path):
        """Hardlinks are distinct entries and all of them must be removed."""
        first = write(tmp_path / "one.bin", b"H" * 100)
        os.link(first, tmp_path / "two.bin")
        a, b = os.stat(first), os.stat(tmp_path / "two.bin")
        recs = [FileRec(str(first), 100, 0.0, a.st_dev, a.st_ino, 2),
                FileRec(str(tmp_path / "two.bin"), 100, 0.0, b.st_dev, b.st_ino, 2)]

        merged = drop_hardlinks(recs)

        assert len(merged) == 1
        assert len(merged[0].all_names) == 2


class TestNoDuplicatesScreen:
    """An empty result after a long scan must not look like a crash."""

    def test_interactive_screen_explains_itself(self, capsys):
        stats = ScanStats(files_seen=1200, bytes_seen=5 * 1024**3)

        dedup.print_no_duplicates(stats, 10 * 1024 * 1024, plain=False)

        out = capsys.readouterr().out
        assert "No duplicates found" in out
        assert "1,200" in out                  # what was actually checked
        assert "--min-size 1" in out           # why it might be empty

    def test_min_size_hint_only_when_a_floor_is_set(self, capsys):
        stats = ScanStats(files_seen=10, bytes_seen=100)

        dedup.print_no_duplicates(stats, 1, plain=False)

        out = capsys.readouterr().out
        assert "No duplicates found" in out
        assert "--min-size" not in out

    def test_plain_mode_stays_terse_for_scripts(self, capsys):
        stats = ScanStats(files_seen=10, bytes_seen=100)

        dedup.print_no_duplicates(stats, 10 * 1024 * 1024, plain=True)

        out = capsys.readouterr().out
        assert out.strip() == "No duplicates found."


class TestSampledHashing:
    """Sampling may never lose a real duplicate — that would be data loss
    in reverse: a copy the user keeps paying for and never sees."""

    def _big(self, path, filler, tail=b"", mid=None):
        size = dedup.SAMPLE_SPAN * 2
        body = bytearray(filler * size)
        if mid is not None:
            body[size // 2:size // 2 + len(mid)] = mid
        if tail:
            body[-len(tail):] = tail
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes(body))
        return path

    def test_identical_large_files_are_still_found(self, tmp_path):
        self._big(tmp_path / "a.bin", b"A")
        self._big(tmp_path / "b.bin", b"A")

        groups = scan(tmp_path)

        assert len(groups) == 1
        assert len(groups[0].files) == 2

    def test_difference_outside_the_samples_is_still_caught(self, tmp_path):
        """The samples can agree while the files differ. Stage 5 reads the
        bytes, so this must NOT report a duplicate."""
        self._big(tmp_path / "a.bin", b"A", tail=b"")
        b = self._big(tmp_path / "b.bin", b"A")
        data = bytearray(b.read_bytes())
        data[dedup.SAMPLE_BYTES + 7] ^= 0xFF          # between sample points
        b.write_bytes(bytes(data))

        assert scan(tmp_path) == []

    def test_sample_reads_far_less_than_the_file(self, tmp_path):
        f = self._big(tmp_path / "a.bin", b"A")

        assert dedup.sampled_bytes(f.stat().st_size) < f.stat().st_size // 3

    def test_small_files_are_read_whole(self, tmp_path):
        f = tmp_path / "small.bin"
        f.write_bytes(b"x" * 1000)

        assert dedup.hash_sample(str(f), 1000) == dedup.hash_full(str(f), 1000)
        assert dedup.sampled_bytes(1000) == 1000

    def test_same_samples_different_size_do_not_collide(self, tmp_path):
        a = self._big(tmp_path / "a.bin", b"A")
        b = self._big(tmp_path / "b.bin", b"A")
        b.write_bytes(b.read_bytes() + b"extra")

        da = dedup.hash_sample(str(a), a.stat().st_size)
        db = dedup.hash_sample(str(b), b.stat().st_size)
        assert da != db


class TestByteComparison:
    """Stage 5 reads the files in step and splits them where they differ.

    It is the stage that decides what gets deleted, so it gets the hardest
    cases: groups that split more than one way, differences one byte from
    the end, and files that only match for a while.
    """

    HEAD = b"H" * (dedup.HEAD_BYTES + 1024)     # shared, so they meet in stage 3

    def _same_head(self, path, tail: bytes, length=200 * 1024):
        body = bytearray(self.HEAD + tail)
        body.extend(b"\0" * (length - len(body)))
        return write(path, bytes(body[:length]))

    def _rec(self, path):
        st = os.stat(path)
        return FileRec(str(path), st.st_size, st.st_mtime, st.st_dev, st.st_ino)

    def test_one_bucket_splits_into_two_groups(self, tmp_path):
        """Same size, same head — one bucket arrives at stage 5 holding two
        different sets of duplicates. Both have to come out."""
        for name in ("a", "b", "c"):
            self._same_head(tmp_path / f"{name}.bin", b"first")
        for name in ("d", "e"):
            self._same_head(tmp_path / f"{name}.bin", b"second")

        groups = scan(tmp_path)

        assert sorted(len(g.files) for g in groups) == [2, 3]

    def test_a_bucket_of_all_different_files_yields_nothing(self, tmp_path):
        for i, name in enumerate("abcd"):
            self._same_head(tmp_path / f"{name}.bin", bytes([i]) * 8)

        assert scan(tmp_path) == []

    def test_difference_in_the_final_byte_is_caught(self, tmp_path):
        a = self._same_head(tmp_path / "a.bin", b"same")
        b = self._same_head(tmp_path / "b.bin", b"same")
        data = bytearray(b.read_bytes())
        data[-1] ^= 0xFF
        b.write_bytes(bytes(data))

        assert scan(tmp_path) == []

    def test_files_matching_across_many_chunks_are_found(self, tmp_path):
        """Drive the multi-round loop without writing a 16 MB file: a tiny
        chunk makes a 200 KB file take dozens of passes."""
        a = self._same_head(tmp_path / "a.bin", b"same")
        b = self._same_head(tmp_path / "b.bin", b"same")
        recs = [self._rec(a), self._rec(b)]

        matched = dedup.split_identical(recs, 4096, ScanStats())

        assert len(matched) == 1 and len(matched[0]) == 2

    def test_divergence_stops_the_read_early(self, tmp_path):
        """The point of comparing rather than hashing: files that turn out to
        differ are not read to the end."""
        a = self._same_head(tmp_path / "a.bin", b"one")
        b = self._same_head(tmp_path / "b.bin", b"two")
        stats = ScanStats()

        dedup.split_identical([self._rec(a), self._rec(b)], 4096, stats)

        assert stats.hashed_bytes < a.stat().st_size

    def test_an_unreadable_member_does_not_break_the_group(self, tmp_path):
        """Opening happens inside the comparison, so a file that can't be read
        has to drop out without taking its twins with it."""
        if os.geteuid() == 0:
            pytest.skip("root can read anything")
        recs = [self._rec(self._same_head(tmp_path / f"{n}.bin", b"same"))
                for n in ("a", "b", "c")]
        locked = tmp_path / "c.bin"
        locked.chmod(0o000)
        try:
            stats = ScanStats()
            matched = dedup.split_identical(recs, 4096, stats)
        finally:
            locked.chmod(0o644)

        assert [len(m) for m in matched] == [2]
        assert str(locked) in stats.unreadable

    def test_same_head_different_length_never_meet(self, tmp_path):
        """A head digest covers 64 KiB, not the file. Size is part of the
        bucket key so two different-length files are never compared."""
        a = self._same_head(tmp_path / "a.bin", b"same", length=200 * 1024)
        b = self._same_head(tmp_path / "b.bin", b"same", length=300 * 1024)
        stats = ScanStats()

        buckets = dedup.hash_stage([self._rec(a), self._rec(b)],
                                   dedup.hash_head, 2, stats)

        assert all(len(v) == 1 for v in buckets.values())

    def test_every_file_is_accounted_for_so_progress_reaches_the_end(self, tmp_path):
        """The bar counts files settled against files entering the stage. A
        scan that stalls at 94% forever reads as a hang — it did once."""
        for name in ("a", "b", "c"):
            self._same_head(tmp_path / f"{name}.bin", b"first")
        self._same_head(tmp_path / "d.bin", b"alone")
        recs = [self._rec(tmp_path / f"{n}.bin") for n in ("a", "b", "c", "d")]
        stats = ScanStats()

        dedup.split_identical(recs, 4096, stats)

        assert stats.hashed_files == len(recs)

    def test_progress_ticks_during_a_long_comparison(self, tmp_path):
        """One group of two enormous files is many rounds and one group. The
        tick has to come from inside the loop, not after it."""
        recs = [self._rec(self._same_head(tmp_path / f"{n}.bin", b"same"))
                for n in ("a", "b")]
        ticks = []

        dedup.split_identical(recs, 4096, ScanStats(), tick=lambda: ticks.append(1))

        assert len(ticks) > 5

    def test_verify_flag_is_accepted_and_changes_nothing(self, tmp_path):
        write(tmp_path / "a.txt", b"hello world")
        write(tmp_path / "b.txt", b"hello world")

        assert scan(tmp_path, verify=True) == scan(tmp_path, verify=False)


class TestComparisonPrimitives:
    def test_partition_splits_three_ways(self):
        blocks = [("a", b"1"), ("b", b"2"), ("c", b"1"), ("d", b"3"), ("e", b"2")]

        runs = dedup.partition_blocks(blocks)

        assert sorted(sorted(r for r, _ in run) for run in runs) == \
            [["a", "c"], ["b", "e"], ["d"]]

    def test_partition_of_nothing_is_nothing(self):
        assert dedup.partition_blocks([]) == []

    def test_read_block_keeps_reading_after_a_short_read(self):
        class Dribble:
            """read(2) may return less than asked for. If that split two
            identical files differently they would look unequal."""
            def __init__(self, data):
                self.data, self.pos = data, 0

            def read(self, n):
                out = self.data[self.pos:self.pos + min(n, 3)]
                self.pos += len(out)
                return out

        assert dedup.read_block(Dribble(b"abcdefghij"), 8) == b"abcdefgh"

    def test_read_block_returns_what_is_left_at_the_end(self):
        import io
        assert dedup.read_block(io.BytesIO(b"abc"), 100) == b"abc"
        assert dedup.read_block(io.BytesIO(b""), 100) == b""

    def test_chunk_is_smaller_on_flash_than_on_a_spinning_disk(self):
        assert dedup.compare_chunk(2, True) < dedup.compare_chunk(2, None)

    def test_a_wide_group_stays_inside_the_memory_budget(self):
        """A round holds one chunk per member. A thousand copies of one file
        is unusual but not impossible, and must not mean a gigabyte of RAM."""
        for members in (2, 8, 64, 512, 1024):
            for solid in (True, False, None):
                held = dedup.compare_chunk(members, solid) * members
                assert held <= dedup.COMPARE_BUDGET, (members, solid, held)

    def test_chunks_never_shrink_to_a_pointless_read(self):
        assert dedup.compare_chunk(100000, None) == dedup.MIN_CHUNK

    def test_unknown_device_is_not_treated_as_flash(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: None)
        assert dedup.all_solid_state(["/x"]) is False
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: True)
        assert dedup.all_solid_state(["/x"]) is True
        assert dedup.all_solid_state([]) is False


class TestDeviceAwareJobs:
    def test_spinning_disk_limits_concurrent_readers(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: False)
        assert dedup.read_jobs(["/x"], None) == 2

    def test_unknown_device_limits_concurrent_readers(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: None)
        assert dedup.read_jobs(["/x"], None) == 2

    def test_ssd_gets_queue_depth(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: True)
        assert dedup.read_jobs(["/x"], None) > 2

    def test_explicit_jobs_wins(self, monkeypatch):
        monkeypatch.setattr(dedup, "is_solid_state", lambda p: True)
        assert dedup.read_jobs(["/x"], 3) == 3
        assert dedup.read_jobs(["/x"], 0) == 1
