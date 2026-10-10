# M10 — identical folders, as one group

Commissioned 2026-10-10. The maintainer browsed a scan where two folders
were complete copies of each other, 200 large files apiece, and the
browser made them step through 200 groups of two to say one thing:
"this folder is a copy of that one". By default, dedup now finds whole
folders that are exact copies and shows each set as ONE group whose
members are the folders. Marking a folder trashes the folder. Claim as
M10 in `rust/MILESTONES.md` (short: what was built, gate output, test
counts). This milestone runs light: one builder, maintainer checks at
each phase boundary and a hands-on end-to-end run before merge, no
separate review document. Land it as reviewable commits, not one
avalanche.

Build it in two phases, and stop for the maintainer after each:

1. Detection, folding, the generalized survivor law, `--list`,
   `--auto`, `--report-json`, `--no-folders`, trash for directories,
   unit tests, freeze gate, conformance.
2. The browser: finished-scan and live paths, unfold, confirm overlay,
   bulk rules, snapshot tests, README.

Proving 200 files identical still means reading 200 files; the bytes
law does not bend for folders. What this milestone saves is the
user's time, not the scan's. Do not claim otherwise anywhere.

## A. What "identical folders" means

Two directories P and Q, neither containing the other and neither the
scan root, are identical when — recursively, looking at EVERYTHING
under them, not at what the scan's filters let through:

- they hold the same set of relative file paths, compared as exact
  byte strings (no case folding, no Unicode normalization);
- each pair of regular files at the same relative path is byte-for-byte
  identical;
- each pair of symlinks at the same relative path has the same target
  string (never followed);
- `.DS_Store` files are ignored on both sides, at every depth. Nothing
  else is special-cased (maintainer's decision; do not add `._*`,
  `Icon\r`, `Thumbs.db` or a flag for it);
- empty directories are ignored: one copy holding an empty `tmp/` the
  other lacks loses no data when trashed. Say so in help and README;
- anything else under either side — a FIFO, socket, device, an entry
  that cannot be listed or read, a cloud placeholder when `--cloud`
  is off — disqualifies the pair. Never guess about a folder.

The names of P and Q themselves do not matter (`Photos` and
`Photos copy` are the canonical case). Identity is an equivalence, so
three or more copies form one folder group.

### Everything counts — the safety reason

Trashing a folder trashes everything in it, so every file in it must
have its twin in the kept folder — including files under `--min-size`,
files hidden by `-x`, `--ext` and `--skip-ext`, and files the scan never
looked at. Small files get compared too (they are small; that is the
point of the floor). A folder pair that differs only in one 3 KB
`notes.txt` is not a folder group, and its large files stay ordinary
file groups exactly as today.

Cloud placeholders: the M9 law stands — a default scan never
materializes a byte. A folder containing a dataless file is never
identical without `--cloud`. Hardlinks: a file whose other links lie
outside the folder keeps the folder markable (its data survives in the
twin), but the reclaimable size must count only bytes actually freed.

### Where candidates come from — a constraint, not a design

Candidate pairs are SEEDED from the file groups the scan already finds:
if `A/x/f` and `B/x/f` are in one group, `(A/x, B/x)` and `(A, B)` are
candidates — walking up while the stripped components match, the
folder's own name excluded. A pair of folders holding only files under
`--min-size` therefore is never found. That is the accepted limit of
this milestone (no hashing every small file in the tree); document it.
The builder chooses the data structure; a tree with no duplicate folder
candidates must pay nothing measurable (see the fence).

Only MAXIMAL folder groups are shown: when `A ≡ B`, the pair
`(A/sub, B/sub)` is implied, not listed.

## B. Folding — how file groups and folder groups coexist

One rule, applied to both file groups and nested folder groups: a group
is FOLDED into folder group F when every one of its members lies inside
a folder of F, exactly one member per folder. Folded groups are not
shown (they are what F says). Every other group stays visible with ALL
its members — including members that live inside folders of some
folder group — so a file with a third copy at `C/f` is still offered
as `{A/f, B/f, C/f}`.

The survivor law (REWRITE.md §3.2) is generalized, and enforced by the
code at apply, not by the UI: after the selection — marked folders
expanded to every file under them — EVERY group, visible or folded,
file or folder, keeps at least one intact surviving member. A selection
that breaks it is refused whole, naming the group, exactly like
`run_selected` today. The UI guard must agree with it (a file row that
lies inside a marked folder shows as going with the folder), but the
code check is the law.

## C. The browser

- Folder groups sit in the same list, ordered by the same rule as file
  groups (largest reclaimable first; the live insertion and
  cursor-stability rules of M8 apply). They are visibly folders: rows
  are the folder paths with a trailing `/`, and the header says how
  many files and how many bytes each copy holds.
- Marking, `a`, `u`, and the bulk rules work on folder groups. For
  `oldest` / `newest`, a folder's age is the builder's choice of a
  deterministic definition over the files inside (stated in LOG.md with
  one reason); ties fall to shortest path, then lexicographic.
- An UNFOLD action on a folder group replaces it, for the rest of the
  session, with the groups folded into it, so the user can still pick
  individual files. Key of the builder's choice, in `?` help.
- Live mode (M8): folder groups may appear only once the candidate
  stages have settled what they need; they are CANDIDATES until proven,
  labeled the same way file candidates are. When a folder group
  appears, the groups it folds disappear without moving the cursor off
  what the user was looking at (a cursor on a folded group lands on
  its folder group).
