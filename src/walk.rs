//! Stage 1 and stage 2: collect every regular file, then keep only the sizes
//! more than one file shares.
//!
//! Symlinks are never followed and never counted. `files_seen`/`bytes_seen` are
//! counted after the name filters and before the size filter, which is the
//! order `dedup.py`'s walkers use and the order the protocol asks for.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::filters::{excluded, extension_allowed, Filters};

/// One directory entry the walk accepted, before hardlinks are collapsed.
pub struct Entry {
    pub relative: String,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub mtime_ns: i64,
}

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
    /// Bytes stage 3 read from offset 0, kept so stage 5 can compare them
    /// instead of reading them a second time.
    pub head: Vec<u8>,
    /// ...and the same for the page at the far end.
    pub tail: Vec<u8>,
}

impl FileRec {
    pub fn path(&self) -> &str {
        &self.names[0]
    }

    pub fn layout(&self) -> (u64, u64) {
        (self.dev, self.ino)
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

pub fn walk(
    root: &Path,
    filters: Filters,
    files_seen: &mut u64,
    bytes_seen: &mut u64,
    unreadable: &mut Vec<String>,
) -> Vec<Entry> {
    let mut survivors = Vec::new();
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                unreadable.push(relative_path(root, &directory));
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    unreadable.push(relative_path(root, &directory));
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
                directories.push(path);
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
            survivors.push(Entry {
                relative,
                size: metadata.len(),
                dev: metadata.dev(),
                ino: metadata.ino(),
                nlink: metadata.nlink(),
                mtime_ns: metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
            });
        }
    }

    survivors
}

/// Fold one inode's directory entries into a single record.
///
/// Several names for one inode is normally a hardlink, and those are separate
/// directory entries that all have to go together. But overlapping roots or a
/// symlinked root can hand us two spellings of a single entry, and deleting it
/// twice would report a failure for work already done — so only multi-name
/// records pay for resolving that.
fn fold_inode(root: &Path, entries: &mut [Entry]) -> FileRec {
    let first = &entries[0];
    let mut record = FileRec {
        names: Vec::with_capacity(entries.len()),
        size: first.size,
        dev: first.dev,
        ino: first.ino,
        nlink: first.nlink,
        mtime_ns: first.mtime_ns,
        head: Vec::new(),
        tail: Vec::new(),
    };
    for entry in entries.iter_mut() {
        if !record.names.contains(&entry.relative) {
            record.names.push(std::mem::take(&mut entry.relative));
        }
    }
    if record.names.len() > 1 {
        let mut seen: Vec<(PathBuf, String)> = Vec::new();
        for name in record.names.drain(..) {
            let real = fs::canonicalize(root.join(&name)).unwrap_or_else(|_| root.join(&name));
            if !seen.iter().any(|(known, _)| known == &real) {
                seen.push((real, name));
            }
        }
        record.names = seen.into_iter().map(|(_, name)| name).collect();
    }
    record
        .names
        .sort_by(|left, right| name_order(left).cmp(&name_order(right)));
    record
}

/// Stage 2: collapse hardlinks, then discard every size only one record holds.
///
/// One sort by (size, device, inode) does all the grouping this stage needs —
/// a size's entries land in one run and an inode's names land adjacent inside
/// it — which keeps the whole stage to the entry list and the records it
/// produces. Grouping through hash maps instead cost about three times the
/// peak, against REWRITE.md §6's per-record budget.
///
/// The records come back in the volume's layout order rather than the grouping
/// order, so a later stage is one sweep across the disk instead of a jump per
/// file.
pub fn candidates(root: &Path, mut entries: Vec<Entry>) -> Vec<FileRec> {
    entries.sort_unstable_by_key(|entry| (entry.size, entry.dev, entry.ino));

    let mut out: Vec<FileRec> = Vec::new();
    let mut run_start = 0;
    while run_start < entries.len() {
        let size = entries[run_start].size;
        let mut run_end = run_start;
        while run_end < entries.len() && entries[run_end].size == size {
            run_end += 1;
        }

        let first_record = out.len();
        let mut name_start = run_start;
        while name_start < run_end {
            let inode = (entries[name_start].dev, entries[name_start].ino);
            let mut name_end = name_start;
            while name_end < run_end && (entries[name_end].dev, entries[name_end].ino) == inode {
                name_end += 1;
            }
            out.push(fold_inode(root, &mut entries[name_start..name_end]));
            name_start = name_end;
        }
        // A size only one record holds is nobody's duplicate, and a hardlink
        // cluster matching nothing else is one record wearing several names.
        if out.len() - first_record < 2 {
            out.truncate(first_record);
        }
        run_start = run_end;
    }

    out.sort_unstable_by_key(FileRec::layout);
    out
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

    fn entry(relative: &str, size: u64, ino: u64) -> Entry {
        Entry {
            relative: relative.to_string(),
            size,
            dev: 1,
            ino,
            nlink: 1,
            mtime_ns: 0,
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
            head: Vec::new(),
            tail: Vec::new(),
        };
        let footprint = std::mem::size_of::<FileRec>()
            + record.names.capacity() * std::mem::size_of::<String>()
            + record.names.iter().map(String::capacity).sum::<usize>();
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
