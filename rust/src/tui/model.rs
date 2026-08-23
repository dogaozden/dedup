//! The browser's state, with no terminal anywhere in it.
//!
//! Everything a keystroke does — moving, marking, the survivor guard, the bulk
//! rules, and the plan that comes out the far end — is decided here, so it can
//! be tested by calling functions instead of by driving a terminal. The paint
//! layer reads this and never writes to it.

use std::collections::BTreeSet;

use crate::trash::Rule;

/// One member of a group as the browser shows it.
///
/// It carries a copy of what the screen needs rather than borrowing the scan,
/// so a test can build one without a filesystem behind it. `record` is the only
/// link back: it indexes the scan's records, and it is what deletion acts on.
pub struct Member {
    pub record: usize,
    pub path: String,
    pub date: String,
    /// Names beyond the first. A hardlinked inode wears several, and removing
    /// it removes all of them together.
    pub extra_links: usize,
    /// False when a name outside the scanned tree still pins the inode, so
    /// removing every name we can see would free nothing.
    pub deletable: bool,
    /// The modification time the oracle compares, for the oldest/newest rules.
    pub mtime: f64,
}

pub struct Group {
    /// Where this group sits in the scan's own list. It never changes, even
    /// once a round of deletion has renumbered what the browser shows, and it
    /// is half of what a mark names.
    pub index: usize,
    pub size: u64,
    pub members: Vec<Member>,
}

/// Which screen is in front. The overlays each read their own keys, so the
/// browse bindings are unreachable while one is up — the same shape as the
/// oracle's private `getch` loops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Browse,
    Help,
    Bulk,
    Confirm,
}

/// A keystroke, named by what it means rather than by which key sent it, so the
/// model never learns about crossterm.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    PreviousGroup,
    NextGroup,
    FirstGroup,
    LastGroup,
    Toggle,
    KeepOnlyThis,
    ClearGroup,
    Bulk,
    Apply,
    Help,
    Quit,
    Escape,
    Yes,
    No,
    Digit(u8),
    Ignored,
}

/// What the event loop should do after a keystroke.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Stay,
    /// The user confirmed a deletion. The loop removes what `plan` names and
    /// hands the result back to `absorb`.
    Apply,
    Quit,
}

/// What a round of deletion did, as the model needs to hear it.
#[derive(Default)]
pub struct Outcome {
    /// Names removed, not records: a hardlinked record removes several.
    pub files: usize,
    pub bytes: u64,
    /// Records that are actually gone, for rebuilding the groups.
    pub removed: Vec<usize>,
    /// Groups whose copies no longer matched and which were left alone whole.
    pub refused: usize,
    pub failures: Vec<String>,
}

pub struct Browser {
    pub groups: Vec<Group>,
    pub group: usize,
    pub member: usize,
    pub mode: Mode,
    pub flash: String,
    /// The scanned root, for the header.
    pub root: String,
    /// Whether removal is permanent. Changes the confirmation's verb, and
    /// nothing else in here.
    pub hard: bool,
    marked: BTreeSet<(usize, usize)>,
    pub applied_files: usize,
    pub applied_bytes: u64,
    pub refused: usize,
    pub failures: Vec<String>,
}

impl Browser {
    pub fn new(groups: Vec<Group>, root: String, hard: bool) -> Self {
        Browser {
            groups,
            group: 0,
            member: 0,
            mode: Mode::Browse,
            flash: String::new(),
            root,
            hard,
            marked: BTreeSet::new(),
            applied_files: 0,
            applied_bytes: 0,
            refused: 0,
            failures: Vec::new(),
        }
    }

    pub fn is_marked(&self, group: usize, member: usize) -> bool {
        self.marked.contains(&(group, member))
    }

    pub fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// What the marks would free. The oracle counts one file's size per mark,
    /// which is the same thing as long as a record is in one group only — and
    /// the pipeline guarantees that.
    pub fn marked_bytes(&self) -> u64 {
        self.marked
            .iter()
            .map(|&(group, _)| self.groups[group].size)
            .sum()
    }

