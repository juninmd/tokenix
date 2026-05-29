#!/usr/bin/env bash
# test_copilot_hook.sh — Verify Copilot hook integration
# Tests via stdin format: {"toolName": "...", "toolArgs": {...}}

set -euo pipefail

TOKENIX="${1:-${TOKENIX_BIN:-tokenix}}"
if [[ "$TOKENIX" != /* ]] && ! command -v "$TOKENIX" &>/dev/null; then
  TOKENIX="$(pwd)/$TOKENIX"
fi
TMPDIR_ROOT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

PASS=0; FAIL=0
_c() { printf "\033[%sm%s\033[0m\n" "$1" "$2"; }
pass() { PASS=$((PASS+1)); _c "32" "  PASS  $1"; }
fail() { FAIL=$((FAIL+1)); _c "31" "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; }
section() { echo; _c "1;34" "==> $1"; }

# ── Test repo ──────────────────────────────────────────────────────────────
REPO="$TMPDIR_ROOT/repo"
mkdir -p "$REPO/src"
echo "small" > "$REPO/src/small.rs"
for i in $(seq 0 219); do echo "fn func_${i}(x: i32) -> i32 { x + ${i} }"; done > "$REPO/src/large.rs"

git init -q "$REPO"
git -C "$REPO" config user.email "test@test.com"
git -C "$REPO" config user.name "Test"
cd "$REPO"
git add -A && git commit -q -m "init" 2>/dev/null || true
"$TOKENIX" index . --no-embed --cpu-profile low &>/dev/null

# ═════════════════════════════════════════════════════════════════════════════
section "Copilot hook: stdin protocol (only supported method)"

# Copilot agent mode sends via stdin: {"toolName": "view|grep|...", "toolArgs": {...}}
# (env vars COPILOT_TOOL_NAME/TOOL_INPUT removed — they don't normalize properly)

echo '{"toolName":"view","toolArgs":{"path":"src/small.rs"}}' | "$TOKENIX" hook >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "0" ]; then
  pass "view small file → exit 0"
else
  fail "view → exit $CODE (expected 0)"
fi

# grep pattern (2 words, no intercept expected)
echo '{"toolName":"grep","toolArgs":{"pattern":"fn main"}}' | "$TOKENIX" hook >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "0" ]; then
  pass "grep 2-word pattern → exit 0 (pass through)"
else
  fail "grep 2-word → exit $CODE (expected 0)"
fi

# Uppercase toolName (normalization)
echo '{"toolName":"VIEW","toolArgs":{"path":"src/small.rs"}}' | "$TOKENIX" hook >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "0" ]; then
  pass "VIEW (uppercase) → normalized and handled"
else
  fail "VIEW → exit $CODE (expected 0)"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "Copilot hook: installation"

TESTDIR="$TMPDIR_ROOT/install"
mkdir -p "$TESTDIR"
cd "$TESTDIR"
git init -q
git config user.email "t@t"
git config user.name "T"

# Install Copilot hooks
"$TOKENIX" install-hook --tool copilot 2>&1 | grep -q "Copilot" && {
  pass "install-hook --tool copilot succeeds"
} || {
  fail "install-hook did not mention Copilot"
}

# Verify hooks.json exists with correct structure
if [ -f ".github/hooks/hooks.json" ]; then
  if grep -q '"preToolUse"' ".github/hooks/hooks.json" && grep -q '"bash"' ".github/hooks/hooks.json"; then
    pass ".github/hooks/hooks.json has preToolUse and bash"
  else
    fail "hooks.json missing preToolUse or bash"
  fi
else
  fail ".github/hooks/hooks.json not created"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "Exit code contract"

# CRITICAL: never exit 1
echo '{}' | "$TOKENIX" hook >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "1" ]; then
  fail "CRITICAL: hook exits 1 on empty JSON (breaks Copilot)"
else
  pass "hook never exits 1 on invalid input (got $CODE)"
fi

echo '{"toolName":"UnknownTool"}' | "$TOKENIX" hook >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "1" ]; then
  fail "CRITICAL: hook exits 1 on unknown tool (breaks Copilot)"
else
  pass "hook never exits 1 on unknown tool (got $CODE)"
fi

# ═════════════════════════════════════════════════════════════════════════════
echo
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
_c "1;32" "  PASS: $PASS"
_c "1;31" "  FAIL: $FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  _c "1;31" "COPILOT INTEGRATION FAILED ($FAIL failures)"
  exit 1
else
  _c "1;32" "COPILOT INTEGRATION OK"
  exit 0
fi
