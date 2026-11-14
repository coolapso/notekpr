#!/bin/sh
# Run a command in the clients' build image (packaging/Dockerfile), with this
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
wasm_bindgen=$(sed -nE 's/^wasm-bindgen = "=([0-9.]+)".*/\1/p' crates/web/Cargo.toml)
docker build -q -t "$image" --build-arg WASM_BINDGEN_VERSION="$wasm_bindgen" \
    -f packaging/Dockerfile packaging > /dev/null

# Builds made in another image (an older one, before a change to the
# Dockerfile) were compiled against its system and may not link or run in
# this one; Cargo can't tell. Start them again then.
id=$(docker image inspect -f '{{.Id}}' "$image")
if [ "$(cat target-release/image 2>/dev/null)" != "$id" ]; then
  rm -rf target-release/target target-release/native target-release/desktop-web
  mkdir -p target-release
  echo "$id" > target-release/image
fi

mkdir -p target target-release/target target-release/cargo-registry target-release/home/.ssh
# The image has no user with your uid, and ssh (git's, publishing to the
# AUR) refuses to run without one: give it one, at home in the container.
printf 'root:x:0:0:root:/root:/bin/sh\n%s:x:%s:%s::/src/target-release/home:/bin/sh\n' \
    "$(id -un)" "$(id -u)" "$(id -g)" > target-release/passwd
tty=
[ -t 0 ] && [ -t 1 ] && tty=-t
exec docker run --rm -i $tty --init \
    --user "$(id -u):$(id -g)" \
    -e HOME=/src/target-release/home \
    -v "$PWD/target-release/passwd:/etc/passwd:ro" \
    -e GITHUB_TOKEN -e AUR_KEY \
    -v "$PWD:/src" -w /src \
    -v "$PWD/target-release/target:/src/target" \
    -v "$PWD/target-release/cargo-registry:/usr/local/cargo/registry" \
    "$image" "$@"
