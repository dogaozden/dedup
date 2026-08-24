//! Removal, and where removed files go.
//!
//! §3.4: files on an external drive go to that drive's `.Trashes/<uid>` rather
//! than the home Trash, so emptying the Trash does not mean copying gigabytes
//! back across a USB cable first. `--hard` is the only permanent deletion and
//! is opt-in.
//!
//! §3.2: a group always keeps one copy, enforced here rather than by whatever
//! is asking.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::ScanCache;
use crate::compare::{verify_attested, Compare};
use crate::device::mount_point;
use crate::pipeline::{DupGroup, ProvenGroups, Scan};
use crate::tui::model::Mark;
use crate::walk::{name_order, FileRec};

/// What a non-interactive run was asked to do.
pub struct Removal {
    pub rule: Rule,
    pub hard: bool,
    pub assume_yes: bool,
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

    fn keeper(self, records: &[FileRec], members: &[usize]) -> usize {
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

/// Pick one keeper per group and return the records to remove.
///
/// The survivor guard is this function: the keeper is never added to the list,
/// so no caller can arrange for a group to lose every copy. A record the scan
/// cannot free — because a name outside the tree still pins the inode — is
/// never a victim however duplicate it is, since removing what we can see
/// would free nothing.
fn plan_auto(records: &[FileRec], groups: &[DupGroup], rule: Rule) -> Vec<usize> {
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
    cache: Option<&mut ScanCache>,
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
    let (groups, refused) = verify_attested(context, &scan.records, candidates, cache);
    report_refusals(&scan.records, &refused);

    let doomed = plan_auto(&scan.records, &groups, removal.rule);
    if doomed.is_empty() {
        println!("\n  Nothing to remove.\n");
        return 0;
    }

    let total: u64 = doomed.iter().map(|&index| scan.records[index].size).sum();
    let verb = if removal.hard {
        "Delete permanently"
    } else {
        "Move to Trash"
    };
    println!(
        "\n  Keeping the {} copy in each of {} groups.",
        removal.rule,
        groups.len()
    );
    println!(
        "  {verb}: {} files · {total} bytes reclaimed\n",
        doomed.len()
    );
    for &index in doomed.iter().take(15) {
        println!("      {}", scan.records[index].path());
    }
    if doomed.len() > 15 {
        println!("      … and {} more", doomed.len() - 15);
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

    let (mut done, mut freed) = (0_usize, 0_u64);
    let mut failures = Vec::new();
    for &index in &doomed {
        let record = &scan.records[index];
        match remove_record(context.root, record, removal.hard) {
            Ok(()) => {
                done += record.names.len();
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
    println!("\n  {done} files {where_to} · {freed} bytes reclaimed");
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

/// Whether any name this record wears is still on disk.
fn still_there(root: &Path, record: &FileRec) -> bool {
    record
        .names
        .iter()
        .any(|name| root.join(name).symlink_metadata().is_ok())
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
    for (&group, chosen) in wanted.iter_mut() {
        chosen.sort_unstable();
        chosen.dedup();
        let survivors = groups[group]
            .members
            .iter()
            .filter(|index| !chosen.contains(index))
            .filter(|&&index| still_there(context.root, &scan.records[index]))
            .count();
        if survivors == 0 {
            return Err(format!(
                "refusing the whole selection: it would leave group {} with no copy at all",
                group + 1
            ));
        }
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
    struct Tree {
        base: PathBuf,
        root: PathBuf,
    }

    impl Tree {
        fn new(files: &[(&str, &[u8])]) -> Tree {
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

        fn write(&self, name: &str, content: &[u8]) {
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

        fn scan(&self, effort: &Effort) -> Scan {
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
                },
                &mut crate::device::VolumeProbe::start(&self.root, Some(1)),
                effort,
                None,
                wanted,
                &crate::pulse::Pulse::off(),
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
}
