#!/usr/bin/env bash
# Build the browser client with the Rust toolchain only (no npm):
#   cargo (wasm32-unknown-unknown) + wasm-bindgen CLI (version must match the crate).
# Output: dist/web (serve with `notekpr-server --web-dir dist/web`).
# NOTEKPR_VERSION, if set, is the version the app says it is (the Dockerfile
# sets it for releases); `dev` otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${PROFILE:-wasm-release}"
OUT="${OUT:-dist/web}"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"

want=$(grep -E '^wasm-bindgen = "=' crates/web/Cargo.toml | sed -E 's/.*"=([0-9.]+)".*/\1/')
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI $have does not match crate version $want." >&2
  echo "Install it with: cargo install wasm-bindgen-cli --version $want" >&2
  exit 1
fi

cargo build -p notekpr-web --target wasm32-unknown-unknown --profile "$PROFILE"
dir="$PROFILE"; [ "$PROFILE" = "dev" ] && dir=debug
rm -rf "$OUT"
mkdir -p "$OUT/pkg"
# Function names make an unoptimized build ~7x bigger and take the browser
# seconds to load; WASM_NAMES=1 keeps them for readable stack traces.
# (Optimized builds are stripped by their profile anyway.)
names=(--remove-name-section)
[ "${WASM_NAMES:-}" = 1 ] && names=()
wasm-bindgen --target web --no-typescript "${names[@]}" --out-dir "$OUT/pkg" \
  "$TARGET_DIR/wasm32-unknown-unknown/$dir/notekpr_web.wasm"
if command -v wasm-opt >/dev/null 2>&1 && [ "$PROFILE" != "dev" ]; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int -o "$OUT/pkg/notekpr_web_bg.wasm" "$OUT/pkg/notekpr_web_bg.wasm"
fi
cp -r crates/web/static/* "$OUT/"
ls -la "$OUT/pkg"
echo "web client built into $OUT"
