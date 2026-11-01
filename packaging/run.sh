#!/bin/sh
# Run a command in the CLI's build image (packaging/Dockerfile), with this
# checkout at /src as the working directory, as your own user:
#
#   packaging/run.sh goreleaser release --snapshot --clean
#
# Builds the image first (from Docker's cache after the first time). Its
# builds and the crates it downloads stay in target-release/: in the
# container that is target/ (where GoReleaser looks for what cargo built),
# so they never mix with yours. Passes on GITHUB_TOKEN and AUR_KEY when
# they are set.
set -eu
cd "$(dirname "$0")/.."

image=notekpr-clients-build
docker build -q -t "$image" -f packaging/Dockerfile packaging > /dev/null

mkdir -p target target-release/target target-release/cargo-registry target-release/home
tty=
[ -t 0 ] && [ -t 1 ] && tty=-t
exec docker run --rm -i $tty --init \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/target-release/home \
    -e GITHUB_TOKEN -e AUR_KEY \
    -v "$PWD:/src" -w /src \
    -v "$PWD/target-release/target:/src/target" \
    -v "$PWD/target-release/cargo-registry:/usr/local/cargo/registry" \
    "$image" "$@"
