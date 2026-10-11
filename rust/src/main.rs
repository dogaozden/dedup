//! dedup-rs — find byte-exact duplicate files.
//!
//! A port of the Python `dedup`, which stays authoritative for behaviour. The
//! conformance protocol (`--report-json`) is how the referee reads this
//! program: after any scan it writes the groups it found and, honestly, what
//! it read to find them.

mod cache;
mod cli;
mod cloud;
mod compare;
mod consts;
mod device;
mod effort;
mod filters;
mod folders;
mod live;
mod ordered;
mod pipeline;
mod pulse;
mod size;
mod stages;
mod sys;
mod trash;
mod tui;
mod walk;

use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use cache::ScanCache;
use cli::{help_text, parse_arguments_from, Options, Parsed};
use compare::Compare;
use device::{Device, VolumeProbe};
use effort::Effort;
use filters::{resolve_extensions, Filters};
use folders::FolderGroup;
use pipeline::{find_duplicates, report_names, size_stage};
use pulse::Pulse;
use trash::{run_auto, Removal, Rule};

/// The conformance protocol object. Paths are borrowed from the scan's records
/// rather than copied: a clone of every reported name is a per-file cost on top
/// of the records themselves, and REWRITE.md §6 counts per-file costs.
#[derive(Serialize)]
struct Report<'a> {
    groups: Vec<Vec<&'a str>>,
    bytes_read: u64,
    files_seen: u64,
    bytes_seen: u64,
    unreadable: &'a [String],
    opens: u64,
    /// Not part of the conformance protocol — the referee neither asks for it
    /// nor reads it. It is here because SPIN.md judges the `spinning` profile
    /// on it, and a number nobody can read out of a run is a number nobody can
    /// check.
    seeks: u64,
    /// Cloud placeholders this scan did not read, absent from the document
    /// entirely when there were none.
    ///
    /// Skipped rather than always-zero because the M8 fence, restated for M9,
    /// is that a tree with no dataless files produces byte-identical output to
    /// the build before this one — and a new key with `0` in it is not
    /// byte-identical. A referee that has never seen this field is a referee
    /// scanning a tree that has nothing to say about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_cloud: Option<u64>,
    /// Folder groups, each a list of folder paths, absent from the document
    /// when there are none. Same reasoning as `skipped_cloud`: a tree without a
    /// folder group must produce the bytes it always did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    folders: Vec<Vec<&'a str>>,
}

/// Whether the flags on this run ask for the browser, leaving aside whether
/// there is a terminal to browse in.
///
/// One rule, read in two places: the menu opens on it before the scan and the
/// browser opens on it after. Written twice they could drift, and a run that
/// stopped to be asked about its options and then never browsed would be the
/// result.
fn browsing_flags(options: &Options) -> bool {
    options.auto.is_none() && (options.interactive || !options.list)
}

/// Whether the pre-flight menu appears. A piped or flagged run never sees it
/// and is byte-for-byte what it has always been.
fn menu_wanted(options: &Options) -> bool {
    tui::has_terminal() && browsing_flags(options)
}

/// Whether the FLAGS on this run ask for the live path, leaving aside whether
/// there is a terminal to browse in.
///
/// Split out of `live_wanted` so it can be decided without a terminal, and so
/// tested. REVIEW-M8 item 3: every case in the freeze proof is a run with no
/// terminal, so `has_terminal()` is already false and the clauses behind it are
/// never evaluated — deleting two of them left `cargo test` at 250 passed and
/// the freeze proof blind. The milestone whose whole premise is one absolute
/// fence should not have half of that fence guarded by nothing.
///
/// The four exclusions, and what each is for:
///
/// * `--auto` is excluded by `browsing_flags`: a run that deletes without
///   asking has nobody to browse for.
/// * `--list` is excluded by `browsing_flags` ONLY when `--interactive` is
///   absent — the expression is `auto.is_none() && (interactive || !list)`.
///   `--list --interactive` on a terminal is an interactive run and takes the
///   live path, which is what `-i` has always meant and what `main` already
///   did with it. See the note in the M8 claim.
/// * An explicit `--report-json` keeps today's pipeline whatever else is true
///   of the run. That is the fence read literally, and it has a second effect
///   worth having: the conformance harness passes `--report-json` on every
///   invocation, so no gate can reach the live path even if one were somehow
///   run on a terminal.
/// * `--stage size` has read nothing, so it has nothing to stream, nothing to
///   prove and nothing anybody may delete.
fn live_flags(options: &Options) -> bool {
    browsing_flags(options) && options.report_json.is_none() && options.stage.is_empty()
}

