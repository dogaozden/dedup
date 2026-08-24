//! The pre-flight menu: the flags, as a form, before the scan starts.
//!
//! It gains nothing the command line does not have, and it proves that rather
//! than promising it: the form's answer is an argument list, and `main` reads
//! that argument list with the same parser it read the real one with. A field
//! is validated by `Flag::apply`, which is the parser's own conversion, so a
//! value the form rejects is rejected in the parser's words.
//!
//! Nothing here is deletion-adjacent — the menu runs before any scan exists,
//! so there is nothing yet to delete from. The model still lives apart from
//! the paint, for the same reason the browser's does: it is testable that way.

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::io::{self, Write};

use crate::cli::{flag_named, Flag, Takes};

/// The flags the form offers, in the order it shows them. Which flags belong
/// here is M5_1-MENU.md's list; what each of them means, what it defaults to
/// and whether it takes a value all come from the flag table.
const FIELDS: [&str; 7] = [
    "--min-size",
    "--ext",
    "--skip-ext",
    "--exclude",
    "--jobs",
    "--profile",
    "--no-cache",
];

/// The narrowest screen the form can be read on: the widest label and value
/// side by side, the root line, seven rows, the hint, and the frame around it.
pub const LEAST_WIDTH: u16 = 54;
pub const LEAST_HEIGHT: u16 = 16;

pub struct Row {
    pub flag: &'static Flag,
    /// What the command line gave or the user typed. Empty means "whatever the
    /// tool does anyway", and the row shows the flag table's default instead.
    pub value: String,
    /// For a flag that takes nothing: whether it is on.
    pub on: bool,
}