    pub fn reclaimable(&self) -> u64 {
        self.groups
            .iter()
            .map(|group| {
                let freeable = group
                    .members
                    .iter()
                    .filter(|member| member.deletable)
                    .count();
                // One copy has to stay, so the last one is never reclaimable.
                group.size * freeable.saturating_sub(1) as u64
            })
            .sum()
    }

    fn marks_in(&self, group: usize) -> usize {
        self.marked.range((group, 0)..(group + 1, 0)).count()
    }

    /// The marks as `(group in the scan, record in the scan)` pairs.
    ///
    /// Screen positions never leave this module. They move whenever a round of
    /// deletion drops a group or a member, and a stale position handed to the
    /// remover would name a different file rather than a missing one.
    pub fn selection(&self) -> Vec<(usize, usize)> {
        self.marked
            .iter()
            .map(|&(group, member)| {
                (
                    self.groups[group].index,
                    self.groups[group].members[member].record,
                )
            })
            .collect()
    }

    fn current(&self) -> &Group {
        &self.groups[self.group]
    }

    /// Mark or unmark the highlighted member.
    ///
    /// Two refusals, both the oracle's: a record pinned from outside the tree
    /// frees nothing, and a group may never have every copy marked. The second
    /// is checked before the mark goes in, so the count it compares is the
    /// count without it.
    pub fn toggle(&mut self) {
        let key = (self.group, self.member);
        if self.marked.remove(&key) {
            return;
        }
        if !self.current().members[self.member].deletable {
            self.flash = "hardlinked outside this tree — deleting it frees nothing".into();
            return;
        }
        if self.marks_in(self.group) >= self.current().members.len() - 1 {
            self.flash = "at least one copy has to stay".into();
            return;
        }
        self.marked.insert(key);
    }

    /// Keep the highlighted copy, mark every other one that can be freed.
    pub fn keep_only_current(&mut self) {
        for position in 0..self.current().members.len() {
            let key = (self.group, position);
            if position == self.member || !self.current().members[position].deletable {
                self.marked.remove(&key);
            } else {
                self.marked.insert(key);
            }
        }
    }

    pub fn clear_group(&mut self) {
        self.marked.retain(|&(group, _)| group != self.group);
    }

    /// Apply a keep rule across every group at once, replacing the marks.
    ///
    /// The keeper comes from `Rule`, which is the same code `--auto` uses, so
    /// "keep the oldest" cannot come to mean one thing here and another there.
    pub fn apply_bulk(&mut self, rule: Rule) -> usize {
        self.marked.clear();
        for (index, group) in self.groups.iter().enumerate() {
            let members: Vec<(&str, f64)> = group
                .members
                .iter()
                .map(|member| (member.path.as_str(), member.mtime))
                .collect();
            let keeper = rule.keeper_of(&members);
            for (position, member) in group.members.iter().enumerate() {
                if position != keeper && member.deletable {
                    self.marked.insert((index, position));
                }
            }
        }
        self.marked.len()
    }

    /// Take one keystroke.
    pub fn on_key(&mut self, key: Key) -> Step {
        self.flash.clear();
        match self.mode {
            Mode::Help => self.help_key(key),
            Mode::Bulk => self.bulk_key(key),
            Mode::Confirm => self.confirm_key(key),
            Mode::Browse => self.browse_key(key),
        }
    }

    fn help_key(&mut self, key: Key) -> Step {
        if matches!(key, Key::Help | Key::Escape | Key::Quit) {
            self.mode = Mode::Browse;
        }
        Step::Stay
    }

