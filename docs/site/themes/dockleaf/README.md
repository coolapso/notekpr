# Dockleaf

A lightweight Hugo documentation theme with built-in search. Plain CSS,
plain JavaScript, and Markdown: no Node.js, package manager, or frontend build step.

Features include responsive navigation, nested sections, a table of contents,
syntax highlighting, hint boxes, configurable homepage and section cards, local
search, and a choice of looks ([themes](#themes)): the dark `searchbase` and the
light-and-dark `notekpr`. System fonts are the default; no external font or search
services are used.

![Dockleaf documentation theme](https://raw.githubusercontent.com/coolapso/hugo-theme-dockleaf/main/images/screenshot.png)

## Live example

See [Searchbase Documentation](https://docs.searchbase.md/) for a live example
of the documentation layout Dockleaf was extracted from (the `searchbase` theme).

## Install

Requires Hugo **0.168.0 or newer** (standard edition is sufficient).
From your Hugo site:

```sh
git submodule add https://github.com/coolapso/hugo-theme-dockleaf.git themes/dockleaf
```

Or copy the theme into `themes/dockleaf`. A copied installation needs `layouts/`,
`static/`, `assets/`, `i18n/`, `hugo.toml`, `theme.toml`, `LICENSE`, and `NOTICE`.
Set this in the site's `hugo.toml`:

```toml
baseURL = 'https://example.org/'
locale = 'en-us'
title = 'My Project Documentation'
theme = 'dockleaf'

[outputs]
  home = ['HTML', 'JSON']

[markup.tableOfContents]
  startLevel = 2
  endLevel = 3

[markup.highlight]
  style = 'onedark'
  lineNos = true

[params]
  brand = 'My Project'
  description = 'Documentation for my project.'
  # Optional paths relative to static/:
  # logo = 'images/logo.svg'
  # favicon = 'images/favicon.svg'
  footer = 'Copyright My Project contributors.'

[[menus.main]]
  name = 'Start'
  pageRef = '/guide/getting-started'
  weight = 1

[[menus.footer]]
  name = 'Source'
  url = 'https://github.com/example/project'
```

`pageRef` resolves internal menu destinations using Hugo's content tree. For card
and image URLs use paths such as `guide/getting-started/` or `images/logo.svg`;
these work with subdirectory hosting too. External URLs are also supported.

The site configuration above enables the homepage JSON output required for search,
a level 2–3 table of contents, and onedark code highlighting. Hugo does not inherit
these settings from theme configuration. Include both `HTML` and `JSON` home outputs.

## Themes

`params.theme` chooses the look; leave it out for the default:

| Theme | Look |
|---|---|
| `searchbase` | Dark, sky blue and violet glows, large headings. The default. |
| `notekpr` | Light and dark, following the system; indigo, the mono of Markdown for labels and headings' `#` markers, a hand-written font (Excalifont, included) for the footer's note. |

```toml
[params]
  theme = 'notekpr'

# notekpr colours code from its palette, in light and dark: let Hugo write classes.
[markup.highlight]
  noClasses = false
  lineNos = false
```

A theme is a folder in `assets/dockleaf/themes/<name>/`: a `style.css` that Dockleaf
loads after `css/main.css` (mostly the colour variables of [Customization](#customization),
then what the theme draws differently), and the files it uses, published with it.
`searchbase` is `main.css` itself. An unknown name stops the build and lists the themes.

## Optional settings

All of these are optional; a site without them looks as it did.

```toml
[params]
  logo = 'images/icon.svg'   # an SVG in assets/ is drawn inline, so its CSS can use the theme's colours
  brandTag = 'docs'          # a second word next to the brand
  editURL = 'https://github.com/example/project/edit/main/docs/content/'  # an "Edit this page" link
  copyCode = true            # a Copy button on code blocks
  about = 'One or two lines about the project.'   # with footer columns: the footer's first column
  footerNote = 'made with care'                   # a note under it

[[menus.main]]
  name = 'Try it'
  url = 'https://demo.example.org'
  [menus.main.params]
    button = true            # drawn as a button

# Footer entries with children become columns; the others go in the bottom row,
# with an optional image (params.image) before their name.
[[menus.footer]]
  name = 'Project'
  identifier = 'project'
[[menus.footer]]
  name = 'Source'
  url = 'https://github.com/example/project'
  parent = 'project'
```

`editURL` is followed by the page's path under `content/`; a page's own `editURL`
replaces it. A `404.html` page is built for hosts that serve one.

## Content and navigation

Create `content/guide/_index.md` for a section and Markdown pages beneath it.
Sidebar sections and pages sort by Hugo `weight` and title. Nested sections work
recursively. Front matter supports `navTitle` for sidebar labels and
`hideFromNav: true` to hide a page or entire subtree from navigation. This does
not make content private or exclude it from search. Section landing pages render
Markdown content; write an introduction or links there as appropriate.

The homepage (`content/_index.md`) supports:

```yaml
---
title: My Project
heading: My Project Docs
description: Learn to install and use My Project.
eyebrow: Documentation
# Optional banner, bannerAlt, bannerWidth, bannerHeight
actions:
  - title: Get started
    url: guide/getting-started/
    primary: true
cards:
  - title: Getting started
    url: guide/getting-started/
    description: Install and configure your first instance.
    icon: 🚀
---
Additional Markdown content goes here.
```

`actions` are buttons under the description; a card's `icon` is optional. Section
pages show `cards` too, after their content. Pages support `description` and
`eyebrow`. The `hint` shortcode renders callouts:

```text
{{< hint warning >}}
Back up your configuration before upgrading.
{{< /hint >}}
```

Use `info` (the default) or `warning`.

## Customization

Put CSS overrides in your site's `assets/css/custom.css`; it loads after the theme.
Available color variables are `--bg`, `--panel`, `--panel-2`, `--border`, `--text`,
`--muted`, `--dim`, `--brand`, `--brand-hover`, `--on-brand`, `--accent`, `--danger`,
the surfaces `--topbar-bg`, `--sidebar-bg`, `--field-bg`, `--card-bg`, `--code-bg`,
`--terminal-bg`, `--hint-bg`, `--warning-bg`, `--brand-soft` and `--brand-ring`, the
glows `--glow-1` and `--glow-2`, the shadows `--shadow` and `--shadow-lg`, and the
fonts `--font` and `--mono`. Override layouts or partials through your site's
`layouts/` directory when needed.

English UI messages are in `i18n/en.toml`. Add translations in your site's `i18n/`
directory. Hugo controls document language, translated content, menus, and URLs;
only English UI translations ship with the initial theme.

Search matches case-insensitive substrings across regular page titles and text,
returns up to six results, and downloads the local index on the first query of
at least two characters; <kbd>/</kbd> goes to the search box. It suits small to
medium documentation sites; it does not provide stemming, ranking, or a remote
search service. All indexed text is public.

## Preview and validate

Clone the source into a directory named `dockleaf`, then preview or validate:

```sh
git clone https://github.com/coolapso/hugo-theme-dockleaf.git dockleaf
cd dockleaf
hugo server --source exampleSite --themesDir ../.. --theme dockleaf
HUGO_PARAMS_THEME=notekpr hugo server --source exampleSite --themesDir ../.. --theme dockleaf
python3 scripts/check.py
```

The preview commands assume this checkout is named `dockleaf`; the second shows the
example in another theme (`task serve THEME=notekpr` does the same). Validation uses a
temporary theme installation and does not depend on the checkout directory name.
It verifies theme metadata and preview images, builds the example in every theme at
both domain root and a subdirectory, checks links, assets, search index, nested
navigation, and customization, checks that an unknown theme stops the build, and
checks JavaScript syntax when Node.js is installed. Python 3 and Hugo are required; Node.js is optional.
No packages need installing.

## License and origin

MIT; see [LICENSE](LICENSE) and [NOTICE](NOTICE). Extracted from the
[Searchbase](https://github.com/coolapso/searchbase) documentation theme.
Searchbase content, branding, and artwork are not part of Dockleaf, nor are
[notekpr](https://github.com/coolapso/notekpr)'s: its theme is the palette and
type of its website, without its logo. The `notekpr` theme includes Excalifont,
under the SIL Open Font License 1.1 (`assets/dockleaf/themes/notekpr/OFL.txt`).

## Support

If you like this project and want to support / contribute in a different way you can always [:heart: Sponsor Me](https://github.com/sponsors/coolapso) or

<a href="https://www.buymeacoffee.com/coolapso" target="_blank">
  <img src="https://cdn.buymeacoffee.com/buttons/default-yellow.png" alt="Buy Me A Coffee" style="height: 51px !important;width: 217px !important;" />
</a>
