//! The live progress line: one line on stderr, painted over itself while the
//! scan runs.
//!
//! Ported from the oracle's `Reporter` (dedup.py:2149) — its stage names, its
//! two body shapes, its `\r\033[2K` repaint. What is new is who does the
//! painting: a ticker thread reads the counters, so the scan path only ever
//! adds to a number and never formats, writes or waits.
//!
//! **Off is off.** Without a terminal on stderr, or with `--quiet`, there is no
//! thread, no line, and nothing on the scan path but a null check the branch
//! predictor gets right every time. RBENCH and the conformance harness both
//! pipe stderr, so both run the scan this file cannot touch.
//!
//! Every number shown is read out of a counter the scan keeps anyway: the
//! files from the stage's own tally, the bytes from `Effort`, which counts what
//! the syscalls really moved. Nothing here estimates.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::effort::Effort;
use crate::size::{human_count, human_size};
use crate::tui::model::Progress;

/// The longest the line may go without being repainted, and the shortest it
/// may go between two repaints.
const BEAT: Duration = Duration::from_millis(100);

/// Cells in the bar, as the oracle draws it.
const BAR: usize = 24;

/// Where the line falls back to when nobody will say how wide the terminal is.
const ASSUMED_WIDTH: u16 = 80;

/// The stage names, as the oracle names them.
pub const SCANNING: &str = "Scanning";
pub const SMALL: &str = "Reading small files";
pub const HEAD: &str = "Sampling";
pub const DEEP_HEAD: &str = "Sampling further";
pub const SAMPLE: &str = "Sampling deep";
pub const COMPARE: &str = "Comparing";

/// What the line is about right now.
struct Stage {
    label: &'static str,
    /// Files this stage was handed. Zero while nothing is running.
    total: u64,
    /// `Effort::bytes_read` when the stage began, so the bytes shown are this
    /// stage's bytes and not the whole scan's.
    ///
    /// There is deliberately no `started` beside it any more. The stage's age
    /// was the only ingredient the 1/t average needed, and with it gone a
    /// lifetime rate is not something this struct can be made to produce by
    /// accident — the window is the only clock the display has.
    bytes_at_start: u64,
    /// The last few seconds of this stage's byte counter, which is where the
    /// displayed rate comes from.
    rate: Rate,
}

/// A rate over the last `RATE_WINDOW`, and never over anything longer.
///
/// A ring of readings rather than an exponential average, and the reason is
/// what the bug was. A lifetime average of a stalled scan decays like 1/t: it
/// slides towards zero without ever getting there, and it reads as a scan
/// that is dying rather than one that is waiting. An EWMA has the same shape —
/// it approaches zero asymptotically and never arrives — so it would have
/// replaced a slow smooth decay with a fast smooth decay, and the maintainer
/// would have had to learn a new number's habits instead of reading it.
///
/// A window subtracts one reading from another. When nothing moves for the
/// length of the window, both readings are the same number and the rate is
/// exactly 0 — not nearly 0, and not still falling. And because the divisor is
/// the span of the window rather than the age of the stage, a scan-lifetime
/// average is not something this can accidentally produce: there is nowhere in
/// it to put the lifetime.
#[derive(Default)]
struct Rate {
    /// `(when, the byte counter then)`, oldest first. Bounded by the window
    /// and by `SPACING`, so a caller that polls quickly cannot grow it.
    readings: std::collections::VecDeque<(Instant, u64)>,
}

/// The closest together two kept readings may be. A caller polling faster than
/// this replaces the newest reading rather than adding one, so the window holds
/// at most `RATE_WINDOW / SPACING + 2` of them however often it is asked.
const SPACING: Duration = Duration::from_millis(100);

