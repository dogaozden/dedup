//! Removal, and where removed files go.
//!
//! §3.4: files on an external drive go to that drive's `.Trashes/<uid>` rather
//! than the home Trash, so emptying the Trash does not mean copying gigabytes
//! back across a USB cable first. `--hard` is the only permanent deletion and
//! is opt-in.
//!
//! §3.2: a group always keeps one copy, enforced here rather than by whatever
//! is asking.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::device::mount_point;
use crate::pipeline::DupGroup;
use crate::walk::{name_order, FileRec};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Shortest,
    Oldest,
    Newest,
}

impl Rule {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "shortest" => Some(Rule::Shortest),
            "oldest" => Some(Rule::Oldest),
            "newest" => Some(Rule::Newest),
            _ => None,
        }
    }

    /// Which member survives. Returns a position within `members`.
    fn keeper(self, records: &[FileRec], members: &[usize]) -> usize {
        let key = |&(_, &index): &(usize, &usize)| records[index].mtime_ns;
        match self {
            Rule::Shortest => members
                .iter()
                .enumerate()
                .min_by_key(|&(_, &index)| name_order(records[index].path()))
                .map(|(position, _)| position),
            Rule::Oldest => members.iter().enumerate().min_by_key(key).map(|(p, _)| p),
            Rule::Newest => members.iter().enumerate().max_by_key(key).map(|(p, _)| p),
        }
        .expect("a group always has members")
    }
}

/// Pick one keeper per group and return the records to remove.
///
/// The survivor guard is this function: the keeper is never added to the list,
/// so no caller can arrange for a group to lose every copy. A record the scan
/// cannot free — because a name outside the tree still pins the inode — is
/// never a victim however duplicate it is, since removing what we can see
/// would free nothing.
pub fn plan_auto(records: &[FileRec], groups: &[DupGroup], rule: Rule) -> Vec<usize> {
    let mut doomed = Vec::new();
    for group in groups {
        let keeper = rule.keeper(records, &group.members);
        for (position, &index) in group.members.iter().enumerate() {
            if position != keeper && records[index].deletable() {
                doomed.push(index);
            }
        }
    }
    doomed
}

/// The mount point of the volume holding `path`.
fn volume_root(path: &Path) -> PathBuf {
    mount_point(path)
}

/// The correct Trash for the volume `path` lives on.
pub fn trash_dir_for(path: &Path) -> PathBuf {
    let root = volume_root(path);
    if root == Path::new("/") {
        return home().join(".Trash");
    }
    root.join(".Trashes").join(uid().to_string())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// The invoking user's id.
///
/// `libc::getuid` would be one call, but REWRITE.md §7 confines `unsafe` to the
/// bulk-walk FFI module, and this is not that module. `id -u` is a subprocess
/// on the same footing as the `diskutil` the oracle already shells out to; the
/// owner of the home directory is the fallback if it is somehow missing.
fn uid() -> u32 {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .or_else(|| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(home())
                .ok()
                .map(|metadata| metadata.uid())
        })
        .unwrap_or(0)
}

/// Pick a non-colliding name inside `directory`.
fn unique_destination(directory: &Path, name: &str) -> PathBuf {
    let target = directory.join(name);
    if !target.exists() {
        return target;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for attempt in 2..10_000 {
        let candidate = directory.join(format!("{stem} {attempt}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(format!("{stem} {}{extension}", crate::cache::now_seconds()))
}

/// Move one file to the Trash of its own volume.
fn send_to_trash(path: &Path) -> Result<(), String> {
    let directory = trash_dir_for(path);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    // Rename first: within a volume it is one atomic operation. A cross-device
    // rename cannot happen here, because the Trash was chosen on the file's own
    // volume, but fall back to copy-then-remove rather than lose the file.
    let destination = unique_destination(&directory, &name);
    match std::fs::rename(path, &destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(path, &destination).map_err(|error| error.to_string())?;
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
    }
}

fn delete_permanently(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|error| error.to_string())
}

/// Remove every name pointing at this record's inode.
///
/// Storage is only released once the last hardlink is gone, so a partial
/// success here would free nothing while looking like it worked.
pub fn remove_record(root: &Path, record: &FileRec, hard: bool) -> Result<(), String> {
    let mut errors = Vec::new();
    for name in &record.names {
        let path = root.join(name);
        let outcome = if hard {
            delete_permanently(&path)
        } else {
            send_to_trash(&path)
        };
        if let Err(error) = outcome {
            errors.push(format!("{name}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, mtime_ns: i64) -> FileRec {
        FileRec {
            names: vec![name.to_string()],
            size: 10,
            dev: 1,
            ino: 1,
            nlink: 1,
            mtime_ns,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }

    #[test]
    fn each_rule_keeps_the_copy_it_names() {
        let records = vec![
            record("deep/nested/a.bin", 300),
            record("b.bin", 100),
            record("c.bin", 200),
        ];
        let members = vec![0, 1, 2];
        assert_eq!(Rule::Shortest.keeper(&records, &members), 1);
        assert_eq!(Rule::Oldest.keeper(&records, &members), 1);
        assert_eq!(Rule::Newest.keeper(&records, &members), 0);
    }

    #[test]
    fn every_group_keeps_one_copy_whatever_the_rule() {
        let records = vec![record("a.bin", 1), record("b.bin", 2), record("c.bin", 3)];
        let groups = vec![DupGroup {
            members: vec![0, 1, 2],
            attested: false,
        }];
        for rule in [Rule::Shortest, Rule::Oldest, Rule::Newest] {
            let doomed = plan_auto(&records, &groups, rule);
            assert_eq!(doomed.len(), records.len() - 1, "{rule:?} kept nothing");
        }
    }

    #[test]
    fn a_record_pinned_from_outside_the_tree_is_never_a_victim() {
        let mut pinned = record("b.bin", 2);
        pinned.nlink = 2; // a second name the scan never saw
        let records = vec![record("a.bin", 1), pinned];
        let groups = vec![DupGroup {
            members: vec![0, 1],
            attested: false,
        }];
        assert!(plan_auto(&records, &groups, Rule::Shortest).is_empty());
    }

    #[test]
    fn a_colliding_trash_name_is_numbered_rather_than_overwritten() {
        let directory = std::env::temp_dir().join("dedup-rs-trash-test");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("scratch directory");
        assert_eq!(
            unique_destination(&directory, "a.bin"),
            directory.join("a.bin")
        );
        std::fs::write(directory.join("a.bin"), b"x").expect("scratch file");
        assert_eq!(
            unique_destination(&directory, "a.bin"),
            directory.join("a 2.bin")
        );
        std::fs::remove_dir_all(&directory).expect("clean up");
    }

    #[test]
    fn the_boot_volume_trashes_to_home_and_others_to_their_own_volume() {
        assert_eq!(trash_dir_for(Path::new("/")), home().join(".Trash"));
    }
}
