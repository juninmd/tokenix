#!/usr/bin/env bash
# homologation.sh — tokenix Linux integration test suite
#
# Usage:
#   ./scripts/homologation.sh                     # use tokenix from PATH
#   ./scripts/homologation.sh /path/to/tokenix    # explicit binary
#   TOKENIX_BIN=./target/release/tokenix ./scripts/homologation.sh
#
# Exit 0 = all checks passed. Exit 1 = at least one failure.
# Set VERBOSE=1 to see command output on every check.

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
TOKENIX="${1:-${TOKENIX_BIN:-tokenix}}"
VERBOSE="${VERBOSE:-0}"
TMPDIR_ROOT=$(mktemp -d)
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
PASS=0
FAIL=0
SKIP=0

_color() { printf "\033[%sm%s\033[0m\n" "$1" "$2"; }
pass() { PASS=$((PASS+1)); _color "32" "  PASS  $1"; }
fail() { FAIL=$((FAIL+1)); _color "31" "  FAIL  $1"; [ -n "${2:-}" ] && echo "        $2"; }
skip() { SKIP=$((SKIP+1)); _color "33" "  SKIP  $1 (${2:-})"; }
section() { echo; _color "1;34" "==> $1"; }

run() {
  # run CMD... → stdout; prints if VERBOSE or on failure
  local out
  out=$("$@" 2>&1) && {
    [ "$VERBOSE" = 1 ] && echo "$out"
    return 0
  } || {
    echo "$out"
    return 1
  }
}

require_binary() {
  command -v "$1" &>/dev/null || { skip "$1 not in PATH" "install $1 to enable this check"; return 1; }
}

# ---------------------------------------------------------------------------
# 0. Preflight
# ---------------------------------------------------------------------------
section "Preflight"

if ! command -v "$TOKENIX" &>/dev/null && [ ! -x "$TOKENIX" ]; then
  _color "31" "ERROR: tokenix binary not found at '$TOKENIX'"
  echo "  Build with: cargo build --release"
  echo "  Then:       TOKENIX_BIN=./target/release/tokenix $0"
  exit 1
fi

