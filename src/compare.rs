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

use crate::consts::{CHUNK, COMPARE_BUDGET, MIN_CHUNK, PARTITION_FANOUT, SLOW_CHUNK};
use crate::effort::Effort;
use crate::ordered::Ordered;
use crate::stages::{sha256, Digest};
use crate::walk::{still_the_same, unchanged, FileRec};

pub struct Compare<'a> {
    pub root: &'a Path,
    pub effort: &'a Effort,
    pub solid: bool,
}

/// How much to read from each file per round.
///
/// Flash likes small chunks; a spinning disk pays a seek every time we move to
/// the next file in the group, so it wants big ones. Either way a round holds
/// one chunk per member, so a group with a lot of copies gets a smaller chunk
/// to stay inside the budget.
pub fn compare_chunk(members: usize, solid: bool) -> usize {
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
fn partition_blocks(items: Vec<(usize, Vec<u8>)>) -> Vec<Vec<(usize, Vec<u8>)>> {
    let mut runs: Vec<Vec<(usize, Vec<u8>)>> = Vec::new();
    let mut pending = items;
    while !pending.is_empty() {
        let mut rest = Vec::new();
        let mut iter = pending.into_iter();
        let mut same = vec![iter.next().expect("pending is not empty")];
        for item in iter {
            let equal = item.1 == same[0].1;
            if equal {
                same.push(item);
            } else {
                rest.push(item);
            }
        }
        let lone = same.len() == 1;
        runs.push(same);
        if lone && rest.len() > PARTITION_FANOUT {
            let mut by_digest: Ordered<Digest, (usize, Vec<u8>)> = Ordered::new();
            for item in rest {
                by_digest.push(sha256(&item.1), item);
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

fn tags(run: Vec<(usize, Vec<u8>)>) -> Vec<usize> {
    run.into_iter().map(|(tag, _)| tag).collect()
}

/// Compare one group of same-size candidates and return the runs that agree to
/// the last byte. Members are positions in `members`, mapped back to record
/// indices on the way out.
pub fn split_identical(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    chunk: usize,
    unreadable: &mut Vec<String>,
    pool: Option<&ThreadPool>,
) -> Vec<Vec<usize>> {
    // A short file that stage 3 read to its end is already here in full.
    // Opening it again to read nothing costs an open against the stat that
    // says the file is still the one we read.
    let held: Vec<&FileRec> = members.iter().map(|&index| &records[index]).collect();
    if members.len() > 1
        && held
            .iter()
            .all(|record| record.head.len() >= record.size as usize)
        && unchanged(context.root, &held)
    {
        let blocks = members
            .iter()
            .map(|&index| {
                let record = &records[index];
                (index, record.head[..record.size as usize].to_vec())
            })
            .collect();
        return partition_blocks(blocks)
            .into_iter()
            .map(tags)
            .filter(|run| run.len() > 1)
            .collect();
    }

    let mut opened: Vec<usize> = Vec::new();
    let mut handles: Vec<Mutex<File>> = Vec::new();
    for &index in members {
        match context
            .effort
            .open(&context.root.join(records[index].path()))
        {
            Ok(file) => {
                opened.push(index);
                handles.push(Mutex::new(file));
            }
            Err(_) => unreadable.push(records[index].path().to_string()),
        }
    }

    // Whatever stage 3 kept is compared here instead of being read again — the
    // bytes themselves, not a digest of them, so the proof is the same proof.
    // Bytes on disk that have moved since they were read are not the bytes we
    // are about to delete something over, so the first stat that disagrees
    // puts the whole group back on reading from zero.
    let mut prefix = opened
        .iter()
        .map(|&index| records[index].head.len())
        .min()
        .unwrap_or(0);
    let mut tail = opened
        .iter()
        .map(|&index| records[index].tail.len())
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

    let mut matched: Vec<Vec<usize>> = Vec::new();
    let mut live: Vec<Vec<usize>> = if opened.len() < 2 {
        Vec::new()
    } else if prefix > 0 {
        partition_blocks(
            opened
                .iter()
                .enumerate()
                .map(|(slot, &index)| (slot, records[index].head[..prefix].to_vec()))
                .collect(),
        )
        .into_iter()
        .map(tags)
        .filter(|run| run.len() > 1) // settled on kept bytes alone
        .collect()
    } else {
        vec![(0..opened.len()).collect()]
    };

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
            let read = |slot: &usize| -> (usize, Option<Vec<u8>>) {
                let mut file = handles[*slot].lock().expect("no reader panics");
                (*slot, context.effort.read_block(&mut file, span).ok())
            };
            let blocks: Vec<(usize, Option<Vec<u8>>)> = match pool {
                Some(pool) => pool.install(|| group.par_iter().map(read).collect()),
                None => group.iter().map(read).collect(),
            };
            let mut good = Vec::with_capacity(blocks.len());
            for (slot, block) in blocks {
                match block {
                    Some(block) => good.push((slot, block)),
                    None => unreadable.push(records[opened[slot]].path().to_string()),
                }
            }
            for run in partition_blocks(good) {
                let at_end = run[0].1.is_empty();
                let members = tags(run);
                if members.len() < 2 {
                    continue; // settled, stop reading
                } else if at_end {
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
                let held = &records[opened[slot]].tail;
                (slot, held[held.len() - tail..].to_vec())
            })
            .collect();
        for run in partition_blocks(blocks) {
            let members = tags(run);
            if members.len() > 1 {
                matched.push(members);
            }
        }
    }

    matched
        .into_iter()
        .map(|run| run.into_iter().map(|slot| opened[slot]).collect())
        .collect()
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
) -> Vec<Vec<usize>> {
    // 4x the reader count so there is enough work to keep every reader busy
    // even once a group or two settles early, floored at 16 so a low -j does
    // not turn nearly everything into a wave of two.
    let wave_members = (4 * jobs).max(16);
    let mut out: Vec<Vec<usize>> = Vec::new();
    let spread = (jobs > 1).then_some(pool);

    let mut index = 0;
    while index < buckets.len() {
        if buckets[index].len() >= wave_members {
            let chunk = compare_chunk(buckets[index].len(), context.solid);
            out.extend(split_identical(
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
            let chunk = compare_chunk(wave[0].len(), context.solid);
            out.extend(split_identical(
                context, records, &wave[0], chunk, unreadable, spread,
            ));
            continue;
        }

        let wave_chunk = compare_chunk(total, context.solid);
        let settled: Vec<(Vec<Vec<usize>>, Vec<String>)> = pool.install(|| {
            wave.par_iter()
                .map(|group| {
                    let mut local = Vec::new();
                    // pool = None: nothing here submits pool work from inside
                    // pool work, so each task reads its own members serially.
                    let runs =
                        split_identical(context, records, group, wave_chunk, &mut local, None);
                    (runs, local)
                })
                .collect()
        });
        for (runs, local) in settled {
            out.extend(runs);
            unreadable.extend(local);
        }
    }
    out
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
        assert_eq!(compare_chunk(2, true), CHUNK);
        assert_eq!(compare_chunk(2, false), SLOW_CHUNK);
        assert_eq!(compare_chunk(64, true), COMPARE_BUDGET / 64);
        assert_eq!(compare_chunk(100_000, true), MIN_CHUNK);
    }
}
