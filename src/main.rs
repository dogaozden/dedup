use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

const DEFAULT_MIN_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct Filters<'a> {
    min_size: u64,
    include_ext: Option<&'a [String]>,
    skip_ext: &'a [String],
    excludes: &'a [String],
}

#[derive(Debug)]
struct Entry {
    relative: String,
    size: u64,
    dev: u64,
    ino: u64,
}

#[derive(Default, Serialize)]
struct Report {
    groups: Vec<Vec<String>>,
    bytes_read: u64,
    files_seen: u64,
    bytes_seen: u64,
    unreadable: Vec<String>,
    opens: u64,
}

fn normalize_extension(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('.');
    if value.is_empty() {
        None
    } else {
        Some(value.to_lowercase())
    }
}

fn file_extension(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_lowercase(),
        _ => String::new(),
    }
}

fn extension_allowed(name: &str, filters: &Filters) -> bool {
    let extension = file_extension(name);
    if let Some(include) = filters.include_ext {
        if !include.iter().any(|allowed| allowed == &extension) {
            return false;
        }
    }
    !filters.skip_ext.iter().any(|skipped| skipped == &extension)
}

fn excluded(relative: &str, filters: &Filters) -> bool {
    let path = Path::new(relative);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    filters.excludes.iter().any(|pattern| {
        glob_match(pattern, name)
            || path.parent().is_some_and(|directory| {
                directory.components().any(|component| {
                    glob_match(pattern, component.as_os_str().to_str().unwrap_or_default())
                })
            })
    })
}

fn glob_match(pattern: &str, text: &str) -> bool {
    fn inner(pattern: &[char], text: &[char]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                for index in 0..=text.len() {
                    if inner(&pattern[1..], &text[index..]) {
                        return true;
                    }
                }
                false
            }
            (Some('?'), Some(_)) => inner(&pattern[1..], &text[1..]),
            (Some(&expected), Some(&actual)) => {
                expected == actual && inner(&pattern[1..], &text[1..])
            }
            _ => false,
        }
    }
    inner(
        &pattern.chars().collect::<Vec<_>>(),
        &text.chars().collect::<Vec<_>>(),
    )
}

fn collect_extensions(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .filter_map(normalize_extension)
        .collect()
}

fn walk(root: &Path, filters: Filters, report: &mut Report) -> Vec<Entry> {
    let mut survivors = Vec::new();
    let mut directories = vec![root.to_path_buf()];

    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                push_unreadable_directory(report, root, &directory, error);
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let path = directory.join("<unreadable-directory-entry>");
                    push_unreadable_path(report, root, &path, error);
                    continue;
                }
            };
            let path = entry.path();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    push_unreadable_path(report, root, &path, error);
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    push_unreadable_path(report, root, &path, error);
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                directories.push(path.clone());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let relative = match path.strip_prefix(root) {
                Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
                Err(_) => path.to_string_lossy().replace('\\', "/"),
            };
            if excluded(&relative, &filters) || !extension_allowed(&relative, &filters) {
                continue;
            }

            report.files_seen += 1;
            report.bytes_seen += metadata.len();
            if metadata.len() < filters.min_size {
                continue;
            }
            survivors.push(Entry {
                relative,
                size: metadata.len(),
                dev: metadata.dev(),
                ino: metadata.ino(),
            });
        }
    }

    survivors
}

fn push_unreadable_directory(report: &mut Report, root: &Path, path: &Path, _: io::Error) {
    report.unreadable.push(relative_or_absolute(root, path));
}

fn push_unreadable_path(report: &mut Report, root: &Path, path: &Path, _: io::Error) {
    report.unreadable.push(relative_or_absolute(root, path));
}

