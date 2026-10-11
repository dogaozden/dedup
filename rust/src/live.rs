//! The live scan: the one piece of state the browser and the scan share.
//!
//! # Who writes what
//!
//! Exactly two threads touch a `Shared`, and the split is deliberate:
//!
//! * **The scan thread** writes `groups`, `finished`, `answer` and `unreadable`.
//!   It is the only thread that ever reads a byte off the scanned device, the
//!   only thread that touches the `ScanCache`, and the only thread that owns
//!   the scan's `records`. Nothing it publishes is ever mutated again once
//!   published — a `Found` is append-only.
//! * **The browser thread** writes `asked` and `cancel`, and reads everything
//!   else. It never opens a file, never reads the disk, and never touches the
//!   cache. Drawing progress is a lock, a memcpy of six numbers, and an
//!   unlock.
//!
//! The `Mutex` covers the whole non-atomic body: the group list, the request
//! slot, the answer slot and the finished flag. `cancel` is an `AtomicBool`
//! because it is read on the scan thread and set on the browser thread and
//! needs nothing more than that.
//!
//! **Where the scan looks.** Both `cancelled` and `taken_ask` are polled at
//! the four stage boundaries of `pipeline`'s `'stages:` block — immediately
//! after `cache.settle()` — and nowhere else, plus the serving loop that runs
//! once the stages are done. Not per file, and not inside a hash stage or a
//! comparison sweep: a request answered mid-stage would be answered while a
//! stage was rewriting the records the deletion walks, which is the race the
//! apply-pauses-scan call exists to avoid.
//!
//! So a cancellation takes effect at the next stage boundary rather than
//! immediately. REVIEW-M8 non-binding note 3 measured what that costs on a
//! 3 GiB corpus: `q` mid-scan took 0.21s against 0.05s after the scan. The
//! comments here used to claim a per-file poll, which would have promised a
//! promptness the code does not have on a tree far larger than that.
//!
//! # Why the disk work is all on one side
//!
//! M7's sweep is built on "one disk, one queue": a second reader working the
//! same platter while the sweep is mid-window is exactly the interleaving the
//! pivot window was written to remove. So the browser asks and the scan
//! answers, rather than the browser reading for itself. That also means the
//! cache — which is per *volume*, shared by every scan touching it — has a
//! single writer for the whole session, and the concurrent-cache-writer race
//! simply does not exist to be reasoned about.
//!
//! # Nothing here runs when nobody is browsing
//!
//! Every hook the pipeline calls takes `Option<&Live>`. On `--list`, `--auto`,
//! a pipe, and every conformance and bench invocation, that option is `None`
//! and each hook is a branch that falls straight through — the same shape
//! `Pulse::off()` uses, and for the same reason. M8-LIVE.md's absolute fence
//! is that those runs keep today's pipeline and today's bytes, byte for byte.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::folders::{self, FolderGroup};
use crate::pipeline::DupGroup;
use crate::tui::model::{Group, Mark, Outcome, FOLDER_BASE};
use crate::walk::FileRec;
use std::collections::HashSet;

/// What the browser has asked the scan to do, and what came back.
///
/// There is one slot for each because the browser blocks until its request is
/// answered: it cannot ask twice, so a queue would be a queue of at most one.
pub enum Ask {
    /// Prove exactly these marks, and remove what survives the proof.
    ///
    /// The proof is the whole point: a streamed group is a candidate, and this
    /// is where it stops being one. See `pipeline::prove_and_remove`.
    Apply { marks: Vec<Mark>, hard: bool },
}

pub enum Answer {
    Applied(Box<Outcome>),
    /// The scan refused the request and said why — a survivor guard, or a
    /// group whose copies stopped matching. Information, not a crash.
    Refused(String),
}

