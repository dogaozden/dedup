//! Stage 1 and stage 2: collect every regular file, then keep only the sizes
//! more than one file shares.
//!
//! Symlinks are never followed and never counted. `files_seen`/`bytes_seen` are
//! counted after the name filters and before the size filter, which is the
//! order `dedup.py`'s walkers use and the order the protocol asks for.

use std::collections::HashMap;
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
    pub mtime_ns: i128,
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
    pub mtime_ns: i128,
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
                mtime_ns: i128::from(metadata.mtime()) * 1_000_000_000
                    + i128::from(metadata.mtime_nsec()),
            });
        }
    }

    survivors
}

/// Collapse each (device, inode) to a single record listing all its names.
fn drop_hardlinks(root: &Path, entries: Vec<Entry>) -> Vec<FileRec> {
    let mut order: Vec<(u64, u64)> = Vec::new();
    let mut by_inode: HashMap<(u64, u64), FileRec> = HashMap::new();
    for entry in entries {
        let key = (entry.dev, entry.ino);
        let record = by_inode.entry(key).or_insert_with(|| {
            order.push(key);
            FileRec {
                names: Vec::new(),
                size: entry.size,
                dev: entry.dev,
                ino: entry.ino,
                mtime_ns: entry.mtime_ns,
                head: Vec::new(),
                tail: Vec::new(),
            }
        });
        // Overlapping roots hand us the same name twice; keeping both would
        // overstate the link count and try to delete one file two times.
        if !record.names.contains(&entry.relative) {
            record.names.push(entry.relative);
        }
    }

    for record in by_inode.values_mut() {
        // Several names for one inode is normally a hardlink, and those are
        // separate directory entries that all have to go. But a symlinked root
        // can hand us two spellings of one entry. Only multi-name records pay
        // for resolving that.
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
    }

    order
        .into_iter()
        .filter_map(|key| by_inode.remove(&key))
        .collect()
}

/// Stage 2: hardlink collapse, then discard every size only one record holds.
/// Read order follows the volume's layout rather than the grouping, so a stage
/// is one sweep across the disk instead of a jump per file.
pub fn candidates(root: &Path, entries: Vec<Entry>) -> Vec<FileRec> {
    let mut by_size: HashMap<u64, Vec<Entry>> = HashMap::new();
    let mut sizes: Vec<u64> = Vec::new();
    for entry in entries {
        by_size.entry(entry.size).or_insert_with(|| {
            sizes.push(entry.size);
            Vec::new()
        });
        let size = entry.size;
        by_size.get_mut(&size).expect("just inserted").push(entry);
    }

    let mut out = Vec::new();
    for size in sizes {
        let bucket = by_size.remove(&size).expect("size was recorded");
        let mut records = drop_hardlinks(root, bucket);
        if records.len() > 1 {
            out.append(&mut records);
        }
    }
    out.sort_by_key(FileRec::layout);
    out
}

/// Whether every file is still the one whose bytes we kept. Kept bytes only
/// prove anything while the file on disk has not moved under us, and this tool
/// deletes what it proves.
pub fn unchanged(root: &Path, members: &[&FileRec]) -> bool {
    members.iter().all(|record| {
        matches!(fs::metadata(root.join(record.path())), Ok(metadata)
            if metadata.len() == record.size
                && i128::from(metadata.mtime()) * 1_000_000_000
                    + i128::from(metadata.mtime_nsec())
                    == record.mtime_ns)
    })
}

/// Whether an open file is still the one the record describes.
pub fn still_the_same(file: &fs::File, record: &FileRec) -> io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(metadata.len() == record.size
        && i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
            == record.mtime_ns)
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
    fn names_sort_by_code_points_rather_than_utf8_length() {
        // "🙂.bin" is 4 characters and 8 bytes; a byte-length sort would put
        // the longer name first and pick the wrong canonical name.
        let mut names = vec!["🙂.bin".to_string(), "abcde.bin".to_string()];
        names.sort_by(|left, right| name_order(left).cmp(&name_order(right)));
        assert_eq!(names, ["🙂.bin", "abcde.bin"]);
    }
}
