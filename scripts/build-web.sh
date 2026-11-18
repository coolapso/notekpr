#!/usr/bin/env bash
# Build the browser client with the Rust toolchain only (no npm):
#   cargo (wasm32-unknown-unknown) + wasm-bindgen CLI (version must match the crate).
# Output: dist/web (serve with `notekpr-server --web-dir dist/web`): the app
# (pkg/notekpr_web*) and the diagram module it loads when needed
# (pkg/notekpr_diagram*).
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

# The diagram module (crates/diagram: merman, Mermaid in Rust) is a
# WebAssembly module of its own, which the app loads when a note first shows
# a diagram, and a cargo workspace of its own (docs/development.md, "Diagrams
# (Mermaid)"). It is always optimized (DIAGRAM_PROFILE): unoptimized it is
# several times bigger and slow, and it rarely changes, so cargo keeps the
# build.
DIAGRAM_PROFILE="${DIAGRAM_PROFILE:-wasm-release}"
cargo build -p notekpr-web --target wasm32-unknown-unknown --profile "$PROFILE"
cargo build --manifest-path crates/diagram/Cargo.toml --target-dir "$TARGET_DIR" --lib \
  --target wasm32-unknown-unknown --profile "$DIAGRAM_PROFILE"
dir="$PROFILE"; [ "$PROFILE" = "dev" ] && dir=debug
ddir="$DIAGRAM_PROFILE"; [ "$DIAGRAM_PROFILE" = "dev" ] && ddir=debug
rm -rf "$OUT"
mkdir -p "$OUT/pkg"
# Function names make an unoptimized build ~7x bigger and take the browser
# seconds to load; WASM_NAMES=1 keeps them for readable stack traces.
# (Optimized builds are stripped by their profile anyway.)
names=(--remove-name-section)
[ "${WASM_NAMES:-}" = 1 ] && names=()
wasm-bindgen --target web --no-typescript "${names[@]}" --out-dir "$OUT/pkg" \
  "$TARGET_DIR/wasm32-unknown-unknown/$dir/notekpr_web.wasm"
wasm-bindgen --target web --no-typescript --remove-name-section --out-dir "$OUT/pkg" \
  "$TARGET_DIR/wasm32-unknown-unknown/$ddir/notekpr_diagram.wasm"
if command -v wasm-opt >/dev/null 2>&1 && [ "$PROFILE" != "dev" ]; then
  for m in notekpr_web notekpr_diagram; do
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int -o "$OUT/pkg/${m}_bg.wasm" "$OUT/pkg/${m}_bg.wasm"
  done
fi
cp -r crates/web/static/* "$OUT/"
# The licences page (scripts/licenses.py) says which version it is and
# links to that version's source: a release's tag, a snapshot's commit
# (0.8.0-3-gabc1234, -dev with uncommitted changes), main for `dev`.
version="${NOTEKPR_VERSION:-dev}"
v="${version%-dev}"
case "$v" in
  dev) source=main ;;
  [0-9]*-g*) source="${v##*-g}" ;;
  [0-9]*) source="v$v" ;;
  *) source="$v" ;;
esac
sed -e "s|@VERSION@|$version|g" -e "s|@SOURCE@|https://github.com/coolapso/notekpr/tree/$source|g" \
  crates/web/static/licenses.html > "$OUT/licenses.html"
ls -la "$OUT/pkg"
echo "web client built into $OUT"
