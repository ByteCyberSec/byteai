#!/bin/bash
# Restore the newest ByteAi checkpoint (the one kept by checkpoint.sh).
# Usage: ~/byteai/scripts/restore-checkpoint.sh [checkpoint-dir]
set -euo pipefail

BYTEAI="$HOME/byteai"
CKPT_DIR="$BYTEAI/.checkpoints"

if [ $# -ge 1 ]; then
  SRC="$1"
else
  SRC="$(find "$CKPT_DIR" -maxdepth 1 -type d -name 'pre-*' | sort -r | head -1 || true)"
fi

if [ -z "$SRC" ] || [ ! -d "$SRC" ]; then
  echo "no checkpoint found in $CKPT_DIR" >&2
  exit 1
fi

echo "→ restoring from: $SRC"
[ -f "$SRC/checkpoint.txt" ] && cat "$SRC/checkpoint.txt"

# Restore source tree.
if [ -f "$SRC/source.tar" ]; then
  echo "→ restoring source tree (crates, Cargo.toml/lock)…"
  tar -C "$BYTEAI" -xf "$SRC/source.tar"
fi

# Restore config files.
for f in "$SRC"/config/*; do
  [ -e "$f" ] || continue
  base="$(basename "$f")"
  case "$base" in
    config.toml) dst="$HOME/Library/Application Support/byteai/config.toml" ;;
    byteai-admin-key.env) dst="$HOME/.memory-tencentdb/byteai-admin-key.env" ;;
    *) dst="$HOME/.config/byteai/$base" ;;
  esac
  echo "→ restoring config → $dst"
  mkdir -p "$(dirname "$dst")"
  cp "$f" "$dst"
done

echo "✓ restored. Rebuild with: cd ~/byteai && cargo build --release -p byteai-cli"
