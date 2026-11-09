#!/usr/bin/env python3
"""The user guide's screenshots (docs/assets/guide), taken from a demo of their own.

`task docs:shots` builds what this needs and runs it. It starts a demo (scripts/demo.py) on a
free port in a temporary folder, adds the examples the guide shows (a vault "Guide", written with
the CLI), signs in as alice in headless Chromium and takes each picture: a part of the app, a
dialog or the lines of a note, some with numbered markers that the guide explains. Chromium does
the cropping, the markers and the WebP encoding, so this needs only the standard library (and
chromium + chromedriver, as the browser tests do). Pictures are twice their CSS size, to stay
sharp.

    python3 scripts/guide-shots.py --web target/guide-web [--only gs-app,wr-math] [--keep]

When the app changes what a picture shows, take them again and look at them before committing.
"""
import argparse
import base64
import datetime
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import textwrap
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests" / "e2e"))
import wd  # noqa: E402  (the browser tests' stdlib WebDriver client)
from wd import KEYS  # noqa: E402

OUT = ROOT / "docs" / "assets" / "guide"
WIDTH, HEIGHT, SCALE = 1280, 820, 2


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


# ------------------------------------------------------------------ the examples

TODAY = datetime.date.today()


def day(delta):
    return (TODAY + datetime.timedelta(days=delta)).isoformat()


# The guide's examples, as notes of a vault "Guide" (title → text). Each ends with an empty line
# for the caret to rest on, so no line of the example shows its marks.
EXAMPLES = {
    "Trip to Lisbon": """
        # Trip to Lisbon

        Pack **light**, bring *sunscreen* and ~~an umbrella~~.
        ==Book the tram tour== before Friday: the code is `LX-2026`.

        ## Day 1

        - Alfama, on foot
        - Lunch at the market
        """,
    "Lists": """
        - Groceries
          - Tomatoes
            - Cherry ones, if they have them
          - Bread

        1. Preheat the oven
        2. Knead the dough

        - [x] Book the flights
        - [ ] Pack the bags
        """,
    "Links": """
        The timetable is on https://example.org/trams, the tours on
        [the tram company's site](https://example.org).

        Plans are in [[Trip plan]], the day trip in [[Trip plan#Day 2|our day trip]].

        Still to write: [[Restaurants to try]].
        """,
    "Trip plan": """
        ## Day 1

        Alfama and the castle.

        ## Day 2

        Sintra, by train.
        """,
    "Table": """
        | Day | Where   |
        | --- | ------- |
        | 1   | Lisbon  |
        | 2   | Sintra  |
        """,
    "HTML table": """
        <table>
        <tr><th rowspan="2">Day</th><th colspan="2" align="center">Meals</th></tr>
        <tr><th>Lunch</th><th>Dinner</th></tr>
        <tr><td>Friday</td><td>At the market</td><td>In Alfama</td></tr>
        <tr><td>Saturday</td><td colspan="2" align="center">A picnic in Sintra</td></tr>
        <tr><td>Sunday</td><td rowspan="2">On the train</td><td>In Belém</td></tr>
        <tr><td>Monday</td><td>At home</td></tr>
        </table>
        """,
    "Callouts": """
        > [!tip] Water in the morning
        > The soil holds it better before the heat.

        > [!warning] Frost on Thursday
        > Cover the seedlings.

        > [!faq]- Can basil go outside?
        > Only once the nights stay above 10 °C.
        """,
    "Code": """
        ```python title=hello.py hl=2
        name = "notekpr"
        print(f"hello, {name}")
        ```
        """,
    "Math": r"""
        Einstein's $E = mc^2$, in a sentence. On its own:

        $$
        \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
        $$
        """,
    "Chemistry": r"""
        - Water: $\ce{H2O}$
        - A reaction: $\ce{2H2 + O2 -> 2H2O}$
        - An ion: $\ce{SO4^2-}$
        - An isotope: $\ce{^{14}C}$
        - An equilibrium: $\ce{A <=> B}$
        - Units: $\pu{123 kJ/mol}$
        """,
    "Diagram": """
        ```mermaid
        flowchart LR
            Idea --> Draft
            Draft --> Review{Good?}
            Review -->|yes| Publish
            Review -->|no| Draft
        ```
        """,
    "More diagrams": """
        ```mermaid
        sequenceDiagram
            Alice->>Bob: Dinner on Friday?
            Bob-->>Alice: Yes! 7pm?
        ```

        ```mermaid
        pie title Where the time went
            "Writing" : 45
            "Meetings" : 30
            "Email" : 25
        ```
        """,
    "Dates": f"""
        - Dentist @{day(0)}
        - Call the plumber @{day(1)}
        - Paid the rent @{day(-1)}
        - The party @2026-12-31
        """,
}


