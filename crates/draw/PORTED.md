# Code this crate ports

`notekpr-draw` is written in Rust, but parts of it are ports (translations)
of JavaScript libraries, so that drawings look exactly like they do in
Excalidraw. Those libraries are under the MIT license, whose notice follows; the
fractional indices (`order.rs`) follow fractional-indexing, which is CC0,
and Obsidian's drawing files (`obsidian.rs`) are read and written as the
obsidian-excalidraw-plugin (Zsolt Viczian) does, which is under the
AGPL-3.0 like notekpr.
No JavaScript is used or shipped.

So that numbers come out exactly as in a browser, `src/js/math.rs` also
ports fdlibm's `sin` and `cos` the way V8 has them, and `src/js.rs`
computes `Math.hypot` the way V8 does; their notices are at the end.

| What | Ported from | Copyright |
|---|---|---|
| hand-drawn shapes | rough.js 4.6.4 | (c) 2019 Preet Shihn |
| hachure fills | hachure-fill 0.5.2 | (c) 2023 Preet Shihn |
| curves to points | points-on-curve 0.2.0 / 1.0.1 | (c) 2020 Preet Shihn |
| SVG paths to points | points-on-path 0.2.1 | (c) 2020 Preet |
| SVG path data | path-data-parser 0.1.0 | (c) 2020 Preet Shihn |
| pen strokes | perfect-freehand 1.2.0 | (c) 2021 Stephen Ruiz Ltd |
| element shapes, file formats, constant-width strokes (laser-pointer) | Excalidraw | (c) 2020 Excalidraw |
| elbow arrows' routing (A* around the shapes they connect) | Excalidraw | (c) 2020 Excalidraw |
| compression of Obsidian's drawings | lz-string 1.5 | (c) 2013 pieroxy |
| `Math.sin`, `Math.cos` | fdlibm 5.3, as in V8 | (c) 1993 Sun Microsystems; the V8 project authors |
| `Math.hypot` | V8 | the V8 project authors |

The test fixture `tests/fixtures/perfect-freehand.json` holds
perfect-freehand's own test inputs and snapshots, from its repository
(MIT, as above).

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

## fdlibm

Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.

Developed at SunSoft, a Sun Microsystems, Inc. business.
Permission to use, copy, modify, and distribute this
software is freely granted, provided that this notice
is preserved.

## V8

Copyright 2014, the V8 project authors. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

* Redistributions of source code must retain the above copyright
  notice, this list of conditions and the following disclaimer.
* Redistributions in binary form must reproduce the above
  copyright notice, this list of conditions and the following
  disclaimer in the documentation and/or other materials provided
  with the distribution.
* Neither the name of Google Inc. nor the names of its
  contributors may be used to endorse or promote products derived
  from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
