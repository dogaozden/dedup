//! Removal, and where removed files go.
//!
//! §3.4: files on an external drive go to that drive's `.Trashes/<uid>` rather
//! than the home Trash, so emptying the Trash does not mean copying gigabytes
//! back across a USB cable first. `--hard` is the only permanent deletion and
//! is opt-in.
//!
//! §3.2: a group always keeps one copy, enforced here rather than by whatever
//! is asking.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::ScanCache;
use crate::compare::{verify_attested, Compare};
use crate::device::mount_point;
use crate::folders::{self, Lost};
use crate::pipeline::{DupGroup, ProvenGroups, Scan};
use crate::tui::model::Mark;
use crate::walk::{name_order, FileRec};

/// What a non-interactive run was asked to do.
pub struct Removal {
    pub rule: Rule,
    pub hard: bool,
    pub assume_yes: bool,
    /// The `--cloud` setting of the scan, so a folder is listed again the way
    /// it was proven.
    pub cloud: bool,
}

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

    fn name(self) -> &'static str {
        match self {
            Rule::Shortest => "shortest",
            Rule::Oldest => "oldest",
            Rule::Newest => "newest",
        }
    }

    /// Which member survives, given each member's canonical name and the
    /// modification time the oracle compares. Returns a position within
    /// `members`.
    ///
    /// Python's `min` and `max` both return the FIRST extreme; Rust's
    /// `max_by_key` returns the LAST. Members arrive shortest-name-first, so an
    /// unadjusted `max` would keep the longest-named copy on a tie where the
    /// oracle keeps the shortest-named one. Reversing the comparator and taking
    /// the minimum gets the first maximum, which is what `dedup.py:2226` does.
    ///
    /// The browser calls this too. It has to hand over the same strings this
    /// gets from `keeper`, which are the scan's own names — a name built for
    /// the screen ranks differently, and sharing the function would then not
    /// be sharing the rule.
    pub fn keeper_of(self, members: &[(&str, f64)]) -> usize {
        match self {
            Rule::Shortest => members
                .iter()
                .enumerate()
                .min_by_key(|&(_, &(name, _))| name_order(name))
                .map(|(position, _)| position),
            Rule::Oldest => members
                .iter()
                .enumerate()
                .min_by(|left, right| left.1 .1.total_cmp(&right.1 .1))
                .map(|(position, _)| position),
            Rule::Newest => members
                .iter()
                .enumerate()
                .min_by(|left, right| right.1 .1.total_cmp(&left.1 .1))
                .map(|(position, _)| position),
        }
        .expect("a group always has members")
    }

    pub(crate) fn keeper(self, records: &[FileRec], members: &[usize]) -> usize {
        let pairs: Vec<(&str, f64)> = members
            .iter()
            .map(|&index| (records[index].path(), oracle_mtime(&records[index])))
            .collect();
        self.keeper_of(&pairs)
    }
}

impl std::fmt::Display for Rule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The modification time the oracle compares, which is `os.stat`'s `st_mtime`:
/// a float64 built as `seconds + 1e-9 * nanoseconds`.
///
/// At current epoch values a float64 resolves about 240 ns, so two files a
/// nanosecond apart are *tied* for the oracle and ordered for anything reading
/// `st_mtime_ns`. Which copy survives is the whole user-visible contract of a
/// keep rule and REWRITE.md §0 makes the oracle authoritative for behaviour, so
/// the coarser quantity is the right one to compare here. The finer `mtime_ns`
/// is still what the cache key and the retained-bytes revalidation use, where
/// being stricter than the oracle only ever means reading more.
pub fn oracle_mtime(record: &FileRec) -> f64 {
    let seconds = record.mtime_ns.div_euclid(1_000_000_000);
    let nanoseconds = record.mtime_ns.rem_euclid(1_000_000_000);
    seconds as f64 + 1e-9 * nanoseconds as f64
}

