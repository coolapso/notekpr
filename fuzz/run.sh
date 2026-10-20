#!/usr/bin/env bash
# Run each fuzz target for a while on the stable toolchain: libFuzzer with
# coverage instrumentation through RUSTFLAGS (no nightly, no sanitizers).
# With nightly + cargo-fuzz, `cargo fuzz run <target>` works too (adds ASan).
#   fuzz/run.sh            # every target, 60 s each
#   fuzz/run.sh zip 300    # one target, 300 s
set -euo pipefail
cd "$(dirname "$0")"
secs="${2:-60}"
targets="${1:-markdown textdiff envelopes protocol zip crdt editor}"
export RUSTFLAGS="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
-Cllvm-args=-sanitizer-coverage-trace-compares"
# An explicit --target keeps RUSTFLAGS off build scripts and proc macros.
target="$(rustc -vV | sed -n 's/^host: //p')"
cargo build --release --bins --target "$target"
mkdir -p corpus artifacts
for t in $targets; do
  mkdir -p "corpus/$t"
  echo "== $t ($secs s)"
  "./target/$target/release/$t" "corpus/$t" -max_total_time="$secs" -artifact_prefix="artifacts/$t-" -print_final_stats=1 2>&1 | tail -3
done
