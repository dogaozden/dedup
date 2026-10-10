//! Identical folders: a set of folders that hold the same files with the same
//! bytes is one group, and removing a copy means moving the whole folder.
//! M10-FOLDERS.md is the law this file implements.
//!
//! Candidates come from the file groups the scan already proved, never from
//! a walk for folders. A pair of folders is looked at only when one of its
//! files shares a group with a file in the other, so a pair whose files are
//! all under `--min-size` is never found. That is the milestone's accepted
//! limit, and it is written down in LOG.md.
//!
//! Identity looks at everything under a folder, not at what the scan's
//! filters let through. A folder goes to the Trash whole, so every file in it
//! needs a twin in the kept folder, including the 3 KB `notes.txt` that
//! `--min-size` kept out of the walk. That is why the listing here is a plain
//! read with `std::fs` and never goes through the walk.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::cache::{Field, ScanCache};
use crate::cloud;
use crate::compare::{compare_chunk, split_identical, Compare};
use crate::pipeline::DupGroup;
use crate::trash::Rule;
use crate::walk::{name_order, FileRec};

/// The one name ignored anywhere under a folder. Nothing else is special-cased:
/// the maintainer's decision, and the reason a `Thumbs.db` or `._*` file makes
/// two folders different.
const FINDER_NOTE: &str = ".DS_Store";

/// What one entry under a folder is, as far as identity cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file. `nlink` is kept so a trash move can tell whether it
    /// frees the bytes or leaves them alive under a name outside the folder.
    File {
        size: u64,
        dev: u64,
        ino: u64,
        nlink: u64,
    },
    /// A symbolic link. The target is compared as a string and never followed.
    Link { target: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the folder, `/`-joined.
    pub name: String,
    pub kind: Kind,
    pub mtime_ns: i64,
}

/// Every entry under one folder, sorted by name. Two listings being equal is
/// what "the same folder, unchanged" means: it is compared when a folder is
/// found and again just before it moves.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
}

impl Listing {
    /// Regular files only: the ones whose bytes were compared.
    pub fn files(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.kind, Kind::File { .. }))
            .count()
    }

    /// Bytes in one copy of the folder.
    pub fn bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|entry| match entry.kind {
                Kind::File { size, .. } => Some(size),
                Kind::Link { .. } => None,
            })
            .sum()
    }

    /// Bytes a trash move actually frees. A file with a name outside the
    /// folder keeps its data alive in that name, so it is not counted.
    pub fn freed(&self) -> u64 {
        self.entries
            .iter()
            .filter_map(|entry| match entry.kind {
                Kind::File { size, nlink, .. } if nlink <= 1 => Some(size),
                _ => None,
            })
            .sum()
    }

    /// The newest entry in the folder. The `oldest` and `newest` rules compare
    /// this, because a folder is only as old as the last time anything in it
    /// was written.
    pub fn age(&self) -> i64 {
        self.entries
            .iter()
            .map(|entry| entry.mtime_ns)
            .max()
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
pub struct Folder {
    /// Relative to the scan root, `/`-joined.
    pub path: String,
    pub listing: Listing,
}

#[derive(Debug)]
pub struct FolderGroup {
    /// Shortest path first, the way file groups list their members.
    pub folders: Vec<Folder>,
    /// True when some file pair was matched from a digest the cache remembers
    /// rather than bytes read this scan. Such a group is a candidate: every
    /// folder in it is compared byte for byte again before anything moves.
    pub attested: bool,
}

impl FolderGroup {
    /// Files in one copy. Every copy holds the same, so the first one speaks
    /// for all of them.
    pub fn files(&self) -> usize {
        self.folders[0].listing.files()
    }

    /// Bytes in one copy.
    pub fn bytes(&self) -> u64 {
        self.folders[0].listing.bytes()
    }
}

/// Whether a file's bytes are here to read. A cloud placeholder is not, and
/// reading one downloads it, so without `--cloud` a folder holding one is never
/// a copy of anything.
fn present(metadata: &fs::Metadata, cloud: bool) -> bool {
    present_flags(metadata.st_flags(), cloud)
}

fn present_flags(flags: u32, cloud: bool) -> bool {
    cloud || !cloud::dataless(flags)
}

/// Read a folder the way identity needs it: everything under it, symlinks as
/// links, nothing followed. `None` disqualifies the folder. Anything that
/// cannot be listed or read, anything that is not a file, link or directory
/// (a FIFO, socket or device), and a cloud placeholder with `--cloud` off all
/// mean the folder is never guessed to be a copy.
pub fn list(root: &Path, folder: &str, cloud: bool) -> Option<Listing> {
    let path = root.join(folder);
    if !present(&fs::symlink_metadata(&path).ok()?, cloud) {
        return None;
    }
    let mut entries = Vec::new();
    collect(&path, "", cloud, &mut entries)?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Some(Listing { entries })
}

fn collect(directory: &Path, prefix: &str, cloud: bool, out: &mut Vec<Entry>) -> Option<()> {
    for item in fs::read_dir(directory).ok()? {
        let item = item.ok()?;
        // A name that is not UTF-8 cannot be opened again through the
        // relative names the records carry, so it disqualifies rather than
        // being compared under a spelling that might be a different file.
        let name = item.file_name().into_string().ok()?;
        let file_type = item.file_type().ok()?;
        if file_type.is_file() && name == FINDER_NOTE {
            continue;
        }
        let metadata = item.metadata().ok()?;
        if !present(&metadata, cloud) {
            return None;
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let mtime_ns = metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec();
        if file_type.is_symlink() {
            let target = fs::read_link(item.path())
                .ok()?
                .into_os_string()
                .into_string()
                .ok()?;
            out.push(Entry {
                name: relative,
                kind: Kind::Link { target },
                mtime_ns,
            });
        } else if file_type.is_dir() {
            // An empty directory leaves nothing here, which is the rule:
            // trashing one copy that holds an empty `tmp/` loses no data.
            collect(&item.path(), &relative, cloud, out)?;
        } else if file_type.is_file() {
            out.push(Entry {
                name: relative,
                kind: Kind::File {
                    size: metadata.len(),
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                    nlink: metadata.nlink(),
                },
                mtime_ns,
            });
        } else {
            return None;
        }
    }
    Some(())
}

/// The folders above `path`, nearest first: `a/b/c` gives `a/b`, then `a`.
fn ancestors(path: &str) -> impl Iterator<Item = &str> + '_ {
    path.rmatch_indices('/').map(move |(at, _)| &path[..at])
}

/// The value for `path` itself, or for the nearest folder above it, if one of
/// the folders in `map` contains it.
pub(crate) fn within<V: Copy>(path: &str, map: &HashMap<&str, V>) -> Option<V> {
    std::iter::once(path)
        .chain(ancestors(path))
        .find_map(|candidate| map.get(candidate).copied())
}

/// The folder pairs a file group suggests.
///
/// For two members `a/x/f` and `b/x/f`, the folders `a/x` and `b/x` are a
/// candidate, and so are `a` and `b` while the names on the way up keep
/// matching. The top names may differ (`Photos` and `Photos copy`): identity
/// never reads a folder's own name, only the names between it and the files.
fn seeds(records: &[FileRec], groups: &[DupGroup]) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for group in groups {
        for (position, &left) in group.members.iter().enumerate() {
            for &right in &group.members[position + 1..] {
                let mut above = parent(records[left].path());
                let mut below = parent(records[right].path());
                loop {
                    if above.is_empty() || below.is_empty() || above == below {
                        break;
                    }
                    if nested(above, below) {
                        // One folder holds the other, and identical folders
                        // never do.
                        break;
                    }
                    let pair = if above < below {
                        (above.to_string(), below.to_string())
                    } else {
                        (below.to_string(), above.to_string())
                    };
                    pairs.insert(pair);
                    if last(above) != last(below) {
                        break;
                    }
                    above = parent(above);
                    below = parent(below);
                }
            }
        }
    }
    pairs
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(above, _)| above)
}

