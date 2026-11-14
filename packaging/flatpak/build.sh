#!/bin/sh
# The desktop app's build in its Flatpak (packaging/flatpak/md.notekpr.Notekpr.yml),
# in the checkout flatpak-builder made, with no network: the crates are
# vendored (cargo-sources.json), the wasm-bindgen CLI is in /app/tools.
set -eu

# The web client is wasm32: Rust's standard library for it (in
# rust-std-wasm32, a source of the manifest) beside the extension's own
# libraries, in a sysroot that only the wasm32 builds use.
# That library fits one Rust only: the one the extension had when the
# manifest was written (generate.py). The extension moves on to newer Rusts;
# then the manifest is written again (task flathub, docs/development.md).
if [ "$(rustc -vV | sed -n 's/^commit-hash: //p')" != "$(cat rust-std-wasm32/git-commit-hash)" ]; then
    echo "The Rust SDK extension's $(rustc -V) isn't the Rust the wasm32 standard library is for ($(cat rust-std-wasm32/version)): write the manifest again (task flathub)." >&2
    exit 1
fi
host=$(rustc -vV | sed -n 's/^host: //p')
mkdir -p wasm-sysroot/lib/rustlib
ln -sfn "/usr/lib/sdk/rust-stable/lib/rustlib/$host" "wasm-sysroot/lib/rustlib/$host"
rm -rf wasm-sysroot/lib/rustlib/wasm32-unknown-unknown
cp -r rust-std-wasm32/rust-std-wasm32-unknown-unknown/lib/rustlib/wasm32-unknown-unknown wasm-sysroot/lib/rustlib/
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="--sysroot=$PWD/wasm-sysroot"

# The web client, then the app with it inside.
OUT=target/desktop-web scripts/build-web.sh > /dev/null
TAURI_CONFIG='{"build":{"frontendDist":"../../target/desktop-web"}}' \
    cargo build --offline --release --locked -p notekpr-desktop

install -Dm755 target/release/notekpr-desktop /app/bin/notekpr-desktop
install -Dm644 packaging/desktop/md.notekpr.Notekpr.desktop /app/share/applications/md.notekpr.Notekpr.desktop
mkdir -p /app/share/metainfo
sed -e "s/@VERSION@/$NOTEKPR_VERSION/" -e "s/@DATE@/$NOTEKPR_RELEASE_DATE/" \
    packaging/desktop/md.notekpr.Notekpr.metainfo.xml > /app/share/metainfo/md.notekpr.Notekpr.metainfo.xml
icons=/app/share/icons/hicolor
install -Dm644 crates/desktop/icons/32x32.png "$icons/32x32/apps/md.notekpr.Notekpr.png"
install -Dm644 crates/desktop/icons/128x128.png "$icons/128x128/apps/md.notekpr.Notekpr.png"
install -Dm644 crates/desktop/icons/128x128@2x.png "$icons/256x256/apps/md.notekpr.Notekpr.png"
install -Dm644 crates/desktop/icons/icon.png "$icons/512x512/apps/md.notekpr.Notekpr.png"
install -Dm644 crates/web/static/favicon.svg "$icons/scalable/apps/md.notekpr.Notekpr.svg"