impl Row {
    pub fn label(&self) -> &'static str {
        self.flag.long.trim_start_matches('-')
    }

    /// What the row shows in its value column, and whether that is the user's
    /// answer or the tool's default.
    pub fn shown(&self) -> (String, bool) {
        match self.flag.takes {
            Takes::Nothing if self.on => ("on".to_string(), true),
            Takes::Nothing => ("off".to_string(), false),
            _ if self.value.is_empty() => (self.flag.default.to_string(), false),
            _ => (self.value.clone(), true),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
pub enum Step {
    Stay,
    /// Accept the whole form and scan.
    Scan,
    /// Leave without scanning.
    Quit,
}

pub struct Form {
    pub root: String,
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// The buffer while a field is being typed into, if one is.
    pub editing: Option<String>,
    /// The parser's own complaint about what was just typed.
    pub error: Option<String>,
    /// Everything from the command line the form does not own, kept so
    /// accepting the form cannot silently drop it.
    carried: Vec<String>,
    /// Whether the last frame drawn was big enough to read. A form nobody
    /// could see is a form nobody agreed to.
    legible: bool,
}

impl Form {
    /// Build the form from the command line that launched this run.
    ///
    /// `arguments` is what came after the program name. Values it gave for the
    /// form's own flags prefill their rows; everything else rides along
    /// untouched. The root is taken from those words rather than from the
    /// parsed path, so accepting the form hands the parser back the very
    /// string it was given — a name a `Path` could only render lossily makes
    /// the round trip unharmed.
    pub fn new(arguments: &[String]) -> Form {
        let mut rows: Vec<Row> = FIELDS
            .iter()
            .map(|&name| Row {
                flag: flag_named(name).expect("every field names a flag in the table"),
                value: String::new(),
                on: false,
            })
            .collect();
        let mut carried = Vec::new();

        let mut walk = arguments.iter().peekable();
        let mut root = String::from(".");
        let mut seen_root = false;
        while let Some(argument) = walk.next() {
            let Some(flag) = flag_named(argument) else {
                // The root itself is shown, not carried; a second bare word is
                // not something the parser accepts, so it never reaches here.
                if !seen_root && !argument.starts_with('-') {
                    seen_root = true;
                    root.clone_from(argument);
                } else {
                    carried.push(argument.clone());
                }
                continue;
            };
            let value = match flag.takes {
                Takes::Nothing => None,
                Takes::Value(_) => walk.next().cloned(),
                Takes::OneOf(_, allowed) => walk
                    .peek()
                    .is_some_and(|next| allowed.contains(&next.as_str()))
                    .then(|| walk.next().expect("peeked a value").clone()),
            };
            match rows.iter_mut().find(|row| row.flag.long == flag.long) {
                // Prefilled through `Flag::show`, so the row a person reads is
                // the row the scan runs on: the form hands these words back to
                // the parser verbatim.
                Some(row) => match (flag.takes, value) {
                    (Takes::Nothing, _) => row.on = true,
                    // A flag given twice fills the field with both, which is
                    // what the parser would have made of it.
                    (_, Some(value)) if row.value.is_empty() => row.value = flag.show(&value),
                    (_, Some(value)) => row.value = format!("{},{}", row.value, flag.show(&value)),
                    (_, None) => {}
                },
                None => {
                    carried.push(argument.clone());
                    carried.extend(value);
                }
            }
        }

        Form {
            root,
            rows,
            cursor: 0,
            editing: None,
            error: None,
            carried,
            legible: false,
        }
    }

    /// Remember whether the frame just drawn was one a person could read.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.legible = width >= LEAST_WIDTH && height >= LEAST_HEIGHT;
    }

    pub fn legible(&self) -> bool {
        self.legible
    }

    /// The command line the form stands for.
    ///
    /// A comma in a value is the parser's own separator for `--ext` and
    /// `--skip-ext`; `--exclude` is split on it too, so one field can hold
    /// several globs. That means a glob with a comma in it cannot be typed
    /// here — the form does less than the flag, which is the safe direction.
    pub fn to_arguments(&self) -> Vec<String> {
        let mut out = vec![self.root.clone()];
        out.extend(self.carried.iter().cloned());
        for row in &self.rows {
            match row.flag.takes {
                Takes::Nothing if row.on => out.push(row.flag.long.to_string()),
                Takes::Nothing => {}
                _ if row.value.is_empty() => {}
                _ => {
                    for piece in row.value.split(',').filter(|piece| !piece.is_empty()) {
                        out.push(row.flag.long.to_string());
                        out.push(piece.to_string());
                    }
                }
            }
        }
        out
    }

    /// Whether `text` would be accepted for the row under the cursor, in the
    /// parser's own words.
    fn complaint(&self, row: usize, text: &str) -> Option<String> {
        if text.is_empty() {
            return None; // cleared: the row goes back to the default
        }
        let flag = self.rows[row].flag;
        let mut scratch = crate::cli::Options::new();
        text.split(',')
            .filter(|piece| !piece.is_empty())
            .find_map(|piece| flag.apply(&mut scratch, Some(piece)).err())
    }

    pub fn on_key(&mut self, key: Key) -> Step {
        // The same rule the browser follows: a screen nobody could read is not
        // a screen anybody agreed to. Only the way out still works.
        if !self.legible {
            return match key {
                Key::Quit => Step::Quit,
                _ => Step::Stay,
            };
        }
        if let Some(buffer) = self.editing.as_mut() {
            match key {
                Key::Type(letter) => {
                    buffer.push(letter);
                    self.error = None;
                }
                Key::Rub => {
                    buffer.pop();
                    self.error = None;
                }
                Key::Enter => {
                    let typed = buffer.clone();
                    match self.complaint(self.cursor, &typed) {
                        // Rejected in the field, with the parser's message,
                        // rather than three minutes into a scan.
                        Some(why) => self.error = Some(why),
                        None => {
                            self.rows[self.cursor].value = typed;
                            self.editing = None;
                            self.error = None;
                        }
                    }
                }
                Key::Quit => {
                    self.editing = None;
                    self.error = None;
                }
                _ => {}
            }
            return Step::Stay;
        }
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(self.rows.len() - 1),
            Key::Edit => match self.rows[self.cursor].flag.takes {
                Takes::Nothing => self.rows[self.cursor].on = !self.rows[self.cursor].on,
                _ => self.editing = Some(self.rows[self.cursor].value.clone()),
            },
            Key::Enter => return Step::Scan,
            Key::Quit => return Step::Quit,
            Key::Type(_) | Key::Rub | Key::Ignored => {}
        }
        Step::Stay
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    /// Toggle a switch, or start typing into a field.
    Edit,
    Enter,
    Quit,
    Type(char),
    Rub,
    Ignored,
}

pub fn key_for(editing: bool, code: KeyCode) -> Key {
    match code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Quit,
        KeyCode::Backspace => Key::Rub,
        KeyCode::Char(letter) if editing => Key::Type(letter),
        KeyCode::Char('k') => Key::Up,
        KeyCode::Char('j') => Key::Down,
        KeyCode::Char(' ') => Key::Edit,
        KeyCode::Char('q') => Key::Quit,
        _ => Key::Ignored,
    }
}