#[derive(Default)]
struct Body {
    /// Groups in the order they settled. Append-only: an index into this
    /// vector names the same group for the whole session.
    groups: Vec<Group>,
    /// True once the scan has stopped finding groups, whether it ran out of
    /// stages or was cancelled. The list is complete and the progress display
    /// goes away; the scan thread may still be there to answer.
    finished: bool,
    /// True while the scan thread is still able to answer a request.
    ///
    /// Separate from `finished` because the two really are different: a
    /// finished scan still serves deletions — that is the ordinary case, a
    /// user who let the scan complete before marking anything — and only a
    /// thread that has actually gone can refuse one.
    serving: bool,
    /// Set when the scan stopped early because the browser asked it to.
    cancelled: bool,
    asked: Option<Ask>,
    answer: Option<Answer>,
    /// Set by `republish`, taken by `fresh`. An explicit flag rather than
    /// something inferred from the lengths, because "the list got shorter" and
    /// "the list was rebuilt" are different events and only one of them means
    /// the browser has to start over.
    replaced: bool,
}

struct Shared {
    body: Mutex<Body>,
    /// Woken when the scan publishes a group, answers a request, or finishes,
    /// so an idle browser is not polling a lock.
    stirred: Condvar,
    cancel: AtomicBool,
}

/// The scan thread's end.
///
/// It carries what turning a settled group into rows needs — the display
/// conventions and the scanned root — because the scan thread is the side that
/// owns the records, and a group has to become rows before it can cross to a
/// browser that has no records to look at.
pub struct Live {
    shared: Arc<Shared>,
    shown: crate::tui::Shown,
    root: std::path::PathBuf,
    /// What every published group is made of, by id: the records in it, and
    /// whether its bytes have been compared.
    ///
    /// Scan-side only — the browser gets rows, not record indices. This is
    /// what a `Mark` is resolved against when the browser asks for a deletion,
    /// and it is why the scan can serve that request without the browser ever
    /// holding a record.
    runs: std::cell::RefCell<Vec<(Vec<usize>, bool)>>,
    /// Whether the folder pass may read cloud placeholders in its proof.
    cloud: bool,
    /// The folder candidates on screen, in the order their browser ids name them.
    /// Recomputed after every apply, since membership changes then.
    folders: std::cell::RefCell<Vec<FolderGroup>>,
    /// For each folder candidate, the file groups it folds, by run id.
    folded_of: std::cell::RefCell<Vec<Vec<usize>>>,
    /// Folder pairs an apply has proved different this session. They are never
    /// offered again, so a refused folder does not come back as a candidate.
    refuted: std::cell::RefCell<HashSet<(String, String)>>,
}

/// The browser thread's end.
pub struct Watch(Arc<Shared>);

/// Build both ends of one live scan.
pub fn channel(root: &std::path::Path, cloud: bool) -> (Live, Watch) {
    let shared = Arc::new(Shared {
        body: Mutex::new(Body::default()),
        stirred: Condvar::new(),
        cancel: AtomicBool::new(false),
    });
    {
        let mut start = body(&shared);
        start.serving = true;
    }
    (
        Live {
            shared: Arc::clone(&shared),
            shown: crate::tui::Shown::new(),
            root: root.to_path_buf(),
            runs: std::cell::RefCell::new(Vec::new()),
            cloud,
            folders: std::cell::RefCell::new(Vec::new()),
            folded_of: std::cell::RefCell::new(Vec::new()),
            refuted: std::cell::RefCell::new(HashSet::new()),
        },
        Watch(shared),
    )
}

/// Releases anyone waiting on the scan when the scan thread's scope is left.
/// See `Live::leaving`.
pub struct Leaving<'a>(&'a Live);

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.0.gone();
    }
}

/// Stops the scan when the scope holding the browser is left. See
/// `Watch::stopping`.
pub struct Stopping<'a>(&'a Watch);

impl Drop for Stopping<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// A lock that a panicking peer cannot take away.
///
/// The browser owns the terminal; if the scan thread panics holding this lock,
/// a browser that then blocked forever would leave the user staring at a dead
/// alternate screen. Taking the poisoned body is right here because nothing in
/// it can be left half-written: every field is replaced whole under the lock.
fn body(shared: &Shared) -> MutexGuard<'_, Body> {
    shared.body.lock().unwrap_or_else(|held| held.into_inner())
}

