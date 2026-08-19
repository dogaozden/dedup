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
