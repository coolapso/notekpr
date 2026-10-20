#!/usr/bin/env bash
# Build the server and browser client, then run the browser end-to-end suite.
# Requirements: chromium (or chrome) + a matching chromedriver on PATH, python3.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p coolpsonotes-server -p coolpsonotes-client
# Its own copy of the web client: `task dev`, `task demo` or `task web:watch`
# rebuild dist/web, which would pull the app from under a running test.
web=target/e2e-web
PROFILE="${PROFILE:-dev}" OUT="$web" ./scripts/build-web.sh >/dev/null
python3 tests/e2e/suite.py --server target/debug/coolpsonotes-server --web "$web" "$@"
python3 tests/e2e/cross_client.py --server target/debug/coolpsonotes-server --cli target/debug/coolpsonotes --web "$web"
