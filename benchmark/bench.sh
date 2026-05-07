#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo
echo "=== tokenix benchmark wrapper ==="
echo "Running the built-in real benchmark from the current checkout."
echo

cargo run -- benchmark --refresh-index
