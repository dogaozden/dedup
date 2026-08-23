# M5 — the interactive TUI

Commissioned 2026-08-23. This extends REWRITE.md; everything in REWRITE.md §0
(fence) and §3 (inviolables) applies unchanged. Work happens in `rust/` of the
dedup monorepo. The claim/review handshake applies: claim M5 in
`rust/MILESTONES.md` when done, a maintainer review is BLOCKING (this is
deletion-adjacent UI), and a `rust/REVIEW-M5.md` with CHANGES is binding.

## Mission

Port the original product surface: the interactive report where the user
browses duplicate groups with full visible paths and chooses what to delete.
The UX oracle is `dedup.py`'s curses interactive mode — read that code first
and match its information architecture (group list, sizes, member paths,
keeper indication, selection, confirmation, summary) rather than inventing a
new one. Where curses idioms don't translate, keep the *information* parity
and use native ratatui idioms; note each such divergence in LOG.md.

## Hard rules

1. **One deletion path.** The TUI is a frontend over the existing machinery:
   selections become work for the SAME code that `--auto` uses —
   `ProvenGroups`, `verify_attested`, the survivor guard, hardlink clusters
   as a unit, trash placement. The TUI must not construct its own removal
   calls, and the `ProvenGroups` type guard must not be weakened, ever. If
   the TUI needs a "delete these specific members" entry point, add it beside
   `run_auto` taking the same token, with the survivor guard enforced per
   group inside it (refuse any selection that dooms all members of a group).
2. **TTY only.** The TUI refuses to start when stdin or stdout is not a
   terminal, with a clear message. Non-interactive use stays `--list`/`--auto`.
3. **No deletion without explicit confirmation** inside the TUI, showing the
   exact count, byte total, and Trash-vs-permanent mode before acting.
4. **Terminal restore is unconditional.** Raw mode / alternate screen must be
   released on every exit path including panic (a drop-guard plus a panic
   hook). A user's wrecked terminal is a failed milestone.
5. **Crates:** `ratatui` + its default `crossterm` backend are allowlisted for
   M5 (REWRITE.md §7). Nothing else new without a logged §7 justification.

## Verification (a TUI cannot be conformance-gated; this replaces the gate)

- **Separate the model from the paint.** Selection state, cursor movement,
  group/member toggling, the survivor-guard interaction, and the deletion
  plan derivation live in plain testable code with no terminal dependency.
  Unit-test that model hard, including: selecting every member of a group is
  refused; hardlink cluster selection behaves as a unit; pinned records are
  not offered.
- **Render tests** with ratatui's `TestBackend`: snapshot the drawn buffer
  for a seeded scan (a fixed scratch tree) at several UI states. Assert
  paths are visible and the keeper is marked.
- **End-to-end on scratch trees only** (REWRITE.md §0): scripted event
  sequences driving the real app against generated trees, asserting on-disk
  outcomes (deleted exactly what was selected, Trash vs --hard placement,
  nothing on refusal). Record the transcripts in `rust/evidence/`.
- `cargo test`, `clippy --all-targets -- -D warnings`, `fmt --check` clean,
  and the existing `--gate all` STILL green (the TUI must not disturb the
  scan/deletion core).

## Out of scope

Progress-bar theatrics, config files, mouse support, themes. Match the
oracle's feature set; stop there.
