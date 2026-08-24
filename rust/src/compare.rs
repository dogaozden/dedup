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
    CHUNK, COMPARE_BUDGET, MIN_CHUNK, PARTITION_FANOUT, PIVOT_GROWTH, PIVOT_RUN, PIVOT_WINDOW,
    SLOW_CHUNK, SPIN_INTERLEAVE,
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

/// What the preliminaries came to: the whole answer, for a group already in
/// memory, or the open handles and the kept-byte bookkeeping a reading loop
/// works from.
enum Prepared {
    Answered(Settled),
    Reading(Reading),
}

/// A group opened and positioned, with whatever the kept bytes could settle
/// already settled.
///
/// Both comparisons start from exactly this. They differ only in the order
/// they read what is left: lockstep advances every member a chunk at a time,
/// the spinning sweep holds one member's window and streams the others past
/// it.
struct Reading {
    /// Record index per slot, in `members` order, for the members that opened.
    opened: Vec<usize>,
    handles: Vec<Mutex<File>>,
    /// Kept head bytes, compared here rather than read a second time.
    prefix: usize,
    /// Kept tail bytes, which settle the end of the file without reading it.
    tail: usize,
    /// Where reading stops so the kept tail is not read twice.
    limit: Option<usize>,
    /// One hasher per slot when `learn`, seeded with the kept head; empty
    /// otherwise.
    hashers: Vec<Sha256>,
    /// Slots the kept head could not tell apart, in `members` order.
    live: Vec<Vec<usize>>,
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
    // One slot per member, in `members` order: the descriptor the staged
    // probes left open on that file, or `None` to open it here. Empty when
    // the pipeline is holding none.
    carried: Vec<Option<File>>,
) -> Settled {
    match prepare(context, records, members, unreadable, pool, carried) {
        Prepared::Answered(settled) => settled,
        Prepared::Reading(reading) => lockstep(context, records, chunk, unreadable, pool, reading),
    }
}