// ------------------------------------------------------------------- drawing

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn draw(frame: &mut Frame, form: &Form) {
    let area = frame.area();
    if area.width < LEAST_WIDTH || area.height < LEAST_HEIGHT {
        let line = Line::from(format!(
            "the pre-flight menu needs {LEAST_WIDTH}x{LEAST_HEIGHT}"
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let widest = form
        .rows
        .iter()
        .map(|row| row.label().chars().count())
        .max()
        .unwrap_or(0);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "dedup-rs — before the scan",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("  {:widest$}  ", "root"), dim()),
            Span::raw(form.root.clone()),
        ]),
        Line::from(""),
    ];

    for (position, row) in form.rows.iter().enumerate() {
        let here = position == form.cursor;
        let (shown, chosen) = row.shown();
        let shown = match (here, form.editing.as_ref()) {
            (true, Some(buffer)) => format!("{buffer}_"),
            _ => shown,
        };
        let marker = if here { "> " } else { "  " };
        let value = if chosen || form.editing.is_some() && here {
            Style::default()
        } else {
            dim()
        };
        let mut spans = vec![
            Span::raw(marker.to_string()),
            Span::raw(format!("{:widest$}  ", row.label())),
            Span::styled(shown, value),
        ];
        if here {
            spans[1] = Span::styled(
                format!("{:widest$}  ", row.label()),
                Style::default().add_modifier(Modifier::BOLD),
            );
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(""));
    // The help for whichever row the cursor is on, straight from the table.
    lines.push(Line::from(Span::styled(
        format!("  {}", form.rows[form.cursor].flag.help),
        dim(),
    )));
    if let Some(error) = &form.error {
        lines.push(Line::from(Span::raw(format!("  {error}"))));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        if form.editing.is_some() {
            "  type to edit   enter accept the field   esc cancel".to_string()
        } else {
            "  up/down move   space edit   enter scan   esc quit".to_string()
        },
        dim(),
    )));
    frame.render_widget(Paragraph::new(lines), Rect { ..area });
}

// ------------------------------------------------------------------ the loop

fn put_the_terminal_back() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = io::stdout().flush();
}

struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        put_the_terminal_back();
    }
}

/// What the form came back with.
pub enum Answer {
    /// Scan with these arguments.
    Scan(Vec<String>),
    /// Esc or q: leave without scanning.
    Quit,
    /// The terminal went away while the form was up. Nothing was scanned, and
    /// there is nobody left to tell about it.
    TerminalGone,
}