fn last(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether one of two paths is a folder above the other.
fn nested(left: &str, right: &str) -> bool {
    let inside = |inner: &str, outer: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    inside(left, right) || inside(right, left)
}

/// Whether two listings have the same names and the same sizes and targets.
/// Checked before any byte is read, so most candidate pairs cost two listings
/// and nothing else.
fn same_shape(left: &Listing, right: &Listing) -> bool {
    left.entries.len() == right.entries.len()
        && left
            .entries
            .iter()
            .zip(&right.entries)
            .all(|(ours, theirs)| {
                ours.name == theirs.name
                    && match (&ours.kind, &theirs.kind) {
                        (Kind::File { size: a, .. }, Kind::File { size: b, .. }) => a == b,
                        (Kind::Link { target: a }, Kind::Link { target: b }) => a == b,
                        _ => false,
                    }
            })
}

fn record_of(path: &str, entry: &Entry) -> Option<FileRec> {
    let Kind::File {
        size,
        dev,
        ino,
        nlink,
    } = entry.kind
    else {
        return None;
    };
    Some(FileRec {
        names: vec![path.to_string()],
        size,
        dev,
        ino,
        nlink,
        mtime_ns: entry.mtime_ns,
        retained: None,
    })
}

/// How two folders were settled, file pair by file pair.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Differ,
    /// Identical. `from_cache` is set when some file pair matched on a digest
    /// the cache remembers, rather than on bytes read in this call.
    Same {
        from_cache: bool,
    },
}

/// Whether two folders hold the same files with the same bytes.
///
/// A pair the scan proved this scan is settled without reading, as before. A
/// pair with a remembered whole-file digest on both sides is settled from the
/// cache, and the answer is marked as such. Every other pair is read through
/// the scan's own comparison, and what that read learns is remembered, so the
/// next scan of an untouched tree reads nothing here.
///
/// A remembered difference is believed as well: the cache may only narrow, and
/// a difference it reports can only cost a missed group, never a wrong removal.
///
/// `proven_in` maps each file name a proven group holds to that group.
fn identical(
    context: &Compare,
    proven_in: &HashMap<&str, usize>,
    mut cache: Option<&mut ScanCache>,
    (left_path, left): (&str, &Listing),
    (right_path, right): (&str, &Listing),
    unreadable: &mut Vec<String>,
) -> Verdict {
    if !same_shape(left, right) {
        return Verdict::Differ;
    }
    let chunk = compare_chunk(context.profile, 2, context.solid);
    let mut from_cache = false;
    for (ours, theirs) in left.entries.iter().zip(&right.entries) {
        let Kind::File { size, .. } = ours.kind else {
            continue;
        };
        if size == 0 {
            continue;
        }
        let here = format!("{left_path}/{}", ours.name);
        let there = format!("{right_path}/{}", theirs.name);
        let known = proven_in.get(here.as_str());
        if known.is_some() && known == proven_in.get(there.as_str()) {
            continue;
        }
        let (Some(first), Some(second)) = (record_of(&here, ours), record_of(&there, theirs))
        else {
            return Verdict::Differ;
        };
        if let Some(cache) = cache.as_deref_mut() {
            if let (Some(remembered), Some(other)) = (
                cache.get(&first, Field::Full),
                cache.get(&second, Field::Full),
            ) {
                if remembered != other {
                    return Verdict::Differ;
                }
                from_cache = true;
                continue;
            }
        }
        let records = [first, second];
        let settled = split_identical(
            context,
            &records,
            &[0, 1],
            chunk,
            unreadable,
            None,
            Vec::new(),
        );
        if settled.matched.len() != 1 || settled.matched[0].len() != 2 {
            return Verdict::Differ;
        }
        if let Some(cache) = cache.as_deref_mut() {
            for (index, digest) in settled.learned {
                cache.remember(&records[index], Field::Full, digest);
            }
        }
    }
    Verdict::Same { from_cache }
}