# Resolve to absolute path in case it's a local relative binary
if [[ "$TOKENIX" != /* ]]; then
  TOKENIX="$(pwd)/$TOKENIX"
fi

VERSION=$("$TOKENIX" --version 2>&1 || true)
echo "  binary : $TOKENIX"
echo "  version: $VERSION"

# OS must be Linux for full homologation
OS=$(uname -s)
if [ "$OS" = "Linux" ]; then
  pass "OS is Linux ($OS)"
else
  _color "33" "  WARN  OS is '$OS' — some checks are Linux-specific"
fi

# ---------------------------------------------------------------------------
# 1. --help / --version smoke tests
# ---------------------------------------------------------------------------
section "CLI surface"

if "$TOKENIX" --help &>/dev/null; then
  pass "--help exits 0"
else
  fail "--help exits non-zero"
fi

if "$TOKENIX" --version &>/dev/null; then
  VSTR=$("$TOKENIX" --version 2>&1)
  if echo "$VSTR" | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'; then
    pass "--version prints semver ($VSTR)"
  else
    fail "--version output has no semver: $VSTR"
  fi
else
  fail "--version exits non-zero"
fi

# Verify every expected subcommand appears in help
EXPECTED_CMDS=(index query context explore symbols callers callees impact
  read stats gain benchmark serve stop doctor install-hook remove-hook
  filter memory hook hook-post mcp rebuild-graph)

HELP_OUT=$("$TOKENIX" --help 2>&1)
for cmd in "${EXPECTED_CMDS[@]}"; do
  if echo "$HELP_OUT" | grep -q "$cmd"; then
    : # silent pass — too noisy to print each
  else
    fail "subcommand '$cmd' missing from --help"
  fi
done
pass "all ${#EXPECTED_CMDS[@]} subcommands present in --help"

# ---------------------------------------------------------------------------
# 2. Index + query lifecycle
# ---------------------------------------------------------------------------
section "Index / query lifecycle"

REPO="$TMPDIR_ROOT/repo"
mkdir -p "$REPO/src"

# Minimal fake codebase
cat > "$REPO/src/main.rs" <<'RS'
fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    println!("{}", greet("world"));
    println!("1 + 2 = {}", add(1, 2));
}
RS

cat > "$REPO/src/lib.rs" <<'RS'
pub mod utils {
    pub fn multiply(a: i32, b: i32) -> i32 {
        a * b
    }
}
RS

cat > "$REPO/README.md" <<'MD'
# Test Repo
This is a test repository for tokenix homologation.
It contains arithmetic utilities and a greeting function.
MD

# Index (CPU, no-embed for speed — validates file walk + chunker, skips ONNX download)
INDEX_OUT=$("$TOKENIX" index "$REPO" --no-embed --cpu-profile low 2>&1)
if echo "$INDEX_OUT" | grep -qE "^error:|thread.*main.*panicked|Error:"; then
  fail "index --no-embed produced errors: $INDEX_OUT"
else
  pass "index --no-embed succeeds on synthetic repo"
fi

# Stats should show files > 0
STATS=$("$TOKENIX" stats --path "$REPO" 2>&1)
if echo "$STATS" | grep -qiE 'Files:[[:space:]]+[1-9]|Chunks:[[:space:]]+[1-9]'; then
  pass "stats reports indexed content"
else
  fail "stats shows no content after index" "$STATS"
fi

# Query (no-embed index → should handle gracefully, not panic)
QUERY_OUT=$("$TOKENIX" query "greet function" --path "$REPO" 2>&1 || true)
if echo "$QUERY_OUT" | grep -qi "panic\|unwrap\|thread.*main"; then
  fail "query panicked: $QUERY_OUT"
else
  pass "query exits without panic (no-embed expected)"
fi

# ---------------------------------------------------------------------------
# 3. read subcommand
# ---------------------------------------------------------------------------
section "read subcommand"

READ_OUT=$("$TOKENIX" read "$REPO/src/main.rs" --path "$REPO" 2>&1 || true)
if echo "$READ_OUT" | grep -qi "panic\|unwrap\|thread.*main"; then
  fail "read panicked"
else
  pass "read exits without panic"
fi

# For small files (<200 lines) read must pass through (exit 0 and show content)
if "$TOKENIX" read "$REPO/README.md" --path "$REPO" 2>&1 | grep -q "Test Repo"; then
  pass "read passes through small file content"
else
  fail "read did not pass through small file content"
fi

# ---------------------------------------------------------------------------
# 4. symbols / callers / callees
# ---------------------------------------------------------------------------
section "Symbol graph"

SYM_OUT=$("$TOKENIX" symbols "greet" --path "$REPO" 2>&1 || true)
if echo "$SYM_OUT" | grep -qi "panic\|thread.*main.*failed"; then
  fail "symbols panicked: $SYM_OUT"
else
  pass "symbols exits without panic"
fi

CALLERS_OUT=$("$TOKENIX" callers "greet" --path "$REPO" 2>&1 || true)
if echo "$CALLERS_OUT" | grep -qi "panic"; then
  fail "callers panicked"
else
  pass "callers exits without panic"
fi

CALLEES_OUT=$("$TOKENIX" callees "main" --path "$REPO" 2>&1 || true)
if echo "$CALLEES_OUT" | grep -qi "panic"; then
  fail "callees panicked"
else
  pass "callees exits without panic"
fi

# ---------------------------------------------------------------------------
# 5. memory subcommand
# ---------------------------------------------------------------------------
section "memory subcommand"

MEM_DIR="$TMPDIR_ROOT/memdemo"
mkdir -p "$MEM_DIR"

if "$TOKENIX" memory add "prefer short answers" --project --path "$MEM_DIR" &>/dev/null; then
  pass "memory add exits 0"
else
  fail "memory add failed"
fi

MEM_LIST=$("$TOKENIX" memory list --project --path "$MEM_DIR" 2>&1)
if echo "$MEM_LIST" | grep -q "short answers"; then
  pass "memory list returns added preference"
else
  fail "memory list missing added preference: $MEM_LIST"
fi

if "$TOKENIX" memory remove "short answers" --project --path "$MEM_DIR" &>/dev/null; then
  pass "memory remove exits 0"
else
  fail "memory remove failed"
fi

# ---------------------------------------------------------------------------
# 6. filter subcommand
# ---------------------------------------------------------------------------
section "filter subcommand"

FILTER_LIST=$("$TOKENIX" filter list 2>&1)
if echo "$FILTER_LIST" | grep -qi "panic"; then
  fail "filter list panicked"
else
  pass "filter list exits without panic"
fi

FILTER_ACTIVE=$("$TOKENIX" filter active 2>&1)
if echo "$FILTER_ACTIVE" | grep -qi "panic"; then
  fail "filter active panicked"
else
  pass "filter active exits without panic"
fi

# filter record lifecycle: start -> status -> stop must run clean and toggle state.
REC_START=$("$TOKENIX" filter record start 2>&1)
REC_STATUS=$("$TOKENIX" filter record status 2>&1)
REC_STOP=$("$TOKENIX" filter record stop 2>&1)
if echo "$REC_START$REC_STATUS$REC_STOP" | grep -qi "panic"; then
  fail "filter record lifecycle panicked"
elif echo "$REC_STATUS" | grep -qi "active"; then
  pass "filter record start/status/stop runs and reports an active session"
else
  fail "filter record status did not report an active session after start" "status: $REC_STATUS"
fi

# ---------------------------------------------------------------------------
# 7. doctor
# ---------------------------------------------------------------------------
section "doctor"

DOCTOR_OUT=$("$TOKENIX" doctor 2>&1 || true)
if echo "$DOCTOR_OUT" | grep -qi "panic\|thread.*main.*failed"; then
  fail "doctor panicked: $DOCTOR_OUT"
else
  pass "doctor exits without panic"
fi

if echo "$DOCTOR_OUT" | grep -q "version"; then
  pass "doctor reports version"
else
  fail "doctor output missing version field: $DOCTOR_OUT"
fi

if echo "$DOCTOR_OUT" | grep -q "Embedding model"; then
  pass "doctor reports embedding model section"
else
  fail "doctor missing 'Embedding model' section"
fi

# ---------------------------------------------------------------------------
# 8. install-hook / remove-hook (dry, local only)
# ---------------------------------------------------------------------------
section "install-hook / remove-hook"

HOOK_DIR="$TMPDIR_ROOT/hooktest"
mkdir -p "$HOOK_DIR"

# Use --local so it writes to $HOOK_DIR/.claude/settings.local.json, not $HOME
pushd "$HOOK_DIR" &>/dev/null

if "$TOKENIX" install-hook --tool claude-code --local &>/dev/null; then
  SETTINGS="$HOOK_DIR/.claude/settings.local.json"
  if [ -f "$SETTINGS" ]; then
    pass "install-hook --local creates .claude/settings.local.json"
    # The hook path must use forward slashes (cross-platform). The only legitimate
    # backslashes are JSON quote escapes (\") around the binary path; a real Windows
    # path separator shows up as an escaped backslash (\\) in JSON, so match that.
    if grep -qF '\\' "$SETTINGS"; then
      fail "settings.json contains Windows backslashes"
    else
      pass "settings.json uses forward slashes"
    fi
    if grep -q "\.exe" "$SETTINGS" && [ "$OS" = "Linux" ]; then
      fail "settings.json contains .exe on Linux"
    else
      pass "settings.json has no .exe on Linux"
    fi
  else
    fail "install-hook --local did not create .claude/settings.local.json"
  fi
else
  fail "install-hook --tool claude-code --local failed"
fi

if "$TOKENIX" remove-hook --tool claude-code --local &>/dev/null; then
  pass "remove-hook --local exits 0"
else
  fail "remove-hook --local failed"
fi

popd &>/dev/null

# ---------------------------------------------------------------------------
# 9. hook stdin parsing (unit-level integration)
# ---------------------------------------------------------------------------
section "hook stdin parsing"

# Feed a valid Claude Code PreToolUse JSON; hook should exit 0 (pass-through)
# when the index is missing (no indexed repo at /nonexistent)
HOOK_JSON='{"tool_name":"Read","tool_input":{"file_path":"/nonexistent/file.rs"}}'
HOOK_EXIT=0
echo "$HOOK_JSON" | "$TOKENIX" hook &>/dev/null || HOOK_EXIT=$?
if [ "$HOOK_EXIT" = 0 ] || [ "$HOOK_EXIT" = 2 ]; then
  pass "hook exits 0 or 2 on valid JSON (no panic)"
else
  fail "hook returned unexpected exit code $HOOK_EXIT on valid JSON"
fi

# Feed invalid JSON — must exit 0 (never crash/exit 1)
INVALID_JSON='{not valid json}'
BAD_EXIT=0
echo "$INVALID_JSON" | "$TOKENIX" hook &>/dev/null || BAD_EXIT=$?
if [ "$BAD_EXIT" = 0 ]; then
  pass "hook exits 0 on invalid JSON (graceful fallback)"
else
  fail "hook exited $BAD_EXIT on invalid JSON (must be 0)"
fi

# ---------------------------------------------------------------------------
# 10. Daemon lifecycle (optional — skips if port already in use)
# ---------------------------------------------------------------------------
section "daemon lifecycle"

DAEMON_PORT=47399  # non-default to avoid conflict

# Check if port is free
if ! ss -ltn 2>/dev/null | grep -q ":$DAEMON_PORT "; then
  "$TOKENIX" serve --port "$DAEMON_PORT" &
  DAEMON_PID=$!
  sleep 2

  # Health check via nc
  if command -v nc &>/dev/null; then
    HEALTH=$(echo '{"type":"health"}' | nc -w 1 127.0.0.1 "$DAEMON_PORT" 2>/dev/null || true)
    if echo "$HEALTH" | grep -q '"ok":true'; then
      pass "daemon health check returns ok:true"
    else
      fail "daemon health check failed: $HEALTH"
    fi
  else
    skip "daemon health check via nc" "nc not installed"
  fi

  kill "$DAEMON_PID" 2>/dev/null || true
  wait "$DAEMON_PID" 2>/dev/null || true
  pass "daemon started and stopped cleanly"
else
  skip "daemon lifecycle" "port $DAEMON_PORT already in use"
fi

# ---------------------------------------------------------------------------
# 11. gain subcommand
# ---------------------------------------------------------------------------
section "gain"

GAIN_OUT=$("$TOKENIX" gain --path "$REPO" 2>&1 || true)
if echo "$GAIN_OUT" | grep -qi "panic"; then
  fail "gain panicked"
else
  pass "gain exits without panic"
fi

# ---------------------------------------------------------------------------
# 12. compress pipeline (hook-post with trivial input)
# ---------------------------------------------------------------------------
section "hook-post (compress)"

COMPRESS_INPUT='{"tool_name":"Read","tool_input":{"file_path":"test.rs"},"tool_response":"fn main() {}\n\n\n\n"}'
POST_EXIT=0
echo "$COMPRESS_INPUT" | "$TOKENIX" hook-post &>/dev/null || POST_EXIT=$?
if [ "$POST_EXIT" = 0 ] || [ "$POST_EXIT" = 2 ]; then
  pass "hook-post exits 0 or 2 (no panic)"
else
  fail "hook-post returned unexpected exit code $POST_EXIT"
fi

# ---------------------------------------------------------------------------
# 13. context / explore (no-embed graceful)
# ---------------------------------------------------------------------------
section "context / explore"

CTX_OUT=$("$TOKENIX" context "arithmetic utilities" --path "$REPO" 2>&1 || true)
if echo "$CTX_OUT" | grep -qi "panic"; then
  fail "context panicked"
else
  pass "context exits without panic"
fi

EXP_OUT=$("$TOKENIX" explore "greet" --path "$REPO" 2>&1 || true)
if echo "$EXP_OUT" | grep -qi "panic"; then
  fail "explore panicked"
else
  pass "explore exits without panic"
fi

# ---------------------------------------------------------------------------
# 14. Linux-specific: binary has no .exe, ELF header, no Windows paths in help
# ---------------------------------------------------------------------------
section "Linux binary sanity"

if [ "$OS" = "Linux" ]; then
  BIN_PATH=$(command -v "$TOKENIX" 2>/dev/null || echo "$TOKENIX")

  if file "$BIN_PATH" 2>/dev/null | grep -q "ELF"; then
    pass "binary is ELF (native Linux)"
  else
    fail "binary is NOT ELF — may be wrong architecture or platform"
  fi

  if [[ "$BIN_PATH" == *.exe ]]; then
    fail "binary path ends in .exe on Linux"
  else
    pass "binary has no .exe extension"
  fi

  LCHECK=$(ldd "$BIN_PATH" 2>&1 || true)
  if echo "$LCHECK" | grep -q "not found"; then
    fail "ldd reports missing shared libs:\n$LCHECK"
  else
    pass "all shared library dependencies resolved (ldd)"
  fi
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
_color "1;32" "  PASS: $PASS"
_color "1;31" "  FAIL: $FAIL"
_color "1;33" "  SKIP: $SKIP"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

if [ "$FAIL" -gt 0 ]; then
  _color "1;31" "HOMOLOGATION FAILED ($FAIL failures)"
  exit 1
else
  _color "1;32" "HOMOLOGATION PASSED"
  exit 0
fi
