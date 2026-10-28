# Code this crate ports

`notekpr-draw` is written in Rust, but parts of it are ports (translations)
of JavaScript libraries, so that drawings look exactly like they do in
Excalidraw. Those libraries are under the MIT license, whose notice follows; the
fractional indices (`order.rs`) follow fractional-indexing, which is CC0,
and Obsidian's drawing files (`obsidian.rs`) are read and written as the
obsidian-excalidraw-plugin (Zsolt Viczian) does, which is under the
AGPL-3.0 like notekpr.
No JavaScript is used or shipped.

| What | Ported from | Copyright |
|---|---|---|
| hand-drawn shapes | rough.js 4.6.4 | (c) 2019 Preet Shihn |
| hachure fills | hachure-fill 0.5.2 | (c) 2023 Preet Shihn |
| curves to points | points-on-curve 0.2.0 / 1.0.1 | (c) 2020 Preet Shihn |
| SVG path data | path-data-parser 0.1.0 | (c) 2020 Preet Shihn |
| pen strokes | perfect-freehand 1.2.0 | (c) 2021 Stephen Ruiz Ltd |
| element shapes, file formats, constant-width strokes (laser-pointer) | Excalidraw | (c) 2020 Excalidraw |
| compression of Obsidian's drawings | lz-string 1.5 | (c) 2013 pieroxy |

## The MIT license

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
