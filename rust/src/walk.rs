//! Stage 1 and stage 2: collect every regular file, then keep only the sizes
//! more than one file shares.
//!
//! Symlinks are never followed and never counted. `files_seen`/`bytes_seen` are
//! counted after the name filters and before the size filter, which is the
//! order `dedup.py`'s walkers use and the order the protocol asks for.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::filters::{excluded, extension_allowed, Filters};
use crate::sys::{BulkDir, VDIR, VLNK, VREG};

/// One file. A hardlinked inode is one record wearing several names, because
/// deleting fewer than all of them frees nothing.
pub struct FileRec {
    /// Every name found for this inode, shortest first, ties lexicographic.
    /// The first is the canonical name the protocol reports and the keeper
    /// `--auto shortest` picks.
    pub names: Vec<String>,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    /// How many names the inode has on disk, which is not always how many the
    /// scan can see.
    pub nlink: u64,
    pub mtime_ns: i64,
    /// Bytes stage 3 read from each end, kept so stage 5 can compare them
    /// instead of reading them a second time.
    ///
    /// Boxed, and absent by default, because most records never carry any: a
    /// tree of small files settles before stage 3 ever runs, and two empty
    /// vectors cost 48 bytes on every record to say nothing.
    pub retained: Option<Box<Retained>>,
}

/// One contiguous run from offset 0 and one that ends at EOF. Stages read each
/// end strictly outward, so that is the only shape either can take.
#[derive(Default)]
pub struct Retained {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
}

impl FileRec {
    pub fn path(&self) -> &str {
        &self.names[0]
    }

