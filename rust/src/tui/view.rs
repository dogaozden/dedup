//! Painting the browser. Reads the model, writes to a frame, decides nothing.
//!
//! The layout is the oracle's, column for column, because a user who knows the
//! Python tool should not have to learn a second screen. Where curses has no
//! ratatui equivalent — hand-drawn box borders, one overlay routine that also
//! measures itself — the native widget does the same job and the difference is
//! recorded in LOG.md.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use super::model::{elide_left, Browser, Mode, MIN_HEIGHT, MIN_WIDTH};
use crate::size::{human_count, human_size};

const ACCENT: Color = Color::Cyan;
const MARK: Color = Color::Red;
const KEEP: Color = Color::Green;
const DIM: Color = Color::Blue;
const WARN: Color = Color::Yellow;

/// Width of the date column, and the gap the path may not run into.
const DATE_WIDTH: usize = 12;

const HINTS: [&str; 2] = [
    "↑↓ file   ←→ group   space mark   a keep-this   u clear",
    "b bulk    ENTER apply   ? help    q quit",
];

fn dim() -> Style {
    Style::default().fg(DIM)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn accent() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

fn width_of(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

/// One line with something at the left margin and something against the right,
/// which is how every heading on this screen is built.
fn spread(
    width: u16,
    indent: usize,
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(indent))];
    let used = indent + width_of(&left) + width_of(&right);
    spans.extend(left);
    // The right edge keeps two columns clear, the way the oracle's
    // `w - len(text) - 2` does.
    let pad = (width as usize).saturating_sub(2).saturating_sub(used);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    Line::from(spans)
}

fn rule(width: u16) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled("─".repeat((width as usize).saturating_sub(2)), dim()),
    ])
}

fn line(row: Rect, frame: &mut Frame, content: Line<'static>) {
    frame.render_widget(Paragraph::new(content), row);
}