/// Prove again, byte for byte, that `folder` still matches the copy beside it.
///
/// Run on an attested folder before anything moves. Its match came from the
/// cache, so nothing the cache says is taken for granted here: both listings are
/// read again, and then every file pair is read, whatever is remembered. A
/// remembered digest can be forged along with a file's mtime, and this is where
/// that is caught.
pub fn reprove(
    context: &Compare,
    folder: &Folder,
    keeper: &Folder,
    cloud: bool,
    unreadable: &mut Vec<String>,
) -> Result<(), String> {
    unchanged(context.root, folder, cloud)?;
    unchanged(context.root, keeper, cloud)?;
    let verdict = identical(
        context,
        &HashMap::new(),
        None,
        (&folder.path, &folder.listing),
        (&keeper.path, &keeper.listing),
        unreadable,
    );
    match verdict {
        Verdict::Same { .. } => Ok(()),
        Verdict::Differ => Err(
            "its files no longer match the copy beside it, or could not be read in full, so \
             nothing in it was moved"
                .to_string(),
        ),
    }
}

/// The entries of one folder as the records a cache knows them by.
fn records_of(folder: &Folder) -> Vec<FileRec> {
    folder
        .listing
        .entries
        .iter()
        .filter_map(|entry| record_of(&format!("{}/{}", folder.path, entry.name), entry))
        .collect()
}

/// Drop what the cache remembers about every file in a group whose match has
/// been found wrong, so the wrong answer cannot survive into the next scan.
pub fn forget(group: &FolderGroup, cache: &mut ScanCache) {
    let records: Vec<FileRec> = group.folders.iter().flat_map(records_of).collect();
    let refs: Vec<&FileRec> = records.iter().collect();
    cache.forget(&refs);
}

fn root_of(parent: &mut [usize], mut at: usize) -> usize {
    while parent[at] != at {
        parent[at] = parent[parent[at]];
        at = parent[at];
    }
    at
}

/// Find the folder groups the proven file groups point at.
///
/// Every edge (a pair of candidate folders) is tested in a fixed order, and a
/// pair is compared only when the two are not already in one group. A group
/// is built from comparisons that each proved identity, and identity is
/// transitive, so every folder in a group is identical to every other.
///
/// Returns nothing when there are no candidates, and then nothing is read.
pub fn find(
    records: &[FileRec],
    groups: &[DupGroup],
    context: &Compare,
    cloud: bool,
    mut cache: Option<&mut ScanCache>,
    unreadable: &mut Vec<String>,
) -> Vec<FolderGroup> {
    let pairs = seeds(records, groups);
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut paths: Vec<&str> = pairs
        .iter()
        .flat_map(|(first, second)| [first.as_str(), second.as_str()])
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let index: HashMap<&str, usize> = paths
        .iter()
        .enumerate()
        .map(|(at, &path)| (path, at))
        .collect();
    let mut listings: Vec<Option<Listing>> = paths
        .iter()
        .map(|path| list(context.root, path, cloud))
        .collect();

    let mut proven_in: HashMap<&str, usize> = HashMap::new();
    for (number, group) in groups.iter().enumerate() {
        if group.attested {
            continue;
        }
        for &member in &group.members {
            for name in &records[member].names {
                proven_in.insert(name.as_str(), number);
            }
        }
    }

    // On a cold scan the bytes read here are what a warm scan will need, so
    // they are learned when there is a cache to learn them into.
    let learning = Compare {
        root: context.root,
        effort: context.effort,
        profile: context.profile,
        solid: context.solid,
        learn: cache.is_some(),
    };
    let mut parent: Vec<usize> = (0..paths.len()).collect();
    // Set on a root when any pair merged into its group matched from memory.
    let mut attested = vec![false; paths.len()];
    for (first, second) in &pairs {
        let (left, right) = (index[first.as_str()], index[second.as_str()]);
        let (root_left, root_right) = (root_of(&mut parent, left), root_of(&mut parent, right));
        if root_left == root_right {
            continue;
        }
        let (Some(ours), Some(theirs)) = (&listings[left], &listings[right]) else {
            continue;
        };
        let verdict = identical(
            &learning,
            &proven_in,
            cache.as_deref_mut(),
            (paths[left], ours),
            (paths[right], theirs),
            unreadable,
        );
        if let Verdict::Same { from_cache } = verdict {
            parent[root_right] = root_left;
            attested[root_left] |= attested[root_right] || from_cache;
        }
    }

    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (at, listing) in listings.iter().enumerate() {
        if listing.is_some() {
            components
                .entry(root_of(&mut parent, at))
                .or_default()
                .push(at);
        }
    }
    let mut found: Vec<FolderGroup> = components
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(root, members)| {
            let mut folders: Vec<Folder> = members
                .into_iter()
                .filter_map(|at| {
                    listings[at].take().map(|listing| Folder {
                        path: paths[at].to_string(),
                        listing,
                    })
                })
                .collect();
            folders.sort_by(|left, right| name_order(&left.path).cmp(&name_order(&right.path)));
            FolderGroup {
                folders,
                attested: attested[root],
            }
        })
        .collect();
    found = maximal(found);
    found.sort_by(|left, right| {
        let reclaim = |group: &FolderGroup| group.bytes() * (group.folders.len() as u64 - 1);
        reclaim(right).cmp(&reclaim(left)).then_with(|| {
            name_order(&left.folders[0].path).cmp(&name_order(&right.folders[0].path))
        })
    });
    found
}

/// Drop every group that a bigger group already says. When `A` and `B` are
/// identical, `A/sub` and `B/sub` are identical too, and listing them again
/// would only repeat the bigger group.
fn maximal(groups: Vec<FolderGroup>) -> Vec<FolderGroup> {
    let owner: HashMap<&str, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(number, group)| {
            group
                .folders
                .iter()
                .map(move |folder| (folder.path.as_str(), number))
        })
        .collect();
    let implied: Vec<bool> = groups
        .iter()
        .enumerate()
        .map(|(number, group)| implied_by_another(number, group, &owner))
        .collect();
    groups
        .into_iter()
        .zip(implied)
        .filter(|(_, implied)| !implied)
        .map(|(group, _)| group)
        .collect()
}

