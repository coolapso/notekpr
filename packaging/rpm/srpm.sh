#!/bin/sh
# Make the source RPM Fedora's COPR builds the CLI from, after GoReleaser:
# the spec (notekpr-cli.spec.in, with the version filled in) and
# GoReleaser's source archive notekpr-cli-<version>.tar.gz from DIR. Writes
# notekpr-cli-<version>-1.src.rpm into DIR. Runs in the build image, which
# has rpmbuild (packaging/run.sh packaging/rpm/srpm.sh dist/clients).
set -eu
cd "$(dirname "$0")/../.."
dir=${1:?usage: packaging/rpm/srpm.sh DIR}

set -- "$dir"/notekpr-cli-*.tar.gz
[ $# = 1 ] && [ -f "$1" ] || { echo "srpm.sh: need exactly one notekpr-cli-<version>.tar.gz in $dir" >&2; exit 1; }
src=$1
version=${src##*/notekpr-cli-}
version=${version%.tar.gz}

top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$top/SPECS"
cp "$src" "$top/SOURCES/"
rust=$(sed -nE 's/^rust-version = "([0-9.]+)".*/\1/p' Cargo.toml)
sed -e "s/@VERSION@/$version/g" -e "s/@RPM_VERSION@/$(printf %s "$version" | tr - ^)/g" \
    -e "s/@RUST_VERSION@/$rust/g" \
    packaging/rpm/notekpr-cli.spec.in > "$top/SPECS/notekpr-cli.spec"
rpmbuild -bs --quiet --define "_topdir $top" "$top/SPECS/notekpr-cli.spec"
cp "$top"/SRPMS/*.src.rpm "$dir/"
ls "$dir"/*.src.rpm
