//! Stages 3 and 4 — the digest probes — and the small-file stage that settles
//! everything at or under `DEEP_HEAD` without staging a digest at all.
//!
//! Every digest here only ever narrows the field. Nothing is called a
//! duplicate on the strength of one: stage 5 compares the bytes.

use std::io::{Seek, SeekFrom};
use std::path::Path;

use rayon::prelude::*;
use rayon::ThreadPool;
use sha2::{Digest as _, Sha256};

use crate::consts::{
    DEEP_HEAD, HASH_BATCH, HEAD_BYTES, RETAIN_BUDGET, RETAIN_CHECK_SPAN, SAMPLE_BYTES,
    SAMPLE_POINTS, TAIL_BYTES,
};
use crate::effort::Effort;
use crate::ordered::Ordered;
use crate::walk::FileRec;

pub type Digest = [u8; 32];

/// What every reading stage needs: where the tree is, who counts the syscalls,
/// and the pool its reads go through.
pub struct Reader<'a> {
    pub root: &'a Path,
    pub effort: &'a Effort,
    pub pool: &'a ThreadPool,
    pub jobs: usize,
}

/// SHA-256 — every CPU this runs on has an instruction for it, which makes it
/// about twice as fast here as BLAKE2b. See `_digest` in the oracle.
fn digest_of(parts: &[&[u8]]) -> Digest {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

pub fn sha256(bytes: &[u8]) -> Digest {
    digest_of(&[bytes])
}

/// Whether a file is long enough for the two end pages not to meet.
pub fn has_tail(size: u64) -> bool {
    size > (DEEP_HEAD + TAIL_BYTES) as u64
}

/// Where stage 4 looks. Everything before `DEEP_HEAD` and everything from the
/// tail page onward has already been read, so the windows spread across what
/// is left and no byte is read twice.
fn sample_points(size: u64) -> Vec<u64> {
    let first = DEEP_HEAD as u64;
    let last = size - TAIL_BYTES as u64 - SAMPLE_BYTES as u64;
    let step = (last - first) / (SAMPLE_POINTS as u64 - 1);
    (0..SAMPLE_POINTS as u64)
        .map(|index| first + index * step)
        .collect()
}

/// What one probe brought back: a digest, and the blocks it read where the
/// stage was asked to keep them.
struct Probed {
    digest: Digest,
    head: Vec<u8>,
    tail: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    /// One page from each end of the file, in one open. Same-size files that
    /// share a header are the ordinary case, and the page at the other end is
    /// where they part — asking for it costs a seek rather than another open.
    Ends { keep: bool },
    /// `HEAD_BYTES..DEEP_HEAD`, the part of the head not already read. Only
    /// files a one-page head left in a bucket with someone else get this far.
    DeepHead { keep: bool },
    /// Windows spread through the middle, for files past the sampling
    /// threshold.
    Sample,
}

fn probe_one(root: &Path, record: &FileRec, probe: Probe, effort: &Effort) -> Option<Probed> {
    let path = root.join(record.path());
    match probe {
        Probe::Ends { keep } => {
            let mut file = effort.open(&path).ok()?;
            let head = effort.read_block(&mut file, HEAD_BYTES).ok()?;
            let tail = if has_tail(record.size) {
                file.seek(SeekFrom::Start(record.size - TAIL_BYTES as u64))
                    .ok()?;
                effort.read_block(&mut file, TAIL_BYTES).ok()?
            } else {
                Vec::new()
            };
            let digest = digest_of(&[&head, &tail]);
            Some(Probed {
                digest,
                head: if keep { head } else { Vec::new() },
                tail: if keep { tail } else { Vec::new() },
            })
        }
        Probe::DeepHead { keep } => {
            let want = (record.size.min(DEEP_HEAD as u64) as usize).saturating_sub(HEAD_BYTES);
            let mut file = effort.open(&path).ok()?;
            file.seek(SeekFrom::Start(HEAD_BYTES as u64)).ok()?;
            let block = effort.read_block(&mut file, want).ok()?;
            let digest = digest_of(&[&block]);
            Some(Probed {
                digest,
                head: if keep { block } else { Vec::new() },
                tail: Vec::new(),
            })
        }
        Probe::Sample => {
            // Only groups past SAMPLE_SPAN reach stage 4, so the oracle's
            // "small enough, just hash it whole" branch is unreachable here.
            let mut hasher = Sha256::new();
            hasher.update(record.size.to_string().as_bytes()); // same samples, different length
            let mut file = effort.open(&path).ok()?;
            for offset in sample_points(record.size) {
                file.seek(SeekFrom::Start(offset)).ok()?;
                let block = effort.read_block(&mut file, SAMPLE_BYTES).ok()?;
                if block.is_empty() {
                    break;
                }
                hasher.update(&block);
            }
            Some(Probed {
                digest: hasher.finalize().into(),
                head: Vec::new(),
                tail: Vec::new(),
            })
        }
    }
}

/// Enough batches to keep every reader busy, but no smaller: on a tree of a
/// hundred thousand small files, one task per file costs more in queueing than
/// the read it is carrying.
fn batch_span(items: usize, jobs: usize) -> usize {
    HASH_BATCH.min(items.div_ceil(jobs.max(1) * 4)).max(1)
}

/// Read `items` through the pool, in order, batched the way the oracle batches
/// hashing — one task per batch of files, not one task per file, or the
/// per-task overhead dominates a stage this many files pass through.
fn read_batched<T, R, F>(pool: &ThreadPool, items: &[T], jobs: usize, read: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync + Send,
{
    let span = batch_span(items.len(), jobs);
    let batches: Vec<Vec<R>> = pool.install(|| {
        items
            .par_chunks(span)
            .map(|chunk| chunk.iter().map(&read).collect())
            .collect()
    });
    batches.into_iter().flatten().collect()
}

/// Hash `todo` with `probe` and bucket the records by size and digest.
///
/// Size is part of the key because a head or sample digest covers only part of
/// the file: two files of different lengths can easily agree on their first
/// 64 KiB and never be duplicates.
///
/// Where the probe was asked to keep its blocks, they are appended to the
/// record's retained run only when that run is already exactly as long as the
/// gap in front of this block. A file answered from memory was never read and
/// carries nothing, so a later stage's block must not be glued to the front of
/// the gap where the earlier one would have been.
pub fn hash_stage(
    reader: &Reader,
    records: &mut [FileRec],
    todo: &[usize],
    probe: Probe,
    unreadable: &mut Vec<String>,
) -> Vec<Vec<usize>> {
    let probed = read_batched(reader.pool, todo, reader.jobs, |&index| {
        probe_one(reader.root, &records[index], probe, reader.effort)
    });

    let mut buckets: Ordered<(u64, Digest), usize> = Ordered::new();
    for (&index, outcome) in todo.iter().zip(probed) {
        let Some(outcome) = outcome else {
            unreadable.push(records[index].path().to_string());
            continue;
        };
        let record = &mut records[index];
        match probe {
            Probe::Ends { keep: true } => {
                if record.head.is_empty() && record.tail.is_empty() {
                    record.head = outcome.head;
                    record.tail = outcome.tail;
                }
            }
            Probe::DeepHead { keep: true } => {
                if record.head.len() == HEAD_BYTES {
                    record.head.extend_from_slice(&outcome.head);
                }
            }
            _ => {}
        }
        buckets.push((record.size, outcome.digest), index);
    }
    buckets.into_values()
}

/// Drop kept bytes for records no later stage will look at again.
pub fn release(records: &mut [FileRec], considered: &[usize], pending: &[Vec<usize>]) {
    let live: std::collections::HashSet<usize> = pending
        .iter()
        .flat_map(|group| group.iter().copied())
        .collect();
    for &index in considered {
        if !live.contains(&index) {
            records[index].head = Vec::new();
            records[index].tail = Vec::new();
        }
    }
}

/// Settle every stage-3 candidate at or under `DEEP_HEAD` without staging a
/// digest through a retention budget and the comparison stage.
///
/// Two tiers, the way stage 3 already is. Tier A is one page — or, for a file
/// no longer than a page, the whole file. A page unmatched by anyone else its
/// size settles right there. Tier B reads the rest, only for what tier A left
/// crowded, and groups on the bytes directly: no hash decides who matches
/// whom, and nothing about them is read again. That is stronger than the
/// digest stage 3 and stage 5 would have produced, not just cheaper.
///
/// Returns (proven, fallback). Fallback is what the retention budget declined
/// to hold, for the caller to fold back into the staged pipeline.
pub fn small_file_stage(
    reader: &Reader,
    records: &[FileRec],
    candidates: &[usize],
    unreadable: &mut Vec<String>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut by_size: Ordered<u64, usize> = Ordered::new();
    for &index in candidates {
        by_size.push(records[index].size, index);
    }

    let mut proven: Vec<Vec<usize>> = Vec::new();
    let mut fallback: Vec<usize> = Vec::new();

    // Content length stands in for size when grouping tiny files or tier B's
    // full content: two different sizes can never read back equal bytes there.
    // A tier-A page's length is fixed regardless of the file's total size, so
    // that bucket alone carries size explicitly.
    let mut tiny: Vec<usize> = Vec::new();
    let mut headed: Vec<usize> = Vec::new();
    for (size, group) in by_size.into_pairs() {
        if size <= HEAD_BYTES as u64 {
            tiny.extend(group);
        } else {
            headed.extend(group);
        }
    }

    // size <= HEAD_BYTES — tier A alone already reads the whole file; there is
    // no tier B left to stage.
    let mut distinct: Ordered<Vec<u8>, usize> = Ordered::new();
    let mut retained = 0_usize;
    let mut cursor = 0;
    while cursor < tiny.len() {
        if retained > RETAIN_BUDGET {
            fallback.extend_from_slice(&tiny[cursor..]);
            break;
        }
        let piece = &tiny[cursor..(cursor + RETAIN_CHECK_SPAN).min(tiny.len())];
        cursor += RETAIN_CHECK_SPAN;
        let read = read_batched(reader.pool, piece, reader.jobs, |&index| {
            read_whole(reader.root, &records[index], reader.effort)
        });
        for (&index, content) in piece.iter().zip(read) {
            let Some(content) = content else {
                unreadable.push(records[index].path().to_string());
                continue;
            };
            let length = content.len();
            if distinct.push(content, index) {
                retained += length;
            }
        }
    }
    for (_content, group) in distinct.into_pairs() {
        if group.len() > 1 {
            proven.push(group);
        }
    }

    // size > HEAD_BYTES — tier A, one page each.
    let mut by_head: Ordered<(u64, Vec<u8>), usize> = Ordered::new();
    retained = 0;
    cursor = 0;
    while cursor < headed.len() {
        if retained > RETAIN_BUDGET {
            fallback.extend_from_slice(&headed[cursor..]);
            break;
        }
        let piece = &headed[cursor..(cursor + RETAIN_CHECK_SPAN).min(headed.len())];
        cursor += RETAIN_CHECK_SPAN;
        let read = read_batched(reader.pool, piece, reader.jobs, |&index| {
            read_head(reader.root, &records[index], reader.effort)
        });
        for (&index, head) in piece.iter().zip(read) {
            let Some(head) = head else {
                unreadable.push(records[index].path().to_string());
                continue;
            };
            let length = head.len();
            if by_head.push((records[index].size, head), index) {
                retained += length;
            }
        }
    }

    // A page nobody its size shares is exactly as conclusive as it is in stage
    // 3: unique, and settled without reading further.
    let mut flat: Vec<(Vec<u8>, usize)> = Vec::new();
    for ((_size, head), group) in by_head.into_pairs() {
        if group.len() > 1 {
            flat.extend(group.into_iter().map(|index| (head.clone(), index)));
        }
    }

    // tier B — only the still-crowded remainder, batched across buckets (and
    // across sizes) so -j isn't limited to one bucket's members.
    let mut distinct: Ordered<Vec<u8>, usize> = Ordered::new();
    retained = 0;
    cursor = 0;
    while cursor < flat.len() {
        if retained > RETAIN_BUDGET {
            fallback.extend(flat[cursor..].iter().map(|(_, index)| *index));
            break;
        }
        let piece = &flat[cursor..(cursor + RETAIN_CHECK_SPAN).min(flat.len())];
        cursor += RETAIN_CHECK_SPAN;
        let read = read_batched(reader.pool, piece, reader.jobs, |(_, index)| {
            read_rest(reader.root, &records[*index], reader.effort)
        });
        for ((head, index), rest) in piece.iter().zip(read) {
            let Some(rest) = rest else {
                unreadable.push(records[*index].path().to_string());
                continue;
            };
            let mut content = head.clone();
            content.extend_from_slice(&rest);
            debug_assert_eq!(content.len(), records[*index].size as usize);
            let length = content.len();
            if distinct.push(content, *index) {
                retained += length;
            }
        }
    }
    for (_content, group) in distinct.into_pairs() {
        if group.len() > 1 {
            proven.push(group);
        }
    }

    (proven, fallback)
}

/// Read a file's entire contents in one open. Used for stage-3 candidates at
/// or under `DEEP_HEAD`: reading the whole thing once costs no more than the
/// head page stage 3 would read for a bigger file.
fn read_whole(root: &Path, record: &FileRec, effort: &Effort) -> Option<Vec<u8>> {
    let mut file = effort.open(&root.join(record.path())).ok()?;
    effort.read_block(&mut file, record.size as usize).ok()
}

fn read_head(root: &Path, record: &FileRec, effort: &Effort) -> Option<Vec<u8>> {
    let mut file = effort.open(&root.join(record.path())).ok()?;
    effort.read_block(&mut file, HEAD_BYTES).ok()
}

fn read_rest(root: &Path, record: &FileRec, effort: &Effort) -> Option<Vec<u8>> {
    let want = (record.size as usize).saturating_sub(HEAD_BYTES);
    let mut file = effort.open(&root.join(record.path())).ok()?;
    file.seek(SeekFrom::Start(HEAD_BYTES as u64)).ok()?;
    effort.read_block(&mut file, want).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_end_pages_only_separate_once_they_cannot_meet() {
        assert!(!has_tail((DEEP_HEAD + TAIL_BYTES) as u64));
        assert!(has_tail((DEEP_HEAD + TAIL_BYTES + 1) as u64));
    }

    #[test]
    fn sample_windows_start_after_the_deep_head_and_end_before_the_tail() {
        let size = 16 * 1024 * 1024;
        let points = sample_points(size);
        assert_eq!(points.len(), SAMPLE_POINTS);
        assert_eq!(points[0], DEEP_HEAD as u64);
        let last = points[SAMPLE_POINTS - 1] + SAMPLE_BYTES as u64;
        assert!(last <= size - TAIL_BYTES as u64);
        assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn batches_stay_wide_enough_to_be_worth_queueing() {
        assert_eq!(batch_span(0, 8), 1);
        assert_eq!(batch_span(1, 8), 1);
        assert_eq!(batch_span(100_000, 8), HASH_BATCH);
        assert_eq!(batch_span(64, 8), 2);
    }
}
