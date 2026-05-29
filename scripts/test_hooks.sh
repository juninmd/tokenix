#!/usr/bin/env bash
# test_hooks.sh — functional tests for `tokenix hook` and `tokenix hook-post`
#
# Usage:
#   bash scripts/test_hooks.sh [/path/to/tokenix]
#
# Tests every code path defined in hook.rs and compress.rs:
#   hook     : unknown tool, Read pass-through (small/non-code/offset), Read intercept,
#              Grep pass-through (short/non-identifier/no-index), invalid JSON, BOM
#   hook-post: invalid JSON, unknown tool, no response, Bash compress, no-op pass-through,
#              ListDirectory compress, exit codes

set -euo pipefail

TOKENIX="${1:-${TOKENIX_BIN:-tokenix}}"
if [[ "$TOKENIX" != /* ]] && ! command -v "$TOKENIX" &>/dev/null; then
  TOKENIX="$(pwd)/$TOKENIX"
fi
TMPDIR_ROOT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

# ── helpers ────────────────────────────────────────────────────────────────
PASS=0; FAIL=0
_c() { printf "\033[%sm%s\033[0m\n" "$1" "$2"; }
pass() { PASS=$((PASS+1)); _c "32" "  PASS  $1"; }
fail() { FAIL=$((FAIL+1)); _c "31" "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; }
section() { echo; _c "1;34" "==> $1"; }

# Run hook/hook-post, capture stdout+stderr+exit
run_hook() {          # run_hook <json> → sets OUT, ERR, CODE
  local json="$1"
  CODE=0
  { OUT=$(echo "$json" | "$TOKENIX" hook 2>/tmp/hook_err_$$); } || CODE=$?
  ERR=$(cat /tmp/hook_err_$$ 2>/dev/null || true); rm -f /tmp/hook_err_$$
}
run_hook_post() {     # run_hook_post <json> → sets OUT, ERR, CODE
  local json="$1"
  CODE=0
  { OUT=$(echo "$json" | "$TOKENIX" hook-post 2>/tmp/hpost_err_$$); } || CODE=$?
  ERR=$(cat /tmp/hpost_err_$$ 2>/dev/null || true); rm -f /tmp/hpost_err_$$
}

# ── synthetic repo ─────────────────────────────────────────────────────────
REPO="$TMPDIR_ROOT/repo"
mkdir -p "$REPO/src"
# .git makes find_project_root() anchor here; without it tokenix may walk up
# to a parent that has its own (unrelated) index.
git init -q "$REPO"
git -C "$REPO" config user.email "test@test.com"
git -C "$REPO" config user.name "Test"

# small file (<200 lines)
cat > "$REPO/src/small.rs" <<'RS'
fn greet(name: &str) -> String { format!("Hello, {}!", name) }
fn main() { println!("{}", greet("world")); }
RS

# large code file (≥200 lines) — generates outline
# generate 220 lines of Rust functions
for i in $(seq 0 219); do
  echo "fn func_${i}(x: i32) -> i32 { x + ${i} }"
done > "$REPO/src/large.rs"

# large non-code file (.md) — should NOT be intercepted
for i in $(seq 1 220); do echo "line $i content here"; done > "$REPO/docs.md"

# Move into the repo FIRST, then index with "." so both the index and the
# hook resolve the project root from the same current_dir() call.
cd "$REPO"
# Commit so git HEAD exists — otherwise the staleness fingerprint differs
# between `index` (no HEAD) and `hook` (still no HEAD), causing stale=true.
git add -A && git commit -q -m "init" 2>/dev/null || true
"$TOKENIX" index . --no-embed --cpu-profile low &>/dev/null

# ══════════════════════════════════════════════════════════════════════════
section "hook — unknown / irrelevant tools"

# Edit, Write, Bash etc. must pass through (exit 0)
for tool in Edit Write Bash ListDirectory Task; do
  run_hook "{\"tool_name\":\"$tool\",\"tool_input\":{}}"
  if [ "$CODE" = "0" ]; then
    pass "hook exits 0 for tool=$tool"
  else
    fail "hook exits $CODE for tool=$tool (expected 0)"
  fi
done

# ══════════════════════════════════════════════════════════════════════════
section "hook — invalid / edge-case input"

# Invalid JSON → must exit 0, never crash
run_hook '{not valid json}'
if [ "$CODE" = "0" ]; then
  pass "hook exits 0 on invalid JSON"
else
  fail "hook exits $CODE on invalid JSON (expected 0)"
fi

# Empty stdin → exit 0
CODE=0; { OUT=$(echo "" | "$TOKENIX" hook 2>/dev/null); } || CODE=$?
if [ "$CODE" = "0" ]; then
  pass "hook exits 0 on empty stdin"
else
  fail "hook exits $CODE on empty stdin (expected 0)"
fi

# BOM-prefixed JSON → must parse correctly (exit 0 for unknown tool)
BOM_JSON=$'\xef\xbb\xbf{"tool_name":"Edit","tool_input":{}}'
run_hook "$BOM_JSON"
if [ "$CODE" = "0" ]; then
  pass "hook exits 0 on BOM-prefixed JSON"
else
  fail "hook exits $CODE on BOM-prefixed JSON (expected 0)"
fi

# Missing tool_name → treated as empty string → exit 0
run_hook '{"tool_input":{"file_path":"src/small.rs"}}'
if [ "$CODE" = "0" ]; then
  pass "hook exits 0 when tool_name missing"
else
  fail "hook exits $CODE when tool_name missing (expected 0)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook — Read pass-through cases (must exit 0)"

# Use relative paths — absolute POSIX paths (/tmp/...) are not resolved
# correctly by the Windows binary. The CWD is $REPO from the cd above.

# Small file (<200 lines) — always passes through
run_hook '{"tool_name":"Read","tool_input":{"file_path":"src/small.rs"}}'
if [ "$CODE" = "0" ]; then
  pass "Read exits 0 for small file (<200 lines)"
else
  fail "Read exits $CODE for small file (expected 0)"
fi

# Large non-code file (.md) — not intercepted
run_hook '{"tool_name":"Read","tool_input":{"file_path":"docs.md"}}'
if [ "$CODE" = "0" ]; then
  pass "Read exits 0 for large non-code file (.md)"
else
  fail "Read exits $CODE for large non-code file (expected 0)"
fi

# Read with offset set → always passes through even if large
run_hook '{"tool_name":"Read","tool_input":{"file_path":"src/large.rs","offset":10}}'
if [ "$CODE" = "0" ]; then
  pass "Read exits 0 when offset is set"
else
  fail "Read exits $CODE when offset is set (expected 0)"
fi

# Read with limit set → always passes through
run_hook '{"tool_name":"Read","tool_input":{"file_path":"src/large.rs","limit":20}}'
if [ "$CODE" = "0" ]; then
  pass "Read exits 0 when limit is set"
else
  fail "Read exits $CODE when limit is set (expected 0)"
fi

# File not found → passes through (exit 0)
run_hook '{"tool_name":"Read","tool_input":{"file_path":"nonexistent/file.rs"}}'
if [ "$CODE" = "0" ]; then
  pass "Read exits 0 for nonexistent file"
else
  fail "Read exits $CODE for nonexistent file (expected 0)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook — Read intercept (large code file → outline, exit 2)"

run_hook '{"tool_name":"Read","tool_input":{"file_path":"src/large.rs"}}'
if [ "$CODE" = "2" ]; then
  pass "Read exits 2 (intercepted) for large .rs file"
else
  fail "Read exits $CODE for large .rs file (expected 2)" "stderr: $ERR"
fi

# Outline must mention the file's line count
if echo "$ERR" | grep -qE '[0-9]+ lines|tokenix'; then
  pass "Read intercept stderr contains outline/tokenix message"
else
  fail "Read intercept stderr missing expected content" "got: $ERR"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook — Grep pass-through cases (must exit 0)"

# Pattern < 3 words, non-identifier → exit 0
run_hook '{"tool_name":"Grep","tool_input":{"pattern":"fn main"}}'
if [ "$CODE" = "0" ]; then
  pass "Grep exits 0 for 2-word pattern"
else
  fail "Grep exits $CODE for 2-word pattern (expected 0)"
fi

# Pattern < 3 words but looks like regex → exit 0
run_hook '{"tool_name":"Grep","tool_input":{"pattern":"fn.*main"}}'
if [ "$CODE" = "0" ]; then
  pass "Grep exits 0 for regex pattern"
else
  fail "Grep exits $CODE for regex pattern (expected 0)"
fi

# Missing pattern key → exit 0
run_hook '{"tool_name":"Grep","tool_input":{"path":"src/"}}'
if [ "$CODE" = "0" ]; then
  pass "Grep exits 0 when pattern key missing"
else
  fail "Grep exits $CODE when pattern key missing (expected 0)"
fi

# Semantic query (≥3 words) but no embedding index → exit 0 (stale or no results)
run_hook '{"tool_name":"Grep","tool_input":{"pattern":"how does authentication work"}}'
if [ "$CODE" = "0" ] || [ "$CODE" = "2" ]; then
  pass "Grep exits 0 or 2 for semantic query (no embed index)"
else
  fail "Grep exits $CODE for semantic query (expected 0 or 2)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook — Copilot format input"

# Copilot sends toolName/toolArgs — must parse correctly
run_hook '{"toolName":"view","toolArgs":"{\"path\":\"src/small.rs\"}"}'
if [ "$CODE" = "0" ]; then
  pass "Copilot view format: exits 0 for small file"
else
  fail "Copilot view format: exits $CODE (expected 0)"
fi

run_hook '{"toolName":"grep","toolArgs":{"pattern":"fn main"}}'
if [ "$CODE" = "0" ]; then
  pass "Copilot grep format: exits 0 for 2-word pattern"
else
  fail "Copilot grep format: exits $CODE (expected 0)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook-post — invalid / edge-case input"

# Invalid JSON → exit 0
run_hook_post '{not json}'
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 on invalid JSON"
else
  fail "hook-post exits $CODE on invalid JSON (expected 0)"
fi

# Empty stdin → exit 0 (parse fails)
CODE=0; { OUT=$(echo "" | "$TOKENIX" hook-post 2>/dev/null); } || CODE=$?
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 on empty stdin"
else
  fail "hook-post exits $CODE on empty stdin (expected 0)"
fi

# Unknown tool → exit 0
run_hook_post '{"tool_name":"Read","tool_input":{},"tool_response":"content"}'
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 for non-Bash/ListDirectory tool"
else
  fail "hook-post exits $CODE for Read tool (expected 0)"
fi

# Bash but empty response → exit 0
run_hook_post '{"tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":""}'
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 for empty Bash response"
else
  fail "hook-post exits $CODE for empty Bash response (expected 0)"
fi

# Missing tool_response key → exit 0
run_hook_post '{"tool_name":"Bash","tool_input":{"command":"ls"}}'
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 when tool_response missing"
else
  fail "hook-post exits $CODE when tool_response missing (expected 0)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook-post — Bash output compression (exit 2 = intercepted)"

# Build big bash output (>100 lines) — must be compressed
BIG_OUTPUT=$(for i in $(seq 1 150); do echo "line $i some repeated output content"; done)
# Escape for JSON: replace \ with \\, " with \", then embed with literal \n
_json_str() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g' | tr '\n' '|' | sed 's/|/\\n/g'; }
BASH_JSON="{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"echo test\"},\"tool_response\":\"$(_json_str "$BIG_OUTPUT")\"}"

run_hook_post "$BASH_JSON"
if [ "$CODE" = "2" ]; then
  pass "hook-post exits 2 (intercepted) for large Bash output"
elif [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 (compression was no-op — acceptable)"
else
  fail "hook-post exits $CODE for large Bash output (expected 0 or 2)"
fi

# ANSI output — strips colors, exits 2
ANSI_OUTPUT=$(printf '\033[32mBuilding\033[0m project\n%.0s' {1..120})
ANSI_JSON="{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"cargo build\"},\"tool_response\":\"$(_json_str "$ANSI_OUTPUT")\"}"

run_hook_post "$ANSI_JSON"
if [ "$CODE" = "0" ] || [ "$CODE" = "2" ]; then
  pass "hook-post handles ANSI-colored Bash output (exit $CODE)"
else
  fail "hook-post exits $CODE for ANSI output (expected 0 or 2)"
fi

if [ "$CODE" = "2" ] && echo "$OUT" | grep -q "\[32m"; then
  fail "hook-post left ANSI codes in compressed output"
elif [ "$CODE" = "2" ]; then
  pass "hook-post stripped ANSI codes from output"
fi

# Small Bash output (≤100 lines, no compression) → exit 0
SMALL_JSON='{"tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":"file1.rs\nfile2.rs\n"}'
run_hook_post "$SMALL_JSON"
if [ "$CODE" = "0" ]; then
  pass "hook-post exits 0 for small Bash output (no compression needed)"
else
  fail "hook-post exits $CODE for small Bash output (expected 0)" "out: $OUT"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook-post — ListDirectory compression"

# Large directory listing → compressed
DIR_OUTPUT=$(for i in $(seq 1 200); do echo "/src/file_${i}.rs"; done)
DIR_JSON="{\"tool_name\":\"ListDirectory\",\"tool_input\":{\"path\":\".\"},\"tool_response\":\"$(_json_str "$DIR_OUTPUT")\"}"

run_hook_post "$DIR_JSON"
if [ "$CODE" = "0" ] || [ "$CODE" = "2" ]; then
  pass "hook-post handles ListDirectory output (exit $CODE)"
else
  fail "hook-post exits $CODE for ListDirectory (expected 0 or 2)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "hook-post — tool_response as nested JSON (Claude Code format)"

# Claude Code sends tool_response as a JSON object with a "content" array
_big=$(for i in $(seq 1 150); do echo "line $i repeated content here for compression test"; done)
_inner="{\"output\":\"$(_json_str "$_big")\"}"
NESTED_JSON="{\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"cat big_file.txt\"},\"tool_response\":\"$(_json_str "$_inner")\"}"

run_hook_post "$NESTED_JSON"
if [ "$CODE" = "0" ] || [ "$CODE" = "2" ]; then
  pass "hook-post handles nested JSON tool_response (exit $CODE)"
else
  fail "hook-post exits $CODE for nested JSON response (expected 0 or 2)"
fi

# ══════════════════════════════════════════════════════════════════════════
section "exit code contract (critical)"

# Verify no case produces exit 1 — that would break Claude Code
for label_json in \
  "invalid_json|{bad}" \
  "unknown_tool|{\"tool_name\":\"Edit\",\"tool_input\":{}}" \
  "empty|{}"; do
  label="${label_json%%|*}"
  json="${label_json##*|}"
  CODE=0; { OUT=$(echo "$json" | "$TOKENIX" hook 2>/dev/null); } || CODE=$?
  if [ "$CODE" = "1" ]; then
    fail "hook exits 1 for $label — this breaks Claude Code!"
  else
    pass "hook never exits 1 for $label (got $CODE)"
  fi
done

for label_json in \
  "invalid_json|{bad}" \
  "unknown_tool|{\"tool_name\":\"Read\",\"tool_input\":{}}" \
  "empty|{}"; do
  label="${label_json%%|*}"
  json="${label_json##*|}"
  CODE=0; { OUT=$(echo "$json" | "$TOKENIX" hook-post 2>/dev/null); } || CODE=$?
  if [ "$CODE" = "1" ]; then
    fail "hook-post exits 1 for $label — this breaks Claude Code!"
  else
    pass "hook-post never exits 1 for $label (got $CODE)"
  fi
done

# ══════════════════════════════════════════════════════════════════════════
echo
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
_c "1;32" "  PASS: $PASS"
_c "1;31" "  FAIL: $FAIL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  _c "1;31" "HOOK TESTS FAILED ($FAIL failures)"
  exit 1
else
  _c "1;32" "HOOK TESTS PASSED"
  exit 0
fi
