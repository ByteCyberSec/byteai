#!/bin/bash
# Phase 1 minimal benchmark: startup latency, idle RAM, one-shot task timing.
# Compare against jcode, pi, hermes, claude, codex, opencode (run with same model if possible).

set -euo pipefail
BIN="$HOME/byteai/target/release/byteai"
echo "=== ByteAi Phase 1 Benchmarks ==="
echo "Binary: $(command -v $BIN)"
echo "Size:  $(ls -lh "$BIN" | awk '{print $5}')"
echo "Date:  $(date)"
echo ""

# 1. Version/help latency
echo "--- 1. Startup latency (--version) ---"
hyperfine --warmup 3 --runs 10 "$BIN --version" 2>/dev/null || {
    for i in 1 2 3; do
        /usr/bin/time -l $BIN --version 2>&1
    done
}

# 2. Doctor latency (requires network — model listing)
echo ""
echo "--- 2. Doctor latency ---"
hyperfine --min-runs 3 "$BIN doctor" 2>/dev/null || echo "hyperfine not installed"

# 3. Idle RAM (doctor after 5s)
echo ""
echo "--- 3. Idle RSS ---"
$BIN doctor &
PID=$!
sleep 5
ps -o rss= -p $PID 2>/dev/null || echo "RSS measurement failed"
kill $PID 2>/dev/null || true

# 4. One-shot task
echo ""
echo "--- 4. One-shot task ---"
TASKS=(
    "Create a file /tmp/byteai-bench-task.txt with content exactly: benchmark successful"
    "What is 2+2? Answer concisely with just the number."
)
# Run a simple task and measure
for task in "${TASKS[@]}"; do
    echo "Task: ${task:0:60}..."
    /usr/bin/time -l bash -c "$BIN chat \"$task\" 2>/dev/null" 2>&1 | tail -3
    echo ""
done