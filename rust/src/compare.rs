//! Stage 5 — read every file in a group once, in step, and split them the
//! moment their bytes differ.
//!
//! Two things make this cheaper than hashing each file and comparing digests.
//! Files that turn out not to match stop being read at the byte where they
//! diverge, instead of being read to the end. And comparing blocks costs
//! almost nothing, where hashing them is the whole cost of a scan on a fast
//! disk. It is also exact by construction: there is no digest left to collide,
//! so nothing is deleted on the strength of a guess.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use rayon::prelude::*;
use rayon::ThreadPool;
use sha2::{Digest as _, Sha256};

use crate::cache::ScanCache;
use crate::consts::{
    CHUNK, COMPARE_BUDGET, MIN_CHUNK, PARTITION_FANOUT, SLOW_CHUNK, SPIN_INTERLEAVE,
};
use crate::device::Profile;
use crate::effort::Effort;
use crate::ordered::Ordered;
use crate::pipeline::DupGroup;
use crate::stages::{sha256, Digest};
use crate::walk::{still_the_same, unchanged, FileRec};

pub struct Compare<'a> {
    pub root: &'a Path,
    pub effort: &'a Effort,
    pub profile: Profile,
    pub solid: bool,
    /// Whether whole-file digests are worth building as the bytes go past. The
    /// bytes are being read anyway and SHA-256 runs at a few GB/s where a disk
    /// does not, so a group read here is also a group learned — but only with
    /// a cache to put it in.
    pub learn: bool,
    /// Where to open this stage's files.
    ///
    /// The comparison reads with `sequential_jobs` because it streams whole
    /// files and that genuinely wants every core. Opening them is the other
    /// shape — one catalog, and past the performance cores the readers only
    /// queue deeper. Measured over `widegroup`'s 1,827 files, open and close
    /// and nothing else, best of nine: **5.54 ms on four readers against
    /// 10.60 on eight.** So the opens go to the same shallow pool the page
    /// probes use, and the reads stay where they were.
    pub open_pool: Option<&'a ThreadPool>,
}

/// What one comparison settled: the runs that agree to the last byte, and the
/// whole-file digests learned on the way. The digests are handed back rather
/// than written straight to the cache, so nothing has to lock it from inside
/// the pool.
#[derive(Default)]
pub struct Settled {
    pub matched: Vec<Vec<usize>>,
    pub learned: Vec<(usize, Digest)>,
}

impl Settled {
    fn absorb(&mut self, other: Settled) {
        self.matched.extend(other.matched);
        self.learned.extend(other.learned);
    }
}

/// How much to read from each file per round.
///
/// Flash likes small chunks; a spinning disk pays a seek every time we move to
/// the next file in the group, so it wants big ones. Either way a round holds
/// one chunk per member, so a group with a lot of copies gets a smaller chunk
/// to stay inside the budget.
///
/// The `spinning` profile does not shrink it, because it does not have to: it
/// never reads more than `SPIN_INTERLEAVE` files at once, and that many
/// `SLOW_CHUNK`s is exactly `COMPARE_BUDGET`. Shrinking the run is the one
/// thing that would undo the profile — a wide group is where the reads most
/// need to stay long.
pub fn compare_chunk(profile: Profile, members: usize, solid: bool) -> usize {
    if profile == Profile::Spinning {
        return SLOW_CHUNK;
    }
    let preferred = if solid { CHUNK } else { SLOW_CHUNK };
    MIN_CHUNK.max(preferred.min(COMPARE_BUDGET / members.max(1)))
}

/// Split (tag, block) pairs into runs that agree on this chunk.
///
/// Comparing against a pivot rather than hashing each block is the point of
/// the whole stage: a memcmp against a block that differs early costs
/// essentially nothing next to a digest of the whole thing. That stops holding
/// when a bucket is mostly distinct content, where every pass strips off just
/// the pivot and the loop goes quadratic; when a pass does that to a bucket
/// that is still large, the rest is grouped on the block itself in one pass.
///
/// That stays byte-exact. A digest only picks which slot to look in — equality
/// still decides, because each slot is re-partitioned by the same pivot loop.
/// Below this many bytes in a pass, the pivot comparison is done on one
/// thread: splitting it costs more than the `memcmp` saves. Above it, on a
/// wide group of large blocks, this is the whole cost of the stage.
const PARTITION_SPREAD: usize = 256 * 1024;

