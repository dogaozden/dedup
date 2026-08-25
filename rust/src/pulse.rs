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
    started: Instant,
    /// `Effort::bytes_read` when the stage began, so the rate is this stage's
    /// rate and not the whole scan's.
    bytes_at_start: u64,
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
}

impl View {
    /// What the scan is doing right now, or `None` before it has begun.
    pub fn read(&self) -> Option<Progress> {
        let stage = self.shared.stage();
        if stage.label.is_empty() {
            return None;
        }
        let done = self.shared.files.load(Ordering::Relaxed);
        let elapsed = stage.started.elapsed();
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
        Some(Progress {
            label: stage.label,
            done,
            total,
            bytes,
            rate: (bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64,
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
    pub fn live(effort: &Arc<Effort>) -> (Pulse, View) {
        let shared = Arc::new(Shared {
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            stage: Mutex::new(Stage {
                label: "",
                total: 0,
                started: Instant::now(),
                bytes_at_start: 0,
            }),
            stop: AtomicBool::new(false),
        });
        let view = View {
            shared: Arc::clone(&shared),
            effort: Arc::clone(effort),
        };
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
                started: Instant::now(),
                bytes_at_start: 0,
            }),
            stop: AtomicBool::new(false),
        });
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
                    started: Instant::now(),
                    bytes_at_start: live.effort.bytes_read(),
                };
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
    let stage = shared.stage();
    if stage.label.is_empty() {
        return;
    }
    let body = if stage.label == SCANNING {
        format!(
            "{} files · {}",
            human_count(shared.files.load(Ordering::Relaxed)),
            human_size(shared.bytes.load(Ordering::Relaxed))
        )
    } else {
        reading(
            shared.files.load(Ordering::Relaxed),
            stage.total,
            effort.bytes_read().saturating_sub(stage.bytes_at_start),
            stage.started.elapsed(),
        )
    };
    let painted = line(stage.label, &body, width);
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
fn reading(done: u64, total: u64, moved: u64, elapsed: Duration) -> String {
    let done = done.min(total);
    let percent = if total == 0 {
        0.0
    } else {
        done as f64 / total as f64 * 100.0
    };
    let filled = (BAR as f64 * percent / 100.0) as usize;
    let bar: String = "█".repeat(filled) + &"░".repeat(BAR - filled);
    let rate = moved as f64 / elapsed.as_secs_f64().max(0.001);
    format!(
        "{bar} {percent:3.0}%  {}/{} · {}/s",
        human_count(done),
        human_count(total),
        human_size(rate as u64)
    )
}

/// The bytes that go to the terminal: back to column one, wipe, repaint.
///
/// Cut a column short of the width so the line can never wrap. A wrapped line
/// leaves its first row behind at the next repaint, and a scan that scrolls its
/// own progress up the screen is the thing this is supposed to replace.
fn line(label: &str, body: &str, width: u16) -> String {
    let visible: String = format!("  {label}…  {body}")
        .chars()
        .take(usize::from(width).saturating_sub(1))
        .collect();
    format!("\r\x1b[2K{visible}")
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
        let (mut pulse, view) = Pulse::live(&effort);
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
            line(SCANNING, "1,000 files · 4.0 GiB", 80),
            "\r\x1b[2K  Scanning…  1,000 files · 4.0 GiB"
        );
        assert_eq!(
            reading(0, 100, 0, Duration::from_secs(1)),
            "░░░░░░░░░░░░░░░░░░░░░░░░   0%  0/100 · 0 B/s"
        );
        assert_eq!(
            reading(50, 100, 1024 * 1024, Duration::from_secs(1)),
            "████████████░░░░░░░░░░░░  50%  50/100 · 1.0 MiB/s"
        );
        assert_eq!(
            reading(100, 100, 10 * 1024 * 1024, Duration::from_secs(2)),
            "████████████████████████ 100%  100/100 · 5.0 MiB/s"
        );
    }

    #[test]
    fn the_bar_never_runs_past_its_own_end() {
        // A stage whose files are answered twice — or a total that was
        // guessed low — must not paint 130%.
        let over = reading(130, 100, 0, Duration::from_secs(1));
        assert!(over.starts_with(&"█".repeat(BAR)), "{over}");
        assert!(over.contains("100%  100/100"), "{over}");
        assert_eq!(over.chars().filter(|&c| c == '░').count(), 0);
        // ...and a stage handed nothing divides by nothing.
        let empty = reading(0, 0, 0, Duration::from_secs(1));
        assert!(empty.contains("  0%  0/0"), "{empty}");
    }

    #[test]
    fn the_line_is_always_one_line() {
        // Anything wider than the terminal would wrap, and the next repaint
        // would clear only the row it left the cursor on.
        for width in [1_u16, 2, 10, 40, 80, 200] {
            let painted = line(
                COMPARE,
                &reading(7, 9999, 1 << 30, Duration::from_secs(3)),
                width,
            );
            let visible = painted.trim_start_matches("\r\x1b[2K");
            assert!(
                visible.chars().count() < usize::from(width).max(1),
                "{width} columns held {} characters",
                visible.chars().count()
            );
        }
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
                    started: Instant::now(),
                    bytes_at_start: 0,
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
