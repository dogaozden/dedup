//! The five-stage narrowing, in the oracle's order: walk, then size groups,
//! then one page from each end (and the rest of the deep head for whatever
//! that left crowded), then sample windows through the middle, then lockstep
//! byte comparison.
//!
//! Stages 3 and 4 only ever narrow. The answer for every reported group traces
//! to bytes compared in stage 5, or to whole content held in memory by the
//! small-file stage.

use std::path::Path;

use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::compare::{compare_stage, Compare};
use crate::consts::{DEEP_HEAD, HEAD_BYTES, RETAIN_BUDGET, SAMPLE_SPAN, TAIL_BYTES};
use crate::device::Device;
use crate::effort::Effort;
use crate::filters::Filters;
use crate::stages::{has_tail, hash_stage, release, small_file_stage, Probe, Reader};
use crate::walk::{basename, candidates, name_order, walk, FileRec};

pub struct DupGroup {
    pub members: Vec<usize>,
}

pub struct Scan {
    pub records: Vec<FileRec>,
    pub groups: Vec<DupGroup>,
    pub files_seen: u64,
    pub bytes_seen: u64,
    pub unreadable: Vec<String>,
}

/// How much the end probe reads — for the retention decision.
fn ends_bytes(size: u64) -> usize {
    size.min(HEAD_BYTES as u64) as usize + usize::from(has_tail(size)) * TAIL_BYTES
}

fn pool_of(jobs: usize) -> ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .expect("a thread pool of a positive size")
}

pub fn find_duplicates(root: &Path, filters: Filters, device: Device, effort: &Effort) -> Scan {
    let mut files_seen = 0;
    let mut bytes_seen = 0;
    let mut unreadable = Vec::new();
    let entries = walk(
        root,
        filters,
        &mut files_seen,
        &mut bytes_seen,
        &mut unreadable,
    );

    // stage 2 — group by size, collapse hardlinks, discard unique sizes
    let mut records = candidates(root, entries);
    let mut scan = Scan {
        groups: Vec::new(),
        files_seen,
        bytes_seen,
        unreadable,
        records: Vec::new(),
    };
    if records.is_empty() {
        return scan;
    }

    let random_jobs = device.random_jobs();
    let sequential_jobs = device.sequential_jobs();
    let random_pool = pool_of(random_jobs);
    let reader = Reader {
        root,
        effort,
        pool: &random_pool,
        jobs: random_jobs,
    };
    let mut unreadable = std::mem::take(&mut scan.unreadable);
    let mut settled: Vec<Vec<usize>> = Vec::new();

    // A file this small costs the same to read whole as stage 3's first page
    // alone would cost for a bigger one, so there is nothing to stage for it:
    // settle it in one pass and let everything below work on what is left.
    let small: Vec<usize> = (0..records.len())
        .filter(|&index| records[index].size <= DEEP_HEAD as u64)
        .collect();
    let mut staged: Vec<usize> = (0..records.len())
        .filter(|&index| records[index].size > DEEP_HEAD as u64)
        .collect();

    if !small.is_empty() {
        let (proven, fallback) = small_file_stage(&reader, &records, &small, &mut unreadable);
        settled.extend(proven);
        if !fallback.is_empty() {
            staged.extend(fallback);
            staged.sort_by_key(|&index| records[index].layout());
        }
    }

    // Stage 5 has to see the bytes of the head to prove a group, and stage 3
    // is about to read them. Keeping them means stage 5 compares what is
    // already in hand instead of reading the same region a second time. The
    // cost is holding them until then, so this only happens when the whole set
    // fits in RETAIN_BUDGET.
    let keep = staged
        .iter()
        .map(|&index| ends_bytes(records[index].size))
        .sum::<usize>()
        <= RETAIN_BUDGET;

    // stage 3 — one page from each end, in one open.
    let end_buckets = hash_stage(
        &reader,
        &mut records,
        &staged,
        Probe::Ends { keep },
        &mut unreadable,
    );
    let mut pending: Vec<Vec<usize>> = end_buckets
        .into_iter()
        .filter(|bucket| bucket.len() > 1)
        .collect();
    if keep {
        release(&mut records, &staged, &pending);
    }

    // One page settles most candidates outright. Whatever it left sitting in a
    // bucket with someone else reads the rest of the deep head — only those
    // files pay for it, and only for the part not already read.
    let deep: Vec<Vec<usize>> = pending
        .iter()
        .filter(|bucket| records[bucket[0]].size > HEAD_BYTES as u64)
        .cloned()
        .collect();
    if !deep.is_empty() {
        pending.retain(|bucket| records[bucket[0]].size <= HEAD_BYTES as u64);
        let mut crowded: Vec<usize> = deep.into_iter().flatten().collect();
        crowded.sort_by_key(|&index| records[index].layout());
        let keep_rest = keep
            && crowded
                .iter()
                .map(|&index| (records[index].size.min(DEEP_HEAD as u64) as usize) - HEAD_BYTES)
                .sum::<usize>()
                <= RETAIN_BUDGET;
        let rest_buckets = hash_stage(
            &reader,
            &mut records,
            &crowded,
            Probe::DeepHead { keep: keep_rest },
            &mut unreadable,
        );
        pending.extend(rest_buckets.into_iter().filter(|bucket| bucket.len() > 1));
        if keep {
            release(&mut records, &crowded, &pending);
        }
    }

    // stage 4 — sample points spread through the file. Only files big enough
    // to be worth sampling go through it: below SAMPLE_SPAN there is nothing
    // to sample, since a full read there would just be repeated by stage 5.
    // Every bucket holds one size, so a bucket is entirely on one side.
    let big: Vec<Vec<usize>> = pending
        .iter()
        .filter(|bucket| records[bucket[0]].size > SAMPLE_SPAN)
        .cloned()
        .collect();
    if !big.is_empty() {
        pending.retain(|bucket| records[bucket[0]].size <= SAMPLE_SPAN);
        let mut survivors: Vec<usize> = big.into_iter().flatten().collect();
        survivors.sort_by_key(|&index| records[index].layout());
        let sample_buckets = hash_stage(
            &reader,
            &mut records,
            &survivors,
            Probe::Sample,
            &mut unreadable,
        );
        pending.extend(sample_buckets.into_iter().filter(|bucket| bucket.len() > 1));
    }

    // stage 5 — compare what is left byte for byte, in step. Sampled agreement
    // is not proof and this tool deletes things, so the bytes get read; doing
    // it as a comparison rather than a hash means files that turn out to
    // differ stop being read where they diverge.
    pending.sort_by_key(|bucket| {
        bucket
            .iter()
            .map(|&index| records[index].layout())
            .min()
            .expect("a bucket always has a member")
    });
    let context = Compare {
        root,
        effort,
        solid: device.all_solid_state(),
    };
    let sequential_pool = pool_of(sequential_jobs);
    settled.extend(compare_stage(
        &context,
        &records,
        &pending,
        sequential_jobs,
        &sequential_pool,
        &mut unreadable,
    ));

    let mut groups: Vec<DupGroup> = settled
        .into_iter()
        .map(|members| DupGroup { members })
        .collect();
    sort_groups(&records, &mut groups);

    scan.records = records;
    scan.groups = groups;
    scan.unreadable = unreadable;
    scan
}

