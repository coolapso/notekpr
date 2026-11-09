# The fonts of drawings

The fonts drawings are written in, as Excalidraw ships them (split by
character ranges, so a browser downloads only what a drawing uses). notekpr
serves them itself: drawings never load anything from elsewhere.
`drawing-fonts.css` declares them.

| Font | Folder | By | License |
|---|---|---|---|
| Excalifont | `Excalifont/` | Ján Filípek / DizajnDesign, based on Virgil | SIL Open Font License 1.1 |
| Virgil | `Virgil/` | Your Own Font Foundry, for Excalidraw | SIL Open Font License 1.1 |
| Nunito | `Nunito/` | Vernon Adams, Cyreal | SIL Open Font License 1.1 |
| Lilita One | `Lilita/` | Juan Montoreano | SIL Open Font License 1.1 |
| Comic Shanns | `ComicShanns/` | Shannon Miwa, Jesus Gonzalez | MIT |
| Cascadia Code | `Cascadia/` | Microsoft | SIL Open Font License 1.1 |
| Liberation Sans | `Liberation/` | Red Hat | SIL Open Font License 1.1 |
| Assistant | `Assistant/` | Ben Nathan | SIL Open Font License 1.1 |

Not shipped: Xiaolai (Excalidraw's handwriting for Chinese and Japanese,
13 MB); such text falls back to the system's fonts.

## The math font

Math (LaTeX in notes) is typeset by the browser as MathML, in Noto Sans
Math (Google, SIL Open Font License 1.1), `NotoSansMath/`. It is the copy
math-core ships (`playground/fonts/NotoSansMath-Regular-centered-diacritics-primes.woff2`
at its tag v0.9.0, from [math-core-fonts](https://github.com/tmke8/math-core-fonts)):
the same glyphs, with primes and three accents placed so that Chromium and
Safari draw them right. `style.css` declares it; a browser downloads it the
first time a note shows math.

The SIL Open Font License 1.1 is in `OFL.txt`; Comic Shanns' MIT license in
`ComicShanns/LICENSE.txt`.
