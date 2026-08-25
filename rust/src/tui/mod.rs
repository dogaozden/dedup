//! The interactive browser: the original product surface, in ratatui.
//!
//! This module owns the terminal and the event loop and nothing else. What a
//! keystroke means is `model`'s; what the screen looks like is `view`'s; what
//! removal does is `trash`'s, unchanged, reached through the same token
//! `--auto` uses. Splitting it that way is what makes a program that deletes
//! files testable without a terminal in the test.

pub mod menu;
pub mod model;
pub mod view;

use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::cache::ScanCache;
use crate::compare::Compare;
use crate::pipeline::{DupGroup, ProvenGroups, Scan};
use crate::sys;
use crate::trash::{oracle_mtime, report_refusals, run_selected, Removed};
use crate::walk::FileRec;

/// How long to wait for a keystroke before going round the loop again. Long
/// enough that an idle browser is doing nothing measurable, short enough that a
/// window resized while nobody is typing redraws while they are still looking.
const WAIT_FOR_KEY: std::time::Duration = std::time::Duration::from_millis(250);

use crate::size::{human_count, human_size};
use model::{Browser, Group, Key, Mark, Member, Mode, Outcome, Step};

/// Whether a browser can run at all. Non-interactive use is `--list`/`--auto`,
/// and a program that deletes files should not be guessing at a pipe.
pub fn has_terminal() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

pub const NO_TERMINAL: &str = "dedup-rs: the interactive browser needs a terminal on stdin and \
                               stdout. Use --list to see the groups or --auto to clean up.";

/// Put the terminal back however the scope holding it is left — returned,
/// errored, or panicked through. §M5.4: a user's wrecked terminal is a failed
/// milestone.
///
/// The action is a closure rather than a call to `put_the_terminal_back`
/// directly so a test can watch the guard fire without a terminal to wreck.
struct Restore<F: FnMut()>(F);

impl<F: FnMut()> Drop for Restore<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

fn put_the_terminal_back() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = io::stdout().flush();
}

/// The browser, from the first keystroke to the summary printed after it.
///
/// `proven` is the same token `run_auto` takes and is here for the same
/// reason: it can only come from `Scan::proven`, so a browser over a scan that
/// merely grouped by size cannot be opened, let alone delete from one.
pub fn run(
    scan: &Scan,
    proven: ProvenGroups<'_>,
    root: &Path,
    hard: bool,
    context: &Compare,
    cache: Option<&mut ScanCache>,
) -> i32 {
    let groups = proven.groups();
    let mut browser = build(scan, groups, root, hard);
    let mut refused: Vec<DupGroup> = Vec::new();

    // The hook goes on before the screen is switched, so a panic between the
    // two still lands on a terminal that works. It runs before unwinding
    // starts, which is what puts the panic message on the normal screen rather
    // than on an alternate one nobody will see again. The previous hook is
    // moved into this one and so cannot be put back; the program exits within
    // a few lines of here either way.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        put_the_terminal_back();
        previous(info);
    }));

    // No explicit restore here: `draw_and_read` holds a drop guard that covers
    // every way out of it, and a second mechanism beside it only makes a reader
    // wonder which one is load-bearing.
    match draw_and_read(&mut browser, scan, context, cache, &mut refused) {
        Err(error) => {
            eprintln!("dedup-rs: {error}");
            1
        }
        // Nothing is printed here on purpose: the summary's destination is the
        // thing that just went away, and `println!` on a dead terminal panics.
        Ok(Ended::TerminalGone) => 1,
        Ok(Ended::Quit) => {
            summarise(&browser, &scan.records, &refused);
            0
        }
    }
}

/// How the loop ended. The two are told apart only so the caller knows whether
/// there is still anybody to print the summary to.
enum Ended {
    Quit,
    TerminalGone,
}

/// Whether the terminal has gone away, asked without waiting for anything.
///
/// Shared with the pre-flight menu, which asks the same question one screen
/// earlier and must answer it the same way.
pub(super) fn hung_up() -> bool {
    sys::wait_for_terminal_input(std::time::Duration::ZERO) == sys::Input::HungUp
}

