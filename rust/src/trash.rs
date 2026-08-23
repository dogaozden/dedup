//! Removal, and where removed files go.
//!
//! §3.4: files on an external drive go to that drive's `.Trashes/<uid>` rather
//! than the home Trash, so emptying the Trash does not mean copying gigabytes
//! back across a USB cable first. `--hard` is the only permanent deletion and
//! is opt-in.
//!
//! §3.2: a group always keeps one copy, enforced here rather than by whatever
//! is asking.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache::ScanCache;
use crate::compare::{verify_attested, Compare};
use crate::device::mount_point;
use crate::pipeline::{DupGroup, ProvenGroups, Scan};
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

    /// Which member survives. Returns a position within `members`.
    ///
    /// Python's `min` and `max` both return the FIRST extreme; Rust's
    /// `max_by_key` returns the LAST. Members arrive shortest-name-first, so an
    /// unadjusted `max` would keep the longest-named copy on a tie where the
    /// oracle keeps the shortest-named one. Reversing the comparator and taking
    /// the minimum gets the first maximum, which is what `dedup.py:2226` does.
    fn keeper(self, records: &[FileRec], members: &[usize]) -> usize {
        let time = |&(_, &index): &(usize, &usize)| oracle_mtime(&records[index]);
        match self {
            Rule::Shortest => members
                .iter()
                .enumerate()
                .min_by_key(|&(_, &index)| name_order(records[index].path()))
                .map(|(position, _)| position),
            Rule::Oldest => members
                .iter()
                .enumerate()
                .min_by(|left, right| time(left).total_cmp(&time(right)))
                .map(|(position, _)| position),
            Rule::Newest => members
                .iter()
                .enumerate()
                .min_by(|left, right| time(right).total_cmp(&time(left)))
                .map(|(position, _)| position),
        }
        .expect("a group always has members")
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
fn oracle_mtime(record: &FileRec) -> f64 {
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

/// Say plainly that some groups were not what the cache thought they were.
fn report_refusals(records: &[FileRec], refused: &[DupGroup]) {
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
