# M5.2 — sizes for humans, and a live pulse

Commissioned 2026-08-23, after the maintainer hit both defects in one
evening: typed `100mb` into the menu and was told sizes are bytes, then
sat through an HDD scan with no sign of life. Extends M5/M5.1 — their
laws apply unchanged (model/paint separation, terminal restore, one
deletion path, menu builds only what flags can say). Claim as M5.2 in
`rust/MILESTONES.md`; the maintainer review is BLOCKING
(`rust/REVIEW-M5_2.md`).

## Sizes for humans

- ONE grammar in ONE function, owned by the flag table — so the CLI,
  the menu and the help text cannot drift. Plain digits stay plain
  bytes (conformance templates unchanged). New: case-insensitive
  suffixed forms — `512k`, `100mb`, `1.5g`, `2TiB` — suffix letters
  k/m/g/t with optional `b`/`ib`, ALL binary multiples (1024ⁿ),
  matching the tool's MiB display convention. Decimals allowed, floored
  to whole bytes. One optional space between number and unit (so every
  displayed form parses back). Overflow, negatives and junk rejected.
- The rejection TEACHES: `not a size: "12qb" — plain bytes or 512k,
  100mb, 1.5g`. The menu already surfaces the parser's own message
  in-field; after this change that message is worth reading.
- Display round-trip law: wherever a size is SHOWN (help default, menu
  prefill/default), it is shown humanly — `10 MiB`, not `10485760` —
  through one humanizer shared with the TUI's `human_size`, and every
  string the tool displays must parse back to exactly the bytes it
  came from. Enforced by test.
- `--help` documents the grammar on the min-size line.

## The live pulse

Port the oracle's `Reporter` (dedup.py:2149–2200) faithfully, then
refine:

- A single status line on stderr, repainted with `\r\033[2K`. Stage
  labels as the oracle names them; Scanning shows `files · bytes`;
  reading stages show a 24-cell █░ bar, percent, done/total and a
  rate. `finish` clears the line — nothing left in scrollback, and the
  browser's alternate screen opens only after the line is gone.
  `--list` stdout never interleaves with it.
- Inert unless stderr is a TTY and `--quiet` is absent. Inert means
  ZERO added work on the scan path — no ticker thread, no new
  syscalls, no allocations. RBENCH and the harnesses pipe stderr; the
  referee will diff counters and wall-clock against main and any
  measurable cost is a defect.
- `--quiet` becomes real and documented; its no-op parse ("nothing to
  be quiet about") dies.
- Repaint at most every ~100 ms. Numbers shown must be TRUE — derived
  from the real counters, monotone within a stage; no invented
  percentages. How totals are threaded (the existing Effort atomics, a
  ticker thread reading them, stage candidate counts) is the builder's
  choice.

## Also

- `tools/passport-measure.sh` switches to the human forms it always
  wanted (`--min-size 1m`) — dogfooding.
- Menu min-size field: humanized default shown, human input accepted —
  mostly falls out of the shared parser; prove it end-to-end anyway.

## Verification

- Grammar unit table: bytes, every suffix, case, `b`/`ib`, decimals,
  the space, zero, negative, overflow, junk — error text asserted.
  Round-trip test over every displayed default.
- Help golden updated; the one-table rule test still passes.
- TestBackend menu snapshot showing the humanized default.
- pty scripts in `rust/evidence/scripts/`: menu accepts `100mb` and
  the scan reflects it; a TTY run paints progress and hands over to a
  clean browser screen; a piped run's stdout is byte-identical to
  today's and its stderr is empty.
- Inertness proof: a scored-shape run with stderr piped shows
  unchanged opens/bytes_read/seeks and wall-clock within noise vs
  main.
- `cargo test` / clippy / fmt green; `--gate all` (with `{stage}`)
  still 33/33.