def note(text):
    return textwrap.dedent(text).strip("\n") + "\n\n"


# ------------------------------------------------------------------ the browser

# Crops `parts` ([{png, rect}]: viewport screenshots and CSS-pixel rectangles) out, side by side
# (or one under the other: `column`) with `gap` between them and an optional caption above each,
# and encodes the result as WebP.
# (Chromium decodes the PNGs itself: no URL to load, so the app's Content-Security-Policy agrees.)
ENCODE = """
const [parts, gap, captions, column, bg, done] = arguments;
(async () => {
  const s = devicePixelRatio;
  const shots = [];
  for (const p of parts) {
    const bin = atob(p.png), bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    shots.push([await createImageBitmap(new Blob([bytes], {type: 'image/png'})), p.rect]);
  }
  const cap = captions ? 30 : 0;
  const sum = k => shots.reduce((a, [, r]) => a + r[k] + cap * (k === 'h'), 0) + gap * (shots.length - 1);
  const most = k => Math.max(...shots.map(([, r]) => r[k] + cap * (k === 'h')));
  const w = column ? most('w') : sum('w'), h = column ? sum('h') : most('h');
  const c = document.createElement('canvas');
  c.width = Math.round(w * s); c.height = Math.round(h * s);
  const g = c.getContext('2d');
  g.fillStyle = bg; g.fillRect(0, 0, c.width, c.height);
  let x = 0, y = 0;
  shots.forEach(([bmp, r], i) => {
    if (captions) {
      g.fillStyle = '#6b6b76'; g.font = `600 ${13 * s}px system-ui, sans-serif`;
      g.fillText(captions[i], (x + 6) * s, (y + 19) * s);
    }
    g.drawImage(bmp, r.x * s, r.y * s, r.w * s, r.h * s, x * s, (y + cap) * s, r.w * s, r.h * s);
    if (column) y += r.h + cap + gap; else x += r.w + gap;
  });
  done(c.toDataURL('image/webp', 0.86).split(',')[1]);
})().catch(e => done('ERROR ' + e));
"""

# Numbered markers over the page: [[n, x, y]] in CSS pixels (styled through the CSSOM, which the
# app's Content-Security-Policy allows).
MARK = """
document.getElementById('guide-marks')?.remove();
const box = document.createElement('div');
box.id = 'guide-marks';
box.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647';
for (const [n, x, y] of arguments[0]) {
  const d = document.createElement('div');
  d.textContent = n;
  d.style.cssText = `position:absolute;left:${x - 12}px;top:${y - 12}px;width:24px;height:24px;
    border-radius:50%;background:#e8590c;color:#fff;font:700 14px/24px system-ui,sans-serif;
    text-align:center;box-shadow:0 0 0 2px #fff,0 2px 6px rgba(0,0,0,.35)`;
  box.append(d);
}
document.body.append(box);
"""

