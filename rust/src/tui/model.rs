//! The browser's state, with no terminal anywhere in it.
//!
//! Everything a keystroke does — moving, marking, the survivor guard, the bulk
//! rules, and the plan that comes out the far end — is decided here, so it can
//! be tested by calling functions instead of by driving a terminal. The paint
//! layer reads this and never writes to it.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::folders;
use crate::size::{counted, human_count};
use crate::trash::Rule;

/// Browser ids for folder groups start here, above every file group's id.
///
/// A file group is named by its position in the scan's list (or its arrival
/// number on the live path), and a folder group by its position in the folder
/// list, offset by this. The two spaces never meet, so a mark, a cursor and the
/// id index can all treat every row alike. Nothing here is ever a real index.
pub const FOLDER_BASE: usize = 1 << 40;

/// Below this the columns stop meaning anything and the paint layer draws a
/// resize message instead of the browser.
///
/// It lives here rather than beside the painting because it is not only a
/// layout number: what is on screen decides what the browser will act on, and
/// hard rule 3 says nothing is removed without the count, the bytes and the
/// mode being shown. The two must agree, so they read the same constant.
pub const MIN_WIDTH: u16 = 54;
pub const MIN_HEIGHT: u16 = 12;

/// What one mark names: a group in the scan and a record in the scan.
///
/// Named fields rather than a bare pair, because the two are both `usize` and
/// nothing but their order distinguished them. Positions never appear here —
/// see `Browser::selection`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Mark {
    pub group: usize,
    pub record: usize,
}

/// One member of a group as the browser shows it.
///
/// It carries a copy of what the screen needs rather than borrowing the scan,
/// so a test can build one without a filesystem behind it. `record` is the only
/// link back: it indexes the scan's records, and it is what deletion acts on.
pub struct Member {
    pub record: usize,
    /// The scan's own name for this record, relative to the scanned root.
    ///
    /// Two strings name this file and they are not interchangeable. This is the
    /// one every decision uses, because it is the one `--auto` ranks; `path` is
    /// built for the screen and its shape depends on where the user happens to
    /// be standing.
    pub name: String,
    /// What to draw. Never compared, never ranked, never a keeper.
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

/// What a folder group carries beyond its rows.
pub struct FolderRow {
    /// Files in one copy, and bytes in one copy: the header says both.
    pub files: usize,
    pub bytes: u64,
    /// Each folder's newest entry, in nanoseconds, parallel to the members. The
    /// bulk rules rank folders by this through `folders::keep_index`, which is
    /// the same function `--auto` uses.
    pub ages: Vec<i64>,
    /// The file groups this folder group folds. Shown again when it is unfolded,
    /// and when an apply refuses it.
    pub folded: Vec<Group>,
}

pub struct Group {
    /// Where this group sits in the scan's own list. It never changes, even
    /// once a round of deletion has renumbered what the browser shows, and it
    /// is half of what a mark names.
    ///
    /// On the live path it is assigned in the order groups settle, so it is
    /// also the identity a streaming insertion is anchored to: a group that
    /// arrives later may be drawn above this one, and neither the cursor nor
    /// any mark may move because of it.
    pub index: usize,
    /// Bytes a removal of one copy frees: the group's size. For a folder group,
    /// the bytes that would be freed by moving one folder, which excludes a
    /// hardlink whose other name is outside the folder.
    pub size: u64,
    pub members: Vec<Member>,
    /// Present for a folder group, whose members are folders and not files.
    pub folder: Option<FolderRow>,
    /// Whether the bytes behind this group have actually been compared.
    ///
    /// False means candidate: the narrowing stages agree, which is a very
    /// strong hint and not a proof. M8-LIVE.md defers the byte proof to the
    /// moment of deletion, so a candidate is shown honestly as one and is
    /// proved — or split, and refused — before anything of it is trashed.
    pub proven: bool,
}

impl Group {
    /// What removing every freeable copy but one would give back.
    ///
    /// The live list is ordered by this rather than by size, because it is
    /// what the user is actually shopping for: a hundred copies of a small
    /// file is a better find than two copies of a slightly bigger one.
    pub fn reclaimable(&self) -> u64 {
        let freeable = self
            .members
            .iter()
            .filter(|member| member.deletable)
            .count();
        self.size * freeable.saturating_sub(1) as u64
    }

    /// The total order the live list is kept in: most reclaimable first, then
    /// the tie-breaks `pipeline::sort_groups` uses, so a streamed list and a
    /// finished one agree about what comes first. Every key is a property of
    /// the group itself — nothing here depends on arrival order, which is what
    /// makes an insertion stable rather than merely appended.
    fn order(&self) -> (std::cmp::Reverse<u64>, std::cmp::Reverse<u64>, &str, &str) {
        let keeper = &self.members[0];
        (
            std::cmp::Reverse(self.reclaimable()),
            std::cmp::Reverse(self.size),
            basename(&keeper.name),
            &keeper.name,
        )
    }
}

/// The last path component, for the ordering tie-break.
fn basename(name: &str) -> &str {
    name.rsplit_once('/').map_or(name, |(_, base)| base)
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
    /// Replace the highlighted folder group with the file groups it folds, for
    /// the rest of the session.
    Unfold,
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
    /// Folders moved, as marks: the folder group and the folder's position.
    pub folders_moved: Vec<Mark>,
    /// Folder groups refused, by browser id. Their folded groups come back.
    pub refused_folders: Vec<usize>,
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
    /// Whether the frame is big enough for the browser to have been drawn.
    ///
    /// The paint layer gives up below `MIN_WIDTH × MIN_HEIGHT` and draws a
    /// resize message, and a confirmation that was never drawn cannot have
    /// been read. Rather than let the model act on a screen that does not
    /// exist, it stops taking orders until there is one.
    legible: bool,
    /// Marks, keyed by `(the group's stable id, the member's position in that
    /// group)` — never by the group's position on screen.
    ///
    /// The live list inserts new groups above old ones, so a screen position
    /// names a different group from one frame to the next. A group's id and a
    /// member's position within it are both fixed for the group's whole life,
    /// which is what lets a scan keep publishing underneath a user who is part
    /// way through marking.
    marked: BTreeSet<(usize, usize)>,
    /// Where each group id currently sits in `groups`. Rebuilt whenever the
    /// list changes, which is the only time it can go stale.
    at: HashMap<usize, usize>,
    pub applied_files: usize,
    pub applied_bytes: u64,
    pub refused: usize,
    pub failures: Vec<String>,
    /// What the scan is doing, or `None` once it has stopped. Read from the
    /// live scan's shared state; the browser never computes it.
    pub progress: Option<Progress>,
    /// True while a scan is still running behind the browser.
    pub scanning: bool,
    /// Cloud placeholders this scan did not read. Kept on the browser rather
    /// than on `progress`, because `progress` goes away when the scan ends and
    /// this fact does not stop being true when it does.
    pub in_cloud: u64,
    /// True when the scan stopped because the user asked it to.
    pub cancelled: bool,
    /// Folder groups the user has unfolded, by the first folder's path. Kept
    /// for the session, so a rebuilt list does not fold them up again.
    unfolded: HashSet<String>,
}

/// A snapshot of what the scan is doing, for the browser to draw.
///
/// Every number in here is read out of a counter the scan keeps anyway — the
/// same counters the stderr pulse reads. Nothing is estimated, and nothing on
/// this path touches the disk: M7's sweep is built on "one disk, one queue",
/// and a progress bar that cost a read would be a second reader.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub label: &'static str,
    pub done: u64,
    pub total: u64,
    /// Bytes the walk has seen, or bytes this reading stage has moved.
    pub bytes: u64,
    /// Bytes a second over the last `RATE_WINDOW`, never over the stage's
    /// lifetime. See `pulse::Rate`.
    pub rate: u64,
    /// The file most recently opened, or empty during the walk, which opens
    /// none. This is what a person staring at a bar that has stopped moving
    /// needs to know, and it is the whole reason M9 puts it on the screen.
    pub current: String,
}