pub fn draw(frame: &mut Frame, browser: &Browser) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!("Terminal too small — resize to at least {MIN_WIDTH}×{MIN_HEIGHT}");
        frame.render_widget(Paragraph::new(message), area);
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1), // title and root
        Constraint::Length(1), // rule
        Constraint::Length(1), // what the scan found
        Constraint::Length(1), // gap
        Constraint::Length(1), // which group
        Constraint::Length(1), // rule
        Constraint::Min(1),    // the members
        Constraint::Length(1), // rule
        Constraint::Length(1), // flash, or the first hint line
        Constraint::Length(1), // the second hint line
    ])
    .split(area);
    let width = area.width;

    let root = elide_left(&browser.root, (width as usize).saturating_sub(12));
    line(
        rows[0],
        frame,
        spread(
            width,
            1,
            vec![Span::styled("dedup", accent())],
            vec![Span::styled(root, dim())],
        ),
    );
    line(rows[1], frame, rule(width));

    let summary = format!(
        "{} groups · {} reclaimable",
        human_count(browser.groups.len() as u64),
        human_size(browser.reclaimable())
    );
    let marked = if browser.marked_count() > 0 {
        vec![Span::styled(
            format!(
                "{} marked · {} to free",
                human_count(browser.marked_count() as u64),
                human_size(browser.marked_bytes())
            ),
            Style::default().fg(MARK).add_modifier(Modifier::BOLD),
        )]
    } else {
        Vec::new()
    };
    line(
        rows[2],
        frame,
        spread(width, 2, vec![Span::styled(summary, bold())], marked),
    );

    // The progress the scan is making, drawn where the gap is once there is a
    // scan to draw. This is the whole of M8's "the browser draws the scan's
    // progress itself": the numbers come from `Browser::progress`, which the
    // loop reads out of the same counters the stderr pulse reads.
    if let Some(progress) = &browser.progress {
        line(rows[3], frame, progress_line(width, progress));
    }

    if browser.groups.is_empty() {
        let (said, how) = if browser.scanning {
            ("Scanning — nothing found yet.", "q to stop and quit")
        } else if browser.cancelled {
            ("Scan stopped.", "q to quit")
        } else {
            ("No duplicates left.", "q to quit")
        };
        line(
            rows[4],
            frame,
            Line::from(vec![Span::raw("  "), Span::styled(said, bold())]),
        );
        line(
            rows[6],
            frame,
            Line::from(vec![Span::raw("  "), Span::styled(how, dim())]),
        );
        // ...and whatever just happened, which is the whole reason the list is
        // empty. Without this, a group whose proof failed at apply time takes
        // its own explanation off the screen with it: the user marks copies,
        // confirms, and is told "No duplicates left" with nothing said about
        // the refusal. Found by `m8_live_end_to_end.py`.
        if !browser.flash.is_empty() {
            line(
                rows[8],
                frame,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        browser.flash.clone(),
                        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                    ),
                ]),
            );
        }
        return;
    }

    if browser.mode == Mode::Help {
        draw_help(frame, rows[4], rows[6], browser);
    } else {
        let group = &browser.groups[browser.group];
        line(
            rows[4],
            frame,
            spread(
                width,
                2,
                vec![Span::styled(
                    format!(
                        "Group {} of {}",
                        human_count(browser.group as u64 + 1),
                        human_count(browser.groups.len() as u64)
                    ),
                    accent(),
                )],
                vec![
                    Span::styled(
                        format!(
                            "{} × {} copies",
                            human_size(group.size),
                            human_count(group.members.len() as u64)
                        ),
                        dim(),
                    ),
                    // Said plainly, because it is the difference between what
                    // the tool knows and what it has guessed. A candidate is
                    // proved before anything of it is deleted, never after.
                    //
                    // Only the guess is labelled. A finished scan's groups are
                    // all proven, and a screen that said so on every row would
                    // be saying nothing on every row.
                    if group.proven {
                        Span::raw("")
                    } else {
                        Span::styled("  candidate", Style::default().fg(WARN))
                    },
                ],
            ),
        );
        line(rows[5], frame, rule(width));
        draw_members(frame, rows[6], browser);
        line(rows[7], frame, rule(width));
    }

    let first = if browser.flash.is_empty() {
        Span::styled(HINTS[0], dim())
    } else {
        Span::styled(
            browser.flash.clone(),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        )
    };
    line(rows[8], frame, Line::from(vec![Span::raw("  "), first]));
    line(
        rows[9],
        frame,
        Line::from(vec![Span::raw("  "), Span::styled(HINTS[1], dim())]),
    );

    match browser.mode {
        Mode::Bulk => overlay(
            frame,
            "Bulk actions",
            &[
                "1   Keep the oldest copy in every group".into(),
                "2   Keep the newest copy in every group".into(),
                "3   Keep the shortest path in every group".into(),
                "0   Clear every mark".into(),
            ],
            "esc to cancel",
        ),
        Mode::Confirm => overlay(
            frame,
            "Confirm",
            &confirmation(browser),
            "y  confirm      n  cancel",
        ),
        _ => {}
    }
}

/// Cells in the progress bar. The stderr pulse draws 24; the browser has a
/// header to share the row with, so it draws fewer.
const LIVE_BAR: usize = 16;

/// The scan's progress, in the browser's own row.
///
/// The two shapes are the pulse's two shapes, for the same reason: the walk
/// has no total to be a fraction of, so it says what it has seen, and a
/// reading stage says how far through it is. Nothing here is estimated — every
/// number arrives from a counter the scan keeps anyway.
pub fn progress_line(width: u16, progress: &crate::tui::model::Progress) -> Line<'static> {
    let body = if progress.total == 0 {
        format!(
            "{} files · {}",
            human_count(progress.done),
            human_size(progress.bytes)
        )
    } else {
        let done = progress.done.min(progress.total);
        let percent = done as f64 / progress.total as f64 * 100.0;
        let filled = (LIVE_BAR as f64 * percent / 100.0) as usize;
        format!(
            "{}{} {percent:3.0}%  {}/{} · {}/s",
            "█".repeat(filled),
            "░".repeat(LIVE_BAR - filled),
            human_count(done),
            human_count(progress.total),
            human_size(progress.rate)
        )
    };
    let text: String = format!("  {}…  {body}", progress.label)
        .chars()
        .take(usize::from(width).saturating_sub(1))
        .collect();
    Line::from(vec![Span::styled(text, Style::default().fg(ACCENT))])
}