# Where a marker goes beside an element: `place` is one of left, right, top, bottom, over (its
# middle), each `off` CSS pixels out; or tl, tr (its top corners, `off` pixels in).
MARK_AT = """
const [sel, text, place, off] = arguments;
const els = [...document.querySelectorAll(sel)].filter(e => e.getClientRects().length);
const el = text ? els.find(e => e.textContent.trim().startsWith(text)) : els[0];
if (!el) return null;
const r = el.getBoundingClientRect();
const mx = r.left + r.width / 2, my = r.top + r.height / 2;
return {left: [r.left - off, my], right: [r.right + off, my], top: [mx, r.top - off],
        bottom: [mx, r.bottom + off], over: [mx, my], tl: [r.left + off, r.top + off],
        tr: [r.right - off, r.top + off]}[place];
"""

# The rectangle around elements (CSS pixels), `pad` around it.
RECT = """
const [sels, pad] = arguments;
const rs = sels.flatMap(s => [...document.querySelectorAll(s)]).filter(e => e.getClientRects().length)
  .map(e => e.getBoundingClientRect());
if (!rs.length) return null;
const l = Math.min(...rs.map(r => r.left)) - pad, t = Math.min(...rs.map(r => r.top)) - pad;
const r = Math.max(...rs.map(r => r.right)) + pad, b = Math.max(...rs.map(r => r.bottom)) + pad;
return {x: Math.max(0, l), y: Math.max(0, t), w: Math.min(innerWidth, r) - Math.max(0, l),
        h: Math.min(innerHeight, b) - Math.max(0, t)};
"""

# The lines of the note in the editor, without the empty ones at the end, `pad` around them: as
# wide as what they show (at least 320 CSS pixels), not the whole column.
NOTE_RECT = """
const pad = arguments[0];
const ed = document.querySelector('.ed').getBoundingClientRect();
const lines = [...document.querySelectorAll('.ed > .ln')];
let last = lines.length - 1;
while (last > 0 && !lines[last].textContent.trim() && !lines[last].querySelector('[data-w]')) last--;
const shown = lines.slice(0, last + 1).filter(l => l.getBoundingClientRect().height);
const rs = shown.map(l => l.getBoundingClientRect());
const t = Math.min(...rs.map(r => r.top)) - pad, b = Math.max(...rs.map(r => r.bottom)) + pad;
const range = document.createRange();
// An HTML block's box is as wide as the column, and so is a table's (it scrolls sideways): what
// they hold is measured instead.
const right = Math.max(...shown.map(l => {
  const html = [...l.querySelectorAll('.hw > :not(table), .hw > table :is(th, td)')];
  if (html.length) return Math.max(...html.map(e => e.getBoundingClientRect().right));
  range.selectNodeContents(l); return range.getBoundingClientRect().right; }));
const w = Math.min(ed.width, Math.max(320, right - ed.left)) + 2 * pad;
return {x: ed.left - pad, y: t, w, h: b - t};
"""