impl Rate {
    /// Take a reading of the running byte total, and answer the rate across
    /// the window that ends at it.
    fn observe(&mut self, now: Instant, total: u64) -> u64 {
        // A reading is only KEPT every `SPACING`; the rate is always computed
        // against `now` and `total` as they were passed in. That separation is
        // what lets the browser redraw as often as it likes without either
        // growing the window or getting a stale answer — an earlier draft
        // overwrote the newest kept reading instead, which on a caller polling
        // every millisecond left exactly one reading in hand and a rate of 0.
        match self.readings.back() {
            Some(&(last, _)) if now.duration_since(last) < SPACING => {}
            _ => self.readings.push_back((now, total)),
        }
        // Drop readings that have fallen out of the window, but keep the one
        // sitting on its far edge, so the span really is the window and not
        // however recently the caller happened to start asking.
        while self.readings.len() >= 2
            && now.duration_since(self.readings[1].0) >= crate::consts::RATE_WINDOW
        {
            self.readings.pop_front();
        }
        let (began, then) = self.readings[0];
        let span = now.duration_since(began).as_secs_f64();
        if span <= 0.0 {
            // One reading is not a rate. Zero rather than a guess: the first
            // frame of a stage has nothing to divide by yet.
            return 0;
        }
        (total.saturating_sub(then) as f64 / span) as u64
    }
}

struct Shared {
    /// Files finished in the stage now running — or seen, during the walk.
    files: AtomicU64,
    /// Bytes the walk has seen. The reading stages take their bytes from
    /// `Effort` instead, because sizes on disk are not bytes read.
    bytes: AtomicU64,
    stage: Mutex<Stage>,
    stop: AtomicBool,
}

impl Shared {
    /// The stage, through a lock that a panicking scan cannot take away. A
    /// painter that panicked would print its own backtrace over the line it
    /// was told to clear.
    fn stage(&self) -> MutexGuard<'_, Stage> {
        self.stage.lock().unwrap_or_else(|held| held.into_inner())
    }
}

struct Live {
    shared: Arc<Shared>,
    effort: Arc<Effort>,
    ticker: Option<JoinHandle<()>>,
    /// Whether anything may be written to stderr.
    ///
    /// Not the same question as "is there a ticker": `stage` paints one frame
    /// itself at every boundary, outside the ticker, so a stage that finishes
    /// inside a single beat is still seen. In live mode that frame would land
    /// on top of the browser's alternate screen — which is exactly the fight
    /// M5.2 ended by clearing the line before the browser opened. Caught by
    /// `m8_live_end_to_end.py`, which found the boundary frames on screen
    /// behind the browser.
    paints: bool,
}

/// The scan's handle on the progress line. `Pulse::off()` is a handle on
/// nothing, and every method on it is a branch that falls straight through.
pub struct Pulse(Option<Live>);

/// A read-only handle on the very counters the ticker paints from.
///
/// M8-LIVE.md: in live mode the browser IS the progress display, drawing
/// "from the same shared state the pulse reads". This is that state, and
/// reading it is a lock, five loads and an unlock — no thread, no syscall, and
/// above all no disk, because a progress bar that cost a read would be a
/// second reader on a platter M7 spent a milestone giving one queue.
pub struct View {
    shared: Arc<Shared>,
    effort: Arc<Effort>,
    cloud: Arc<crate::cloud::Cloud>,
}

impl View {
    /// How many cloud placeholders the scan has left alone. Read every frame,
    /// because the walk is still finding them while the browser is open.
    pub fn in_cloud(&self) -> u64 {
        self.cloud.skipped()
    }

    /// What the scan is doing right now, or `None` before it has begun.
    pub fn read(&self) -> Option<Progress> {
        let mut stage = self.shared.stage();
        if stage.label.is_empty() {
            return None;
        }
        let done = self.shared.files.load(Ordering::Relaxed);
        let (total, bytes) = if stage.label == SCANNING {
            (0, self.shared.bytes.load(Ordering::Relaxed))
        } else {
            (
                stage.total,
                self.effort
                    .bytes_read()
                    .saturating_sub(stage.bytes_at_start),
            )
        };
        // Reading the progress is what advances the window, so the browser
        // asking for a frame is the same act as taking a sample. There is no
        // ticker on this path to do it instead.
        let rate = stage.rate.observe(Instant::now(), bytes);
        Some(Progress {
            label: stage.label,
            done,
            total,
            bytes,
            rate,
            current: self.effort.now_reading(),
        })
    }
}