/// Whether this run opens the browser before the scan rather than after it.
///
/// `terminal` is taken rather than asked for, on REVIEW-M8's non-binding note
/// 4. Asked for, it is false in every `cargo test` process, so an assertion
/// comparing this against `has_terminal() && live_flags(..)` was `false && …`
/// on both sides and held whatever this function did with the flags — the
/// reviewer dropped the `--report-json` and `--stage` exclusions back out of
/// it and all 258 tests stayed green. As a parameter both values are
/// reachable, and the test below runs each.
fn live_wanted(terminal: bool, options: &Options) -> bool {
    terminal && live_flags(options)
}

/// The `--list` view: folder groups first, then the file groups that are not
/// folded into one. Without folder groups it is exactly the list it always was.
fn print_report(folders: &[FolderGroup], groups: &[Vec<&str>]) {
    if folders.is_empty() && groups.is_empty() {
        println!("\n  No duplicates found.\n");
        return;
    }
    match (folders.is_empty(), groups.is_empty()) {
        (true, _) => println!("\n  {} duplicate group(s)\n", groups.len()),
        (false, true) => println!("\n  {} folder group(s)\n", folders.len()),
        (false, false) => println!(
            "\n  {} folder group(s) and {} file group(s)\n",
            folders.len(),
            groups.len()
        ),
    }
    for group in folders {
        for (position, folder) in group.folders.iter().enumerate() {
            let marker = if position == 0 { "keep" } else { "    " };
            if position == 0 {
                // The browser's word for a group proved at apply, not before. Only
                // folder groups are labelled here: file groups are not, and a
                // label on them would change every warm `--list`.
                let candidate = if group.attested { "  candidate" } else { "" };
                println!(
                    "    {marker} {}/   ({} files, {} each copy){candidate}",
                    folder.path,
                    group.files(),
                    size::human_size(group.bytes())
                );
            } else {
                println!("    {marker} {}/", folder.path);
            }
        }
        println!();
    }
    for group in groups {
        for (position, name) in group.iter().enumerate() {
            println!("    {} {name}", if position == 0 { "keep" } else { "    " });
        }
        println!();
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut options = match parse_arguments_from(arguments.iter().cloned())? {
        Parsed::Help => {
            print!("{}", help_text());
            return Ok(0);
        }
        Parsed::Scan(options) => *options,
    };
    if !options.root.is_dir() {
        eprintln!("dedup-rs: not a directory: {}", options.root.display());
        return Ok(2);
    }

    // The pre-flight menu, before anything has been read. What it hands back
    // is a command line, and it goes through the same parser and the same
    // checks the real one did — the form has no way to say anything the flags
    // cannot say.
    if menu_wanted(&options) {
        match tui::menu::ask(&arguments)? {
            tui::menu::Answer::Quit => return Ok(0),
            // The same ending the browser gives the same event, and for the
            // same reason: nothing was done and nobody is there to be told.
            tui::menu::Answer::TerminalGone => return Ok(1),
            tui::menu::Answer::Scan(chosen) => match parse_arguments_from(chosen.into_iter())? {
                Parsed::Scan(chosen) => options = *chosen,
                // The form emits no --help, so this cannot happen; refusing to
                // scan is the safe reading of it if it ever did.
                Parsed::Help => return Ok(2),
            },
        }
    }
    let root = fs::canonicalize(&options.root)?;

    let (include_ext, skip_ext) = match resolve_extensions(&options.ext, &options.skip_ext) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("dedup-rs: {error}");
            return Ok(2);
        }
    };
    let filters = Filters {
        min_size: options.min_size,
        include_ext: include_ext.as_deref(),
        skip_ext: &skip_ext,
        excludes: &options.excludes,
        cloud: options.cloud,
    };

    // Defence in depth, and the second half of M9-CLOUD.md's law. The walk
    // drops dataless files before they become records; this makes the kernel
    // refuse to fetch one that got past it anyway — a file EVICTED to the
    // cloud after the walk saw its bytes cannot be caught by any amount of
    // walk-time care, and reading it would start a download.
    //
    // Set here, after the menu (which may have turned `--cloud` on or off) and
    // before the first byte is read. Not lifted under `--cloud`, but never
    // installed: a run that asked to compare cloud files has asked for the
    // downloads, and overriding a policy the environment set for its own
    // reasons is not this tool's business.
    let refusing = cloud::install_refusal(options.cloud);
    let seen_in_cloud = Arc::new(cloud::Cloud::default());

    let rule = match options.auto.as_deref().map(Rule::parse) {
        Some(None) => {
            eprintln!("dedup-rs: --auto takes shortest, oldest or newest");
            return Ok(2);
        }
        other => other.flatten(),
    };

    // Shared so the pulse's ticker can read the same counters the readers are
    // writing: the rate on the line is bytes that really moved, not an
    // estimate kept beside them.
    let effort = Arc::new(Effort::default());

    // The live scan: a browser opened before anything has been read, with the
    // scan behind it. Everything below this point is the path every other run
    // has always taken, unchanged.
    if live_wanted(tui::has_terminal(), &options) {
        return live_scan(&options, &root, filters, &effort, &seen_in_cloud);
    }
    // After the menu, which owns the terminal until it hands it back, and
    // before anything is read.
    let mut pulse = Pulse::start(options.quiet, &effort);

    // Started rather than answered: what kind of disk this is takes a
    // subprocess to find out, and nothing needs it until the pools are sized.
    let mut probe = VolumeProbe::start(&root, options.jobs);
    let mut cache = (!options.no_cache && options.stage != "size").then(|| {
        if options.force {
            ScanCache::forced()
        } else {
            ScanCache::new()
        }
    });
    let mut scan = if options.stage == "size" {
        size_stage(&root, filters, &pulse, &seen_in_cloud)
    } else {
        find_duplicates(
            &root,
            filters,
            &mut probe,
            &effort,
            cache.as_mut(),
            options.profile,
            &pulse,
            &seen_in_cloud,
            // Not live: this is the path every non-interactive run takes.
            None,
        )
    };
    // The line goes before anything else is said, so nothing of it is left in
    // the scrollback and nothing written after it lands on top of it.
    pulse.finish();
    if let Some(hint) = scan.hint {
        eprintln!("{hint}");
    }
    // Identical folders, found from the file groups this scan just proved. They
    // are looked for only after the proof, and only when two files shared a
    // group across different folders. `--no-folders` skips the whole pass, and
    // the live path never reaches here.
    if !options.no_folders && !scan.reported().is_empty() && scan.proven().is_some() {
        let device = probe.device();
        let context = Compare {
            root: &root,
            effort: &effort,
            profile: scan.profile,
            solid: device.all_solid_state(),
            learn: false,
        };
        let mut folder_unreadable = Vec::new();
        let found = folders::find(
            &scan.records,
            scan.reported(),
            &context,
            options.cloud,
            cache.as_mut(),
            &mut folder_unreadable,
        );
        scan.unreadable.extend(folder_unreadable);
        scan.attach_folders(found, options.cloud);
    }
    scan.unreadable.sort();
    scan.unreadable.dedup();
    // Which of the files that would not read live in the cloud. Asked now,
    // of the files that actually failed, rather than tracked through every
    // read path: the flag is the fact, and a file that was evicted while a
    // stage was reading it still carries it. On the ordinary run this list is
    // empty and the loop is over an empty vector.
    seen_in_cloud.evicted(cloud::in_cloud(&scan.unreadable, |name| {
        cloud::flags_of(&root.join(name))
    }));
    let skipped_cloud = seen_in_cloud.skipped();
    if let Some(note) = cloud::evicted_notice(&seen_in_cloud.evicted_names(), options.cloud) {
        eprintln!("{note}");
    }
    if let Some(note) = cloud::notice(skipped_cloud, options.cloud) {
        eprintln!("  {note}");
        if !options.cloud && !refusing {
            // Said only where there is already something to say, so a tree
            // with no cloud files in it stays byte-identical whatever this
            // machine thinks of the policy.
            eprintln!(
                "  (this system would not hold the no-materialize I/O policy; \
                 the walk-time skip was the only guard on this run)"
            );
        }
    }

    let report = Report {
        groups: report_names(&scan.records, scan.reported()),
        bytes_read: effort.bytes_read(),
        files_seen: scan.files_seen,
        bytes_seen: scan.bytes_seen,
        unreadable: &scan.unreadable,
        opens: effort.opens(),
        seeks: effort.seeks(),
        skipped_cloud: (skipped_cloud > 0).then_some(skipped_cloud),
        folders: scan
            .folder_groups()
            .iter()
            .map(|group| {
                group
                    .folders
                    .iter()
                    .map(|folder| folder.path.as_str())
                    .collect()
            })
            .collect(),
    };

    let destination = options
        .report_json
        .clone()
        .unwrap_or_else(default_report_path);
    // Straight to the file: building the whole document in memory first costs
    // another copy of every reported path.
    serde_json::to_writer_pretty(BufWriter::new(fs::File::create(destination)?), &report)?;

    // A pipe with neither --list nor --auto still gets the report: the oracle
    // prints one when there is no terminal to browse in, and a run that reads
    // a whole tree and says nothing looks like a hang.
    if options.list || (rule.is_none() && !options.interactive && !tui::has_terminal()) {
        let visible: Vec<Vec<&str>> = report
            .groups
            .iter()
            .enumerate()
            .filter(|(number, _)| !scan.is_folded(*number))
            .map(|(_, group)| group.clone())
            .collect();
        print_report(scan.folder_groups(), &visible);
    }

    // Deletion happens after the report is written, so the report always
    // describes the scan rather than the wreckage. The cache is still open on
    // purpose: a group it vouched for has to be read again before anything in
    // it goes.
    let device = probe.device();
    let code = match rule {
        None => browse(&options, &scan, &root, &effort, device, cache.as_mut()),
        Some(rule) => match scan.proven() {
            Some(proven) => {
                let context = Compare {
                    root: &root,
                    effort: &effort,
                    profile: scan.profile,
                    solid: device.all_solid_state(),
                    learn: false,
                };
                let removal = Removal {
                    rule,
                    hard: options.hard,
                    assume_yes: options.yes,
                    cloud: options.cloud,
                };
                run_auto(&scan, proven, &removal, &context, cache.as_mut())
            }
            None => {
                eprintln!(
                    "dedup-rs: --auto cannot act on a --stage size scan. Size buckets are \
                     files that merely share a length; nothing has read their contents, so \
                     removing one would not be removing a duplicate. Re-run without --stage."
                );
                2
            }
        },
    };
    if let Some(cache) = cache.as_mut() {
        cache.close();
    }
    Ok(code)
}