impl Live {
    /// Publish a group the scan has settled on.
    ///
    /// `proven` says whether the bytes have actually been compared. A group
    /// that streams in from the narrowing stages has NOT been, and says so —
    /// see `Group::proven` and M8-LIVE.md's prove-on-delete.
    pub fn found(&self, group: Group) {
        let mut body = body(&self.shared);
        body.groups.push(group);
        drop(body);
        self.shared.stirred.notify_all();
    }

    /// Turn a settled run of records into rows and publish it.
    ///
    /// The id is handed out here, in arrival order, and is the group's
    /// identity for the rest of the session: it is what a mark names and what
    /// a streaming insertion anchors the cursor to.
    pub fn publish(&self, records: &[FileRec], members: &[usize], proven: bool) {
        let id = {
            let mut runs = self.runs.borrow_mut();
            runs.push((members.to_vec(), proven));
            runs.len() - 1
        };
        self.found(self.shown.group(records, members, &self.root, id, proven));
    }

    /// Publish every run in a batch, in the order the stage settled them.
    pub fn publish_all(&self, records: &[FileRec], runs: &[Vec<usize>], proven: bool) {
        for members in runs {
            self.publish(records, members, proven);
        }
    }

    /// Every published group, as `(members, proven)`, indexed by id.
    pub fn runs(&self) -> std::cell::Ref<'_, Vec<(Vec<usize>, bool)>> {
        self.runs.borrow()
    }

    /// Forget the records a deletion removed, and rebuild the rows the browser
    /// draws from what is left.
    ///
    /// The scan owns the records, so the scan is the side that decides what a
    /// group is now made of. A group that drops to one copy stops being a
    /// group; its id is retired rather than reused, so a mark that named it
    /// resolves to nothing instead of to somebody else.
    pub fn forget(&self, records: &[FileRec], gone: &[usize]) {
        let gone: std::collections::HashSet<usize> = gone.iter().copied().collect();
        {
            let mut runs = self.runs.borrow_mut();
            for (members, _) in runs.iter_mut() {
                members.retain(|index| !gone.contains(index));
            }
        }
        self.recompute(records);
        let rows = self.rows(records);
        self.republish(rows);
    }

    /// Look for folder candidates among the groups published so far, and
    /// publish them. Called once, when the stages have settled: a folder group
    /// may only appear then, and it arrives as a candidate.
    pub fn publish_folders(&self, records: &[FileRec]) {
        self.recompute(records);
        for row in self.folder_rows(records) {
            self.found(row);
        }
    }

    /// Whether folder candidates may read cloud placeholders in their proof.
    pub fn cloud(&self) -> bool {
        self.cloud
    }

    /// The folder candidates, handed to the scan so an apply can prove them.
    /// Given back by `put_folders` once the apply has run.
    pub fn take_folders(&self) -> Vec<FolderGroup> {
        std::mem::take(&mut *self.folders.borrow_mut())
    }

    pub fn put_folders(&self, folders: Vec<FolderGroup>) {
        *self.folders.borrow_mut() = folders;
    }

    /// The folder candidates' member paths, which an apply names when it moves
    /// one. Read before the apply, so the names are the ones it proved.
    pub fn folder_paths(&self, number: usize) -> Vec<String> {
        self.folders
            .borrow()
            .get(number)
            .map(|group| {
                group
                    .folders
                    .iter()
                    .map(|folder| folder.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Never offer the folder pairs of a candidate an apply has proved different.
    pub fn refute(&self, number: usize) {
        let paths = self.folder_paths(number);
        let mut refuted = self.refuted.borrow_mut();
        for (at, first) in paths.iter().enumerate() {
            for second in &paths[at + 1..] {
                let pair = if first < second {
                    (first.clone(), second.clone())
                } else {
                    (second.clone(), first.clone())
                };
                refuted.insert(pair);
            }
        }
    }

    /// Rebuild the folder candidates from the groups as they stand now, and the
    /// file groups each one folds.
    fn recompute(&self, records: &[FileRec]) {
        let runs = self.runs.borrow();
        let ids: Vec<usize> = (0..runs.len()).filter(|&id| runs[id].0.len() > 1).collect();
        let groups: Vec<DupGroup> = ids
            .iter()
            .map(|&id| DupGroup {
                members: runs[id].0.clone(),
                // A candidate, not yet proved, is what the browser calls attested
                // and what the apply must prove again before anything moves.
                attested: !runs[id].1,
            })
            .collect();
        let refuted = self.refuted.borrow();
        let found = folders::live_candidates(records, &groups, &self.root, self.cloud, &refuted);
        let active: Vec<usize> = (0..found.len()).collect();
        let folded = folders::fold(records, &groups, &found, &active);
        let mut folded_of = vec![Vec::new(); found.len()];
        for (group, into) in folded.iter().enumerate() {
            if let Some(number) = into {
                folded_of[*number].push(ids[group]);
            }
        }
        drop(runs);
        *self.folders.borrow_mut() = found;
        *self.folded_of.borrow_mut() = folded_of;
    }

    /// The rows the browser should show: every file group that no folder
    /// candidate folds, and every folder candidate with the file groups it folds.
    fn rows(&self, records: &[FileRec]) -> Vec<Group> {
        let runs = self.runs.borrow();
        let folded_of = self.folded_of.borrow();
        let folded: HashSet<usize> = folded_of.iter().flatten().copied().collect();
        let mut rows: Vec<Group> = runs
            .iter()
            .enumerate()
            .filter(|(id, (members, _))| members.len() > 1 && !folded.contains(id))
            .map(|(id, (members, proven))| {
                self.shown.group(records, members, &self.root, id, *proven)
            })
            .collect();
        drop(runs);
        drop(folded_of);
        rows.extend(self.folder_rows(records));
        rows
    }

    /// The folder candidates as rows, each with the file groups it folds.
    fn folder_rows(&self, records: &[FileRec]) -> Vec<Group> {
        let runs = self.runs.borrow();
        let folders = self.folders.borrow();
        let folded_of = self.folded_of.borrow();
        folders
            .iter()
            .enumerate()
            .map(|(number, group)| {
                let folded = folded_of[number]
                    .iter()
                    .map(|&id| {
                        let (members, proven) = &runs[id];
                        self.shown.group(records, members, &self.root, id, *proven)
                    })
                    .collect();
                self.shown
                    .folder_group(group, FOLDER_BASE + number, folded, &self.root)
            })
            .collect()
    }

    /// Mark a group as having had its bytes compared.
    ///
    /// Called when the apply-time proof settles a candidate: the group is the
    /// same group, it is simply no longer a guess.
    pub fn now_proven(&self, id: usize) {
        if let Some((_, proven)) = self.runs.borrow_mut().get_mut(id) {
            *proven = true;
        }
    }

    /// Replace a candidate that failed its proof with what the bytes actually
    /// say.
    ///
    /// The group keeps its id and becomes its first real class; the other
    /// classes arrive as new groups with new ids. Every one of them is proven,
    /// because the comparison that produced them is the proof. A class of one
    /// is not a duplicate and is dropped — the file is simply not a copy of
    /// anything, which is the honest thing to show and the honest thing to
    /// leave alone.
    pub fn resplit(&self, id: usize, classes: &[Vec<usize>]) {
        let mut runs = self.runs.borrow_mut();
        let Some(slot) = runs.get_mut(id) else { return };
        let mut classes = classes.iter().filter(|class| class.len() > 1);
        match classes.next() {
            Some(first) => *slot = (first.clone(), true),
            // Nothing in the group matched anything else in it. It stops being
            // a group at all.
            None => *slot = (Vec::new(), true),
        }
        for rest in classes {
            runs.push((rest.clone(), true));
        }
    }

    /// Whether the browser has asked the scan to stop.
    ///
    /// Read at the four stage boundaries and in the serving loop — see the
    /// module note. `Acquire` rather than `Relaxed` because the scan must see
    /// everything the browser wrote before it set the flag.
    #[inline]
    pub fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Acquire)
    }

    /// Take whatever the browser has asked for, if anything.
    ///
    /// Called at the scan's safe points, which are the four STAGE boundaries
    /// and the serving loop — never between files, and never inside a hash
    /// stage or a comparison sweep, so a request can never be served with a
    /// group's class half-decided or with a stage part-way through rewriting
    /// the records a deletion would walk.
    pub fn taken_ask(&self) -> Option<Ask> {
        // The fast path is a lock and a `None`, taken a handful of times per
        // scan. It is only ever reached on the interactive path.
        let mut body = body(&self.shared);
        body.asked.take()
    }

    /// Answer the request the browser is blocked on.
    pub fn answer(&self, answer: Answer) {
        let mut body = body(&self.shared);
        body.answer = Some(answer);
        drop(body);
        self.shared.stirred.notify_all();
    }

    /// Replace the published group list wholesale.
    ///
    /// Used once, when a round of deletion has changed what is left: the scan
    /// thread owns the records and the groups, so it is the side that rebuilds
    /// them, and the browser re-reads rather than trying to patch its own copy
    /// in step with someone else's.
    pub fn republish(&self, groups: Vec<Group>) {
        let mut body = body(&self.shared);
        body.groups = groups;
        body.replaced = true;
        drop(body);
        self.shared.stirred.notify_all();
    }

    /// The scan has stopped finding groups. Nothing is published after this,
    /// but requests are still answered until `gone`.
    pub fn finished(&self) {
        let mut body = body(&self.shared);
        body.finished = true;
        body.cancelled = self.shared.cancel.load(Ordering::Acquire);
        drop(body);
        self.shared.stirred.notify_all();
    }

    /// Say the scan thread is leaving however the scope holding it is left —
    /// returned, or panicked through.
    ///
    /// A browser blocked inside `apply` is a browser that is not drawing, not
    /// reading keys, and not restoring the terminal. If the scan falls over
    /// while it is in there, something has to let it out, and a drop guard is
    /// the only thing that runs on both ways out.
    pub fn leaving(&self) -> Leaving<'_> {
        Leaving(self)
    }

    /// The scan thread is leaving. Nothing is answered after this.
    pub fn gone(&self) {
        let mut body = body(&self.shared);
        body.finished = true;
        body.serving = false;
        drop(body);
        self.shared.stirred.notify_all();
    }

    /// Wait for a request, or for the browser to ask the scan to stop.
    ///
    /// Used by the serving loop after the stages have run out: there is
    /// nothing left to do but answer, so it waits rather than spins.
    pub fn idle(&self, timeout: std::time::Duration) {
        let guard = body(&self.shared);
        if guard.asked.is_some() || self.cancelled() {
            return;
        }
        let _ = self.shared.stirred.wait_timeout(guard, timeout);
    }
}

