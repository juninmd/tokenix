#!/usr/bin/env bash
# Local pre-push gate: the same checks, in the same order, as rust.yml and
# homologation.yml. Machine-wide state goes to a throwaway TOKENIX_HOME, so a
# run never touches ~/.tokenix (trust store, hook log, daemon token).
#
#   ./scripts/verify.sh            # fmt, clippy, tests, release build, CLI + hooks
#   ./scripts/verify.sh --models   # also the model-gated tests (downloads ~130 MB once)
set -euo pipefail
cd "$(dirname "$0")/.."

export CI=true
TOKENIX_HOME="$(mktemp -d)"
export TOKENIX_HOME
trap 'rm -rf "$TOKENIX_HOME"' EXIT

step() { printf '\n==> %s\n' "$*"; }

step "fmt";    cargo fmt --all -- --check
step "clippy"; cargo clippy --all-targets --locked --quiet -- -D warnings
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    # release.yml ships a `--features directml` Windows binary.
    step "clippy (directml)"
    cargo clippy --locked --quiet --features directml -- -D warnings
    ;;
esac
step "test";   cargo test --locked --quiet
if [[ "${1:-}" == "--models" ]]; then
  step "model-gated tests"
  cargo test --locked --quiet --features model-tests
fi

step "release build"
cargo build --release --locked --quiet
bin="$PWD/target/release/tokenix"
[[ -x "$bin" ]] || bin="$bin.exe"

step "CLI homologation"; bash scripts/homologation.sh "$bin"
step "hook contracts";   bash scripts/test_hooks.sh "$bin"
step "Copilot hook";     bash scripts/test_copilot_hook.sh "$bin"

printf '\nverify: every gate green\n'