fn implied_by_another(number: usize, group: &FolderGroup, owner: &HashMap<&str, usize>) -> bool {
    let first = group.folders[0].path.as_str();
    ancestors(first).any(|above| {
        let suffix = &first[above.len() + 1..];
        let mut bigger: Option<usize> = None;
        for folder in &group.folders {
            let Some(base) = folder
                .path
                .strip_suffix(suffix)
                .and_then(|rest| rest.strip_suffix('/'))
            else {
                return false;
            };
            match owner.get(base) {
                Some(&found) if found != number && bigger.is_none_or(|known| known == found) => {
                    bigger = Some(found);
                }
                _ => return false,
            }
        }
        bigger.is_some()
    })
}

/// Whether every member of a file group lies in a different folder of a folder
/// group, and every folder of that group holds exactly one of them.
///
/// Such a file group is what the folder group already says, so it is folded
/// into it and not shown. Exactly one per folder is the rule because a group
/// with a copy missing from one folder says something the folder group does
/// not.
fn folds_into(
    records: &[FileRec],
    members: &[usize],
    index: &HashMap<&str, usize>,
    size: usize,
) -> bool {
    if members.len() != size {
        return false;
    }
    let mut taken = vec![false; size];
    for &member in members {
        let Some(slot) =
            ancestors(records[member].path()).find_map(|above| index.get(above).copied())
        else {
            return false;
        };
        if std::mem::replace(&mut taken[slot], true) {
            return false;
        }
    }
    true
}

/// For each file group, the folder group it folds into, if any. Only the
/// folder groups listed in `active` are considered.
pub fn fold(
    records: &[FileRec],
    groups: &[DupGroup],
    folders: &[FolderGroup],
    active: &[usize],
) -> Vec<Option<usize>> {
    let indexes: Vec<(usize, HashMap<&str, usize>)> = active
        .iter()
        .map(|&number| {
            let index = folders[number]
                .folders
                .iter()
                .enumerate()
                .map(|(at, folder)| (folder.path.as_str(), at))
                .collect();
            (number, index)
        })
        .collect();
    groups
        .iter()
        .map(|group| {
            indexes
                .iter()
                .find(|(number, index)| {
                    folds_into(
                        records,
                        &group.members,
                        index,
                        folders[*number].folders.len(),
                    )
                })
                .map(|(number, _)| *number)
        })
        .collect()
}

/// The folder `rule` keeps out of a folder group.
///
/// `oldest` and `newest` compare each folder's newest entry (`Listing::age`).
/// A tie falls to the shortest path, then the lexicographically first, the same
/// tie-break the file rules use.
pub fn keep_folder(rule: Rule, folders: &[Folder]) -> usize {
    let picked = match rule {
        Rule::Shortest => folders
            .iter()
            .enumerate()
            .min_by_key(|(_, folder)| name_order(&folder.path)),
        Rule::Oldest => folders
            .iter()
            .enumerate()
            .min_by_key(|(_, folder)| (folder.listing.age(), name_order(&folder.path))),
        Rule::Newest => folders
            .iter()
            .enumerate()
            .min_by_key(|(_, folder)| (Reverse(folder.listing.age()), name_order(&folder.path))),
    };
    picked.map_or(0, |(at, _)| at)
}

/// Whether a folder still reads as it did when it was proven. Asked just
/// before it moves, and the window that remains is the one file removal has.
pub fn unchanged(root: &Path, folder: &Folder, cloud: bool) -> Result<(), String> {
    match list(root, &folder.path, cloud) {
        Some(now) if now == folder.listing => Ok(()),
        Some(_) => Err("it changed since it was compared, so it was left alone".to_string()),
        None => Err("it can no longer be read in full, so it was left alone".to_string()),
    }
}

/// What a removal would leave with no copy on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum Lost {
    /// The file group with this index in `groups`.
    File(usize),
    /// The folder group with this index in `folders`.
    Folder(usize),
}

/// Whether a record still has a name that survives the removal.
fn survives(
    record: &FileRec,
    removed: bool,
    moved: &HashMap<&str, usize>,
    on_disk: &dyn Fn(&str) -> bool,
) -> bool {
    !removed
        && record
            .names
            .iter()
            .any(|name| within(name, moved).is_none() && on_disk(name))
}

/// The survivor law, generalized to folders (M10-FOLDERS.md §B). Every group
/// touched by the removal must keep one copy on disk, and a group a removal
/// does not touch is left alone, as it always was.
///
/// `removed` names whole records. `moved` names folders that go, each with the
/// folder group it belongs to. `on_disk` says whether a name still exists.
pub fn stranded(
    records: &[FileRec],
    groups: &[DupGroup],
    folders: &[FolderGroup],
    removed: &HashSet<usize>,
    moved: &HashMap<&str, usize>,
    on_disk: &dyn Fn(&str) -> bool,
) -> Option<Lost> {
    for (number, group) in groups.iter().enumerate() {
        let touched = group.members.iter().any(|&member| {
            removed.contains(&member)
                || records[member]
                    .names
                    .iter()
                    .any(|name| within(name, moved).is_some())
        });
        if !touched {
            continue;
        }
        let alive = group
            .members
            .iter()
            .any(|&member| survives(&records[member], removed.contains(&member), moved, on_disk));
        if !alive {
            return Some(Lost::File(number));
        }
    }
    for (number, group) in folders.iter().enumerate() {
        let touched = group
            .folders
            .iter()
            .any(|folder| within(&folder.path, moved).is_some());
        if !touched {
            continue;
        }
        let alive = group
            .folders
            .iter()
            .any(|folder| within(&folder.path, moved).is_none() && on_disk(&folder.path));
        if !alive {
            return Some(Lost::Folder(number));
        }
    }
    None
}

/// What `--auto` will do about folders, and the files it still removes one by
/// one.
#[derive(Debug, Default)]
pub struct Plan {
    /// Folders to move, as (folder group, folder index). Only the outermost of
    /// any nest: a folder inside another moved folder goes with it.
    pub folders: Vec<(usize, usize)>,
    /// Records to remove, in group order and member order.
    pub files: Vec<usize>,
    /// Folder groups left whole, because moving them would strand a file group
    /// or tear a folder another group keeps.
    pub refused: Vec<usize>,
}

