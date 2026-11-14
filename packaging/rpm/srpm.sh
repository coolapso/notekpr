#!/bin/sh
# Make the source RPMs Fedora's COPR builds the CLI and the desktop app
# from, after GoReleaser: each spec here (*.spec.in, with the versions
# filled in) with GoReleaser's source archive notekpr-<version>.tar.gz from
# DIR. Writes <package>-<version>-1.src.rpm into DIR. Runs in the build
# image, which has rpmbuild (packaging/run.sh packaging/rpm/srpm.sh
# dist/clients).
set -eu
cd "$(dirname "$0")/../.."
dir=${1:?usage: packaging/rpm/srpm.sh DIR}

set -- "$dir"/notekpr-[0-9]*.tar.gz
[ $# = 1 ] && [ -f "$1" ] || { echo "srpm.sh: need exactly one notekpr-<version>.tar.gz in $dir" >&2; exit 1; }
src=$1
version=${src##*/notekpr-}
version=${version%.tar.gz}

rust=$(sed -nE 's/^rust-version = "([0-9.]+)".*/\1/p' Cargo.toml)
web_rust=$(sed -nE 's/^rust-version = "([0-9.]+)".*/\1/p' crates/diagram/Cargo.toml)
wasm_bindgen=$(sed -nE 's/^wasm-bindgen = "=([0-9.]+)".*/\1/p' crates/web/Cargo.toml)

top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$top/SPECS"
cp "$src" "$top/SOURCES/"
for spec in packaging/rpm/*.spec.in; do
  name=$(basename "$spec" .spec.in)
  sed -e "s/@VERSION@/$version/g" -e "s/@RPM_VERSION@/$(printf %s "$version" | tr - ^)/g" \
      -e "s/@RUST_VERSION@/$rust/g" -e "s/@WEB_RUST_VERSION@/$web_rust/g" \
      -e "s/@WASM_BINDGEN_VERSION@/$wasm_bindgen/g" \
      "$spec" > "$top/SPECS/$name.spec"
  rpmbuild -bs --quiet --define "_topdir $top" "$top/SPECS/$name.spec"
done
cp "$top"/SRPMS/*.src.rpm "$dir/"
ls "$dir"/*.src.rpm
