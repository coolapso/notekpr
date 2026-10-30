"""Minimal W3C WebDriver client (stdlib only) used by the browser end-to-end tests."""

import base64, json, time, urllib.request

import os
def _wd():
    return os.environ.get("NOTEKPR_WEBDRIVER", "http://127.0.0.1:19515")
KEYS = {"enter": "", "backspace": "", "tab": "", "escape": "", "ctrl": "", "alt": "",
        "shift": "", "left": "", "right": "", "up": "", "down": "", "home": "", "end": ""}


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(_wd() + path, data=data, method=method, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(r, timeout=60) as resp:
            return json.loads(resp.read() or b"{}").get("value")
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"{method} {path}: {e.read().decode()[:500]}")


class Browser:
    def __init__(self, width=1400, height=900):
        caps = {"capabilities": {"alwaysMatch": {
            "browserName": "chrome",
            "goog:chromeOptions": {"binary": os.environ.get("NOTEKPR_CHROME") or "/usr/bin/chromium",
                                   "args": ["--headless=new", "--no-sandbox", f"--window-size={width},{height}",
                                            "--disable-gpu", "--lang=en-US"]},
            "goog:loggingPrefs": {"browser": "ALL"}}}}
        v = req("POST", "/session", caps)
        self.sid = v["sessionId"]

    def p(self, suffix):
        return f"/session/{self.sid}{suffix}"

    def go(self, url):
        req("POST", self.p("/url"), {"url": url})

    def js(self, script, *args):
        return req("POST", self.p("/execute/sync"), {"script": script, "args": list(args)})

    def find(self, css, timeout=10):
        end = time.time() + timeout
        last = None
        while time.time() < end:
            try:
                v = req("POST", self.p("/element"), {"using": "css selector", "value": css})
                return list(v.values())[0]
            except RuntimeError as e:
                last = e
                time.sleep(0.15)
        raise RuntimeError(f"element not found: {css} ({last})")

    def find_text(self, css, text, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            els = req("POST", self.p("/elements"), {"using": "css selector", "value": css})
            for e in els:
                eid = list(e.values())[0]
                try:
                    t = req("GET", self.p(f"/element/{eid}/text"))
                except RuntimeError:
                    continue
                if text in (t or ""):
                    return eid
            time.sleep(0.2)
        raise RuntimeError(f"no {css} containing {text!r}")

    def click(self, css_or_id, timeout=10):
        eid = css_or_id if not any(c in css_or_id for c in " .#[:>") and len(css_or_id) > 30 else self.find(css_or_id, timeout)
        req("POST", self.p(f"/element/{eid}/click"), {})

    def click_el(self, eid):
        req("POST", self.p(f"/element/{eid}/click"), {})

    def type(self, css, text):
        eid = self.find(css)
        req("POST", self.p(f"/element/{eid}/value"), {"text": text})

    def keys(self, text):
        """Type into the focused element using the Actions API (real key events)."""
        actions = []
        for ch in text:
            actions.append({"type": "keyDown", "value": ch})
            actions.append({"type": "keyUp", "value": ch})
        req("POST", self.p("/actions"), {"actions": [{"type": "key", "id": "kb", "actions": actions}]})

    def chord(self, *keys):
        down = [{"type": "keyDown", "value": k} for k in keys]
        up = [{"type": "keyUp", "value": k} for k in reversed(keys)]
        req("POST", self.p("/actions"), {"actions": [{"type": "key", "id": "kb", "actions": down + up}]})

    def click_at(self, css, x=0, y=0):
        """A real mouse click (pointer events) at (`x`, `y`) from the centre of `css`."""
        el = {"element-6066-11e4-a52e-4f735466cecf": self.find(css)}
        req("POST", self.p("/actions"), {"actions": [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
            "actions": [{"type": "pointerMove", "origin": el, "x": x, "y": y},
                        {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})

    def text(self, css):
        return req("GET", self.p(f"/element/{self.find(css)}/text"))

    def wait(self, cond_js, timeout=10, msg="condition", *args):
        """Until `cond_js` (given `args` as `arguments`) is true."""
        end = time.time() + timeout
        while time.time() < end:
            if self.js(cond_js, *args):
                return
            time.sleep(0.2)
        self.shot("timeout")
        raise RuntimeError(f"timed out waiting for {msg}: {cond_js}")

    def shot(self, name):
        png = req("GET", self.p("/screenshot"))
        out = os.environ.get("NOTEKPR_E2E_OUT", "target/e2e")
        os.makedirs(out, exist_ok=True)
        path = os.path.join(out, f"{name}.png")
        open(path, "wb").write(base64.b64decode(png))
        return path

    def new_tab(self):
        """Open a tab in the same browser profile (shared storage) and switch to it."""
        h = req("POST", self.p("/window/new"), {"type": "tab"})["handle"]
        self.switch(h)
        return h

    def switch(self, handle):
        req("POST", self.p("/window"), {"handle": handle})

    def tab(self):
        return req("GET", self.p("/window"))

    def close_tab(self):
        """Close the current tab; switch to another one afterwards."""
        return req("DELETE", self.p("/window"))

    def latency(self, ms):
        """Answer every request `ms` later, as a server across a network does
        (a local one answers at once, which hides races)."""
        req("POST", self.p("/chromium/network_conditions"), {"network_conditions": {
            "offline": False, "latency": ms, "download_throughput": -1, "upload_throughput": -1}})

    def logs(self):
        try:
            return req("POST", self.p("/se/log"), {"type": "browser"})
        except RuntimeError:
            return []

    def close(self):
        try:
            req("DELETE", self.p(""))
        except Exception:
            pass

def base_url():
    return os.environ.get("NOTEKPR_URL", "http://localhost:18080/")


def editor_text(b):
    return b.js("return Array.from(document.querySelectorAll('.ed > .ln')).map(l => { const c = l.cloneNode(true); c.querySelectorAll('[data-w]').forEach(w => w.remove()); return c.textContent; }).join('\\n')")


def signup(b, name):
    b.go(base_url())
    b.find(".dev-box input", 20)
    b.type(".dev-box input", name)
    b.click(".dev-box button[type=submit]")
    b.click_el(b.find_text(".auth-card button", "Create my keys", 15))
    b.find(".recovery-key", 15)
    b.click(".check input")
    b.click_el(b.find_text(".auth-card button", "Continue"))
    b.find(".ed .ln", 20)
