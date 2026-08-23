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

use crate::cache::ScanCache;
use crate::compare::{compare_stage, Compare};
use crate::consts::{DEEP_HEAD, HEAD_BYTES, RETAIN_BUDGET, SAMPLE_SPAN, TAIL_BYTES};
use crate::device::{Profile, VolumeProbe, Wanted};
use crate::effort::Effort;
use crate::filters::Filters;
use crate::ordered::Ordered;
use crate::stages::{has_tail, hash_stage, release, small_file_stage, Probe, Reader};
use crate::walk::{basename, candidates, name_order, walk, FileRec};

#[derive(Debug)]
pub struct DupGroup {
    pub members: Vec<usize>,
    /// True when the members were matched from remembered digests instead of
    /// being read this scan. Such a group is a candidate, not a conclusion:
    /// nothing may be deleted from it until the bytes have been read again.
    pub attested: bool,
}

pub struct Scan {
    pub records: Vec<FileRec>,
    /// Which profile the scan actually ran with, and the one line to say about
    /// it if `auto` had nothing to go on. Printing belongs to `main`; deciding
    /// belongs here, where the volume probe has been collected.
    pub profile: Profile,
    pub hint: Option<&'static str>,
    /// Private on purpose. The only ways out are `reported`, for the protocol
    /// report, where a size bucket is an honest answer; and `proven`, for
    /// deletion, where it is not.
    groups: Vec<DupGroup>,
    proven: bool,
    pub files_seen: u64,
    pub bytes_seen: u64,
    pub unreadable: Vec<String>,
}

/// A borrow of groups that trace to bytes compared this scan — or to a cache
/// attestation that `verify_attested` re-proves from bytes before anything is
/// removed.
///
/// `Scan::proven` is the only way to obtain one and it returns `None` for a
/// scan that merely grouped by size, so the deletion path cannot be reached
/// from one. The guard is the type: `run_auto` will not accept anything else,
/// and no later caller can reintroduce the hole by forgetting a comparison on
/// a mode string.
pub struct ProvenGroups<'a>(&'a [DupGroup]);

impl<'a> ProvenGroups<'a> {
    pub fn groups(self) -> &'a [DupGroup] {
        self.0
    }
}

impl Scan {
    /// Every group the scan found, for reporting.
    pub fn reported(&self) -> &[DupGroup] {
        &self.groups
    }

    /// The groups deletion may act on, or `None` when nothing has read a byte
    /// of any file's contents.
    pub fn proven(&self) -> Option<ProvenGroups<'_>> {
        self.proven.then_some(ProvenGroups(&self.groups))
    }
}

/// How much the end probe reads — for the retention decision.
///
/// The `spinning` profile does not go to the far end, so it has no tail to
/// hold and its budget is the heads alone.
fn ends_bytes(profile: Profile, size: u64) -> usize {
    let head = size.min(HEAD_BYTES as u64) as usize;
    match profile {
        Profile::Spinning => head,
        Profile::Flash => head + usize::from(has_tail(size)) * TAIL_BYTES,
    }
}

/// Stage 1 and 2 alone: the candidate set, which is all a walk and a size
/// bucket can say.
///
/// The M1 gate grades exactly this, on the premise that an implementation at
/// M1 has no content stages and so reports it by default. That premise expires
/// the moment stage 3 exists, which is why the harness's own oracle driver
/// carries the same switch. Nothing but `--gate m1` ever asks for it, and no
/// milestone here is claimed with it — see LOG.md.
pub fn size_stage(root: &Path, filters: Filters) -> Scan {
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
    let records = candidates(root, entries);
    let mut buckets: Ordered<u64, usize> = Ordered::new();
    for (index, record) in records.iter().enumerate() {
        buckets.push(record.size, index);
    }
    let mut groups: Vec<DupGroup> = buckets
        .into_values()
        .into_iter()
        .filter(|bucket| bucket.len() > 1)
        .map(|members| DupGroup {
            members,
            attested: false,
        })
        .collect();
    sort_groups(&records, &mut groups);
    Scan {
        records,
        // Nothing here reads, so nothing here has a profile worth the name.
        profile: Profile::Flash,
        hint: None,
        groups,
        // Files that merely share a length. Nothing here has read a byte.
        proven: false,
        files_seen,
        bytes_seen,
        unreadable,
    }
}

fn pool_of(jobs: usize) -> ThreadPool {
    ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .expect("a thread pool of a positive size")
}