/// The loop itself, with the terminal held by a guard so every way out of it
/// puts the screen back.
fn draw_and_read(
    browser: &mut Browser,
    scan: &Scan,
    context: &Compare,
    mut cache: Option<&mut ScanCache>,
    refused: &mut Vec<DupGroup>,
) -> io::Result<Ended> {
    enable_raw_mode()?;
    let _restore = Restore(put_the_terminal_back);
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let ended = loop {
        // The frame that was actually painted decides what the browser will
        // act on next: below the layout minimum the paint layer draws a resize
        // message instead of the browser, and the model refuses to act on a
        // screen that was never drawn.
        let painted = match terminal.draw(|frame| view::draw(frame, browser)) {
            Ok(frame) => frame.area,
            // A paint that fails on a terminal which has since hung up is that
            // terminal dying, not a fault. Whether the write or the wait below
            // notices first is a matter of timing, and both mean the same
            // thing; a genuine error on a terminal that is still there is
            // still an error.
            Err(error) => {
                if hung_up() {
                    break Ended::TerminalGone;
                }
                return Err(error);
            }
        };
        browser.resize(painted.width, painted.height);
        // Ask the descriptor before crossterm does. A terminal that has gone
        // away is readable forever and empty forever, and crossterm answers
        // that by looping inside `read` — no error comes back, so `?` below
        // never gets the chance to fire. Leaving here is the difference
        // between an orphaned browser exiting and one spinning on a core.
        match sys::wait_for_terminal_input(WAIT_FOR_KEY) {
            sys::Input::Ready => {}
            sys::Input::HungUp => break Ended::TerminalGone,
            // Nothing typed. Round the loop to repaint, which is also how a
            // window that was resized while idle gets its new size.
            sys::Input::Waited => continue,
        }
        let event = event::read()?;
        let Event::Key(key) = event else { continue };
        // Windows sends a release for every press; acting on both would double
        // every keystroke.
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let step = browser.on_key(key_for(browser.mode, key));
        let step = match step {
            Step::Apply => {
                terminal.draw(|frame| view::draw(frame, browser))?;
                let marks = browser.selection();
                debug_assert!(browser.legible(), "confirmed on a screen nobody could read");
                let removed = remove(scan, &marks, browser.hard, context, cache.as_deref_mut());
                match removed {
                    Ok(mut removed) => {
                        let summary = Outcome {
                            files: removed.files,
                            bytes: removed.bytes,
                            removed: std::mem::take(&mut removed.records),
                            refused: removed.refused.len(),
                            failures: std::mem::take(&mut removed.failures),
                        };
                        refused.append(&mut removed.refused);
                        browser.absorb(summary)
                    }
                    Err(error) => {
                        // A refusal is information, not a crash: say why on the
                        // status line and let the user fix the selection.
                        browser.flash = error;
                        Step::Stay
                    }
                }
            }
            other => other,
        };
        if step == Step::Quit {
            break Ended::Quit;
        }
    };

    if matches!(ended, Ended::TerminalGone) {
        // `Terminal`'s own `Drop` tries to show the cursor and, when it cannot,
        // `eprintln!`s about it — which panics when the descriptor it would
        // print to is the terminal that just died. There is no cursor to put
        // back on a terminal that is gone, so that work is skipped rather than
        // attempted and mourned. The guard above still runs, and still restores
        // a terminal that is really there.
        std::mem::forget(terminal);
    }
    Ok(ended)
}

/// Ask `trash` to remove what is marked.
///
/// The token is taken from the scan again rather than kept across the loop:
/// `Scan::proven` is the only thing that can mint one, so asking it every time
/// is asking the same question that let the browser open. A scan cannot stop
/// being proven, but if one ever did, this refuses instead of assuming.
fn remove(
    scan: &Scan,
    marks: &[Mark],
    hard: bool,
    context: &Compare,
    cache: Option<&mut ScanCache>,
) -> Result<Removed, String> {
    let proven = scan
        .proven()
        .ok_or("these groups were never compared byte for byte; nothing will be removed")?;
    run_selected(scan, proven, marks, hard, context, cache)
}