fn partition_blocks<B: AsRef<[u8]> + Send + Sync>(items: Vec<(usize, B)>) -> Vec<Vec<(usize, B)>> {
    let mut runs: Vec<Vec<(usize, B)>> = Vec::new();
    let mut pending = items;
    while !pending.is_empty() {
        let mut iter = pending.into_iter();
        let pivot = iter.next().expect("pending is not empty");
        let others: Vec<(usize, B)> = iter.collect();

        // Every block is compared against the pivot and against nothing else,
        // so the comparisons are independent — and on a wide group of large
        // blocks they are the whole of what this stage does: `widegroup` puts
        // 227 blocks of a quarter of a megabyte through here, per group, and
        // used to do it on one thread.
        //
        // The verdicts are collected in order and the split is done after, so
        // a run holds its members in the same order it always did. Nothing but
        // the `memcmp` moves.
        let bytes = others.len().saturating_mul(pivot.1.as_ref().len());
        let equal: Vec<bool> = if bytes >= PARTITION_SPREAD {
            others
                .par_iter()
                .map(|item| item.1.as_ref() == pivot.1.as_ref())
                .collect()
        } else {
            others
                .iter()
                .map(|item| item.1.as_ref() == pivot.1.as_ref())
                .collect()
        };

        let mut same = vec![pivot];
        let mut rest = Vec::new();
        for (item, alike) in others.into_iter().zip(equal) {
            if alike {
                same.push(item);
            } else {
                rest.push(item);
            }
        }
        let lone = same.len() == 1;
        runs.push(same);
        if lone && rest.len() > PARTITION_FANOUT {
            let mut by_digest: Ordered<Digest, (usize, B)> = Ordered::new();
            for item in rest {
                by_digest.push(sha256(item.1.as_ref()), item);
            }
            for slot in by_digest.into_values() {
                runs.extend(partition_blocks(slot));
            }
            return runs;
        }
        pending = rest;
    }
    runs
}

fn tags<B>(run: Vec<(usize, B)>) -> Vec<usize> {
    run.into_iter().map(|(tag, _)| tag).collect()
}

