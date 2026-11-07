# Dockleaf, copied into notekpr

The docs site's Hugo theme, [Dockleaf](https://github.com/coolapso/hugo-theme-dockleaf)
(MIT), with its `notekpr` theme chosen in `docs/site/hugo.toml`.

Copied from commit `927a849` by `scripts/docs-theme.sh`. Don't change it here: change
Dockleaf, then copy it again:

```sh
task docs:theme DOCKLEAF=~/Dev/dockleaf            # its checked-out commit
task docs:theme DOCKLEAF=~/Dev/dockleaf REF=v1.2.0  # or a release
```

To try changes to a Dockleaf checkout before copying them, point Hugo at the folder
that holds it: `HUGO_THEMESDIR=~/Dev task docs` (the checkout must be named `dockleaf`).
