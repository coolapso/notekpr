#!/bin/sh
# Copy the Dockleaf Hugo theme into docs/site/themes/dockleaf from a checkout of
# https://github.com/coolapso/hugo-theme-dockleaf, at a commit or tag (default: its HEAD),
# and note which in its NOTEKPR.md. `task docs:theme DOCKLEAF=… [REF=…]` runs it.
set -eu
src=${1:?usage: scripts/docs-theme.sh DOCKLEAF_CHECKOUT [REF]}
ref=${2:-HEAD}
dest=docs/site/themes/dockleaf
commit=$(git -C "$src" rev-parse --short --verify "$ref^{commit}")
version=$(git -C "$src" describe --tags --always "$commit")
notes=$(sed "s|^Copied from .* by \`scripts/|Copied from \`$version\` (commit \`$commit\`) by \`scripts/|" "$dest/NOTEKPR.md")
rm -rf "$dest"
mkdir -p "$dest"
git -C "$src" archive "$commit" layouts static assets i18n hugo.toml theme.toml LICENSE NOTICE README.md |
  tar -x -C "$dest"
printf '%s\n' "$notes" > "$dest/NOTEKPR.md"
echo "Dockleaf $version is in $dest"
