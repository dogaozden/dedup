#!/bin/zsh
# passport-measure.sh — supervised, READ-ONLY spinning-media measurement.
# Run this YOURSELF (no agents): from the dedup repo root,
#   ./tools/passport-measure.sh "/Volumes/My New Passport/<some-subtree>"
# Pick a subtree of roughly 2–10 GB. Everything here is read-only:
# --list scans only, no deletion flags, nothing written to the drive.
set -u
TREE="${1:?usage: passport-measure.sh <subtree-on-the-hdd>}"
BIN="${DEDUP_BIN:-$HOME/.local/bin/dedup}"
OUT="passport-measure-$(date +%Y%m%d-%H%M).txt"

[[ -d "$TREE" ]] || { echo "not a directory: $TREE"; exit 2; }
[[ -x "$BIN" ]]  || { echo "dedup binary not found at $BIN"; exit 2; }

log() { echo "$@" | tee -a "$OUT"; }

log "== passport measurement $(date) =="
log "tree: $TREE"
log "binary: $BIN ($($BIN --help 2>/dev/null | head -1))"

log ""
log "== drive info (read-only) =="
diskutil info "$(df "$TREE" | tail -1 | awk '{print $1}')" 2>&1 | \
  grep -E 'Device Node|Volume Name|File System|Solid State|Protocol|Disk Size|SMART' | tee -a "$OUT"

log ""
log "== walk only (metadata seeks) =="
log "-- find, cold-ish:"
FLIST=$(mktemp)
( time find "$TREE" -type f 2>/dev/null > "$FLIST" ) 2>&1 | tee -a "$OUT"
NFILES=$(wc -l < "$FLIST" | tr -d ' ')
rm -f "$FLIST"
log "$NFILES files"
if [[ "$NFILES" -lt 100 ]]; then
  log "WARNING: only $NFILES files in this tree — too small to measure anything."
  log "Pick a subtree with thousands of files (2-10 GB) and rerun."
fi

# The scan's own stderr goes to the terminal rather than into the log, so the
# live progress line is on while you sit through an hour of platter; only the
# timings are captured, because a repainted line logged verbatim is a page of
# carriage returns. Where there is no controlling terminal to write to, the
# line stays off and everything goes to the log as it always did — a missing
# /dev/tty must not cost you the measurement.
LIVE=/dev/stderr
( : >/dev/tty ) 2>/dev/null && LIVE=/dev/tty

timed() {  # timed <flags...>
  ( time "$BIN" "$TREE" "$@" >/dev/null 2>"$LIVE" ) 2>&1 | tee -a "$OUT"
}

run_scan() {  # run_scan <label> <extra flags...>
  local label="$1"; shift
  log ""
  log "== $label =="
  log "$ dedup <tree> --list --no-cache $*"
  timed --list --no-cache "$@"
}

# IMPORTANT: between the two profile runs, EJECT and re-attach the drive if
# you want true cold-cache numbers, then rerun this script with SKIP_FLASH=1
# or SKIP_SPIN=1 to do one half at a time. Same-session back-to-back runs
# share the page cache — still informative, but say so when reading results.

[[ "${SKIP_FLASH:-0}" = 1 ]] || run_scan "flash profile (current default behavior)" --profile flash --min-size 1m
[[ "${SKIP_SPIN:-0}"  = 1 ]] || run_scan "spinning profile" --profile spinning --min-size 1m

log ""
log "== warm re-scan (cache on) =="
log "$ dedup <tree> --list   (twice; second one shows the cache)"
timed --list --min-size 1m
timed --list --min-size 1m

log ""
log "== done — results in $OUT =="
echo "Paste $OUT into the chat (or just scroll up) for analysis."