/// The live scan: the browser opens now, the scan runs behind it.
///
/// The two threads are scoped rather than detached so every borrow they share
/// — the root, the filters, the counters, the cache — is checked rather than
/// cloned. The scan thread owns the records and does all the reading; the
/// browser thread owns the terminal and does none.
fn live_scan(
    options: &Options,
    root: &std::path::Path,
    filters: Filters,
    effort: &Arc<Effort>,
    cloud: &Arc<cloud::Cloud>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let (live, watch) = live::channel(root, options.cloud);
    // Counters without a painter: in live mode the browser is the progress
    // display, so nothing may write to stderr behind the alternate screen.
    let (pulse, seen) = Pulse::live(effort, cloud);
    let mut probe = VolumeProbe::start(root, options.jobs);
    let mut cache = (!options.no_cache).then(|| {
        if options.force {
            ScanCache::forced()
        } else {
            ScanCache::new()
        }
    });

    let code = std::thread::scope(|scope| {
        // The borrows the scan needs are taken here and MOVED into it, rather
        // than being captured by reference. `Live` holds a `RefCell` and so is
        // deliberately not `Sync`: only one thread may ever publish, and this
        // is how the compiler is told which one.
        let probing = &mut probe;
        let caching = cache.as_mut();
        let scanning = scope.spawn(move || {
            // Whatever happens in here — a clean finish, or a panic on the way
            // through — the browser is released rather than left blocked on a
            // thread that has stopped running.
            let _leaving = live.leaving();
            let mut caching = caching;
            let mut scan = find_duplicates(
                root,
                filters,
                probing,
                effort,
                caching.as_deref_mut(),
                options.profile,
                &pulse,
                cloud,
                Some(&live),
            );
            // Folder candidates are looked for once the file groups have settled,
            // and they arrive as candidates: nothing here reads a file in full.
            if !options.no_folders && !live.cancelled() {
                live.publish_folders(&scan.records);
            }
            // The list is complete; the progress display goes away. The thread
            // stays to answer, because a user who waited for the scan to
            // finish before marking anything is the ordinary case.
            live.finished();
            let context = Compare {
                root,
                effort,
                profile: scan.profile,
                solid: probing.device().all_solid_state(),
                learn: caching.is_some(),
            };
            while !live.cancelled() {
                if !pipeline::serve(
                    &live,
                    &mut scan.records,
                    &context,
                    caching.as_deref_mut(),
                    &mut scan.unreadable,
                ) {
                    live.idle(std::time::Duration::from_millis(100));
                }
            }
        });
        // Whatever the browser does — return, error, or panic on its way out —
        // the scan is told to stop. It has to be a guard rather than the call
        // below it: a panic here unwinds straight into this scope's join, and
        // the scan thread it would be joining is looping on `!cancelled()`.
        // The plain `watch.cancel()` that used to stand after `run_live` was
        // skipped by exactly that unwind, and the process hung instead of
        // dying (REVIEW-M8 item 1).
        let stopping = watch.stopping();
        let code = tui::run_live(&watch, &seen, root, options.hard, options.cloud);
        // ...and on the ordinary path the scan is stopped and joined before
        // the cache is closed, so a half-written row is impossible: the only
        // thread that writes rows has stopped running.
        drop(stopping);
        let _ = scanning.join();
        code
    });

    if let Some(cache) = cache.as_mut() {
        cache.close();
    }
    Ok(code)
}