fn relative_or_absolute(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn canonical_name(names: &[&str]) -> String {
    names
        .iter()
        .min_by_key(|name| (name.chars().count(), *name))
        .map(ToString::to_string)
        .expect("hardlink record always has a name")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters<'a>(excludes: &'a [String], skip_ext: &'a [String]) -> Filters<'a> {
        Filters {
            min_size: 1,
            include_ext: None,
            skip_ext,
            excludes,
        }
    }

    #[test]
    fn extension_matching_uses_final_suffix_and_dotfiles_have_none() {
        assert_eq!(file_extension("photo.JPG"), "jpg");
        assert_eq!(file_extension(".hidden"), "");
        assert_eq!(file_extension("archive.tar.gz"), "gz");
    }

    #[test]
    fn glob_matches_basename_only() {
        let empty = Vec::new();
        let patterns = vec!["*.tmp".to_string()];
        let name_filters = filters(&patterns, &empty);
        assert!(excluded("backup/old.tmp", &name_filters));
        assert!(excluded("backup.tmp/file", &name_filters));
        let directory_patterns = vec!["backup".to_string()];
        assert!(excluded(
            "backup/old.tmp",
            &filters(&directory_patterns, &empty)
        ));
    }

    #[test]
    fn exclude_matches_any_directory_component() {
        let empty = Vec::new();
        let patterns = vec!["backup".to_string()];
        let filters = filters(&patterns, &empty);
        assert!(excluded("backup/report.bin", &filters));
        assert!(excluded("nested/backup/report.bin", &filters));
    }

    #[test]
    fn canonical_hardlink_names_are_shortest_then_lexicographic() {
        assert_eq!(canonical_name(&["copies/x", "x", "another"]), "x");
    }
}

fn size_groups(entries: Vec<Entry>) -> Vec<Vec<String>> {
    let mut by_size: HashMap<u64, Vec<&Entry>> = HashMap::new();
    for entry in &entries {
        by_size.entry(entry.size).or_default().push(entry);
    }

    let mut groups = Vec::new();
    for (size, bucket) in by_size {
        if bucket.len() < 2 {
            continue;
        }
        let mut by_inode: HashMap<(u64, u64), Vec<&Entry>> = HashMap::new();
        for entry in bucket {
            by_inode
                .entry((entry.dev, entry.ino))
                .or_default()
                .push(entry);
        }
        if by_inode.len() < 2 {
            continue;
        }
        let mut members = by_inode
            .values()
            .map(|links| {
                let names = links
                    .iter()
                    .map(|entry| entry.relative.as_str())
                    .collect::<Vec<_>>();
                canonical_name(&names)
            })
            .collect::<Vec<_>>();
        members.sort_by_key(|name| (name.chars().count(), name.clone()));
        groups.push((size, members));
    }

    groups.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| basename(left.1[0].as_str()).cmp(basename(right.1[0].as_str())))
            .then_with(|| left.1[0].as_str().cmp(right.1[0].as_str()))
    });
    groups.into_iter().map(|(_size, members)| members).collect()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let root_arg = arguments.next().ok_or("usage: dedup-rs <root>")?;
    let root = fs::canonicalize(&root_arg)?;
    let mut report_path = None;
    let mut min_size = DEFAULT_MIN_SIZE;
    let mut ext = Vec::new();
    let mut skip_ext = Vec::new();
    let mut excludes = Vec::new();

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--report-json" | "--min-size" | "-j" | "-x" | "--exclude" | "--ext" | "--skip-ext" => {
                let value = arguments
                    .next()
                    .ok_or(format!("{argument} needs a value"))?;
                match argument.as_str() {
                    "--report-json" => report_path = Some(PathBuf::from(value)),
                    "--min-size" => min_size = value.parse()?,
                    "-x" | "--exclude" => excludes.push(value),
                    "--ext" => ext.push(value),
                    "--skip-ext" => skip_ext.push(value),
                    _ => {}
                }
            }
            "--no-cache" | "--list" | "-y" | "--yes" | "--hard" => {}
            "--auto" => {}
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let include_ext = if ext.is_empty() {
        None
    } else {
        Some(collect_extensions(&ext))
    };
    let skip_ext = collect_extensions(&skip_ext);
    let filters = Filters {
        min_size,
        include_ext: include_ext.as_deref(),
        skip_ext: &skip_ext,
        excludes: &excludes,
    };
    let mut report = Report::default();
    let entries = walk(&root, filters, &mut report);
    report.groups = size_groups(entries);
    report.unreadable.sort();
    report.unreadable.dedup();

    let destination = report_path.unwrap_or_else(|| {
        let mut path = std::env::temp_dir();
        path.push("dedup-rs-report.json");
        path
    });
    fs::write(destination, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