    pub fn layout(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    /// Name a read of this file at `offset`, for the seek count.
    pub fn at(&self, offset: u64) -> crate::effort::At {
        crate::effort::At {
            dev: self.dev,
            ino: self.ino,
            offset,
        }
    }

    pub fn head(&self) -> &[u8] {
        self.retained.as_ref().map_or(&[], |kept| &kept.head)
    }

    pub fn tail(&self) -> &[u8] {
        self.retained.as_ref().map_or(&[], |kept| &kept.tail)
    }

    /// Keep what the end probe read. Only ever called on a record carrying
    /// nothing, which is what makes the run contiguous from offset 0.
    pub fn keep_ends(&mut self, head: Vec<u8>, tail: Vec<u8>) {
        self.retained = Some(Box::new(Retained { head, tail }));
    }

    /// Continue the head with the block behind it.
    pub fn extend_head(&mut self, block: &[u8]) {
        if let Some(kept) = self.retained.as_mut() {
            kept.head.extend_from_slice(block);
        }
    }

    /// Drop kept bytes no later stage will look at again.
    pub fn release(&mut self) {
        self.retained = None;
    }

    /// True when removing our copies actually frees the bytes.
    ///
    /// A hardlinked inode only releases its storage once every name is gone.
    /// If the scan cannot see all of them, some name outside the tree is still
    /// holding the data down and deleting what we can see frees nothing.
    pub fn deletable(&self) -> bool {
        self.names.len() as u64 >= self.nlink
    }
}

/// Shortest, then lexicographic — by character count, because the oracle
/// measures a Python string in code points and the trees carry emoji. Rust's
/// byte order over UTF-8 agrees with Python's code-point order, so only the
/// length needs saying differently.
pub fn name_order(name: &str) -> (usize, &str) {
    (name.chars().count(), name)
}

pub fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Collect every regular file under `root`, one record per directory entry.
///
/// Hardlinks are folded together afterwards by `candidates`, in place. The walk
/// builds `FileRec`s rather than a lighter intermediate on purpose: an
/// intermediate would have to stay alive while the records were built beside
/// it, and holding two per-file structures at once is what put the peak over
/// REWRITE.md §6's budget.
pub fn walk(
    root: &Path,
    filters: Filters,
    files_seen: &mut u64,
    bytes_seen: &mut u64,
    unreadable: &mut Vec<String>,
) -> Vec<FileRec> {
    let mut survivors = Vec::new();
    // Each directory carries the relative path it was reached by, so a child's
    // relative name is one push onto its parent's rather than a fresh strip of
    // the whole path.
    let mut directories = vec![(root.to_path_buf(), String::new())];
    let mut bulk = BulkDir::default();

    while let Some((directory, prefix)) = directories.pop() {
        // One call for the whole directory, names and attributes together.
        // Anything the kernel will not describe completely comes back `false`
        // with nothing reported, and that directory alone takes the portable
        // path below.
        if bulk.read(&directory) {
            gather_bulk(
                &bulk,
                &directory,
                &prefix,
                filters,
                files_seen,
                bytes_seen,
                &mut survivors,
                &mut directories,
            );
            continue;
        }
        gather_portable(
            root,
            &directory,
            filters,
            files_seen,
            bytes_seen,
            unreadable,
            &mut survivors,
            &mut directories,
        );
    }

    survivors
}

/// A directory the kernel described in bulk.
#[allow(clippy::too_many_arguments)]
fn gather_bulk(
    bulk: &BulkDir,
    directory: &Path,
    prefix: &str,
    filters: Filters,
    files_seen: &mut u64,
    bytes_seen: &mut u64,
    survivors: &mut Vec<FileRec>,
    directories: &mut Vec<(PathBuf, String)>,
) {
    for (name, entry) in bulk.entries() {
        let relative = child_relative(prefix, name);
        // Tested before the entry is looked at any further, so an excluded
        // directory is never entered and nothing inside it can be tripped
        // over on the way in.
        if excluded(&relative, &filters) {
            continue;
        }
        if entry.objtype == VLNK {
            continue;
        }
        if entry.objtype == VDIR {
            // Joined from the raw name rather than the relative one: only the
            // relative spelling folds a backslash onto a slash, and the path
            // we open has to stay the name the kernel gave us.
            directories.push((directory.join(OsStr::from_bytes(name)), relative));
            continue;
        }
        if entry.objtype != VREG || !extension_allowed(&relative, &filters) {
            continue;
        }

        *files_seen += 1;
        *bytes_seen += entry.size;
        if entry.size < filters.min_size {
            continue;
        }
        survivors.push(FileRec {
            names: vec![relative],
            size: entry.size,
            dev: entry.dev,
            ino: entry.ino,
            nlink: entry.nlink,
            mtime_ns: entry.mtime_ns,
            retained: None,
        });
    }
}

/// `prefix/name`, spelled the way `relative_path` spells it.
fn child_relative(prefix: &str, name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    let mut relative = String::with_capacity(prefix.len() + 1 + name.len());
    if !prefix.is_empty() {
        relative.push_str(prefix);
        relative.push('/');
    }
    for character in name.chars() {
        relative.push(if character == '\\' { '/' } else { character });
    }
    relative
}

/// The portable walk, for a directory `getattrlistbulk` would not describe.
#[allow(clippy::too_many_arguments)]
fn gather_portable(
    root: &Path,
    directory: &Path,
    filters: Filters,
    files_seen: &mut u64,
    bytes_seen: &mut u64,
    unreadable: &mut Vec<String>,
    survivors: &mut Vec<FileRec>,
    directories: &mut Vec<(PathBuf, String)>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(_) => {
            unreadable.push(relative_path(root, directory));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unreadable.push(relative_path(root, directory));
                continue;
            }
        };
        let path = entry.path();
        let relative = relative_path(root, &path);
        // Tested before the entry is looked at any further, so an excluded
        // directory is never entered and nothing inside it can be tripped
        // over on the way in.
        if excluded(&relative, &filters) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                unreadable.push(relative);
                continue;
            }
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            directories.push((path, relative));
            continue;
        }
        if !file_type.is_file() || !extension_allowed(&relative, &filters) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                unreadable.push(relative);
                continue;
            }
        };

        *files_seen += 1;
        *bytes_seen += metadata.len();
        if metadata.len() < filters.min_size {
            continue;
        }
        survivors.push(FileRec {
            names: vec![relative],
            size: metadata.len(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            nlink: metadata.nlink(),
            mtime_ns: metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
            retained: None,
        });
    }
}

