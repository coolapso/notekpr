#!/bin/sh
# Write the CLI's shell completion scripts into DIR, for the archives and
# packages (.goreleaser.yaml runs it before building): notekpr.bash,
# _notekpr (zsh), notekpr.fish, notekpr.ps1 (PowerShell), notekpr.elv.
set -eu
out=${1:?usage: packaging/completions.sh DIR}
mkdir -p "$out"
# Its own native build, apart from the release builds and from target/.
CARGO_TARGET_DIR=target-release/native cargo build --locked -q -p notekpr-cli
bin=target-release/native/debug/notekpr
"$bin" completions bash > "$out/notekpr.bash"
"$bin" completions zsh > "$out/_notekpr"
"$bin" completions fish > "$out/notekpr.fish"
"$bin" completions powershell > "$out/notekpr.ps1"
"$bin" completions elvish > "$out/notekpr.elv"
