#!/bin/sh
# The macOS release files, from the release builds of the app and the CLI
# (task build:macos builds them and runs this, on a Mac):
#
#   OUT/notekpr-desktop_<version>_darwin_arm64.dmg  the app, notekpr.app, to
#       drag into Applications (the disk image shows where)
#   OUT/notekpr-cli_<version>_darwin_arm64.tar.gz   the CLI, as the Linux
#       archives hold it: notekpr, LICENSE, README.md, completions/
#
#   packaging/macos/package.sh VERSION DESKTOP_BINARY CLI_BINARY OUT
#
# Apple Silicon only. Signed ad hoc, as every program for Apple Silicon must
# be, not with an Apple Developer ID yet (#33): macOS asks people to allow
# the app once (Privacy & Security, "Open Anyway"). Needs macOS's own tools:
# codesign, hdiutil, plutil.
set -eu
cd "$(dirname "$0")/../.."

usage="usage: packaging/macos/package.sh VERSION DESKTOP_BINARY CLI_BINARY OUT"
version=${1:?$usage}
desktop=${2:?$usage}
cli=${3:?$usage}
out=${4:?$usage}
# Info.plist's versions are numbers only: a snapshot's (0.19.2-7-gabc1234)
# is its release's there, and whole in the files' names.
numbers=$(printf '%s\n' "$version" | sed -nE 's/^([0-9]+\.[0-9]+\.[0-9]+).*/\1/p')
if [ -z "$numbers" ]; then
  echo "package.sh: '$version' isn't a version (0.20.0, or a snapshot's 0.19.2-7-gabc1234)" >&2
  exit 1
fi
arch=arm64

work=$(mktemp -d "${TMPDIR:-/tmp}/notekpr-macos.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
# No ._ files (macOS's extended attributes) in the archive.
export COPYFILE_DISABLE=1

# The app: notekpr.app, signed as a whole (its Info.plist and icon too).
app=$work/dmg/notekpr.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$numbers/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"
cp "$desktop" "$app/Contents/MacOS/notekpr-desktop"
cp crates/desktop/icons/icon.icns "$app/Contents/Resources/icon.icns"
cp LICENSE "$app/Contents/Resources/LICENSE.txt"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
# The disk image: the app, and Applications to drag it to.
ln -s /Applications "$work/dmg/Applications"
dmg=$out/notekpr-desktop_${version}_darwin_$arch.dmg
rm -f "$dmg"
hdiutil create -quiet -volname notekpr -srcfolder "$work/dmg" -fs HFS+ -format UDZO "$dmg"

# The CLI, its completions written by itself.
c=$work/cli
mkdir -p "$c/completions"
cp "$cli" "$c/notekpr"
cp LICENSE README.md "$c/"
"$c/notekpr" completions bash > "$c/completions/notekpr.bash"
"$c/notekpr" completions zsh > "$c/completions/_notekpr"
"$c/notekpr" completions fish > "$c/completions/notekpr.fish"
"$c/notekpr" completions powershell > "$c/completions/notekpr.ps1"
"$c/notekpr" completions elvish > "$c/completions/notekpr.elv"
tar -C "$c" -czf "$out/notekpr-cli_${version}_darwin_$arch.tar.gz" notekpr LICENSE README.md completions

ls -l "$dmg" "$out/notekpr-cli_${version}_darwin_$arch.tar.gz"
