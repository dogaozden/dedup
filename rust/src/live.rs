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
//! because the scan reads it in its innermost per-file loop, where taking a
//! lock per file would be a cost on a path this milestone is not allowed to
//! slow down.
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

use crate::tui::model::{Group, Mark, Outcome};

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
    /// True once the scan has stopped, whether it finished or was cancelled.
    finished: bool,
    /// Set when the scan stopped early because the browser asked it to.
    cancelled: bool,
    asked: Option<Ask>,
    answer: Option<Answer>,
    /// How many groups the browser has already taken. A cursor into `groups`,
    /// not a count of what is left: the published list is kept whole so that a
    /// `republish` can rebuild it from nothing.
    taken: usize,
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

/// The scan thread's end. Publishing is the only thing it can do.
pub struct Live(Arc<Shared>);

/// The browser thread's end.
pub struct Watch(Arc<Shared>);

/// Build both ends of one live scan.
pub fn channel() -> (Live, Watch) {
    let shared = Arc::new(Shared {
        body: Mutex::new(Body::default()),
        stirred: Condvar::new(),
        cancel: AtomicBool::new(false),
    });
    (Live(Arc::clone(&shared)), Watch(shared))
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
        let mut body = body(&self.0);
        body.groups.push(group);
        drop(body);
        self.0.stirred.notify_all();
    }

    /// Whether the browser has asked the scan to stop.
    ///
    /// Read in the per-file loop of every reading stage, so it is a relaxed
    /// atomic load and nothing more. `Acquire` rather than `Relaxed` because
    /// the scan must see everything the browser wrote before it set the flag.
    #[inline]
    pub fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Acquire)
    }

    /// Take whatever the browser has asked for, if anything.
    ///
    /// Called at the scan's safe points — between files in a reading stage and
    /// between stages — never inside a comparison sweep, so a request can
    /// never be served with a group's class half-decided.
    pub fn taken_ask(&self) -> Option<Ask> {
        // The fast path is a lock and a `None`, taken once per file. It is
        // only ever reached on the interactive path.
        let mut body = body(&self.0);
        body.asked.take()
    }

    /// Answer the request the browser is blocked on.
    pub fn answer(&self, answer: Answer) {
        let mut body = body(&self.0);
        body.answer = Some(answer);
        drop(body);
        self.0.stirred.notify_all();
    }

    /// Replace the published group list wholesale.
    ///
    /// Used once, when a round of deletion has changed what is left: the scan
    /// thread owns the records and the groups, so it is the side that rebuilds
    /// them, and the browser re-reads rather than trying to patch its own copy
    /// in step with someone else's.
    pub fn republish(&self, groups: Vec<Group>) {
        let mut body = body(&self.0);
        body.groups = groups;
        body.taken = 0;
        body.replaced = true;
        drop(body);
        self.0.stirred.notify_all();
    }

    /// The scan has stopped. Nothing is published after this.
    pub fn finished(&self) {
        let mut body = body(&self.0);
        body.finished = true;
        body.cancelled = self.0.cancel.load(Ordering::Acquire);
        drop(body);
        self.0.stirred.notify_all();
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
    /// member and REWRITE.md §6 counts per-file costs — so the published list
    /// holds `None` where the browser has already taken one. `taken` is the
    /// cursor; the slots stay, which is what keeps every index stable.
    pub fn fresh(&self) -> Fresh {
        let mut body = body(&self.0);
        let replaced = std::mem::take(&mut body.replaced);
        // The invariant, which `republish` is the only thing that breaks and
        // the only thing that repairs: after a look, the cursor sits at the
        // end of the list, so the next `found` lands beyond it.
        let from = body.taken.min(body.groups.len());
        let groups: Vec<Group> = body.groups.drain(from..).collect();
        body.taken = body.groups.len();
        Fresh {
            groups,
            finished: body.finished,
            cancelled: body.cancelled,
            replaced,
        }
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
            // A scan that has already stopped will never take the request, so
            // the caller is told rather than left waiting for a thread that is
            // not going to run again.
            if guard.finished && guard.asked.is_some() {
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

    /// Wait until something happens or the timeout runs out.
    ///
    /// The browser uses this only when it has nothing else to do; a keystroke
    /// wakes it through the terminal instead.
    pub fn settle(&self, timeout: std::time::Duration) {
        let guard = body(&self.0);
        if guard.finished {
            return;
        }
        let _ = self.0.stirred.wait_timeout(guard, timeout);
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
        let (live, watch) = channel();
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
        let (live, watch) = channel();
        assert!(!live.cancelled());
        watch.cancel();
        assert!(live.cancelled());
        live.finished();
        let fresh = watch.fresh();
        assert!(fresh.finished && fresh.cancelled);
    }

    #[test]
    fn a_scan_that_finished_without_cancelling_says_so() {
        let (live, watch) = channel();
        live.finished();
        let fresh = watch.fresh();
        assert!(fresh.finished);
        assert!(!fresh.cancelled, "an ordinary finish looked like a cancel");
    }

    #[test]
    fn an_apply_crosses_to_the_scan_and_the_answer_crosses_back() {
        let (live, watch) = channel();
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
    fn an_apply_after_the_scan_has_stopped_is_refused_rather_than_waited_on() {
        // The browser must never block on a thread that will not run again.
        let (live, watch) = channel();
        live.finished();
        match watch.apply(Vec::new(), false) {
            Answer::Refused(why) => assert!(why.contains("stopped"), "{why}"),
            Answer::Applied(_) => panic!("a stopped scan applied something"),
        }
    }

    #[test]
    fn republishing_replaces_the_list_and_the_browser_is_told_it_was_replaced() {
        let (live, watch) = channel();
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

    #[test]
    fn settling_returns_at_once_once_the_scan_has_finished() {
        // Otherwise a browser with nothing to draw would wait out the whole
        // timeout after every scan, on every frame.
        let (live, watch) = channel();
        live.finished();
        let began = std::time::Instant::now();
        watch.settle(std::time::Duration::from_secs(5));
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
    }
}