impl Pulse {
    pub fn off() -> Pulse {
        Pulse(None)
    }

    /// Counters kept, but no ticker and nothing written to stderr.
    ///
    /// The browser owns the terminal in live mode; a second painter putting a
    /// line on stderr would fight the alternate screen for it, which is
    /// exactly the mess M5.2 finished by clearing the line before the browser
    /// opened. So the scan keeps feeding the same numbers and the browser
    /// draws them itself.
    pub fn live(effort: &Arc<Effort>, cloud: &Arc<crate::cloud::Cloud>) -> (Pulse, View) {
        let shared = Arc::new(Shared {
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            stage: Mutex::new(Stage {
                label: "",
                total: 0,
                bytes_at_start: 0,
                rate: Rate::default(),
            }),
            stop: AtomicBool::new(false),
        });
        let view = View {
            shared: Arc::clone(&shared),
            effort: Arc::clone(effort),
            cloud: Arc::clone(cloud),
        };
        // Something is displaying now, so the file each open lands on is worth
        // recording. Nothing else in the program turns this on.
        effort.watch_reads();
        (
            Pulse(Some(Live {
                shared,
                effort: Arc::clone(effort),
                // No thread, and no boundary frames either: the browser is the
                // progress display, and nothing may reach stderr behind it.
                ticker: None,
                paints: false,
            })),
            view,
        )
    }

    /// On, if there is a terminal on stderr to paint to and nobody asked for
    /// quiet. Anything else is `off`, which is the state the harnesses and
    /// every piped run see.
    pub fn start(quiet: bool, effort: &Arc<Effort>) -> Pulse {
        if quiet || !std::io::stderr().is_terminal() {
            return Pulse::off();
        }
        let shared = Arc::new(Shared {
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            stage: Mutex::new(Stage {
                label: "",
                total: 0,
                bytes_at_start: 0,
                rate: Rate::default(),
            }),
            stop: AtomicBool::new(false),
        });
        effort.watch_reads();
        let (mine, theirs) = (Arc::clone(&shared), Arc::clone(effort));
        let ticker = std::thread::spawn(move || {
            let mut painted = Instant::now();
            loop {
                // Parked rather than slept so `finish` gets the line back at
                // once instead of at the end of a beat.
                std::thread::park_timeout(BEAT);
                if mine.stop.load(Ordering::Acquire) {
                    break;
                }
                if painted.elapsed() >= BEAT {
                    paint(&mine, &theirs);
                    painted = Instant::now();
                }
            }
        });
        Pulse(Some(Live {
            shared,
            effort: Arc::clone(effort),
            ticker: Some(ticker),
            paints: true,
        }))
    }

