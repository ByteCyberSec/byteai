#!/bin/bash
# ByteAi checkpoint — snapshot state before a change, keep ONLY the newest.
#
# Usage:
#   ~/byteai/scripts/checkpoint.sh            # auto: pre-<ts>
#   ~/byteai/scripts/checkpoint.sh "memory"   # labeled: pre-<ts>-memory
#   ~/byteai/scripts/restore-checkpoint.sh    # restore newest snapshot
#
# Policy (user rule): BEFORE every byteai change, save a checkpoint. After
# the 3rd checkpoint exists, delete the older two — always leaving exactly
# ONE (the newest) in .checkpoints/.
set -euo pipefail

BYTEAI="$HOME/byteai"
CKPT_DIR="$BYTEAI/.checkpoints"
LABEL="${1:-auto}"
TS="$(date +%Y%m%d-%H%M%S)"
DEST="$CKPT_DIR/pre-$TS-$LABEL"

echo "→ snapshotting byteai state → $DEST"

# Snapshot the source tree exactly as it is now (git-tracked + untracked in
# the crates + config). Uses tar so timestamps/permissions are preserved and
# restore is a single untar.
mkdir -p "$DEST"
tar -C "$BYTEAI" --exclude='.checkpoints' --exclude='target' --exclude='.git' \
    -cf "$DEST/source.tar" \
    crates Cargo.toml Cargo.lock 2>/dev/null || true

# Config files (both locations byteai actually reads).
mkdir -p "$DEST/config"
for f in \
    "$HOME/Library/Application Support/byteai/config.toml" \
    "$HOME/.config/byteai/config.toml" \
    "$HOME/.memory-tencentdb/byteai-admin-key.env" \
    ; do
  if [ -f "$f" ]; then
    cp "$f" "$DEST/config/$(basename "$f")" 2>/dev/null || true
  fi
done

# Metadata
{
  echo "created: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "label: $LABEL"
  echo "git: $(cd "$BYTEAI" && git rev-parse --short HEAD 2>/dev/null || echo n/a)"
  echo "tree: $(cd "$BYTEAI" && git status --porcelain 2>/dev/null | wc -l | tr -d ' ') changed files"
} > "$DEST/checkpoint.txt"

# Prune: keep ONLY the newest checkpoint (by mtime), delete the rest.
# bash-3 / macOS safe: ls -t sorts newest-first, no mapfile needed.
shopt -s nullglob
ALL_SORTED=( $(ls -td "$CKPT_DIR"/pre-*/ "$CKPT_DIR"/toolcards-*/ 2>/dev/null) )
if [ "${#ALL_SORTED[@]}" -gt 1 ]; then
  KEEP="${ALL_SORTED[0]}"
  for old in "${ALL_SORTED[@]:1}"; do
    echo "  pruning old checkpoint: $(basename "$old")"
    rm -rf "$old"
  done
  echo "→ kept: $(basename "$KEEP")"
fi

echo "✓ checkpoint saved: $DEST"
echo "  restore with: ~/byteai/scripts/restore-checkpoint.sh"
