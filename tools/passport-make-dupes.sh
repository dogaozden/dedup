#!/bin/zsh
# passport-make-dupes.sh — build a compare-stage-heavy test folder.
# Run this YOURSELF (no agents). It WRITES ~2.3 GB of synthetic files:
#   ./tools/passport-make-dupes.sh "/Volumes/My New Passport"
# creates "<target>/dedup-dupetest" and nothing outside it. Remove later with:
#   rm -rf "<target>/dedup-dupetest"
#
# Shape: 6 groups of 4 files, 96 MB each, all four the SAME SIZE. In each
# group three are byte-identical and one differs only in its final megabyte —
# the worst case for a byte-exact comparer: every candidate must be read to
# the tail before the odd one can be split off. This is the "95%, comparing"
# workload.
set -eu
TARGET="${1:?usage: passport-make-dupes.sh <folder-to-create-the-test-dir-in>}"
DEST="$TARGET/dedup-dupetest"

[[ -d "$TARGET" ]] || { echo "not a directory: $TARGET"; exit 2; }
[[ -e "$DEST" ]] && { echo "$DEST already exists — remove it first."; exit 2; }

FREE_KB=$(df -k "$TARGET" | tail -1 | awk '{print $4}')
(( FREE_KB > 5 * 1024 * 1024 )) || { echo "less than 5 GB free on target; refusing."; exit 2; }

echo "Writing ~2.3 GB into $DEST (6 groups x 4 x 96 MB)..."
mkdir -p "$DEST"
for g in 1 2 3 4 5 6; do
  dir="$DEST/group$g"
  mkdir "$dir"
  # One 96 MB random source, made locally-ish via dd from urandom.
  dd if=/dev/urandom of="$dir/original.bin" bs=1m count=96 status=none
  cp "$dir/original.bin" "$dir/copy-a.bin"
  cp "$dir/original.bin" "$dir/copy-b.bin"
  # Same size, different final megabyte: identical for 95 MB, then diverges.
  cp "$dir/original.bin" "$dir/near-miss.bin"
  dd if=/dev/urandom of="$dir/near-miss.bin" bs=1m count=1 seek=95 conv=notrunc status=none
  echo "  group$g done"
done

echo ""
echo "Done. Expected answer: 6 groups of 3 duplicates each (18 files, 1.7 GB"
echo "reclaimable); every near-miss.bin correctly left out despite matching"
echo "size and first 95 MB. Now eject, re-attach, and measure:"
echo "  SKIP_FLASH=1 ./tools/passport-measure.sh \"$DEST\"   # cold spinning"
echo "eject + re-attach again, then:"
echo "  SKIP_SPIN=1  ./tools/passport-measure.sh \"$DEST\"   # cold flash"
echo "Clean up when finished:  rm -rf \"$DEST\""
