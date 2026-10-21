#!/usr/bin/env bash
# Build the load test's live dashboard with the Rust toolchain only (no npm):
#   cargo (wasm32-unknown-unknown) + the wasm-bindgen CLI, as for the web client.
# Output: dist/load (the load test serves it, `notekpr-load --dashboard-dir`).
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${PROFILE:-wasm-release}"
OUT="${OUT:-dist/load}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"

want=$(grep -E '^wasm-bindgen = "=' crates/load-ui/Cargo.toml | sed -E 's/.*"=([0-9.]+)".*/\1/')
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI $have does not match crate version $want." >&2
  echo "Install it with: cargo install wasm-bindgen-cli --version $want" >&2
  exit 1
fi

cargo build -p notekpr-load-ui --target wasm32-unknown-unknown --profile "$PROFILE"
dir="$PROFILE"; [ "$PROFILE" = "dev" ] && dir=debug
rm -rf "$OUT"
mkdir -p "$OUT/pkg"
wasm-bindgen --target web --no-typescript --out-dir "$OUT/pkg" \
  "$TARGET_DIR/wasm32-unknown-unknown/$dir/notekpr_load_ui.wasm"
if command -v wasm-opt >/dev/null 2>&1 && [ "$PROFILE" != "dev" ]; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int \
    -o "$OUT/pkg/notekpr_load_ui_bg.wasm" "$OUT/pkg/notekpr_load_ui_bg.wasm"
fi
cp crates/load-ui/static/* "$OUT/"
echo "load test dashboard built into $OUT"