/// Which key was pressed, in the browser's vocabulary.
///
/// The mapping needs the mode because the oracle's does: `n` is "next group"
/// while browsing and "no" in the confirmation, and each of its overlays reads
/// its own keys rather than sharing the browse map.
fn key_for(mode: Mode, key: KeyEvent) -> Key {
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Key::Quit;
    }
    match mode {
        Mode::Confirm => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Key::Yes,
            KeyCode::Char('n') | KeyCode::Char('N') => Key::No,
            KeyCode::Esc => Key::Escape,
            _ => Key::Ignored,
        },
        Mode::Bulk => match key.code {
            KeyCode::Char(digit @ '0'..='3') => Key::Digit(digit as u8 - b'0'),
            KeyCode::Esc => Key::Escape,
            KeyCode::Char('q') | KeyCode::Char('Q') => Key::Quit,
            _ => Key::Ignored,
        },
        Mode::Help => match key.code {
            KeyCode::Char('?') => Key::Help,
            KeyCode::Esc => Key::Escape,
            KeyCode::Char('q') | KeyCode::Char('Q') => Key::Quit,
            _ => Key::Ignored,
        },
        Mode::Browse => match key.code {
            KeyCode::Up | KeyCode::Char('k') => Key::Up,
            KeyCode::Down | KeyCode::Char('j') => Key::Down,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') => Key::NextGroup,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') => Key::PreviousGroup,
            KeyCode::Char('g') => Key::FirstGroup,
            KeyCode::Char('G') => Key::LastGroup,
            KeyCode::Char(' ') => Key::Toggle,
            KeyCode::Char('a') => Key::KeepOnlyThis,
            KeyCode::Char('u') => Key::ClearGroup,
            KeyCode::Char('b') => Key::Bulk,
            KeyCode::Enter => Key::Apply,
            KeyCode::Char('?') => Key::Help,
            KeyCode::Char('q') | KeyCode::Char('Q') => Key::Quit,
            KeyCode::Esc => Key::Escape,
            _ => Key::Ignored,
        },
    }
}

/// What one group looks like on screen.
///
/// Split out of `build` because the live path needs exactly this and nothing
/// else: the scan thread owns the records, so it is the side that turns a
/// settled group into rows, and it does so one group at a time as they settle.
///
/// `id` is the group's stable identity — its position in the scan's own list
/// for a finished scan, its arrival number for a live one. `proven` says
/// whether the bytes behind it have really been compared.
pub struct Shown {
    pub offset: i64,
    pub cwd: Option<std::path::PathBuf>,
}

impl Shown {
    pub fn new() -> Shown {
        Shown {
            offset: local_offset(),
            cwd: std::env::current_dir().ok(),
        }
    }

    pub fn group(
        &self,
        records: &[FileRec],
        members: &[usize],
        root: &Path,
        id: usize,
        proven: bool,
    ) -> Group {
        Group {
            index: id,
            size: records[members[0]].size,
            proven,
            members: members
                .iter()
                .map(|&index| {
                    let record = &records[index];
                    Member {
                        record: index,
                        name: record.path().to_string(),
                        path: display_path(root, record, self.cwd.as_deref()),
                        date: date_of(record.mtime_ns, self.offset),
                        extra_links: record.names.len() - 1,
                        deletable: record.deletable(),
                        mtime: oracle_mtime(record),
                    }
                })
                .collect(),
        }
    }
}

/// Turn the scan into what the screen needs, once, so the loop never touches a
/// `FileRec` again.
pub fn build(scan: &Scan, groups: &[DupGroup], root: &Path, hard: bool) -> Browser {
    let shown = Shown::new();
    let groups = groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            // A finished scan's groups have all been compared — that is what
            // `Scan::proven` means — except the ones the cache attested,
            // which `run_selected` re-reads before it removes anything.
            shown.group(&scan.records, &group.members, root, index, !group.attested)
        })
        .collect();
    Browser::new(groups, root.to_string_lossy().into_owned(), hard)
}