/// Open the browser, if the browser is what this run is.
///
/// The oracle browses whenever a terminal is there and neither `--list` nor
/// `--auto` was given, so this does too. `-i` says it outright, and then a pipe
/// is an error instead of a silent fall-through to the report — the difference
/// matters for a script that meant to be interactive.
fn browse(
    options: &Options,
    scan: &pipeline::Scan,
    root: &std::path::Path,
    effort: &Effort,
    device: Device,
    cache: Option<&mut ScanCache>,
) -> i32 {
    if !tui::has_terminal() {
        if options.interactive {
            eprintln!("{}", tui::NO_TERMINAL);
            return 2;
        }
        return 0;
    }
    if !browsing_flags(options) {
        // --list already printed everything the run was asked for. (`--auto`
        // never reaches here: the caller took the other branch.)
        return 0;
    }
    if scan.reported().is_empty() {
        if !options.list {
            println!("\n  No duplicates found.\n");
        }
        return 0;
    }
    // No token, no browser: the same guard `--auto` passes through.
    let Some(proven) = scan.proven() else {
        eprintln!(
            "dedup-rs: the browser cannot act on a --stage size scan. Size buckets are files \
             that merely share a length; nothing has read their contents. Re-run without --stage."
        );
        return 2;
    };
    let context = Compare {
        root,
        effort,
        profile: scan.profile,
        solid: device.all_solid_state(),
        learn: false,
    };
    tui::run(scan, proven, root, options.hard, &context, cache)
}

