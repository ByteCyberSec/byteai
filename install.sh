#!/usr/bin/env bash
# ByteAi — one-command install for a fresh machine.
#
#   curl -fsSL https://raw.githubusercontent.com/byteai/byteai/main/install.sh | bash
#
# Builds the release binary with cargo and installs it to ~/.local/bin (or
# $PREFIX/bin). Requires: Rust toolchain (rustup) + git.

set -euo pipefail

# ── resolve prefix ────────────────────────────────────────────────────────────
if [[ -n "${PREFIX:-}" ]]; then
    BIN_DIR="$PREFIX/bin"
elif [[ -d "$HOME/.local/bin" || -w "$HOME/.local" ]]; then
    BIN_DIR="$HOME/.local/bin"
else
    BIN_DIR="/usr/local/bin"
fi
mkdir -p "$BIN_DIR"

# ── source ────────────────────────────────────────────────────────────────────
REPO_URL="${BYTEAI_REPO_URL:-https://github.com/byteai/byteai.git}"
BRANCH="${BYTEAI_BRANCH:-main}"
SRC_DIR="${BYTEAI_SRC_DIR:-$(mktemp -d)/byteai}"

clone_or_update() {
    if [[ -d "$SRC_DIR/.git" ]]; then
        echo "→ updating existing checkout at $SRC_DIR"
        git -C "$SRC_DIR" fetch --quiet origin "$BRANCH"
        git -C "$SRC_DIR" checkout --quiet "$BRANCH"
        git -C "$SRC_DIR" pull --quiet --ff-only origin "$BRANCH"
    else
        echo "→ cloning $REPO_URL ($BRANCH)"
        git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" "$SRC_DIR"
    fi
}

# ── prerequisites ─────────────────────────────────────────────────────────────
require_cmd() { command -v "$1" >/dev/null 2>&1 || { echo "✗ missing: $1"; exit 1; }; }
require_cmd cargo
require_cmd git

# ── build ─────────────────────────────────────────────────────────────────────
clone_or_update
cd "$SRC_DIR"
echo "→ building release (this can take a few minutes)"
cargo build --release

# ── install ───────────────────────────────────────────────────────────────────
install -m 0755 target/release/byteai "$BIN_DIR/byteai"

# ── config ────────────────────────────────────────────────────────────────────
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/byteai"
if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
    mkdir -p "$CONFIG_DIR"
    cp "$SRC_DIR/install/config.example.toml" "$CONFIG_DIR/config.toml"
    echo "→ wrote default config to $CONFIG_DIR/config.toml (edit providers/API keys)"
fi

echo
echo "✓ byteai installed to $BIN_DIR/byteai"
echo
echo "  Run:  byteai doctor"
echo "  Chat: byteai chat 'hello'"
echo "  REPL: byteai chat"
echo "  TUI:  byteai tui"
if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
    echo
    echo "  NOTE: $BIN_DIR is not on your PATH. Add it:"
    echo "    echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc"
fi
