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
mod trash;
mod walk;

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use cache::ScanCache;
use compare::{verify_attested, Compare};
use consts::DEFAULT_MIN_SIZE;
use device::Device;
use effort::Effort;
use filters::{resolve_extensions, Filters};
use pipeline::{find_duplicates, report_names, size_stage, DupGroup, Scan};
use trash::{plan_auto, remove_record, Rule};
use walk::FileRec;

#[derive(Debug, Default, Serialize)]
struct Report {
    groups: Vec<Vec<String>>,
    bytes_read: u64,
    files_seen: u64,
    bytes_seen: u64,
    unreadable: Vec<String>,
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

fn print_report(groups: &[Vec<String>]) {
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
    let device = Device::probe(&root, options.jobs);
    let mut cache = (!options.no_cache && options.stage != "size").then(ScanCache::new);
    let scan = if options.stage == "size" {
        size_stage(&root, filters)
    } else {
        find_duplicates(&root, filters, device, &effort, cache.as_mut())
    };

    let mut report = Report {
        groups: report_names(&scan.records, &scan.groups),
        bytes_read: effort.bytes_read(),
        files_seen: scan.files_seen,
        bytes_seen: scan.bytes_seen,
        unreadable: scan.unreadable.clone(),
        opens: effort.opens(),
    };
    report.unreadable.sort();
    report.unreadable.dedup();

    let destination = options
        .report_json
        .clone()
        .unwrap_or_else(default_report_path);
    fs::write(destination, serde_json::to_vec_pretty(&report)?)?;

    if options.list {
        print_report(&report.groups);
    }

    // Deletion happens after the report is written, so the report always
    // describes the scan rather than the wreckage. The cache is still open on
    // purpose: a group it vouched for has to be read again before anything in
    // it goes.
    let code = match rule {
        Some(rule) => run_auto(
            &root,
            &scan,
            rule,
            &options,
            device,
            &effort,
            cache.as_mut(),
        ),
        None => 0,
    };
    if let Some(cache) = cache.as_mut() {
        cache.close();
    }
    Ok(code)
}

/// Non-interactive cleanup: keep one copy per group by `rule`, remove the rest.
fn run_auto(
    root: &Path,
    scan: &Scan,
    rule: Rule,
    options: &Options,
    device: Device,
    effort: &Effort,
    cache: Option<&mut ScanCache>,
) -> i32 {
    // Groups matched from the cache have not been read yet. They are read here,
    // before a plan is even printed, so the plan describes files that really
    // are identical rather than files a digest said were.
    let context = Compare {
        root,
        effort,
        solid: device.all_solid_state(),
        learn: false,
    };
    let groups = scan
        .groups
        .iter()
        .map(|group| DupGroup {
            members: group.members.clone(),
            attested: group.attested,
        })
        .collect();
    let (groups, refused) = verify_attested(&context, &scan.records, groups, cache);
    report_refusals(&scan.records, &refused);

    let doomed = plan_auto(&scan.records, &groups, rule);
    if doomed.is_empty() {
        println!("\n  Nothing to remove.\n");
        return 0;
    }

    let total: u64 = doomed.iter().map(|&index| scan.records[index].size).sum();
    let verb = if options.hard {
        "Delete permanently"
    } else {
        "Move to Trash"
    };
    println!(
        "\n  Keeping the {rule:?} copy in each of {} groups.",
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

    if !options.yes && !std::io::stdin().is_terminal() {
        println!();
        eprintln!("  Refusing to delete without confirmation. Re-run with -y to proceed.");
        return 1;
    }
    if !options.yes {
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
        match remove_record(root, record, options.hard) {
            Ok(()) => {
                done += record.names.len();
                freed += record.size;
            }
            Err(error) => failures.push(error),
        }
    }

    let where_to = if options.hard {
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