/// Show the form.
///
/// The loop is the browser's loop, and deliberately so. `wait_for_terminal_input`
/// has three answers and all three are consumed here: a terminal that has gone
/// away is readable forever and empty forever, and crossterm answers that by
/// looping inside `read` — no error comes back, so `?` never fires and the
/// process burns a core. Entering `event::read()` only when poll has just said
/// bytes are waiting is what makes that unreachable. Falling through to the
/// read on `Waited` — the ordinary state of a form somebody is looking at —
/// puts it straight back.
pub fn ask(arguments: &[String]) -> io::Result<Answer> {
    let mut form = Form::new(arguments);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        put_the_terminal_back();
        previous(info);
    }));

    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    let answer = loop {
        // A paint that fails on a terminal which has since hung up is that
        // terminal dying, not a fault; on one that is still there it is still
        // an error.
        let painted = match terminal.draw(|frame| draw(frame, &form)) {
            Ok(frame) => frame.area,
            Err(error) => {
                if super::hung_up() {
                    break Answer::TerminalGone;
                }
                return Err(error);
            }
        };
        form.resize(painted.width, painted.height);
        match crate::sys::wait_for_terminal_input(super::WAIT_FOR_KEY) {
            crate::sys::Input::Ready => {}
            crate::sys::Input::HungUp => break Answer::TerminalGone,
            // Nothing typed. Round the loop to repaint, which is also how a
            // window resized while nobody was typing gets its new size.
            crate::sys::Input::Waited => continue,
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match form.on_key(key_for(form.editing.is_some(), key.code)) {
            Step::Stay => {}
            Step::Scan => {
                debug_assert!(form.legible(), "scanned on a screen nobody could read");
                break Answer::Scan(form.to_arguments());
            }
            Step::Quit => break Answer::Quit,
        }
    };

    if matches!(answer, Answer::TerminalGone) {
        // `Terminal`'s own `Drop` tries to show the cursor and, when it cannot,
        // `eprintln!`s about it — which panics when the descriptor it would
        // print to is the terminal that just died. There is no cursor to put
        // back on a terminal that is gone. The guard above still runs, and
        // still restores a terminal that is really there.
        std::mem::forget(terminal);
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{parse_arguments_from, Parsed};

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    fn form(arguments: &[&str]) -> Form {
        let mut form = Form::new(&words(arguments));
        form.resize(100, 40);
        form
    }

    fn options(arguments: &[String]) -> crate::cli::Options {
        match parse_arguments_from(arguments.iter().cloned()).expect("the form's own words") {
            Parsed::Scan(options) => *options,
            Parsed::Help => panic!("the form never asks for help"),
        }
    }

    fn row_of(form: &Form, long: &str) -> usize {
        form.rows
            .iter()
            .position(|row| row.flag.long == long)
            .expect("a field")
    }

    #[test]
    fn an_untouched_form_is_the_command_line_it_was_built_from() {
        let form = form(&["/tmp/tree"]);
        assert_eq!(form.to_arguments(), words(&["/tmp/tree"]));
        // Enter-through, with nothing typed, is a flagless run.
        let straight = options(&words(&["/tmp/tree"]));
        let through = options(&form.to_arguments());
        assert_eq!(straight.min_size, through.min_size);
        assert_eq!(straight.profile, through.profile);
        assert_eq!(straight.jobs, through.jobs);
        assert!(!through.no_cache);
    }

    #[test]
    fn the_command_line_prefills_the_fields_it_names() {
        let form = form(&[
            "/tmp/tree",
            "--min-size",
            "4096",
            "--ext",
            "jpg",
            "--ext",
            "png",
            "--profile",
            "spinning",
            "--no-cache",
        ]);
        // Prefilled humanly, and prefilled with the words the form will hand
        // back: 4096 goes in and 4 KiB comes out, and 4 KiB is what the scan
        // then runs on.
        assert_eq!(form.rows[row_of(&form, "--min-size")].value, "4 KiB");
        assert_eq!(form.rows[row_of(&form, "--ext")].value, "jpg,png");
        assert_eq!(form.rows[row_of(&form, "--profile")].value, "spinning");
        assert!(form.rows[row_of(&form, "--no-cache")].on);
        // ...and reading the form back gives the same options.
        let back = options(&form.to_arguments());
        assert_eq!(back.min_size, 4096);
        assert_eq!(back.ext, ["jpg", "png"]);
        assert_eq!(back.profile, crate::device::Wanted::Spinning);
        assert!(back.no_cache);
    }

    #[test]
    fn a_flag_the_form_does_not_own_rides_through_untouched() {
        let form = form(&[
            "/tmp/tree",
            "--report-json",
            "/tmp/out.json",
            "--hard",
            "-i",
            "--min-size",
            "1",
        ]);
        let back = options(&form.to_arguments());
        assert_eq!(back.report_json.unwrap().to_str().unwrap(), "/tmp/out.json");
        assert!(back.hard);
        assert!(back.interactive);
        assert_eq!(back.min_size, 1);
        assert_eq!(back.root.to_str().unwrap(), "/tmp/tree");
    }

    #[test]
    fn editing_a_field_changes_what_the_scan_is_asked_for() {
        let mut form = form(&["/tmp/tree"]);
        assert_eq!(form.on_key(Key::Edit), Step::Stay); // min-size is row 0
        assert!(form.editing.is_some());
        for letter in "512".chars() {
            form.on_key(Key::Type(letter));
        }
        assert_eq!(form.on_key(Key::Enter), Step::Stay);
        assert!(form.editing.is_none(), "enter commits the field");
        assert_eq!(form.on_key(Key::Enter), Step::Scan, "enter again scans");
        assert_eq!(options(&form.to_arguments()).min_size, 512);
    }

    #[test]
    fn a_field_is_refused_in_the_field_with_the_parsers_own_words() {
        let mut form = form(&["/tmp/tree"]);
        form.on_key(Key::Edit);
        for letter in "lots".chars() {
            form.on_key(Key::Type(letter));
        }
        form.on_key(Key::Enter);
        assert_eq!(
            form.error.as_deref(),
            Some("not a size: \"lots\" — plain bytes or 512k, 100mb, 1.5g")
        );
        assert!(form.editing.is_some(), "the field keeps the cursor");
        // The same message the command line gives.
        let straight = parse_arguments_from(words(&["/tmp", "--min-size", "lots"]).into_iter());
        assert_eq!(straight.err().unwrap(), form.error.unwrap());
    }

    #[test]
    fn a_bad_profile_is_refused_the_same_way() {
        let mut form = form(&["/tmp/tree"]);
        form.cursor = row_of(&form, "--profile");
        form.on_key(Key::Edit);
        for letter in "hdd".chars() {
            form.on_key(Key::Type(letter));
        }
        form.on_key(Key::Enter);
        assert_eq!(
            form.error.as_deref(),
            Some("--profile takes flash, spinning or auto")
        );
    }

    #[test]
    fn clearing_a_field_puts_the_default_back() {
        let mut form = form(&["/tmp/tree", "--min-size", "4096"]);
        form.on_key(Key::Edit);
        for _ in 0.."4 KiB".len() {
            form.on_key(Key::Rub);
        }
        form.on_key(Key::Enter);
        assert_eq!(form.rows[0].value, "");
        // The table's default, humanised there and shown here, and a value
        // this field would accept if it were typed into it.
        assert_eq!(form.rows[0].shown().0, "10 MiB");
        assert!(form.complaint(0, "10 MiB").is_none());
        assert_eq!(
            options(&form.to_arguments()).min_size,
            crate::consts::DEFAULT_MIN_SIZE
        );
    }

    #[test]
    fn a_switch_toggles_and_reaches_the_options() {
        let mut form = form(&["/tmp/tree"]);
        form.cursor = row_of(&form, "--no-cache");
        assert_eq!(form.rows[form.cursor].shown().0, "off");
        form.on_key(Key::Edit);
        assert_eq!(form.rows[form.cursor].shown().0, "on");
        assert!(options(&form.to_arguments()).no_cache);
        form.on_key(Key::Edit);
        assert!(!options(&form.to_arguments()).no_cache);
    }

    #[test]
    fn esc_and_q_both_leave_without_scanning() {
        let mut form = form(&["/tmp/tree"]);
        assert_eq!(form.on_key(Key::Quit), Step::Quit);
        assert_eq!(key_for(false, KeyCode::Esc), Key::Quit);
        assert_eq!(key_for(false, KeyCode::Char('q')), Key::Quit);
        // ...but q is a letter once a field is being typed into.
        assert_eq!(key_for(true, KeyCode::Char('q')), Key::Type('q'));
        assert_eq!(key_for(true, KeyCode::Esc), Key::Quit);
    }

    #[test]
    fn the_cursor_stops_at_both_ends() {
        let mut form = form(&["/tmp/tree"]);
        form.on_key(Key::Up);
        assert_eq!(form.cursor, 0);
        for _ in 0..20 {
            form.on_key(Key::Down);
        }
        assert_eq!(form.cursor, form.rows.len() - 1);
    }

    #[test]
    fn a_form_too_small_to_read_is_a_form_nobody_agreed_to() {
        // REVIEW-M5's second finding was a dialog that was never drawn but
        // still acted on. Nothing here deletes, but a scan started on settings
        // nobody could see is the same mistake.
        let mut form = Form::new(&words(&["/tmp/tree"]));
        form.resize(20, 6);
        assert!(!form.legible());
        assert_eq!(form.on_key(Key::Enter), Step::Stay);
        assert_eq!(form.on_key(Key::Edit), Step::Stay);
        assert!(form.editing.is_none());
        assert_eq!(
            form.on_key(Key::Quit),
            Step::Quit,
            "the way out still works"
        );
        form.resize(LEAST_WIDTH, LEAST_HEIGHT);
        assert_eq!(form.on_key(Key::Enter), Step::Scan);
    }

    // ------------------------------------------------------- what it looks like

    fn render(form: &Form, width: u16, height: u16) -> Vec<String> {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, form))
            .expect("draw the form");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                let row: String = (0..width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map_or(" ".to_string(), |cell| cell.symbol().to_string())
                    })
                    .collect();
                row.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn the_form_shows_the_root_every_field_and_every_default() {
        let drawn = render(&form(&["/tmp/tree"]), 72, 20);
        let screen = drawn.join("\n");
        assert!(
            screen.contains("/tmp/tree"),
            "the root is not shown:\n{screen}"
        );
        for name in FIELDS {
            let flag = flag_named(name).expect("a flag");
            let label = name.trim_start_matches('-');
            assert!(
                drawn.iter().any(|line| line.contains(label)),
                "{label} has no row:\n{screen}"
            );
            let shown = match flag.takes {
                Takes::Nothing => "off",
                _ => flag.default,
            };
            assert!(
                screen.contains(shown),
                "{label} does not show its default {shown:?}:\n{screen}"
            );
        }
        assert!(
            screen.contains("enter scan"),
            "no way out is offered:\n{screen}"
        );
        assert!(screen.contains("esc quit"));
    }

    #[test]
    fn a_value_from_the_command_line_is_shown_in_place_of_its_default() {
        let drawn = render(&form(&["/tmp/tree", "--profile", "spinning"]), 72, 20);
        let screen = drawn.join("\n");
        assert!(screen.contains("profile"));
        assert!(screen.contains("spinning"));
        // The row shows the answer, so the default is no longer on that line.
        let row = drawn
            .iter()
            .find(|line| line.contains("profile"))
            .expect("a profile row");
        assert!(!row.contains("auto"), "still showing the default: {row}");
    }

    #[test]
    fn the_row_under_the_cursor_is_marked_and_explains_itself() {
        let mut form = form(&["/tmp/tree"]);
        form.cursor = row_of(&form, "--jobs");
        let drawn = render(&form, 72, 20);
        let row = drawn
            .iter()
            .find(|line| line.contains("jobs"))
            .expect("a jobs row");
        assert!(row.starts_with("> "), "the cursor row is not marked: {row}");
        // The help line is the flag table's own, not a second copy of it.
        assert!(drawn
            .join("\n")
            .contains(flag_named("--jobs").unwrap().help));
    }

    #[test]
    fn a_field_being_typed_into_shows_what_has_been_typed() {
        let mut form = form(&["/tmp/tree"]);
        form.on_key(Key::Edit);
        for letter in "40".chars() {
            form.on_key(Key::Type(letter));
        }
        let screen = render(&form, 72, 20).join("\n");
        assert!(screen.contains("40_"), "the buffer is not shown:\n{screen}");
        assert!(screen.contains("esc cancel"), "the edit keys are not shown");
    }

    #[test]
    fn a_refusal_is_shown_beside_the_field_it_belongs_to() {
        let mut form = form(&["/tmp/tree"]);
        form.on_key(Key::Edit);
        for letter in "lots".chars() {
            form.on_key(Key::Type(letter));
        }
        form.on_key(Key::Enter);
        let screen = render(&form, 72, 20).join("\n");
        assert!(
            screen.contains("not a size: \"lots\" — plain bytes or 512k, 100mb, 1.5g"),
            "{screen}"
        );
    }

    #[test]
    fn the_min_size_field_shows_a_size_a_person_can_read_and_type() {
        // The default first, then a value from the command line, then one
        // typed into the field — the three ways a size reaches this row, all
        // of them showing what the scan will actually use.
        let screen = render(&form(&["/tmp/tree"]), 72, 20).join("\n");
        assert!(screen.contains("min-size  10 MiB"), "{screen}");
        assert!(!screen.contains("10485760"), "{screen}");

        let screen = render(&form(&["/tmp/tree", "--min-size", "104857600"]), 72, 20).join("\n");
        assert!(screen.contains("min-size  100 MiB"), "{screen}");

        let mut form = form(&["/tmp/tree"]);
        form.on_key(Key::Edit);
        for letter in "100mb".chars() {
            form.on_key(Key::Type(letter));
        }
        form.on_key(Key::Enter);
        assert_eq!(form.error, None);
        let screen = render(&form, 72, 20).join("\n");
        assert!(screen.contains("min-size  100mb"), "{screen}");
        assert_eq!(options(&form.to_arguments()).min_size, 100 * 1024 * 1024);
    }

    #[test]
    fn the_paint_and_the_model_agree_on_what_is_too_small_to_read() {
        // The two rules are the same two constants, and this is what keeps
        // them that way: REVIEW-M5's second finding was a screen the paint
        // layer refused to draw that the model went on acting for.
        for (width, height) in [
            (LEAST_WIDTH - 1, LEAST_HEIGHT),
            (LEAST_WIDTH, LEAST_HEIGHT - 1),
            (LEAST_WIDTH, LEAST_HEIGHT),
            (LEAST_WIDTH + 20, LEAST_HEIGHT + 10),
        ] {
            let mut form = Form::new(&words(&["/tmp/tree"]));
            form.resize(width, height);
            let painted = render(&form, width, height).join("\n");
            let drew_the_form = painted.contains("before the scan");
            assert_eq!(
                drew_the_form,
                form.legible(),
                "at {width}x{height} the paint and the model disagree:\n{painted}"
            );
            assert_eq!(
                form.on_key(Key::Enter) == Step::Scan,
                form.legible(),
                "at {width}x{height} a scan started on a screen nobody could read"
            );
        }
    }

    #[test]
    fn every_field_names_a_flag_the_parser_knows() {
        for name in FIELDS {
            assert!(flag_named(name).is_some(), "{name} is not a flag");
        }
        let form = form(&["/tmp/tree"]);
        for row in &form.rows {
            // The default shown is the flag table's, not one written twice.
            assert_eq!(
                row.flag.default,
                flag_named(row.flag.long).expect("in the table").default
            );
        }
    }
}