/// What the browser learned in one look.
pub struct Fresh {
    /// Groups published since the last look, in the order they settled.
    pub groups: Vec<Group>,
    /// True once the scan has stopped for any reason.
    pub finished: bool,
    pub cancelled: bool,
    /// Set when the whole list was replaced rather than added to.
    pub replaced: bool,
}

impl Watch {
    /// Everything published since the last call. Never blocks.
    ///
    /// Groups are moved out rather than cloned — a group carries a string per
    /// member and REWRITE.md §6 counts per-file costs — so `body.groups` is
    /// emptied on every look and refills from the next `found`. There is no
    /// cursor and no need for one: nothing indexes this list, and `republish`
    /// rebuilds what the browser draws from `Live::runs` rather than from
    /// anything kept here.
    ///
    /// REVIEW-M8 non-binding note 2: this used to carry a `taken` cursor and a
    /// comment describing slots that stay behind. Neither was real — `groups`
    /// is a plain `Vec<Group>`, `drain` empties it, so `taken` was permanently
    /// zero and the `.min()` guarding it was dead. The behaviour was right and
    /// the description was not, which is the worse of the two ways to be
    /// wrong; the cursor is gone rather than implemented.
    pub fn fresh(&self) -> Fresh {
        let mut body = body(&self.0);
        Fresh {
            groups: std::mem::take(&mut body.groups),
            finished: body.finished,
            cancelled: body.cancelled,
            replaced: std::mem::take(&mut body.replaced),
        }
    }