/// Members shortest-name-first; groups by size descending, then the keeper's
/// basename, then the keeper's path.
///
/// The keeper's path is the last word: size and basename can both tie, and
/// without a third key the order falls back to however the pipeline happened
/// to emit the groups — which traces to inode allocation, so the same tree
/// scanned twice could report in a different order.
pub fn sort_groups(records: &[FileRec], groups: &mut [DupGroup]) {
    for group in groups.iter_mut() {
        group.members.sort_by(|&left, &right| {
            name_order(records[left].path()).cmp(&name_order(records[right].path()))
        });
    }
    groups.sort_by(|left, right| {
        let (first, second) = (&records[left.members[0]], &records[right.members[0]]);
        second
            .size
            .cmp(&first.size)
            .then_with(|| basename(first.path()).cmp(basename(second.path())))
            .then_with(|| first.path().cmp(second.path()))
    });
}

pub fn report_names(records: &[FileRec], groups: &[DupGroup]) -> Vec<Vec<String>> {
    groups
        .iter()
        .map(|group| {
            group
                .members
                .iter()
                .map(|&index| records[index].path().to_string())
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::TAIL_BYTES;

    #[test]
    fn the_end_probe_only_pays_for_a_tail_once_the_pages_cannot_meet() {
        assert_eq!(ends_bytes(100), 100);
        assert_eq!(ends_bytes(HEAD_BYTES as u64 + 1), HEAD_BYTES);
        assert_eq!(ends_bytes((DEEP_HEAD + TAIL_BYTES) as u64), HEAD_BYTES);
        assert_eq!(
            ends_bytes((DEEP_HEAD + TAIL_BYTES + 1) as u64),
            HEAD_BYTES + TAIL_BYTES
        );
    }
}
