//! dedup-rs — find byte-exact duplicate files.
//!
//! A port of the Python `dedup`, which stays authoritative for behaviour. The
//! conformance protocol (`--report-json`) is how the referee reads this
//! program: after any scan it writes the groups it found and, honestly, what
//! it read to find them.

mod cache;
mod compare;
mod consts;
mod device;
mod effort;
mod filters;
mod ordered;
mod pipeline;
mod stages;
mod sys;
mod trash;
mod tui;
mod walk;

use std::fs;
use std::io::BufWriter;
use std::path::PathBuf;

use serde::Serialize;

use cache::ScanCache;
use compare::Compare;
use consts::DEFAULT_MIN_SIZE;
use device::{Device, VolumeProbe};
use effort::Effort;
use filters::{resolve_extensions, Filters};
use pipeline::{find_duplicates, report_names, size_stage};
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
}

#[derive(Default)]
struct Options {
    root: PathBuf,
    report_json: Option<PathBuf>,
    min_size: u64,
    jobs: Option<usize>,
    excludes: Vec<String>,
    ext: Vec<String>,
    skip_ext: Vec<String>,
    list: bool,
    /// Ask for the browser outright. Without it the browser is what a terminal
    /// gets when nothing else was asked for, which is the oracle's behaviour;
    /// with it, no terminal is an error rather than a silent fallback.
    interactive: bool,
    auto: Option<String>,
    yes: bool,
    hard: bool,
    no_cache: bool,
    /// "size" stops after the walk and size grouping. Only the M1 gate asks
    /// for it; every other gate leaves it empty.
    stage: String,
}

const KEEP_RULES: [&str; 3] = ["shortest", "oldest", "newest"];

fn parse_arguments() -> Result<Options, Box<dyn std::error::Error>> {
    let mut options = Options {
        min_size: DEFAULT_MIN_SIZE,
        ..Options::default()
    };
    let mut root = None;
    let mut arguments = std::env::args().skip(1).peekable();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--report-json" | "--min-size" | "-j" | "--jobs" | "-x" | "--exclude" | "--ext"
            | "--skip-ext" | "--stage" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("{argument} needs a value"))?;
                match argument.as_str() {
                    "--report-json" => options.report_json = Some(PathBuf::from(value)),
                    "--min-size" => options.min_size = value.parse()?,
                    "-j" | "--jobs" => options.jobs = Some(value.parse()?),
                    "-x" | "--exclude" => options.excludes.push(value),
                    "--ext" => options.ext.push(value),
                    "--stage" => options.stage = value,
                    _ => options.skip_ext.push(value),
                }
            }
            "--auto" => {
                // The rule is optional, so a following word is only consumed
                // when it is one of the rules rather than the next flag.
                let named = arguments
                    .peek()
                    .is_some_and(|next| KEEP_RULES.contains(&next.as_str()));
                options.auto = Some(if named {
                    arguments.next().expect("peeked a rule")
                } else {
                    "shortest".to_string()
                });
            }
            "-l" | "--list" => options.list = true,
            "-i" | "--interactive" => options.interactive = true,
            "-y" | "--yes" => options.yes = true,
            "--hard" => options.hard = true,
            "--no-cache" => options.no_cache = true,
            "-q" | "--quiet" | "--verify" => {}
            other if root.is_none() && !other.starts_with('-') => {
                root = Some(PathBuf::from(other));
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    options.root = root.ok_or("usage: dedup-rs <root> [options]")?;
    Ok(options)
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
    let options = parse_arguments()?;
    if !options.root.is_dir() {
        eprintln!("dedup-rs: not a directory: {}", options.root.display());
        return Ok(2);
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

    let effort = Effort::default();
    // Started rather than answered: what kind of disk this is takes a
    // subprocess to find out, and nothing needs it until the pools are sized.
    let mut probe = VolumeProbe::start(&root, options.jobs);
    let mut cache = (!options.no_cache && options.stage != "size").then(ScanCache::new);
    let mut scan = if options.stage == "size" {
        size_stage(&root, filters)
    } else {
        find_duplicates(&root, filters, &mut probe, &effort, cache.as_mut())
    };
    scan.unreadable.sort();
    scan.unreadable.dedup();

    let report = Report {
        groups: report_names(&scan.records, scan.reported()),
        bytes_read: effort.bytes_read(),
        files_seen: scan.files_seen,
        bytes_seen: scan.bytes_seen,
        unreadable: &scan.unreadable,
        opens: effort.opens(),
    };

    let destination = options
        .report_json
        .clone()
        .unwrap_or_else(default_report_path);
    // Straight to the file: building the whole document in memory first costs
    // another copy of every reported path.
    serde_json::to_writer_pretty(BufWriter::new(fs::File::create(destination)?), &report)?;

    if options.list {
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
    if !options.interactive && options.list {
        return 0; // --list already printed everything the run was asked for
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
}