impl Browser {
    pub fn new(groups: Vec<Group>, root: String, hard: bool) -> Self {
        let mut browser = Browser {
            groups,
            group: 0,
            member: 0,
            mode: Mode::Browse,
            flash: String::new(),
            root,
            hard,
            // Assumed until the first frame says otherwise, so a browser built
            // in a test behaves like one on a terminal anybody could use.
            legible: true,
            marked: BTreeSet::new(),
            at: HashMap::new(),
            applied_files: 0,
            applied_bytes: 0,
            refused: 0,
            failures: Vec::new(),
            progress: None,
            in_cloud: 0,
            scanning: false,
            cancelled: false,
            unfolded: HashSet::new(),
        };
        browser.reindex();
        browser
    }

    /// Rebuild the id-to-position index. Called from everywhere `groups`
    /// changes shape and nowhere else.
    fn reindex(&mut self) {
        self.at.clear();
        for (position, group) in self.groups.iter().enumerate() {
            self.at.insert(group.index, position);
        }
    }

    /// The stable id of the group at a screen position.
    fn id_at(&self, position: usize) -> usize {
        self.groups[position].index
    }

    /// Add a group the scan has just settled, keeping the list in order and
    /// the cursor on whatever it was already on.
    ///
    /// The cursor is re-found by identity afterwards rather than adjusted by
    /// arithmetic: an insertion above it shifts every position below by one,
    /// and "add one if the insertion was above me" is the kind of reasoning
    /// that is right until a group is inserted at exactly the cursor.
    pub fn insert(&mut self, group: Group) {
        let anchor = self.groups.get(self.group).map(|group| group.index);
        let at = self
            .groups
            .partition_point(|existing| existing.order() < group.order());
        self.groups.insert(at, group);
        self.reindex();
        // Back onto the group the user was looking at. A cursor that had
        // nothing under it (an empty list) stays where it was.
        if let Some(anchor) = anchor {
            if let Some(&position) = self.at.get(&anchor) {
                self.group = position;
            }
        }
    }

    /// The key an unfolded folder group is remembered by.
    fn folder_key(group: &Group) -> String {
        group.members[0].name.clone()
    }

    /// Put the rows in the order the list is kept in: most reclaimable first.
    /// Used once, when a finished scan is turned into rows.
    pub fn arrange(&mut self) {
        self.groups
            .sort_by(|left, right| left.order().cmp(&right.order()));
        self.reindex();
    }

    /// Put a group in the list in its order, whatever it is.
    ///
    /// A folder group that the user has unfolded is not put back: the file
    /// groups it folds go in instead, as they were before it folded them.
    fn place(&mut self, mut group: Group) {
        if group.folder.is_some() && self.unfolded.contains(&Self::folder_key(&group)) {
            let folded = group
                .folder
                .as_mut()
                .map(|row| std::mem::take(&mut row.folded))
                .unwrap_or_default();
            for file_group in folded {
                if !self
                    .groups
                    .iter()
                    .any(|existing| existing.index == file_group.index)
                {
                    self.place(file_group);
                }
            }
            return;
        }
        let at = self
            .groups
            .partition_point(|existing| existing.order() < group.order());
        self.groups.insert(at, group);
        self.reindex();
    }

