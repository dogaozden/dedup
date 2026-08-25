//! The five-stage narrowing, in the oracle's order: walk, then size groups,
//! then one page from each end (and the rest of the deep head for whatever
//! that left crowded), then sample windows through the middle, then lockstep
//! byte comparison.
//!
//! Stages 3 and 4 only ever narrow. The answer for every reported group traces
//! to bytes compared in stage 5, or to whole content held in memory by the
//! small-file stage.

use std::fs::File;
use std::path::Path;

use rayon::{ThreadPool, ThreadPoolBuilder};

use crate::cache::ScanCache;
use crate::compare::{compare_stage, Compare};
use crate::consts::{DEEP_HEAD, HANDLE_BUDGET, HEAD_BYTES, RETAIN_BUDGET, SAMPLE_SPAN, TAIL_BYTES};
use crate::device::{Profile, VolumeProbe, Wanted};
use crate::effort::Effort;
use crate::filters::Filters;
use crate::ordered::Ordered;
use crate::pulse::{self, Pulse};
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
pub fn size_stage(root: &Path, filters: Filters, pulse: &Pulse) -> Scan {
    let mut files_seen = 0;
    let mut bytes_seen = 0;
    let mut unreadable = Vec::new();
    pulse.stage(pulse::SCANNING, 0);
    let entries = walk(
        root,
        filters,
        &mut files_seen,
        &mut bytes_seen,
        &mut unreadable,
        pulse,
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

/// The interactive scan, which has a browser open in front of it.
///
/// `None` on every other run, and every hook below is then a branch that falls
/// through — M8-LIVE.md's absolute fence is that `--list`, `--auto`,
/// `--report-json`, pipes and every gate keep today's pipeline and today's
/// bytes, so the live path may not cost them an instruction it can avoid.
pub type Streaming<'a> = Option<&'a crate::live::Live>;

pub fn find_duplicates(
    root: &Path,
    filters: Filters,
    probe: &mut VolumeProbe,
    effort: &Effort,
    mut cache: Option<&mut ScanCache>,
    wanted: Wanted,
    pulse: &Pulse,
    live: Streaming<'_>,
) -> Scan {
    if let Some(cache) = cache.as_deref_mut() {
        cache.open_path(root);
    }
    let mut files_seen = 0;
    let mut bytes_seen = 0;
    let mut unreadable = Vec::new();
    // The stage names are the oracle's, and so is the order they arrive in.
    pulse.stage(pulse::SCANNING, 0);
    let entries = walk(
        root,
        filters,
        &mut files_seen,
        &mut bytes_seen,
        &mut unreadable,
        pulse,
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
    let learning = cache.is_some();
    let (profile, hint) = wanted.resolve(device);
    scan.profile = profile;
    scan.hint = hint;
    let random_jobs = device.random_jobs(profile);
    let sequential_jobs = device.sequential_jobs(profile);
    let random_pool = pool_of(random_jobs);
    // The deep reader, for the stages that hash enough per file to want every
    // core: `DeepHead` reads 60 KiB and `Sample` half a megabyte.
    let reader = Reader {
        root,
        effort,
        pool: &random_pool,
        jobs: random_jobs,
        pulse,
    };
    // The shallow reader, for the stages that read a page and hash a page.
    // Those are waiting on the catalog, not computing, and `fast_bound` is
    // where that bottoms out — the same pool the small-file stage wants, so
    // where the two counts agree there is one pool and not two.
    let shallow_jobs = device.small_file_jobs(profile);
    let shallow_pool = (shallow_jobs != random_jobs).then(|| pool_of(shallow_jobs));
    let shallow = Reader {
        root,
        effort,
        pool: shallow_pool.as_ref().unwrap_or(&random_pool),
        jobs: shallow_jobs,
        pulse,
    };
    let mut unreadable = std::mem::take(&mut scan.unreadable);
    let mut settled: Vec<Vec<usize>> = Vec::new();

    // What the apply-time proof and the deletion behind it read with. Built
    // once, here, because a request may arrive at any stage boundary from now
    // on and the answer must not depend on which one it arrived at.
    let serving = Compare {
        root,
        effort,
        profile,
        solid: device.all_solid_state(),
        learn: learning,
    };

    // A file this small costs the same to read whole as stage 3's first page
    // alone would cost for a bigger one, so there is nothing to stage for it:
    // settle it in one pass and let everything below work on what is left.
    let small: Vec<usize> = (0..records.len())
        .filter(|&index| records[index].size <= DEEP_HEAD as u64)
        .collect();
    let mut staged: Vec<usize> = (0..records.len())
        .filter(|&index| records[index].size > DEEP_HEAD as u64)
        .collect();

    // One descriptor per staged file, carried from the ends probe through the
    // deep-head and sample probes. Only worth building when the staged set is
    // small enough to hold; past that every stage reopens as before.
    let holding = !staged.is_empty() && staged.len() <= HANDLE_BUDGET;
    let mut held: Vec<Option<File>> = if holding {
        (0..records.len()).map(|_| None).collect()
    } else {
        Vec::new()
    };

    let mut remembered: Vec<Vec<usize>> = Vec::new();
    // Whatever is still in a bucket with somebody else when the narrowing
    // stops. Hoisted out of the stages so that a cancellation can leave the
    // block early and still hand back everything settled before it.
    let mut pending: Vec<Vec<usize>> = Vec::new();

    // Every reading stage, as one block a cancellation can leave. On the
    // non-live path `live` is `None`, nothing ever breaks out of it, and the
    // block is exactly the straight line it was before this milestone.
    #[allow(clippy::never_loop)]
    'stages: {
        if !small.is_empty() {
            pulse.stage(pulse::SMALL, small.len() as u64);
            // The same shallow reader stage 3's page probe uses; see
            // `Device::small_file_jobs`.
            let (proven, attested, fallback) = small_file_stage(
                &shallow,
                &records,
                &small,
                &mut unreadable,
                cache.as_deref_mut(),
            );
            // The first groups the browser sees, and the only ones the live path
            // can call proven while the scan is still running: the small-file
            // stage held every byte of these files in memory and compared them, so
            // there is nothing left for a later proof to add. The attested ones
            // beside them came from remembered digests and are candidates.
            if let Some(live) = live {
                live.publish_all(&records, &proven, true);
                live.publish_all(&records, &attested, false);
            }
            settled.extend(proven);
            remembered.extend(attested);
            if !fallback.is_empty() {
                staged.extend(fallback);
                staged.sort_by_key(|&index| records[index].layout());
            }
        }

        // A safe point: nothing is half-decided here, so what has been learned
        // can be committed, a request can be answered, and a cancellation can
        // be obeyed. `settle` is timed, so a scan short enough to finish
        // inside one interval writes nothing extra and the frozen path stays
        // frozen; the other two are no-ops off the live path.
        if let Some(cache) = cache.as_deref_mut() {
            cache.settle();
        }
        if let Some(live) = live {
            serve(
                live,
                &mut records,
                &serving,
                cache.as_deref_mut(),
                &mut unreadable,
            );
            if live.cancelled() {
                break 'stages;
            }
        }
        // Publishing a bucket that no later stage will narrow again.
        //
        // Buckets are disjoint, so a bucket's first member names it uniquely and
        // is what keeps a bucket from being published twice as it is carried
        // through `pending` from one stage to the next. Nothing here mutates
        // `pending`, so the non-live path — where `live` is `None` and the whole
        // closure is a branch that falls through — runs exactly the code it ran
        // before this milestone.
        let mut streamed: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut stream =
            |records: &[FileRec], buckets: &[Vec<usize>], final_now: &dyn Fn(u64) -> bool| {
                let Some(live) = live else { return };
                for bucket in buckets {
                    if final_now(records[bucket[0]].size) && streamed.insert(bucket[0]) {
                        live.publish(records, bucket, false);
                    }
                }
            };

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
        pulse.stage(pulse::HEAD, staged.len() as u64);
        let end_buckets = hash_stage(
            &shallow,
            &mut records,
            &staged,
            ends,
            &mut unreadable,
            cache.as_deref_mut(),
            holding.then_some(held.as_mut_slice()),
        );
        pending = end_buckets
            .into_iter()
            .filter(|bucket| bucket.len() > 1)
            .collect();
        if keep {
            release(&mut records, &staged, &pending);
        }
        // A bucket no bigger than the page just read is as narrow as it will ever
        // get: the deep head only looks at files bigger than one page, and the
        // sampler only at files bigger than a span. So it goes to the browser now.
        stream(&records, &pending, &|size| size <= HEAD_BYTES as u64);

        // A safe point: nothing is half-decided here, so what has been learned
        // can be committed, a request can be answered, and a cancellation can
        // be obeyed. `settle` is timed, so a scan short enough to finish
        // inside one interval writes nothing extra and the frozen path stays
        // frozen; the other two are no-ops off the live path.
        if let Some(cache) = cache.as_deref_mut() {
            cache.settle();
        }
        if let Some(live) = live {
            serve(
                live,
                &mut records,
                &serving,
                cache.as_deref_mut(),
                &mut unreadable,
            );
            if live.cancelled() {
                break 'stages;
            }
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
            pulse.stage(pulse::DEEP_HEAD, crowded.len() as u64);
            let rest_buckets = hash_stage(
                &reader,
                &mut records,
                &crowded,
                Probe::DeepHead { keep: keep_rest },
                &mut unreadable,
                cache.as_deref_mut(),
                holding.then_some(held.as_mut_slice()),
            );
            pending.extend(rest_buckets.into_iter().filter(|bucket| bucket.len() > 1));
            if keep {
                release(&mut records, &crowded, &pending);
            }
        }
        // Past the deep head, only the sampler is left, and it only looks at files
        // bigger than a span — and not at all on a spinning disk, where the whole
        // stage is skipped. So everything the sampler will not touch is settled.
        let sampling = profile != Profile::Spinning;
        stream(&records, &pending, &|size| !sampling || size <= SAMPLE_SPAN);

        // A safe point: nothing is half-decided here, so what has been learned
        // can be committed, a request can be answered, and a cancellation can
        // be obeyed. `settle` is timed, so a scan short enough to finish
        // inside one interval writes nothing extra and the frozen path stays
        // frozen; the other two are no-ops off the live path.
        if let Some(cache) = cache.as_deref_mut() {
            cache.settle();
        }
        if let Some(live) = live {
            serve(
                live,
                &mut records,
                &serving,
                cache.as_deref_mut(),
                &mut unreadable,
            );
            if live.cancelled() {
                break 'stages;
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
            pulse.stage(pulse::SAMPLE, survivors.len() as u64);
            let sample_buckets = hash_stage(
                &reader,
                &mut records,
                &survivors,
                Probe::Sample,
                &mut unreadable,
                cache.as_deref_mut(),
                holding.then_some(held.as_mut_slice()),
            );
            pending.extend(sample_buckets.into_iter().filter(|bucket| bucket.len() > 1));
        }
        // Every narrowing stage has run. Whatever is still in a bucket with
        // somebody else is a candidate, and on the live path it is the last thing
        // the browser hears about before stage 5 is deferred out from under it.
        stream(&records, &pending, &|_| true);

        // A safe point: nothing is half-decided here, so what has been learned
        // can be committed, a request can be answered, and a cancellation can
        // be obeyed. `settle` is timed, so a scan short enough to finish
        // inside one interval writes nothing extra and the frozen path stays
        // frozen; the other two are no-ops off the live path.
        if let Some(cache) = cache.as_deref_mut() {
            cache.settle();
        }
        if let Some(live) = live {
            serve(
                live,
                &mut records,
                &serving,
                cache.as_deref_mut(),
                &mut unreadable,
            );
            if live.cancelled() {
                break 'stages;
            }
        }
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
    if live.is_some() {
        // M8-LIVE.md: stage 5 becomes DEFERRED for the live path. The browser
        // already has every one of these buckets as a candidate, and the whole
        // point of the milestone is not to make the user sit through the
        // expensive read for groups they are never going to delete.
        //
        // They join `remembered` rather than `settled`, which is not a
        // convenience: `attested` is precisely "matched without reading the
        // bytes this scan", and it is the flag `run_selected` keys on when it
        // sends a group through `verify_attested` before removing anything
        // from it. So the deferral reuses the guard that already exists rather
        // than introducing a second, weaker one.
        remembered.extend(std::mem::take(&mut pending));
    } else {
        let context = Compare {
            root,
            effort,
            profile,
            solid: device.all_solid_state(),
            learn: cache.is_some(),
        };
        let sequential_pool = pool_of(sequential_jobs);
        pulse.stage(
            pulse::COMPARE,
            pending.iter().map(|bucket| bucket.len() as u64).sum(),
        );
        let compared = compare_stage(
            &context,
            &records,
            &pending,
            sequential_jobs,
            &sequential_pool,
            &mut unreadable,
            pulse,
            holding.then_some(held.as_mut_slice()),
        );
        settled.extend(compared.matched);
        // The bytes went past anyway, so a group read here is also a group
        // learned. The digests come back from the pool rather than being
        // written from inside it, so the cache never needs a lock.
        if let Some(cache) = cache.as_deref_mut() {
            for (index, digest) in compared.learned {
                cache.remember(&records[index], crate::cache::Field::Full, digest);
            }
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

/// Answer whatever the browser has asked for, at a point where nothing is
/// half-decided.
///
/// Called only from the scan thread, and only between stages — never inside a
/// comparison sweep, so no group's class is ever left part-way through and no
/// pivot cursor is ever perturbed by a pause.
///
/// The records are lent to a `Scan` for the length of the request and taken
/// straight back: a pointer swap, not a copy. Building a real `Scan` is the
/// point rather than an inconvenience — it is what keeps `ProvenGroups` the
/// only key to deletion. This path mints its token exactly the way `--auto`
/// does, from `Scan::proven`, and `run_selected` cannot tell the two apart.
///
/// Every group that reaches `run_selected` here has just had its bytes
/// compared by `prove_marked`, so `attested` is false on all of them and
/// `verify_attested` reads nothing a second time.
pub fn serve(
    live: &crate::live::Live,
    records: &mut Vec<FileRec>,
    context: &Compare,
    cache: Option<&mut ScanCache>,
    unreadable: &mut Vec<String>,
) -> bool {
    let Some(crate::live::Ask::Apply { marks, hard }) = live.taken_ask() else {
        return false;
    };
    let mut want: Vec<usize> = marks.iter().map(|mark| mark.group).collect();
    want.sort_unstable();
    want.dedup();

    // The proof, for exactly the groups the marks touch and nothing else.
    let verdicts = {
        let runs = live.runs();
        prove_marked(context, records, &runs, &want, unreadable)
    };
    let mut broke = 0;
    let mut standing: Vec<usize> = Vec::new();
    for verdict in &verdicts {
        if verdict.whole {
            live.now_proven(verdict.id);
            standing.push(verdict.id);
        } else {
            // The bytes disagree with the narrowing stages. Nothing from this
            // group is deleted; what is left is what the bytes actually say.
            live.resplit(verdict.id, &verdict.classes);
            broke += 1;
        }
    }

    // Only marks on groups that survived their proof go anywhere near the
    // remover, and the marks on a group that came apart are simply dropped.
    let keeping: Vec<crate::tui::model::Mark> = marks
        .into_iter()
        .filter(|mark| standing.contains(&mark.group))
        .collect();

    let groups: Vec<DupGroup> = live
        .runs()
        .iter()
        .map(|(members, proven)| DupGroup {
            members: members.clone(),
            attested: !proven,
        })
        .collect();
    let mut lent = Scan {
        records: std::mem::take(records),
        profile: context.profile,
        hint: None,
        groups,
        // Honest: every group here is either compared this scan or flagged
        // `attested`, and `run_selected` sends every attested group through a
        // full byte comparison before removing anything from it.
        proven: true,
        files_seen: 0,
        bytes_seen: 0,
        unreadable: Vec::new(),
    };
    let answer = if keeping.is_empty() {
        Err(String::new())
    } else {
        match lent.proven() {
            Some(token) => crate::trash::run_selected(&lent, token, &keeping, hard, context, cache)
                .map_err(|why| why),
            None => Err("these groups were never compared byte for byte".to_string()),
        }
    };
    *records = std::mem::take(&mut lent.records);

    let answer = match answer {
        Ok(mut removed) => {
            let gone = std::mem::take(&mut removed.records);
            live.forget(records, &gone);
            crate::live::Answer::Applied(Box::new(crate::tui::model::Outcome {
                files: removed.files,
                bytes: removed.bytes,
                removed: gone,
                refused: removed.refused.len() + broke,
                failures: std::mem::take(&mut removed.failures),
            }))
        }
        // Nothing was removable, but the proof still changed what the list
        // says. Republish before answering, or the browser would keep drawing
        // a group the bytes have just disowned.
        Err(why) if why.is_empty() => {
            live.forget(records, &[]);
            crate::live::Answer::Applied(Box::new(crate::tui::model::Outcome {
                refused: broke,
                ..Default::default()
            }))
        }
        Err(why) => {
            if broke > 0 {
                live.forget(records, &[]);
            }
            crate::live::Answer::Refused(why)
        }
    };
    live.answer(answer);
    true
}

/// The byte-equality classes of one candidate group, read now.
///
/// This is the proof M8-LIVE.md defers stage 5 to. It runs the same
/// comparison the scan would have run — `split_identical` on flash,
/// `split_identical_capped` on spinning, the same width cap and the same chunk
/// — so a group proved here is proved by exactly the code that proves one
/// during an ordinary scan. Nothing about the answer depends on it being late.
fn byte_classes(
    context: &Compare,
    records: &[FileRec],
    members: &[usize],
    unreadable: &mut Vec<String>,
) -> Vec<Vec<usize>> {
    let chunk = crate::compare::compare_chunk(context.profile, members.len(), context.solid);
    let settled = if context.profile == Profile::Spinning {
        crate::compare::split_identical_capped(context, records, members, chunk, unreadable)
    } else {
        crate::compare::split_identical(
            context,
            records,
            members,
            chunk,
            unreadable,
            None,
            Vec::new(),
        )
    };
    settled.matched
}

/// What the apply-time proof decided about one marked group.
pub struct Proved {
    /// The group's id, as the browser knows it.
    pub id: usize,
    /// True when every member really is identical and the group stands whole.
    pub whole: bool,
    /// What the bytes actually say: one run per byte-equality class. A group
    /// that came apart has more than one, or one that is smaller than the
    /// group was.
    pub classes: Vec<Vec<usize>>,
}

/// Prove exactly the groups the user marked, before anything is removed.
///
/// M8-LIVE.md: "The full byte proof for a group runs when it is needed: at
/// APPLY, for exactly the groups the user marked, before anything is trashed."
///
/// A group that fails is not merely refused — its real classes come back, so
/// the browser can show what the bytes actually say instead of a group it can
/// no longer justify. Nothing from a failed group is deleted: only `whole`
/// groups go on to `run_selected`, and the marks on the others are dropped.
pub fn prove_marked(
    context: &Compare,
    records: &[FileRec],
    runs: &[(Vec<usize>, bool)],
    want: &[usize],
    unreadable: &mut Vec<String>,
) -> Vec<Proved> {
    let mut out = Vec::new();
    for &id in want {
        let Some((members, proven)) = runs.get(id) else {
            continue;
        };
        if *proven {
            // Already read this scan. Reading it again would be honest and
            // pointless; the small-file stage held every byte of it.
            out.push(Proved {
                id,
                whole: true,
                classes: vec![members.clone()],
            });
            continue;
        }
        let classes = byte_classes(context, records, members, unreadable);
        let whole = classes.len() == 1 && classes[0].len() == members.len();
        out.push(Proved { id, whole, classes });
    }
    out
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
    use crate::device::UNKNOWN_MEDIUM;

    /// A scratch tree of two identical files, big enough to reach stage 3.
    struct Tree(std::path::PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Tree {
        fn new(name: &str) -> Tree {
            let root = std::env::temp_dir().join(format!(
                "feat-auto-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("a scratch directory");
            let content = vec![0x7E_u8; DEEP_HEAD * 4];
            for name in ["one.bin", "two.bin"] {
                std::fs::write(root.join(name), &content).expect("a scratch file");
            }
            Tree(root)
        }

        fn scan(&self, solid: Option<bool>, wanted: Wanted) -> Scan {
            let effort = Effort::default();
            find_duplicates(
                &self.0,
                Filters {
                    min_size: 1,
                    include_ext: None,
                    skip_ext: &[],
                    excludes: &[],
                },
                &mut VolumeProbe::answered(solid, Some(1)),
                &effort,
                None,
                wanted,
                &Pulse::off(),
                None,
            )
        }
    }

    #[test]
    fn auto_runs_the_spinning_pipeline_on_a_disk_that_says_it_spins() {
        // Not just that `resolve` returns the right word — that the scan the
        // word chose is the scan that ran, all the way down.
        let tree = Tree::new("rotational");
        let scan = tree.scan(Some(false), Wanted::Auto);
        assert_eq!(scan.profile, Profile::Spinning);
        assert_eq!(scan.hint, None);
        assert_eq!(scan.reported().len(), 1);
        assert_eq!(scan.reported()[0].members.len(), 2);
    }

    #[test]
    fn auto_stays_on_flash_for_a_disk_that_will_not_say_and_says_so_once() {
        let tree = Tree::new("unknown");
        let scan = tree.scan(None, Wanted::Auto);
        assert_eq!(scan.profile, Profile::Flash);
        assert_eq!(
            scan.hint,
            Some(UNKNOWN_MEDIUM),
            "an unknown medium gets the hint that names the flag"
        );
        assert!(scan.hint.expect("a hint").contains("--profile spinning"));
        assert_eq!(scan.reported().len(), 1);
    }

    #[test]
    fn a_disk_that_says_it_is_flash_gets_flash_and_no_advice() {
        let tree = Tree::new("solid");
        let scan = tree.scan(Some(true), Wanted::Auto);
        assert_eq!(scan.profile, Profile::Flash);
        assert_eq!(scan.hint, None);
    }

    #[test]
    fn an_explicit_profile_overrules_the_disk_and_never_advises() {
        let tree = Tree::new("forced");
        for solid in [Some(true), Some(false), None] {
            let scan = tree.scan(solid, Wanted::Spinning);
            assert_eq!(scan.profile, Profile::Spinning);
            assert_eq!(scan.hint, None);
            assert_eq!(scan.reported().len(), 1, "and it still finds the pair");
            let scan = tree.scan(solid, Wanted::Flash);
            assert_eq!(scan.profile, Profile::Flash);
            assert_eq!(scan.hint, None);
        }
    }

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