fn default_report_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push("dedup-rs-report.json");
    path
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("dedup-rs: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_default_report_path_is_a_file_not_a_directory() {
        assert!(default_report_path().file_name().is_some());
        assert!(default_report_path().starts_with(Path::new(&std::env::temp_dir())));
    }

    /// Options as a command line would build them, so a case here is a case
    /// somebody could type.
    fn flags(argv: &[&str]) -> Options {
        let mut given: Vec<String> = vec!["/scratch".to_string()];
        given.extend(argv.iter().map(|word| word.to_string()));
        match parse_arguments_from(given.into_iter()).expect("a valid command line") {
            Parsed::Scan(options) => *options,
            Parsed::Help => panic!("the fixture asked for help"),
        }
    }

    #[test]
    fn the_fence_decides_the_live_path_by_flags_alone() {
        // REVIEW-M8 item 3. This is the milestone's one absolute fence, and
        // until this test existed two of its four clauses were guarded by
        // nothing at any level: the freeze proof is 84 non-interactive cases,
        // so `has_terminal()` short-circuits before the flags are ever looked
        // at, and deleting `report_json.is_none()` and `stage.is_empty()` left
        // the whole suite green.
        //
        // Every clause is pinned by at least one case that flips when it goes.
        let cases: [(&[&str], bool, &str); 9] = [
            (&[], true, "a bare interactive run is the live path"),
            (
                &["--report-json", "/tmp/r.json"],
                false,
                "an explicit --report-json keeps today's pipeline — this is \
                 the clause every conformance invocation relies on",
            ),
            (
                &["--stage", "size"],
                false,
                "--stage size has read nothing, so there is nothing to stream \
                 and nothing anybody may delete",
            ),
            (
                &["--auto", "shortest"],
                false,
                "--auto has nobody to browse for",
            ),
            (&["--list"], false, "--list prints and stops"),
            (
                &["--list", "--interactive"],
                true,
                "-i means browse them, and it has always overruled --list; \
                 this row is the behaviour REVIEW-M8 item 4 records",
            ),
            (&["--interactive"], true, "-i on its own is the live path"),
            (
                &["--auto", "shortest", "--interactive"],
                false,
                "--auto wins over -i: there is nothing to browse afterwards",
            ),
            (
                &["--quiet", "--hard"],
                true,
                "flags that say nothing about browsing do not change the path",
            ),
        ];
        for (argv, wanted, why) in cases {
            assert_eq!(
                live_flags(&flags(argv)),
                wanted,
                "dedup-rs /scratch {}: {why}",
                argv.join(" ")
            );
        }
    }

    #[test]
    fn the_live_path_is_the_flags_and_a_terminal_and_nothing_else() {
        // `live_wanted` may add the terminal to `live_flags` and may not add
        // anything else — otherwise the test above would be pinning a
        // different function from the one that decides.
        //
        // REVIEW-M8's non-binding note 4: this test used to ask
        // `has_terminal()` for the terminal half, which is false in every
        // `cargo test` process, so both sides read `false && …` and the
        // assertion could not fail. Both values are supplied now, and it is
        // the `terminal = true` half that does the work — with no terminal
        // every row below is false on both sides no matter what the flags say.
        for terminal in [false, true] {
            for argv in [
                &[][..],
                &["--list"],
                &["--list", "--interactive"],
                &["--report-json", "/tmp/r.json"],
                &["--stage", "size"],
                &["--auto", "shortest"],
            ] {
                let options = flags(argv);
                assert_eq!(
                    live_wanted(terminal, &options),
                    terminal && live_flags(&options),
                    "terminal={terminal} {argv:?}"
                );
            }
        }
    }

    #[test]
    fn the_menu_opens_for_exactly_the_runs_the_browser_opens_for() {
        // Every combination of the three flags that decide it. The menu is the
        // browser's front door, so a run that is asked about its options and
        // then does not browse — or browses without being asked — is the bug
        // this pins.
        for list in [false, true] {
            for interactive in [false, true] {
                for auto in [None, Some("shortest".to_string())] {
                    let options = Options {
                        list,
                        interactive,
                        auto: auto.clone(),
                        ..Options::new()
                    };
                    let by_flags = browsing_flags(&options);
                    let wanted = auto.is_none() && (interactive || !list);
                    assert_eq!(
                        by_flags, wanted,
                        "list={list} interactive={interactive} auto={auto:?}"
                    );
                    // ...and the menu adds the terminal and nothing else.
                    assert_eq!(
                        menu_wanted(&options),
                        tui::has_terminal() && by_flags,
                        "list={list} interactive={interactive} auto={auto:?}"
                    );
                }
            }
        }
    }
}
