#!/usr/bin/env python3
"""Typing latency of the live editor on a large note (not part of the suite:
timings depend on the machine, so nothing is asserted).

    python3 tests/e2e/perf.py --server target/debug/coolpsonotes-server --web dist/web [--lines 5000]

Pastes a note of LINES lines of mixed Markdown, puts the caret in the middle
and dispatches `beforeinput` events the way typing does. Each sample is the
synchronous time of one keystroke: the editor's handler, the CRDT edit, the DOM
update and the style/layout the browser needs for it (forced by reading
`offsetHeight`), measured in the page with `performance.now()`.
"""
import argparse, os, statistics, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from suite import Env, uniq, new_note  # noqa: E402

BLOCK = [
    "## Section {i}",
    "Some text with **bold**, *italic*, `code` and a [[Link {i}]] plus #tag{i}.",
    "- item one",
    "- [ ] task two",
    "1. first",
    "> a quote line",
    "| a | b |",
    "| --- | --- |",
    "| {i} | x |",
    "plain paragraph line {i} with some more words to make it longer than usual",
]

MEASURE = """
const [n, kind, line] = arguments;
const ed = document.querySelector('.ed');
ed.focus();
const ln = ed.children[line];
const r = document.createRange(); r.selectNodeContents(ln); r.collapse(false);
const s = getSelection(); s.removeAllRanges(); s.addRange(r);
const times = [];
for (let i = 0; i < n; i++) {
  const t0 = performance.now();
  const ev = kind === 'enter'
    ? new InputEvent('beforeinput', {inputType: 'insertParagraph', bubbles: true, cancelable: true})
    : new InputEvent('beforeinput', {inputType: 'insertText', data: 'x', bubbles: true, cancelable: true});
  ed.dispatchEvent(ev);
  void ed.offsetHeight;
  times.push(performance.now() - t0);
}
return times;
"""


def stats(name, xs):
    xs = sorted(xs)
    p95 = xs[min(len(xs) - 1, int(len(xs) * 0.95))]
    print(f"  {name:<22} median {statistics.median(xs):7.2f} ms   p95 {p95:7.2f} ms   max {xs[-1]:7.2f} ms   (n={len(xs)})")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/coolpsonotes-server")
    ap.add_argument("--web", default="dist/web")
    ap.add_argument("--lines", type=int, default=5000)
    ap.add_argument("--keys", type=int, default=60)
    args = ap.parse_args()
    env = Env(args.server, args.web)
    from wd import Browser, signup  # noqa: E402  (after Env set the WebDriver address)
    b = Browser()
    try:
        signup(b, uniq("perf"))
        new_note(b, "Big", "")
        text = "\n".join(BLOCK[k % len(BLOCK)].format(i=k // len(BLOCK)) for k in range(args.lines))
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
             "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]);"
             "ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));", text)
        b.wait("return document.querySelectorAll('.ed > .ln').length >= %d" % args.lines, 60, "the big note")
        print(f"{args.lines} lines, web build {args.web}")
        mid = args.lines // 2
        b.js(MEASURE, 5, "text", mid)  # warm up
        stats("type a character", b.js(MEASURE, args.keys, "text", mid))
        stats("press Enter", b.js(MEASURE, max(10, args.keys // 3), "enter", mid))
        stats("type at the top", b.js(MEASURE, args.keys, "text", 1))
    finally:
        b.close()
        env.close()


if __name__ == "__main__":
    main()
