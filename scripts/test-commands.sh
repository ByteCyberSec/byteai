#!/usr/bin/env bash
# ByteAI slash-command test harness — one test per command.
# Usage: scripts/test-commands.sh
# Each command is piped into `byteai chat` (REPL) and its output asserted.
# Uses an ISOLATED data dir so state-dependent tests are deterministic.
set -u
cd "$(dirname "$0")/.."
BIN=./target/debug/byteai
[ -x "$BIN" ] || cargo build 2>/dev/null

# Isolated data dir: fresh state for every run, cleaned on exit.
TEST_DATA="$(mktemp -d /tmp/byteai-cmdtest.XXXXXX)"
trap 'rm -rf "$TEST_DATA"' EXIT
export BYTEAI_DATA_DIR="$TEST_DATA"

PASS=0; FAIL=0; FAILED_TESTS=()

run() { # run <label> <expected-substr> <stdin...>
  local label="$1" expect="$2"; shift 2
  local out
  out=$(printf '%s\n/quit\n' "$@" | "$BIN" chat 2>&1)
  if printf '%s' "$out" | grep -qF -- "$expect"; then
    PASS=$((PASS+1)); echo "PASS  $label"
  else
    FAIL=$((FAIL+1)); FAILED_TESTS+=("$label")
    echo "FAIL  $label   (wanted: $expect)"
    printf '%s' "$out" | grep -iE "error|panic|warn" | head -3 | sed 's/^/      /'
  fi
}

echo "== /help =="
run "help lists commands" "/route <type> <task>" "/help"

echo "== /model =="
run "model shows current" "[model =" "/model"
run "model switches" "[model -> deepseek-v4-flash]" "/model deepseek-v4-flash"
run "model persists default" "[model = deepseek-v4-flash]" "/model"

echo "== /provider =="
run "provider shows list" "[provider =" "/provider"
run "provider switch" "[provider -> bai" "/provider bai"
run "provider add usage" "usage: /provider add" "/provider add onlyname"

echo "== /tools =="
run "tools lists registry" "autoskill" "/tools"
run "tools shows new tools" "conductor" "/tools"

echo "== /route =="
run "route fast" "routing →" "/route fast \"list files\""
run "route reasoning" "why:" "/route reasoning \"think about it\""

echo "== /govern =="
run "govern blocks delete" "BLOCKED" "/govern delete \"delete all files in /etc\""
run "govern allows safe" "APPROVED" "/govern check \"create a file\""

echo "== /ideas =="
run "ideas menu" "what should ByteAI discover" "/ideas menu"

echo "== /github =="
run "github status" "Logged in to github.com" "/github status"

echo "== /goal =="
run "goal get empty" "no active goal" "/goal get"
run "goal set" "goal set for session" "/goal set \"ship the api\""
run "goal get shows" "ship the api" "/goal get"
run "goal complete" "goal completed" "/goal complete"

echo "== /terminal =="
run "terminal list empty" "no terminal sessions" "/terminal list"
run "terminal create" "terminal session created" "/terminal create work"
run "terminal list shows" "work" "/terminal list"

echo "== /feedback =="
run "feedback stats" "feedback stats" "/feedback stats"
run "feedback remark" "feedback recorded" "/feedback remark \"loved it\""
run "feedback rate usage" "usage: feedback rate" "/feedback rate m1"

echo "== /autoskill =="
run "autoskill list empty" "no lessons yet" "/autoskill list"
run "autoskill learn" "lesson recorded" "/autoskill learn \"check before edit\" \"rust\""

echo "== /conductor =="
run "conductor list empty" "no conductors yet" "/conductor list"
run "conductor new" "conductor \"ship\" created" "/conductor new ship"
run "conductor phase" "phase \"backend\" added" "/conductor phase ship backend"
run "conductor task" "task \"schema\" added" "/conductor task ship backend schema"
run "conductor status" "0% complete" "/conductor status ship"

echo "== /autocontext =="
run "autocontext status" "no spill files" "/autocontext status"

echo "== /usage =="
run "usage shows tokens" "[usage:" "/usage"

echo "== /cap =="
run "cap toggles" "CAP" "/cap"

echo "== /settings =="
run "settings shows model" "model=" "/settings"

echo "== /clear =="
run "clear" "conversation cleared" "/clear"

echo "== /save =="
run "save session" "saved" "/save smoke-test-$(date +%s)"

echo "== unknown command =="
run "unknown command handled" "unknown command /bogus" "/bogus"

echo "== /quit =="
run "quit exits cleanly" "ByteAi" "/help" "/quit"

echo
echo "======================================"
echo "RESULTS: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  printf 'FAILED: %s\n' "${FAILED_TESTS[@]}"
  exit 1
fi
echo "ALL COMMANDS PASS"