    /// Stop the scan however the scope holding the browser is left —
    /// returned, errored, or panicked through.
    ///
    /// The mirror of `Live::leaving`, and needed for the mirror-image reason.
    /// `live_scan` runs the browser inside a `std::thread::scope`, which will
    /// not return until the scan thread does, and the scan thread's serving
    /// loop runs `while !self.cancelled()`. A panic on the browser thread
    /// skips the `cancel()` after `run_live` and unwinds into the scope's
    /// join — so the scan spins on, the join never finishes, and the panic is
    /// never resumed. REVIEW-M8 item 1 measured that: the process was still
    /// alive 25 seconds later and had to be killed. The panic hook restores
    /// the terminal, so the user gets a working shell and a process that will
    /// not die.
    pub fn stopping(&self) -> Stopping<'_> {
        Stopping(self)
    }

    /// Ask the scan to stop at its next safe point.
    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Release);
        self.0.stirred.notify_all();
    }

    /// Ask the scan to prove and remove, and block until it answers.
    ///
    /// Blocking is the design call M8-LIVE.md leaves open (see LOG.md): the
    /// scan pauses while a deletion runs. The user is looking at a
    /// confirmation dialog they just answered, so there is nothing on screen
    /// that a paused scan makes worse, and the proof gets to read a tree no
    /// other thread is touching.
    pub fn apply(&self, marks: Vec<Mark>, hard: bool) -> Answer {
        let mut guard = body(&self.0);
        guard.asked = Some(Ask::Apply { marks, hard });
        drop(guard);
        self.0.stirred.notify_all();
        let mut guard = body(&self.0);
        loop {
            if let Some(answer) = guard.answer.take() {
                return answer;
            }
            // A scan thread that has gone will never take the request, so the
            // caller is told rather than left waiting for something that is
            // not going to run again. A merely FINISHED scan still answers.
            if !guard.serving {
                guard.asked = None;
                return Answer::Refused("the scan has stopped".into());
            }
            guard = self
                .0
                .stirred
                .wait(guard)
                .unwrap_or_else(|held| held.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{Group, Member};

    fn group(id: usize, size: u64, proven: bool) -> Group {
        Group {
            index: id,
            size,
            proven,
            folder: None,
            members: vec![Member {
                record: id,
                name: format!("f{id}.bin"),
                path: format!("f{id}.bin"),
                date: "2026-08-24".into(),
                extra_links: 0,
                deletable: true,
                mtime: 1.0,
            }],
        }
    }

    #[test]
    fn a_group_published_is_a_group_the_browser_sees_once() {
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        live.found(group(0, 100, false));
        live.found(group(1, 50, false));
        let fresh = watch.fresh();
        assert_eq!(fresh.groups.len(), 2);
        assert!(!fresh.finished);
        // ...and only once. A browser that saw a group twice would draw it
        // twice and mark it twice.
        assert!(watch.fresh().groups.is_empty());

        live.found(group(2, 25, false));
        let fresh = watch.fresh();
        assert_eq!(fresh.groups.len(), 1);
        assert_eq!(fresh.groups[0].index, 2);
    }

    #[test]
    fn cancelling_is_seen_by_the_scan_and_survives_into_the_finish() {
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        assert!(!live.cancelled());
        watch.cancel();
        assert!(live.cancelled());
        live.finished();
        let fresh = watch.fresh();
        assert!(fresh.finished && fresh.cancelled);
    }

    #[test]
    fn a_scan_that_finished_without_cancelling_says_so() {
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        live.finished();
        let fresh = watch.fresh();
        assert!(fresh.finished);
        assert!(!fresh.cancelled, "an ordinary finish looked like a cancel");
    }

    #[test]
    fn an_apply_crosses_to_the_scan_and_the_answer_crosses_back() {
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        let scan = std::thread::spawn(move || {
            // The scan's safe-point poll, as the pipeline runs it.
            loop {
                if let Some(Ask::Apply { marks, hard }) = live.taken_ask() {
                    assert_eq!(marks.len(), 1);
                    assert!(hard);
                    live.answer(Answer::Applied(Box::new(Outcome {
                        files: 1,
                        bytes: 100,
                        removed: vec![marks[0].record],
                        refused: 0,
                        failures: Vec::new(),
                        ..Default::default()
                    })));
                    live.finished();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let answer = watch.apply(
            vec![Mark {
                group: 0,
                record: 7,
            }],
            true,
        );
        match answer {
            Answer::Applied(outcome) => {
                assert_eq!(outcome.files, 1);
                assert_eq!(outcome.removed, vec![7]);
            }
            Answer::Refused(why) => panic!("refused: {why}"),
        }
        scan.join().expect("the scan thread");
    }

    #[test]
    fn an_apply_after_the_scan_thread_has_gone_is_refused_rather_than_waited_on() {
        // The browser must never block on a thread that will not run again.
        // Without this the whole session wedges: the browser is inside
        // `apply`, so it is not drawing, not reading keys, and not quitting.
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        live.gone();
        match watch.apply(Vec::new(), false) {
            Answer::Refused(why) => assert!(why.contains("stopped"), "{why}"),
            Answer::Applied(_) => panic!("a gone scan applied something"),
        }
    }

    #[test]
    fn a_finished_scan_still_answers_a_deletion() {
        // The ordinary case, and the one the split between `finished` and
        // `gone` exists for: the user let the scan run out before marking
        // anything. A browser that could only delete DURING a scan would be a
        // worse tool than the one this milestone started from.
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        live.finished();
        let scan = std::thread::spawn(move || {
            for _ in 0..2000 {
                if live.taken_ask().is_some() {
                    live.answer(Answer::Applied(Box::default()));
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            false
        });
        let answered = matches!(watch.apply(Vec::new(), false), Answer::Applied(_));
        assert!(
            scan.join().expect("the scan thread"),
            "the ask never arrived"
        );
        assert!(answered, "a finished scan refused a deletion");
    }

    #[test]
    fn a_scan_thread_that_falls_over_does_not_wedge_the_browser() {
        // The guard, not the happy path: if the scan panics while the browser
        // is inside `apply`, the browser has to come back out. Otherwise the
        // terminal is never restored, because the loop that restores it is
        // blocked on a condvar nobody will notify again.
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        let scan = std::thread::spawn(move || {
            let _guard = Leaving(&live);
            panic!("the scan fell over");
        });
        // The panic message is expected; the test is that this returns at all.
        let answer = watch.apply(Vec::new(), false);
        assert!(
            matches!(answer, Answer::Refused(_)),
            "a panicking scan left the browser waiting"
        );
        assert!(scan.join().is_err(), "the scan did not actually panic");
    }

    #[test]
    fn republishing_replaces_the_list_and_the_browser_is_told_it_was_replaced() {
        let (live, watch) = channel(std::path::Path::new("/scratch"), false);
        live.found(group(0, 100, false));
        live.found(group(1, 50, false));
        let fresh = watch.fresh();
        assert_eq!(fresh.groups.len(), 2);
        assert!(!fresh.replaced);

        // A round of deletion left one group behind.
        live.republish(vec![group(1, 50, true)]);
        let fresh = watch.fresh();
        assert!(fresh.replaced, "the browser was not told to start over");
        assert_eq!(fresh.groups.len(), 1);
        assert_eq!(fresh.groups[0].index, 1);
        assert!(fresh.groups[0].proven);
    }
}