pub fn find_duplicates(
    root: &Path,
    filters: Filters,
    probe: &mut VolumeProbe,
    effort: &Effort,
    mut cache: Option<&mut ScanCache>,
    wanted: Wanted,
) -> Scan {
    if let Some(cache) = cache.as_deref_mut() {
        cache.open_path(root);
    }
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
        profile: Profile::Flash,
        hint: None,
        groups: Vec::new(),
        proven: true,
        files_seen,
        bytes_seen,
        unreadable,
        records: Vec::new(),
    };
    if records.is_empty() {
        return scan;
    }
    if let Some(cache) = cache.as_deref_mut() {
        // ...and for any further volume the scan turned out to reach into.
        cache.open_for(root, &records);
    }

    // The volume probe has been running since the root was known; this is the
    // first thing that needs its answer.
    let device = probe.device();
    let (profile, hint) = wanted.resolve(device);
    scan.profile = profile;
    scan.hint = hint;
    let random_jobs = device.random_jobs(profile);
    let sequential_jobs = device.sequential_jobs(profile);
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

    let mut remembered: Vec<Vec<usize>> = Vec::new();
    if !small.is_empty() {
        let (proven, attested, fallback) = small_file_stage(
            &reader,
            &records,
            &small,
            &mut unreadable,
            cache.as_deref_mut(),
        );
        settled.extend(proven);
        remembered.extend(attested);
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
        .map(|&index| ends_bytes(profile, records[index].size))
        .sum::<usize>()
        <= RETAIN_BUDGET;

    // stage 3 — one page from each end, in one open. On a spinning disk the
    // page at the far end costs a seek per file and saves only the files that
    // differ there, so the profile reads the head alone and lets the stages
    // behind it do the separating.
    let ends = match profile {
        Profile::Flash => Probe::Ends { keep },
        Profile::Spinning => Probe::Head { keep },
    };
    let end_buckets = hash_stage(
        &reader,
        &mut records,
        &staged,
        ends,
        &mut unreadable,
        cache.as_deref_mut(),
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
            cache.as_deref_mut(),
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
    // Eight windows spread through a file is eight seeks for every member of
    // every surviving group, which is the worst thing this program does to a
    // spinning disk. That profile skips the stage entirely and lets the
    // comparison do the work in one sweep per file.
    let big: Vec<Vec<usize>> = if profile == Profile::Spinning {
        Vec::new()
    } else {
        pending
            .iter()
            .filter(|bucket| records[bucket[0]].size > SAMPLE_SPAN)
            .cloned()
            .collect()
    };
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
            cache.as_deref_mut(),
        );
        pending.extend(sample_buckets.into_iter().filter(|bucket| bucket.len() > 1));
    }

    // stage 5 — compare what is left byte for byte, in step. Sampled agreement
    // is not proof and this tool deletes things, so the bytes get read; doing
    // it as a comparison rather than a hash means files that turn out to
    // differ stop being read where they diverge.
    // A group every one of whose members carries a remembered whole-file
    // digest is set aside instead of read: it is reported, marked attested,
    // and read again at deletion time, where being sure actually matters.
    if let Some(cache) = cache.as_deref_mut() {
        let (still_pending, matched) = cache.attested_runs(&records, std::mem::take(&mut pending));
        pending = still_pending;
        remembered.extend(matched);
    }

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
        profile,
        solid: device.all_solid_state(),
        learn: cache.is_some(),
    };
    let sequential_pool = pool_of(sequential_jobs);
    let compared = compare_stage(
        &context,
        &records,
        &pending,
        sequential_jobs,
        &sequential_pool,
        &mut unreadable,
    );
    settled.extend(compared.matched);
    // The bytes went past anyway, so a group read here is also a group learned.
    // The digests come back from the pool rather than being written from inside
    // it, so the cache never needs a lock.
    if let Some(cache) = cache {
        for (index, digest) in compared.learned {
            cache.remember(&records[index], crate::cache::Field::Full, digest);
        }
    }

    let mut groups: Vec<DupGroup> = settled
        .into_iter()
        .map(|members| DupGroup {
            members,
            attested: false,
        })
        .chain(remembered.into_iter().map(|members| DupGroup {
            members,
            attested: true,
        }))
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

pub fn report_names<'a>(records: &'a [FileRec], groups: &[DupGroup]) -> Vec<Vec<&'a str>> {
    groups
        .iter()
        .map(|group| {
            group
                .members
                .iter()
                .map(|&index| records[index].path())
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
        let flash = Profile::Flash;
        assert_eq!(ends_bytes(flash, 100), 100);
        assert_eq!(ends_bytes(flash, HEAD_BYTES as u64 + 1), HEAD_BYTES);
        assert_eq!(
            ends_bytes(flash, (DEEP_HEAD + TAIL_BYTES) as u64),
            HEAD_BYTES
        );
        assert_eq!(
            ends_bytes(flash, (DEEP_HEAD + TAIL_BYTES + 1) as u64),
            HEAD_BYTES + TAIL_BYTES
        );
    }

    #[test]
    fn the_spinning_profile_budgets_for_no_tail_at_any_size() {
        let spin = Profile::Spinning;
        assert_eq!(ends_bytes(spin, 100), 100);
        assert_eq!(ends_bytes(spin, HEAD_BYTES as u64 + 1), HEAD_BYTES);
        assert_eq!(
            ends_bytes(spin, (DEEP_HEAD + TAIL_BYTES + 1) as u64),
            HEAD_BYTES
        );
    }
}