class Shots:
    def __init__(self, b, url, only):
        self.b, self.url, self.only = b, url, only
        self.taken = []
        # What went wrong, said at the end: a picture is still taken without a marker it lacks.
        self.problems = []

    def wanted(self, name):
        return not self.only or name in self.only

    def grab(self):
        """The viewport as a PNG (base64), without what only the demo shows: its development-mode
        badge, and the banner of its vault that is full on purpose."""
        self.b.js("""for (const e of document.querySelectorAll('.devstrip, .vault-full-banner'))
            e.style.display = 'none';""")
        time.sleep(0.4)
        return wd.req("GET", self.b.p("/screenshot"))

    def window(self, height):
        """The page `height` CSS pixels tall (a long note fits without scrolling)."""
        w, h, ow, oh = self.b.js("return [innerWidth, innerHeight, outerWidth, outerHeight]")
        wd.req("POST", self.b.p("/window/rect"), {"width": WIDTH + ow - w, "height": height + oh - h})
        time.sleep(0.8)

    def dialog(self):
        """A dialog on the page's own background, not the dimmed app."""
        self.b.js("""const m = document.querySelector('.modal-back');
            if (m) m.style.background = getComputedStyle(document.body).backgroundColor;""")

    def view(self):
        return {"x": 0, "y": 0, "w": WIDTH, "h": HEIGHT}

    def top(self):
        """The note scrolled back to its start (the caret may have scrolled it)."""
        self.b.js("""for (let e = document.querySelector('.ed'); e; e = e.parentElement) e.scrollTop = 0;""")
        time.sleep(0.3)

    def save(self, name, parts, gap=16, captions=None, column=False):
        bg = self.b.js("return getComputedStyle(document.body).backgroundColor") or "#fff"
        out = wd.req("POST", self.b.p("/execute/async"),
                     {"script": ENCODE, "args": [parts, gap, captions, column, bg]})
        if not out or out.startswith("ERROR"):
            raise RuntimeError(f"{name}: {out}")
        OUT.mkdir(parents=True, exist_ok=True)
        path = OUT / f"{name}.webp"
        path.write_bytes(base64.b64decode(out))
        self.taken.append(name)
        print(f"  {path.relative_to(ROOT)} ({path.stat().st_size // 1024} KB)", flush=True)

    def rect(self, *sels, pad=8):
        r = self.b.js(RECT, list(sels), pad)
        if not r:
            raise RuntimeError(f"nothing to crop: {sels}")
        return r

    def inside(self, r, sel):
        """`r` cut to what `sel` covers."""
        o = self.rect(sel, pad=0)
        x, y = max(r["x"], o["x"]), max(r["y"], o["y"])
        return {"x": x, "y": y, "w": min(r["x"] + r["w"], o["x"] + o["w"]) - x,
                "h": min(r["y"] + r["h"], o["y"] + o["h"]) - y}

    def numbered(self, name, buttons_js, around, pad=10):
        """Buttons numbered under them, from the left: `buttons_js` returns their rectangles."""
        buttons = self.b.js(buttons_js)
        r = self.rect(*around, pad=pad)
        self.grab()
        self.b.js(MARK, [[i + 1, x + w / 2, y + h + 20] for i, (x, y, w, h) in enumerate(buttons)])
        self.save(name, [{"png": self.grab(), "rect": {**r, "h": r["h"] + 34}}])
        self.unmark()

    def marks(self, specs):
        """Numbered markers: [(n, selector, text or None, place, offset)]."""
        points = []
        for n, sel, text, place, off in specs:
            p = self.b.js(MARK_AT, sel, text, place, off)
            if not p:
                self.problems.append(f"marker {n}: no {sel} {text or ''}")
                continue
            points.append([n, *p])
        self.b.js(MARK, points)

    def click_on(self, sel, text=None):
        """A real mouse click in the middle of what `sel` (starting with `text`) shows."""
        x, y = self.b.js(MARK_AT, sel, text, "over", 0)
        wd.req("POST", self.b.p("/actions"), {"actions": [{
            "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
            "actions": [{"type": "pointerMove", "origin": "viewport", "x": int(x), "y": int(y)},
                        {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})
        time.sleep(0.5)

    def unmark(self):
        self.b.js("document.getElementById('guide-marks')?.remove()")

    # Getting around the app.

    def route(self, path):
        self.b.js("history.pushState(null, '', arguments[0]); dispatchEvent(new PopStateEvent('popstate'))", path)
        time.sleep(0.8)

    def escape(self):
        self.b.js("document.activeElement?.blur?.()")
        self.b.keys(KEYS["escape"])
        time.sleep(0.2)

    def open(self, title, vault=None):
        """Open a note or drawing by its title (in `vault`), with the quick switcher."""
        self.escape()
        self.b.click(".sb-search")
        self.b.find(".modal.palette input", 5)
        self.b.type(".modal.palette input", title)
        item = None
        end = time.time() + 10
        while item is None and time.time() < end:
            item = self.b.js("""
                const [t, v] = arguments;
                return [...document.querySelectorAll('.palette-item')].findIndex(i =>
                  i.querySelector('.grow').textContent.trim() === t &&
                  (!v || i.querySelector('.muted').textContent.trim().startsWith(v)))""", title, vault)
            if item is not None and item < 0:
                item = None
                time.sleep(0.2)
        if item is None:
            raise RuntimeError(f"no note {title} in the quick switcher")
        self.b.js("document.querySelectorAll('.palette-item')[arguments[0]].click()", item)
        time.sleep(1.2)
        # Notes without the side panel, which gives the text more room (a preference: closed
        # once, closed for every note).
        if self.b.js("return !!document.querySelector('aside.panel')"):
            self.b.click(".note-head [aria-label='Side panel']")
            time.sleep(1.5)  # (until the header has moved to where it stays)

    def rest_caret(self):
        """The caret on the note's last (empty) line, where it shows no marks, and not drawn."""
        self.b.js("""const ed = document.querySelector('.ed'); ed.style.caretColor = 'transparent'; ed.focus();
            const last = ed.lastElementChild; getSelection().collapse(last, last.childNodes.length);""")
        self.b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.6)

    def example(self, name, title, wait=None, before=None):
        """The lines of an example note, as the editor shows them (in a window tall enough for
        them, without the toolbar over their top)."""
        if not self.wanted(name):
            return
        self.open(title, "Guide")
        self.b.find(".ed .ln", 10)
        self.b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        self.rest_caret()
        self.top()
        if wait:
            self.b.find(wait, 40)
            time.sleep(0.6)
        if before:
            before()
        self.save(name, [{"png": self.grab(), "rect": self.b.js(NOTE_RECT, 14)}])


def sign_in(b, url):
    b.go(url)
    b.find(".dev-box input", 30)
    b.type(".dev-box input", "alice")
    b.click(".dev-box button[type=submit]")
    # The demo approves new browsers.
    b.click_el(b.find_text(".auth-card button", "Approve from another device", 20))
    b.find(".sidebar", 60)
    time.sleep(2)


# ------------------------------------------------------------------ the pictures

def getting_started(s):
    b = s.b
    if s.wanted("gs-app"):
        s.open("Tomatoes", "Home")
        s.rest_caret()
        s.top()
        s.grab()
        s.marks([
            (1, ".vault-btn", None, "right", 10),
            (2, ".sb-search", None, "tl", 2),
            (3, ".sb-actions", None, "tl", 2),
            (4, ".sb-link", "Quick notes", "right", -40),
            (5, ".sb-label", "Recent", "right", -110),
            (6, ".sb-label", "Notes", "right", -110),
            (7, ".sb-bottom", None, "tr", 30),
            (8, "[aria-label='Change icon'], [aria-label='Add an icon']", None, "left", 14),
            (9, ".toolbar", None, "left", 14),
            (10, ".note-head .seg", None, "left", 14),
        ])
        s.save("gs-app", [{"png": s.grab(), "rect": s.view()}])
        s.unmark()
    if s.wanted("gs-note-buttons"):
        s.open("Tomatoes", "Home")
        s.top()
        s.numbered("gs-note-buttons", """const seg = document.querySelector('.note-head .seg').getBoundingClientRect();
            return [...document.querySelectorAll('.note-head button')].map(e => e.getBoundingClientRect())
              .filter(r => r.width && r.left >= seg.left - 1 && r.top < seg.bottom)
              .sort((a, b) => a.left - b.left).map(r => [r.left, r.top, r.width, r.height]);""",
                   [".note-head .seg", ".note-head [aria-label='More']"])
    if s.wanted("gs-search"):
        s.escape()
        b.chord(KEYS["ctrl"], KEYS["shift"], "f")
        b.find(".modal input", 5)
        b.type(".modal input", "tag:garden water")
        time.sleep(1.5)
        s.dialog()
        s.save("gs-search", [{"png": s.grab(), "rect": s.rect(".modal", pad=12)}])
        s.escape()
    if s.wanted("gs-graph"):
        s.open("Tomatoes", "Home")
        s.escape()
        b.chord(KEYS["ctrl"], "g")
        time.sleep(4)
        s.save("gs-graph", [{"png": s.grab(), "rect": s.rect("main.main", pad=0)}])
    if s.wanted("gs-share"):
        s.open("Holiday plans", "Family")
        b.click_el(b.find_text(".sb-bottom button", "Share vault", 5))
        b.find(".modal .people .person", 10)
        time.sleep(1)
        s.dialog()
        s.save("gs-share", [{"png": s.grab(), "rect": s.rect(".modal", pad=12)}])
        s.escape()
    if s.wanted("gs-comments"):
        s.open("Holiday plans", "Family")
        b.click(".seg button[aria-label='Reading view']")
        b.find(".reading [data-thread]", 15)
        time.sleep(0.8)
        b.js("""const e = [...document.querySelectorAll('.reading [data-thread]')]
            .find(e => e.textContent.startsWith('Camping'));
            e.classList.add('guide-pick'); e.scrollIntoView({block: 'center'});""")
        time.sleep(0.5)
        # (Measured first: opening the thread draws the text again.)
        text = s.rect(".guide-pick", pad=16)
        s.click_on(".reading [data-thread]", "Camping")
        b.find(".cm-pop", 10)
        time.sleep(1)
        pop = s.rect(".cm-pop", pad=16)
        x, y = min(text["x"], pop["x"]), min(text["y"], pop["y"])
        r = {"x": x, "y": y, "w": max(text["x"] + text["w"], pop["x"] + pop["w"]) - x,
             "h": max(text["y"] + text["h"], pop["y"] + pop["h"]) - y}
        s.save("gs-comments", [{"png": s.grab(), "rect": r}])
        s.escape()
        b.click(".seg button[title='Live preview']")
    if s.wanted("gs-history"):
        s.open("Garden plan", "Home")
        b.click(".note-head [aria-label='Version history']")
        b.find(".modal .h-item", 10)
        b.js("document.querySelectorAll('.modal .h-item')[1]?.click()")
        b.find(".modal .h-preview .reading", 10)
        time.sleep(1.5)
        s.dialog()
        s.save("gs-history", [{"png": s.grab(), "rect": s.rect(".modal", pad=12)}])
        s.escape()
    if s.wanted("gs-quick"):
        s.route("/quick")
        time.sleep(1)
        b.js("document.activeElement?.blur?.()")
        r = s.inside(s.rect(".quick-page .page-head", ".quick-page .page-body > *", pad=8), "main.main")
        s.save("gs-quick", [{"png": s.grab(), "rect": r}])


def writing(s):
    b = s.b
    if s.wanted("wr-toolbar"):
        s.open("Trip to Lisbon", "Guide")
        s.numbered("wr-toolbar", """return [...document.querySelectorAll('.toolbar > .tb, .toolbar > .tb-drop > .tb')]
            .map(e => e.getBoundingClientRect()).filter(r => r.width).sort((a, b) => a.left - b.left)
            .map(r => [r.left, r.top, r.width, r.height]);""", [".toolbar"], pad=6)
    s.window(1700)
    if s.wanted("wr-preview-source"):
        s.open("Trip to Lisbon", "Guide")
        b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        s.rest_caret()
        s.top()
        live = {"png": s.grab(), "rect": b.js(NOTE_RECT, 14)}
        b.click(".seg button[title='Markdown source']")
        time.sleep(0.8)
        b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        s.rest_caret()
        s.top()
        source = {"png": s.grab(), "rect": b.js(NOTE_RECT, 14)}
        b.click(".seg button[title='Live preview']")
        s.save("wr-preview-source", [live, source], gap=24, column=True,
               captions=["Live preview", "Markdown source"])
    s.example("wr-lists", "Lists")
    s.example("wr-links", "Links")
    s.example("wr-table", "Table")
    s.example("wr-html-table", "HTML table", wait=".ed .hw table")
    s.example("wr-callouts", "Callouts")
    s.example("wr-code", "Code")
    s.example("wr-math", "Math", wait=".ed .mw")
    s.example("wr-chemistry", "Chemistry", wait=".ed .mth")
    s.example("wr-diagram", "Diagram", wait=".ed .w.dg svg")
    if s.wanted("wr-diagram-editing"):
        s.open("Diagram", "Guide")
        b.find(".ed .w.dg svg", 40)
        b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        s.rest_caret()
        s.top()
        b.click(".ed .w.dg svg .node")
        time.sleep(0.8)
        b.js("document.querySelector('.ed').style.caretColor = 'transparent'")
        s.save("wr-diagram-editing", [{"png": s.grab(), "rect": b.js(NOTE_RECT, 14)}])
    s.example("wr-more-diagrams", "More diagrams", wait=".ed .w.dg svg")
    s.example("wr-dates", "Dates")
    if s.wanted("wr-picture"):
        s.open("Garden plan", "Home")
        b.find(".ed img", 20)
        b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        s.rest_caret()
        s.top()
        time.sleep(1)
        s.save("wr-picture", [{"png": s.grab(), "rect": s.rect(".ed > .ln:nth-child(-n+5)", pad=14)}])
    s.window(HEIGHT)


def drawings(s):
    b = s.b
    if s.wanted("dr-app") or s.wanted("dr-selected"):
        s.open("Tomatoes", "Home")  # (the quick switcher finds notes, not drawings)
        b.click_el(b.find_text(".sb-link", "Drawings", 5))
        b.click_el(b.find_text(".drawing-card", "Garden beds", 15))
        b.find(".drawing-canvas", 20)
        time.sleep(2)
        if s.wanted("dr-app"):
            s.marks([
                (1, ".drawing-tools", None, "left", 14),
                (2, ".drawing-name-field", None, "left", 14),
                (3, "[aria-label='Saved shapes']", None, "bottom", 14),
                (4, "[aria-label='Version history']", None, "bottom", 14),
                (5, "[aria-label='Export']", None, "bottom", 14),
                (6, ".drawing-zoom", None, "top", 14),
                (7, ".drawing-history", None, "top", 14),
                (8, ".drawing-snap", None, "top", 14),
            ])
            s.save("dr-app", [{"png": s.grab(), "rect": s.view()}])
            s.unmark()
        if s.wanted("dr-selected"):
            b.click(".drawing-tools [aria-label='Selection']")
            # The box "Herbs" (where the demo's drawing has it, shown whole at this size).
            b.click_at(".drawing-canvas", -162, -131)
            b.find(".drawing-props", 5)
            time.sleep(1)
            s.save("dr-selected", [{"png": s.grab(), "rect": s.rect("main.main", pad=0)}])
    if s.wanted("dr-in-note"):
        s.open("Beds", "Home")
        s.window(1700)
        b.js("document.querySelector('.toolbar-wrap').style.visibility = 'hidden'")
        s.rest_caret()
        s.top()
        time.sleep(2)
        s.save("dr-in-note", [{"png": s.grab(), "rect": b.js(NOTE_RECT, 14)}])
        s.window(HEIGHT)


# ------------------------------------------------------------------ running it

def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--web", type=Path, default=ROOT / "target" / "guide-web")
    ap.add_argument("--server-bin", type=Path, default=ROOT / "target" / "debug" / "notekpr-server")
    ap.add_argument("--cli-bin", type=Path, default=ROOT / "target" / "debug" / "notekpr")
    ap.add_argument("--only", default="", help="comma-separated names (gs-app, wr-math, …): only these")
    ap.add_argument("--keep", action="store_true", help="keep the demo running afterwards (Ctrl+C stops it)")
    ap.add_argument("--data", type=Path,
                    help="the demo's folder, kept to be used again (faster); by default a temporary one")
    args = ap.parse_args()
    only = {n for n in args.only.split(",") if n}

    data = args.data.resolve() if args.data else Path(tempfile.mkdtemp(prefix="notekpr-guide-"))
    data.mkdir(parents=True, exist_ok=True)
    # The demo's CLI profiles keep its address: a demo used again runs where it ran.
    saved = data / "port"
    port = int(saved.read_text()) if saved.exists() else free_port()
    saved.write_text(str(port))
    wd_port = free_port()
    url = f"http://localhost:{port}/"
    log = open(data / "demo.log", "w")
    print(f"seeding a demo in {data} (log: demo.log there)…", flush=True)
    demo = subprocess.Popen(
        [sys.executable, "-u", str(ROOT / "scripts" / "demo.py"), "--listen", f"127.0.0.1:{port}",
         "--data", str(data / "demo"), "--web-dir", str(args.web), "--server-bin", str(args.server_bin),
         "--cli-bin", str(args.cli_bin)],
        stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
    driver = None
    b = s = None
    try:
        end = time.time() + 600
        while "Demo running on" not in (data / "demo.log").read_text():
            if demo.poll() is not None or time.time() > end:
                sys.exit(f"the demo didn't start; see {data / 'demo.log'}")
            time.sleep(1)
        env = dict(os.environ, NOTEKPR_CONFIG_DIR=str(data / "demo" / "cli" / "config"),
                   NOTEKPR_DATA_DIR=str(data / "demo" / "cli" / "data"))

        def cli(*a, stdin=b"", check=True):
            return subprocess.run([str(args.cli_bin), "-p", "alice", *a], input=stdin, env=env,
                                  check=check, capture_output=True, text=False)
        if b"Guide" not in cli("vault", "list").stdout:
            cli("-q", "vault", "create", "Guide")
        for title, text in EXAMPLES.items():
            cli("-q", "write", "Guide", f"{title}.md", stdin=note(text).encode())

        driver = subprocess.Popen([shutil.which("chromedriver") or "chromedriver", f"--port={wd_port}"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.environ["NOTEKPR_WEBDRIVER"] = f"http://127.0.0.1:{wd_port}"
        time.sleep(1)
        b = wd.Browser(WIDTH, HEIGHT, scale=SCALE)
        sign_in(b, url)
        # The page itself WIDTH × HEIGHT: the window is a little larger.
        w, h, ow, oh = b.js("return [innerWidth, innerHeight, outerWidth, outerHeight]")
        wd.req("POST", b.p("/window/rect"), {"width": WIDTH + ow - w, "height": HEIGHT + oh - h})
        time.sleep(1)
        s = Shots(b, url, only)
        print("taking the pictures:", flush=True)
        getting_started(s)
        writing(s)
        drawings(s)
        missing = only - set(s.taken)
        if missing:
            s.problems.append(f"no such pictures: {', '.join(sorted(missing))}")
        for p in s.problems:
            print(f"problem: {p}", file=sys.stderr)
        if args.keep:
            print(f"the demo runs on {url} (alice); Ctrl+C stops it", flush=True)
            demo.wait()
    except KeyboardInterrupt:
        pass
    except Exception:
        if b:
            print(f"the page then: {b.shot('guide-shots-failure')}", file=sys.stderr)
        raise
    finally:
        if b:
            b.close()
        if driver:
            driver.terminate()
        demo.terminate()
        try:
            demo.wait(20)
        except subprocess.TimeoutExpired:
            demo.kill()
        if not args.data:
            shutil.rmtree(data, ignore_errors=True)
    if s and s.problems:
        sys.exit(1)


if __name__ == "__main__":
    main()
