#!/bin/bash -eu

cd "$SRC/tokenix"
cargo fuzz build -O

FUZZ_TARGET_OUTPUT_DIR="fuzz/target/x86_64-unknown-linux-gnu/release"
for target in fuzz/fuzz_targets/*.rs; do
  target_name="$(basename "${target%.*}")"
  cp "$FUZZ_TARGET_OUTPUT_DIR/$target_name" "$OUT/"
done
