#!/usr/bin/env python3
"""Drawings in the browser, checked from the CLI: drawing with the tools,
arrows attaching to shapes, a line's points edited, the Export menu, what
reaches the server.

    python3 tests/e2e/drawings.py --server target/debug/notekpr-server \
        --cli target/debug/notekpr --web dist/web
"""
import argparse, json, os, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from suite import Env, uniq, check, share, severe_logs  # noqa: E402
from wd import Browser, KEYS, req, signup  # noqa: E402

CANVAS = ".drawing-canvas"


def drag(b, frm, to, steps=6, hold=None, pause=0):
    """A mouse drag on the canvas, from and to points relative to its
    centre; `hold`: a key held down meanwhile; `pause`: ms held still at
    the end before letting go."""
    el = {"element-6066-11e4-a52e-4f735466cecf": b.find(CANVAS)}
    acts = [{"type": "pointerMove", "origin": el, "x": frm[0], "y": frm[1]}, {"type": "pointerDown", "button": 0}]
    for i in range(1, steps + 1):
        x = frm[0] + (to[0] - frm[0]) * i / steps
        y = frm[1] + (to[1] - frm[1]) * i / steps
        acts.append({"type": "pointerMove", "origin": el, "x": int(x), "y": int(y), "duration": 16})
    if pause:
        acts.append({"type": "pause", "duration": pause})
    acts.append({"type": "pointerUp", "button": 0})
    sources = [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"}, "actions": acts}]
    if hold:
        keys = [{"type": "keyDown", "value": hold}] + [{"type": "pause", "duration": 0}] * (len(acts) - 1) + [{"type": "keyUp", "value": hold}]
        sources.append({"type": "key", "id": "kb", "actions": keys})
    req("POST", b.p("/actions"), {"actions": sources})
    time.sleep(0.4)


def tool(b, key):
    b.js("document.querySelector('.drawing-page').focus()")
    b.keys(key)
    time.sleep(0.2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/notekpr-server")
    ap.add_argument("--cli", default="target/debug/notekpr")
    ap.add_argument("--web", default="dist/web")
    args = ap.parse_args()
    env = Env(args.server, args.web)
    home = tempfile.mkdtemp(prefix="notekpr-cli-")
    cenv = dict(os.environ, NOTEKPR_CONFIG_DIR=f"{home}/config", NOTEKPR_DATA_DIR=f"{home}/data")
    cli = lambda *a: subprocess.run([os.path.abspath(args.cli), "-p", "bob", *a], env=cenv, capture_output=True, text=True, timeout=60)
    a = Browser()

    def drawing():
        """The drawing as the server has it, read by the CLI."""
        time.sleep(1.5)
        r = cli("cat", "Art", "Untitled drawing")
        check(r.returncode == 0, "the CLI reads the drawing")
        d = json.loads(r.stdout)
        return [e for e in d["elements"] if not e.get("isDeleted")]

    try:
        an, bn = uniq("amy"), uniq("bert")
        signup(a, an)
        r = cli("login", "--server", env.url.rstrip("/"), "--dev-user", bn, "--device-name", "laptop")
        check(r.returncode == 0, "CLI login")
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Art")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Art", 15)
        share(a, ".sb-bottom button:nth-child(2)", bn)
        a.click(".sb-actions button[aria-label='New drawing']")
        a.find(CANVAS, 10)
        time.sleep(1)

        # Two boxes and an arrow from one to the other: it attaches to both.
        # (The properties panel covers the canvas's left side.)
        tool(a, "r")
        drag(a, (-250, -150), (-100, -50))
        tool(a, "r")
        drag(a, (150, -150), (300, -50))
        tool(a, "a")
        drag(a, (-175, -100), (225, -100))
        els = drawing()
        rects = [e["id"] for e in els if e["type"] == "rectangle"]
        check(len(rects) == 2, f"two rectangles drawn ({[e['type'] for e in els]})")
        arrow = next(e for e in els if e["type"] == "arrow")
        check((arrow.get("startBinding") or {}).get("elementId") in rects
              and (arrow.get("endBinding") or {}).get("elementId") in rects
              and arrow["startBinding"]["elementId"] != arrow["endBinding"]["elementId"],
              "an arrow drawn from one box to another attaches to both")

        # A line: its end dragged, then the handle halfway along it bends it.
        tool(a, "l")
        drag(a, (-250, 100), (-50, 100))
        drag(a, (-50, 100), (-50, 200))
        line = next(e for e in drawing() if e["type"] == "line")
        check(len(line["points"]) == 2 and abs(line["height"] - 100) < 2, "a selected line's end is dragged on its own")
        drag(a, (-150, 150), (-210, 220))
        line = next(e for e in drawing() if e["type"] == "line")
        check(len(line["points"]) == 3, "the handle halfway along a line adds a point")
        a.shot("drawings-line")
        # Editing its points (Enter): the middle one deleted.
        tool(a, KEYS["enter"])
        a.click_at(CANVAS, -210, 220)
        time.sleep(0.3)
        tool(a, KEYS["backspace"])
        tool(a, KEYS["escape"])
        line = next(e for e in drawing() if e["type"] == "line")
        check(len(line["points"]) == 2, "a point deleted while editing the line's points")

        # Alt+drag: a copy of the box moves, the box stays.
        tool(a, KEYS["escape"])
        a.click_at(CANVAS, -250, -100)
        time.sleep(0.3)
        # (Its edges are handles once selected: from inside it.)
        drag(a, (-200, -80), (-200, 270), hold=KEYS["alt"])
        check(sum(e["type"] == "rectangle" for e in drawing()) == 3, "Alt+drag makes a copy")

        # The Export menu opens inside the window, with our own file first.
        a.click(".drawing-page .note-head .tb-drop .icon-btn")
        a.find(".drawing-page .note-head .menu", 5)
        left, right, width = a.js("const r = document.querySelector('.drawing-page .note-head .menu').getBoundingClientRect(); return [r.left, r.right, window.innerWidth]")
        check(left >= 0 and right <= width, f"the Export menu fits in the window ({left:.0f}–{right:.0f} of {width})")
        first = a.text(".drawing-page .note-head .menu button")
        check(".drawing" in first, "the first export is a .drawing file")
        a.chord(KEYS["escape"])

        a.shot("drawings")
        check(not severe_logs(a), "no browser errors")
        print("ALL PASSED")
    finally:
        a.close()
        env.close()


if __name__ == "__main__":
    main()
