#!/bin/sh
# Put `dedup` on PATH by symlinking it into ~/.local/bin.
set -e

SRC="$(cd "$(dirname "$0")" && pwd)/dedup.py"
BIN_DIR="$HOME/.local/bin"
TARGET="$BIN_DIR/dedup"

mkdir -p "$BIN_DIR"
chmod +x "$SRC"
ln -sf "$SRC" "$TARGET"

echo "installed: $TARGET -> $SRC"

case ":$PATH:" in
    *":$BIN_DIR:"*)
        echo "run 'dedup' in any folder to scan it" ;;
    *)
        echo
        echo "NOTE: $BIN_DIR is not on your PATH. Add this to ~/.zshrc:"
        echo "    export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
