#!/bin/zsh
# cloud-validate.sh — supervised, READ-ONLY proof that a default scan does not
# download a single byte of a cloud folder.
#
# Run this YOURSELF (no agents). From the dedup repo root:
#   ./tools/cloud-validate.sh "$HOME/Library/CloudStorage/GoogleDrive-you@example.com/My Drive/some-subtree"
#
# Why this exists, and why it is not a test. A real dataless file cannot be
# fabricated: SF_DATALESS is one of the SF_SYNTHETIC flags, which <sys/stat.h>
# deliberately keeps out of SF_SUPPORTED so that no userland process — root
# included — can set it. Only a File Provider can make one. So the walk-time
# detector is unit-tested against injected flags, the parse offset is checked
# against lstat, and the whole path is exercised end to end by swapping the
# flag constant for one chflags CAN set (evidence/scripts/m9_dataless_walk.py).
# What none of that can show is this tool meeting a real Google Drive
# placeholder. That is what you are doing here.
#
# Everything below is read-only: --list scans and reports, no deletion flag is
# passed, and nothing is written inside the tree. The one number that matters
# is `du` before and after — if the scan had materialized anything, the local
# footprint would grow by the size of whatever it fetched.
set -u

TREE="${1:?usage: cloud-validate.sh <subtree-of-a-cloud-folder>}"
BIN="${DEDUP_BIN:-rust/target/release/dedup-rs}"
OUT="cloud-validate-$(date +%Y%m%d-%H%M).txt"

[[ -d "$TREE" ]] || { echo "not a directory: $TREE"; exit 2; }
[[ -x "$BIN"  ]] || { echo "dedup binary not found at $BIN (build it, or set DEDUP_BIN)"; exit 2; }

log() { echo "$@" | tee -a "$OUT"; }

log "== cloud validation $(date) =="
log "tree:   $TREE"
log "binary: $BIN"
log ""

# How many of the files down there are placeholders right now, asked of the
# filesystem rather than of the tool being tested. `find -flags` understands
# the name; SF_DATALESS is 0x40000000.
log "== what is down there, according to find(1) and not according to dedup =="
TOTAL=$(find "$TREE" -type f 2>/dev/null | wc -l | tr -d ' ')
DATALESS=$(find "$TREE" -type f -flags +dataless 2>/dev/null | wc -l | tr -d ' ')
log "files:            $TOTAL"
log "dataless (cloud): $DATALESS"
if [[ "$DATALESS" == "0" ]]; then
  log ""
  log "NOTE: nothing in this subtree is a placeholder, so this run cannot prove"
  log "much. Pick a subtree with files that show the cloud/download icon in"
  log "Finder — or right-click one and choose 'Remove Download' first."
fi
log ""

# The footprint before. -H so the numbers are the same units either side, and
# apparent-size is deliberately NOT used: a placeholder's LOGICAL size is the
# full size of the file, and it is the blocks actually on disk that would grow
# if anything were fetched.
log "== local footprint before (du: blocks on this disk, not logical size) =="
BEFORE_RAW=$(du -sk "$TREE" 2>/dev/null | awk '{print $1}')
log "$(du -sh "$TREE" 2>/dev/null)"
log ""

log "== the default scan: no --cloud, no deletion flag, --list only =="
REPORT=$(mktemp -t cloud-validate-report)
log "\$ $BIN \"$TREE\" --list --report-json $REPORT"
"$BIN" "$TREE" --list --report-json "$REPORT" 2>&1 | tee -a "$OUT"
# ${pipestatus[1]} and not $?, which in zsh is the LAST element of the pipeline
# — that is `tee`'s status, and `tee` succeeds whatever the scan did. Captured
# on the line straight after the pipeline, because the next command overwrites
# it. REVIEW-M9 item 4: this is the script the maintainer runs on real cloud
# storage, where the transcript IS the record.
SCAN_STATUS=${pipestatus[1]}
log "   exit ${SCAN_STATUS}"
if [[ "${SCAN_STATUS}" != "0" ]]; then
  log ""
  log "NOTE: the scan itself failed. Everything below still measures the tree,"
  log "but a scan that did not finish proves nothing about what it would read."
fi
log ""

log "== what the report says it skipped =="
python3 - "$REPORT" <<'PY' | tee -a "$OUT"
import json, sys
with open(sys.argv[1]) as fh:
    said = json.load(fh)
print("skipped_cloud:", said.get("skipped_cloud", "(absent — the scan met no cloud file)"))
print("files_seen:   ", said["files_seen"])
print("bytes_read:   ", said["bytes_read"])
print("groups:       ", len(said["groups"]))
PY
rm -f "$REPORT"
log ""

# The footprint after. This is the whole proof.
log "== local footprint after =="
AFTER_RAW=$(du -sk "$TREE" 2>/dev/null | awk '{print $1}')
log "$(du -sh "$TREE" 2>/dev/null)"
log ""
log "before: ${BEFORE_RAW} KiB"
log "after:  ${AFTER_RAW} KiB"
if [[ "$BEFORE_RAW" == "$AFTER_RAW" ]]; then
  log ""
  log "PASS: not one block was fetched. The scan read no cloud file."
  VERDICT=0
else
  GREW=$(( AFTER_RAW - BEFORE_RAW ))
  log ""
  log "FAIL: the tree grew by ${GREW} KiB. Something was materialized."
  log "(Check that no other process — Finder previews, Spotlight, a backup —"
  log " was walking the same folder while this ran, then run it again.)"
  VERDICT=1
fi

log ""
log "== still dataless afterwards? =="
STILL=$(find "$TREE" -type f -flags +dataless 2>/dev/null | wc -l | tr -d ' ')
log "dataless before: $DATALESS"
log "dataless after:  $STILL"
if [[ "$DATALESS" != "$STILL" ]]; then
  log "FAIL: $(( DATALESS - STILL )) file(s) stopped being placeholders."
  VERDICT=1
fi

log ""
log "written to $OUT"
exit "$VERDICT"
