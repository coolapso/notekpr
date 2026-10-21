#!/usr/bin/env bash
# Build the server and browser client, then run the browser end-to-end suite
# and the browser ↔ CLI test. `-k NAME` runs only the tests whose name
# contains NAME (the browser ↔ CLI test is called `cross_client`).
# Requirements: chromium (or chrome) + a matching chromedriver on PATH, python3.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p notekpr-server -p notekpr-client
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
