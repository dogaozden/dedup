# M5.1 — the pre-flight menu, and --help

Commissioned 2026-08-23. Extends M5 (M5-TUI.md's laws apply unchanged: model/
paint separation, terminal restore, TTY-only interactivity, one deletion
path). Claim as M5.1 in `rust/MILESTONES.md`; the maintainer review is
BLOCKING, shared with M6.

## --help

`--help` and `-h` print real usage — every documented flag with a one-line
description and its default — and exit 0. Help text and the parser must come
from ONE table (a single source of flag truth), so they cannot drift apart.
Today `--help` exits 2 with "unknown argument"; that is the bug.

## The menu

On an INTERACTIVE launch only — the browser's own condition (a terminal,
neither `--list` nor `--auto`), including explicit `-i` — a pre-flight
screen appears before the scan: a small form showing the scan root and the
tool's real options (minimum size, `--ext`, `--skip-ext`, `-x` excludes,
jobs, cache on/off, and `--profile` once M6 lands). Values given on the
command line prefill their fields; everything else shows its default.
Enter-through accepts the whole form and scans; simple navigation edits a
field; Esc/q exits 0 without scanning. Piped and flagged non-interactive
use never sees the menu and is byte-for-byte unchanged.

Rules:
- The menu only ever BUILDS the same options struct the CLI builds — it
  gains no capability the flags don't have, and it never touches the
  deletion path (it runs before any scan exists).
- Defaults shown are the tool's real defaults, read from the same flag
  table as --help. No new configuration file, no persistence of choices.
- Invalid input is rejected in the field with the parser's own error
  message, not at scan time.

## Verification

- Unit tests on the form model (terminal-free): prefill-from-flags,
  edit/validate per field, Enter-through equals CLI defaults, Esc scans
  nothing.
- TestBackend snapshots of the menu (fields visible, defaults shown).
- End-to-end pty scripts in `rust/evidence/scripts/`: launch menu, change
  min-size, run scan, assert the listed groups reflect the changed value;
  and an Enter-through run identical to a flagless CLI run.
- A --help golden test asserting every flag the parser accepts appears in
  the help text and vice versa (the one-table rule, enforced).
- `--gate all` (with `{stage}`) still 33/33 — conformance templates are
  non-interactive and must be unaffected.
