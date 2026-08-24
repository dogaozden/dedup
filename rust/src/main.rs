//! dedup-rs — find byte-exact duplicate files.
//!
//! A port of the Python `dedup`, which stays authoritative for behaviour. The
//! conformance protocol (`--report-json`) is how the referee reads this
//! program: after any scan it writes the groups it found and, honestly, what
//! it read to find them.

mod cache;
mod cli;
mod compare;
mod consts;
mod device;
mod effort;
mod filters;
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

fn print_report(groups: &[Vec<&str>]) {
    if groups.is_empty() {
        println!("\n  No duplicates found.\n");
        return;
    }
    println!("\n  {} duplicate group(s)\n", groups.len());
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
    };

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
    // After the menu, which owns the terminal until it hands it back, and
    // before anything is read.
    let mut pulse = Pulse::start(options.quiet, &effort);

    // Started rather than answered: what kind of disk this is takes a
    // subprocess to find out, and nothing needs it until the pools are sized.
    let mut probe = VolumeProbe::start(&root, options.jobs);
    let mut cache = (!options.no_cache && options.stage != "size").then(ScanCache::new);
    let mut scan = if options.stage == "size" {
        size_stage(&root, filters, &pulse)
    } else {
        find_duplicates(
            &root,
            filters,
            &mut probe,
            &effort,
            cache.as_mut(),
            options.profile,
            &pulse,
        )
    };
    // The line goes before anything else is said, so nothing of it is left in
    // the scrollback and nothing written after it lands on top of it.
    pulse.finish();
    if let Some(hint) = scan.hint {
        eprintln!("{hint}");
    }
    scan.unreadable.sort();
    scan.unreadable.dedup();

    let report = Report {
        groups: report_names(&scan.records, scan.reported()),
        bytes_read: effort.bytes_read(),
        files_seen: scan.files_seen,
        bytes_seen: scan.bytes_seen,
        unreadable: &scan.unreadable,
        opens: effort.opens(),
        seeks: effort.seeks(),
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
        print_report(&report.groups);
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