    fn bulk_key(&mut self, key: Key) -> Step {
        let rule = match key {
            Key::Escape | Key::Quit => {
                self.mode = Mode::Browse;
                return Step::Stay;
            }
            Key::Digit(0) => {
                self.marked.clear();
                self.mode = Mode::Browse;
                self.flash = "all marks cleared".into();
                return Step::Stay;
            }
            Key::Digit(1) => Rule::Oldest,
            Key::Digit(2) => Rule::Newest,
            Key::Digit(3) => Rule::Shortest,
            _ => return Step::Stay,
        };
        let marked = self.apply_bulk(rule);
        self.mode = Mode::Browse;
        self.flash = format!(
            "{} files marked — keeping the {rule} copy",
            human_count(marked)
        );
        Step::Stay
    }

    fn confirm_key(&mut self, key: Key) -> Step {
        match key {
            Key::Yes => {
                self.mode = Mode::Browse;
                Step::Apply
            }
            Key::No | Key::Escape => {
                self.mode = Mode::Browse;
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    fn browse_key(&mut self, key: Key) -> Step {
        if self.groups.is_empty() {
            return if key == Key::Quit {
                Step::Quit
            } else {
                Step::Stay
            };
        }
        match key {
            Key::Quit => return Step::Quit,
            Key::Help => self.mode = Mode::Help,
            Key::Down => {
                self.member = (self.member + 1).min(self.current().members.len() - 1);
            }
            Key::Up => self.member = self.member.saturating_sub(1),
            Key::NextGroup => {
                if self.group + 1 < self.groups.len() {
                    self.group += 1;
                    self.member = 0;
                }
            }
            Key::PreviousGroup => {
                if self.group > 0 {
                    self.group -= 1;
                    self.member = 0;
                }
            }
            Key::FirstGroup => {
                self.group = 0;
                self.member = 0;
            }
            Key::LastGroup => {
                self.group = self.groups.len() - 1;
                self.member = 0;
            }
            Key::Toggle => self.toggle(),
            Key::KeepOnlyThis => self.keep_only_current(),
            Key::ClearGroup => self.clear_group(),
            Key::Bulk => self.mode = Mode::Bulk,
            Key::Apply => {
                if self.marked.is_empty() {
                    self.flash = "nothing marked yet".into();
                } else {
                    self.mode = Mode::Confirm;
                }
            }
            _ => {}
        }
        Step::Stay
    }

    /// Fold a round of deletion back in: drop what is gone, keep the tallies,
    /// and forget every mark, including the ones a refused group still holds.
    pub fn absorb(&mut self, outcome: Outcome) -> Step {
        self.applied_files += outcome.files;
        self.applied_bytes += outcome.bytes;
        self.refused += outcome.refused;
        self.failures.extend(outcome.failures);

        let gone: BTreeSet<usize> = outcome.removed.into_iter().collect();
        let mut kept = Vec::with_capacity(self.groups.len());
        for group in std::mem::take(&mut self.groups) {
            let members: Vec<Member> = group
                .members
                .into_iter()
                .filter(|member| !gone.contains(&member.record))
                .collect();
            // One copy left is not a duplicate any more.
            if members.len() > 1 {
                kept.push(Group {
                    index: group.index,
                    size: group.size,
                    members,
                });
            }
        }
        self.groups = kept;
        self.marked.clear();
        self.group = self.group.min(self.groups.len().saturating_sub(1));
        self.member = 0;
        self.flash = if outcome.refused > 0 {
            format!(
                "{} group(s) refused — their copies no longer match",
                human_count(outcome.refused)
            )
        } else {
            let verb = if self.hard { "deleted" } else { "moved" };
            format!("{verb} {} files", human_count(outcome.files))
        };
        if self.groups.is_empty() {
            Step::Quit
        } else {
            Step::Stay
        }
    }
}

/// Format a byte count the way a person would say it — the oracle's rounding,
/// including its switch to whole numbers at 100.
pub fn human_size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    for unit in ["KB", "MB", "GB", "TB", "PB"] {
        value /= 1024.0;
        if value < 1024.0 {
            return if value < 100.0 {
                format!("{value:.1} {unit}")
            } else {
                format!("{value:.0} {unit}")
            };
        }
    }
    format!("{value:.1} EB")
}

pub fn human_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Trim from the left so the end of a path — the filename — stays visible.
pub fn elide_left(text: &str, width: usize) -> String {
    if width <= 1 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= width {
        return text.to_string();
    }
    let skip = length - (width - 1);
    let mut out = String::from("…");
    out.extend(text.chars().skip(skip));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the marks point at, for tests that care about records rather than
    /// screen positions. The real path derives this inside `run_selected`.
    fn plan(browser: &Browser) -> Vec<usize> {
        browser
            .selection()
            .into_iter()
            .map(|(_, record)| record)
            .collect()
    }

    fn member(record: usize, path: &str, mtime: f64) -> Member {
        Member {
            record,
            path: path.into(),
            date: "2026-08-23".into(),
            extra_links: 0,
            deletable: true,
            mtime,
        }
    }

    /// Two groups of three, records numbered straight through.
    fn browser() -> Browser {
        Browser::new(
            vec![
                Group {
                    index: 0,
                    size: 1000,
                    members: vec![
                        member(0, "a.bin", 300.0),
                        member(1, "bb.bin", 100.0),
                        member(2, "ccc.bin", 200.0),
                    ],
                },
                Group {
                    index: 1,
                    size: 10,
                    members: vec![member(3, "d.bin", 5.0), member(4, "ee.bin", 9.0)],
                },
            ],
            "/scratch".into(),
            false,
        )
    }

    #[test]
    fn a_group_may_never_have_every_copy_marked() {
        let mut browser = browser();
        browser.toggle();
        browser.member = 1;
        browser.toggle();
        assert_eq!(browser.marked_count(), 2);

        // The third one is the last survivor, and the refusal says so.
        browser.member = 2;
        browser.toggle();
        assert_eq!(browser.marked_count(), 2, "the last copy was marked");
        assert_eq!(browser.flash, "at least one copy has to stay");

        // The guard is per group, not global: the other group is untouched.
        browser.group = 1;
        browser.member = 0;
        browser.toggle();
        assert_eq!(browser.marked_count(), 3);
    }

    #[test]
    fn the_guard_lets_the_mark_come_off_again() {
        let mut browser = browser();
        browser.toggle();
        browser.member = 1;
        browser.toggle();
        browser.member = 0;
        browser.toggle(); // unmark
        assert_eq!(browser.marked_count(), 1);
        assert!(!browser.is_marked(0, 0));
        // ...and now the third can be marked, because two would still leave one.
        browser.member = 2;
        browser.toggle();
        assert_eq!(browser.marked_count(), 2);
    }

    #[test]
    fn a_record_pinned_from_outside_the_tree_is_not_offered() {
        let mut browser = browser();
        browser.groups[0].members[1].deletable = false;
        browser.member = 1;
        browser.toggle();
        assert_eq!(browser.marked_count(), 0);
        assert_eq!(
            browser.flash,
            "hardlinked outside this tree — deleting it frees nothing"
        );

        // Keep-this leaves it alone as well, and it still counts as a survivor,
        // so both of the others may go.
        browser.member = 0;
        browser.keep_only_current();
        assert!(!browser.is_marked(0, 1), "pinned record was marked");
        assert!(browser.is_marked(0, 2));
        assert_eq!(browser.marked_count(), 1);
    }

    #[test]
    fn a_group_of_nothing_but_pinned_records_can_lose_nothing() {
        let mut browser = browser();
        for member in browser.groups[0].members.iter_mut() {
            member.deletable = false;
        }
        for position in 0..3 {
            browser.member = position;
            browser.toggle();
        }
        assert_eq!(browser.marked_count(), 0);
        browser.keep_only_current();
        assert_eq!(browser.marked_count(), 0);
        // A bulk rule still marks the other group, and still nothing here.
        browser.apply_bulk(Rule::Shortest);
        assert!(
            (0..3).all(|position| !browser.is_marked(0, position)),
            "a bulk rule marked a pinned record"
        );
        assert_eq!(plan(&browser), vec![4]);
    }

    #[test]
    fn keep_this_one_marks_the_rest_of_its_own_group_only() {
        let mut browser = browser();
        browser.member = 2;
        browser.keep_only_current();
        assert!(browser.is_marked(0, 0) && browser.is_marked(0, 1));
        assert!(!browser.is_marked(0, 2));
        assert_eq!(browser.marked_count(), 2, "the other group was touched");
    }

    #[test]
    fn clearing_a_group_leaves_the_other_groups_marks_alone() {
        let mut browser = browser();
        browser.apply_bulk(Rule::Shortest);
        assert_eq!(browser.marked_count(), 3);
        browser.clear_group();
        assert_eq!(browser.marked_count(), 1);
        assert!(browser.is_marked(1, 1));
    }

    #[test]
    fn every_bulk_rule_keeps_one_copy_in_every_group() {
        for rule in [Rule::Shortest, Rule::Oldest, Rule::Newest] {
            let mut browser = browser();
            browser.apply_bulk(rule);
            for (index, group) in browser.groups.iter().enumerate() {
                let marked = (0..group.members.len())
                    .filter(|&position| browser.is_marked(index, position))
                    .count();
                assert_eq!(
                    marked,
                    group.members.len() - 1,
                    "{rule} left the wrong number in group {index}"
                );
            }
        }
    }

    #[test]
    fn the_bulk_rules_keep_the_copy_they_name() {
        let mut browser = browser();
        browser.apply_bulk(Rule::Oldest);
        assert!(!browser.is_marked(0, 1), "bb.bin is the oldest");
        browser.apply_bulk(Rule::Newest);
        assert!(!browser.is_marked(0, 0), "a.bin is the newest");
        browser.apply_bulk(Rule::Shortest);
        assert!(!browser.is_marked(0, 0), "a.bin has the shortest name");
    }

    #[test]
    fn a_bulk_rule_replaces_the_marks_rather_than_adding_to_them() {
        let mut browser = browser();
        browser.member = 2;
        browser.keep_only_current(); // keeps ccc.bin
        browser.apply_bulk(Rule::Shortest); // keeps a.bin
        assert!(!browser.is_marked(0, 0));
        assert!(browser.is_marked(0, 2), "the old keeper is a victim now");
    }

    #[test]
    fn the_plan_names_records_not_screen_positions() {
        let mut browser = browser();
        browser.member = 1;
        browser.keep_only_current();
        browser.group = 1;
        browser.member = 0;
        browser.keep_only_current();
        assert_eq!(plan(&browser), vec![0, 2, 4]);
    }

    #[test]
    fn a_hardlink_cluster_is_one_row_and_goes_as_one_unit() {
        let mut browser = browser();
        browser.groups[0].members[0].extra_links = 2;
        browser.member = 1;
        browser.keep_only_current();
        // Three names, one record, one mark, one entry in the plan.
        assert_eq!(plan(&browser), vec![0, 2]);
        assert_eq!(browser.marked_count(), 2);
    }

    #[test]
    fn enter_with_nothing_marked_says_so_instead_of_confirming() {
        let mut browser = browser();
        assert_eq!(browser.on_key(Key::Apply), Step::Stay);
        assert_eq!(browser.mode, Mode::Browse);
        assert_eq!(browser.flash, "nothing marked yet");
    }

    #[test]
    fn deletion_needs_the_confirmation_and_nothing_else_will_do() {
        let mut browser = browser();
        browser.on_key(Key::Toggle);
        assert_eq!(browser.on_key(Key::Apply), Step::Stay);
        assert_eq!(browser.mode, Mode::Confirm);

        // Anything but y is not a yes.
        for key in [Key::Ignored, Key::Down, Key::Toggle, Key::Apply] {
            assert_eq!(browser.on_key(key), Step::Stay);
            assert_eq!(browser.mode, Mode::Confirm, "{key:?} left the dialog");
        }
        assert_eq!(browser.on_key(Key::No), Step::Stay);
        assert_eq!(browser.mode, Mode::Browse, "n did not cancel");
        assert_eq!(browser.marked_count(), 1, "cancelling threw the marks away");

        browser.on_key(Key::Apply);
        assert_eq!(browser.on_key(Key::Yes), Step::Apply);
    }

    #[test]
    fn escape_cancels_the_confirmation_too() {
        let mut browser = browser();
        browser.on_key(Key::Toggle);
        browser.on_key(Key::Apply);
        assert_eq!(browser.on_key(Key::Escape), Step::Stay);
        assert_eq!(browser.mode, Mode::Browse);
    }

    #[test]
    fn the_overlays_swallow_the_browse_keys() {
        let mut browser = browser();
        browser.on_key(Key::Help);
        assert_eq!(browser.mode, Mode::Help);
        browser.on_key(Key::Down);
        assert_eq!(browser.member, 0, "help let a movement key through");
        browser.on_key(Key::Toggle);
        assert_eq!(browser.marked_count(), 0, "help let a mark through");
        browser.on_key(Key::Help);
        assert_eq!(browser.mode, Mode::Browse);
    }

    #[test]
    fn quit_from_the_browser_leaves_and_from_an_overlay_only_closes_it() {
        let mut browser = browser();
        browser.on_key(Key::Bulk);
        assert_eq!(browser.on_key(Key::Quit), Step::Stay);
        assert_eq!(browser.mode, Mode::Browse);
        assert_eq!(browser.on_key(Key::Quit), Step::Quit);
    }

    #[test]
    fn the_bulk_menu_clears_every_mark_on_zero() {
        let mut browser = browser();
        browser.apply_bulk(Rule::Shortest);
        browser.on_key(Key::Bulk);
        browser.on_key(Key::Digit(0));
        assert_eq!(browser.marked_count(), 0);
        assert_eq!(browser.mode, Mode::Browse);
        assert_eq!(browser.flash, "all marks cleared");
    }

    #[test]
    fn movement_stops_at_the_ends_rather_than_wrapping() {
        let mut browser = browser();
        browser.on_key(Key::Up);
        assert_eq!(browser.member, 0);
        browser.on_key(Key::PreviousGroup);
        assert_eq!(browser.group, 0);
        for _ in 0..5 {
            browser.on_key(Key::Down);
        }
        assert_eq!(browser.member, 2);
        browser.on_key(Key::NextGroup);
        assert_eq!((browser.group, browser.member), (1, 0));
        for _ in 0..5 {
            browser.on_key(Key::NextGroup);
        }
        assert_eq!(browser.group, 1);
        browser.on_key(Key::LastGroup);
        assert_eq!((browser.group, browser.member), (1, 0));
        browser.on_key(Key::FirstGroup);
        assert_eq!((browser.group, browser.member), (0, 0));
    }

    #[test]
    fn a_group_that_drops_to_one_copy_stops_being_a_group() {
        let mut browser = browser();
        browser.member = 1;
        browser.keep_only_current(); // dooms records 0 and 2
        let step = browser.absorb(Outcome {
            files: 2,
            bytes: 2000,
            removed: vec![0, 2],
            refused: 0,
            failures: Vec::new(),
        });
        assert_eq!(step, Step::Stay);
        assert_eq!(browser.groups.len(), 1, "the emptied group stayed");
        assert_eq!(browser.groups[0].size, 10);
        assert_eq!(browser.applied_files, 2);
        assert_eq!(browser.applied_bytes, 2000);
        assert_eq!(browser.marked_count(), 0);
        assert_eq!(browser.flash, "moved 2 files");
    }

    #[test]
    fn a_mark_names_the_scans_group_even_after_the_list_has_renumbered() {
        // The first group empties out and disappears, so what the browser now
        // calls group 0 is the scan's group 1. A mark that carried the screen
        // position would name the wrong group here, and the wrong group is a
        // different file rather than a missing one.
        let mut browser = browser();
        browser.member = 0;
        browser.keep_only_current();
        browser.absorb(Outcome {
            files: 2,
            bytes: 2000,
            removed: vec![1, 2],
            refused: 0,
            failures: Vec::new(),
        });
        assert_eq!(browser.groups.len(), 1);
        assert_eq!(browser.group, 0, "the browser renumbered as expected");

        browser.member = 0;
        browser.keep_only_current();
        assert_eq!(
            browser.selection(),
            vec![(1, 4)],
            "the mark did not name the scan's own group and record"
        );
    }

    #[test]
    fn clearing_the_last_group_ends_the_session() {
        let mut browser = browser();
        browser.groups.truncate(1);
        browser.member = 0;
        browser.keep_only_current();
        let step = browser.absorb(Outcome {
            files: 2,
            bytes: 2000,
            removed: vec![1, 2],
            refused: 0,
            failures: Vec::new(),
        });
        assert_eq!(step, Step::Quit);
        assert!(browser.groups.is_empty());
        assert_eq!(browser.group, 0, "the cursor pointed past the end");
    }

    #[test]
    fn a_refused_group_keeps_its_copies_and_says_why() {
        let mut browser = browser();
        browser.member = 1;
        browser.keep_only_current();
        browser.absorb(Outcome {
            files: 0,
            bytes: 0,
            removed: Vec::new(),
            refused: 1,
            failures: Vec::new(),
        });
        assert_eq!(browser.groups.len(), 2, "a refusal removed something");
        assert_eq!(browser.groups[0].members.len(), 3);
        assert_eq!(browser.applied_files, 0);
        assert_eq!(browser.refused, 1);
        assert_eq!(
            browser.flash,
            "1 group(s) refused — their copies no longer match"
        );
    }

    #[test]
    fn what_the_marks_would_free_is_counted_per_mark() {
        let mut browser = browser();
        assert_eq!(browser.marked_bytes(), 0);
        browser.member = 0;
        browser.keep_only_current();
        assert_eq!(browser.marked_bytes(), 2000);
        browser.group = 1;
        browser.member = 0;
        browser.keep_only_current();
        assert_eq!(browser.marked_bytes(), 2010);
    }

    #[test]
    fn reclaimable_never_counts_the_copy_that_has_to_stay() {
        let mut browser = browser();
        assert_eq!(browser.reclaimable(), 2000 + 10);
        // A pinned record cannot be freed, so it drops out of the count.
        browser.groups[0].members[0].deletable = false;
        assert_eq!(browser.reclaimable(), 1000 + 10);
        // And a group with nothing freeable reclaims nothing rather than
        // underflowing on the subtraction.
        browser.groups[1].members[0].deletable = false;
        browser.groups[1].members[1].deletable = false;
        assert_eq!(browser.reclaimable(), 1000);
    }

    #[test]
    fn sizes_read_the_way_the_oracle_says_them() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        // Past 100 of a unit the decimal stops earning its place.
        assert_eq!(human_size(100 * 1024), "100 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn counts_are_grouped_in_threes() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1,000");
        assert_eq!(human_count(1234567), "1,234,567");
    }

    #[test]
    fn a_path_too_long_to_fit_keeps_its_filename() {
        assert_eq!(elide_left("short", 10), "short");
        assert_eq!(elide_left("abcdefghij", 10), "abcdefghij");
        assert_eq!(elide_left("abcdefghijk", 10), "…cdefghijk");
        assert_eq!(elide_left("abc", 1), "");
        assert_eq!(elide_left("abc", 0), "");
        // Counted in characters, not bytes, or a multi-byte path would be cut
        // to the wrong width and split a character while it was at it.
        assert_eq!(elide_left("αβγδε", 3), "…δε");
    }
}
