use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
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
struct Effort {
    opens: u64,
    bytes_read: u64,
}

impl Effort {
    #[cfg_attr(not(test), allow(dead_code))]
    fn opened(&mut self) -> OpenedFile<'_> {
        self.opens += 1;
        OpenedFile { effort: self }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
struct OpenedFile<'a> {
    effort: &'a mut Effort,
}

impl OpenedFile<'_> {
    #[cfg_attr(not(test), allow(dead_code))]
    fn read_exactly_to_end(
        &mut self,
        file: &mut fs::File,
        requested: usize,
    ) -> io::Result<Vec<u8>> {
        let mut contents = Vec::with_capacity(requested);
        let mut chunk = [0_u8; 64 * 1024];
        while contents.len() < requested {
            let remaining = requested - contents.len();
            let limit = chunk.len().min(remaining);
            let length = file.read(&mut chunk[..limit])?;
            if length == 0 {
                break;
            }
            self.effort.bytes_read += length as u64;
            contents.extend_from_slice(&chunk[..length]);
        }
        Ok(contents)
    }
}

#[cfg(test)]
fn read_whole_file(path: &Path, size: usize, effort: &mut Effort) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut opened = effort.opened();
    opened.read_exactly_to_end(&mut file, size)
}

fn normalize_extension(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix('.').unwrap_or(value);
    value.to_lowercase()
}