/// The path as the oracle shows it: relative to where the user is standing when
/// that is shorter and still points at the file, absolute otherwise.
fn display_path(root: &Path, record: &FileRec, cwd: Option<&Path>) -> String {
    let absolute = root.join(record.path());
    match cwd.and_then(|cwd| absolute.strip_prefix(cwd).ok()) {
        Some(relative) => relative.to_string_lossy().into_owned(),
        None => absolute.to_string_lossy().into_owned(),
    }
}

/// Seconds this machine is ahead of UTC.
///
/// `localtime_r` would be one call, but REWRITE.md §7 keeps `unsafe` inside the
/// bulk-walk FFI module and this is not that module. `date` is a subprocess on
/// the same footing as the `id -u` in `trash`, and it runs once for the whole
/// session rather than once per file. UTC is the fallback, which shifts a
/// displayed date by at most a day and never affects what is deleted.
fn local_offset() -> i64 {
    let output = Command::new("date").arg("+%z").output().ok();
    let text = output
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    parse_offset(&text).unwrap_or(0)
}

/// `+hhmm` or `-hhmm` as a number of seconds.
fn parse_offset(text: &str) -> Option<i64> {
    let sign = match text.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits = &text[1..];
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// The oracle's `%Y-%m-%d` in local time.
fn date_of(mtime_ns: i64, offset: i64) -> String {
    let seconds = mtime_ns.div_euclid(1_000_000_000) + offset;
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the epoch to a calendar date, by Howard Hinnant's `civil_from_days`.
///
/// The arithmetic counts from March so that the leap day lands at the end of a
/// year and no month-length table is needed.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (year + i64::from(month <= 2), month, day)
}

/// What the session did, on the terminal the user gets back.
fn summarise(browser: &Browser, records: &[FileRec], refused: &[DupGroup]) {
    if browser.applied_files > 0 {
        let where_to = if browser.hard {
            "deleted"
        } else {
            "moved to Trash"
        };
        println!(
            "\n  {} files {where_to} · {} reclaimed\n",
            human_count(browser.applied_files as u64),
            human_size(browser.applied_bytes)
        );
    }
    report_refusals(records, refused);
    if !browser.failures.is_empty() {
        eprintln!(
            "  {} could not be removed:",
            human_count(browser.failures.len() as u64)
        );
        for line in browser.failures.iter().take(10) {
            eprintln!("    {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_browse_map_is_the_oracles_keymap() {
        let cases = [
            (KeyCode::Up, Key::Up),
            (KeyCode::Char('k'), Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Char('j'), Key::Down),
            (KeyCode::Right, Key::NextGroup),
            (KeyCode::Char('l'), Key::NextGroup),
            (KeyCode::Char('n'), Key::NextGroup),
            (KeyCode::Left, Key::PreviousGroup),
            (KeyCode::Char('h'), Key::PreviousGroup),
            (KeyCode::Char('p'), Key::PreviousGroup),
            (KeyCode::Char('g'), Key::FirstGroup),
            (KeyCode::Char('G'), Key::LastGroup),
            (KeyCode::Char(' '), Key::Toggle),
            (KeyCode::Char('a'), Key::KeepOnlyThis),
            (KeyCode::Char('u'), Key::ClearGroup),
            (KeyCode::Char('b'), Key::Bulk),
            (KeyCode::Enter, Key::Apply),
            (KeyCode::Char('?'), Key::Help),
            (KeyCode::Char('q'), Key::Quit),
        ];
        for (code, expected) in cases {
            assert_eq!(key_for(Mode::Browse, press(code)), expected, "{code:?}");
        }
    }

    #[test]
    fn n_is_next_group_while_browsing_and_no_in_the_confirmation() {
        assert_eq!(
            key_for(Mode::Browse, press(KeyCode::Char('n'))),
            Key::NextGroup
        );
        assert_eq!(key_for(Mode::Confirm, press(KeyCode::Char('n'))), Key::No);
        assert_eq!(key_for(Mode::Confirm, press(KeyCode::Char('y'))), Key::Yes);
    }

    #[test]
    fn nothing_in_the_confirmation_but_yes_no_and_escape_means_anything() {
        for code in [
            KeyCode::Char(' '),
            KeyCode::Enter,
            KeyCode::Char('a'),
            KeyCode::Char('q'),
            KeyCode::Down,
        ] {
            assert_eq!(
                key_for(Mode::Confirm, press(code)),
                Key::Ignored,
                "{code:?}"
            );
        }
        assert_eq!(key_for(Mode::Confirm, press(KeyCode::Esc)), Key::Escape);
    }

    #[test]
    fn the_bulk_menu_reads_digits_and_the_help_screen_does_not() {
        assert_eq!(
            key_for(Mode::Bulk, press(KeyCode::Char('0'))),
            Key::Digit(0)
        );
        assert_eq!(
            key_for(Mode::Bulk, press(KeyCode::Char('3'))),
            Key::Digit(3)
        );
        assert_eq!(key_for(Mode::Bulk, press(KeyCode::Char('4'))), Key::Ignored);
        assert_eq!(key_for(Mode::Help, press(KeyCode::Char('1'))), Key::Ignored);
        assert_eq!(key_for(Mode::Help, press(KeyCode::Char('?'))), Key::Help);
    }

    #[test]
    fn ctrl_c_leaves_from_wherever_it_is_pressed() {
        let interrupt = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        for mode in [Mode::Browse, Mode::Help, Mode::Bulk, Mode::Confirm] {
            assert_eq!(key_for(mode, interrupt), Key::Quit, "{mode:?}");
        }
        // ...and a plain c does not.
        assert_eq!(
            key_for(Mode::Browse, press(KeyCode::Char('c'))),
            Key::Ignored
        );
    }

    #[test]
    fn the_guard_puts_the_terminal_back_on_every_way_out_of_its_scope() {
        use std::cell::Cell;
        use std::rc::Rc;

        // Returning normally.
        let fired = Rc::new(Cell::new(0));
        {
            let seen = Rc::clone(&fired);
            let _guard = Restore(move || seen.set(seen.get() + 1));
        }
        assert_eq!(fired.get(), 1, "the guard did not fire on a normal exit");

        // Unwinding through it, which is the case the panic hook cannot cover:
        // the hook runs before the stack comes down, the guard as it does.
        let fired = Rc::new(Cell::new(0));
        let seen = Rc::clone(&fired);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = Restore(move || seen.set(seen.get() + 1));
            panic!("the browser fell over");
        }));
        assert!(outcome.is_err(), "the panic did not happen");
        assert_eq!(fired.get(), 1, "the guard did not fire on a panic");
    }

    #[test]
    fn dates_come_out_the_way_strftime_writes_them() {
        // 2026-08-23T00:00:00Z, and the day before it in a zone behind UTC.
        let midnight = 1_787_443_200_i64 * 1_000_000_000;
        assert_eq!(date_of(midnight, 0), "2026-08-23");
        assert_eq!(date_of(midnight, -3600), "2026-08-22");
        assert_eq!(date_of(midnight, 3600), "2026-08-23");
        // The epoch, a leap day, and a century that is not a leap year.
        assert_eq!(date_of(0, 0), "1970-01-01");
        assert_eq!(date_of(1_709_164_800_i64 * 1_000_000_000, 0), "2024-02-29");
        assert_eq!(date_of(951_782_400_i64 * 1_000_000_000, 0), "2000-02-29");
        assert_eq!(date_of(1_709_251_200_i64 * 1_000_000_000, 0), "2024-03-01");
        // A time before the epoch still floors to the right day.
        assert_eq!(date_of(-1_000_000_000, 0), "1969-12-31");
    }

    #[test]
    fn a_zone_offset_is_read_or_treated_as_utc() {
        assert_eq!(parse_offset("+0000"), Some(0));
        assert_eq!(parse_offset("+0300"), Some(10_800));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        for bad in ["", "0300", "+03", "+03000", "+03xx", "Z"] {
            assert_eq!(parse_offset(bad), None, "{bad:?}");
        }
    }
}