/// Decide what `--auto` removes. `held` names folder groups that must not move
/// this run, because their proof failed before the plan was made.
///
/// Each folder group keeps the folder `rule` picks and moves the rest. Then the
/// file groups are planned against what is left: a member inside a kept folder
/// is the keeper if one exists, and otherwise `rule` picks among the members
/// that are still on disk.
///
/// A plan that breaks the survivor law is never returned. A folder group whose
/// move would tear a kept folder, or leave some file group without a copy, is
/// dropped from the plan and its folders are kept. The loop runs until nothing
/// more needs dropping, and every pass only ever keeps more, so it terminates.
pub fn plan(
    records: &[FileRec],
    groups: &[DupGroup],
    folders: &[FolderGroup],
    rule: Rule,
    held: &[usize],
) -> Plan {
    let mut active = vec![true; folders.len()];
    for &number in held {
        active[number] = false;
    }
    let mut refused = Vec::new();
    loop {
        let live: Vec<usize> = (0..folders.len())
            .filter(|&number| active[number])
            .collect();
        // A held group's folders stay exactly where they are, and what is in
        // them stays too: they shelter their files the way a kept folder does.
        let mut kept: HashMap<&str, usize> = HashMap::new();
        for &number in held {
            for folder in &folders[number].folders {
                kept.insert(folder.path.as_str(), number);
            }
        }
        let mut moves: Vec<(usize, usize)> = Vec::new();
        for &number in &live {
            let keeper = keep_folder(rule, &folders[number].folders);
            kept.insert(folders[number].folders[keeper].path.as_str(), number);
            moves.extend(
                (0..folders[number].folders.len())
                    .filter(|&at| at != keeper)
                    .map(|at| (number, at)),
            );
        }
        let around_kept: HashSet<&str> = kept.keys().flat_map(|path| ancestors(path)).collect();
        let clash = moves.iter().find(|&&(number, at)| {
            let path = folders[number].folders[at].path.as_str();
            within(path, &kept).is_some() || around_kept.contains(path)
        });
        if let Some(&(number, _)) = clash {
            active[number] = false;
            refused.push(number);
            continue;
        }

        let moved: HashMap<&str, usize> = moves
            .iter()
            .map(|&(number, at)| (folders[number].folders[at].path.as_str(), number))
            .collect();
        let folded = fold(records, groups, folders, &live);
        let doomed = plan_files(records, groups, &folded, &moved, &kept, rule);
        let doomed_set: HashSet<usize> = doomed.iter().copied().collect();
        match stranded(records, groups, folders, &doomed_set, &moved, &|_| true) {
            None => {
                let outermost = moves
                    .iter()
                    .copied()
                    .filter(|&(number, at)| {
                        let path = folders[number].folders[at].path.as_str();
                        !ancestors(path).any(|above| moved.contains_key(above))
                    })
                    .collect();
                return Plan {
                    folders: outermost,
                    files: doomed,
                    refused,
                };
            }
            Some(lost) => {
                if live.is_empty() {
                    // Nothing is moved, so nothing can have stranded anything.
                    // Returned rather than looped on, so a bug here cannot hang.
                    return Plan {
                        folders: Vec::new(),
                        files: doomed,
                        refused,
                    };
                }
                let cause = match lost {
                    Lost::Folder(number) => Some(number),
                    Lost::File(number) => groups[number].members.iter().find_map(|&member| {
                        records[member]
                            .names
                            .iter()
                            .find_map(|name| within(name, &moved))
                    }),
                };
                match cause {
                    Some(number) => {
                        active[number] = false;
                        refused.push(number);
                    }
                    None => {
                        for &number in &live {
                            active[number] = false;
                            refused.push(number);
                        }
                    }
                }
            }
        }
    }
}