    /// A file the walk has looked at, and how big it turned out to be.
    #[inline]
    pub fn walked(&self, bytes: u64) {
        if let Some(live) = &self.0 {
            live.shared.files.fetch_add(1, Ordering::Relaxed);
            live.shared.bytes.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// A file the stage now running is finished with, read or remembered.
    #[inline]
    pub fn file(&self) {
        self.files(1);
    }

    #[inline]
    pub fn files(&self, count: u64) {
        if let Some(live) = &self.0 {
            live.shared.files.fetch_add(count, Ordering::Relaxed);
        }
    }

    /// Start a stage over `total` files. The counter goes back to zero under
    /// the same lock that publishes the new total, so no frame is ever drawn
    /// with one stage's progress against another's size.
    pub fn stage(&self, label: &'static str, total: u64) {
        if let Some(live) = &self.0 {
            {
                let mut stage = live.shared.stage();
                live.shared.files.store(0, Ordering::Relaxed);
                live.shared.bytes.store(0, Ordering::Relaxed);
                *stage = Stage {
                    label,
                    total,
                    bytes_at_start: live.effort.bytes_read(),
                    // A new window, because the counter behind it changes: the
                    // walk's rate is bytes SEEN and a reading stage's is bytes
                    // READ, and subtracting one from the other across a
                    // boundary would produce a number that is not a rate of
                    // anything.
                    rate: Rate::default(),
                };
                // ...and no file is being read yet. See `Effort::opened`.
                live.effort.forget_reading();
            }
            // One frame at the boundary, outside the lock the painter takes:
            // a stage that finishes inside a single beat — the parallel walk
            // does, on any tree the page cache holds — is still seen. Not in
            // live mode, where stderr is behind the browser's screen.
            if live.paints {
                paint(&live.shared, &live.effort);
            }
        }
    }

    /// Stop, and leave the line clear.
    ///
    /// The ticker is joined before the line is wiped, so nothing can paint
    /// after this returns. That is what lets the browser's alternate screen —
    /// and `--list`'s stdout — follow it without a race. Idempotent, because
    /// `Drop` calls it too: a scan that panics still gives the terminal back
    /// the way it found it.
    pub fn finish(&mut self) {
        let Some(live) = self.0.as_mut() else {
            return;
        };
        let Some(ticker) = live.ticker.take() else {
            return;
        };
        live.shared.stop.store(true, Ordering::Release);
        ticker.thread().unpark();
        let _ = ticker.join();
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(b"\r\x1b[2K");
        let _ = err.flush();
    }
}

impl Drop for Pulse {
    fn drop(&mut self) {
        self.finish();
    }
}

fn paint(shared: &Shared, effort: &Effort) {
    let width = width();
    // One lock for the stage and its counter: see `Pulse::stage`.
    let mut stage = shared.stage();
    if stage.label.is_empty() {
        return;
    }
    let now = Instant::now();
    let body = if stage.label == SCANNING {
        let seen = shared.bytes.load(Ordering::Relaxed);
        // The walk's window runs over bytes SEEN, which is what the walk moves:
        // it reads directories, not file contents.
        stage.rate.observe(now, seen);
        format!(
            "{} files · {}",
            human_count(shared.files.load(Ordering::Relaxed)),
            human_size(seen)
        )
    } else {
        let moved = effort.bytes_read().saturating_sub(stage.bytes_at_start);
        let rate = stage.rate.observe(now, moved);
        reading(shared.files.load(Ordering::Relaxed), stage.total, rate)
    };
    let painted = line(stage.label, &body, &effort.now_reading(), width);
    drop(stage);
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(painted.as_bytes());
    let _ = err.flush();
}

/// The body of a reading stage: bar, percent, files, rate.
///
/// `done` is clamped to `total` rather than allowed past it, so the bar cannot
/// claim more than the stage was given; nothing here is invented, and a stage
/// whose files were answered from the cache simply arrives at the end sooner.
///
/// `rate` arrives already measured, over `RATE_WINDOW`. This used to divide the
/// stage's bytes by the stage's age, which is the 1/t decay M9-CLOUD.md part B
/// exists to remove; there is now nothing left in here for a lifetime average
/// to be computed from.
fn reading(done: u64, total: u64, rate: u64) -> String {
    let done = done.min(total);
    let percent = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64 * 100.0
    };
    let filled = (BAR as f64 * percent / 100.0) as usize;
    let bar: String = "█".repeat(filled) + &"░".repeat(BAR - filled);
    format!(
        "{bar} {percent:3.0}%  {}/{} · {}/s",
        human_count(done),
        human_count(total),
        human_size(rate)
    )
}

/// The bytes that go to the terminal: back to column one, wipe, repaint.
///
/// Cut a column short of the width so the line can never wrap. A wrapped line
/// leaves its first row behind at the next repaint, and a scan that scrolls its
/// own progress up the screen is the thing this is supposed to replace.
///
/// `current` is the file being read, and it goes last because it is the part
/// that can be given up. A narrow terminal keeps the bar and the rate and
/// drops the name, rather than keeping the front of a path and losing the
/// numbers the line exists for.
fn line(label: &str, body: &str, current: &str, width: u16) -> String {
    let room = usize::from(width).saturating_sub(1);
    let mut visible: String = format!("  {label}…  {body}").chars().take(room).collect();
    let left = room.saturating_sub(visible.chars().count());
    if !current.is_empty() && left > SEPARATOR.chars().count() + LEAST_NAME {
        visible.push_str(SEPARATOR);
        visible.push_str(&middle_truncated(current, left - SEPARATOR.chars().count()));
    }
    format!("\r\x1b[2K{visible}")
}

/// Between the numbers and the name of the file they are about.
const SEPARATOR: &str = "  ← ";

/// Narrower than this, a path says nothing worth the columns it took: "…g.jpg"
/// is not a file anybody can recognise, and the rate it pushed off the end of
/// the line was.
const LEAST_NAME: usize = 12;

/// A path cut in the middle rather than at the end.
///
/// The end of a path is the part that identifies the file and the front is the
/// part that says where it lives, so cutting the tail off keeps the less
/// useful half. The tail therefore gets the larger share of the room. `…`
/// marks the cut, so nothing here can be read back as a real name.
pub fn middle_truncated(path: &str, room: usize) -> String {
    let characters: Vec<char> = path.chars().collect();
    if characters.len() <= room {
        return path.to_string();
    }
    if room <= 1 {
        return "…".repeat(room);
    }
    let left = (room - 1) / 3;
    let right = room - 1 - left;
    let mut cut = String::with_capacity(room);
    cut.extend(&characters[..left]);
    cut.push('…');
    cut.extend(&characters[characters.len() - right..]);
    cut
}

fn width() -> u16 {
    ratatui::crossterm::terminal::size()
        .map(|(columns, _)| columns)
        .unwrap_or(ASSUMED_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_pulse_keeps_the_counters_and_writes_nothing() {
        // The browser owns the terminal in live mode. `stage` paints a frame
        // at every boundary on its own, outside the ticker, so "no ticker" is
        // not enough on its own to keep stderr clear — that frame landed on
        // top of the browser's alternate screen until `paints` existed.
        let effort = Arc::new(Effort::default());
        let cloud = Arc::new(crate::cloud::Cloud::default());
        let (mut pulse, view) = Pulse::live(&effort, &cloud);
        let live = pulse.0.as_ref().expect("live");
        assert!(!live.paints, "a live pulse would write to stderr");
        assert!(live.ticker.is_none(), "a live pulse started a painter");

        // ...and it still counts, which is the whole point of it.
        assert!(
            view.read().is_none(),
            "a pulse that has not begun reported a stage"
        );
        pulse.stage(SCANNING, 0);
        pulse.walked(4096);
        pulse.walked(4096);
        let seen = view.read().expect("a stage");
        assert_eq!(seen.label, SCANNING);
        assert_eq!(seen.done, 2);
        assert_eq!(seen.bytes, 8192);
        assert_eq!(seen.total, 0, "the walk has no total to be a fraction of");

        pulse.stage(COMPARE, 10);
        pulse.files(3);
        let seen = view.read().expect("a stage");
        assert_eq!((seen.label, seen.done, seen.total), (COMPARE, 3, 10));
        // The count the browser draws comes through this same handle, and
        // keeps answering while the walk is still finding them.
        assert_eq!(view.in_cloud(), 0);
        cloud.skip();
        cloud.skip();
        assert_eq!(view.in_cloud(), 2);
        // Finishing a live pulse joins nothing and clears nothing.
        pulse.finish();
    }

    #[test]
    fn off_is_off() {
        // Nothing to join, nothing to clear, and every counter call a branch
        // that falls through. The whole inertness claim rests on this shape.
        let mut pulse = Pulse::off();
        pulse.stage(COMPARE, 100);
        pulse.walked(4096);
        pulse.file();
        pulse.files(10);
        pulse.finish();
        pulse.finish();
        assert!(pulse.0.is_none());
    }

    #[test]
    fn the_walk_says_what_it_has_seen_and_the_rest_draw_a_bar() {
        assert_eq!(
            line(SCANNING, "1,000 files · 4.0 GiB", "", 80),
            "\r\x1b[2K  Scanning…  1,000 files · 4.0 GiB"
        );
        assert_eq!(
            reading(0, 100, 0),
            "░░░░░░░░░░░░░░░░░░░░░░░░   0%  0/100 · 0 B/s"
        );
        assert_eq!(
            reading(50, 100, 1024 * 1024),
            "████████████░░░░░░░░░░░░  50%  50/100 · 1.0 MiB/s"
        );
        assert_eq!(
            reading(100, 100, 5 * 1024 * 1024),
            "████████████████████████ 100%  100/100 · 5.0 MiB/s"
        );
    }

    #[test]
    fn the_bar_never_runs_past_its_own_end() {
        // A stage whose files are answered twice — or a total that was
        // guessed low — must not paint 130%.
        let over = reading(130, 100, 0);
        assert!(over.starts_with(&"█".repeat(BAR)), "{over}");
        assert!(over.contains("100%  100/100"), "{over}");
        assert_eq!(over.chars().filter(|&c| c == '░').count(), 0);
        // ...and a stage handed nothing divides by nothing.
        let empty = reading(0, 0, 0);
        assert!(empty.contains("  0%  0/0"), "{empty}");
    }

    #[test]
    fn the_line_is_always_one_line() {
        // Anything wider than the terminal would wrap, and the next repaint
        // would clear only the row it left the cursor on. Now with a long
        // path competing for the same row, which is the new way to overrun it.
        let deep = "/Users/somebody/Library/CloudStorage/GoogleDrive-somebody@\
                    example.com/My Drive/holidays/2019/day four/IMG_20190714_183012.MOV";
        for width in [1_u16, 2, 10, 40, 80, 200] {
            for current in ["", deep, "a.bin"] {
                let painted = line(COMPARE, &reading(7, 9999, 1 << 30), current, width);
                let visible = painted.trim_start_matches("\r\x1b[2K");
                assert!(
                    visible.chars().count() < usize::from(width).max(1),
                    "{width} columns held {} characters for {current:?}",
                    visible.chars().count()
                );
            }
        }
    }

    #[test]
    fn a_stall_reads_zero_within_one_window_instead_of_decaying_like_one_over_t() {
        // The bug this milestone exists for. A scan reads 30 MiB and then
        // blocks on one file that is being fetched from a network. The old
        // display divided 30 MiB by the stage's whole age, so it slid down
        // through 3 MiB/s, 300 KiB/s, 8 KiB/s — never reaching zero, and
        // reading as a scan that was dying rather than one that was waiting.
        let mut rate = Rate::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);

        // Ten seconds of real reading at 3 MiB per second.
        let mut moved = 0_u64;
        let mut last = 0;
        for tick in 0..100 {
            moved += 3 * 1024 * 1024 / 10;
            last = rate.observe(at(tick * 100), moved);
        }
        let near = |value: u64, wanted: u64| {
            value.abs_diff(wanted) * 20 < wanted * 3 // within 15%
        };
        assert!(
            near(last, 3 * 1024 * 1024),
            "reading steadily should read about 3 MiB/s, got {last}"
        );

        // ...and then nothing moves at all. `moved` never changes again.
        let stalled_for = |ms: u64| {
            let mut copy = Rate::default();
            let mut total = 0_u64;
            for tick in 0..100 {
                total += 3 * 1024 * 1024 / 10;
                copy.observe(at(tick * 100), total);
            }
            let mut answer = 0;
            for tick in 100..(100 + ms / 100) {
                answer = copy.observe(at(tick * 100), total);
            }
            answer
        };
        // One window later there is no reading left inside it that moved, so
        // the subtraction is zero — exactly zero, not approaching it.
        let window_ms = crate::consts::RATE_WINDOW.as_millis() as u64;
        assert_eq!(
            stalled_for(window_ms + 200),
            0,
            "a stall must read 0 one window after the last byte"
        );
        assert_eq!(stalled_for(window_ms * 3), 0, "and stay there");
        // Before the window is out it is already collapsing, which is the
        // half-truth an average could not tell either: at half a window the
        // window still holds half its bytes.
        let half = stalled_for(window_ms / 2);
        assert!(
            half < 3 * 1024 * 1024,
            "the rate should be falling inside the window, got {half}"
        );

        // The 1/t signature, spelled out: a lifetime average over the same
        // history is still well above zero long after the window has said 0.
        // 30 MiB over 13 seconds is 2.3 MiB/s — the number the old display
        // would have shown while nothing at all was happening.
        let lifetime = 30 * 1024 * 1024 / 13;
        assert!(
            lifetime > 2 * 1024 * 1024,
            "the average this replaces would still be reading {lifetime}"
        );
    }

    #[test]
    fn the_window_never_looks_further_back_than_the_window() {
        // The structural half of the same claim: however long a stage runs,
        // the span the rate is divided by is the window and not the stage.
        // A scan that reads 1 MiB in its first second and then 10 MiB/s
        // forever must read 10 MiB/s, not the average of the two.
        let mut rate = Rate::default();
        let start = Instant::now();
        let mut moved = 1024 * 1024;
        rate.observe(start, moved);
        let mut last = 0;
        for tick in 1..600 {
            moved += 1024 * 1024;
            last = rate.observe(start + Duration::from_millis(tick * 100), moved);
        }
        assert!(
            last.abs_diff(10 * 1024 * 1024) * 20 < 10 * 1024 * 1024 * 3,
            "a minute in, the window should still read about 10 MiB/s, got {last}"
        );
        // ...and it holds a bounded number of readings while doing it.
        assert!(
            rate.readings.len()
                <= (crate::consts::RATE_WINDOW.as_millis() as usize / SPACING.as_millis() as usize)
                    + 2,
            "the window grew to {} readings",
            rate.readings.len()
        );
    }

    #[test]
    fn polling_faster_than_the_spacing_does_not_grow_the_window() {
        // The browser redraws as often as it likes; the window must not become
        // a list of every frame it ever drew.
        let mut rate = Rate::default();
        let start = Instant::now();
        for tick in 0..10_000_u64 {
            rate.observe(start + Duration::from_millis(tick), tick * 1024);
        }
        assert!(
            rate.readings.len() <= 34,
            "{} readings",
            rate.readings.len()
        );
        // ...and it is still a correct rate: 1 KiB per millisecond.
        let answer = rate.observe(start + Duration::from_millis(10_000), 10_000 * 1024);
        assert!(
            answer.abs_diff(1024 * 1000) * 20 < 1024 * 1000 * 3,
            "got {answer}"
        );
    }

    #[test]
    fn one_reading_is_not_a_rate() {
        // The first frame of a stage has nothing to subtract from. Zero, not a
        // number invented out of the only reading there is.
        let mut rate = Rate::default();
        assert_eq!(rate.observe(Instant::now(), 5 * 1024 * 1024), 0);
    }

    #[test]
    fn the_line_names_the_file_and_cuts_it_in_the_middle() {
        let deep = "/Users/somebody/Library/CloudStorage/GoogleDrive-x@y.com/\
                    My Drive/holidays/IMG_20190714_183012.MOV";
        let painted = line(COMPARE, &reading(7, 99, 8 * 1024), deep, 120);
        let visible = painted.trim_start_matches("\r\x1b[2K");
        assert!(visible.contains('←'), "{visible:?}");
        // The end of the path is the half that identifies the file, so it is
        // the half that survives.
        assert!(visible.contains("IMG_20190714_183012.MOV"), "{visible:?}");
        assert!(visible.contains('…'), "{visible:?}");
        // The numbers it shares the row with are still there.
        assert!(visible.contains("8.0 KiB/s"), "{visible:?}");

        // A row with room for the numbers but not for a useful name keeps the
        // numbers and drops the name. 70 columns: the body needs 60 of them,
        // which leaves too few for a path worth reading. (Narrower than about
        // 61 and the bar alone crowds the rate off the end, which is what this
        // line has always done and not something M9 changed.)
        let cramped = line(COMPARE, &reading(7, 99, 8 * 1024), deep, 70);
        assert!(cramped.contains("8.0 KiB/s"), "{:?}", cramped);
        assert!(!cramped.contains('←'), "{:?}", cramped);
        // ...and a walk, which opens no files, says nothing about one.
        assert_eq!(
            line(SCANNING, "3 files · 1.0 KiB", "", 80),
            "\r\x1b[2K  Scanning…  3 files · 1.0 KiB"
        );
    }

    #[test]
    fn a_middle_cut_keeps_the_name_and_marks_the_cut() {
        assert_eq!(middle_truncated("short.bin", 40), "short.bin");
        assert_eq!(middle_truncated("short.bin", 9), "short.bin");
        let cut = middle_truncated("/a/very/long/path/to/a/file/IMG_0001.MOV", 20);
        assert_eq!(cut.chars().count(), 20);
        assert!(cut.ends_with("IMG_0001.MOV"), "{cut}");
        assert!(cut.contains('…'), "{cut}");
        // Degenerate rooms do not panic and do not overrun.
        for room in 0..12 {
            let cut = middle_truncated("/a/very/long/path/IMG_0001.MOV", room);
            assert_eq!(cut.chars().count(), room, "room {room} gave {cut:?}");
        }
        // A multi-byte name is cut by characters, not by bytes: cutting a path
        // of emoji at a byte boundary would put a replacement character on the
        // maintainer's screen.
        let emoji = "/holidays/🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂/photo.jpg";
        let cut = middle_truncated(emoji, 15);
        assert_eq!(cut.chars().count(), 15, "{cut}");
        assert!(cut.ends_with("photo.jpg"), "{cut}");
    }

    #[test]
    fn a_stage_starts_its_count_and_its_clock_over() {
        let effort = Arc::new(Effort::default());
        let mut pulse = Pulse(Some(Live {
            shared: Arc::new(Shared {
                files: AtomicU64::new(0),
                bytes: AtomicU64::new(0),
                stage: Mutex::new(Stage {
                    label: "",
                    total: 0,
                    bytes_at_start: 0,
                    rate: Rate::default(),
                }),
                stop: AtomicBool::new(false),
            }),
            effort: Arc::clone(&effort),
            // No thread: this is the bookkeeping, not the painting.
            ticker: None,
            paints: false,
        }));
        pulse.stage(SCANNING, 0);
        pulse.walked(1024);
        pulse.walked(2048);
        let live = pulse.0.as_ref().expect("live");
        assert_eq!(live.shared.files.load(Ordering::Relaxed), 2);
        assert_eq!(live.shared.bytes.load(Ordering::Relaxed), 3072);
        pulse.stage(COMPARE, 5);
        let live = pulse.0.as_ref().expect("live");
        assert_eq!(live.shared.files.load(Ordering::Relaxed), 0);
        assert_eq!(live.shared.bytes.load(Ordering::Relaxed), 0);
        assert_eq!(live.shared.stage().total, 5);
        pulse.files(3);
        pulse.file();
        assert_eq!(
            pulse
                .0
                .as_ref()
                .expect("live")
                .shared
                .files
                .load(Ordering::Relaxed),
            4
        );
        // ...and finishing a pulse that never had a ticker is not a hang.
        pulse.finish();
    }
}