/// Compare one group of same-size candidates and return the runs that agree to
/// the last byte.
pub fn split_identical(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    chunk: usize,
    unreadable: &mut Vec<String>,
    pool: Option<&ThreadPool>,
) -> Settled {
    let mut settled = Settled::default();

    // A short file that stage 3 read to its end is already here in full.
    // Opening it again to read nothing costs an open against the stat that
    // says the file is still the one we read.
    let held: Vec<&FileRec> = members.iter().map(|&index| &records[index]).collect();
    if members.len() > 1
        && held
            .iter()
            .all(|record| record.head().len() >= record.size as usize)
        && unchanged(context.root, &held)
    {
        let blocks = members
            .iter()
            .map(|&index| {
                let record = &records[index];
                (index, &record.head()[..record.size as usize])
            })
            .collect();
        for run in partition_blocks(blocks) {
            if run.len() < 2 {
                continue;
            }
            if context.learn {
                // The whole file is in hand and it matched, so this is the
                // same proof the reading path produces, learned for free.
                let digest = sha256(run[0].1);
                settled
                    .learned
                    .extend(run.iter().map(|&(index, _)| (index, digest)));
            }
            settled.matched.push(tags(run));
        }
        return settled;
    }

    // Every other stage opens in parallel and this one did not: a wide group
    // is a thousand-odd opens on one thread while eight readers wait for it.
    // Collected in `members` order, so which file lands in which slot — and
    // the order `unreadable` is appended in — is what it always was.
    let files: Vec<Option<File>> = {
        let open = |&index: &usize| {
            context
                .effort
                .open(&context.root.join(records[index].path()))
                .ok()
        };
        match context.open_pool.or(pool) {
            Some(pool) => pool.install(|| members.par_iter().map(open).collect()),
            None => members.iter().map(open).collect(),
        }
    };
    let mut opened: Vec<usize> = Vec::new();
    let mut handles: Vec<Mutex<File>> = Vec::new();
    for (&index, file) in members.iter().zip(files) {
        match file {
            Some(file) => {
                opened.push(index);
                handles.push(Mutex::new(file));
            }
            None => unreadable.push(records[index].path().to_string()),
        }
    }

    // Whatever stage 3 kept is compared here instead of being read again — the
    // bytes themselves, not a digest of them, so the proof is the same proof.
    // Bytes on disk that have moved since they were read are not the bytes we
    // are about to delete something over, so the first stat that disagrees
    // puts the whole group back on reading from zero.
    let mut prefix = opened
        .iter()
        .map(|&index| records[index].head().len())
        .min()
        .unwrap_or(0);
    let mut tail = opened
        .iter()
        .map(|&index| records[index].tail().len())
        .min()
        .unwrap_or(0);
    if tail > 0
        && opened
            .iter()
            .any(|&index| prefix + tail > records[index].size as usize)
    {
        tail = 0; // the two kept runs would overlap
    }
    if prefix > 0 || tail > 0 {
        for (slot, &index) in opened.iter().enumerate() {
            let file = handles[slot].lock().expect("no reader panics");
            if !still_the_same(&file, &records[index]).unwrap_or(false) {
                prefix = 0;
                tail = 0;
                break;
            }
        }
    }
    if prefix > 0 {
        for handle in &handles {
            let mut file = handle.lock().expect("no reader panics");
            if file.seek(SeekFrom::Start(prefix as u64)).is_err() {
                prefix = 0;
                break;
            }
        }
    }
    // Read up to where the kept tail begins; the tail settles the rest.
    let limit = (tail > 0).then(|| {
        opened
            .iter()
            .map(|&index| records[index].size as usize - tail)
            .min()
            .unwrap_or(0)
    });

    // The digest being learned has to cover the file, not just the part of it
    // this call happens to read. Kept bytes are part of the file too, so they
    // go into the hasher in their place: head first, then everything read,
    // then the tail as each run finishes. A digest built any other way would be
    // remembered as a whole-file digest without being one, and the next scan
    // would match two files on their middles alone.
    let mut hashers: Vec<Sha256> = if context.learn {
        opened
            .iter()
            .map(|&index| {
                let mut hasher = Sha256::new();
                if prefix > 0 {
                    hasher.update(&records[index].head()[..prefix]);
                }
                hasher
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut live: Vec<Vec<usize>> = if opened.len() < 2 {
        Vec::new()
    } else if prefix > 0 {
        partition_blocks(
            opened
                .iter()
                .enumerate()
                .map(|(slot, &index)| (slot, &records[index].head()[..prefix]))
                .collect(),
        )
        .into_iter()
        .map(tags)
        .filter(|run| run.len() > 1) // settled on kept bytes alone
        .collect()
    } else {
        vec![(0..opened.len()).collect()]
    };

    let mut matched: Vec<Vec<usize>> = Vec::new();
    // Where each handle actually is, counted from the bytes that came back
    // rather than from the bytes asked for: a read stopped short by the end of
    // the file leaves the handle where the bytes stopped, and a seek count
    // built on the request instead would charge for a jump nobody made.
    let mut positions: Vec<u64> = vec![prefix as u64; opened.len()];
    let mut position = prefix;
    loop {
        if live.is_empty() {
            break;
        }
        let span = match limit {
            None => chunk,
            Some(limit) => chunk.min(limit.saturating_sub(position)),
        };
        if span == 0 {
            break; // everything up to the kept tail agrees
        }
        position += span;
        let mut following: Vec<Vec<usize>> = Vec::new();
        for group in std::mem::take(&mut live) {
            let blocks: Vec<(usize, Option<Vec<u8>>)> = {
                let at = &positions;
                let read = |slot: &usize| -> (usize, Option<Vec<u8>>) {
                    let mut file = handles[*slot].lock().expect("no reader panics");
                    let where_from = records[opened[*slot]].at(at[*slot]);
                    (
                        *slot,
                        context.effort.read_block(&mut file, span, where_from).ok(),
                    )
                };
                match pool {
                    Some(pool) => pool.install(|| group.par_iter().map(read).collect()),
                    None => group.iter().map(read).collect(),
                }
            };
            let mut good = Vec::with_capacity(blocks.len());
            for (slot, block) in blocks {
                match block {
                    Some(block) => {
                        positions[slot] += block.len() as u64;
                        good.push((slot, block));
                    }
                    None => unreadable.push(records[opened[slot]].path().to_string()),
                }
            }
            if context.learn {
                for (slot, block) in &good {
                    hashers[*slot].update(block);
                }
            }

            for run in partition_blocks(good) {
                let at_end = run[0].1.is_empty();
                let members = tags(run);
                if members.len() < 2 {
                    continue; // settled, stop reading
                } else if at_end {
                    // Reaching EOF here means the read covered everything after
                    // the kept head. With a kept tail in hand the run stops
                    // short of EOF instead and is learned below, once that tail
                    // has gone into the digest.
                    if context.learn && tail == 0 {
                        for &slot in &members {
                            settled
                                .learned
                                .push((opened[slot], hashers[slot].clone().finalize().into()));
                        }
                    }
                    matched.push(members); // same bytes to EOF
                } else {
                    following.push(members); // still together
                }
            }
        }
        live = following;
    }

    // Only reached with a kept tail in hand: the rounds above stopped short of
    // EOF and these bytes are what is left of the file.
    for group in live {
        let blocks = group
            .into_iter()
            .map(|slot| {
                let kept = records[opened[slot]].tail();
                (slot, &kept[kept.len() - tail..])
            })
            .collect();
        for run in partition_blocks(blocks) {
            if run.len() < 2 {
                continue;
            }
            if context.learn {
                for (slot, block) in &run {
                    hashers[*slot].update(*block);
                    settled
                        .learned
                        .push((opened[*slot], hashers[*slot].clone().finalize().into()));
                }
            }
            matched.push(tags(run));
        }
    }

    settled.matched.extend(
        matched
            .into_iter()
            .map(|run| run.into_iter().map(|slot| opened[slot]).collect()),
    );
    settled
}

/// The exact partition of one cohort, and which of its members could not be
/// read.
///
/// `split_identical` reports only the runs worth having — two or more members
/// that agree to the last byte — because that is all its caller wants. The
/// capped comparison wants the whole partition: a file that matched nobody in
/// this cohort still has to be carried to the next pivot, where it may match
/// somebody. A file that could not be READ must not be, so the two are told
/// apart here rather than guessed at.
fn compare_cohort(
    context: &Compare,
    records: &[FileRec],
    cohort: &[usize],
    chunk: usize,
    settled: &mut Settled,
    unreadable: &mut Vec<String>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut reported: Vec<String> = Vec::new();
    let outcome = split_identical(context, records, cohort, chunk, &mut reported, None);
    settled.learned.extend(outcome.learned);
    let mut runs = outcome.matched;
    let accounted: std::collections::HashSet<usize> = runs.iter().flatten().copied().collect();
    let mut lost = Vec::new();
    for &index in cohort {
        if accounted.contains(&index) {
            continue;
        }
        // Every path in `reported` came from this call, and a record's path is
        // unique in the scan, so this says exactly whether THIS member is the
        // one that failed.
        if reported.iter().any(|path| path == records[index].path()) {
            lost.push(index);
        } else {
            runs.push(vec![index]); // read, and equal to nobody here
        }
    }
    unreadable.extend(reported);
    (runs, lost)
}

/// Compare a group without ever holding more than `SPIN_INTERLEAVE` files open
/// at once, carrying what each pass proved into the next.
///
/// A round of the ordinary comparison reads one chunk from every member in
/// turn. On flash that is free. On a spinning disk a group of forty copies
/// means the heads visit forty places to advance one chunk, over and over, and
/// the read that should have been sequential becomes a seek per member per
/// chunk. So the group is compared a cohort at a time instead, and members are
/// read more than once — which SPIN.md asks for by name: the profile trades
/// bytes for seeks.
///
/// **Why the answer is the same answer.** A *cluster* here is a set of records
/// already proved byte-equal to each other. Each round takes one cluster's
/// representative as the pivot and compares it, in cohorts of at most
/// `SPIN_INTERLEAVE`, against the representative of every other cluster. Two
/// facts make that exact:
///
/// - `split_identical` on a cohort is the same lockstep comparison the flash
///   profile runs, so its runs are exactly the members of that cohort whose
///   bytes agree to the last one.
/// - Byte equality is transitive, so a representative that matches the pivot
///   brings its whole cluster with it, and two representatives that match each
///   other fuse their clusters. Every pair in a reported group is joined by a
///   chain of comparisons that each read real bytes this scan, which is what
///   §3.1 asks for. It is the same reasoning `partition_blocks` already
///   depends on, where a run is every block equal to the run's first.
///
/// Nothing is lost, either: a member that does not match the pivot goes back
/// into the pool and is offered to every later pivot, and each round removes
/// exactly one cluster from the pool, so every pair meets.
pub fn split_identical_capped(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    chunk: usize,
    unreadable: &mut Vec<String>,
) -> Settled {
    let mut settled = Settled::default();
    if members.len() <= SPIN_INTERLEAVE {
        // Narrow enough to compare in one pass, which is the ordinary
        // comparison exactly.
        return split_identical(context, records, members, chunk, unreadable, None);
    }

    let mut clusters: Vec<Vec<usize>> = members.iter().map(|&index| vec![index]).collect();
    while clusters.len() > 1 {
        let mut round = Round::around(clusters.remove(0));
        for batch in clusters.chunks(SPIN_INTERLEAVE - 1) {
            let mut cohort = Vec::with_capacity(batch.len() + 1);
            cohort.push(round.pivot);
            cohort.extend(batch.iter().map(|cluster| cluster[0]));
            let (runs, lost) =
                compare_cohort(context, records, &cohort, chunk, &mut settled, unreadable);
            round.fold(batch, &runs, &lost);
        }
        clusters = round.close(&mut settled.matched);
    }
    // Whatever the last round left: already proved equal among themselves.
    if let Some(last) = clusters.pop() {
        if last.len() > 1 {
            settled.matched.push(last);
        }
    }
    settled
}

/// One pivot's sweep over the pool, in the middle of being taken.
///
/// The bookkeeping is separated from the reading for the reason M5-TUI.md gives
/// for separating the model from the paint: the interesting case is a file that
/// goes away between one cohort and the next, and no test can stage that with a
/// real file. Here it is three arguments.
struct Round {
    pivot: usize,
    /// The pivot's cluster, and everything proved equal to it this sweep.
    merged: Vec<usize>,
    /// Clusters for the next pivot to try, in the order they were met.
    leftovers: Vec<Vec<usize>>,
    /// Whether some cohort could not read the pivot. Nothing that cohort saw
    /// was measured against anything, so the sweep proves less than it looks
    /// like it did.
    lost_the_pivot: bool,
}

impl Round {
    fn around(cluster: Vec<usize>) -> Round {
        Round {
            pivot: cluster[0],
            merged: cluster,
            leftovers: Vec::new(),
            lost_the_pivot: false,
        }
    }

    /// Take in what one cohort came back with: `runs` partitions the cohort
    /// members that were read, `lost` names the ones that were not.
    fn fold(&mut self, batch: &[Vec<usize>], runs: &[Vec<usize>], lost: &[usize]) {
        let cluster_of = |representative: usize| -> &[usize] {
            batch
                .iter()
                .find(|cluster| cluster[0] == representative)
                .expect("every name in a cohort came from this batch")
        };
        self.lost_the_pivot |= lost.contains(&self.pivot);

        for run in runs {
            let mut fused: Vec<usize> = Vec::new();
            let with_pivot = run.contains(&self.pivot);
            for &representative in run {
                if representative == self.pivot {
                    continue;
                }
                if with_pivot {
                    self.merged.extend_from_slice(cluster_of(representative));
                } else {
                    fused.extend_from_slice(cluster_of(representative));
                }
            }
            if !fused.is_empty() {
                self.leftovers.push(fused);
            }
        }

        // A cluster whose representative could not be read keeps its other
        // members: they were proved equal to each other before the file went
        // away, and one of them speaks for the cluster next time.
        for &index in lost {
            if index == self.pivot {
                continue;
            }
            let rest = cluster_of(index)[1..].to_vec();
            if !rest.is_empty() {
                self.leftovers.push(rest);
            }
        }
    }

    /// Finish the sweep: settle the pivot's class if there is one, and hand
    /// back the pool for the next pivot.
    fn close(mut self, matched: &mut Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        if self.lost_the_pivot {
            // Everything this sweep did NOT put in `merged` was sent to the
            // pool without ever being measured against the pivot, so settling
            // `merged` here would strand its members: a file equal to them,
            // sitting in the pool, would never meet them again. The cluster
            // goes back in without the pivot and takes its turn afresh.
            self.merged.retain(|&index| index != self.pivot);
            if !self.merged.is_empty() {
                self.leftovers.insert(0, self.merged);
            }
        } else if self.merged.len() > 1 {
            matched.push(self.merged);
        }
        self.leftovers
    }
}

/// Run `split_identical` over every candidate group.
///
/// A group big enough on its own gets the whole pool to itself, members read
/// in parallel: a folder whose duplicates are two enormous files is one group,
/// and it should still use the whole disk queue. Everything smaller is packed
/// with its layout-sorted neighbours into a wave, and every group in the wave
/// runs as its own task instead of the wave waiting on one group's turn — a
/// tree of thousands of two-file groups used to leave most of `-j` idle. The
/// chunk is sized on the whole wave's member count, so the wave's aggregate
/// memory stays inside `COMPARE_BUDGET`.
pub fn compare_stage(
    context: &Compare,
    records: &[FileRec],
    buckets: &[Vec<usize>],
    jobs: usize,
    pool: &ThreadPool,
    unreadable: &mut Vec<String>,
) -> Settled {
    // One disk, one queue, one group at a time, in the order the buckets were
    // sorted into — which is inode order, so the whole stage walks the platter
    // forwards. Waves exist to keep several readers busy and there is only one.
    if context.profile == Profile::Spinning {
        let mut out = Settled::default();
        for bucket in buckets {
            let chunk = compare_chunk(context.profile, bucket.len(), context.solid);
            out.absorb(split_identical_capped(
                context, records, bucket, chunk, unreadable,
            ));
        }
        return out;
    }

    // 4x the reader count so there is enough work to keep every reader busy
    // even once a group or two settles early, floored at 16 so a low -j does
    // not turn nearly everything into a wave of two.
    let wave_members = (4 * jobs).max(16);
    let mut out = Settled::default();
    let spread = (jobs > 1).then_some(pool);

    let mut index = 0;
    while index < buckets.len() {
        if buckets[index].len() >= wave_members {
            let chunk = compare_chunk(context.profile, buckets[index].len(), context.solid);
            out.absorb(split_identical(
                context,
                records,
                &buckets[index],
                chunk,
                unreadable,
                spread,
            ));
            index += 1;
            continue;
        }

        let mut total = buckets[index].len();
        let mut end = index + 1;
        while total < wave_members && end < buckets.len() && buckets[end].len() < wave_members {
            total += buckets[end].len();
            end += 1;
        }
        let wave = &buckets[index..end];
        index = end;

        if wave.len() == 1 {
            // Nothing left to pack it with, and it never reached the threshold
            // alone either.
            let chunk = compare_chunk(context.profile, wave[0].len(), context.solid);
            out.absorb(split_identical(
                context, records, &wave[0], chunk, unreadable, spread,
            ));
            continue;
        }

        let wave_chunk = compare_chunk(context.profile, total, context.solid);
        let results: Vec<(Settled, Vec<String>)> = pool.install(|| {
            wave.par_iter()
                .map(|group| {
                    let mut local = Vec::new();
                    // pool = None: nothing here submits pool work from inside
                    // pool work, so each task reads its own members serially.
                    let settled =
                        split_identical(context, records, group, wave_chunk, &mut local, None);
                    (settled, local)
                })
                .collect()
        });
        for (settled, local) in results {
            out.absorb(settled);
            unreadable.extend(local);
        }
    }
    out
}

/// Byte-compare every group matched from memory rather than read.
///
/// Returns (safe to act on, refused). Nothing from a refused group may be
/// deleted: its members disagree, so whatever was remembered of them was
/// wrong, and the tool has no idea which copy is which. A group compared
/// during this scan is already proved and is not read again.
pub fn verify_attested(
    context: &Compare,
    records: &[FileRec],
    groups: Vec<DupGroup>,
    cache: Option<&mut ScanCache>,
) -> (Vec<DupGroup>, Vec<DupGroup>) {
    let mut safe = Vec::new();
    let mut refused = Vec::new();
    let mut forget: Vec<usize> = Vec::new();
    for mut group in groups {
        if !group.attested {
            safe.push(group);
            continue;
        }
        let mut unreadable = Vec::new();
        let chunk = compare_chunk(context.profile, group.members.len(), context.solid);
        // The same width cap as the scan used, for the same reason: a wide
        // attested group re-proved at deletion time would otherwise open every
        // member at once on the one disk that cannot afford it.
        let settled = if context.profile == Profile::Spinning {
            split_identical_capped(context, records, &group.members, chunk, &mut unreadable)
        } else {
            split_identical(
                context,
                records,
                &group.members,
                chunk,
                &mut unreadable,
                None,
            )
        };
        if settled.matched.len() == 1
            && settled.matched[0].len() == group.members.len()
            && unreadable.is_empty()
        {
            group.attested = false; // proved, here, now
            safe.push(group);
        } else {
            forget.extend(group.members.iter().copied());
            refused.push(group);
        }
    }
    if let Some(cache) = cache {
        let doubted: Vec<&FileRec> = forget.iter().map(|&index| &records[index]).collect();
        cache.forget(&doubted);
    }
    (safe, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(items: &[(usize, &[u8])]) -> Vec<(usize, Vec<u8>)> {
        items
            .iter()
            .map(|(tag, block)| (*tag, block.to_vec()))
            .collect()
    }

    #[test]
    fn a_pass_groups_exactly_the_blocks_that_are_equal() {
        let runs = partition_blocks(blocks(&[
            (0, b"aaa"),
            (1, b"bbb"),
            (2, b"aaa"),
            (3, b"ccc"),
            (4, b"bbb"),
        ]));
        let mut got: Vec<Vec<usize>> = runs.into_iter().map(tags).collect();
        got.sort();
        assert_eq!(got, [vec![0, 2], vec![1, 4], vec![3]]);
    }

    #[test]
    fn the_fanout_fallback_still_decides_on_equality() {
        // One pivot that matches nobody, then more distinct blocks than the
        // fanout guard tolerates: the digest picks the slot, memcmp decides.
        let mut items = vec![(0_usize, b"pivot".to_vec())];
        for tag in 1..=(PARTITION_FANOUT + 2) {
            items.push((tag, format!("block-{tag}").into_bytes()));
        }
        items.push((PARTITION_FANOUT + 3, b"block-1".to_vec()));
        let runs = partition_blocks(items);
        let paired: Vec<Vec<usize>> = runs
            .into_iter()
            .map(tags)
            .filter(|run| run.len() > 1)
            .collect();
        assert_eq!(paired, [vec![1, PARTITION_FANOUT + 3]]);
    }

    #[test]
    fn the_chunk_shrinks_with_the_member_count_but_not_below_the_floor() {
        let flash = Profile::Flash;
        assert_eq!(compare_chunk(flash, 2, true), CHUNK);
        assert_eq!(compare_chunk(flash, 2, false), SLOW_CHUNK);
        // The budget only starts biting past COMPARE_BUDGET / CHUNK members;
        // below that the chunk is what flash prefers.
        assert_eq!(compare_chunk(flash, 64, true), CHUNK);
        assert_eq!(compare_chunk(flash, 512, true), COMPARE_BUDGET / 512);
        assert!(COMPARE_BUDGET / 512 < CHUNK, "512 members must shrink it");
        assert_eq!(compare_chunk(flash, 100_000, true), MIN_CHUNK);
    }

    #[test]
    fn the_spinning_chunk_stays_long_however_wide_the_group_is() {
        let spin = Profile::Spinning;
        for members in [2, 8, 64, 100_000] {
            assert_eq!(compare_chunk(spin, members, true), SLOW_CHUNK);
            assert_eq!(compare_chunk(spin, members, false), SLOW_CHUNK);
        }
        // ...and the width cap is what keeps that inside the budget.
        assert_eq!(SLOW_CHUNK * SPIN_INTERLEAVE, COMPARE_BUDGET);
    }

    // ------------------------------------------------ the capped comparison

    /// A scratch tree of same-size files, one per content class.
    struct Bench {
        root: std::path::PathBuf,
        records: Vec<FileRec>,
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            for record in &self.records {
                let _ = std::fs::set_permissions(
                    self.root.join(record.path()),
                    std::os::unix::fs::PermissionsExt::from_mode(0o644),
                );
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Bench {
        /// One file per letter, every file the same length, files sharing a
        /// letter sharing their bytes. `chunk` says how long, so a caller can
        /// choose whether the comparison takes one round or several.
        fn new(name: &str, classes: &[u8], size: usize) -> Bench {
            let root = std::env::temp_dir().join(format!(
                "feat-capped-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("a scratch directory");
            let mut records = Vec::new();
            for (position, &letter) in classes.iter().enumerate() {
                // Same bytes everywhere but the last kilobyte, so nothing can
                // separate two files without reading them to the end.
                let mut content = vec![0x5A_u8; size];
                let tail = content.len() - 1024;
                content[tail..].fill(letter);
                let path = format!("f{position:03}.bin");
                std::fs::write(root.join(&path), &content).expect("a scratch file");
                let meta = std::fs::metadata(root.join(&path)).expect("a stat");
                use std::os::unix::fs::MetadataExt;
                records.push(FileRec {
                    names: vec![path],
                    size: meta.size(),
                    dev: meta.dev(),
                    ino: meta.ino(),
                    nlink: meta.nlink(),
                    mtime_ns: 0,
                    retained: None,
                });
            }
            Bench { root, records }
        }

        fn lock(&self, position: usize) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                self.root.join(self.records[position].path()),
                std::fs::Permissions::from_mode(0o000),
            )
            .expect("a scratch file we own");
        }

        /// The partition each comparison reaches, normalised so two answers
        /// can be compared regardless of the order they were found in.
        fn partitions(&self, chunk: usize) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
            let members: Vec<usize> = (0..self.records.len()).collect();
            let effort = Effort::default();
            let mut sink = Vec::new();
            let plain = split_identical(
                &Compare {
                    root: &self.root,
                    effort: &effort,
                    profile: Profile::Flash,
                    solid: true,
                    learn: false,
                    open_pool: None,
                },
                &self.records,
                &members,
                chunk,
                &mut sink,
                None,
            );
            let mut sink = Vec::new();
            let capped = split_identical_capped(
                &Compare {
                    root: &self.root,
                    effort: &effort,
                    profile: Profile::Spinning,
                    solid: true,
                    learn: false,
                    open_pool: None,
                },
                &self.records,
                &members,
                chunk,
                &mut sink,
            );
            (tidy(plain.matched), tidy(capped.matched))
        }
    }

    fn tidy(mut groups: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        for group in groups.iter_mut() {
            group.sort_unstable();
        }
        groups.sort();
        groups
    }

    #[test]
    fn a_group_wider_than_the_cap_still_comes_back_whole() {
        // Three times the cohort width, every file the same: the sweep has to
        // gather all of them into one class, across four cohorts.
        let bench = Bench::new("wide", &[b'A'; 24], 8192);
        let (plain, capped) = bench.partitions(4096);
        assert_eq!(capped, vec![(0..24).collect::<Vec<usize>>()]);
        assert_eq!(capped, plain);
    }

    #[test]
    fn equal_members_split_across_cohorts_are_still_found() {
        // The shape SPIN.md names: with a cap of 8 the first round compares
        // the pivot against members 1..=7, then against 8..=14. Members 7 and
        // 8 are the same file's content and never meet in that round. Only a
        // later pass over what the first one left can put them together.
        assert_eq!(SPIN_INTERLEAVE, 8, "this shape is built around the cap");
        let classes = *b"ACCCCCCBBDDDDDDA";
        let bench = Bench::new("boundary", &classes, 8192);
        let (plain, capped) = bench.partitions(4096);
        assert_eq!(
            capped,
            vec![
                vec![0, 15],                 // A, either end of the group
                vec![1, 2, 3, 4, 5, 6],      // C, all inside the first cohort
                vec![7, 8],                  // B, one in each cohort of the first round
                vec![9, 10, 11, 12, 13, 14], // D, all inside the second
            ]
        );
        assert_eq!(capped, plain);
    }

    #[test]
    fn a_member_that_matches_nobody_is_carried_to_every_later_pass() {
        // The singletons are what a naive cohort split loses: each one has to
        // be offered to every pivot before it can be called unique.
        let classes = *b"ABCDEFGHIJKLMNOPA";
        let bench = Bench::new("singletons", &classes, 8192);
        let (plain, capped) = bench.partitions(4096);
        assert_eq!(capped, vec![vec![0, 16]]);
        assert_eq!(capped, plain);
    }

    #[test]
    fn a_group_taking_several_rounds_is_partitioned_the_same_way() {
        // Chunk smaller than the file, so the comparison runs to several
        // rounds inside each cohort and the divergence is in the last one.
        let classes = *b"AABBBCAAAAABBCCCCA";
        let bench = Bench::new("rounds", &classes, 32 * 1024);
        let (plain, capped) = bench.partitions(4096);
        assert_eq!(capped, plain);
        assert_eq!(
            capped,
            vec![
                vec![0, 1, 6, 7, 8, 9, 10, 17],
                vec![2, 3, 4, 11, 12],
                vec![5, 13, 14, 15, 16],
            ]
        );
    }

    #[test]
    fn the_capped_sweep_agrees_with_the_uncapped_one_on_every_shape() {
        // The property that matters, over shapes nobody chose by hand: the two
        // comparisons are the same function. A partition carried wrongly
        // between passes shows up here as a group the plain comparison did not
        // find, or one it did.
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |ceiling: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % ceiling
        };
        for shape in 0..40 {
            let width = 2 + next(30) as usize;
            let variety = 1 + next(6) as u8;
            let classes: Vec<u8> = (0..width)
                .map(|_| b'A' + next(variety as u64) as u8)
                .collect();
            let bench = Bench::new(&format!("shape{shape}"), &classes, 8192);
            let (plain, capped) = bench.partitions(4096);
            assert_eq!(
                capped,
                plain,
                "shape {shape}: {:?}",
                String::from_utf8_lossy(&classes)
            );
        }
    }

    #[test]
    fn a_member_that_cannot_be_read_joins_no_group_and_stops_nobody_else() {
        let classes = *b"AAAAAAAAAAAABBBBBB";
        let bench = Bench::new("locked", &classes, 8192);
        // One from each class, one of them the very first file — which is the
        // first round's pivot, the case that has to unwind a whole sweep.
        bench.lock(0);
        bench.lock(13);
        let members: Vec<usize> = (0..bench.records.len()).collect();
        let effort = Effort::default();
        let mut unreadable = Vec::new();
        let capped = split_identical_capped(
            &Compare {
                root: &bench.root,
                effort: &effort,
                profile: Profile::Spinning,
                solid: true,
                learn: false,
                open_pool: None,
            },
            &bench.records,
            &members,
            4096,
            &mut unreadable,
        );
        assert_eq!(
            tidy(capped.matched),
            vec![
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
                vec![12, 14, 15, 16, 17]
            ]
        );
        unreadable.sort();
        unreadable.dedup();
        assert_eq!(unreadable, vec!["f000.bin", "f013.bin"]);
    }

    // ------------------------------------------- the sweep's own bookkeeping

    #[test]
    fn a_run_holding_the_pivot_brings_whole_clusters_with_it() {
        let mut round = Round::around(vec![1, 2]);
        let batch = vec![vec![3, 4], vec![5], vec![6, 7, 8]];
        // 3 matched the pivot; 5 and 6 matched each other but not it.
        round.fold(&batch, &[vec![1, 3], vec![5, 6]], &[]);
        let mut matched = Vec::new();
        let pool = round.close(&mut matched);
        assert_eq!(matched, vec![vec![1, 2, 3, 4]]);
        assert_eq!(pool, vec![vec![5, 6, 7, 8]]);
    }

    #[test]
    fn a_cluster_whose_representative_goes_away_keeps_its_other_members() {
        let mut round = Round::around(vec![1]);
        let batch = vec![vec![2, 3, 4], vec![5]];
        // 2 could not be read this pass. 3 and 4 were proved equal to it
        // before it went, so they stay in the pool as a cluster.
        round.fold(&batch, &[vec![1, 5]], &[2]);
        let mut matched = Vec::new();
        let pool = round.close(&mut matched);
        assert_eq!(matched, vec![vec![1, 5]]);
        assert_eq!(pool, vec![vec![3, 4]]);
    }

    #[test]
    fn a_pivot_that_goes_away_mid_sweep_settles_nothing_and_goes_back_in() {
        // The case a file on disk cannot be made to produce: the pivot is read
        // for the first cohort and gone by the second. What it proved in the
        // first is real, but the second cohort's members were never measured
        // against it — so settling the class now would strand them.
        let mut round = Round::around(vec![1]);
        round.fold(&[vec![2], vec![3]], &[vec![1, 2], vec![3]], &[]);
        round.fold(&[vec![4], vec![5]], &[vec![4, 5]], &[1]);
        let mut matched = Vec::new();
        let pool = round.close(&mut matched);
        assert!(
            matched.is_empty(),
            "nothing may be settled on a pivot half the sweep could not read"
        );
        // 2 was proved equal to the pivot, so it leads the cluster now; 4 and
        // 5 are still to be offered to it.
        assert_eq!(pool, vec![vec![2], vec![3], vec![4, 5]]);
    }

    #[test]
    fn a_pivot_alone_and_unreadable_leaves_nothing_behind() {
        let mut round = Round::around(vec![9]);
        round.fold(&[vec![1], vec![2]], &[vec![1, 2]], &[9]);
        let mut matched = Vec::new();
        let pool = round.close(&mut matched);
        assert!(matched.is_empty());
        assert_eq!(pool, vec![vec![1, 2]], "9 is in no group and no pool");
    }

    #[test]
    fn the_cohorts_never_hold_more_than_the_cap_open_at_once() {
        // The whole point of the profile. `split_identical` opens every member
        // it is given and charges an open for each, so the widest cohort is
        // visible in the open count: a sweep of 24 files that opened them all
        // at once would be the unchanged comparison wearing a new name.
        let bench = Bench::new("width", &[b'A'; 24], 8192);
        let members: Vec<usize> = (0..24).collect();
        let effort = Effort::default();
        let mut sink = Vec::new();
        split_identical_capped(
            &Compare {
                root: &bench.root,
                effort: &effort,
                profile: Profile::Spinning,
                solid: true,
                learn: false,
                open_pool: None,
            },
            &bench.records,
            &members,
            4096,
            &mut sink,
        );
        // One round of four cohorts: 8 + 8 + 8 + 3 files, the pivot re-opened
        // in each. Anything wider would mean the cap is not being applied.
        assert_eq!(effort.opens(), 27);
    }
}