/// The file records `--auto` removes when no folder group is in play.
///
/// The survivor guard is the planner: the keeper is never added to the list,
/// so no caller can arrange for a group to lose every copy. A record the scan
/// cannot free — because a name outside the tree still pins the inode — is
/// never a victim however duplicate it is, since removing what we can see
/// would free nothing.
#[cfg(test)]
fn plan_auto(records: &[FileRec], groups: &[DupGroup], rule: Rule) -> Vec<usize> {
    folders::plan(records, groups, &[], rule, &[]).files
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

/// Move one file or one whole folder to the Trash of its own volume.
fn send_to_trash(path: &Path) -> Result<(), String> {
    let directory = trash_dir_for(path);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let destination = unique_destination(&directory, &name);
    move_to(path, &destination)
}

/// Move a file or a folder to `destination`: one rename, and a copy only when
/// the rename crossed a volume boundary.
///
/// A folder is one directory rename (M10-FOLDERS.md §D), not a move per file.
/// Any other rename failure, such as a permission, a busy file or a name that
/// is too long, has changed nothing, so it is returned as it is and the source
/// stays where it was. Copying after such a failure would repeat it, or leave
/// two copies of something that was meant to move. A copy that fails part-way
/// is cleaned up, and the source is removed only once the copy is complete.
fn move_to(path: &Path, destination: &Path) -> Result<(), String> {
    let is_folder = path.symlink_metadata().is_ok_and(|meta| meta.is_dir());
    match std::fs::rename(path, destination) {
        Ok(()) => Ok(()),
        Err(error) if !crosses_devices(&error) => Err(error.to_string()),
        Err(_) if is_folder => {
            if let Err(error) = copy_tree(path, destination) {
                let _ = std::fs::remove_dir_all(destination);
                return Err(error.to_string());
            }
            std::fs::remove_dir_all(path).map_err(|error| error.to_string())
        }
        Err(_) => {
            if let Err(error) = std::fs::copy(path, destination) {
                let _ = std::fs::remove_file(destination);
                return Err(error.to_string());
            }
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
    }
}

/// Whether a failed rename failed only because source and destination are on
/// different volumes. That is the one case where copying is the way to move
/// something.
fn crosses_devices(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::CrossesDevices
}

/// Copy a folder tree, links as links. Nothing is followed, so a link cannot
/// pull in a file from outside the folder.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &target)?;
        } else if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Delete a file, or a folder and everything in it. `remove_dir_all` does not
/// follow links, so only the folder's own tree goes.
fn delete_permanently(path: &Path) -> Result<(), String> {
    let is_folder = path.symlink_metadata().is_ok_and(|meta| meta.is_dir());
    if is_folder {
        std::fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        std::fs::remove_file(path).map_err(|error| error.to_string())
    }
}

/// One path, removed the way the run asked: to the Trash, or for good.
fn remove_path(path: &Path, hard: bool) -> Result<(), String> {
    if hard {
        delete_permanently(path)
    } else {
        send_to_trash(path)
    }
}

/// Remove the named paths under `root`, every one or none of them reported.
fn remove_names(root: &Path, names: &[&str], hard: bool) -> Result<(), String> {
    let mut errors = Vec::new();
    for name in names {
        if let Err(error) = remove_path(&root.join(name), hard) {
            errors.push(format!("{name}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Remove every name pointing at this record's inode.
///
/// Storage is only released once the last hardlink is gone, so a partial
/// success here would free nothing while looking like it worked.
pub fn remove_record(root: &Path, record: &FileRec, hard: bool) -> Result<(), String> {
    let names: Vec<&str> = record.names.iter().map(String::as_str).collect();
    remove_names(root, &names, hard)
}

/// The test seam's hook: called with each folder just before `--auto` checks it
/// again and moves it, so a test can change the tree between the plan and the move.
#[cfg(test)]
type BeforeMove = Box<dyn Fn(&Path)>;

#[cfg(test)]
thread_local! {
    static BEFORE_MOVE: std::cell::RefCell<Option<BeforeMove>> =
        const { std::cell::RefCell::new(None) };
}

/// Non-interactive cleanup: keep one copy per group by `rule`, remove the rest.
///
/// Taking `ProvenGroups` rather than a slice is the guard: the only way to
/// build one is `Scan::proven`, which refuses a scan that merely grouped by
/// size. A caller cannot reach this function with groups whose members have
/// never been compared, however the modes are spelled on the command line.
pub fn run_auto(
    scan: &Scan,
    proven: ProvenGroups<'_>,
    removal: &Removal,
    context: &Compare,
    mut cache: Option<&mut ScanCache>,
) -> i32 {
    // Groups matched from the cache have not been read yet. They are read here,
    // before a plan is even printed, so the plan describes files that really
    // are identical rather than files a digest said were.
    let candidates = proven
        .groups()
        .iter()
        .map(|group| DupGroup {
            members: group.members.clone(),
            attested: group.attested,
        })
        .collect();
    let (groups, refused) =
        verify_attested(context, &scan.records, candidates, cache.as_deref_mut());
    report_refusals(&scan.records, &refused);

    // The same for folder groups matched from the cache: every folder is
    // compared byte for byte with its kept copy again. A group that fails is
    // held where it is, whole, and what the cache remembered about it is dropped.
    let folders = proven.folders();
    let mut held = Vec::new();
    let mut unreadable = Vec::new();
    for (number, group) in folders.iter().enumerate() {
        if !group.attested {
            continue;
        }
        let keeper = folders::keep_folder(removal.rule, &group.folders);
        let failure = group
            .folders
            .iter()
            .enumerate()
            .filter(|&(at, _)| at != keeper)
            .find_map(|(_, folder)| {
                folders::reprove(
                    context,
                    folder,
                    &group.folders[keeper],
                    removal.cloud,
                    &mut unreadable,
                )
                .err()
                .map(|why| (folder.path.as_str(), why))
            });
        if let Some((path, why)) = failure {
            eprintln!(
                "\n  Refusing the folder group at {}/: {path}/ {why}. \
                 What the cache remembered about the group has been dropped.",
                group.folders[keeper].path
            );
            if let Some(cache) = cache.as_deref_mut() {
                folders::forget(group, cache);
            }
            held.push(number);
        }
    }

    let plan = folders::plan(&scan.records, &groups, folders, removal.rule, &held);
    for &number in &plan.refused {
        let group = &folders[number];
        eprintln!(
            "\n  Left alone: the folder group at {}/ (+{} more). Moving its copies \
             would leave a file with no copy, or tear a folder another group keeps.",
            group.folders[0].path,
            group.folders.len() - 1
        );
    }

    // The plan is correct by construction; this says so again against the disk
    // before anything moves, and refuses the whole run if it is ever wrong.
    let moved: HashMap<&str, usize> = plan
        .folders
        .iter()
        .map(|&(number, at)| (folders[number].folders[at].path.as_str(), number))
        .collect();
    let doomed: HashSet<usize> = plan.files.iter().copied().collect();
    let on_disk = |name: &str| context.root.join(name).symlink_metadata().is_ok();
    if let Some(lost) =
        folders::stranded(&scan.records, &groups, folders, &doomed, &moved, &on_disk)
    {
        let which = match lost {
            Lost::File(number) => format!("group {}", number + 1),
            Lost::Folder(number) => format!("folder group {}", number + 1),
        };
        eprintln!(
            "  Refusing: this plan would leave {which} with no copy at all. Nothing was removed."
        );
        return 1;
    }

    if plan.folders.is_empty() && plan.files.is_empty() {
        println!("\n  Nothing to remove.\n");
        return 0;
    }

    let folder_files: usize = plan
        .folders
        .iter()
        .map(|&(number, at)| folders[number].folders[at].listing.files())
        .sum();
    let folder_freed: u64 = plan
        .folders
        .iter()
        .map(|&(number, at)| folders[number].folders[at].listing.freed())
        .sum();
    let total: u64 = plan
        .files
        .iter()
        .map(|&index| scan.records[index].size)
        .sum::<u64>()
        + folder_freed;
    let verb = if removal.hard {
        "Delete permanently"
    } else {
        "Move to Trash"
    };
    if folders.is_empty() {
        println!(
            "\n  Keeping the {} copy in each of {} groups.",
            removal.rule,
            groups.len()
        );
        println!(
            "  {verb}: {} files · {total} bytes reclaimed\n",
            plan.files.len()
        );
    } else {
        println!(
            "\n  Keeping the {} copy in each of {} folder group(s) and {} file group(s).",
            removal.rule,
            folders.len() - held.len(),
            groups.len()
        );
        if plan.folders.is_empty() {
            println!(
                "  {verb}: {} files · {total} bytes reclaimed\n",
                plan.files.len()
            );
        } else {
            println!(
                "  {verb}: {} folder(s) ({folder_files} files) and {} file(s) · {total} bytes reclaimed\n",
                plan.folders.len(),
                plan.files.len()
            );
        }
    }
    let lines: Vec<String> = plan
        .folders
        .iter()
        .map(|&(number, at)| format!("{}/", folders[number].folders[at].path))
        .chain(
            plan.files
                .iter()
                .map(|&index| scan.records[index].path().to_string()),
        )
        .collect();
    for line in lines.iter().take(15) {
        println!("      {line}");
    }
    if lines.len() > 15 {
        println!("      … and {} more", lines.len() - 15);
    }
    println!();

    if !removal.assume_yes {
        if !std::io::stdin().is_terminal() {
            let _ = std::io::stdout().flush(); // keep the plan above the refusal
            eprintln!("  Refusing to delete without confirmation. Re-run with -y to proceed.");
            return 1;
        }
        let mut answer = String::new();
        print!("  Type 'yes' to proceed: ");
        let _ = std::io::stdout().flush();
        if std::io::stdin().lock().read_line(&mut answer).is_err() || answer.trim() != "yes" {
            println!("  Aborted. Nothing deleted.");
            return 1;
        }
    }

    // Folders first: a file inside a folder that is going must not be touched
    // separately, and a folder that refuses to move leaves its files where they
    // are. Each folder and its kept twin are listed again immediately before the
    // move, and a folder that has changed since the proof stays where it is.
    let (mut done, mut freed) = (0_usize, 0_u64);
    let (mut folders_done, mut folder_files_done) = (0_usize, 0_usize);
    let mut failures = Vec::new();
    for &(number, at) in &plan.folders {
        let group = &folders[number];
        let folder = &group.folders[at];
        let twin = &group.folders[folders::keep_folder(removal.rule, &group.folders)];
        #[cfg(test)]
        BEFORE_MOVE.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook(&context.root.join(&folder.path));
            }
        });
        let proof_holds = folders::unchanged(context.root, folder, removal.cloud)
            .and_then(|()| folders::unchanged(context.root, twin, removal.cloud));
        if let Err(why) = proof_holds {
            failures.push(format!("{}/: {why}", folder.path));
            continue;
        }
        match remove_path(&context.root.join(&folder.path), removal.hard) {
            Ok(()) => {
                folders_done += 1;
                folder_files_done += folder.listing.files();
                freed += folder.listing.freed();
            }
            Err(error) => failures.push(format!("{}/: {error}", folder.path)),
        }
    }
    for &index in &plan.files {
        let record = &scan.records[index];
        // A name inside a folder the plan moves has gone with the folder, or is
        // being kept with it, and is not removed separately.
        let names: Vec<&str> = record
            .names
            .iter()
            .map(String::as_str)
            .filter(|name| folders::within(name, &moved).is_none())
            .collect();
        match remove_names(context.root, &names, removal.hard) {
            Ok(()) => {
                done += names.len();
                freed += record.size;
            }
            Err(error) => failures.push(error),
        }
    }

    let where_to = if removal.hard {
        "deleted"
    } else {
        "moved to Trash"
    };
    if folders.is_empty() {
        println!("\n  {done} files {where_to} · {freed} bytes reclaimed");
    } else {
        println!(
            "\n  {folders_done} folder(s) ({folder_files_done} files) and {done} files {where_to} · {freed} bytes reclaimed"
        );
    }
    if !failures.is_empty() {
        eprintln!("  {} could not be removed:", failures.len());
        for line in failures.iter().take(10) {
            eprintln!("    {line}");
        }
    }
    println!();
    0
}

/// What one round of removal did, for a caller that has to carry on afterwards.
#[derive(Debug, Default)]
pub struct Removed {
    /// Names removed, not records: a hardlinked record removes several.
    pub files: usize,
    pub bytes: u64,
    /// The records that are actually gone.
    pub records: Vec<usize>,
    /// Groups whose copies no longer matched, left whole and untouched.
    pub refused: Vec<DupGroup>,
    pub failures: Vec<String>,
}

/// Remove exactly the records the browser marked.
///
/// This is `run_auto`'s sibling and takes the same token for the same reason:
/// `ProvenGroups` can only be built by `Scan::proven`, so a browser over a
/// scan that merely grouped by size cannot reach this function at all.
///
/// A `Mark` names a group in the scan and a record in the scan, never a
/// position on anyone's screen. Positions move — a browser that has already
/// removed something renumbers what is left — and a stale position would name
/// a different file rather than an absent one, which for this program means
/// deleting something nobody chose.
///
/// The survivor guard is enforced here rather than trusted from the UI, because
/// §3.2 says it is the code's job. What it counts is copies that will still be
/// on disk afterwards, not rows in a list: a group half-emptied by an earlier
/// round has fewer survivors left than its membership suggests. The whole
/// request is checked before anything is removed and a request that would empty
/// any group is refused entire — a deletion tool that quietly did most of what
/// it was asked would be worse than one that did nothing.
pub fn run_selected(
    scan: &Scan,
    proven: ProvenGroups<'_>,
    marks: &[Mark],
    hard: bool,
    context: &Compare,
    mut cache: Option<&mut ScanCache>,
) -> Result<Removed, String> {
    let groups = proven.groups();
    let mut wanted: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &Mark { group, record } in marks {
        let members = &groups
            .get(group)
            .ok_or_else(|| format!("no such group: {group}"))?
            .members;
        if !members.contains(&record) {
            return Err(format!("record {record} is not in group {}", group + 1));
        }
        wanted.entry(group).or_default().push(record);
    }
    for chosen in wanted.values_mut() {
        chosen.sort_unstable();
        chosen.dedup();
    }
    // The survivor law, checked over everything this selection touches. A
    // browser selection names records only, so no folder is moved here.
    let removed: HashSet<usize> = wanted.values().flatten().copied().collect();
    let on_disk = |name: &str| context.root.join(name).symlink_metadata().is_ok();
    if let Some(lost) = folders::stranded(
        &scan.records,
        groups,
        proven.folders(),
        &removed,
        &HashMap::new(),
        &on_disk,
    ) {
        return Err(match lost {
            Lost::File(group) => format!(
                "refusing the whole selection: it would leave group {} with no copy at all",
                group + 1
            ),
            Lost::Folder(group) => format!(
                "refusing the whole selection: it would leave folder group {} with no copy at all",
                group + 1
            ),
        });
    }

    let mut outcome = Removed::default();
    for (&group, chosen) in wanted.iter() {
        // A group the cache vouched for has not been read this scan. Read it
        // now, before anything in it goes, and drop the whole group if the
        // copies have stopped matching.
        let candidate = vec![DupGroup {
            members: groups[group].members.clone(),
            attested: groups[group].attested,
        }];
        let (_proved, refused) =
            verify_attested(context, &scan.records, candidate, cache.as_deref_mut());
        if !refused.is_empty() {
            outcome.refused.extend(refused);
            continue;
        }
        for &index in chosen {
            let record = &scan.records[index];
            // Removing a record pinned from outside the tree would take the
            // names away and free nothing, so it is never a victim, however it
            // came to be marked.
            if !record.deletable() {
                continue;
            }
            match remove_record(context.root, record, hard) {
                Ok(()) => {
                    outcome.files += record.names.len();
                    outcome.bytes += record.size;
                    outcome.records.push(index);
                }
                Err(error) => outcome.failures.push(error),
            }
        }
    }
    Ok(outcome)
}

/// Say plainly that some groups were not what the cache thought they were.
pub fn report_refusals(records: &[FileRec], refused: &[DupGroup]) {
    if refused.is_empty() {
        return;
    }
    eprintln!(
        "\n  Refusing to touch {} group(s): their copies no longer match.",
        refused.len()
    );
    for group in refused.iter().take(10) {
        eprintln!(
            "    {} (+{} more)",
            records[group.members[0]].path(),
            group.members.len() - 1
        );
    }
    eprintln!(
        "  Nothing was removed from them, and what was remembered about them \
         has been discarded."
    );
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
            retained: None,
        }
    }

    #[test]
    fn a_rule_round_trips_through_its_name() {
        for rule in [Rule::Shortest, Rule::Oldest, Rule::Newest] {
            assert_eq!(Rule::parse(&rule.to_string()), Some(rule));
        }
        assert_eq!(Rule::parse("biggest"), None);
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
    fn a_tie_keeps_the_first_member_the_way_python_does() {
        // Python's min and max both return the first extreme. Members arrive
        // shortest-name-first, so a tie must keep the shortest name — an
        // unadjusted Rust max_by_key would keep the last, the longest.
        let records = vec![
            record("c.bin", 500),
            record("bb.bin", 500),
            record("aaa.bin", 500),
        ];
        let members = vec![0, 1, 2];
        assert_eq!(
            Rule::Newest.keeper(&records, &members),
            0,
            "newest on a tie"
        );
        assert_eq!(
            Rule::Oldest.keeper(&records, &members),
            0,
            "oldest on a tie"
        );

        // ...and with only the later two tied for newest, the first of those.
        let records = vec![
            record("c.bin", 100),
            record("bb.bin", 500),
            record("aaa.bin", 500),
        ];
        assert_eq!(Rule::Newest.keeper(&records, &members), 1);
    }

    #[test]
    fn mtimes_closer_than_the_oracle_can_see_count_as_tied() {
        // The oracle compares st_mtime as a float64, which resolves about
        // 240 ns at current epoch values, so one nanosecond apart is a tie and
        // the tie-break decides. Comparing mtime_ns instead would order them
        // and keep a different file.
        let base = 1_700_000_000_i64 * 1_000_000_000;
        let records = vec![
            record("c.bin", base + 8_000_000_000),
            record("bb.bin", base + 1),
            record("aaa.bin", base),
        ];
        let members = vec![0, 1, 2];
        assert_eq!(
            Rule::Oldest.keeper(&records, &members),
            1,
            "bb.bin ties aaa.bin"
        );
        assert!(oracle_mtime(&records[1]) == oracle_mtime(&records[2]));
        // A whole second apart is still plainly ordered.
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

/// `run_selected` against real trees, because what it has to get right is what
/// is left on the disk afterwards.
///
/// Everything here removes permanently rather than to the Trash, so a test run
/// never puts anything in the user's own Trash to clean up. Trash placement is
/// covered end to end in `evidence/scripts/m5_end_to_end.py`, which can point
/// `HOME` somewhere scratch.
#[cfg(test)]
mod selection {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::effort::Effort;
    use crate::filters::Filters;
    use crate::pipeline::find_duplicates;

    static SERIAL: AtomicUsize = AtomicUsize::new(0);

    /// A scratch tree, and the ground beside it for names the scan must not
    /// see. Removed when the test ends, however it ends.
    pub(super) struct Tree {
        pub(super) base: PathBuf,
        pub(super) root: PathBuf,
    }

    impl Tree {
        pub(super) fn new(files: &[(&str, &[u8])]) -> Tree {
            let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
            let base =
                std::env::temp_dir().join(format!("m5-selection-{}-{serial}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let root = base.join("root");
            std::fs::create_dir_all(&root).expect("scratch root");
            let tree = Tree { base, root };
            for (name, content) in files {
                tree.write(name, content);
            }
            tree
        }

        pub(super) fn write(&self, name: &str, content: &[u8]) {
            let path = self.root.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("scratch directory");
            }
            std::fs::write(path, content).expect("scratch file");
        }

        fn link(&self, from: &str, to: &str) {
            let target = self.root.join(to);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("scratch directory");
            }
            std::fs::hard_link(self.root.join(from), target).expect("hard link");
        }

        /// A name for the same inode that lives outside the scanned root, so
        /// the scan sees fewer names than the inode has.
        fn pin(&self, name: &str) {
            std::fs::hard_link(self.root.join(name), self.base.join("pinned"))
                .expect("hard link outside the tree");
        }

        pub(super) fn scan(&self, effort: &Effort) -> Scan {
            self.scan_with(effort, crate::device::Wanted::Flash)
        }

        fn scan_with(&self, effort: &Effort, wanted: crate::device::Wanted) -> Scan {
            find_duplicates(
                &self.root,
                Filters {
                    min_size: 1,
                    include_ext: None,
                    skip_ext: &[],
                    excludes: &[],
                    cloud: false,
                },
                &mut crate::device::VolumeProbe::start(&self.root, Some(1)),
                effort,
                None,
                wanted,
                &crate::pulse::Pulse::off(),
                &crate::cloud::Cloud::default(),
                None,
            )
        }

        fn survivors(&self) -> Vec<String> {
            let mut found = Vec::new();
            let mut stack = vec![self.root.clone()];
            while let Some(directory) = stack.pop() {
                for entry in std::fs::read_dir(&directory).expect("read the tree") {
                    let entry = entry.expect("a directory entry");
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else {
                        found.push(
                            path.strip_prefix(&self.root)
                                .expect("under the root")
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
            }
            found.sort();
            found
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// The mark that names a file: its group, and its record in the scan.
    fn locate(scan: &Scan, name: &str) -> Mark {
        for (group, members) in scan.reported().iter().enumerate() {
            for &record in &members.members {
                if scan.records[record].path() == name {
                    return Mark { group, record };
                }
            }
        }
        panic!("{name} is not in any reported group");
    }

    fn context<'a>(tree: &'a Tree, effort: &'a Effort) -> Compare<'a> {
        context_for(tree, effort, crate::device::Profile::Flash)
    }

    fn context_for<'a>(
        tree: &'a Tree,
        effort: &'a Effort,
        profile: crate::device::Profile,
    ) -> Compare<'a> {
        Compare {
            root: &tree.root,
            effort,
            profile,
            solid: true,
            learn: false,
        }
    }

    #[test]
    fn exactly_the_marked_members_go_and_the_rest_stay() {
        let tree = Tree::new(&[
            ("a.bin", b"the same hundred bytes"),
            ("b.bin", b"the same hundred bytes"),
            ("c.bin", b"the same hundred bytes"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let marks = vec![locate(&scan, "b.bin"), locate(&scan, "c.bin")];
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &marks,
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("a selection that leaves a survivor");

        assert_eq!(tree.survivors(), vec!["a.bin"]);
        assert_eq!(removed.files, 2);
        assert_eq!(removed.bytes, 2 * b"the same hundred bytes".len() as u64);
        assert_eq!(removed.records.len(), 2);
        assert!(removed.refused.is_empty());
        assert!(removed.failures.is_empty());
    }

    #[test]
    fn a_selection_that_would_empty_a_group_is_refused_whole() {
        let tree = Tree::new(&[
            ("a.bin", b"aaaaaaaaaaaaaaaaaaaaaaaa"),
            ("b.bin", b"aaaaaaaaaaaaaaaaaaaaaaaa"),
            ("c.bin", b"aaaaaaaaaaaaaaaaaaaaaaaa"),
            ("x.dat", b"bbbbbbbbbbbb"),
            ("y.dat", b"bbbbbbbbbbbb"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        // Every copy in the first group, plus a perfectly legal mark in the
        // second one. The legal mark must not be acted on either.
        let marks = vec![
            locate(&scan, "a.bin"),
            locate(&scan, "b.bin"),
            locate(&scan, "c.bin"),
            locate(&scan, "y.dat"),
        ];
        let error = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &marks,
            true,
            &context(&tree, &effort),
            None,
        )
        .expect_err("marking every copy was allowed");

        assert!(error.contains("no copy at all"), "{error}");
        assert_eq!(
            tree.survivors(),
            vec!["a.bin", "b.bin", "c.bin", "x.dat", "y.dat"],
            "a refused request still removed something"
        );
    }

    #[test]
    fn the_same_mark_twice_is_still_one_file_and_still_guarded() {
        let tree = Tree::new(&[
            ("a.bin", b"duplicated content here"),
            ("b.bin", b"duplicated content here"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let b = locate(&scan, "b.bin");
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[b, b, b],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("one file marked three times is one file");
        assert_eq!(removed.files, 1);
        assert_eq!(tree.survivors(), vec!["a.bin"]);
    }

    #[test]
    fn a_record_pinned_from_outside_the_tree_survives_being_marked() {
        let tree = Tree::new(&[
            ("a.bin", b"pinned content xxxxxxxx"),
            ("b.bin", b"pinned content xxxxxxxx"),
        ]);
        tree.pin("b.bin"); // a second name the scan cannot see
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "b.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("the request itself is legal");

        assert_eq!(removed.files, 0, "a pinned record was removed");
        assert_eq!(removed.bytes, 0);
        assert!(removed.records.is_empty());
        assert_eq!(tree.survivors(), vec!["a.bin", "b.bin"]);
    }

    #[test]
    fn a_hardlink_cluster_is_removed_as_one_unit() {
        let tree = Tree::new(&[
            ("a.bin", b"clustered content yyyy"),
            ("z.bin", b"clustered content yyyy"),
        ]);
        tree.link("a.bin", "sub/also-a.bin"); // both names are inside the tree
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "a.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("the cluster may go, z.bin survives");

        // Two names, one inode: both names gone, the bytes counted once.
        assert_eq!(removed.files, 2);
        assert_eq!(removed.bytes, b"clustered content yyyy".len() as u64);
        assert_eq!(removed.records.len(), 1);
        assert_eq!(tree.survivors(), vec!["z.bin"]);
    }

    #[test]
    fn a_mark_that_names_nothing_is_an_error_rather_than_a_deletion() {
        let tree = Tree::new(&[
            ("a.bin", b"content for the bounds test"),
            ("b.bin", b"content for the bounds test"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let out_of_range = [
            (
                Mark {
                    group: 9,
                    record: 0,
                },
                "no such group",
            ),
            (
                Mark {
                    group: 0,
                    record: 9,
                },
                "is not in group",
            ),
        ];
        for (bad, expected) in out_of_range {
            let error = run_selected(
                &scan,
                scan.proven().expect("the bytes were compared"),
                &[bad],
                true,
                &context(&tree, &effort),
                None,
            )
            .expect_err("an out-of-range mark was accepted");
            assert!(error.contains(expected), "{error}");
        }
        assert_eq!(tree.survivors(), vec!["a.bin", "b.bin"]);
    }

    #[test]
    fn the_guard_counts_copies_still_on_disk_rather_than_rows_in_the_group() {
        // A browsing session removes in rounds, and the scan's idea of a group
        // does not shrink between them. Counting members would let the second
        // round take the last copy, because the group still claims three.
        let tree = Tree::new(&[
            ("a.bin", b"content across two rounds"),
            ("b.bin", b"content across two rounds"),
            ("c.bin", b"content across two rounds"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "b.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("the first round is legal");
        assert_eq!(tree.survivors(), vec!["a.bin", "c.bin"]);

        let error = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "a.bin"), locate(&scan, "c.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect_err("the second round emptied the group");
        assert!(error.contains("no copy at all"), "{error}");
        assert_eq!(tree.survivors(), vec!["a.bin", "c.bin"]);

        // ...and one of the two is still perfectly legal to remove.
        run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "c.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("one of two is legal");
        assert_eq!(tree.survivors(), vec!["a.bin"]);
    }

    #[test]
    fn marking_nothing_removes_nothing() {
        let tree = Tree::new(&[
            ("a.bin", b"content for the empty test"),
            ("b.bin", b"content for the empty test"),
        ]);
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("an empty selection is legal");
        assert_eq!(removed.files, 0);
        assert_eq!(tree.survivors(), vec!["a.bin", "b.bin"]);
    }

    #[test]
    fn the_guard_counts_a_pinned_copy_as_a_survivor() {
        let tree = Tree::new(&[
            ("a.bin", b"one deletable one pinned"),
            ("b.bin", b"one deletable one pinned"),
            ("c.bin", b"one deletable one pinned"),
        ]);
        tree.pin("c.bin");
        let effort = Effort::default();
        let scan = tree.scan(&effort);
        // Both freeable copies may go, because the pinned one still holds the
        // content. Marking all three is still refused.
        let removed = run_selected(
            &scan,
            scan.proven().expect("the bytes were compared"),
            &[locate(&scan, "a.bin"), locate(&scan, "b.bin")],
            true,
            &context(&tree, &effort),
            None,
        )
        .expect("two of three is a legal selection");
        assert_eq!(removed.files, 2);
        assert_eq!(tree.survivors(), vec!["c.bin"]);
    }

    /// Attested folder groups: the cache, not the bytes, settles a scan's
    /// folder pairs, and `--auto` has to compare them again before it moves.
    mod attested {
        use super::super::*;
        use super::Tree;
        use crate::cache::ScanCache;
        use crate::cloud::Cloud;
        use crate::device::{Profile, VolumeProbe, Wanted};
        use crate::effort::Effort;
        use crate::filters::Filters;
        use crate::folders;
        use crate::pipeline::find_duplicates;
        use crate::pulse::Pulse;
        use std::fs::File;
        use std::time::{Duration, SystemTime};

        /// Two identical folders with a big-enough file and a small one, each
        /// old enough that the cache does not treat it as racily clean.
        fn two_copies() -> Tree {
            let big = vec![0x5A_u8; 6000];
            let tree = Tree::new(&[
                ("Photos/a.bin", &big),
                ("Photos/n.txt", b"same note"),
                ("Photos copy/a.bin", &big),
                ("Photos copy/n.txt", b"same note"),
            ]);
            backdate(&tree.root);
            tree
        }

        fn backdate(root: &Path) {
            let past = SystemTime::now() - Duration::from_secs(60);
            for base in [
                root.to_path_buf(),
                root.join("Photos"),
                root.join("Photos copy"),
            ] {
                for name in std::fs::read_dir(&base)
                    .expect("a scratch folder")
                    .flatten()
                {
                    let path = name.path();
                    if path.is_file() {
                        File::open(&path)
                            .and_then(|file| file.set_modified(past))
                            .expect("an old mtime");
                    }
                }
            }
        }

        /// A scan, and the folder groups it finds, with `cache` learning and
        /// answering. Returns the scan with its folder groups attached.
        fn scan_with_folders(tree: &Tree, effort: &Effort, cache: &mut ScanCache) -> Scan {
            let mut scan = find_duplicates(
                &tree.root,
                Filters {
                    min_size: 1,
                    include_ext: None,
                    skip_ext: &[],
                    excludes: &[],
                    cloud: false,
                },
                &mut VolumeProbe::start(&tree.root, Some(1)),
                effort,
                Some(cache),
                Wanted::Flash,
                &Pulse::off(),
                &Cloud::default(),
                None,
            );
            let context = Compare {
                root: &tree.root,
                effort,
                profile: Profile::Flash,
                solid: true,
                learn: false,
            };
            let mut unreadable = Vec::new();
            let found = folders::find(
                &scan.records,
                scan.reported(),
                &context,
                false,
                Some(cache),
                &mut unreadable,
            );
            scan.attach_folders(found);
            scan
        }

        fn context<'a>(tree: &'a Tree, effort: &'a Effort) -> Compare<'a> {
            Compare {
                root: &tree.root,
                effort,
                profile: Profile::Flash,
                solid: true,
                learn: false,
            }
        }

        fn removal(rule: Rule) -> Removal {
            Removal {
                rule,
                // Nothing here may reach the real Trash: a test removes for good,
                // and only ever inside its own scratch tree.
                hard: true,
                assume_yes: true,
                cloud: false,
            }
        }

        /// Make a file's bytes different while keeping its size and its mtime,
        /// which is what defeats the cache's key.
        fn forge_keeping_size_and_mtime(path: &Path, body: &[u8]) {
            let before = std::fs::metadata(path).expect("the file");
            let mtime = before.modified().expect("an mtime");
            std::fs::write(path, body).expect("a rewrite");
            File::options()
                .write(true)
                .open(path)
                .and_then(|file| file.set_modified(mtime))
                .expect("the mtime put back");
        }

        #[test]
        fn a_folder_matched_from_the_cache_is_compared_again_and_then_moves() {
            let tree = two_copies();
            let cache_dir = tree.base.join("cache");
            let mut cache = ScanCache::in_directory(cache_dir.clone());
            let cold = Effort::default();
            scan_with_folders(&tree, &cold, &mut cache);
            let effort = Effort::default();
            let scan = scan_with_folders(&tree, &effort, &mut cache);
            let proven = scan.proven().expect("a proven scan");
            let found = proven.folders();
            assert_eq!(found.len(), 1, "one folder group");
            assert!(found[0].attested, "the warm scan settled it from the cache");
            assert_eq!(effort.bytes_read(), 0, "the warm scan read nothing");

            let check = Effort::default();
            let code = run_auto(
                &scan,
                proven,
                &removal(Rule::Shortest),
                &context(&tree, &check),
                Some(&mut cache),
            );
            assert_eq!(code, 0);
            assert!(check.bytes_read() > 0, "the move proved the group again");
            assert!(!tree.root.join("Photos copy").exists(), "the copy moved");
            assert_eq!(
                std::fs::read(tree.root.join("Photos/a.bin")).expect("kept"),
                vec![0x5A_u8; 6000]
            );
        }

        #[test]
        fn a_forged_file_refuses_its_folder_group_and_both_folders_stay() {
            let tree = two_copies();
            let mut cache = ScanCache::in_directory(tree.base.join("cache"));
            scan_with_folders(&tree, &Effort::default(), &mut cache);
            let effort = Effort::default();
            let scan = scan_with_folders(&tree, &effort, &mut cache);
            assert!(scan.proven().expect("proven").folders()[0].attested);

            // Same size, different bytes, and the mtime put back: the cache
            // still believes the old digest.
            forge_keeping_size_and_mtime(&tree.root.join("Photos copy/n.txt"), b"same notf");
            let check = Effort::default();
            let code = run_auto(
                &scan,
                scan.proven().expect("proven"),
                &removal(Rule::Shortest),
                &context(&tree, &check),
                Some(&mut cache),
            );
            assert_eq!(code, 0);
            assert!(
                tree.root.join("Photos copy/a.bin").exists(),
                "the copy stayed whole"
            );
            assert_eq!(
                std::fs::read(tree.root.join("Photos copy/n.txt")).expect("forged"),
                b"same notf"
            );
            assert!(
                tree.root.join("Photos/n.txt").exists(),
                "the kept folder stayed whole"
            );
            assert!(tree.root.join("Photos/a.bin").exists());
        }

        #[test]
        fn a_file_added_after_the_scan_blocks_its_move_through_run_auto() {
            // A proven group, not an attested one: the listing check at the move
            // is what catches the change, and the hook makes the change between
            // the plan and that check.
            let tree = two_copies();
            let effort = Effort::default();
            let mut cache = ScanCache::in_directory(tree.base.join("cache"));
            let scan = scan_with_folders(&tree, &effort, &mut cache);
            let late = tree.root.join("Photos copy/late.txt");
            BEFORE_MOVE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(|folder: &Path| {
                    std::fs::write(folder.join("late.txt"), b"arrived after the scan")
                        .expect("a file added in the window");
                }));
            });
            let code = run_auto(
                &scan,
                scan.proven().expect("proven"),
                &removal(Rule::Shortest),
                &context(&tree, &Effort::default()),
                None,
            );
            BEFORE_MOVE.with(|hook| hook.borrow_mut().take());
            assert_eq!(code, 0);
            assert!(late.exists(), "the folder that changed was not moved");
            assert!(tree.root.join("Photos copy/a.bin").exists());
            assert!(tree.root.join("Photos/a.bin").exists());
        }
    }

    /// The move policy, away from the real Trash: `move_to` takes its destination,
    /// so these tests never name a directory the user would see.
    mod move_policy {
        use super::super::*;
        use std::io::ErrorKind;
        use std::path::PathBuf;

        fn scratch(name: &str) -> PathBuf {
            let root = std::env::temp_dir().join(format!("m10-move-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("scratch");
            root
        }

        #[test]
        fn a_rename_that_fails_for_another_reason_leaves_the_file_in_place() {
            let root = scratch("file");
            let source = root.join("keep.bin");
            std::fs::write(&source, b"still here").expect("a file");
            // The destination's folder does not exist, so the rename fails with
            // ENOENT: not a volume boundary, so no copy may be made.
            let destination = root.join("no-such-folder/keep.bin");
            assert!(move_to(&source, &destination).is_err());
            assert_eq!(std::fs::read(&source).expect("the source"), b"still here");
            assert!(!destination.exists(), "nothing was copied");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_rename_that_fails_for_another_reason_leaves_the_folder_in_place() {
            let root = scratch("folder");
            let source = root.join("Photos");
            std::fs::create_dir_all(source.join("sub")).expect("a folder");
            std::fs::write(source.join("sub/a.bin"), b"inside").expect("a file");
            let destination = root.join("no-such-folder/Photos");
            assert!(move_to(&source, &destination).is_err());
            assert_eq!(
                std::fs::read(source.join("sub/a.bin")).expect("kept"),
                b"inside"
            );
            assert!(!destination.exists(), "no tree was copied");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn only_a_volume_boundary_is_a_reason_to_copy() {
            assert!(crosses_devices(&std::io::Error::from(
                ErrorKind::CrossesDevices
            )));
            // EXDEV on macOS is 18: the errno a rename gives across volumes.
            assert!(crosses_devices(&std::io::Error::from_raw_os_error(18)));
            // ENOENT, EACCES and EBUSY: none of them is a reason to copy.
            assert!(!crosses_devices(&std::io::Error::from_raw_os_error(2)));
            assert!(!crosses_devices(&std::io::Error::from_raw_os_error(13)));
            assert!(!crosses_devices(&std::io::Error::from_raw_os_error(16)));
        }
    }
}