    /// Every mark's file, by name, and every marked folder, by path, as the
    /// survivor law wants them.
    fn names_marked<'a>(
        marked: &BTreeSet<(usize, usize)>,
        groups: &'a [Group],
        at: &HashMap<usize, usize>,
    ) -> (HashSet<&'a str>, HashMap<&'a str, ()>) {
        let mut files = HashSet::new();
        let mut folders = HashMap::new();
        for &(id, member) in marked {
            let Some(&position) = at.get(&id) else {
                continue;
            };
            let Some(member) = groups[position].members.get(member) else {
                continue;
            };
            if id >= FOLDER_BASE {
                folders.insert(member.name.as_str(), ());
            } else {
                files.insert(member.name.as_str());
            }
        }
        (files, folders)
    }

    /// The first group, by screen position, that a selection would leave with no
    /// copy on disk, or `None` if every group keeps one.
    ///
    /// This is the browser's copy of the survivor law, and it is the same law:
    /// the function that decides it is `folders::first_without_copy`, which the
    /// scan's own guard also calls. Only the names differ — the browser has no
    /// records, so it asks by name, which is the same question for the names a
    /// group shows.
    fn strands(&self, marked: &BTreeSet<(usize, usize)>) -> Option<usize> {
        let (files, folders) = Self::names_marked(marked, &self.groups, &self.at);
        let alive: Vec<Vec<bool>> = self
            .groups
            .iter()
            .map(|group| {
                group
                    .members
                    .iter()
                    .map(|member| {
                        !files.contains(member.name.as_str())
                            && folders::within(&member.name, &folders).is_none()
                    })
                    .collect()
            })
            .collect();
        folders::first_without_copy(&alive)
    }

    /// Whether a file lies inside a folder that is marked: it goes with it.
    fn goes_with_a_folder(&self, name: &str) -> bool {
        let (_, folders) = Self::names_marked(&self.marked, &self.groups, &self.at);
        folders::within(name, &folders).is_some()
    }

    /// What the marks would remove, split the way the confirmation names it:
    /// folders, the files those folders hold, and loose files.
    pub fn marked_split(&self) -> (usize, usize, usize) {
        let (mut folders, mut folder_files, mut files) = (0, 0, 0);
        for &(id, _) in &self.marked {
            let Some(&position) = self.at.get(&id) else {
                continue;
            };
            if id >= FOLDER_BASE {
                folders += 1;
                folder_files += self.groups[position]
                    .folder
                    .as_ref()
                    .map_or(0, |row| row.files);
            } else {
                files += 1;
            }
        }
        (folders, folder_files, files)
    }

    /// The paths of the marked folders, for the screen to say which files go
    /// with them.
    pub fn folders_marked(&self) -> Vec<&str> {
        self.marked
            .iter()
            .filter(|(id, _)| *id >= FOLDER_BASE)
            .filter_map(|&(id, member)| {
                let &position = self.at.get(&id)?;
                self.groups[position]
                    .members
                    .get(member)
                    .map(|m| m.name.as_str())
            })
            .collect()
    }

    pub fn is_marked(&self, group: usize, member: usize) -> bool {
        self.marked.contains(&(self.id_at(group), member))
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
            .filter_map(|&(id, _)| self.at.get(&id))
            .map(|&position| self.groups[position].size)
            .sum()
    }

    /// The groups a deletion would have to prove first: the marked ones that
    /// have not had their bytes compared.
    pub fn unproven_marked(&self) -> usize {
        self.marked
            .iter()
            .filter_map(|&(id, _)| self.at.get(&id))
            .filter(|&&position| !self.groups[position].proven)
            .collect::<BTreeSet<_>>()
            .len()
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
        let id = self.id_at(group);
        self.marked.range((id, 0)..(id + 1, 0)).count()
    }

    /// The marks as `(group in the scan, record in the scan)` pairs.
    ///
    /// Screen positions never leave this module. They move whenever a round of
    /// deletion drops a group or a member — and, on the live path, whenever
    /// the scan inserts a group above the one being looked at. A stale
    /// position handed to the remover would name a different file rather than
    /// an absent one.
    /// A mark whose member position is past the end of its group names
    /// nothing and is dropped rather than panicked on. That is a BACKSTOP and
    /// not the guard: `absorb_found` clears every mark whenever the scan
    /// rebuilds the list, so a mark can only be out of range if some future
    /// path rebuilds without going through it. The guard is there; this is
    /// here because the cost of being wrong on the deletion path is a panic on
    /// the browser thread, and a panic on the browser thread used to hang the
    /// process (REVIEW-M8 item 1, both halves).
    pub fn selection(&self) -> Vec<Mark> {
        self.marked
            .iter()
            .filter_map(|&(id, member)| {
                let &position = self.at.get(&id)?;
                Some(Mark {
                    group: id,
                    record: self.groups[position].members.get(member)?.record,
                })
            })
            .collect()
    }

    fn current(&self) -> &Group {
        &self.groups[self.group]
    }

    /// File marks that lie inside a folder being marked go with the folder, so
    /// they are not also counted as separate removals.
    fn settle(&self, marked: &mut BTreeSet<(usize, usize)>) {
        let (_, folders) = Self::names_marked(marked, &self.groups, &self.at);
        if folders.is_empty() {
            return;
        }
        let inside: Vec<(usize, usize)> = self
            .groups
            .iter()
            .filter(|group| group.folder.is_none())
            .flat_map(|group| {
                group
                    .members
                    .iter()
                    .enumerate()
                    .filter(|(_, member)| folders::within(&member.name, &folders).is_some())
                    .map(move |(position, _)| (group.index, position))
            })
            .collect();
        for key in inside {
            marked.remove(&key);
        }
    }

    /// Mark or unmark the highlighted member.
    ///
    /// Refusals, all of them the oracle's or the survivor law's: a record pinned
    /// from outside the tree frees nothing; a group may never have every copy
    /// marked; a file inside a marked folder goes with the folder; and no mark
    /// may leave any group with no copy. The count the first two compare is the
    /// count without the new mark, and the law is checked on the marks with it.
    pub fn toggle(&mut self) {
        let key = (self.id_at(self.group), self.member);
        if self.marked.remove(&key) {
            return;
        }
        let member = &self.current().members[self.member];
        if self.current().folder.is_none() && self.goes_with_a_folder(&member.name) {
            self.flash = "it goes with its folder — unmark the folder to keep it".into();
            return;
        }
        if !member.deletable {
            self.flash = "hardlinked outside this tree — deleting it frees nothing".into();
            return;
        }
        if self.marks_in(self.group) >= self.current().members.len() - 1 {
            self.flash = "at least one copy has to stay".into();
            return;
        }
        let mut next = self.marked.clone();
        next.insert(key);
        self.settle(&mut next);
        if let Some(position) = self.strands(&next) {
            self.flash = format!(
                "that would leave group {} with no copy",
                human_count(position as u64 + 1)
            );
            return;
        }
        self.marked = next;
    }

    /// Keep the highlighted copy, mark every other one that can be freed.
    pub fn keep_only_current(&mut self) {
        let id = self.id_at(self.group);
        let mut next = self.marked.clone();
        for position in 0..self.current().members.len() {
            let key = (id, position);
            if position == self.member || !self.current().members[position].deletable {
                next.remove(&key);
            } else {
                next.insert(key);
            }
        }
        self.settle(&mut next);
        if let Some(position) = self.strands(&next) {
            self.flash = format!(
                "that would leave group {} with no copy",
                human_count(position as u64 + 1)
            );
            return;
        }
        self.marked = next;
    }

    pub fn clear_group(&mut self) {
        let id = self.id_at(self.group);
        self.marked.retain(|&(group, _)| group != id);
    }

    /// Apply a keep rule across every group at once, replacing the marks.
    ///
    /// Folder groups are decided first, one folder kept per group by
    /// `folders::keep_index` — the function `--auto` uses. A file group then
    /// keeps the copy the rule names among the copies that do not go with a
    /// marked folder, so the two halves cannot strand each other. Whatever the
    /// result, the survivor law is checked before the marks are replaced.
    pub fn apply_bulk(&mut self, rule: Rule) -> Result<usize, String> {
        let mut next = BTreeSet::new();
        for group in self.groups.iter() {
            let Some(row) = group.folder.as_ref() else {
                continue;
            };
            let names: Vec<&str> = group
                .members
                .iter()
                .map(|member| member.name.as_str())
                .collect();
            let keeper = folders::keep_index(rule, &names, &row.ages);
            for (position, member) in group.members.iter().enumerate() {
                if position != keeper && member.deletable {
                    next.insert((group.index, position));
                }
            }
        }
        // What the folder choices move, and what they keep. A file inside a kept
        // folder is the keeper if there is one, as `--auto` makes it, so the bulk
        // rule never removes a copy out of a folder it is keeping.
        let (_, moved) = Self::names_marked(&next, &self.groups, &self.at);
        let mut kept: HashMap<&str, ()> = HashMap::new();
        for group in self.groups.iter().filter(|group| group.folder.is_some()) {
            for (position, member) in group.members.iter().enumerate() {
                if !next.contains(&(group.index, position)) {
                    kept.insert(member.name.as_str(), ());
                }
            }
        }
        for group in self.groups.iter().filter(|group| group.folder.is_none()) {
            let candidates: Vec<usize> = group
                .members
                .iter()
                .enumerate()
                .filter(|(_, member)| folders::within(&member.name, &moved).is_none())
                .map(|(position, _)| position)
                .collect();
            let sheltered: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|&position| folders::within(&group.members[position].name, &kept).is_some())
                .collect();
            if !sheltered.is_empty() {
                for &position in &candidates {
                    let member = &group.members[position];
                    if !sheltered.contains(&position) && member.deletable {
                        next.insert((group.index, position));
                    }
                }
                continue;
            }
            let pairs: Vec<(&str, f64)> = candidates
                .iter()
                .map(|&position| {
                    let member = &group.members[position];
                    (member.name.as_str(), member.mtime)
                })
                .collect();
            if pairs.is_empty() {
                continue;
            }
            let keeper = candidates[rule.keeper_of(&pairs)];
            for &position in &candidates {
                let member = &group.members[position];
                if position != keeper && member.deletable {
                    next.insert((group.index, position));
                }
            }
        }
        self.settle(&mut next);
        if let Some(position) = self.strands(&next) {
            return Err(format!(
                "the {rule} rule would leave group {} with no copy, so nothing was marked",
                human_count(position as u64 + 1)
            ));
        }
        let marked = next.len();
        self.marked = next;
        Ok(marked)
    }

    /// Tell the model how big the last frame was.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.legible = width >= MIN_WIDTH && height >= MIN_HEIGHT;
    }

    pub fn legible(&self) -> bool {
        self.legible
    }

    /// Take one keystroke.
    pub fn on_key(&mut self, key: Key) -> Step {
        self.flash.clear();
        // A screen nobody can read takes no orders but the one to leave. This
        // is hard rule 3 in the corner where the paint layer has given up: in
        // `Mode::Confirm` on a frame too small to lay out, the dialog is not on
        // screen, so `y` must not be a deletion. Movement and marking are
        // refused with it — acting on a list that was never drawn is the same
        // mistake in a cheaper suit — and the resize message the user *can*
        // see says what to do about it.
        if !self.legible {
            return if key == Key::Quit {
                Step::Quit
            } else {
                Step::Stay
            };
        }
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
        self.mode = Mode::Browse;
        self.flash = match self.apply_bulk(rule) {
            Ok(marked) => format!(
                "{} files marked — keeping the {rule} copy",
                human_count(marked as u64)
            ),
            Err(why) => why,
        };
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
            Key::Unfold => self.unfold(),
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

    /// Replace a folder group with the file groups it folds, for the session.
    ///
    /// The cursor lands on the first of them, and the marks on the folder group
    /// go with it.
    pub fn unfold(&mut self) {
        let position = self.group;
        let id = self.groups[position].index;
        let Some(row) = self.groups[position].folder.as_mut() else {
            self.flash = "only a folder group can be unfolded".into();
            return;
        };
        let folded = std::mem::take(&mut row.folded);
        let count = folded.len();
        self.unfolded
            .insert(Self::folder_key(&self.groups[position]));
        self.marked.retain(|&(group, _)| group != id);
        self.groups.remove(position);
        let first = folded.first().map(|group| group.index);
        for group in folded {
            if !self
                .groups
                .iter()
                .any(|existing| existing.index == group.index)
            {
                self.place(group);
            }
        }
        self.reindex();
        self.group = first
            .and_then(|first| self.at.get(&first).copied())
            .unwrap_or(position.min(self.groups.len().saturating_sub(1)));
        self.member = 0;
        self.flash = format!("unfolded — {} groups of files", human_count(count as u64));
    }

    /// Fold a round of deletion back in: drop what is gone, keep the tallies,
    /// and forget every mark, including the ones a refused group still holds.
    ///
    /// A folder group that lost folders keeps its other folders and loses the
    /// file groups it folded, which named the folders that moved. A folder group
    /// the apply refused comes back apart: its folded file groups are visible
    /// again, as the scan would show them.
    pub fn absorb(&mut self, outcome: Outcome) -> Step {
        self.applied_files += outcome.files;
        self.applied_bytes += outcome.bytes;
        self.refused += outcome.refused;
        self.failures.extend(outcome.failures);

        let moved: HashSet<(usize, usize)> = outcome
            .folders_moved
            .iter()
            .map(|mark| (mark.group, mark.record))
            .collect();
        let refused: HashSet<usize> = outcome.refused_folders.into_iter().collect();
        let mut moved_paths: Vec<String> = Vec::new();
        for group in &self.groups {
            if group.folder.is_some() {
                for (position, member) in group.members.iter().enumerate() {
                    if moved.contains(&(group.index, position)) {
                        moved_paths.push(member.name.clone());
                    }
                }
            }
        }
        let moved_map: HashMap<&str, ()> =
            moved_paths.iter().map(|path| (path.as_str(), ())).collect();
        let gone: BTreeSet<usize> = outcome.removed.into_iter().collect();
        // The folder groups the apply refused, named by their first folder, read
        // before the list is rebuilt.
        let refused_names: Vec<String> = self
            .groups
            .iter()
            .filter(|group| refused.contains(&group.index))
            .map(|group| format!("{}/", group.members[0].name))
            .collect();

        let mut kept = Vec::with_capacity(self.groups.len());
        let mut restored = Vec::new();
        for group in std::mem::take(&mut self.groups) {
            let Group {
                index,
                size,
                members,
                folder,
                proven,
            } = group;
            if let Some(mut row) = folder {
                if refused.contains(&index) {
                    restored.extend(std::mem::take(&mut row.folded));
                    continue;
                }
                let keep: Vec<bool> = (0..members.len())
                    .map(|position| !moved.contains(&(index, position)))
                    .collect();
                let mut ages = Vec::new();
                let mut remaining = Vec::new();
                for (position, member) in members.into_iter().enumerate() {
                    if keep[position] {
                        ages.push(row.ages[position]);
                        remaining.push(member);
                    }
                }
                // One folder left is not a duplicate, and the file groups it
                // folded named folders that have gone.
                if remaining.len() > 1 {
                    kept.push(Group {
                        index,
                        size,
                        members: remaining,
                        folder: Some(FolderRow {
                            files: row.files,
                            bytes: row.bytes,
                            ages,
                            folded: Vec::new(),
                        }),
                        proven,
                    });
                }
                continue;
            }
            let members: Vec<Member> = members
                .into_iter()
                .filter(|member| {
                    !gone.contains(&member.record)
                        && folders::within(&member.name, &moved_map).is_none()
                })
                .collect();
            // One copy left is not a duplicate any more.
            if members.len() > 1 {
                kept.push(Group {
                    index,
                    size,
                    members,
                    folder: None,
                    proven,
                });
            }
        }
        self.groups = kept;
        self.reindex();
        for group in restored {
            self.place(group);
        }
        self.reindex();
        self.marked.clear();
        self.group = self.group.min(self.groups.len().saturating_sub(1));
        self.member = 0;
        self.flash = if let Some(first) = refused_names.first() {
            format!(
                "folder group at {first} refused — its files no longer match the copy beside it"
            )
        } else if outcome.refused > 0 {
            format!(
                "{} group(s) refused — their copies no longer match",
                human_count(outcome.refused as u64)
            )
        } else {
            let verb = if self.hard { "deleted" } else { "moved" };
            format!("{verb} {}", counted(outcome.files as u64, "file", "files"))
        };
        // An empty list ends the session — unless a scan is still running
        // behind it, where empty only means "nothing found YET" and quitting
        // would throw away the rest of the scan the user is waiting on.
        if self.groups.is_empty() && !self.scanning {
            Step::Quit
        } else {
            Step::Stay
        }
    }

    /// Fold a round of the scan's findings in.
    ///
    /// `replaced` says the scan rebuilt the list rather than adding to it,
    /// which is what happens after a deletion: the scan owns the records, so
    /// it is the side that decides what is left.
    ///
    /// A folder group arriving on its own removes the file groups it folds from
    /// the list. A cursor on one of those lands on the folder group instead.
    pub fn absorb_found(&mut self, found: Vec<Group>, replaced: bool) {
        if replaced {
            let anchor = self.groups.get(self.group).map(|group| group.index);
            self.groups.clear();
            self.reindex();
            for group in found {
                self.place(group);
            }
            // A cursor on a group that is now folded into a folder group lands
            // on that folder group, the same rule as an arrival.
            let target = anchor.and_then(|anchor| {
                if self.at.contains_key(&anchor) {
                    return Some(anchor);
                }
                self.groups
                    .iter()
                    .find(|group| {
                        group.folder.as_ref().is_some_and(|row| {
                            row.folded.iter().any(|folded| folded.index == anchor)
                        })
                    })
                    .map(|group| group.index)
            });
            self.group = target
                .and_then(|target| self.at.get(&target).copied())
                .unwrap_or(0)
                .min(self.groups.len().saturating_sub(1));
            self.member = 0;
            // Every mark goes, which is the rule `absorb` states: a rebuilt list
            // only ever arrives after a proof or a deletion, and both rewrite
            // membership. A mark that survived would name a different file.
            self.marked.clear();
            return;
        }
        for mut group in found {
            let anchor = self.groups.get(self.group).map(|group| group.index);
            if group.folder.is_none() {
                self.insert(group);
                continue;
            }
            let key = Self::folder_key(&group);
            if self.unfolded.contains(&key) {
                let folded = group
                    .folder
                    .as_mut()
                    .map(|row| std::mem::take(&mut row.folded))
                    .unwrap_or_default();
                for file_group in folded {
                    if !self
                        .groups
                        .iter()
                        .any(|existing| existing.index == file_group.index)
                    {
                        self.place(file_group);
                    }
                }
                continue;
            }
            let folded_ids: HashSet<usize> = group
                .folder
                .as_ref()
                .map(|row| row.folded.iter().map(|folded| folded.index).collect())
                .unwrap_or_default();
            let cursor_was_folded = anchor.is_some_and(|anchor| folded_ids.contains(&anchor));
            self.groups
                .retain(|existing| !folded_ids.contains(&existing.index));
            self.marked.retain(|&(id, _)| !folded_ids.contains(&id));
            self.reindex();
            let folder_id = group.index;
            self.group = match anchor {
                Some(anchor) if !cursor_was_folded => self.at.get(&anchor).copied().unwrap_or(0),
                _ => 0,
            };
            self.insert(group);
            if cursor_was_folded {
                if let Some(&position) = self.at.get(&folder_id) {
                    self.group = position;
                }
            }
        }
    }
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
            .map(|mark| mark.record)
            .collect()
    }

    /// A member whose two names agree, which is the ordinary case.
    fn member(record: usize, name: &str, mtime: f64) -> Member {
        shown_as(record, name, name, mtime)
    }

    /// A member whose display string is not its scan name — what happens when
    /// the user runs the browser from inside the tree being scanned.
    fn shown_as(record: usize, name: &str, path: &str, mtime: f64) -> Member {
        Member {
            record,
            name: name.into(),
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
                    proven: true,
                    members: vec![
                        member(0, "a.bin", 300.0),
                        member(1, "bb.bin", 100.0),
                        member(2, "ccc.bin", 200.0),
                    ],

                    folder: None,
                },
                Group {
                    index: 1,
                    size: 10,
                    proven: true,
                    members: vec![member(3, "d.bin", 5.0), member(4, "ee.bin", 9.0)],

                    folder: None,
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
        browser.apply_bulk(Rule::Shortest).unwrap();
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
        browser.apply_bulk(Rule::Shortest).unwrap();
        assert_eq!(browser.marked_count(), 3);
        browser.clear_group();
        assert_eq!(browser.marked_count(), 1);
        assert!(browser.is_marked(1, 1));
    }

    #[test]
    fn every_bulk_rule_keeps_one_copy_in_every_group() {
        for rule in [Rule::Shortest, Rule::Oldest, Rule::Newest] {
            let mut browser = browser();
            browser.apply_bulk(rule).unwrap();
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
        browser.apply_bulk(Rule::Oldest).unwrap();
        assert!(!browser.is_marked(0, 1), "bb.bin is the oldest");
        browser.apply_bulk(Rule::Newest).unwrap();
        assert!(!browser.is_marked(0, 0), "a.bin is the newest");
        browser.apply_bulk(Rule::Shortest).unwrap();
        assert!(!browser.is_marked(0, 0), "a.bin has the shortest name");
    }

    #[test]
    fn a_bulk_rule_ranks_the_scans_name_not_the_one_on_screen() {
        // Standing in `tree/photos`, the browser draws `b.bin` as its absolute
        // path — it is not under the working directory — while `photos/aaaa.bin`
        // draws as the bare `aaaa.bin`. So the two orders disagree: by the
        // scan's names `b.bin` is shortest, by what is on screen `aaaa.bin` is.
        // `--auto shortest` keeps `b.bin`, and so must this.
        let mut browser = Browser::new(
            vec![Group {
                index: 0,
                size: 100,
                proven: true,
                members: vec![
                    shown_as(0, "b.bin", "/scratch/tree/b.bin", 1.0),
                    shown_as(1, "photos/aaaa.bin", "aaaa.bin", 2.0),
                ],

                folder: None,
            }],
            "/scratch/tree".into(),
            false,
        );
        assert_eq!(browser.apply_bulk(Rule::Shortest).unwrap(), 1);
        assert!(
            !browser.is_marked(0, 0),
            "the shortest name in the scan was marked for deletion"
        );
        assert_eq!(plan(&browser), vec![1]);

        // The same group with the display strings swapped keeps the same copy,
        // which is the whole point: what is drawn cannot change what dies.
        let mut swapped = Browser::new(
            vec![Group {
                index: 0,
                size: 100,
                proven: true,
                members: vec![
                    shown_as(0, "b.bin", "b.bin", 1.0),
                    shown_as(1, "photos/aaaa.bin", "photos/aaaa.bin", 2.0),
                ],

                folder: None,
            }],
            "/scratch/tree".into(),
            false,
        );
        swapped.apply_bulk(Rule::Shortest).unwrap();
        assert_eq!(plan(&swapped), plan(&browser));
    }

    #[test]
    fn keep_this_one_and_the_survivor_guard_do_not_read_the_display_string() {
        // The other two ways a copy gets marked, on the same disagreeing group.
        let mut browser = Browser::new(
            vec![Group {
                index: 0,
                size: 100,
                proven: true,
                members: vec![
                    shown_as(0, "b.bin", "/scratch/tree/b.bin", 1.0),
                    shown_as(1, "photos/aaaa.bin", "aaaa.bin", 2.0),
                ],

                folder: None,
            }],
            "/scratch/tree".into(),
            false,
        );
        browser.keep_only_current();
        assert_eq!(
            plan(&browser),
            vec![1],
            "keep-this looked at the wrong name"
        );

        browser.member = 1;
        browser.toggle(); // unmark record 1
        browser.toggle(); // and back on
        assert_eq!(plan(&browser), vec![1]);
        browser.member = 0;
        browser.toggle();
        assert_eq!(plan(&browser), vec![1], "both copies were marked");
        assert_eq!(browser.flash, "at least one copy has to stay");
    }

    #[test]
    fn a_screen_too_small_to_read_takes_no_orders_but_quit() {
        let mut browser = browser();
        browser.member = 0;
        browser.keep_only_current();
        browser.on_key(Key::Apply);
        assert_eq!(browser.mode, Mode::Confirm);

        // The frame shrinks under the confirmation. The paint layer has stopped
        // drawing it, so `y` must stop being a deletion.
        browser.resize(20, 8);
        assert!(!browser.legible());
        assert_eq!(browser.on_key(Key::Yes), Step::Stay, "y deleted unseen");
        assert_eq!(browser.mode, Mode::Confirm, "the dialog was dismissed");

        // Nothing else moves either, and the marks are still there to act on.
        for key in [Key::Down, Key::Toggle, Key::Bulk, Key::Apply, Key::Help] {
            assert_eq!(browser.on_key(key), Step::Stay, "{key:?} was acted on");
        }
        assert_eq!(browser.member, 0);
        assert_eq!(browser.marked_count(), 2);
        assert_eq!(browser.mode, Mode::Confirm);

        // Quitting always works — it is the one thing the resize message asks
        // the user to be able to do without seeing anything.
        assert_eq!(browser.on_key(Key::Quit), Step::Quit);

        // Back at a legible size, the confirmation is live again.
        browser.resize(MIN_WIDTH, MIN_HEIGHT);
        assert!(browser.legible());
        assert_eq!(browser.on_key(Key::Yes), Step::Apply);
    }

    #[test]
    fn the_legibility_floor_is_the_one_the_paint_layer_uses() {
        let mut browser = browser();
        browser.resize(MIN_WIDTH, MIN_HEIGHT);
        assert!(browser.legible(), "the minimum itself must be legible");
        browser.resize(MIN_WIDTH - 1, MIN_HEIGHT);
        assert!(!browser.legible());
        browser.resize(MIN_WIDTH, MIN_HEIGHT - 1);
        assert!(!browser.legible());
    }

    #[test]
    fn a_bulk_rule_replaces_the_marks_rather_than_adding_to_them() {
        let mut browser = browser();
        browser.member = 2;
        browser.keep_only_current(); // keeps ccc.bin
        browser.apply_bulk(Rule::Shortest).unwrap(); // keeps a.bin
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
        browser.apply_bulk(Rule::Shortest).unwrap();
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

            ..Default::default()
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

            ..Default::default()
        });
        assert_eq!(browser.groups.len(), 1);
        assert_eq!(browser.group, 0, "the browser renumbered as expected");

        browser.member = 0;
        browser.keep_only_current();
        assert_eq!(
            browser.selection(),
            vec![Mark {
                group: 1,
                record: 4
            }],
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

            ..Default::default()
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

            ..Default::default()
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

    // ---------------------------------------------------------- the live list

    /// A group of `copies` identical files of `size`, with a stable id.
    fn streamed(id: usize, size: u64, copies: usize, proven: bool) -> Group {
        Group {
            index: id,
            size,
            proven,
            members: (0..copies)
                .map(|position| member(id * 100 + position, &format!("g{id}/m{position}.bin"), 1.0))
                .collect(),
            folder: None,
        }
    }

    #[test]
    fn a_streamed_list_is_ordered_by_what_it_would_give_back() {
        // Arrival order is deliberately the reverse of the answer, so a list
        // that merely appended would fail this outright.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 10, 2, false)); //  10 reclaimable
        browser.insert(streamed(1, 100, 2, false)); // 100
        browser.insert(streamed(2, 50, 4, false)); // 150
        let order: Vec<usize> = browser.groups.iter().map(|group| group.index).collect();
        assert_eq!(order, vec![2, 1, 0]);
        assert_eq!(
            browser.groups[0].reclaimable(),
            150,
            "a small file in many copies outranks a big one in two"
        );
    }

    #[test]
    fn a_group_arriving_above_the_cursor_does_not_move_the_cursor() {
        // M8-LIVE.md: "a new group may insert above, the cursor must stay on
        // the group it was on".
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 100, 2, false));
        browser.insert(streamed(1, 50, 2, false));
        browser.group = 1; // sitting on the smaller group, id 1
        assert_eq!(browser.groups[browser.group].index, 1);

        // Something bigger than both arrives and is drawn at the top.
        browser.insert(streamed(2, 1000, 2, false));
        assert_eq!(
            browser.groups[0].index, 2,
            "the big group was not drawn first"
        );
        assert_eq!(
            browser.groups[browser.group].index, 1,
            "the cursor slid off the group it was on"
        );

        // ...and one that sorts below the cursor does not move it either.
        browser.insert(streamed(3, 1, 2, false));
        assert_eq!(browser.groups[browser.group].index, 1);
    }

    #[test]
    fn marks_survive_a_group_being_inserted_above_them() {
        // The mark is keyed by the group's id, so an insertion that shifts
        // every screen position below it must not repoint it. A mark that
        // followed a position would name a different file here — and for this
        // program that means deleting something nobody chose.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        // The group that gets marked must sit at a screen position that is
        // NOT its own id, or a mark keyed the wrong way would still land on
        // the right group by coincidence and this test would pass while
        // testing nothing. Id 1 reclaims more, so id 0 is drawn second.
        browser.insert(streamed(0, 100, 3, false));
        browser.insert(streamed(1, 300, 2, false));
        let marking = browser.at[&0];
        assert_eq!(marking, 1, "the fixture stopped putting id 0 at position 1");
        assert_ne!(
            marking, 0,
            "the marked group's position equals its id — the mis-keying this \
             test exists for would be invisible"
        );

        browser.group = marking;
        browser.member = 1;
        browser.toggle();
        let before = browser.selection();
        assert_eq!(before.len(), 1);
        assert_eq!(
            before[0].group, 0,
            "the mark named the group at the cursor's POSITION, not the group"
        );

        for id in 2..7 {
            browser.insert(streamed(id, 10_000, 2, false));
        }
        assert_eq!(browser.groups.len(), 7);
        assert_ne!(
            browser.at[&0], marking,
            "the marked group did not actually move on screen"
        );
        assert_eq!(
            browser.selection(),
            before,
            "the mark followed a screen position instead of its group"
        );
        assert!(
            browser.is_marked(browser.at[&0], 1),
            "the mark is not on the row it was put on"
        );
        // ...and nothing else got marked in the shuffle.
        assert_eq!(browser.marked_count(), 1);
    }

    #[test]
    fn a_streamed_group_is_a_candidate_until_something_proves_it() {
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 100, 2, false));
        browser.insert(streamed(1, 100, 2, true));
        assert!(!browser.groups.iter().find(|g| g.index == 0).unwrap().proven);
        assert!(browser.groups.iter().find(|g| g.index == 1).unwrap().proven);

        // Marking the unproven one is what makes a proof owed at apply time.
        browser.group = browser.at[&0];
        browser.member = 0;
        browser.toggle();
        assert_eq!(browser.unproven_marked(), 1);
        browser.group = browser.at[&1];
        browser.member = 0;
        browser.toggle();
        assert_eq!(
            browser.unproven_marked(),
            1,
            "a proven group was counted as owing a proof"
        );
    }

    #[test]
    fn an_empty_list_ends_the_session_only_once_the_scan_has_stopped() {
        // Mid-scan, "nothing found" means "nothing found yet"; quitting there
        // would throw away the rest of the scan the user is waiting on.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 100, 2, true));
        browser.scanning = true;
        browser.member = 0;
        browser.keep_only_current();
        let step = browser.absorb(Outcome {
            files: 1,
            bytes: 100,
            removed: vec![1],
            refused: 0,
            failures: Vec::new(),

            ..Default::default()
        });
        assert_eq!(step, Step::Stay, "the browser quit while the scan ran");

        browser.scanning = false;
        browser.insert(streamed(1, 100, 2, true));
        browser.group = 0;
        browser.member = 0;
        browser.keep_only_current();
        let step = browser.absorb(Outcome {
            files: 1,
            bytes: 100,
            removed: vec![100, 101],
            refused: 0,
            failures: Vec::new(),

            ..Default::default()
        });
        assert_eq!(step, Step::Quit);
    }

    #[test]
    fn a_rebuilt_list_keeps_the_cursor_and_drops_marks_on_groups_that_went() {
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        for id in 0..3 {
            browser.insert(streamed(id, 100 * (id as u64 + 1), 2, false));
        }
        browser.group = browser.at[&1];
        browser.member = 0;
        browser.toggle();
        assert_eq!(browser.marked_count(), 1);

        // The scan rebuilt the list after a deletion and group 1 is gone.
        browser.absorb_found(
            vec![streamed(0, 100, 2, true), streamed(2, 300, 2, true)],
            true,
        );
        assert_eq!(browser.groups.len(), 2);
        assert_eq!(
            browser.marked_count(),
            0,
            "a mark survived on a group that no longer exists"
        );
        assert!(browser.groups.iter().all(|group| group.proven));
        assert!(
            browser.group < browser.groups.len(),
            "the cursor points past the end"
        );
    }

    #[test]
    fn a_mark_does_not_survive_a_group_being_rebuilt_smaller() {
        // REVIEW-M8 item 1, the reviewer's own reproducer: mark member 3 of a
        // four-member candidate, absorb a replaced list in which that group
        // has two members, then ask for the selection.
        //
        // Before the fix this panicked — `absorb_found` pruned marks by group
        // id only, the group was not GONE but SMALLER, and `selection()`
        // indexed `members[3]` of a two-member group. On the browser thread,
        // where that panic lands, it did not end the process; it hung it.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 1000, 4, false));
        browser.group = 0;
        browser.member = 3;
        browser.toggle();
        assert_eq!(
            browser.marked_count(),
            1,
            "the fixture never marked anything"
        );
        assert_eq!(
            browser.selection(),
            vec![Mark {
                group: 0,
                record: 3
            }],
            "the fixture marked the wrong member"
        );

        // The proof failed and the scan republished the group as its first
        // real byte-equality class: same id, two members.
        browser.absorb_found(vec![streamed(0, 1000, 2, true)], true);
        assert_eq!(browser.groups.len(), 1);
        assert_eq!(browser.groups[0].members.len(), 2);

        // The mark named a member that is no longer in the group. It is gone,
        // and asking for the selection returns rather than panics.
        assert_eq!(
            browser.selection(),
            Vec::new(),
            "a mark survived a rebuild that changed what its group is made of"
        );
        assert_eq!(browser.marked_count(), 0);
    }

    #[test]
    fn a_rebuild_clears_marks_on_groups_it_did_not_touch_either() {
        // The rule is `absorb`'s — forget every mark — and not "forget the
        // ones that stopped being valid". A rebuilt list only ever arrives
        // after a proof or a deletion, and both rewrite membership: a mark
        // left standing on an untouched group would be a mark the user set
        // against a list that no longer exists.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 1000, 3, false));
        browser.insert(streamed(1, 900, 3, false));
        browser.group = browser.at[&1];
        browser.member = 0;
        browser.toggle();
        assert_eq!(browser.marked_count(), 1);

        // Group 0 came apart; group 1 was not touched.
        browser.absorb_found(
            vec![streamed(0, 1000, 2, true), streamed(1, 900, 3, false)],
            true,
        );
        assert_eq!(browser.marked_count(), 0, "a mark outlived the rebuild");
        assert_eq!(browser.groups.len(), 2, "the rebuild lost a group");
    }

    #[test]
    fn a_mark_out_of_range_is_dropped_rather_than_panicked_on() {
        // The backstop, reached directly. `absorb_found` makes this
        // unreachable through the ordinary path, so it is provoked by
        // shrinking a group underneath a mark — which is what any future
        // republishing path that forgot to clear would do.
        let mut browser = Browser::new(Vec::new(), "/scratch".into(), false);
        browser.insert(streamed(0, 1000, 4, false));
        browser.group = 0;
        browser.member = 3;
        browser.toggle();
        browser.groups[0].members.truncate(2);
        assert_eq!(
            browser.selection(),
            Vec::new(),
            "an out-of-range mark reached the deletion path"
        );
    }

    #[test]
    fn insertion_puts_a_group_in_the_same_place_however_it_arrived() {
        // The list's order must be a property of the groups, not of the order
        // the scan happened to settle them in — otherwise two runs of the same
        // scan would draw different lists.
        let shapes = [(0_usize, 100_u64, 2), (1, 50, 4), (2, 400, 2), (3, 10, 9)];
        let mut forwards = Browser::new(Vec::new(), "/s".into(), false);
        for &(id, size, copies) in shapes.iter() {
            forwards.insert(streamed(id, size, copies, false));
        }
        let mut backwards = Browser::new(Vec::new(), "/s".into(), false);
        for &(id, size, copies) in shapes.iter().rev() {
            backwards.insert(streamed(id, size, copies, false));
        }
        let one: Vec<usize> = forwards.groups.iter().map(|g| g.index).collect();
        let two: Vec<usize> = backwards.groups.iter().map(|g| g.index).collect();
        assert_eq!(one, two, "arrival order leaked into the list order");
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

#[cfg(test)]
mod folder_groups {
    //! The folder side of the browser, with no terminal anywhere in it: rows,
    //! marks, the guard, unfolding, the cursor when a folder group arrives, the
    //! bulk rules, and what an apply hands back.

    use super::*;

    /// A file member at a record number, shown under its own name.
    fn file_member(record: usize, name: &str, mtime: f64) -> Member {
        Member {
            record,
            name: name.into(),
            path: name.into(),
            date: "2026-08-23".into(),
            extra_links: 0,
            deletable: true,
            mtime,
        }
    }

    /// A file group, its records numbered from `index * 100`.
    fn file_group(index: usize, size: u64, names: &[&str]) -> Group {
        Group {
            index,
            size,
            proven: true,
            folder: None,
            members: names
                .iter()
                .enumerate()
                .map(|(position, name)| file_member(index * 100 + position, name, 1.0))
                .collect(),
        }
    }

    /// A folder group. `ages` are each folder's newest entry in nanoseconds, and
    /// `folded` are the file groups it stands for.
    fn folder_row(id: usize, paths: &[&str], ages: &[i64], size: u64, folded: Vec<Group>) -> Group {
        Group {
            index: FOLDER_BASE + id,
            size,
            proven: false,
            folder: Some(FolderRow {
                files: 2,
                bytes: size * 2,
                ages: ages.to_vec(),
                folded,
            }),
            members: paths
                .iter()
                .enumerate()
                .map(|(position, path)| Member {
                    record: position,
                    name: (*path).into(),
                    path: format!("{path}/"),
                    date: "2026-08-23".into(),
                    extra_links: 0,
                    deletable: true,
                    mtime: ages[position] as f64 / 1e9,
                })
                .collect(),
        }
    }

    fn browser(groups: Vec<Group>) -> Browser {
        Browser::new(groups, "/root".into(), false)
    }

    fn press(browser: &mut Browser, group: usize, member: usize, key: Key) {
        browser.group = group;
        browser.member = member;
        browser.on_key(key);
    }

    #[test]
    fn a_marked_folder_is_named_by_its_position_and_its_group() {
        let folded = vec![file_group(7, 10, &["A/f", "B/f"])];
        let mut browser = browser(vec![folder_row(0, &["A", "B"], &[1, 2], 10, folded)]);
        press(&mut browser, 0, 1, Key::Toggle);
        assert_eq!(
            browser.selection(),
            vec![Mark {
                group: FOLDER_BASE,
                record: 1
            }]
        );
        assert_eq!(
            browser.marked_split(),
            (1, 2, 0),
            "one folder, two files in it"
        );
    }

    #[test]
    fn a_folder_group_keeps_at_least_one_folder_like_a_file_group() {
        let mut browser = browser(vec![folder_row(0, &["A", "B"], &[1, 2], 10, Vec::new())]);
        press(&mut browser, 0, 0, Key::Toggle);
        press(&mut browser, 0, 1, Key::Toggle);
        assert_eq!(browser.marked_count(), 1, "the second folder was refused");
        assert!(
            browser.flash.contains("at least one copy"),
            "{}",
            browser.flash
        );
    }

    #[test]
    fn a_file_inside_a_marked_folder_goes_with_it_and_cannot_be_marked_alone() {
        let mut browser = browser(vec![
            folder_row(0, &["A", "B"], &[1, 2], 10, Vec::new()),
            file_group(3, 5, &["A/f", "B/f", "C/f"]),
        ]);
        press(&mut browser, 0, 1, Key::Toggle);
        press(&mut browser, 1, 1, Key::Toggle);
        assert_eq!(browser.marked_count(), 1);
        assert!(
            browser.flash.contains("goes with its folder"),
            "{}",
            browser.flash
        );
    }

    #[test]
    fn marking_a_folder_takes_the_file_marks_inside_it_with_it() {
        let mut browser = browser(vec![
            folder_row(0, &["A", "B"], &[1, 2], 10, Vec::new()),
            file_group(3, 5, &["A/f", "B/f", "C/f"]),
        ]);
        press(&mut browser, 1, 1, Key::Toggle);
        assert_eq!(browser.marked_count(), 1, "B/f is marked");
        press(&mut browser, 0, 1, Key::Toggle);
        assert_eq!(
            browser.selection(),
            vec![Mark {
                group: FOLDER_BASE,
                record: 1
            }],
            "the folder took B/f, which is not counted twice"
        );
    }

    #[test]
    fn the_guard_refuses_the_cross_group_trap() {
        // Folder B is marked. A/f is then marked, which is fine: C/f is still a
        // copy. C/f would leave {A/f, B/f, C/f} with nothing on disk, so it is
        // refused, and the mark on A/f stays.
        let mut browser = browser(vec![
            folder_row(0, &["A", "B"], &[1, 2], 10, Vec::new()),
            file_group(3, 5, &["A/f", "B/f", "C/f"]),
        ]);
        press(&mut browser, 0, 1, Key::Toggle);
        press(&mut browser, 1, 0, Key::Toggle);
        assert_eq!(browser.marked_count(), 2);
        press(&mut browser, 1, 2, Key::Toggle);
        assert_eq!(browser.marked_count(), 2, "the trap mark was taken");
        assert!(browser.flash.contains("no copy"), "{}", browser.flash);
    }

    #[test]
    fn the_guard_agrees_with_the_scan_side_law() {
        // The browser's answer and `folders::first_without_copy` must match on
        // the same alive pattern, since both are the one definition of the law.
        let alive = vec![vec![true, true], vec![false, false], vec![true]];
        assert_eq!(folders::first_without_copy(&alive), Some(1));
        let browser = browser(vec![file_group(3, 5, &["A/f", "B/f"])]);
        let mut marked = BTreeSet::new();
        marked.insert((3, 0));
        assert_eq!(browser.strands(&marked), None, "one copy still stands");
        marked.insert((3, 1));
        assert_eq!(browser.strands(&marked), Some(0), "both copies would go");
    }

    #[test]
    fn unfolding_replaces_the_folder_group_and_puts_the_cursor_on_its_first_file_group() {
        let folded = vec![
            file_group(3, 5, &["A/f", "B/f"]),
            file_group(4, 4, &["A/g", "B/g"]),
        ];
        let mut browser = browser(vec![folder_row(0, &["A", "B"], &[1, 2], 10, folded)]);
        assert_eq!(browser.groups.len(), 1);
        browser.on_key(Key::Unfold);
        let indexes: Vec<usize> = browser.groups.iter().map(|group| group.index).collect();
        assert_eq!(indexes, vec![3, 4]);
        assert_eq!(
            browser.groups[browser.group].index, 3,
            "the cursor lands on the first"
        );
        assert!(browser.groups.iter().all(|group| group.folder.is_none()));
    }

    #[test]
    fn an_unfolded_folder_group_stays_unfolded_when_the_list_is_rebuilt() {
        let mut browser = browser(vec![folder_row(
            0,
            &["A", "B"],
            &[1, 2],
            10,
            vec![file_group(3, 5, &["A/f", "B/f"])],
        )]);
        browser.on_key(Key::Unfold);
        browser.absorb_found(
            vec![folder_row(
                0,
                &["A", "B"],
                &[1, 2],
                10,
                vec![file_group(3, 5, &["A/f", "B/f"])],
            )],
            true,
        );
        assert_eq!(browser.groups.len(), 1);
        assert_eq!(
            browser.groups[0].index, 3,
            "the rebuilt list folded it back up"
        );
    }

    #[test]
    fn a_folder_group_arriving_folds_the_file_groups_it_covers_and_the_cursor_follows() {
        let mut browser = browser(vec![
            file_group(1, 100, &["X/f", "Y/f"]),
            file_group(2, 50, &["A/f", "B/f"]),
        ]);
        browser.group = 1;
        browser.absorb_found(
            vec![folder_row(
                0,
                &["A", "B"],
                &[1, 2],
                40,
                vec![file_group(2, 50, &["A/f", "B/f"])],
            )],
            false,
        );
        let indexes: Vec<usize> = browser.groups.iter().map(|group| group.index).collect();
        assert!(
            !indexes.contains(&2),
            "the folded file group is gone: {indexes:?}"
        );
        assert_eq!(
            browser.groups[browser.group].index, FOLDER_BASE,
            "the cursor followed it"
        );
    }

    #[test]
    fn a_cursor_on_a_file_group_stays_on_it_when_a_folder_group_arrives_above() {
        let mut browser = browser(vec![
            file_group(1, 10, &["X/f", "Y/f"]),
            file_group(2, 5, &["Q/f", "R/f"]),
        ]);
        browser.group = 1;
        browser.absorb_found(
            vec![folder_row(
                0,
                &["A", "B"],
                &[1, 2],
                400,
                vec![file_group(9, 1, &["A/z", "B/z"])],
            )],
            false,
        );
        assert_eq!(
            browser.groups[0].index, FOLDER_BASE,
            "the folder group sorts first"
        );
        assert_eq!(
            browser.groups[browser.group].index, 2,
            "the cursor did not move"
        );
    }

    #[test]
    fn a_bulk_rule_keeps_the_folder_it_names_and_never_marks_a_file_in_a_kept_folder() {
        // Folder B is the newest folder, so Newest moves A. The file group has
        // B/f inside the kept folder: it is the keeper, and C/f goes.
        let mut browser = browser(vec![
            folder_row(0, &["A", "B"], &[1, 9], 10, Vec::new()),
            file_group(3, 5, &["A/f", "B/f", "C/f"]),
        ]);
        browser
            .apply_bulk(Rule::Newest)
            .expect("a rule that keeps every group");
        let marked: Vec<(usize, usize)> = browser.marked.iter().copied().collect();
        assert_eq!(
            marked,
            vec![(3, 2), (FOLDER_BASE, 0)],
            "C/f goes, and A is moved"
        );
    }

    #[test]
    fn a_bulk_rule_on_folders_follows_the_same_keeper_as_the_scan() {
        let folders = ["A", "B", "C"];
        let ages = [5_i64, 1, 9];
        for (rule, keep) in [(Rule::Shortest, 0), (Rule::Oldest, 1), (Rule::Newest, 2)] {
            let mut browser = browser(vec![folder_row(0, &folders, &ages, 10, Vec::new())]);
            browser.apply_bulk(rule).expect("a legal bulk rule");
            let kept = (0..3)
                .find(|position| !browser.is_marked(0, *position))
                .expect("one folder stays");
            assert_eq!(kept, keep, "{rule}");
        }
    }

    #[test]
    fn a_moved_folder_leaves_its_row_and_the_file_groups_it_folded() {
        let mut browser = browser(vec![folder_row(
            0,
            &["A", "B"],
            &[1, 2],
            10,
            vec![file_group(3, 5, &["A/f", "B/f"])],
        )]);
        let step = browser.absorb(Outcome {
            files: 2,
            bytes: 10,
            folders_moved: vec![Mark {
                group: FOLDER_BASE,
                record: 1,
            }],
            ..Default::default()
        });
        assert_eq!(
            step,
            Step::Quit,
            "one folder left is not a duplicate: the list is empty"
        );
        assert!(browser.groups.is_empty());
    }

    #[test]
    fn a_refused_folder_group_comes_back_apart_and_says_which_folder() {
        let mut browser = browser(vec![folder_row(
            0,
            &["A", "B"],
            &[1, 2],
            10,
            vec![file_group(3, 5, &["A/f", "B/f"])],
        )]);
        browser.scanning = true;
        browser.absorb(Outcome {
            refused: 1,
            refused_folders: vec![FOLDER_BASE],
            ..Default::default()
        });
        let indexes: Vec<usize> = browser.groups.iter().map(|group| group.index).collect();
        assert_eq!(indexes, vec![3], "the file group is visible again");
        assert!(browser.flash.contains("A/"), "{}", browser.flash);
        assert!(browser.flash.contains("refused"), "{}", browser.flash);
    }

    #[test]
    fn a_folder_that_moved_keeps_the_others_in_its_group() {
        let mut browser = browser(vec![folder_row(
            0,
            &["A", "B", "C"],
            &[1, 2, 3],
            10,
            vec![file_group(3, 5, &["A/f", "B/f", "C/f"])],
        )]);
        browser.absorb(Outcome {
            folders_moved: vec![Mark {
                group: FOLDER_BASE,
                record: 1,
            }],
            ..Default::default()
        });
        assert_eq!(
            browser.groups.len(),
            1,
            "A and C still duplicate each other"
        );
        let names: Vec<&str> = browser.groups[0]
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["A", "C"]);
        assert!(browser.groups[0]
            .folder
            .as_ref()
            .is_some_and(|row| row.folded.is_empty()));
    }

    /// The browser's guard asks by name, and the scan's law asks by record. A
    /// record can wear several names, so the two could differ. This enumerates
    /// the small shapes that can differ and every selection over them, and checks
    /// the one direction that matters: a selection the browser allows is never a
    /// selection the record law forbids.
    #[test]
    fn the_browser_guard_never_allows_what_the_record_law_forbids() {
        use crate::folders::{stranded, Folder, FolderGroup, Listing};
        use crate::pipeline::DupGroup;
        use crate::walk::FileRec;

        let record = |names: &[&str]| FileRec {
            names: names.iter().map(|name| (*name).to_string()).collect(),
            size: 10,
            dev: 1,
            ino: 1,
            nlink: names.len() as u64,
            mtime_ns: 0,
            retained: None,
        };
        let folder = |path: &str| FolderGroup {
            folders: vec![
                Folder {
                    path: path.to_string(),
                    listing: Listing::default(),
                },
                Folder {
                    path: "B".to_string(),
                    listing: Listing::default(),
                },
            ],
            attested: false,
        };

        // r0 wears a canonical name and maybe one more; r1 is one name. The
        // canonical name is the first, as the scan sorts them.
        let first_names = ["A/x", "C/x"];
        let extra_names = [None, Some("A/z"), Some("C/z")];
        let second_names = ["A/q", "C/q"];
        let mut checked = 0;
        let mut browser_stricter = 0;
        for first in first_names {
            for extra in extra_names {
                for second in second_names {
                    let mut names = vec![first];
                    names.extend(extra);
                    let records = vec![record(&names), record(&[second])];
                    let groups = vec![DupGroup {
                        members: vec![0, 1],
                        attested: false,
                    }];
                    let folders = vec![folder("A")];

                    let browser = Browser::new(
                        vec![
                            folder_row(0, &["A", "B"], &[1, 2], 10, Vec::new()),
                            Group {
                                index: 5,
                                size: 10,
                                proven: true,
                                folder: None,
                                members: vec![
                                    file_member(0, first, 1.0),
                                    file_member(1, second, 1.0),
                                ],
                            },
                        ],
                        "/root".into(),
                        false,
                    );
                    // Every selection over: the folder A, record 0, record 1.
                    for bits in 0..8_u8 {
                        let mut marked = BTreeSet::new();
                        let mut removed = HashSet::new();
                        let mut moved = HashMap::new();
                        if bits & 1 != 0 {
                            marked.insert((FOLDER_BASE, 0));
                            moved.insert("A", 0_usize);
                        }
                        if bits & 2 != 0 {
                            marked.insert((5, 0));
                            removed.insert(0);
                        }
                        if bits & 4 != 0 {
                            marked.insert((5, 1));
                            removed.insert(1);
                        }
                        let by_name = browser.strands(&marked);
                        let by_record =
                            stranded(&records, &groups, &folders, &removed, &moved, &|_| true);
                        checked += 1;
                        assert!(
                            by_name.is_some() || by_record.is_none(),
                            "the browser allowed {names:?} + {second}, bits {bits:03b}, \
                             which the record law forbids: {by_record:?}"
                        );
                        if by_name.is_some() && by_record.is_none() {
                            browser_stricter += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 0);
        // The browser is stricter in one corner: a canonical name inside a moved
        // folder, with a surviving second name outside it. Refusing there costs a
        // selection, never a file.
        assert!(
            browser_stricter > 0,
            "the corner this test exists for never arose"
        );
    }
}