#[derive(Debug)]
struct FilterError {
    message: String,
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FilterError {}

fn collect_extensions(values: &[String], flag: &str) -> Result<Vec<String>, FilterError> {
    let mut extensions = Vec::new();
    for raw in values {
        for token in raw.split(',') {
            let extension = normalize_extension(token);
            if extension.is_empty() {
                return Err(FilterError {
                    message: format!("{flag}: empty extension in '{raw}'"),
                });
            }
            extensions.push(extension);
        }
    }
    Ok(extensions)
}

fn resolve_extensions(
    ext: &[String],
    skip_ext: &[String],
) -> Result<(Option<Vec<String>>, Vec<String>), FilterError> {
    let include_ext = if ext.is_empty() {
        None
    } else {
        Some(collect_extensions(ext, "--ext")?)
    };
    let exclude_ext = collect_extensions(skip_ext, "--skip-ext")?;
    if let Some(include_ext) = &include_ext {
        let mut conflicts = include_ext
            .iter()
            .filter(|extension| exclude_ext.contains(extension))
            .cloned()
            .collect::<Vec<_>>();
        conflicts.sort();
        conflicts.dedup();
        if !conflicts.is_empty() {
            return Err(FilterError {
                message: format!("in both --ext and --skip-ext: {}", conflicts.join(", ")),
            });
        }
    }
    Ok((include_ext, exclude_ext))
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
    #[derive(Clone)]
    enum Token {
        Any,
        One,
        Set {
            characters: Vec<char>,
            ranges: Vec<(char, char)>,
            negated: bool,
        },
    }

    fn parse(pattern: &str) -> Option<Vec<Token>> {
        let characters = pattern.chars().collect::<Vec<_>>();
        let mut index = 0;
        let mut tokens = Vec::new();
        while index < characters.len() {
            match characters[index] {
                '*' => {
                    tokens.push(Token::Any);
                    index += 1;
                }
                '?' => {
                    tokens.push(Token::One);
                    index += 1;
                }
                '[' => {
                    let start = index + 1;
                    let mut cursor = start;
                    let negated = matches!(characters.get(cursor), Some('!'));
                    if negated {
                        cursor += 1;
                    }
                    // A ']' in first position is literal, matching fnmatch.
                    let mut set = Vec::new();
                    let mut ranges = Vec::new();
                    let mut closed = false;
                    while cursor < characters.len() {
                        if characters[cursor] == ']' && cursor > start {
                            closed = true;
                            break;
                        }
                        let first = characters[cursor];
                        if first == ']' {
                            set.push(first);
                            cursor += 1;
                            continue;
                        }
                        if matches!(characters.get(cursor + 1), Some('-'))
                            && matches!(characters.get(cursor + 2), Some(&second) if second != ']')
                        {
                            let second = characters[cursor + 2];
                            ranges.push((first.min(second), first.max(second)));
                            cursor += 3;
                            continue;
                        }
                        set.push(first);
                        cursor += 1;
                    }
                    if !closed {
                        tokens.push(Token::Set {
                            characters: vec!['['],
                            ranges: Vec::new(),
                            negated: false,
                        });
                        index += 1;
                    } else {
                        tokens.push(Token::Set {
                            characters: set,
                            ranges,
                            negated,
                        });
                        index = cursor + 1;
                    }
                }
                expected => {
                    tokens.push(Token::Set {
                        characters: vec![expected],
                        ranges: Vec::new(),
                        negated: false,
                    });
                    index += 1;
                }
            }
        }
        Some(tokens)
    }

    fn matches_set(token: &Token, actual: char) -> bool {
        let Token::Set {
            characters,
            ranges,
            negated,
        } = token
        else {
            return false;
        };
        let contained = characters.contains(&actual)
            || ranges
                .iter()
                .any(|(start, end)| actual >= *start && actual <= *end);
        contained != *negated
    }

    fn inner(tokens: &[Token], text: &[char]) -> bool {
        match (tokens.first(), text.first()) {
            (None, None) => true,
            (Some(Token::Any), _) => {
                (0..=text.len()).any(|index| inner(&tokens[1..], &text[index..]))
            }
            (Some(Token::One), Some(_)) => inner(&tokens[1..], &text[1..]),
            (Some(token), Some(&actual)) if matches!(token, Token::Set { .. }) => {
                matches_set(token, actual) && inner(&tokens[1..], &text[1..])
            }
            _ => false,
        }
    }
    if let Some(parsed) = parse(pattern) {
        let text = text.chars().collect::<Vec<_>>();
        inner(&parsed, &text)
    } else {
        false
    }
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
                    push_unreadable_path(report, root, &directory, error);
                    continue;
                }
            };
            let path = entry.path();
            let relative = relative_path(root, &path);
            if excluded(&relative, &filters) {
                continue;
            }
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

            if !extension_allowed(&relative, &filters) {
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

fn relative_path(root: &Path, path: &Path) -> String {
    relative_or_absolute(root, path)
}

fn canonical_name(names: &[&str]) -> String {
    names
        .iter()
        .min_by_key(|name| (name.chars().count(), *name))
        .map(ToString::to_string)
        .expect("hardlink record always has a name")
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
            .then_with(|| {
                let left_basename = Path::new(&left.1[0])
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default();
                let right_basename = Path::new(&right.1[0])
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default();
                left_basename.cmp(right_basename)
            })
            .then_with(|| left.1[0].as_str().cmp(right.1[0].as_str()))
    });
    groups.into_iter().map(|(_size, members)| members).collect()
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
            "--auto" => return Err("--auto is not implemented in M1".into()),
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    let (include_ext, skip_ext) = resolve_extensions(&ext, &skip_ext)?;
    let filters = Filters {
        min_size,
        include_ext: include_ext.as_deref(),
        skip_ext: &skip_ext,
        excludes: &excludes,
    };
    let mut report = Report::default();
    let effort = Effort::default();
    let entries = walk(&root, filters, &mut report);
    report.groups = size_groups(entries);
    report.unreadable.sort();
    report.unreadable.dedup();

    let destination = report_path.unwrap_or_else(|| {
        let mut path = std::env::temp_dir();
        path.push("dedup-rs-report.json");
        path
    });
    report.bytes_read = effort.bytes_read;
    report.opens = effort.opens;
    fs::write(destination, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
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
    fn normalization_strips_whitespace_and_exactly_one_leading_dot() {
        assert_eq!(normalize_extension(" JPG "), "jpg");
        assert_eq!(normalize_extension(".gz"), "gz");
        assert_eq!(normalize_extension("..gz"), ".gz");
    }

    #[test]
    fn extension_filter_conflicts_and_empty_tokens_are_errors() {
        let error = resolve_extensions(&["gz".to_string()], &[".GZ".to_string()])
            .expect_err("same normalized extension must conflict");
        assert_eq!(error.message, "in both --ext and --skip-ext: gz");

        let error = resolve_extensions(&[], &["jpg,,gz".to_string()])
            .expect_err("empty token must be rejected");
        assert_eq!(error.message, "--skip-ext: empty extension in 'jpg,,gz'");

        let (include, exclude) = resolve_extensions(&["jpg".to_string()], &["tmp".to_string()])
            .expect("different extensions are legal");
        assert_eq!(include.as_deref(), Some(&["jpg".to_string()][..]));
        assert_eq!(exclude, ["tmp".to_string()]);
    }

    #[test]
    fn glob_supports_character_classes_ranges_and_negation() {
        assert!(glob_match("[ab].gz", "a.gz"));
        assert!(glob_match("[ab].gz", "b.gz"));
        assert!(!glob_match("[ab].gz", "c.gz"));
        assert!(glob_match("[!x]*.gz", "abc.gz"));
        assert!(!glob_match("[!x]*.gz", "x.gz"));
        assert!(glob_match("[a-z].bin", "q.bin"));
        assert!(!glob_match("[a-z].bin", "Q.bin"));
        assert!(glob_match("[[]literal]", "[literal]"));
        assert!(glob_match("literal[", "literal["));
    }

    #[test]
    fn exclusion_matches_entry_or_any_directory_component() {
        let empty = Vec::new();
        let patterns = vec!["*.tmp".to_string()];
        let temporary = filters(&patterns, &empty);
        assert!(!excluded("file", &temporary));
        assert!(excluded("backup.tmp/file", &temporary));

        let directory_patterns = vec!["backup".to_string()];
        let backup = filters(&directory_patterns, &empty);
        assert!(excluded("backup/report.bin", &backup));
        assert!(excluded("nested/backup/report.bin", &backup));
        assert!(!excluded("backups/report.bin", &backup));
    }

    #[test]
    fn size_groups_collapse_hardlinks_and_sort_deterministically() {
        let entries = vec![
            Entry {
                relative: "z.bin".into(),
                size: 2,
                dev: 1,
                ino: 10,
            },
            Entry {
                relative: "links/a.bin".into(),
                size: 2,
                dev: 1,
                ino: 9,
            },
            Entry {
                relative: "links/b.bin".into(),
                size: 2,
                dev: 1,
                ino: 9,
            },
            Entry {
                relative: "a.bin".into(),
                size: 1,
                dev: 1,
                ino: 11,
            },
            Entry {
                relative: "copy/a.bin".into(),
                size: 1,
                dev: 1,
                ino: 12,
            },
        ];
        assert_eq!(
            size_groups(entries),
            [["z.bin", "links/a.bin"], ["a.bin", "copy/a.bin"],]
        );
    }

    #[test]
    fn unique_sizes_and_single_inode_buckets_are_not_groups() {
        let entries = vec![
            Entry {
                relative: "one.bin".into(),
                size: 3,
                dev: 1,
                ino: 1,
            },
            Entry {
                relative: "two.bin".into(),
                size: 4,
                dev: 1,
                ino: 2,
            },
            Entry {
                relative: "hard-a.bin".into(),
                size: 5,
                dev: 1,
                ino: 3,
            },
            Entry {
                relative: "hard-b.bin".into(),
                size: 5,
                dev: 1,
                ino: 3,
            },
        ];
        assert!(size_groups(entries).is_empty());
    }

    #[test]
    fn effort_counts_only_real_file_opens_and_returned_bytes() {
        let mut effort = Effort::default();
        let contents = read_whole_file(Path::new("Cargo.toml"), 1 << 20, &mut effort)
            .expect("Cargo.toml is readable");
        let expected = fs::read("Cargo.toml").expect("Cargo.toml is readable");
        assert_eq!(contents, expected);
        assert_eq!(effort.opens, 1);
        assert_eq!(effort.bytes_read, contents.len() as u64);
    }
}