/// Fold the names of one inode, whose records sit at `[..]`, onto the first.
///
/// Several names for one inode is normally a hardlink, and those are separate
/// directory entries that all have to go together. But overlapping roots or a
/// symlinked root can hand us two spellings of a single entry, and deleting it
/// twice would report a failure for work already done — so only multi-name
/// records pay for resolving that.
fn fold_names(root: &Path, first: &mut FileRec, rest: &mut [FileRec]) {
    for other in rest.iter_mut() {
        let name = std::mem::take(&mut other.names[0]);
        if !first.names.contains(&name) {
            first.names.push(name);
        }
    }
    if first.names.len() > 1 {
        let mut seen: Vec<(PathBuf, String)> = Vec::new();
        for name in first.names.drain(..) {
            let real = fs::canonicalize(root.join(&name)).unwrap_or_else(|_| root.join(&name));
            if !seen.iter().any(|(known, _)| known == &real) {
                seen.push((real, name));
            }
        }
        first.names = seen.into_iter().map(|(_, name)| name).collect();
    }
    first
        .names
        .sort_by(|left, right| name_order(left).cmp(&name_order(right)));
}

/// Stage 2: collapse hardlinks, then discard every size only one record holds.
///
/// One sort by (size, device, inode) does all the grouping this stage needs —
/// a size's entries land in one run and an inode's names land adjacent inside
/// it — and the compaction then happens in place, so no second per-file
/// structure is ever live beside the first.
///
/// The records come back in the volume's layout order rather than the grouping
/// order, so a later stage is one sweep across the disk instead of a jump per
/// file.
pub fn candidates(root: &Path, mut records: Vec<FileRec>) -> Vec<FileRec> {
    records.sort_unstable_by_key(|record| (record.size, record.dev, record.ino));

    // `write` trails `read`: everything before it is a kept record, everything
    // between them is a husk whose name has already been moved onto its
    // cluster's first record. Nothing is dropped until the truncate, so no
    // element is ever duplicated.
    let mut write = 0;
    let mut read = 0;
    while read < records.len() {
        let size = records[read].size;
        let run_start = write;
        while read < records.len() && records[read].size == size {
            let inode = (records[read].dev, records[read].ino);
            records.swap(write, read);
            let mut last = read + 1;
            while last < records.len() && (records[last].dev, records[last].ino) == inode {
                last += 1;
            }
            // `rest` starts at absolute `write + 1`; the cluster's other
            // names start at absolute `read + 1`.
            let (kept, rest) = records[write..last].split_at_mut(1);
            fold_names(root, &mut kept[0], &mut rest[read - write..]);
            read = last;
            write += 1;
        }
        // A size only one record holds is nobody's duplicate, and a hardlink
        // cluster matching nothing else is one record wearing several names.
        if write - run_start < 2 {
            write = run_start;
        }
    }
    records.truncate(write);
    records.sort_unstable_by_key(FileRec::layout);
    records
}

/// Whether every file is still the one whose bytes we kept. Kept bytes only
/// prove anything while the file on disk has not moved under us, and this tool
/// deletes what it proves.
pub fn unchanged(root: &Path, members: &[&FileRec]) -> bool {
    members.iter().all(|record| {
        matches!(fs::metadata(root.join(record.path())), Ok(metadata)
            if metadata.len() == record.size
                && metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec() == record.mtime_ns)
    })
}

/// Whether an open file is still the one the record describes.
pub fn still_the_same(file: &fs::File, record: &FileRec) -> io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(metadata.len() == record.size
        && metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec() == record.mtime_ns)
}