/// Exactly what is about to happen, in the words the user needs to judge it:
/// how many, how much, and whether it can be taken back.
pub fn confirmation(browser: &Browser) -> Vec<String> {
    let verb = if browser.hard {
        "Delete permanently"
    } else {
        "Move to Trash"
    };
    let mut lines = vec![
        format!(
            "{verb}: {} files",
            human_count(browser.marked_count() as u64)
        ),
        format!("Reclaims {}", human_size(browser.marked_bytes())),
    ];
    // What the user is agreeing to pay for. The proof reads every byte of
    // every candidate group before anything is trashed, and on a big selection
    // that is the slow part of the whole session — so it is said in the
    // dialog rather than discovered afterwards.
    let owed = browser.unproven_marked();
    if owed > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} group(s) will be read and proved first.",
            human_count(owed as u64)
        ));
    }
    if browser.hard {
        lines.push(String::new());
        lines.push("This cannot be undone.".into());
    }
    lines
}

fn draw_members(frame: &mut Frame, area: Rect, browser: &Browser) {
    let group = &browser.groups[browser.group];
    let width = area.width as usize;
    let visible = area.height as usize;

    // Keep the cursor in the middle of the window where the list is long
    // enough for a middle to exist.
    let start = browser
        .member
        .saturating_sub(visible / 2)
        .min(group.members.len().saturating_sub(visible));
    let end = (start + visible).min(group.members.len());

    let mut items = Vec::with_capacity(end - start);
    for position in start..end {
        let member = &group.members[position];
        let is_cursor = position == browser.member;
        let is_marked = browser.is_marked(browser.group, position);

        // A record pinned from outside the tree is shown, not hidden — the user
        // is entitled to know the copy is there and why it cannot go.
        let (glyph, colour) = if !member.deletable {
            ("⊘", DIM)
        } else if is_marked {
            ("●", MARK)
        } else {
            ("○", KEEP)
        };
        let badge = match member.extra_links {
            0 => String::new(),
            1 => "  +1 link".into(),
            extra => format!("  +{extra} links"),
        };
        let room = width.saturating_sub(9 + DATE_WIDTH + badge.chars().count());
        let path = elide_left(&member.path, room);
        let path_style = if is_cursor {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if is_marked {
            dim()
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::raw("   "),
            Span::styled(
                if is_cursor { "›" } else { " " },
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                glyph,
                Style::default().fg(colour).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(path.clone(), path_style),
            Span::styled(badge.clone(), Style::default().fg(WARN)),
        ];
        let used = 7 + path.chars().count() + badge.chars().count();
        let pad = width.saturating_sub(DATE_WIDTH + 1).saturating_sub(used);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(member.date.clone(), dim()));
        items.push(ListItem::new(Line::from(spans)));
    }
    frame.render_widget(List::new(items), area);

    if group.members.len() > visible {
        let more = format!(
            "… {} more",
            human_count((group.members.len() - visible) as u64)
        );
        let row = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(more, dim()))).right_aligned(),
            row,
        );
    }
}

