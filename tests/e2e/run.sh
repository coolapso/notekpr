#!/usr/bin/env bash
# Build the server and browser client, then run the browser end-to-end suite.
# Requirements: chromium (or chrome) + a matching chromedriver on PATH, python3.
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build -p coolpsonotes-server -p coolpsonotes-client
PROFILE="${PROFILE:-dev}" ./scripts/build-web.sh >/dev/null
python3 tests/e2e/suite.py --server target/debug/coolpsonotes-server --web dist/web "$@"
python3 tests/e2e/cross_client.py --server target/debug/coolpsonotes-server --cli target/debug/coolpsonotes --web dist/web
