use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

const DEFAULT_MIN_SIZE: u64 = 10 * 1024 * 1024;
const HEAD_BYTES: usize = 4 * 1024;
const DEEP_HEAD: usize = 64 * 1024;
const TAIL_BYTES: usize = 4 * 1024;
const SAMPLE_BYTES: usize = 64 * 1024;
const SAMPLE_POINTS: usize = 8;
const SAMPLE_SPAN: u64 = 8 * 1024 * 1024;
const COMPARE_BUDGET: usize = 128 * 1024 * 1024;
const MIN_CHUNK: usize = 128 * 1024;
const RETAIN_BUDGET: usize = 128 * 1024 * 1024;

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

#[derive(Clone, Debug)]
struct FileRecord {
    relative: String,
    size: u64,
    dev: u64,
    ino: u64,
    head: Vec<u8>,
    tail: Vec<u8>,
}

impl FileRecord {
    fn key(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
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
    fn opened(&mut self) -> OpenedFile<'_> {
        self.opens += 1;
        OpenedFile { effort: self }
    }
}

struct OpenedFile<'a> {
    effort: &'a mut Effort,
}

impl OpenedFile<'_> {
    fn read_exactly(&mut self, file: &mut fs::File, requested: usize) -> io::Result<Vec<u8>> {
        let mut contents = vec![0_u8; requested];
        let mut completed = 0_usize;
        while completed < requested {
            let length = file.read(&mut contents[completed..])?;
            if length == 0 {
                break;
            }
            self.effort.bytes_read += length as u64;
            completed += length;
        }
        contents.truncate(completed);
        Ok(contents)
    }

    fn read_block(&mut self, file: &mut fs::File, requested: usize) -> io::Result<Vec<u8>> {
        let mut contents = Vec::with_capacity(requested);
        let mut chunk = vec![0_u8; requested.min(1024 * 1024)];
        while contents.len() < requested {
            let wanted = requested - contents.len();
            let limit = wanted.min(chunk.len());
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

fn open_counted(path: &Path, effort: &mut Effort) -> io::Result<fs::File> {
    let file = fs::File::open(path)?;
    effort.opens += 1;
    Ok(file)
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
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default();
            let relative = relative_path(root, &path);
            if filters.excludes.iter().any(|pattern| glob_match(pattern, &name)) {
                continue;
            }
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
            if !file_type.is_file() || !extension_allowed(&relative, &filters) {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    push_unreadable_path(report, root, &path, error);
                    continue;
                }
            };

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

fn collapse_hardlinks(entries: Vec<Entry>) -> Vec<FileRecord> {
    let mut by_inode: BTreeMap<(u64, u64), FileRecord> = BTreeMap::new();
    for entry in entries {
        by_inode
            .entry((entry.dev, entry.ino))
            .or_insert_with(|| FileRecord {
                relative: entry.relative,
                size: entry.size,
                dev: entry.dev,
                ino: entry.ino,
                head: Vec::new(),
                tail: Vec::new(),
            });
    }
    by_inode.into_values().collect()
}

fn candidate_records(entries: Vec<Entry>) -> Vec<FileRecord> {
    let records = collapse_hardlinks(entries);
    let mut by_size: HashMap<u64, Vec<FileRecord>> = HashMap::new();
    for record in records {
        by_size.entry(record.size).or_default().push(record);
    }
    let mut candidates = Vec::new();
    for mut bucket in by_size.into_values() {
        if bucket.len() > 1 {
            candidates.append(&mut bucket);
        }
    }
    candidates.sort_by_key(|record| record.key());
    candidates
}

fn ends_bytes(size: u64) -> usize {
    (size.min(HEAD_BYTES as u64)) as usize
        + usize::try_from(size).map_or(0, |size| {
            if size > DEEP_HEAD + TAIL_BYTES {
                TAIL_BYTES
            } else {
                0
            }
        })
}

fn sample_points(size: u64) -> [usize; SAMPLE_POINTS] {
    let first = DEEP_HEAD;
    let last = (size as usize).saturating_sub(TAIL_BYTES + SAMPLE_BYTES);
    let step = if SAMPLE_POINTS > 1 {
        (last - first) / (SAMPLE_POINTS - 1)
    } else {
        0
    };
    std::array::from_fn(|index| first + index * step)
}

fn sampled_bytes(size: u64) -> usize {
    if size <= SAMPLE_SPAN {
        size as usize
    } else {
        SAMPLE_BYTES * SAMPLE_POINTS
    }
}

fn compare_chunk(members: usize, solid_state: bool) -> usize {
    let preferred = if solid_state {
        4 * 1024 * 1024
    } else {
        16 * 1024 * 1024
    };
    preferred
        .min(COMPARE_BUDGET / members.max(1))
        .max(MIN_CHUNK)
}

fn partition_blocks<T>(mut pending: Vec<(T, Vec<u8>)>) -> Vec<Vec<(T, Vec<u8>)>> {
    let mut runs = Vec::new();
    while !pending.is_empty() {
        let pivot = pending.remove(0);
        let mut same = vec![(pivot.0, pivot.1.clone())];
        let mut rest = Vec::new();
        for item in pending {
            if item.1 == pivot.1 {
                same.push(item);
            } else {
                rest.push(item);
            }
        }
        runs.push(same);
        pending = rest;
    }
    runs
}

fn read_whole(path: &Path, requested: usize, effort: &mut Effort) -> Option<Vec<u8>> {
    let mut file = open_counted(path, effort).ok()?;
    let mut opened = effort.opened();
    let result = opened.read_exactly(&mut file, requested);
    result.ok()
}
fn read_from(

    path: &Path,
    offset: usize,
    requested: usize,
    effort: &mut Effort,
) -> Option<Vec<u8>> {
    let mut file = open_counted(path, effort).ok()?;
    file.seek(SeekFrom::Start(offset as u64)).ok()?;
    let mut opened = effort.opened();
    opened.read_exactly(&mut file, requested).ok()
}
fn keep_ends(path: &Path, size: u64, effort: &mut Effort) -> Option<([u8; 32], Vec<u8>, Vec<u8>)> {
    use sha2::{Digest, Sha256};
    let mut file = open_counted(path, effort).ok()?;

    let mut opened = effort.opened();
    let head = opened.read_exactly(&mut file, HEAD_BYTES).ok()?;
    let tail = if size > (DEEP_HEAD + TAIL_BYTES) as u64 {
        file.seek(SeekFrom::Start(size - TAIL_BYTES as u64)).ok()?;
        opened.read_block(&mut file, TAIL_BYTES).ok()?
    } else {
        Vec::new()
    };
    let mut hasher = Sha256::new();
    hasher.update(&head);
    hasher.update(&tail);
    Some((hasher.finalize().into(), head, tail))
}

fn keep_head_rest(path: &Path, size: u64, effort: &mut Effort) -> Option<([u8; 32], Vec<u8>)> {
    use sha2::{Digest, Sha256};
    let wanted = (size.min(DEEP_HEAD as u64) as usize).saturating_sub(HEAD_BYTES);
    if wanted == 0 {
        return Some(([0; 32], Vec::new()));
    }
    let mut file = open_counted(path, effort).ok()?;
    let mut opened = effort.opened();
    file.seek(SeekFrom::Start(HEAD_BYTES as u64)).ok()?;
    let block = opened.read_exactly(&mut file, wanted).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&block);
    Some((hasher.finalize().into(), block))
}

fn hash_sample(path: &Path, size: u64, effort: &mut Effort) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    if size <= SAMPLE_SPAN {
        let content = read_whole(path, size as usize, effort)?;
        hasher.update((size as usize).to_le_bytes());
        hasher.update(&content);
    } else {
        let mut file = open_counted(path, effort).ok()?;
        let mut opened = effort.opened();
        hasher.update((size as usize).to_le_bytes());
        for offset in sample_points(size) {
            file.seek(SeekFrom::Start(offset as u64)).ok()?;
            let block = opened.read_block(&mut file, SAMPLE_BYTES).ok()?;
            hasher.update(&block);
        }
    }
    Some(hasher.finalize().into())
}