- The `ENTER` confirm overlay counts folders as folders ("2 folders
  (400 files) and 3 files · 8.2 GB") — never as 200 expanded files.
- Live mode recomputes folder groups and folding after every apply,
  since membership changes then.

## D. Proof and removal

- Non-live paths (`--list`, `--auto`, `--report-json`): a folder group
  is reported only when every pair was byte-compared THIS SCAN (or held
  whole in memory), per REWRITE.md §3.1. Big files already proven in
  one file group need no second read.
- Live path: prove-on-apply, as M8. A folder group the user marked is
  proven at apply before anything moves: re-list both sides
  (unfiltered, as §A), compare listings, compare bytes. If anything
  differs or appeared, nothing in that folder group is removed, the
  user is told which folder and why, and its folded groups come back
  as visible groups.
- Immediately before each folder is moved, re-list it once more and
  refuse if its listing changed since the proof. State in LOG.md the
  window that remains and why it is the same window file removal has.
- A marked folder goes to the Trash as ONE directory move, on the
  correct volume (REWRITE.md §3.4) — not 200 file moves. `--hard`
  removes the directory tree. Today `send_to_trash`'s cross-device
  fallback (`fs::copy` + `remove_file`) and `delete_permanently` are
  files-only (`trash.rs` ~:201-221): both need directory-capable
  paths, and the fallback must never remove the source until the copy
  is complete.
- The cache narrows, never proves (§3.3). Cache attestation of small
  files may save reads; it can never be the reason a folder is
  trashed. The README promise holds: a warm rescan of an untouched
  tree reads nothing, folders included. Prefer the existing per-file
  fields; any schema change is justified in LOG.md first.

## E. --auto, --list, --report-json

- `--auto RULE` plans folder groups first: keep one folder per folder
  group by RULE, trash the rest whole. Then, for each visible file
  group: members inside trashed folders are already going; a member
  inside a KEPT folder is the keeper if one exists (a kept folder is
  never torn apart); otherwise RULE picks as today. The plan printout
  lists folders as folders, then files.
- `--list` prints folder groups (paths with `/`, file count, bytes per
  copy) ahead of — or interleaved by size with — the file groups;
  builder's call, deterministic, stated in LOG.md.
- `--report-json` is the conformance protocol: `groups` keeps reporting
  every FILE group, unfolded, exactly as today. A new `folders` key —
  list of lists of folder paths — is emitted ONLY WHEN NONEMPTY. No
  other schema change.
- New flag `--no-folders`: file groups only, exactly today's behaviour.
  In the FLAGS table; menu row required (MENU_EXPOSURE enforces it).

## Laws

- THE FENCE: on any tree with no duplicate-folder candidates, and on
  EVERY tree under `--no-folders`, every output — interactive and not,
  counters included — is byte-identical to main. The M8/M9 freeze
  harness is the proof; extend it, re-run it, re-capture it.
  Conformance 33/33 at defaults (check whether any conformance corpus
  contains a candidate pair, and say so in the claim with the numbers).
- `ProvenGroups` remains the only key to deletion. A folder group is
  removable only through it (extend the type; do not route around it).
- §3 inviolables, all six, hold for folders. Deterministic output
  includes folder group order and keeper choice.
- HARD CONSTRAINT for the builder: never touch real user data, any
  `/Volumes` path (the conformance `dedupconf` image excepted), or
  `~/Library/CloudStorage`. Every folder you trash is one you generated
  in a scratch tree.

## Verification

- Unit: identity rules one by one (extra small file, differing small
  file, extra `.DS_Store` ignored, extra empty dir ignored, symlink
  target differs, FIFO disqualifies, unreadable disqualifies, dataless
  disqualifies without `--cloud`, names of P/Q irrelevant, case
  differs => not identical); maximality; 3-way groups; folding; the
  generalized survivor law, including the cross-group trap: folder B
  marked plus `A/f` and `C/f` marked in `{A/f, B/f, C/f}` => refused;
  `--auto` never tears a kept folder; a file added to a marked folder
  after the scan blocks its removal.
- One scripted end-to-end run, `rust/evidence/scripts/m10_folders.py`:
  builds a scratch tree with two identical folders (some small files,
  a `.DS_Store` that differs), a near-copy that differs in one small
  file, and a loose third copy of one file; runs `--list`, then
  `--auto shortest -y`; checks the folder went to the Trash as one
  directory, its twin is intact, the near-copy is untouched, and a
  second run finds nothing to remove.
- Freeze differential vs main (`m9_freeze_gate.sh` with the base set
  to main): 0 differing at defaults (its corpora have no candidate
  pairs — confirm that) and under `--no-folders`. Conformance 33/33
  via `m9_gate.sh`. Tests >=277 stay green; `cargo clippy
  --all-targets -- -D warnings`; `cargo fmt --check`.
- Phase 2 adds TestBackend snapshots (folder group in the list,
  confirm overlay, unfold, refusal message) and README updates: the
  duplicate definition, keys, `--auto`, options, and the "everything
  counts / seeded from big files" limits, in the README's plain voice.

## For the record

Not this milestone, noted so nobody builds it early: a folder whose
files all exist inside a bigger folder (B ⊂ A), and identity that
ignores names. Note them in LOG.md as future milestones if useful.
