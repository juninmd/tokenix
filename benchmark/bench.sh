#!/usr/bin/env bash
# benchmark/bench.sh
# Real tokenix benchmark — measures actual token savings for file-read interception.
#
# Usage:
#   cd /path/to/tokenix
#   bash benchmark/bench.sh
#
# Requirements: cargo in PATH. Ollama is NOT required for Phase 1.
#   Optional Phase 2 (semantic search) requires: Ollama + nomic-embed-text + indexed repo.

set -euo pipefail

# --------------------------------------------------------------------------
# Locate repo root
# --------------------------------------------------------------------------
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SAMPLES_DIR="$REPO_ROOT/benchmark/samples"
cd "$REPO_ROOT"

# --------------------------------------------------------------------------
# Colors
# --------------------------------------------------------------------------
BOLD=$'\e[1m'
GREEN=$'\e[32m'
CYAN=$'\e[36m'
YELLOW=$'\e[33m'
RESET=$'\e[0m'

# --------------------------------------------------------------------------
# Build
# --------------------------------------------------------------------------
echo ""
echo "${BOLD}=== tokenix benchmark ===${RESET}"
echo ""
echo "${YELLOW}Building tokenix (release)...${RESET}"
cargo build --release 2>&1 | grep -E "Compiling|Finished|error" || true
TOKENIX="$REPO_ROOT/target/release/tokenix"

if [[ ! -x "$TOKENIX" ]]; then
    echo "Build failed — cannot find $TOKENIX"
    exit 1
fi
echo ""

# --------------------------------------------------------------------------
# Helper: count tokens from text via same approximation tokenix uses
# --------------------------------------------------------------------------
token_count() {
    local text="$1"
    local len="${#text}"
    echo $(( (len + 3) / 4 ))
}

# --------------------------------------------------------------------------
# PHASE 1: File-read interception (outline mode) — no Ollama needed
# --------------------------------------------------------------------------
echo "${BOLD}Phase 1 — File-read interception (outline mode)${RESET}"
echo "Simulates an AI assistant reading source files."
echo "tokenix intercepts reads on files ≥200 lines and returns a symbol outline."
echo ""

# Combine: tokenix own source files + sample files
ALL_FILES=()
while IFS= read -r -d '' f; do
    ALL_FILES+=("$f")
done < <(find "$REPO_ROOT/src" "$SAMPLES_DIR" -type f \
    \( -name "*.rs" -o -name "*.py" -o -name "*.ts" -o -name "*.go" \) \
    -print0 2>/dev/null | sort -z)

printf "${BOLD}%-42s %6s %10s %11s %8s${RESET}\n" \
    "File" "Lines" "RawTok" "HookTok" "Saved"
printf '%s\n' "$(printf -- '-%.0s' {1..80})"

total_raw=0
total_hook=0
intercepted_count=0
passed_count=0

for abs in "${ALL_FILES[@]}"; do
    rel="${abs#$REPO_ROOT/}"
    content="$(cat "$abs")"
    lines="$(echo "$content" | wc -l)"
    raw_tok="$(token_count "$content")"

    if [[ "$lines" -ge 200 ]]; then
        # tokenix read generates the same outline the hook would return
        hook_out="$("$TOKENIX" read "$abs" 2>/dev/null)"
        hook_tok="$(token_count "$hook_out")"
        saved_pct=$(awk "BEGIN {printf \"%.1f\", (1 - $hook_tok / $raw_tok) * 100}")
        label="${GREEN}intercepted${RESET}"
        printf "%-42s %6d %10d %11d %7s%%  ${GREEN}✓${RESET}\n" \
            "$rel" "$lines" "$raw_tok" "$hook_tok" "$saved_pct"
        total_raw=$((total_raw + raw_tok))
        total_hook=$((total_hook + hook_tok))
        intercepted_count=$((intercepted_count + 1))
    else
        printf "%-42s %6d %10d %11s %8s  (pass — <200 lines)\n" \
            "$rel" "$lines" "$raw_tok" "-" "-"
        passed_count=$((passed_count + 1))
    fi
done

printf '%s\n' "$(printf -- '-%.0s' {1..80})"

if [[ "$total_raw" -gt 0 ]]; then
    total_saved_pct=$(awk "BEGIN {printf \"%.1f\", (1 - $total_hook / $total_raw) * 100}")
    total_saved_tok=$((total_raw - total_hook))
    printf "${BOLD}%-42s %6s %10d %11d %7s%%${RESET}\n" \
        "TOTAL (intercepted files)" "" "$total_raw" "$total_hook" "$total_saved_pct"
    echo ""
    echo "${CYAN}${BOLD}Summary:${RESET}"
    echo "  Files intercepted : $intercepted_count"
    echo "  Files passed      : $passed_count"
    echo "  Tokens consumed   : $total_raw  (without tokenix)"
    echo "  Tokens consumed   : $total_hook  (with tokenix)"
    echo "  Tokens saved      : ${GREEN}${BOLD}$total_saved_tok${RESET}"
    echo "  Reduction         : ${GREEN}${BOLD}${total_saved_pct}%${RESET}"
    est_cost=$(awk "BEGIN {printf \"%.4f\", $total_saved_tok / 1000 * 0.003}")
    echo "  Est. cost saved   : \$$est_cost (at \$3/M input tokens)"
fi

# --------------------------------------------------------------------------
# PHASE 2: Semantic search — requires Ollama + tokenix index
# --------------------------------------------------------------------------
echo ""
echo "${BOLD}Phase 2 — Semantic search (requires Ollama + indexed repo)${RESET}"

DB="$REPO_ROOT/.tokenix/index.db"
if [[ ! -f "$DB" ]]; then
    echo "${YELLOW}  Skipped: no index found. Run: tokenix index .${RESET}"
    echo "  (Ollama must be running: ollama serve)"
    echo "  (Model must be pulled:   ollama pull nomic-embed-text)"
else
    age_secs=$(( $(date +%s) - $(stat -c %Y "$DB" 2>/dev/null || stat -f %m "$DB") ))
    if [[ "$age_secs" -gt 3600 ]]; then
        echo "${YELLOW}  Warning: index is ${age_secs}s old (>1h). Re-run: tokenix index .${RESET}"
    fi

    echo "  Simulating Grep interception for semantic queries..."
    echo ""

    QUERIES=(
        "how does authentication work"
        "database connection and query execution"
        "error handling and validation"
        "user registration and password hashing"
    )

    for q in "${QUERIES[@]}"; do
        echo "  Query: \"$q\""
        result="$("$TOKENIX" query "$q" --budget 2000 2>/dev/null)"
        tok="$(token_count "$result")"
        echo "  Returned: ~${tok} tokens"
        echo ""
    done
fi

echo "${BOLD}=== benchmark complete ===${RESET}"
echo ""