/// The keymap, including the two things the oracle's own help text leaves out.
fn help_lines(hard: bool) -> Vec<(&'static str, String)> {
    let apply = if hard {
        "review and apply — delete marked files permanently"
    } else {
        "review and apply — move marked files to Trash"
    };
    vec![
        ("↑ ↓  or  j k", "move between files in this group".into()),
        ("← →  or  h l", "previous / next group".into()),
        ("n / p", "next / previous group".into()),
        ("space", "mark or unmark the highlighted file".into()),
        ("a", "mark every copy except the highlighted one".into()),
        ("u", "clear all marks in this group".into()),
        ("g / G", "jump to first / last group".into()),
        ("b", "bulk actions across every group".into()),
        ("ENTER", apply.into()),
        ("?", "toggle this help".into()),
        ("q", "quit without deleting anything".into()),
    ]
}

fn draw_help(frame: &mut Frame, title: Rect, body: Rect, browser: &Browser) {
    line(
        title,
        frame,
        Line::from(vec![Span::raw("  "), Span::styled("Keys", accent())]),
    );
    let lines: Vec<Line<'static>> = help_lines(browser.hard)
        .into_iter()
        .take(body.height as usize)
        .map(|(keys, description)| {
            Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{keys:<14}"), bold()),
                Span::raw("  "),
                Span::styled(description, dim()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}

/// A centred box. curses drew its own border and measured its own text; a
/// ratatui `Block` does both, so the box is a widget here and the corners are
/// rounded rather than square.
fn overlay(frame: &mut Frame, title: &str, lines: &[String], footer: &str) {
    let area = frame.area();
    let content = lines
        .iter()
        .map(|text| text.chars().count())
        .max()
        .unwrap_or(0);
    let inner = content
        .max(title.chars().count())
        .max(footer.chars().count());
    let width = ((inner + 6) as u16).min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(format!(" {title} "), accent()))
        .title_bottom(Span::styled(format!(" {footer} "), dim()));
    let text: Vec<Line<'static>> = lines
        .iter()
        .map(|content| Line::from(vec![Span::raw(" "), Span::raw(content.clone())]))
        .collect();
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(text).block(block), box_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::{Browser, Group, Key, Member};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn member(record: usize, path: &str, deletable: bool, links: usize) -> Member {
        Member {
            record,
            name: path.into(),
            path: path.into(),
            date: "2026-08-23".into(),
            extra_links: links,
            deletable,
            mtime: record as f64,
        }
    }

    /// The seeded scan every snapshot below is taken of: two groups, one
    /// hardlink cluster, one record pinned from outside the tree.
    fn browser() -> Browser {
        Browser::new(
            vec![
                Group {
                    index: 0,
                    size: 4096,
                    proven: true,
                    members: vec![
                        member(0, "photos/beach.jpg", true, 0),
                        member(1, "backup/photos/beach.jpg", true, 2),
                        member(2, "old/beach copy.jpg", false, 0),
                    ],
                },
                Group {
                    index: 1,
                    size: 512,
                    proven: true,
                    members: vec![
                        member(3, "notes.txt", true, 0),
                        member(4, "archive/notes.txt", true, 0),
                    ],
                },
            ],
            "/scratch/tree".into(),
            false,
        )
    }

    fn render(browser: &Browser, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, browser))
            .expect("draw the browser");
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

    /// The style of the first cell whose symbol matches, scanning in reading
    /// order.
    fn style_of(browser: &Browser, width: u16, height: u16, symbol: &str) -> Style {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, browser))
            .expect("draw the browser");
        let buffer = terminal.backend().buffer().clone();
        for y in 0..height {
            for x in 0..width {
                if let Some(cell) = buffer.cell((x, y)) {
                    if cell.symbol() == symbol {
                        return cell.style();
                    }
                }
            }
        }
        panic!("no cell drew {symbol:?}");
    }

    #[test]
    fn the_group_view_shows_every_path_and_marks_the_keeper() {
        let screen = render(&browser(), 80, 16);
        assert_eq!(
            screen,
            vec![
                " dedup                                                           /scratch/tree",
                " ──────────────────────────────────────────────────────────────────────────────",
                "  2 groups · 4.5 KiB reclaimable",
                "",
                "  Group 1 of 2                                              4.0 KiB × 3 copies",
                " ──────────────────────────────────────────────────────────────────────────────",
                "   › ○ photos/beach.jpg                                            2026-08-23",
                "     ○ backup/photos/beach.jpg  +2 links                           2026-08-23",
                "     ⊘ old/beach copy.jpg                                          2026-08-23",
                "",
                "",
                "",
                "",
                " ──────────────────────────────────────────────────────────────────────────────",
                "  ↑↓ file   ←→ group   space mark   a keep-this   u clear",
                "  b bulk    ENTER apply   ? help    q quit",
            ]
        );
    }

    #[test]
    fn a_marked_copy_and_a_pinned_one_look_different_from_a_keeper() {
        let mut browser = browser();
        browser.member = 1;
        browser.on_key(Key::Toggle);
        let screen = render(&browser, 80, 16);
        assert!(
            screen[7].contains("● backup/photos/beach.jpg"),
            "the marked copy did not change glyph: {:?}",
            screen[7]
        );
        assert!(
            screen[8].contains("⊘ old/beach copy.jpg"),
            "the pinned copy lost its glyph: {:?}",
            screen[8]
        );
        assert!(
            screen[6].contains("○ photos/beach.jpg"),
            "the surviving copy lost its glyph: {:?}",
            screen[6]
        );
        assert_eq!(
            screen[2],
            "  2 groups · 4.5 KiB reclaimable                    1 marked · 4.0 KiB to free"
        );

        // The glyphs carry the meaning in colour as well as in shape.
        assert_eq!(style_of(&browser, 80, 16, "●").fg, Some(MARK));
        assert_eq!(style_of(&browser, 80, 16, "○").fg, Some(KEEP));
        assert_eq!(style_of(&browser, 80, 16, "⊘").fg, Some(DIM));
    }

    #[test]
    fn the_cursor_row_is_the_one_drawn_in_reverse() {
        let mut browser = browser();
        browser.member = 2;
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &browser))
            .expect("draw the browser");
        let buffer = terminal.backend().buffer().clone();
        // Column 7 is where paths start; row 8 is the third member.
        let cell = buffer.cell((7, 8)).expect("a cell under the path column");
        assert!(cell.style().add_modifier.contains(Modifier::REVERSED));
        let other = buffer.cell((7, 6)).expect("a cell on another row");
        assert!(!other.style().add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn the_confirmation_says_how_many_how_much_and_whether_it_is_reversible() {
        let mut browser = browser();
        browser.on_key(Key::KeepOnlyThis);
        browser.on_key(Key::Apply);
        let screen = render(&browser, 80, 16);
        let body = screen.join("\n");
        assert!(body.contains("Confirm"), "{body}");
        assert!(body.contains("Move to Trash: 1 files"), "{body}");
        assert!(body.contains("Reclaims 4.0 KiB"), "{body}");
        assert!(body.contains("y  confirm      n  cancel"), "{body}");
        assert!(
            !body.contains("cannot be undone"),
            "a Trash move claimed to be permanent"
        );
    }

    #[test]
    fn the_permanent_confirmation_says_so_in_as_many_words() {
        let mut browser = browser();
        browser.hard = true;
        browser.on_key(Key::KeepOnlyThis);
        browser.on_key(Key::Apply);
        let body = render(&browser, 80, 16).join("\n");
        assert!(body.contains("Delete permanently: 1 files"), "{body}");
        assert!(body.contains("This cannot be undone."), "{body}");
    }

    #[test]
    fn the_bulk_menu_lists_the_three_rules_and_the_way_out() {
        let mut browser = browser();
        browser.on_key(Key::Bulk);
        let body = render(&browser, 80, 16).join("\n");
        for line in [
            "1   Keep the oldest copy in every group",
            "2   Keep the newest copy in every group",
            "3   Keep the shortest path in every group",
            "0   Clear every mark",
            "esc to cancel",
        ] {
            assert!(body.contains(line), "missing {line:?} in\n{body}");
        }
    }

    #[test]
    fn help_lists_every_binding_and_names_the_right_verb() {
        let mut browser = browser();
        browser.on_key(Key::Help);
        let body = render(&browser, 80, 20).join("\n");
        assert!(body.contains("Keys"), "{body}");
        assert!(body.contains("move between files in this group"), "{body}");
        assert!(body.contains("move marked files to Trash"), "{body}");

        browser.hard = true;
        let body = render(&browser, 80, 20).join("\n");
        assert!(body.contains("delete marked files permanently"), "{body}");
    }

    #[test]
    fn a_long_list_scrolls_around_the_cursor_and_says_what_is_off_screen() {
        let members = (0..20)
            .map(|index| member(index, &format!("copy-{index:02}.bin"), true, 0))
            .collect();
        let mut browser = Browser::new(
            vec![Group {
                index: 0,
                size: 100,
                proven: true,
                members,
            }],
            "/scratch".into(),
            false,
        );
        // The window is three rows at this height, so seventeen are off screen.
        let screen = render(&browser, 80, 12);
        assert!(screen[6].contains("copy-00.bin"), "{:?}", screen[6]);
        assert!(screen[8].contains("… 17 more"), "{:?}", screen[8]);

        browser.member = 10;
        let screen = render(&browser, 80, 12);
        assert!(screen[6].contains("copy-09.bin"), "{:?}", screen[6]);
        assert!(screen[7].contains("copy-10.bin"), "{:?}", screen[7]);
        assert!(
            !screen.join("\n").contains("copy-00.bin"),
            "list did not scroll"
        );

        // At the end the window stops rather than running past the last row.
        browser.member = 19;
        let screen = render(&browser, 80, 12);
        assert!(screen[6].contains("copy-17.bin"), "{:?}", screen[6]);
        assert!(screen[8].contains("copy-19.bin"), "{:?}", screen[8]);
    }

    #[test]
    fn a_path_too_long_for_the_screen_keeps_its_filename() {
        let long = format!("{}/target.bin", "nested/".repeat(20));
        let browser = Browser::new(
            vec![Group {
                index: 0,
                size: 100,
                proven: true,
                members: vec![member(0, &long, true, 0), member(1, "b.bin", true, 0)],
            }],
            "/scratch".into(),
            false,
        );
        let screen = render(&browser, 80, 14);
        assert!(screen[6].contains("…"), "{:?}", screen[6]);
        assert!(screen[6].contains("target.bin"), "{:?}", screen[6]);
        assert!(screen[6].contains("2026-08-23"), "the date was pushed off");
        assert!(screen[6].chars().count() <= 80, "the row overflowed");
    }

    #[test]
    fn a_terminal_too_small_to_lay_out_says_so_instead_of_scrambling() {
        let screen = render(&browser(), 40, 10);
        assert!(
            screen[0].starts_with("Terminal too small"),
            "{:?}",
            screen[0]
        );
    }

    #[test]
    fn the_empty_screen_offers_the_way_out() {
        let browser = Browser::new(Vec::new(), "/scratch".into(), false);
        let body = render(&browser, 80, 14).join("\n");
        assert!(body.contains("No duplicates left."), "{body}");
        assert!(body.contains("q to quit"), "{body}");
    }

    #[test]
    fn an_emptied_list_still_says_why_it_emptied() {
        // The case that matters is a candidate whose proof failed at apply
        // time: the group leaves the list, and if the screen loses the
        // explanation with it the user is told "No duplicates left" and
        // nothing about the refusal. A deletion tool that goes quiet at
        // exactly the moment it declined to delete something is lying by
        // omission.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.flash = "1 group(s) refused — their copies no longer match".into();
        let body = render(&browser, 80, 14).join("\n");
        assert!(body.contains("No duplicates left."), "{body}");
        assert!(
            body.contains("refused"),
            "the refusal vanished with the group: {body}"
        );
    }

    #[test]
    fn a_scanning_browser_says_so_rather_than_claiming_there_is_nothing() {
        // Mid-scan, an empty list means "nothing found YET".
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.scanning = true;
        browser.progress = Some(crate::tui::model::Progress {
            label: "Scanning",
            done: 1234,
            total: 0,
            bytes: 5678,
            rate: 0,
        });
        let body = render(&browser, 80, 14).join("\n");
        assert!(body.contains("Scanning — nothing found yet."), "{body}");
        assert!(body.contains("q to stop and quit"), "{body}");
        // ...and the progress it is making is on the screen, from the scan's
        // own counters.
        assert!(body.contains("1,234 files"), "{body}");
    }

    #[test]
    fn the_progress_row_draws_the_two_shapes_the_pulse_draws() {
        // The walk has no total to be a fraction of; a reading stage does.
        let walking = progress_line(
            80,
            &crate::tui::model::Progress {
                label: "Scanning",
                done: 1000,
                total: 0,
                bytes: 4 * 1024 * 1024 * 1024,
                rate: 0,
            },
        );
        let text: String = walking
            .spans
            .iter()
            .map(|span| span.content.clone())
            .collect();
        assert!(text.contains("Scanning…"), "{text}");
        assert!(text.contains("1,000 files · 4.0 GiB"), "{text}");

        let reading = progress_line(
            80,
            &crate::tui::model::Progress {
                label: "Comparing",
                done: 50,
                total: 100,
                bytes: 1024 * 1024,
                rate: 1024 * 1024,
            },
        );
        let text: String = reading
            .spans
            .iter()
            .map(|span| span.content.clone())
            .collect();
        assert!(text.contains(" 50%  50/100 · 1.0 MiB/s"), "{text}");
        assert!(text.contains("████████░░░░░░░░"), "{text}");

        // ...and it never wraps, whatever it is asked to fit into.
        for width in [1_u16, 2, 10, 40, 80] {
            let line = progress_line(
                width,
                &crate::tui::model::Progress {
                    label: "Sampling deep",
                    done: 7,
                    total: 9999,
                    bytes: 1 << 30,
                    rate: 1 << 30,
                },
            );
            let drawn: String = line.spans.iter().map(|span| span.content.clone()).collect();
            assert!(
                drawn.chars().count() < usize::from(width).max(1),
                "{width} columns held {} characters",
                drawn.chars().count()
            );
        }
    }

    #[test]
    fn a_candidate_says_so_and_a_proven_group_does_not() {
        let mut browser = browser();
        browser.groups[0].proven = false;
        let body = render(&browser, 80, 16).join("\n");
        assert!(body.contains("candidate"), "{body}");
        browser.groups[0].proven = true;
        let body = render(&browser, 80, 16).join("\n");
        assert!(
            !body.contains("candidate"),
            "a proven group was labelled a guess: {body}"
        );
    }

    #[test]
    fn the_confirmation_says_what_the_proof_will_cost() {
        let mut browser = browser();
        browser.groups[0].proven = false;
        browser.member = 0;
        browser.keep_only_current();
        let lines = confirmation(&browser).join("\n");
        assert!(
            lines.contains("1 group(s) will be read and proved first."),
            "{lines}"
        );
        // A selection with nothing to prove does not threaten a read.
        browser.groups[0].proven = true;
        let lines = confirmation(&browser).join("\n");
        assert!(!lines.contains("proved first"), "{lines}");
    }

    #[test]
    fn a_refusal_is_shown_where_the_hints_were() {
        let mut browser = browser();
        browser.member = 2; // the pinned copy
        browser.on_key(Key::Toggle);
        let screen = render(&browser, 80, 16);
        assert!(
            screen[14].contains("hardlinked outside this tree"),
            "{:?}",
            screen[14]
        );
    }
}