/// The file-level half of the plan: which records `--auto` removes one by one.
///
/// Only visible groups are planned. A folded group is what its folder group
/// already decides, so nothing is removed from it here.
fn plan_files(
    records: &[FileRec],
    groups: &[DupGroup],
    folded: &[Option<usize>],
    moved: &HashMap<&str, usize>,
    kept: &HashMap<&str, usize>,
    rule: Rule,
) -> Vec<usize> {
    let mut doomed = Vec::new();
    for (number, group) in groups.iter().enumerate() {
        if folded.get(number).copied().flatten().is_some() {
            continue;
        }
        let survivors: Vec<usize> = group
            .members
            .iter()
            .copied()
            .filter(|&member| survives(&records[member], false, moved, &|_| true))
            .collect();
        let sheltered: Vec<usize> = survivors
            .iter()
            .copied()
            .filter(|&member| {
                records[member]
                    .names
                    .iter()
                    .any(|name| within(name, kept).is_some())
            })
            .collect();
        if !sheltered.is_empty() {
            doomed.extend(
                survivors
                    .iter()
                    .copied()
                    .filter(|member| !sheltered.contains(member))
                    .filter(|&member| records[member].deletable()),
            );
        } else if !survivors.is_empty() {
            let keeper = rule.keeper(records, &survivors);
            doomed.extend(
                survivors
                    .iter()
                    .enumerate()
                    .filter(|&(position, _)| position != keeper)
                    .map(|(_, &member)| member)
                    .filter(|&member| records[member].deletable()),
            );
        }
    }
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::SF_DATALESS;
    use crate::device::Profile;
    use crate::effort::Effort;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    /// A scratch tree that removes itself. Nothing here is ever moved to the
    /// Trash: the plan is checked, and removal is left to the e2e script.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!(
                "m10-folders-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("a scratch directory");
            Scratch(root)
        }

        fn write(&self, rel: &str, body: &[u8]) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
            fs::write(path, body).expect("a scratch file");
        }

        fn folder(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }

    /// Stand-in for the scan: every regular file under `root` becomes a record,
    /// and every set of same-size files with the same body becomes one group.
    /// Real scans build these with the walk and the stages; the folder code
    /// only looks at the records and groups, so this is enough to test it.
    fn scan_like(root: &Path) -> (Vec<FileRec>, Vec<DupGroup>) {
        let mut records = Vec::new();
        let mut bodies: Vec<Vec<u8>> = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for item in fs::read_dir(&dir).expect("a readable scratch folder") {
                let item = item.expect("an entry");
                let path = item.path();
                let kind = item.file_type().expect("a file type");
                if kind.is_dir() {
                    stack.push(path);
                    continue;
                }
                // Links and FIFOs are not regular files, and the walk never
                // reads them either. Reading a FIFO here would block forever.
                if !kind.is_file() {
                    continue;
                }
                let name = path
                    .strip_prefix(root)
                    .expect("under root")
                    .to_string_lossy()
                    .into_owned();
                let metadata = fs::symlink_metadata(&path).expect("metadata");
                let body = fs::read(&path).expect("body");
                if body.is_empty() {
                    continue;
                }
                records.push(FileRec {
                    names: vec![name],
                    size: metadata.len(),
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                    nlink: metadata.nlink(),
                    mtime_ns: metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
                    retained: None,
                });
                bodies.push(body);
            }
        }
        let mut order: Vec<usize> = (0..records.len()).collect();
        order.sort_by(|&a, &b| records[a].path().cmp(records[b].path()));
        let mut groups: Vec<DupGroup> = Vec::new();
        let mut assigned = vec![false; records.len()];
        for &first in &order {
            if assigned[first] {
                continue;
            }
            let members: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&other| !assigned[other] && bodies[other] == bodies[first])
                .collect();
            for &member in &members {
                assigned[member] = true;
            }
            if members.len() > 1 {
                groups.push(DupGroup {
                    members,
                    attested: false,
                });
            }
        }
        for group in &mut groups {
            group.members.sort_by(|&a, &b| {
                name_order(records[a].path()).cmp(&name_order(records[b].path()))
            });
        }
        (records, groups)
    }

    fn compare<'a>(root: &'a Path, effort: &'a Effort) -> Compare<'a> {
        Compare {
            root,
            effort,
            profile: Profile::Flash,
            solid: true,
            learn: false,
        }
    }

    /// Two copies of a folder with a few small files, and the byte-identical
    /// twin of each. Returns the scan's view of the tree.
    fn two_copies(scratch: &Scratch) {
        for copy in ["Photos", "Photos copy"] {
            scratch.write(&format!("{copy}/a.jpg"), &[1_u8; 4096]);
            scratch.write(&format!("{copy}/notes.txt"), b"same note");
            scratch.write(&format!("{copy}/sub/b.jpg"), &[2_u8; 4096]);
        }
    }

    fn found(scratch: &Scratch) -> Vec<FolderGroup> {
        let (records, groups) = scan_like(&scratch.0);
        let effort = Effort::default();
        let context = compare(&scratch.0, &effort);
        let mut unreadable = Vec::new();
        find(&records, &groups, &context, false, None, &mut unreadable)
    }

    fn paths(group: &FolderGroup) -> Vec<&str> {
        group
            .folders
            .iter()
            .map(|folder| folder.path.as_str())
            .collect()
    }

    /// Whether the two top-level copies are a group together. `Photos/sub` and
    /// `Photos copy/sub` can be identical on their own, and those are their own
    /// group, so the tests about the top level look at this and nothing else.
    fn top_level_pair(groups: &[FolderGroup]) -> bool {
        groups
            .iter()
            .any(|group| paths(group).contains(&"Photos") || paths(group).contains(&"Photos copy"))
    }

    // ------------------------------------------------------- identity rules

    #[test]
    fn two_identical_folders_are_one_group_whatever_they_are_called() {
        let scratch = Scratch::new("names");
        two_copies(&scratch);
        let groups = found(&scratch);
        assert_eq!(groups.len(), 1, "one group of two identical folders");
        assert_eq!(paths(&groups[0]), ["Photos", "Photos copy"]);
        assert_eq!(groups[0].files(), 3);
    }

    #[test]
    fn an_extra_small_file_stops_two_folders_being_identical() {
        let scratch = Scratch::new("extra");
        two_copies(&scratch);
        scratch.write("Photos copy/extra.txt", b"only here");
        // The top-level copies are no longer identical. `sub` still is, and is
        // its own group.
        let groups = found(&scratch);
        assert!(
            !top_level_pair(&groups),
            "{:?}",
            groups.iter().map(paths).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_small_file_that_differs_by_one_byte_stops_identity() {
        let scratch = Scratch::new("differs");
        two_copies(&scratch);
        scratch.write("Photos copy/notes.txt", b"same notf");
        assert!(!top_level_pair(&found(&scratch)));
    }

    #[test]
    fn a_ds_store_that_differs_is_ignored_at_every_depth() {
        let scratch = Scratch::new("dsstore");
        two_copies(&scratch);
        scratch.write("Photos/.DS_Store", b"one view");
        scratch.write("Photos copy/.DS_Store", b"another view");
        scratch.write("Photos copy/sub/.DS_Store", b"deep");
        let groups = found(&scratch);
        assert_eq!(groups.len(), 1, ".DS_Store must not count");
    }

    #[test]
    fn an_empty_directory_on_one_side_is_ignored() {
        let scratch = Scratch::new("emptydir");
        two_copies(&scratch);
        fs::create_dir_all(scratch.folder("Photos copy/tmp/deeper")).expect("a folder");
        let groups = found(&scratch);
        assert_eq!(groups.len(), 1, "an empty tmp/ loses no data");
    }

    #[test]
    fn a_symlink_with_a_different_target_stops_identity() {
        let scratch = Scratch::new("link");
        two_copies(&scratch);
        std::os::unix::fs::symlink("a.jpg", scratch.folder("Photos/latest")).expect("a link");
        std::os::unix::fs::symlink("b.jpg", scratch.folder("Photos copy/latest")).expect("a link");
        assert!(
            !top_level_pair(&found(&scratch)),
            "targets differ, so not identical"
        );

        fs::remove_file(scratch.folder("Photos copy/latest")).expect("removed");
        std::os::unix::fs::symlink("a.jpg", scratch.folder("Photos copy/latest")).expect("a link");
        assert!(
            top_level_pair(&found(&scratch)),
            "same target string is identical"
        );
    }

    #[test]
    fn a_fifo_disqualifies_its_folder() {
        let scratch = Scratch::new("fifo");
        two_copies(&scratch);
        let status = std::process::Command::new("mkfifo")
            .arg(scratch.folder("Photos/pipe"))
            .status()
            .expect("mkfifo runs");
        assert!(status.success());
        assert!(
            !top_level_pair(&found(&scratch)),
            "a FIFO is never guessed about"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_disqualifies_its_pair() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new("unreadable");
        two_copies(&scratch);
        let locked = scratch.folder("Photos copy/sub");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let outcome = list(&scratch.0, "Photos copy", false);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore");
        assert!(
            outcome.is_none(),
            "a folder that cannot be read is not listed"
        );
    }

    #[test]
    fn a_dataless_file_disqualifies_unless_cloud_is_on() {
        // A placeholder cannot be made in a test, so the decision is checked on
        // the flags word it is made from, which is what the walk checks too.
        assert!(!present_flags(SF_DATALESS, false));
        assert!(present_flags(SF_DATALESS, true));
        assert!(present_flags(0, false));
    }

    #[test]
    fn case_differs_means_not_identical() {
        let scratch = Scratch::new("case");
        scratch.write("one/Note.txt", b"same body");
        scratch.write("two/note.txt", b"same body");
        let groups = found(&scratch);
        assert!(groups.is_empty(), "names are compared as exact bytes");
    }

    // -------------------------------------------------- maximality and 3-way

    #[test]
    fn three_identical_folders_are_one_group() {
        let scratch = Scratch::new("three");
        for copy in ["A", "B", "C"] {
            scratch.write(&format!("{copy}/x.bin"), &[7_u8; 3000]);
            scratch.write(&format!("{copy}/y.bin"), &[8_u8; 3000]);
        }
        let groups = found(&scratch);
        assert_eq!(groups.len(), 1, "identity is an equivalence, one group");
        assert_eq!(groups[0].folders.len(), 3);
    }

    #[test]
    fn a_group_the_bigger_group_already_says_is_not_listed() {
        let scratch = Scratch::new("maximal");
        two_copies(&scratch);
        let groups = found(&scratch);
        // Photos/sub and Photos copy/sub are identical too, but they are part
        // of the group above and are not listed on their own.
        assert_eq!(groups.len(), 1);
        assert!(!groups
            .iter()
            .any(|group| paths(group).contains(&"Photos/sub")));
    }

    #[test]
    fn maximal_drops_a_group_nested_inside_another() {
        let folder = |path: &str| Folder {
            path: path.to_string(),
            listing: Listing::default(),
        };
        let outer = FolderGroup {
            folders: vec![folder("A"), folder("B")],
            attested: false,
        };
        let inner = FolderGroup {
            folders: vec![folder("A/sub"), folder("B/sub")],
            attested: false,
        };
        let kept = maximal(vec![outer, inner]);
        assert_eq!(kept.len(), 1);
        assert_eq!(paths(&kept[0]), ["A", "B"]);
    }

    // ------------------------------------------------------------- folding

    #[test]
    fn a_file_group_with_one_member_per_folder_folds_into_the_folder_group() {
        let scratch = Scratch::new("fold");
        two_copies(&scratch);
        let (records, groups) = scan_like(&scratch.0);
        let effort = Effort::default();
        let context = compare(&scratch.0, &effort);
        let mut unreadable = Vec::new();
        let folders = find(&records, &groups, &context, false, None, &mut unreadable);
        // a.jpg, notes.txt and sub/b.jpg each have exactly one copy per folder.
        let folded = fold(&records, &groups, &folders, &[0]);
        assert_eq!(folded.len(), 3);
        assert!(folded.iter().all(|group| *group == Some(0)), "{folded:?}");
    }

    #[test]
    fn a_third_loose_copy_keeps_its_file_group_visible() {
        let scratch = Scratch::new("loose");
        two_copies(&scratch);
        scratch.write("loose/a.jpg", &[1_u8; 4096]);
        let (records, groups) = scan_like(&scratch.0);
        let effort = Effort::default();
        let context = compare(&scratch.0, &effort);
        let mut unreadable = Vec::new();
        let folders = find(&records, &groups, &context, false, None, &mut unreadable);
        assert_eq!(folders.len(), 1);
        let folded = fold(&records, &groups, &folders, &[0]);
        let loose = groups
            .iter()
            .position(|group| {
                group
                    .members
                    .iter()
                    .any(|&m| records[m].path() == "loose/a.jpg")
            })
            .expect("a group holds the loose copy");
        assert!(
            folded[loose].is_none(),
            "three members do not fold into two folders"
        );
    }

    // ------------------------------------------------- the survivor law

    fn folder_group(paths: &[&str]) -> FolderGroup {
        FolderGroup {
            folders: paths
                .iter()
                .map(|path| Folder {
                    path: path.to_string(),
                    listing: Listing::default(),
                })
                .collect(),
            attested: false,
        }
    }

    fn file(path: &str) -> FileRec {
        FileRec {
            names: vec![path.to_string()],
            size: 10,
            dev: 1,
            ino: 1,
            nlink: 1,
            mtime_ns: 0,
            retained: None,
        }
    }

    #[test]
    fn the_cross_group_trap_is_refused() {
        // Folder B is marked, and A/f and C/f are marked as well, in the file
        // group {A/f, B/f, C/f}. Expanded, every copy goes.
        let records = vec![file("A/f"), file("B/f"), file("C/f")];
        let groups = vec![DupGroup {
            members: vec![0, 1, 2],
            attested: false,
        }];
        let folders = vec![folder_group(&["A", "B"])];
        let moved: HashMap<&str, usize> = HashMap::from([("B", 0)]);
        let removed: HashSet<usize> = HashSet::from([0, 2]);
        let verdict = stranded(&records, &groups, &folders, &removed, &moved, &|_| true);
        assert_eq!(verdict, Some(Lost::File(0)));
    }

    #[test]
    fn a_removal_that_keeps_one_copy_passes() {
        let records = vec![file("A/f"), file("B/f"), file("C/f")];
        let groups = vec![DupGroup {
            members: vec![0, 1, 2],
            attested: false,
        }];
        let folders: Vec<FolderGroup> = Vec::new();
        let moved: HashMap<&str, usize> = HashMap::new();
        let removed: HashSet<usize> = HashSet::from([2]);
        let verdict = stranded(&records, &groups, &folders, &removed, &moved, &|_| true);
        assert_eq!(verdict, None);
    }

    #[test]
    fn a_folder_whose_only_copy_is_moved_is_refused() {
        let records: Vec<FileRec> = Vec::new();
        let groups: Vec<DupGroup> = Vec::new();
        let folders = vec![folder_group(&["A", "B"])];
        let moved: HashMap<&str, usize> = HashMap::from([("A", 0), ("B", 0)]);
        let verdict = stranded(
            &records,
            &groups,
            &folders,
            &HashSet::new(),
            &moved,
            &|_| true,
        );
        assert_eq!(verdict, Some(Lost::Folder(0)));
    }

    // ------------------------------------------------------- the --auto plan

    fn plan_of(
        scratch: &Scratch,
        rule: Rule,
    ) -> (Vec<FileRec>, Vec<DupGroup>, Vec<FolderGroup>, Plan) {
        let (records, groups) = scan_like(&scratch.0);
        let effort = Effort::default();
        let context = compare(&scratch.0, &effort);
        let mut unreadable = Vec::new();
        let folders = find(&records, &groups, &context, false, None, &mut unreadable);
        let decided = plan(&records, &groups, &folders, rule, &[]);
        (records, groups, folders, decided)
    }

    #[test]
    fn auto_moves_one_whole_folder_and_keeps_the_other() {
        let scratch = Scratch::new("plan-moves");
        two_copies(&scratch);
        let (_, _, folders, decided) = plan_of(&scratch, Rule::Shortest);
        assert_eq!(decided.folders, [(0, 1)], "Photos copy goes, Photos stays");
        assert!(decided.refused.is_empty());
        assert!(decided.files.is_empty(), "no loose duplicates to delete");
        assert_eq!(paths(&folders[0])[0], "Photos");
    }

    #[test]
    fn auto_never_tears_a_kept_folder() {
        // Built from ages rather than files: group {A, B} keeps A and would
        // move B, but group {B/x, C/x} keeps B/x, which lies inside B. Moving B
        // would tear the folder the other group keeps, so {A, B} is refused and
        // only C/x moves.
        let folder = |path: &str, mtime_ns: i64| Folder {
            path: path.to_string(),
            listing: Listing {
                entries: vec![Entry {
                    name: "f".to_string(),
                    kind: Kind::Link {
                        target: "t".to_string(),
                    },
                    mtime_ns,
                }],
            },
        };
        let folders = vec![
            FolderGroup {
                folders: vec![folder("A", 900), folder("B", 100)],
                attested: false,
            },
            FolderGroup {
                folders: vec![folder("B/x", 500), folder("C/x", 50)],
                attested: false,
            },
        ];
        let decided = plan(&[], &[], &folders, Rule::Newest, &[]);
        assert_eq!(decided.refused, [0], "the group whose move would tear B/x");
        assert_eq!(decided.folders, [(1, 1)], "only C/x moves");
    }

    #[test]
    fn a_file_added_to_a_marked_folder_blocks_its_move() {
        let scratch = Scratch::new("plan-added");
        two_copies(&scratch);
        let (_, _, folders, _) = plan_of(&scratch, Rule::Shortest);
        scratch.write("Photos copy/late.txt", b"arrived after the scan");
        let verdict = unchanged(&scratch.0, &folders[0].folders[1], false);
        assert!(
            verdict.is_err(),
            "a new file means it is not the proven folder"
        );
        let still_fine = unchanged(&scratch.0, &folders[0].folders[0], false);
        assert!(still_fine.is_ok(), "the kept twin did not change");
    }

    #[test]
    fn a_hardlink_with_a_name_outside_the_folder_is_not_counted_as_freed() {
        // A file with a second name outside the folder keeps its data alive
        // after the folder moves, so only the bytes that really go are counted.
        let file = |name: &str, size: u64, nlink: u64| Entry {
            name: name.to_string(),
            kind: Kind::File {
                size,
                dev: 1,
                ino: name.len() as u64,
                nlink,
            },
            mtime_ns: 0,
        };
        let listing = Listing {
            entries: vec![
                file("alone.bin", 100, 1),
                file("linked.bin", 50, 2),
                file("notes.txt", 7, 1),
            ],
        };
        assert_eq!(listing.bytes(), 157, "every byte in the folder");
        assert_eq!(listing.freed(), 107, "the linked file is not freed");
        assert_eq!(listing.files(), 3);
    }

    #[test]
    fn oldest_and_newest_pick_by_the_newest_file_inside() {
        let older = Folder {
            path: "B".to_string(),
            listing: Listing {
                entries: vec![Entry {
                    name: "f".to_string(),
                    kind: Kind::Link {
                        target: "x".to_string(),
                    },
                    mtime_ns: 100,
                }],
            },
        };
        let newer = Folder {
            path: "A".to_string(),
            listing: Listing {
                entries: vec![Entry {
                    name: "f".to_string(),
                    kind: Kind::Link {
                        target: "x".to_string(),
                    },
                    mtime_ns: 900,
                }],
            },
        };
        let folders = vec![newer, older];
        assert_eq!(keep_folder(Rule::Oldest, &folders), 1);
        assert_eq!(keep_folder(Rule::Newest, &folders), 0);
        assert_eq!(
            keep_folder(Rule::Shortest, &folders),
            0,
            "A is shorter than B"
        );
    }

    #[test]
    fn with_no_folder_groups_the_plan_is_the_old_file_plan() {
        let records = vec![file("a/x.bin"), file("bb/x.bin"), file("c.bin")];
        let groups = vec![DupGroup {
            members: vec![2, 0, 1],
            attested: false,
        }];
        let decided = plan(&records, &groups, &[], Rule::Shortest, &[]);
        assert!(decided.folders.is_empty());
        assert_eq!(decided.files, [0, 1], "the shortest name keeps c.bin");
    }
}