#[allow(clippy::type_complexity)]
fn hash_stage(
    files: &[FileRecord],
    mode: HashMode,
    _solid_state: bool,
    stats: &mut Stats,
    effort: &mut Effort,
) -> BTreeMap<(u64, [u8; 32]), Vec<FileRecord>> {
    let mut buckets: BTreeMap<(u64, [u8; 32]), Vec<FileRecord>> = BTreeMap::new();
    for record in files {
        let result = match mode {
            HashMode::Ends => keep_ends(Path::new(&record.relative), record.size, effort),
            HashMode::HeadRest => keep_head_rest(Path::new(&record.relative), record.size, effort)
                .map(|(digest, block)| (digest, record.head.clone(), block)),
            HashMode::Sample => hash_sample(Path::new(&record.relative), record.size, effort)
                .map(|digest| (digest, Vec::new(), Vec::new())),
        };
        match result {
            Some((digest, head, tail)) => {
                buckets
                    .entry((record.size, digest))
                    .or_default()
                    .push(record.clone());
                stats.hashed_files += 1;
                stats.bytes_read += match mode {
                    HashMode::Ends => ends_bytes(record.size),
                    HashMode::HeadRest => {
                        (record.size.min(DEEP_HEAD as u64) as usize).saturating_sub(HEAD_BYTES)
                    }
                    HashMode::Sample => sampled_bytes(record.size),
                } as u64;
                if mode == HashMode::Ends {
                    let bucket = buckets
                        .get_mut(&(record.size, digest))
                        .expect("bucket was inserted");
                    for member in bucket.iter_mut() {
                        member.head = head.clone();
                        member.tail = tail.clone();
                    }
                } else {
                    drop((head, tail));
                }
            }
            None => stats.unreadable.push(record.relative.clone()),
        }
    }
    buckets
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HashMode {
    Ends,
    HeadRest,
    Sample,
}

#[derive(Default)]
struct Stats {
    hashed_files: u64,
    bytes_read: u64,
    unreadable: Vec<String>,
}

impl Stats {
    fn fold(&mut self, other: Stats) {
        self.hashed_files += other.hashed_files;
        self.bytes_read += other.bytes_read;
        self.unreadable.extend(other.unreadable);
    }
}

fn small_file_stage(
    candidates: Vec<FileRecord>,
    stats: &mut Stats,
    effort: &mut Effort,
) -> (Vec<Vec<FileRecord>>, Vec<FileRecord>) {
    let mut by_size: BTreeMap<u64, Vec<FileRecord>> = BTreeMap::new();
    for record in candidates {
        by_size.entry(record.size).or_default().push(record);
    }
    let mut proven = Vec::new();
    let mut fallback = Vec::new();

    let mut tiny = Vec::new();
    let mut headed = Vec::new();
    for bucket in by_size.into_values() {
        if bucket[0].size <= HEAD_BYTES as u64 {
            tiny.extend(bucket);
        } else {
            headed.extend(bucket);
        }
    }

    let mut distinct: HashMap<Vec<u8>, Vec<FileRecord>> = HashMap::new();
    let mut retained = 0_usize;
    for record in tiny {
        if retained > RETAIN_BUDGET {
            fallback.push(record);
            continue;
        }
        match read_whole(Path::new(&record.relative), record.size as usize, effort) {
            Some(content) => {
                stats.hashed_files += 1;
                stats.bytes_read += content.len() as u64;
                retained += content.len();
                distinct.entry(content).or_default().push(record);
            }
            None => {
                stats.hashed_files += 1;
                stats.unreadable.push(record.relative.clone());
            }
        }
    }
    for group in distinct.into_values() {
        if group.len() > 1 {
            proven.push(group);
        }
    }

    let mut by_head: HashMap<(u64, Vec<u8>), Vec<FileRecord>> = HashMap::new();
    retained = 0;
    for record in headed {
        if retained > RETAIN_BUDGET {
            fallback.push(record);
            continue;
        }
        match read_whole(Path::new(&record.relative), HEAD_BYTES, effort) {
            Some(head) => {
                stats.bytes_read += head.len() as u64;
                retained += head.len();
                by_head.entry((record.size, head)).or_default().push(record);
            }
            None => {
                stats.hashed_files += 1;
                stats.unreadable.push(record.relative.clone());
            }
        }
    }

    let mut flat = Vec::new();
    for ((size, _head), mut group) in by_head {
        if group.len() > 1 {
            for record in group.drain(..) {
                let mut copy = record.clone();
                copy.size = size;
                flat.push((copy, record));
            }
        } else {
            stats.hashed_files += group.len() as u64;
        }
    }

    let mut distinct: HashMap<Vec<u8>, Vec<FileRecord>> = HashMap::new();
    retained = 0;
    for (mut pivot, mut record) in flat {
        if retained > RETAIN_BUDGET {
            fallback.push(record);
            continue;
        }
        let wanted = (record.size as usize).saturating_sub(HEAD_BYTES);
        let rest = read_from(Path::new(&record.relative), HEAD_BYTES, wanted, effort);
        match rest {
            Some(rest) => {
                stats.hashed_files += 1;
                stats.bytes_read += rest.len() as u64;
                retained += rest.len();
                let key = [pivot.head.as_slice(), &rest].concat();
                record.head.clear();
                distinct.entry(key).or_default().push(record);
            }
            None => {
                stats.hashed_files += 1;
                stats.unreadable.push(record.relative.clone());
            }
        }
        let _ = &mut pivot;
    }
    for group in distinct.into_values() {
        if group.len() > 1 {
            proven.push(group);
        }
    }
    (proven, fallback)
}

fn compare_group(
    members: &mut [FileRecord],
    chunk: usize,
    stats: &mut Stats,
    effort: &mut Effort,
) -> Option<Vec<FileRecord>> {
    if members.len() < 2 {
        return None;
    }
    let prefix_len = members.iter().map(|r| r.head.len()).min().unwrap_or(0);
    let tail_len = members.iter().map(|r| r.tail.len()).min().unwrap_or(0);
    let overlap = members
        .iter()
        .any(|r| prefix_len + tail_len > r.size as usize);
    let tail_len = if tail_len == 0 || overlap {
        0
    } else {
        tail_len
    };

    let mut handles: Vec<fs::File> = Vec::with_capacity(members.len());
    for member in members.iter_mut() {
        match open_counted(Path::new(&member.relative), effort) {
            Ok(file) => handles.push(file),
            Err(_) => return None,
        }
    }
    let limit = if tail_len == 0 {
        usize::MAX
    } else {
        members
            .iter()
            .map(|r| r.size as usize - tail_len)
            .min()
            .unwrap_or(0)
    };

    let mut live = vec![0_usize];
    while let Some(index) = live.pop() {
        let start = index * members.len();
        let end = ((index + 1) * members.len()).min(members.len());
        let _ = (start, end);
        break;
    }

    let mut position = 0_usize;
    while position < limit.min(members.first().map(|r| r.size as usize).unwrap_or(0)) {
        let span = chunk.min(limit - position);
        let mut blocks = Vec::with_capacity(handles.len());
        for file in &mut handles {
            file.seek(SeekFrom::Start(position as u64)).ok()?;
            let block = effort.opened().read_block(file, span).ok()?;
            blocks.push(block);
        }
        let first = blocks[0].clone();
        if blocks
            .iter()
            .any(|block| block != &first || block.is_empty())
        {
            return None;
        }
        position += span;
    }
    if tail_len == 0 && position < members.first().map(|r| r.size as usize).unwrap_or(0) {
        return None;
    }
    if tail_len > 0 {
        let start = members[0].size as usize - tail_len;
        let mut tails = Vec::with_capacity(handles.len());
        for file in &mut handles {
            file.seek(SeekFrom::Start(start as u64)).ok()?;
            tails.push(effort.opened().read_block(file, tail_len).ok()?);
        }
        let first = tails[0].clone();
        if tails.iter().any(|tail| tail != &first) {
            return None;
        }
    }
    stats.bytes_read += 0;
    Some(members.to_vec())
}

fn find_duplicates(
    root: &Path,
    filters: Filters,
    report: &mut Report,
    effort: &mut Effort,
) -> Vec<Vec<String>> {
    let entries = walk(root, filters, report);
    let candidates = candidate_records(entries);
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut stats = Stats::default();
    let mut groups = Vec::new();
    let (small, big): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|record| record.size <= DEEP_HEAD as u64);
    let (small_groups, small_fallback) = small_file_stage(small, &mut stats, effort);
    groups.extend(small_groups);
    let ends_buckets = hash_stage(&big, HashMode::Ends, true, &mut stats, effort);
    let mut pending: Vec<FileRecord> = Vec::new();
    let mut deep_pending = Vec::new();
    for ((_size, _digest), bucket) in ends_buckets {
        let members = bucket;
        if members.len() < 2 {
            continue;
        }
        if members[0].size > HEAD_BYTES as u64 {
            deep_pending.extend(members);
        } else {
            pending.extend(members);
        }
    }
    // Stage 3b — the rest of the deep head.
    let crowded = std::mem::take(&mut deep_pending);
    let rest_buckets = hash_stage(&crowded, HashMode::HeadRest, true, &mut stats, effort);
    for ((_size, _digest), mut bucket) in rest_buckets {
        if bucket.len() > 1 {
            pending.append(&mut bucket);
        }
    }

    let sample_buckets = hash_stage(&pending, HashMode::Sample, true, &mut stats, effort);

    // Stage 5 — byte comparison.
    let mut compare_buckets = sample_buckets
        .into_keys()
        .map(|key| key.0)
        .collect::<Vec<_>>();
    compare_buckets.sort();
    for size in compare_buckets {
        let mut members = pending
            .iter()
            .filter(|record| record.size == size)
            .cloned()
            .collect::<Vec<_>>();
        if members.len() < 2 {
            continue;
        }
        let chunk = compare_chunk(members.len(), true);
        if let Some(group) = compare_group(&mut members, chunk, &mut stats, effort) {
            groups.push(group);
        }
    }

    report.opens = effort.opens;
    report.bytes_read = effort.bytes_read;
    report.unreadable.extend(stats.unreadable.drain(..));
    report.unreadable.sort();
    report.unreadable.dedup();

    let mut output: Vec<(u64, Vec<String>)> = groups
        .into_iter()
        .map(|members| {
            let size = members[0].size;
            let mut names = members
                .iter()
                .map(|r| r.relative.clone())
                .collect::<Vec<_>>();
            names.sort_by_key(|name| (name.chars().count(), name.clone()));
            (size, names)
        })
        .collect();
    output.sort_by(|left, right| right.0.cmp(&left.0));
    output.into_iter().map(|(_, names)| names).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let root_arg = arguments.next().ok_or("usage: dedup-rs <root>")?;
    let root = fs::canonicalize(&root_arg)?;
    let mut report_path = None;
    let mut min_size = DEFAULT_MIN_SIZE;
    let mut ext = Vec::new();
    let mut skip_ext = Vec::new();
    let mut excludes = Vec::new();
    let mut list = false;

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
            "--no-cache" | "-y" | "--yes" | "--hard" | "--quiet" | "-q" => {}
            "--list" => list = true,
            "--auto" => return Err("--auto is not implemented in M2".into()),
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
    let mut effort = Effort::default();
    let groups = find_duplicates(&root, filters, &mut report, &mut effort);

    if list {
        if groups.is_empty() {
            println!("\n  No duplicates found.\n");
        } else {
            println!("\n  {} duplicate groups", groups.len());
            for group in &groups {
                println!("      {}", group.join(" "));
            }
        }
    }

    let destination = report_path.unwrap_or_else(|| {
        let mut path = std::env::temp_dir();
        path.push("dedup-rs-report.json");
        path
    });
    fs::write(destination, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
