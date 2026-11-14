#!/usr/bin/env bash
# Build the server and browser client, then run the browser end-to-end suite,
# the browser ↔ CLI tests and the desktop app's. `-k NAME` runs only the tests
# whose name contains NAME (the browser ↔ CLI tests are called `cross_client`
# and `drawings`, the desktop app's `desktop`).
# Requirements: chromium (or chrome) + a matching chromedriver on PATH, python3;
# for the desktop app's, sway and WebKitWebDriver (skipped without them).
set -euo pipefail
# Print as it goes, also into a pipe (`task release` runs it in one): a
# failure's traceback then lands next to its test, not somewhere earlier.
export PYTHONUNBUFFERED=1
cd "$(dirname "$0")/../.."
cargo build -p notekpr-server -p notekpr-cli
# Its own copy of the web client: `task dev`, `task demo` or `task web:watch`
# rebuild dist/web, which would pull the app from under a running test. The
# wasm build has its own target directory, as in the Taskfile, so native and
# wasm builds don't keep invalidating each other.
web=target/e2e-web
CARGO_TARGET_DIR="${WEB_TARGET_DIR:-target-web}" PROFILE="${PROFILE:-dev}" OUT="$web" \
  ./scripts/build-web.sh >/dev/null
filter=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  if [ "${args[i]}" = "-k" ]; then filter="${args[i + 1]:-}"; fi
done
python3 tests/e2e/suite.py --server target/debug/notekpr-server --web "$web" "$@"
if [ -z "$filter" ] || [[ "cross_client" == *"$filter"* ]]; then
  python3 tests/e2e/cross_client.py --server target/debug/notekpr-server --cli target/debug/notekpr --web "$web"
fi
if [ -z "$filter" ] || [[ "drawings" == *"$filter"* ]]; then
  python3 tests/e2e/drawings.py --server target/debug/notekpr-server --cli target/debug/notekpr --web "$web"
fi
if [ -z "$filter" ] || [[ "desktop" == *"$filter"* ]]; then
  # The desktop app with this web client inside (crates/desktop: its build
  # takes the web client's folder from TAURI_CONFIG).
  TAURI_CONFIG="{\"build\":{\"frontendDist\":\"../../$web\"}}" cargo build -p notekpr-desktop
  python3 tests/e2e/desktop.py --server target/debug/notekpr-server --cli target/debug/notekpr \
    --app target/debug/notekpr-desktop
fi
