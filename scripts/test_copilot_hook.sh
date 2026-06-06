#!/usr/bin/env bash
# test_copilot_hook.sh — Verify Copilot hook integration
# Tests via stdin format: {"toolName": "...", "toolArgs": {...}}

set -euo pipefail

TOKENIX="${1:-${TOKENIX_BIN:-tokenix}}"
# Make the binary path absolute before we cd into the temp repo. A relative path
# with a slash (e.g. ./bin/tokenix) stops resolving after cd; a bare name not on
# PATH also needs anchoring. A bare name on PATH is left for PATH lookup.
if [[ "$TOKENIX" != /* ]] && { [[ "$TOKENIX" == */* ]] || ! command -v "$TOKENIX" &>/dev/null; }; then
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
section "Copilot hook-post: Bash output compression (modifiedResult JSON)"

# Copilot postToolUse sends: {"toolName":"bash","toolArgs":{...},"toolResult":{"textResultForLlm":"..."}}
# tokenix must reply on stdout with {"modifiedResult":{"resultType":"success","textResultForLlm":"<compressed>"}}
# and exit 0 (Copilot parses stdout JSON only on exit 0).

# git status → git-status filter strips the "no changes added to commit" help hint.
GIT_STATUS='On branch main\nYour branch is up to date.\n\nChanges not staged for commit:\n\tmodified:   src/a.rs\n\tmodified:   src/b.rs\n\tmodified:   src/c.rs\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n'
PAYLOAD="{\"toolName\":\"bash\",\"toolArgs\":{\"command\":\"git status\"},\"toolResult\":{\"resultType\":\"success\",\"textResultForLlm\":\"$GIT_STATUS\"}}"
OUT=$(printf '%s' "$PAYLOAD" | "$TOKENIX" hook-post 2>/dev/null); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q '"modifiedResult"' && echo "$OUT" | grep -q '"textResultForLlm"'; then
  pass "git status → modifiedResult JSON, exit 0"
else
  fail "git status post-hook (code=$CODE)" "out: $OUT"
fi
if echo "$OUT" | grep -q "no changes added to commit"; then
  fail "git status filter did not strip help-hint line" "out: $OUT"
else
  pass "git status RTK filter stripped help-hint line"
fi

# git diff → git-diff filter strips diff headers (diff --git, index, ---, +++).
GIT_DIFF='diff --git a/src/a.rs b/src/a.rs\nindex 1234567..89abcde 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,3 @@\n-old line\n+new line\n context\n'
PAYLOAD="{\"toolName\":\"bash\",\"toolArgs\":{\"command\":\"git diff\"},\"toolResult\":{\"textResultForLlm\":\"$GIT_DIFF\"}}"
OUT=$(printf '%s' "$PAYLOAD" | "$TOKENIX" hook-post 2>/dev/null); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q '"modifiedResult"'; then
  pass "git diff → modifiedResult JSON, exit 0"
else
  fail "git diff post-hook (code=$CODE)" "out: $OUT"
fi
if echo "$OUT" | grep -q "diff --git"; then
  fail "git diff filter did not strip 'diff --git' header" "out: $OUT"
else
  pass "git diff RTK filter stripped 'diff --git' header"
fi

# toolArgs as a JSON-encoded string (Copilot sometimes does this).
PAYLOAD='{"toolName":"bash","toolArgs":"{\"command\":\"git status\"}","toolResult":{"textResultForLlm":"On branch main\nno changes added to commit (use \"git add\")\n"}}'
OUT=$(printf '%s' "$PAYLOAD" | "$TOKENIX" hook-post 2>/dev/null); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q '"modifiedResult"'; then
  pass "string-encoded toolArgs → command parsed, compressed"
else
  fail "string-encoded toolArgs post-hook (code=$CODE)" "out: $OUT"
fi

# Unknown post tool (view) → pass through: exit 0, no modifiedResult.
PAYLOAD='{"toolName":"view","toolArgs":{"path":"x"},"toolResult":{"textResultForLlm":"small"}}'
OUT=$(printf '%s' "$PAYLOAD" | "$TOKENIX" hook-post 2>/dev/null); CODE=$?
if [ "$CODE" = "0" ] && ! echo "$OUT" | grep -q "modifiedResult"; then
  pass "view post-hook passes through (exit 0, no modifiedResult)"
else
  fail "view post-hook should pass through (code=$CODE)" "out: $OUT"
fi

# Post-hook must also never exit 1 (would corrupt Copilot's result handling).
printf '%s' '{not json}' | "$TOKENIX" hook-post >/dev/null 2>&1; CODE=$?
if [ "$CODE" = "1" ]; then
  fail "CRITICAL: hook-post exits 1 on invalid JSON (breaks Copilot)"
else
  pass "hook-post never exits 1 on invalid input (got $CODE)"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "Copilot hook: installation"

TESTDIR="$TMPDIR_ROOT/install"
mkdir -p "$TESTDIR"
cd "$TESTDIR"
git init -q
git config user.email "t@t"
git config user.name "T"

# Install Copilot hooks (local so .github/hooks/hooks.json is created)
"$TOKENIX" install-hook --tool copilot --local 2>&1 | grep -q "Copilot" && {
  pass "install-hook --tool copilot succeeds"
} || {
  fail "install-hook did not mention Copilot"
}

# Verify hooks.json exists with correct structure
if [ -f ".github/hooks/hooks.json" ]; then
  if grep -q '"PreToolUse"' ".github/hooks/hooks.json" && grep -q '"windows"' ".github/hooks/hooks.json"; then
    pass ".github/hooks/hooks.json has PreToolUse and windows"
  else
    fail "hooks.json missing PreToolUse or windows"
  fi
  if grep -q '"timeout"' ".github/hooks/hooks.json" && grep -q '"command"' ".github/hooks/hooks.json"; then
    pass ".github/hooks/hooks.json uses documented timeout and command fields"
  else
    fail "hooks.json missing documented timeout or command fields"
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