pub fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(relative: &str, size: u64, ino: u64) -> FileRec {
        FileRec {
            names: vec![relative.to_string()],
            size,
            dev: 1,
            ino,
            nlink: 1,
            mtime_ns: 0,
            retained: None,
        }
    }

    fn names(records: &[FileRec]) -> Vec<&str> {
        records.iter().map(FileRec::path).collect()
    }

    #[test]
    fn unique_sizes_and_lone_inodes_are_not_candidates() {
        let root = Path::new("/nowhere");
        let records = candidates(root, vec![entry("one.bin", 3, 1), entry("two.bin", 4, 2)]);
        assert!(records.is_empty());
    }

    #[test]
    fn a_hardlink_cluster_alone_at_its_size_is_not_a_candidate() {
        let root = Path::new("/nowhere");
        let records = candidates(root, vec![entry("a.bin", 5, 3), entry("b.bin", 5, 3)]);
        assert!(records.is_empty());
    }

    #[test]
    fn hardlink_names_collapse_onto_one_record_shortest_first() {
        let root = Path::new("/nowhere");
        let records = candidates(
            root,
            vec![
                entry("links/b.bin", 2, 9),
                entry("z.bin", 2, 10),
                entry("links/a.bin", 2, 9),
            ],
        );
        let mut listed = names(&records);
        listed.sort_unstable();
        assert_eq!(listed, ["links/a.bin", "z.bin"]);
        let cluster = records
            .iter()
            .find(|record| record.ino == 9)
            .expect("the cluster survives");
        assert_eq!(cluster.names, ["links/a.bin", "links/b.bin"]);
    }

    #[test]
    fn a_discarded_size_run_does_not_disturb_the_runs_around_it() {
        // The compaction writes over the husks it leaves behind, so a run that
        // turns out to be a single record has to be reclaimed without
        // corrupting the run that follows it.
        let root = Path::new("/nowhere");
        let records = candidates(
            root,
            vec![
                entry("lone-a.bin", 1, 1), // size 1: one record, discarded
                entry("pair-a.bin", 2, 2), // size 2: a real pair
                entry("pair-b.bin", 2, 3),
                entry("lone-b.bin", 3, 4), // size 3: one record, discarded
                entry("trio-a.bin", 4, 5), // size 4: a real trio
                entry("trio-b.bin", 4, 6),
                entry("trio-c.bin", 4, 7),
            ],
        );
        let mut got: Vec<&str> = records.iter().map(FileRec::path).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "pair-a.bin",
                "pair-b.bin",
                "trio-a.bin",
                "trio-b.bin",
                "trio-c.bin"
            ]
        );
        assert!(records.iter().all(|record| record.names.len() == 1));
    }

    #[test]
    fn a_cluster_beside_a_discarded_run_keeps_all_its_names() {
        let root = Path::new("/nowhere");
        let records = candidates(
            root,
            vec![
                entry("lone.bin", 1, 1),
                entry("c/x.bin", 2, 2),
                entry("c/y.bin", 2, 2),
                entry("c/z.bin", 2, 2),
                entry("twin.bin", 2, 3),
            ],
        );
        assert_eq!(records.len(), 2);
        let cluster = records
            .iter()
            .find(|record| record.ino == 2)
            .expect("the cluster survives");
        assert_eq!(cluster.names, ["c/x.bin", "c/y.bin", "c/z.bin"]);
        assert!(records.iter().any(|record| record.path() == "twin.bin"));
    }

    #[test]
    fn a_record_fits_the_per_file_memory_budget() {
        // REWRITE.md §6 allows 230 bytes per file record, §4 says 180-230 is
        // the shape to aim for. A record is the struct, the vector holding its
        // names, and the name bytes themselves.
        let name = "photos/2021/holiday/IMG_20210714_183012.jpg";
        let record = FileRec {
            names: vec![name.to_string()],
            size: 1,
            dev: 1,
            ino: 1,
            nlink: 1,
            mtime_ns: 0,
            retained: None,
        };
        let footprint = std::mem::size_of::<FileRec>()
            + record.names.capacity() * std::mem::size_of::<String>()
            + record.names.iter().map(String::capacity).sum::<usize>();
        // Retained bytes are not part of this: they exist only for records
        // that reach stage 3, are bounded by RETAIN_BUDGET rather than by the
        // file count, and are dropped by `release` as soon as a stage is done
        // with them.
        assert!(
            footprint <= 230,
            "{footprint} bytes per record, over the 230-byte budget"
        );
    }

    #[test]
    fn names_sort_by_code_points_rather_than_utf8_length() {
        // "🙂.bin" is 4 characters and 8 bytes; a byte-length sort would put
        // the longer name first and pick the wrong canonical name.
        let mut names = vec!["🙂.bin".to_string(), "abcde.bin".to_string()];
        names.sort_by(|left, right| name_order(left).cmp(&name_order(right)));
        assert_eq!(names, ["🙂.bin", "abcde.bin"]);
    }
}