/// Open the group, compare whatever earlier stages kept of it, and leave the
/// handles where reading has to start.
fn prepare(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    unreadable: &mut Vec<String>,
    pool: Option<&ThreadPool>,
    carried: Vec<Option<File>>,
) -> Prepared {
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
        return Prepared::Answered(settled);
    }

    // Every other stage opens in parallel and this one did not: a wide group
    // is a thousand-odd opens on one thread while eight readers wait for it.
    // Collected in `members` order, so which file lands in which slot — and
    // the order `unreadable` is appended in — is what it always was.
    let files: Vec<Option<File>> = {
        // A descriptor the probes already opened is reused; the rest are
        // opened here, in parallel, as they were.
        let paired: Vec<(usize, Option<File>)> = if carried.len() == members.len() {
            members.iter().copied().zip(carried).collect()
        } else {
            members.iter().copied().map(|index| (index, None)).collect()
        };
        let open = |(index, file): (usize, Option<File>)| match file {
            Some(file) => Some(file),
            None => context
                .effort
                .open(&context.root.join(records[index].path()))
                .ok(),
        };
        match pool {
            Some(pool) => pool.install(|| paired.into_par_iter().map(open).collect()),
            None => paired.into_iter().map(open).collect(),
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
    // Always positioned explicitly: a descriptor handed over by the probes is
    // wherever that probe left it, not at zero.
    {
        let mut failed = false;
        for handle in &handles {
            let mut file = handle.lock().expect("no reader panics");
            if file.seek(SeekFrom::Start(prefix as u64)).is_err() {
                failed = true;
                break;
            }
        }
        if failed {
            prefix = 0;
            for handle in &handles {
                let mut file = handle.lock().expect("no reader panics");
                let _ = file.seek(SeekFrom::Start(0));
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
    let hashers: Vec<Sha256> = if context.learn {
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

    let live: Vec<Vec<usize>> = if opened.len() < 2 {
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

    Prepared::Reading(Reading {
        opened,
        handles,
        prefix,
        tail,
        limit,
        hashers,
        live,
    })
}

/// Advance every member of a group a chunk at a time, splitting it the moment
/// two of them disagree.
fn lockstep(
    context: &Compare,
    records: &[FileRec],
    chunk: usize,
    unreadable: &mut Vec<String>,
    pool: Option<&ThreadPool>,
    reading: Reading,
) -> Settled {
    let Reading {
        opened,
        handles,
        prefix,
        tail,
        limit,
        mut hashers,
        mut live,
    } = reading;
    let mut settled = Settled::default();
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

// ---------------------------------------------------- the pivot-window sweep

/// What one member's run against the held window came to.
enum Verdict {
    /// Every byte matched.
    Same,
    /// The run beginning at this offset disagreed.
    Apart(usize),
    /// The member could not be read.
    Gone,
}

/// Why a sweep stopped.
enum Ending {
    /// Nobody is left matching the pivot.
    Alone,
    /// The pivot's file ended, and whoever is left ended with it.
    Ended,
    /// Reading reached the kept tail, which settles what is left.
    AtTail,
    /// The pivot could not be read.
    Lost,
}

/// One pivot's sweep down its group, in the middle of being taken.
///
/// Separated from the reading for the reason `Round` is: the interesting case
/// is a pivot that goes away between one window and the next, and no test can
/// stage that with a real file. Here it is three method calls.
struct Sweep {
    /// Slots still equal to the pivot, in the order they were met.
    live: Vec<usize>,
    /// Slots that left, filed under the offset of the run they left in. Two
    /// members filed under different offsets are already told apart — see the
    /// law on `split_pivot_windows`.
    apart: Ordered<usize, usize>,
    /// How far the live slots have been proved equal to the pivot.
    matched_to: usize,
}

impl Sweep {
    fn over(live: Vec<usize>, from: usize) -> Sweep {
        Sweep {
            live,
            apart: Ordered::new(),
            matched_to: from,
        }
    }

    fn left(&mut self, slot: usize, at: usize) {
        self.apart.push(at, slot);
    }

    fn reached(&mut self, to: usize) {
        self.matched_to = to;
    }

    /// Who is left matching the pivot, and what is still to compare: one class
    /// per run a member left in, each already proved equal as far as that run
    /// begins. A class of one is settled and does not come back.
    fn residue(self) -> (Vec<usize>, Vec<(Vec<usize>, usize)>) {
        let Sweep { live, apart, .. } = self;
        let left = apart
            .into_pairs()
            .into_iter()
            .filter(|(_, slots)| slots.len() > 1)
            .map(|(at, slots)| (slots, at))
            .collect();
        (live, left)
    }

    /// The pivot went away part-way down the group.
    ///
    /// Everything still live agreed with it — and so with each other — as far
    /// as `matched_to`, so it carries on together under a new pivot from
    /// exactly there. Settling the class here instead would rest it on a pivot
    /// the rest of the group was never measured against.
    fn abandoned(self) -> Vec<(Vec<usize>, usize)> {
        let at = self.matched_to;
        let (live, mut left) = self.residue();
        if live.len() > 1 {
            left.insert(0, (live, at));
        }
        left
    }
}

/// How much of a window is read at a time — see `PIVOT_RUN`. Never longer than
/// the chunk, so a caller asking for short reads gets them.
fn pivot_run(chunk: usize) -> usize {
    chunk.min(PIVOT_RUN)
}

/// What one sweep may hold: the pivot's window, and the run of the one member
/// being compared against it.
///
/// There is no term for the group's width. That is the law M7-PIVOT.md sets,
/// and the reason a 1,800-member group costs the same here as a pair — where a
/// lockstep round holds one run per member and shrinks the run to stay inside
/// `COMPARE_BUDGET`.
fn held_ceiling(chunk: usize) -> usize {
    PIVOT_WINDOW.max(chunk) + pivot_run(chunk)
}

/// Compare a group by holding one member's window in memory and streaming the
/// rest of the group past it, a member at a time.
///
/// Lockstep round-robins the head between every member of a group to advance
/// it one chunk. On a spinning disk that is a seek per member per chunk, and it
/// is the wall M7-PIVOT.md was commissioned from: stage 5 on the maintainer's
/// USB disk moved at ~44 MB/s against a ~110-130 MB/s sequential ceiling. Here
/// a group of N members costs N+1 head movements per WINDOW instead, and a
/// window grows to sixteen chunks.
///
/// **Why the answer is the same answer.** A *run* is one read the pivot asked
/// for; every member is asked for the same lengths at the same offsets, so the
/// runs are the same runs for everybody, and comparing a run against the
/// pivot's is the same `memcmp` on the same boundaries lockstep does.
///
/// - A member kept in the pivot's class matched every run to the end of the
///   pivot's file, so the class the sweep settles is exactly the members equal
///   to the pivot.
/// - Two members that left in DIFFERENT runs cannot be equal, and are never
///   read again to find out: the one that left in the earlier run differs from
///   the pivot somewhere inside it, and the one that left later matched the
///   pivot through the whole of that run.
/// - Two members that left in the SAME run agree with the pivot — and so with
///   each other — everywhere before it, and are compared among themselves from
///   exactly there, the first of them becoming the next pivot. Recursively, so
///   every pair either meets or is told apart by the rule above.
///
/// Every member therefore ends in exactly one class, and the classes are the
/// byte-equality classes — the same partition lockstep reaches, which
/// `the_two_comparisons_are_the_same_function` holds it to on random shapes.
///
/// **What it holds.** One window and one member's run, `held_ceiling`, however
/// wide the group: the pivot's bytes are held, the members' stream past.
///
/// **Windows and runs are two different sizes, for two different reasons.** A
/// window is what a head movement buys — it starts at `chunk` and grows by
/// `PIVOT_GROWTH` to `PIVOT_WINDOW`, so a group that parts early costs what
/// the lockstep round it replaces cost, and one that keeps agreeing pays a
/// movement per member per 256 MiB. A run is what a parting is placed to, and
/// since runs inside a window carry straight on from each other they cost no
/// movement at all — so `PIVOT_RUN` is short, and a member that parts is read
/// barely past the byte it parted at.
fn split_pivot_windows(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    chunk: usize,
    unreadable: &mut Vec<String>,
    carried: Vec<Option<File>>,
) -> Settled {
    // No pool: one disk, one queue, which is what the profile is for.
    let reading = match prepare(context, records, members, unreadable, None, carried) {
        Prepared::Answered(settled) => return settled,
        Prepared::Reading(reading) => reading,
    };
    let Reading {
        opened,
        handles,
        prefix,
        tail,
        limit,
        mut hashers,
        live,
    } = reading;
    let mut settled = Settled::default();
    let mut matched: Vec<Vec<usize>> = Vec::new();
    let mut at_tail: Vec<Vec<usize>> = Vec::new();
    let mut positions: Vec<u64> = vec![prefix as u64; opened.len()];
    let ceiling = PIVOT_WINDOW.max(chunk);
    let run = pivot_run(chunk);

    // Classes still to settle, each already proved equal as far as the offset
    // beside it. Taken from the back, so the first class is swept first.
    let mut pending: Vec<(Vec<usize>, usize)> =
        live.into_iter().map(|class| (class, prefix)).collect();
    pending.reverse();

    while let Some((class, from)) = pending.pop() {
        // A class arrives proved equal as far as `from`, but its members
        // stopped wherever they last differed, so the handles have to be put
        // back. One that will not move is one that cannot be compared.
        let mut ready: Vec<usize> = Vec::with_capacity(class.len());
        for slot in class {
            if positions[slot] != from as u64 {
                let moved = handles[slot]
                    .lock()
                    .expect("no reader panics")
                    .seek(SeekFrom::Start(from as u64))
                    .is_ok();
                if !moved {
                    unreadable.push(records[opened[slot]].path().to_string());
                    continue;
                }
                positions[slot] = from as u64;
            }
            ready.push(slot);
        }
        if ready.len() < 2 {
            continue; // a class of one is settled
        }

        let pivot = ready[0];
        let mut sweep = Sweep::over(ready[1..].to_vec(), from);
        let mut offset = from;
        let mut window = chunk;
        let mut ending = Ending::Alone;
        while !sweep.live.is_empty() {
            let span = match limit {
                None => window,
                Some(limit) => window.min(limit.saturating_sub(offset)),
            };
            if span == 0 {
                ending = Ending::AtTail; // everything before the kept tail agrees
                break;
            }

            // The pivot's window, read in runs that each carry straight on from
            // the last, so the whole of it costs one head movement.
            let mut held: Vec<(usize, Vec<u8>)> = Vec::new();
            let mut filled = 0;
            let mut at_end = false;
            let mut lost = false;
            while filled < span {
                let want = run.min(span - filled);
                let block = {
                    let mut file = handles[pivot].lock().expect("no reader panics");
                    let where_from = records[opened[pivot]].at(positions[pivot]);
                    context.effort.read_block(&mut file, want, where_from)
                };
                match block {
                    Ok(block) => {
                        positions[pivot] += block.len() as u64;
                        filled += block.len();
                        at_end = block.len() < want;
                        held.push((want, block));
                        if at_end {
                            break; // short of what it asked for: the file ends here
                        }
                    }
                    Err(_) => {
                        lost = true;
                        break;
                    }
                }
            }
            if lost {
                ending = Ending::Lost;
                break;
            }
            debug_assert!(
                held.iter().map(|(_, block)| block.len()).sum::<usize>() + run
                    <= held_ceiling(chunk),
                "the window budget is the cap"
            );
            if context.learn {
                for (_, block) in &held {
                    hashers[pivot].update(block);
                }
            }

            // Every other member reads the same range, each in one run of its
            // own, and stops at the first read that disagrees. Asking for the
            // lengths the pivot asked for is what makes a file that ended
            // early a member that DIFFERS rather than one that agrees.
            let mut still: Vec<usize> = Vec::with_capacity(sweep.live.len());
            for slot in std::mem::take(&mut sweep.live) {
                let mut done = 0;
                let mut verdict = Verdict::Same;
                for (want, theirs) in &held {
                    let block = {
                        let mut file = handles[slot].lock().expect("no reader panics");
                        let where_from = records[opened[slot]].at(positions[slot]);
                        context.effort.read_block(&mut file, *want, where_from)
                    };
                    let Ok(block) = block else {
                        verdict = Verdict::Gone;
                        break;
                    };
                    positions[slot] += block.len() as u64;
                    if block != *theirs {
                        verdict = Verdict::Apart(offset + done);
                        break;
                    }
                    // Only a run that matched goes into the digest: a member
                    // that leaves is read again from the start of the run it
                    // left in, and a hasher cannot be wound back.
                    if context.learn {
                        hashers[slot].update(&block);
                    }
                    done += block.len();
                }
                match verdict {
                    Verdict::Same => still.push(slot),
                    Verdict::Apart(at) => sweep.left(slot, at),
                    Verdict::Gone => unreadable.push(records[opened[slot]].path().to_string()),
                }
            }
            sweep.live = still;
            offset += filled;
            sweep.reached(offset);
            if at_end {
                ending = Ending::Ended;
                break;
            }
            window = window.saturating_mul(PIVOT_GROWTH).min(ceiling);
        }

        let left = match ending {
            Ending::Lost => {
                unreadable.push(records[opened[pivot]].path().to_string());
                sweep.abandoned()
            }
            _ => {
                let (live, left) = sweep.residue();
                if !live.is_empty() {
                    let mut class = Vec::with_capacity(live.len() + 1);
                    class.push(pivot);
                    class.extend(live);
                    if matches!(ending, Ending::AtTail) {
                        at_tail.push(class);
                    } else {
                        // Reaching the end of the pivot's file with the class
                        // still together means everything after the kept head
                        // agreed. With a kept tail in hand the sweep stops
                        // short of EOF instead and is learned below, once that
                        // tail has gone into the digest.
                        if context.learn && tail == 0 {
                            for &slot in &class {
                                settled
                                    .learned
                                    .push((opened[slot], hashers[slot].clone().finalize().into()));
                            }
                        }
                        matched.push(class);
                    }
                }
                left
            }
        };
        pending.extend(left.into_iter().rev());
    }

    // Only reached with a kept tail in hand: the sweeps above stopped short of
    // EOF and these bytes are what is left of the file.
    for group in at_tail {
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
    let outcome = split_identical(
        context,
        records,
        cohort,
        chunk,
        &mut reported,
        None,
        Vec::new(),
    );
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
        return split_identical(
            context,
            records,
            members,
            chunk,
            unreadable,
            None,
            Vec::new(),
        );
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
/// `pulse` is told a group at a time rather than a file at a time: a group is
/// where this stage's work is indivisible anyway, and counting there keeps the
/// reading loops — the hottest code in the program — exactly as they were.
#[allow(clippy::too_many_arguments)]
pub fn compare_stage(
    context: &Compare,
    records: &[FileRec],
    buckets: &[Vec<usize>],
    jobs: usize,
    pool: &ThreadPool,
    unreadable: &mut Vec<String>,
    pulse: &crate::pulse::Pulse,
    mut held: Option<&mut [Option<File>]>,
) -> Settled {
    // Every descriptor a group needs is lifted out of the table here, on this
    // thread, before anything fans out — a wave's members run as parallel
    // tasks and cannot share `&mut` access to it.
    fn lift(held: &mut Option<&mut [Option<File>]>, members: &[usize]) -> Vec<Option<File>> {
        match held.as_deref_mut() {
            Some(table) => members.iter().map(|&index| table[index].take()).collect(),
            None => Vec::new(),
        }
    }
    // One disk, one queue, one group at a time, in the order the buckets were
    // sorted into — which is inode order, so the whole stage walks the platter
    // forwards. Waves exist to keep several readers busy and there is only one.
    //
    // The descriptors the probes left open are lifted here as they are for
    // every other group: the sweep opens each member once and keeps it, so a
    // carried descriptor saves exactly the open the flash path saves. What the
    // sweep does not carry over from the capped comparison is its width cap —
    // holding a descriptor costs nothing on a disk that is only ever asked to
    // read from one of them at a time, and SPIN_INTERLEAVE existed to bound
    // the round-robin this replaces.
    if context.profile == Profile::Spinning {
        let mut out = Settled::default();
        for bucket in buckets {
            let chunk = compare_chunk(context.profile, bucket.len(), context.solid);
            let carried = lift(&mut held, bucket);
            out.absorb(split_pivot_windows(
                context, records, bucket, chunk, unreadable, carried,
            ));
            pulse.files(bucket.len() as u64);
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
            let carried = lift(&mut held, &buckets[index]);
            out.absorb(split_identical(
                context,
                records,
                &buckets[index],
                chunk,
                unreadable,
                spread,
                carried,
            ));
            pulse.files(buckets[index].len() as u64);
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
            let carried = lift(&mut held, &wave[0]);
            out.absorb(split_identical(
                context, records, &wave[0], chunk, unreadable, spread, carried,
            ));
            pulse.files(wave[0].len() as u64);
            continue;
        }

        let wave_chunk = compare_chunk(context.profile, total, context.solid);
        let lifted: Vec<Vec<Option<File>>> =
            wave.iter().map(|group| lift(&mut held, group)).collect();
        let results: Vec<(Settled, Vec<String>)> = pool.install(|| {
            wave.par_iter()
                .zip(lifted.into_par_iter())
                .map(|(group, carried)| {
                    let mut local = Vec::new();
                    // pool = None: nothing here submits pool work from inside
                    // pool work, so each task reads its own members serially.
                    let settled = split_identical(
                        context, records, group, wave_chunk, &mut local, None, carried,
                    );
                    // Inside the wave, not after it: a whole wave is often the
                    // whole stage, and progress counted there would arrive
                    // only once it had stopped being progress.
                    pulse.files(group.len() as u64);
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
                Vec::new(),
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
        const { assert!(COMPARE_BUDGET / 512 < CHUNK, "512 members must shrink it") };
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

    /// What the three comparisons made of one bench: lockstep, the capped
    /// cohort sweep, and the pivot-window sweep, each normalised.
    type Answers = (Vec<Vec<usize>>, Vec<Vec<usize>>, Vec<Vec<usize>>);

    /// The reading context every comparison in these tests is given.
    fn reading<'a>(root: &'a std::path::Path, effort: &'a Effort, profile: Profile) -> Compare<'a> {
        Compare {
            root,
            effort,
            profile,
            solid: true,
            learn: false,
        }
    }

    impl Bench {
        /// One file per letter, every file the same length, files sharing a
        /// letter sharing their bytes. `chunk` says how long, so a caller can
        /// choose whether the comparison takes one round or several.
        fn new(name: &str, classes: &[u8], size: usize) -> Bench {
            Bench::of(
                name,
                classes
                    .iter()
                    .map(|&letter| {
                        // Same bytes everywhere but the last kilobyte, so
                        // nothing can separate two files without reading them
                        // to the end.
                        let mut content = vec![0x5A_u8; size];
                        let tail = content.len() - 1024;
                        content[tail..].fill(letter);
                        content
                    })
                    .collect(),
            )
        }

        /// Files that agree everywhere except at the offsets named for each of
        /// them. Two files given the same plan are byte-identical, which is
        /// how a class is built that parts from the pivot at a chosen depth.
        fn sculpt(name: &str, size: usize, plans: &[&[(usize, u8)]]) -> Bench {
            Bench::of(
                name,
                plans
                    .iter()
                    .map(|plan| {
                        let mut content = vec![0x5A_u8; size];
                        for &(at, byte) in plan.iter() {
                            content[at] = byte;
                        }
                        content
                    })
                    .collect(),
            )
        }

        fn of(name: &str, contents: Vec<Vec<u8>>) -> Bench {
            let root = std::env::temp_dir().join(format!(
                "feat-capped-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("a scratch directory");
            let mut records = Vec::new();
            for (position, content) in contents.iter().enumerate() {
                let path = format!("f{position:03}.bin");
                std::fs::write(root.join(&path), content).expect("a scratch file");
                let meta = std::fs::metadata(root.join(&path)).expect("a stat");
                use std::os::unix::fs::MetadataExt;
                records.push(FileRec {
                    names: vec![path],
                    size: meta.size(),
                    dev: meta.dev(),
                    ino: meta.ino(),
                    nlink: meta.nlink(),
                    // The real one, so a record carrying kept bytes survives
                    // the stat that decides whether they still prove anything.
                    mtime_ns: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
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

        /// Turn a member into a directory under the same name: the open still
        /// succeeds and every read fails. It is the only way to stage a file
        /// that goes away with its descriptor already in hand, which is the
        /// case the sweep's bookkeeping exists for.
        fn vanish(&self, position: usize) {
            let path = self.root.join(self.records[position].path());
            std::fs::remove_file(&path).expect("a scratch file we own");
            std::fs::create_dir(&path).expect("a scratch directory");
        }

        /// Hand every record the bytes stage 3 would have kept of it, so the
        /// comparison starts where the retained head ends and stops where the
        /// retained tail begins.
        fn keep(&mut self, head: usize, tail: usize) {
            for record in self.records.iter_mut() {
                let whole = std::fs::read(self.root.join(record.path())).expect("a scratch file");
                let end = whole.len() - tail;
                record.keep_ends(whole[..head].to_vec(), whole[end..].to_vec());
            }
        }

        /// The partition each comparison reaches, normalised so the answers
        /// can be compared regardless of the order they were found in.
        fn partitions(&self, chunk: usize) -> Answers {
            let members: Vec<usize> = (0..self.records.len()).collect();
            let effort = Effort::default();
            let mut sink = Vec::new();
            let plain = split_identical(
                &reading(&self.root, &effort, Profile::Flash),
                &self.records,
                &members,
                chunk,
                &mut sink,
                None,
                Vec::new(),
            );
            let mut sink = Vec::new();
            let capped = split_identical_capped(
                &reading(&self.root, &effort, Profile::Spinning),
                &self.records,
                &members,
                chunk,
                &mut sink,
            );
            let mut sink = Vec::new();
            let pivot = split_pivot_windows(
                &reading(&self.root, &effort, Profile::Spinning),
                &self.records,
                &members,
                chunk,
                &mut sink,
                Vec::new(),
            );
            (
                tidy(plain.matched),
                tidy(capped.matched),
                tidy(pivot.matched),
            )
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
        let (plain, capped, pivot) = bench.partitions(4096);
        assert_eq!(capped, vec![(0..24).collect::<Vec<usize>>()]);
        assert_eq!(capped, plain);
        assert_eq!(pivot, plain);
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
        let (plain, capped, pivot) = bench.partitions(4096);
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
        assert_eq!(pivot, plain);
    }

    #[test]
    fn a_member_that_matches_nobody_is_carried_to_every_later_pass() {
        // The singletons are what a naive cohort split loses: each one has to
        // be offered to every pivot before it can be called unique.
        let classes = *b"ABCDEFGHIJKLMNOPA";
        let bench = Bench::new("singletons", &classes, 8192);
        let (plain, capped, pivot) = bench.partitions(4096);
        assert_eq!(capped, vec![vec![0, 16]]);
        assert_eq!(capped, plain);
        assert_eq!(pivot, plain);
    }

    #[test]
    fn a_group_taking_several_rounds_is_partitioned_the_same_way() {
        // Chunk smaller than the file, so the comparison runs to several
        // rounds inside each cohort and the divergence is in the last one.
        let classes = *b"AABBBCAAAAABBCCCCA";
        let bench = Bench::new("rounds", &classes, 32 * 1024);
        let (plain, capped, pivot) = bench.partitions(4096);
        assert_eq!(capped, plain);
        assert_eq!(pivot, plain);
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
            let (plain, capped, pivot) = bench.partitions(4096);
            let says = String::from_utf8_lossy(&classes).into_owned();
            assert_eq!(capped, plain, "shape {shape}: {says}");
            assert_eq!(pivot, plain, "shape {shape}: {says}");
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

    // -------------------------------------------------------- the pivot windows

    /// The chunk every shape below is built around. A window starts there and
    /// grows by `PIVOT_GROWTH`, so with a group read from offset zero the
    /// windows of a `SPAN`-long file are `[0, C)`, `[C, 5C)`, `[5C, 21C)`,
    /// `[21C, SPAN)`, and the runs inside every one of them are C bytes.
    ///
    /// Those numbers are not written down anywhere below: `window_edges` works
    /// them out from the constants, and `the_windows_are_where_the_growth_factor
    /// _puts_them` measures where they really fall and holds the two to each
    /// other. A change to the ramp moves all three together or fails.
    const C: usize = 4096;

    /// Long enough for four windows and an end, so a shape can put its parting
    /// in each of them in turn and either side of all three boundaries.
    const SPAN: usize = 40 * C;

    /// Where the sweep's windows end, for a group read from `from` to `size` at
    /// chunk `C` — derived from the constants rather than from a comment, so it
    /// cannot describe a ramp the code no longer has.
    ///
    /// A window that reaches exactly to EOF still ends: the sweep only learns
    /// the file is over when a read comes back short of what it asked for, so
    /// one more window follows to find that out. `<=` rather than `<` is that
    /// fact, and it is what makes the count agree with the seeks.
    fn window_edges(from: usize, size: usize) -> Vec<usize> {
        let mut edges = Vec::new();
        let mut at = from;
        let mut window = C;
        while at + window <= size {
            at += window;
            edges.push(at);
            window = window.saturating_mul(PIVOT_GROWTH).min(PIVOT_WINDOW.max(C));
        }
        edges
    }

    fn sweep(
        bench: &Bench,
        chunk: usize,
        effort: &Effort,
        unreadable: &mut Vec<String>,
    ) -> Settled {
        let members: Vec<usize> = (0..bench.records.len()).collect();
        split_pivot_windows(
            &reading(&bench.root, effort, Profile::Spinning),
            &bench.records,
            &members,
            chunk,
            unreadable,
            Vec::new(),
        )
    }

    fn march(
        bench: &Bench,
        chunk: usize,
        effort: &Effort,
        unreadable: &mut Vec<String>,
    ) -> Settled {
        let members: Vec<usize> = (0..bench.records.len()).collect();
        split_identical(
            &reading(&bench.root, effort, Profile::Flash),
            &bench.records,
            &members,
            chunk,
            unreadable,
            None,
            Vec::new(),
        )
    }

    /// Two pairs that agree everywhere but the byte at `at`.
    fn parting(name: &str, at: usize) -> Bench {
        Bench::sculpt(name, SPAN, &[&[], &[], &[(at, b'B')], &[(at, b'B')]])
    }

    fn classes(bench: &Bench, chunk: usize) -> Vec<Vec<usize>> {
        let effort = Effort::default();
        let mut sink = Vec::new();
        tidy(sweep(bench, chunk, &effort, &mut sink).matched)
    }

    #[test]
    fn a_divergence_in_the_first_window_splits_the_group() {
        assert_eq!(classes(&parting("first", 100), C), [[0, 1], [2, 3]]);
    }

    #[test]
    fn a_divergence_in_a_middle_window_splits_the_group() {
        // Inside the third window, with windows either side of it.
        let at = window_edges(0, SPAN)[1] + C;
        assert_eq!(classes(&parting("middle", at), C), [[0, 1], [2, 3]]);
    }

    #[test]
    fn a_divergence_in_the_last_window_splits_the_group() {
        // Past the last boundary, so only the run that reaches the end of the
        // file can separate them.
        let last = *window_edges(0, SPAN).last().expect("several windows");
        assert!(SPAN - 7 > last);
        assert_eq!(classes(&parting("last", SPAN - 7), C), [[0, 1], [2, 3]]);
    }

    #[test]
    fn the_windows_are_where_the_growth_factor_puts_them() {
        // The geometry every boundary test below stands on, MEASURED rather
        // than read off a comment. A window costs one head movement per member,
        // so a pair of identical files costs two per window: the seek count
        // says how many windows the file took, and `window_edges` has to agree.
        //
        // This is the guard REVIEW-M7 asked for. When the ramp changes, the
        // measured count and the derivation move together and this still holds;
        // when only one of them changes, it fails.
        for size in [C, SPAN, 4 * SPAN] {
            let bench = Bench::new(&format!("geom{size}"), &[b'A'; 2], size);
            let effort = Effort::default();
            let mut sink = Vec::new();
            sweep(&bench, C, &effort, &mut sink);
            let windows = window_edges(0, size).len() + 1;
            assert_eq!(
                effort.seeks(),
                2 * windows as u64,
                "a {size}-byte pair should take {windows} window(s)"
            );
        }

        // ...and the offsets the shapes below are written around, spelled out
        // once so that changing the growth factor fails HERE, where a reader is
        // told which offsets to look at, rather than silently retargeting a
        // test named for boundaries onto offsets that are not boundaries.
        assert_eq!(PIVOT_GROWTH, 4, "the offsets below belong to this factor");
        assert_eq!(window_edges(0, SPAN), [C, 5 * C, 21 * C]);
    }

    #[test]
    fn a_divergence_on_a_window_boundary_splits_the_group() {
        // Either side of every real window boundary in this file, plus the
        // first and last byte of the file itself. An off-by-one in where a
        // window starts or ends loses one of these.
        let mut offsets = vec![0, 1, SPAN - 1];
        for edge in window_edges(0, SPAN).into_iter().filter(|&e| e + 1 < SPAN) {
            offsets.extend([edge - 1, edge, edge + 1]);
        }
        assert!(offsets.len() >= 12, "this file has boundaries to walk");
        for at in offsets {
            let bench = parting(&format!("edge{at}"), at);
            assert_eq!(classes(&bench, C), [[0, 1], [2, 3]], "parting at {at}");
        }
    }

    #[test]
    fn a_divergence_on_a_run_boundary_splits_the_group() {
        // The same walk over the run boundaries, which are every C bytes and
        // are not the same thing. Worth testing, just not under the other name.
        for run in 0..SPAN / C {
            for at in [run * C, (run * C + C).saturating_sub(1)] {
                let bench = parting(&format!("run{at}"), at);
                assert_eq!(classes(&bench, C), [[0, 1], [2, 3]], "parting at {at}");
            }
        }
    }

    #[test]
    fn the_sweep_finds_the_same_classes_wherever_the_files_part() {
        // The named cases above, exhaustively: every run boundary, every window
        // boundary and its neighbours, and a spread of offsets between them,
        // each checked against the lockstep answer rather than a hand-written
        // one. The stride is coprime with C so it does not walk in step with
        // the runs.
        let edges: Vec<usize> = window_edges(0, SPAN)
            .into_iter()
            .filter(|&e| e + 1 < SPAN)
            .collect();
        let walk = (0..SPAN)
            .step_by(311)
            .chain((0..SPAN / C).map(|run| run * C))
            .chain(edges.iter().flat_map(|&e| [e - 1, e, e + 1]));
        for at in walk {
            let bench = parting(&format!("walk{at}"), at);
            let effort = Effort::default();
            let mut sink = Vec::new();
            let lock = tidy(march(&bench, C, &effort, &mut sink).matched);
            assert_eq!(classes(&bench, C), lock, "parting at {at}");
        }
    }

    #[test]
    fn two_members_that_left_the_pivot_together_are_compared_to_each_other() {
        // The residue regroup, in one run: 1 and 2 both part from the pivot at
        // the same byte and are equal to each other. Nothing in the pivot's own
        // sweep can see that, so a sweep that dropped its leavers would report
        // no group at all.
        let bench = Bench::sculpt(
            "residue-one-run",
            SPAN,
            &[&[], &[(5 * C + 7, b'X')], &[(5 * C + 7, b'X')]],
        );
        assert_eq!(classes(&bench, C), [[1, 2]]);
    }

    #[test]
    fn two_members_that_left_the_pivot_in_different_runs_still_find_their_own() {
        // The residue spans two windows: 1 leaves in the first, 2 and 3 in the
        // fourth. 2 and 3 are equal and must still be found, and 1 must join
        // neither — it differs from both at the byte it left over.
        let bench = Bench::sculpt(
            "residue-two-runs",
            SPAN,
            &[&[], &[(200, b'X')], &[(9 * C, b'Y')], &[(9 * C, b'Y')]],
        );
        assert_eq!(classes(&bench, C), [[2, 3]]);
    }

    #[test]
    fn a_residue_that_is_two_classes_is_split_again_rather_than_merged() {
        // Four members leave the pivot in the same run, as two pairs. The
        // regroup has to run a second time to tell the pairs apart: a sweep
        // that took "left in the same run" for "equal" would merge two files
        // that differ, which is the failure this stage exists to prevent.
        let at = 5 * C + 900;
        let bench = Bench::sculpt(
            "residue-two-classes",
            SPAN,
            &[
                &[],
                &[(at, b'X')],
                &[(at, b'X')],
                &[(at, b'Y')],
                &[(at, b'Y')],
            ],
        );
        assert_eq!(classes(&bench, C), [[1, 2], [3, 4]]);
    }

    #[test]
    fn members_that_left_in_one_run_at_different_bytes_are_not_merged() {
        // Both leave the pivot inside the run beginning at `5C`, seven hundred
        // bytes apart. They are filed together and then told apart by reading
        // that run again against each other.
        let bench = Bench::sculpt(
            "residue-same-run-apart",
            SPAN,
            &[&[], &[(5 * C + 10, b'X')], &[(5 * C + 710, b'X')]],
        );
        assert!(classes(&bench, C).is_empty());
    }

    #[test]
    fn a_residue_class_that_parts_later_parts_at_the_right_byte() {
        // 1 and 2 leave the pivot together and agree for another two windows
        // before parting from each other. The second sweep has to carry on
        // from where the first left off and still read to the end.
        let bench = Bench::sculpt(
            "residue-deep",
            SPAN,
            &[
                &[],
                &[(C + 5, b'X'), (11 * C, b'M')],
                &[(C + 5, b'X'), (11 * C, b'N')],
                &[(C + 5, b'X'), (11 * C, b'M')],
            ],
        );
        assert_eq!(classes(&bench, C), [[1, 3]]);
    }

    #[test]
    fn a_file_shorter_than_one_window_is_compared_in_one() {
        for size in [1, 7, 1024, C - 1, C, C + 1] {
            let bench = Bench::sculpt(
                &format!("tiny{size}"),
                size,
                &[&[], &[], &[(size - 1, b'B')]],
            );
            assert_eq!(classes(&bench, C), [[0, 1]], "at {size} bytes");
        }
    }

    #[test]
    fn groups_of_two_of_three_and_of_many_all_come_back_whole() {
        for width in [2, 3, 9, 64] {
            let bench = Bench::new(&format!("width{width}"), &vec![b'A'; width], SPAN);
            assert_eq!(
                classes(&bench, C),
                [(0..width).collect::<Vec<usize>>()],
                "a group of {width}"
            );
        }
    }

    #[test]
    fn a_group_where_nothing_matches_reports_nothing() {
        let letters: Vec<u8> = (0..12).map(|position| b'A' + position).collect();
        let bench = Bench::new("allunique", &letters, SPAN);
        assert!(classes(&bench, C).is_empty());
    }

    #[test]
    fn a_pivot_that_cannot_be_read_hands_its_class_to_the_next_one() {
        // The open succeeds and the read does not, which is a file that went
        // away with the descriptor already in hand. Nothing it was measured
        // against may be settled on it, and nobody may be stranded.
        let bench = Bench::new("lostpivot", b"AAAABB", SPAN);
        bench.vanish(0);
        let effort = Effort::default();
        let mut unreadable = Vec::new();
        let settled = sweep(&bench, C, &effort, &mut unreadable);
        assert_eq!(tidy(settled.matched), [vec![1, 2, 3], vec![4, 5]]);
        assert_eq!(unreadable, ["f000.bin"]);
    }

    #[test]
    fn a_member_that_cannot_be_read_leaves_the_rest_of_its_group_alone() {
        let bench = Bench::new("lostmember", b"AAAABBB", SPAN);
        bench.lock(1); // the open fails
        bench.vanish(5); // the open works and the read does not
        let effort = Effort::default();
        let mut unreadable = Vec::new();
        let settled = sweep(&bench, C, &effort, &mut unreadable);
        assert_eq!(tidy(settled.matched), [vec![0, 2, 3], vec![4, 6]]);
        unreadable.sort();
        assert_eq!(unreadable, ["f001.bin", "f005.bin"]);
    }

    #[test]
    fn the_sweep_opens_each_member_once_and_reads_it_in_one_run_per_window() {
        // The point of the profile, in the two counters that see it. Lockstep
        // charges a head movement per member per chunk; the sweep charges one
        // per member per window, and a window grows by `PIVOT_GROWTH`.
        let bench = Bench::new("runs", &[b'A'; 8], 128 * C);
        let lock = Effort::default();
        let mut sink = Vec::new();
        march(&bench, C, &lock, &mut sink);
        let spin = Effort::default();
        let mut sink = Vec::new();
        sweep(&bench, C, &spin, &mut sink);

        assert_eq!(lock.opens(), 8);
        assert_eq!(spin.opens(), 8, "one open per member, and it is kept");
        assert_eq!(
            spin.bytes_read(),
            lock.bytes_read(),
            "identical files: every byte read once, whichever order"
        );
        // 128 chunks: lockstep takes 129 rounds (the last one finds the end)
        // and moves the head once per member per round. The sweep's windows
        // are 1, 4, 16 and 64 chunks — 85 of them — and then a fifth that asks
        // for 256 and finds the last 43 and the end, at one movement per
        // member per window.
        assert_eq!(lock.seeks(), 129 * 8);
        assert_eq!(spin.seeks(), 5 * 8);
    }

    #[test]
    fn the_window_budget_is_the_cap_however_wide_the_group_is() {
        // No term for the group's width, which is the law M7-PIVOT.md sets:
        // only the pivot's window is held, and members stream against it one
        // at a time.
        for chunk in [MIN_CHUNK, SLOW_CHUNK, CHUNK] {
            assert_eq!(held_ceiling(chunk), PIVOT_WINDOW + chunk.min(PIVOT_RUN));
        }
        assert_eq!(held_ceiling(SLOW_CHUNK), PIVOT_WINDOW + PIVOT_RUN);
        // ...and the sweep's own debug assertion that it never holds more,
        // exercised over a group wide enough that a window per member would be
        // the whole budget many times over.
        let bench = Bench::new("hold", &[b'A'; 200], 4 * C);
        let effort = Effort::default();
        let mut sink = Vec::new();
        let settled = sweep(&bench, C, &effort, &mut sink);
        assert_eq!(tidy(settled.matched), [(0..200).collect::<Vec<usize>>()]);
        assert_eq!(effort.opens(), 200);
    }

    #[test]
    fn the_kept_head_is_compared_rather_than_read_a_second_time() {
        let mut bench = Bench::new("kepthead", b"AAB", 8 * C);
        bench.keep(C, 0);
        let effort = Effort::default();
        let mut sink = Vec::new();
        let settled = sweep(&bench, C, &effort, &mut sink);
        assert_eq!(tidy(settled.matched), [[0, 1]]);
        assert_eq!(
            effort.bytes_read(),
            3 * (8 * C - C) as u64,
            "three files, each read from the end of its kept head"
        );
    }

    #[test]
    fn the_kept_tail_settles_the_end_without_reading_it() {
        // The divergence is inside the kept tail, so the sweep must stop short
        // of it and let the bytes in hand do the splitting.
        let mut bench = Bench::new("kepttail", b"AAB", 8 * C);
        bench.keep(C, C);
        let effort = Effort::default();
        let mut sink = Vec::new();
        let settled = sweep(&bench, C, &effort, &mut sink);
        assert_eq!(tidy(settled.matched), [[0, 1]]);
        assert_eq!(
            effort.bytes_read(),
            3 * (8 * C - C - C) as u64,
            "neither end is read a second time"
        );
    }

    #[test]
    fn a_group_read_here_is_learned_as_the_digest_of_the_whole_file() {
        for (name, head, tail) in [("plain", 0, 0), ("ends", C, C)] {
            let mut bench = Bench::new(&format!("learn{name}"), b"AAB", 8 * C);
            if head > 0 || tail > 0 {
                bench.keep(head, tail);
            }
            let effort = Effort::default();
            let mut sink = Vec::new();
            let members: Vec<usize> = (0..3).collect();
            let settled = split_pivot_windows(
                &Compare {
                    root: &bench.root,
                    effort: &effort,
                    profile: Profile::Spinning,
                    solid: true,
                    learn: true,
                },
                &bench.records,
                &members,
                C,
                &mut sink,
                Vec::new(),
            );
            assert_eq!(settled.learned.len(), 2, "{name}: the pair that matched");
            for (index, digest) in &settled.learned {
                let path = bench.root.join(bench.records[*index].path());
                let whole = std::fs::read(path).expect("a scratch file");
                assert_eq!(*digest, sha256(&whole), "{name}: file {index}");
            }
        }
    }

    #[test]
    fn a_digest_survives_the_regrouping_that_reads_a_run_twice() {
        // 1 and 2 leave the pivot in the same run and are then compared to
        // each other from the start of that run — so those bytes go past twice.
        // A hasher that had eaten them on the way out would learn the digest of
        // a file that does not exist, and the next scan would match on it.
        let bench = Bench::sculpt(
            "relearn",
            8 * C,
            &[&[], &[(3 * C + 11, b'X')], &[(3 * C + 11, b'X')]],
        );
        let effort = Effort::default();
        let mut sink = Vec::new();
        let members: Vec<usize> = (0..3).collect();
        let settled = split_pivot_windows(
            &Compare {
                root: &bench.root,
                effort: &effort,
                profile: Profile::Spinning,
                solid: true,
                learn: true,
            },
            &bench.records,
            &members,
            C,
            &mut sink,
            Vec::new(),
        );
        assert_eq!(settled.matched, [vec![1, 2]]);
        assert_eq!(settled.learned.len(), 2);
        for (index, digest) in &settled.learned {
            let path = bench.root.join(bench.records[*index].path());
            let whole = std::fs::read(path).expect("a scratch file");
            assert_eq!(*digest, sha256(&whole), "file {index}");
        }
    }

    #[test]
    fn the_two_comparisons_are_the_same_function() {
        // The property M7-PIVOT.md makes binding, over shapes nobody chose by
        // hand and at chunk sizes that move every window boundary: the sweep
        // reaches the partition lockstep reaches, byte for byte. A residue
        // carried wrongly between windows shows up here as a group lockstep did
        // not find, or one it did.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move |ceiling: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % ceiling
        };
        for shape in 0..60 {
            let size = 512 + next(40_000) as usize;
            let chunk = 64 + next(6_000) as usize;
            let variety = 1 + next(5) as usize;
            let plans: Vec<Vec<(usize, u8)>> = (0..variety)
                .map(|_| {
                    (0..1 + next(3))
                        .map(|_| (next(size as u64) as usize, b'A' + next(3) as u8))
                        .collect()
                })
                .collect();
            let width = 2 + next(9) as usize;
            let drawn: Vec<&[(usize, u8)]> = (0..width)
                .map(|_| plans[next(variety as u64) as usize].as_slice())
                .collect();
            let bench = Bench::sculpt(&format!("prop{shape}"), size, &drawn);
            let effort = Effort::default();
            let mut sink = Vec::new();
            let lock = tidy(march(&bench, chunk, &effort, &mut sink).matched);
            assert_eq!(
                classes(&bench, chunk),
                lock,
                "shape {shape}: {width} files of {size} bytes at chunk {chunk}"
            );
        }
    }

    // --------------------------------- runs shorter than the chunk asked for

    /// Every shape above reads at chunks below `PIVOT_RUN`, where a run IS the
    /// chunk. The spinning profile's real chunk is `SLOW_CHUNK`, a hundred and
    /// twenty-eight times longer than a run, and the split between the two is
    /// what keeps the residue cheap. These read at a chunk above the run.
    const LONG: usize = 4 * PIVOT_RUN;

    #[test]
    fn a_window_longer_than_a_run_is_still_split_at_the_right_byte() {
        // Two windows of four runs each, with the parting walked across every
        // run boundary in them and either side of each.
        let size = 12 * PIVOT_RUN;
        for at in [
            0,
            PIVOT_RUN - 1,
            PIVOT_RUN,
            2 * PIVOT_RUN + 3,
            4 * PIVOT_RUN - 1,
            4 * PIVOT_RUN,
            7 * PIVOT_RUN,
            size - 1,
        ] {
            let bench = Bench::sculpt(
                &format!("longrun{at}"),
                size,
                &[&[], &[], &[(at, b'B')], &[(at, b'B')]],
            );
            let effort = Effort::default();
            let mut sink = Vec::new();
            let lock = tidy(march(&bench, LONG, &effort, &mut sink).matched);
            assert_eq!(classes(&bench, LONG), lock, "parting at {at}");
            assert_eq!(classes(&bench, LONG), [[0, 1], [2, 3]], "parting at {at}");
        }
    }

    #[test]
    fn a_member_that_parts_is_read_to_the_end_of_its_run_and_no_further() {
        // The reason a run is short, and the reason the first window is only a
        // chunk long. Two files of eight runs that part in their first
        // kilobyte cost the pivot one window — a chunk, exactly what lockstep's
        // first round reads from each member — and the member one run.
        let size = 8 * PIVOT_RUN;
        let bench = Bench::sculpt("earlypart", size, &[&[], &[(64, b'B')]]);
        let effort = Effort::default();
        let mut sink = Vec::new();
        assert!(sweep(&bench, LONG, &effort, &mut sink).matched.is_empty());
        assert_eq!(effort.bytes_read(), (LONG + PIVOT_RUN) as u64);

        // ...where lockstep at the same chunk reads one from each.
        let plain = Effort::default();
        let mut sink = Vec::new();
        march(&bench, LONG, &plain, &mut sink);
        assert_eq!(plain.bytes_read(), 2 * LONG as u64);
    }

    #[test]
    fn a_residue_is_read_again_from_its_own_run_rather_than_from_the_start() {
        // Four content classes in a file that fits inside one chunk — the
        // shape the differential calls `cohort-boundary`, and the one a sweep
        // that regrouped from offset zero reads three times over. Here every
        // class parts in the last run, so the three passes after the first
        // cost one run per member instead of one file per member.
        let size = 8 * PIVOT_RUN;
        let at = size - 100;
        let bench = Bench::sculpt(
            "residue-runs",
            size,
            &[
                &[],
                &[],
                &[(at, b'B')],
                &[(at, b'B')],
                &[(at, b'C')],
                &[(at, b'C')],
                &[(at, b'D')],
                &[(at, b'D')],
            ],
        );
        let effort = Effort::default();
        let mut sink = Vec::new();
        let settled = sweep(&bench, LONG, &effort, &mut sink);
        assert_eq!(
            tidy(settled.matched),
            [vec![0, 1], vec![2, 3], vec![4, 5], vec![6, 7]]
        );
        // Eight files read whole, then the six that parted read again a run at
        // a time: 6, then 4, then 2 — never the whole file a second time.
        assert_eq!(
            effort.bytes_read(),
            (8 * size + 12 * PIVOT_RUN) as u64,
            "a class re-read from zero would cost far more than this"
        );
    }

    #[test]
    fn the_two_comparisons_agree_where_a_window_holds_several_runs() {
        // The property test again, at a chunk above `PIVOT_RUN`, so windows
        // hold several runs and the residue offsets are run boundaries rather
        // than window ones.
        let mut state = 0xDEAD_BEEF_1234_5678_u64;
        let mut next = move |ceiling: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % ceiling
        };
        for shape in 0..12 {
            let size = 2 * PIVOT_RUN + next(6 * PIVOT_RUN as u64) as usize;
            let variety = 1 + next(4) as usize;
            let plans: Vec<Vec<(usize, u8)>> = (0..variety)
                .map(|_| {
                    (0..1 + next(3))
                        .map(|_| (next(size as u64) as usize, b'A' + next(3) as u8))
                        .collect()
                })
                .collect();
            let width = 2 + next(6) as usize;
            let drawn: Vec<&[(usize, u8)]> = (0..width)
                .map(|_| plans[next(variety as u64) as usize].as_slice())
                .collect();
            let bench = Bench::sculpt(&format!("longprop{shape}"), size, &drawn);
            let effort = Effort::default();
            let mut sink = Vec::new();
            let lock = tidy(march(&bench, LONG, &effort, &mut sink).matched);
            assert_eq!(
                classes(&bench, LONG),
                lock,
                "shape {shape}: {width} files of {size} bytes"
            );
        }
    }

    // -------------------------------------------- the sweep's own bookkeeping

    #[test]
    fn members_that_left_in_different_runs_are_never_compared_again() {
        let mut sweep = Sweep::over(vec![1, 2, 3, 4], 0);
        sweep.left(1, 0);
        sweep.left(2, C);
        sweep.left(3, 0);
        sweep.left(4, C);
        sweep.live = Vec::new();
        let (live, left) = sweep.residue();
        assert!(live.is_empty());
        assert_eq!(left, [(vec![1, 3], 0), (vec![2, 4], C)]);
    }

    #[test]
    fn a_member_that_left_on_its_own_is_settled_and_does_not_come_back() {
        let mut sweep = Sweep::over(vec![1, 2], 0);
        sweep.left(1, 0);
        sweep.left(2, C);
        sweep.live = Vec::new();
        let (_, left) = sweep.residue();
        assert!(
            left.is_empty(),
            "two classes of one are two settled classes"
        );
    }

    #[test]
    fn a_pivot_that_goes_away_mid_sweep_settles_nothing_and_carries_its_class_on() {
        // The case a file on disk cannot be made to produce: the pivot is read
        // for the first window and gone by the second. What it proved in the
        // first is real — 1 and 2 are equal to each other as far as it got — so
        // they carry on together from exactly there rather than starting over.
        let mut sweep = Sweep::over(vec![1, 2, 3], 0);
        sweep.left(3, 0);
        sweep.live = vec![1, 2];
        sweep.reached(C);
        assert_eq!(sweep.abandoned(), [(vec![1, 2], C)]);
    }

    #[test]
    fn a_pivot_that_goes_away_with_one_member_left_strands_nobody() {
        let mut sweep = Sweep::over(vec![1], 0);
        sweep.reached(8 * C);
        assert!(
            sweep.abandoned().is_empty(),
            "a class of one is not a group and is not work"
        );
    }
}
