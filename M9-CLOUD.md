# M9 — cloud placeholders, and a rate that tells the truth

Commissioned 2026-08-27. The maintainer scanned their home directory
and watched "8 KiB/s" on an SSD: the scan had wandered into
`~/Library/CloudStorage/GoogleDrive-…`, where files are streamed
placeholders — reading one makes the provider download it from the
network. A 470 MB .MOV with 12 MB physically on disk sat at the head
of the queue, and the rate display — a scan-lifetime average — decayed
like 1/t while nothing moved, which read as "mysteriously dying"
instead of "downloading one file from Google". Claim as M9 in
`rust/MILESTONES.md`; the maintainer review is BLOCKING
(`rust/REVIEW-M9.md`).

## A. Cloud placeholders are skipped by default

- A dataless file (macOS `SF_DATALESS` in `st_flags` /
  `ATTR_CMN_FLAGS`; the walk's getattrlistbulk request already fetches
  attributes and can carry the flags) is EXCLUDED at walk time, before
  any grouping. The allocated-vs-logical-size heuristic is NOT the
  detector — sparse and compressed files legitimately under-allocate.
- Defense in depth: by default the process sets the
  materialize-dataless-files-off I/O policy
  (`setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, …)`), so
  even a file that slips past detection — including one EVICTED
  MID-SCAN — fails its read (EDEADLK) instead of triggering a
  download. That failure must degrade gracefully through the existing
  read-error path: the file leaves its group honestly, nothing
  crashes, the message names the file and says it lives in the cloud.
- New flag `--cloud`: include dataless files (and lift the I/O
  policy). Help text must say plainly that comparing them downloads
  them. Menu row required (MENU_EXPOSURE will enforce it).
- Skipped files are COUNTED and VISIBLE, never silent: the live
  browser and the human summary say "N files in cloud — not scanned
  (--cloud to include)" whenever N > 0. `--report-json` gains a
  skipped-cloud count EMITTED ONLY WHEN NONZERO, so byte-identity on
  dataless-free trees is preserved. No other schema change.
- The LAW: a default scan must never cause a single byte of cloud
  materialization. Not from the walk, not from sampling, not from a
  proof at apply.
- Simplicity rule: skipped means skipped — no grouping dataless files
  from cached facts this milestone (note the idea in LOG.md if you
  see it; do not build it). Non-macOS gather path: nothing is
  dataless, `--cloud` is a no-op; say so in help only if free.

## B. The progress display stops averaging away stalls

- Every rate shown (live browser and stderr pulse) becomes a RECENT-
  WINDOW rate (window constant in consts.rs; EWMA or ring — builder's
  choice, stated in LOG.md). A stalled read must read ~0 within a few
  seconds. A scan-lifetime average may never be displayed.
- The progress area names the CURRENT FILE being read (most recently
  opened, under parallel readers), path middle-truncated to the
  width. A user staring at a frozen bar must be able to see WHICH
  file it is frozen on.
- `--quiet` and piped runs stay exactly as inert as M5.2 left them;
  re-prove, don't assume.

## Laws

- The M8 absolute fence, restated for M9: on any tree containing NO
  dataless files, every output — interactive and not — is
  byte-identical to main, counters included. The M8 freeze harness is
  the proof; re-run it, re-captured.
- Deletion path: `ProvenGroups` untouched. A `--cloud` proof that
  reads a cloud file is a deliberate, flagged act; the default path
  can never reach one.
- Cache: no verdicts are ever recorded for files that were not read.
- HARD CONSTRAINT for the builder: you never touch
  `~/Library/CloudStorage`, any real user data, or any `/Volumes`
  path (the conformance gate's own `dedupconf` image excepted). Real
  dataless files cannot be fabricated in CI — that is not permission
  to test on the maintainer's Drive. Detection goes behind a seam and
  is unit-tested with injected flags; the I/O-policy setting is
  asserted via `getiopolicy_np` in-process; the EDEADLK path is
  fault-injected. Provide `tools/cloud-validate.sh` for the
  MAINTAINER to run on a real cloud folder (prints skip counts and
  before/after `du` proof of zero materialization; deletes nothing).

## Verification

- Unit: dataless flag → excluded by default, included with `--cloud`,
  counted either way; EDEADLK mid-scan eviction degrades gracefully;
  iopolicy set by default and lifted under `--cloud`; windowed-rate
  math (a stall decays to ~0 in one window, never 1/t).
- TestBackend snapshots: current-file line; the cloud-skip notice.
- pty (`rust/evidence/scripts/m9_*.py`): pulse shows the current file
  on a TTY; quiet/piped inertness; menu carries `--cloud`
  (MENU_EXPOSURE green).
- M8 freeze differential vs main on dataless-free corpora: 0
  differing, re-captured. Conformance 33/33. RBENCH flash within
  noise. Tests ≥258 stay green; clippy -D warnings --all-targets;
  fmt; claim_audit.py M9 clean.

## For the record

The maintainer's stuck scan was comparing a REAL duplicate pair —
local `~/AI_Projects/dus_pesime/VIDEO_TS` against its Drive copy.
Cloud-aware dedup (grouping from cache without reads) is a future
milestone, not this one.
