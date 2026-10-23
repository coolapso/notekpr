#!/usr/bin/env python3
"""Browser end-to-end tests for notekpr (stdlib + chromedriver only).

Starts a development-mode server and chromedriver, then drives several
independent headless Chromium profiles as different users.

    python3 tests/e2e/suite.py --server target/debug/notekpr-server --web dist/web
    python3 tests/e2e/suite.py -k collab        # run only matching tests

Screenshots are written to $NOTEKPR_E2E_OUT (default target/e2e).
"""
import argparse, json, os, shutil, socket, subprocess, sys, tempfile, time, traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


class Env:
    def __init__(self, server, web):
        self.server_bin, self.web = os.path.abspath(server), os.path.abspath(web)
        self.data = tempfile.mkdtemp(prefix="notekpr-e2e-")
        self.port, self.wd_port = free_port(), free_port()
        self.url = f"http://localhost:{self.port}/"
        os.environ["NOTEKPR_URL"] = self.url
        os.environ["NOTEKPR_WEBDRIVER"] = f"http://127.0.0.1:{self.wd_port}"
        self.server = None
        self.driver = subprocess.Popen([shutil.which("chromedriver") or "chromedriver", f"--port={self.wd_port}"],
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.start_server()
        time.sleep(1)

    def start_server(self):
        log = open(os.path.join(self.data, "server.log"), "a")
        self.server = subprocess.Popen([self.server_bin, "--dev-mode", "--web-dir", self.web, "--data-dir", self.data,
                                        "--listen", f"127.0.0.1:{self.port}", "--public-url", self.url[:-1]],
                                       stdout=log, stderr=log)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", self.port), timeout=0.2).close()
                return
            except OSError:
                time.sleep(0.1)
        raise RuntimeError("server did not start")

    def stop_server(self):
        """Stop it as an update does (SIGTERM): it tells clients it's restarting."""
        if self.server:
            self.server.terminate()
            self.server.wait(10)
            self.server = None

    def kill_server(self):
        """Kill it without a word (SIGKILL), as a crash would."""
        if self.server:
            self.server.kill()
            self.server.wait(10)
            self.server = None

    def close(self):
        self.stop_server()
        self.driver.terminate()
        shutil.rmtree(self.data, ignore_errors=True)


import wd  # noqa: E402  (after env vars are set)
from wd import Browser, KEYS, editor_text, signup  # noqa: E402


def uniq(prefix):
    return f"{prefix}{int(time.time() * 1000) % 10_000_000}"


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)
    print(f"    ✓ {msg}")


def severe_logs(*browsers):
    out = []
    for b in browsers:
        for l in b.logs() or []:
            m = l.get("message", "")
            # Requests cut off on purpose (set_offline, a stopped server) aren't errors.
            expected = ("WebSocket connection", "ERR_CONNECTION_REFUSED", "ERR_INTERNET_DISCONNECTED")
            if l.get("level") == "SEVERE" and not any(e in m for e in expected):
                out.append(m[:300])
    for m in out:
        print(f"    ! browser: {m}")
    return out


def new_note(b, title, body):
    b.click(".sb-actions .btn.primary")
    b.find(".note-title", 10)
    time.sleep(0.4)
    b.keys(title + KEYS["enter"])
    time.sleep(0.4)
    b.keys(body)
    time.sleep(0.6)


def share(owner, vault_btn_css, user, role="editor"):
    owner.click(vault_btn_css)
    owner.type(".share-add input", user)
    owner.find(".found .person", 10)
    if role == "viewer":
        owner.js("const s=document.querySelector('.share-add select'); s.value='viewer'; s.dispatchEvent(new Event('change'))")
    owner.click(".found .person .btn.primary")
    owner.find_text(".people .person", user.capitalize(), 10)
    owner.chord(KEYS["escape"])


# ---------------------------------------------------------------- tests

def test_onboarding(env):
    b = Browser()
    try:
        # A reload on the recovery-key screen shows the same key again.
        b.go(env.url)
        b.find(".dev-box input", 20)
        b.type(".dev-box input", uniq("eve"))
        b.click(".dev-box button[type=submit]")
        b.click_el(b.find_text(".auth-card button", "Create my keys", 15))
        key = b.find_text(".recovery-key", "-", 15) and b.js("return document.querySelector('.recovery-key').textContent")
        b.go(env.url)
        b.wait("return document.querySelector('.recovery-key') && document.querySelector('.recovery-key').textContent === %r" % key, 20,
               "the recovery key after a reload")
        check(True, "a reload before the recovery key is confirmed shows it again")
        b.click(".check input")
        b.click_el(b.find_text(".auth-card button", "Continue"))
        b.find(".ed .ln", 20)
        b.go(env.url)
        b.find(".ed .ln", 20)
        check(not b.js("return !!document.querySelector('.recovery-key')"), "once confirmed it isn't shown again")
        b.close()
        b = Browser()
        signup(b, uniq("ada"))
        check(b.js("return document.querySelector('.note-title').value") == "Welcome", "welcome note is created and opened")
        check(b.js("return document.querySelectorAll('.ed .ln').length") > 5, "live editor renders the note")
        time.sleep(1)
        b.go(env.url)
        try:
            b.find(".ed .ln", 20)
        except Exception:
            b.shot("onboarding-reload-fail")
            print("DEBUG url", b.js("return location.href"), "body", b.js("return document.body.innerText.slice(0,300)"))
            for l in b.logs() or []:
                print("DEBUG LOG", l.get("level"), l.get("message")[:300])
            raise
        check(True, "reload restores session and unlocks with the device key")
        b.shot("onboarding")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_collab(env):
    a, b = Browser(), Browser()
    try:
        an, bn = uniq("ann"), uniq("ben")
        signup(a, an)
        signup(b, bn)
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Family")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Family", 15)
        a.click_el(a.find_text(".qa", "New note"))
        a.find(".note-title", 10)
        time.sleep(0.5)
        a.keys("Groceries" + KEYS["enter"])
        time.sleep(0.5)
        a.keys("- milk" + KEYS["enter"] + "eggs" + KEYS["enter"] + KEYS["enter"] + "Thanks!")
        time.sleep(0.8)
        check(a.js("return document.querySelector('.note-title').value") == "Groceries", "new note is titled from the title field")
        check(editor_text(a) == "- milk\n- eggs\n\nThanks!", "list continuation and list exit work")
        url = a.js("return location.pathname")
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.click(".vault-btn")
        b.find_text(".vault-menu button", "Family", 15)
        check(True, "a newly shared vault appears for the recipient without reloading")
        b.chord(KEYS["escape"])
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Thanks!')", 20, "ben sees the note")
        check(True, "shared note decrypts for the second user")
        offers_share = "return Array.from(document.querySelectorAll(arguments[0] + ' button')).map(e => e.textContent).join('|')"
        b.js("Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === 'Groceries').querySelector('.tree-more').click()")
        b.find(".tree-row .menu button", 5)
        in_tree = b.js(offers_share, ".tree-row .menu")
        b.chord(KEYS["escape"])
        b.click(".note-head button[title='More']")
        b.find(".note-head .menu button", 5)
        in_note = b.js(offers_share, ".note-head .menu")
        b.chord(KEYS["escape"])
        check("favorites" in in_tree and "Download" in in_note and "Share" not in in_tree + in_note,
              "only the owner is offered to share a note")
        vid = url.split('/')[2]
        log = api_get(b, f"/vaults/{vid}/log")
        check([e["body"]["kind"] for e in log["entries"]] == ["create", "require_signed", "add"],
              "the owner signed the vault's creation, the signature requirement and ben's addition")
        ch = api_get(b, f"/vaults/{vid}/changes?since=0")
        check(ch["updates"] and all(u.get("signature") for u in ch["updates"]),
              "every change is signed by its author")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(KEYS["enter"] + "bread")
        a.wait("return document.querySelector('.ed').textContent.includes('bread')", 10, "alice sees ben's edit")
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const r=document.createRange(); r.setStart(ed.firstChild.firstChild || ed.firstChild, 0); r.collapse(true); const s=getSelection(); s.removeAllRanges(); s.addRange(r);")
        a.keys("Shopping: ")
        b.wait("return document.querySelector('.ed').textContent.includes('Shopping: ')", 10, "ben sees alice's edit")
        time.sleep(1)
        check(editor_text(a) == editor_text(b), "both editors converge to identical text")
        # An IME composition that ends while a remote change is waiting must neither
        # kill the editor nor be applied at outdated offsets.
        a.js("document.querySelector('.ed').dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true, data: ''}));")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(" remote")
        time.sleep(2)
        check("remote" not in editor_text(a), "remote text waits while a composition is in progress")
        a.js("""
            const ed = document.querySelector('.ed');
            const n = document.createTreeWalker(ed.firstElementChild, NodeFilter.SHOW_TEXT).nextNode();
            n.data = 'かな' + n.data;
            ed.dispatchEvent(new InputEvent('input', {bubbles: true, isComposing: true, inputType: 'insertCompositionText', data: 'かな'}));
            ed.dispatchEvent(new CompositionEvent('compositionend', {bubbles: true, data: 'かな'}));
        """)
        a.wait("return document.querySelector('.ed').textContent.includes('remote')", 10, "remote text after the composition")
        b.wait("return document.querySelector('.ed').textContent.includes('かな')", 10, "the composition reaches ben")
        time.sleep(1)
        check(editor_text(a) == editor_text(b) and editor_text(a).startswith("かなShopping") and editor_text(a).endswith(" remote"),
              "a composition committed while remote text waited lands where it was typed")
        # A cancelled composition (no text) applies the waiting remote text.
        a.js("document.querySelector('.ed').dispatchEvent(new CompositionEvent('compositionstart', {bubbles: true, data: ''}));")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(" again")
        time.sleep(2)
        a.js("document.querySelector('.ed').dispatchEvent(new CompositionEvent('compositionend', {bubbles: true, data: ''}));")
        a.wait("return document.querySelector('.ed').textContent.includes('remote again')", 10, "remote text after a cancelled composition")
        check(True, "remote text that waited for a cancelled composition is shown")
        # Remote edits don't scroll the page back to the caret.
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
             "const dt = new DataTransfer(); dt.setData('text/plain', '\\nfiller'.repeat(150)); ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));")
        b.wait("return document.querySelector('.ed').textContent.endsWith('fillerfiller')", 10, "ben gets the long text")
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const r=document.createRange(); r.setStart(ed.firstChild.firstChild || ed.firstChild, 0); r.collapse(true); const s=getSelection(); s.removeAllRanges(); s.addRange(r);")
        time.sleep(0.3)
        a.js("document.querySelector('.main').scrollTop = 1e6;")
        time.sleep(0.3)
        top = a.js("return document.querySelector('.main').scrollTop")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(" last")
        a.wait("return document.querySelector('.ed').textContent.endsWith('filler last')", 10, "alice gets ben's edit")
        time.sleep(0.5)
        check(top > 500 and abs(a.js("return document.querySelector('.main').scrollTop") - top) < 5,
              "a remote edit doesn't scroll the page back to the caret")
        check(a.js("return document.querySelectorAll('.presence .avatar').length") >= 1, "collaborator presence is shown")
        check(a.js("return document.querySelectorAll('.rc').length") >= 1, "remote cursor is shown")
        a.shot("collab-alice")
        b.shot("collab-ben")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_devices_and_recovery(env):
    name = uniq("dora")
    a, b2, b3 = Browser(), Browser(), Browser()
    try:
        signup(a, name)
        b2.go(env.url)
        b2.find(".dev-box input", 20)
        b2.type(".dev-box input", name)
        b2.click(".dev-box button[type=submit]")
        b2.click_el(b2.find_text(".auth-card button", "Approve from another device", 15))
        b2.find(".verify-code", 10)
        code_new = b2.text(".verify-code")
        a.click_el(a.find(".banner .btn", 15))
        a.wait(f"return (document.querySelector('.modal .verify-code')||{{}}).textContent == '{code_new}'", 10, "matching code in approval dialog")
        check(True, "both devices show the same verification code")
        a.click_el(a.find_text(".modal button", "approve"))
        b2.find_text(".card", "My notes", 20)
        b2.click_el(b2.find_text(".card", "My notes"))
        b2.click_el(b2.find_text(".note-card", "Welcome", 15))
        b2.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Getting started')", 15, "device 2 decrypts")
        check(True, "approved device decrypts existing notes")
        a.go(env.url + "settings/security")
        a.find_text(".settings-body button", "Create a new recovery key", 15)
        a.js("window.confirm = () => true")
        a.click_el(a.find_text(".settings-body button", "Create a new recovery key"))
        a.find(".recovery-key", 10)
        rk = a.text(".recovery-key")
        b3.go(env.url)
        b3.find(".dev-box input", 20)
        b3.type(".dev-box input", name)
        b3.click(".dev-box button[type=submit]")
        b3.click_el(b3.find_text(".auth-card button", "Use my recovery key", 15))
        b3.type(".auth-card textarea", rk.lower().replace("-", " "))
        b3.click_el(b3.find_text(".auth-card button", "Unlock with recovery key"))
        b3.find_text(".card", "My notes", 20)
        check(True, "new device recovers with the (rotated) recovery key")
        a.go(env.url + "settings/devices")
        a.find(".settings-body .list-row", 10)
        time.sleep(0.8)
        check(a.js("return document.querySelectorAll('.settings-body .list .list-row').length") == 3, "three devices are listed")
        # "Forget this browser" signs out, removes the device and deletes its key locally.
        b3.go(env.url + "settings/devices")
        forget = b3.find_text(".settings-body .list-row button", "Forget this browser", 15)
        b3.js("window.confirm = () => true")
        b3.click_el(forget)
        b3.find(".dev-box input", 15)
        check(b3.js("return Object.keys(localStorage).filter(k => k.startsWith('notekpr.device.') || k.startsWith('notekpr.pending.') || k.startsWith('notekpr.newkeys.') || k.startsWith('notekpr.head.')).length") == 0,
              "forgetting the browser deletes its device key and sync data")
        b3.type(".dev-box input", name)
        b3.click(".dev-box button[type=submit]")
        b3.find_text(".auth-card button", "Approve from another device", 15)
        check(True, "signing in again needs approval or the recovery key")
        a.go(env.url + "settings/devices")
        a.find(".settings-body .list-row", 10)
        time.sleep(0.8)
        check(a.js("return document.querySelectorAll('.settings-body .list .list-row').length") == 2, "the forgotten browser is removed from the devices")
        check(not severe_logs(a, b2, b3), "no browser errors")
    finally:
        for x in (a, b2, b3):
            x.close()


def test_features(env):
    a, v = Browser(), Browser()
    try:
        an, vn = uniq("eve"), uniq("vic")
        signup(a, an)
        signup(v, vn)
        new_note(a, "Ideas", "Plant a #garden" + KEYS["enter"] + "See [[Plans]]")
        new_note(a, "Plans", "Weekend plans")
        time.sleep(0.6)
        check(a.js("return Array.from(document.querySelectorAll('.panel section h3')).find(h => h.textContent.includes('Backlinks')).querySelector('.count').textContent") == "1", "backlink is detected")
        a.js("const t=document.querySelector('.note-title'); t.focus(); t.select();")
        a.keys("Roadmap" + KEYS["enter"])
        time.sleep(1)
        a.click_el(a.find_text(".tree-item", "Ideas"))
        time.sleep(1)
        check(editor_text(a) == "Plant a #garden\nSee [[Roadmap]]", "renaming a note updates links to it")
        a.click(".seg button[title^='Reading']")
        time.sleep(0.6)
        check(a.js("return !!document.querySelector('.reading a.wl[data-note]') && !!document.querySelector('.reading a.tg')"), "reading view renders wiki links and tags")
        a.click(".seg button[title='Live preview']")
        a.chord(KEYS["ctrl"], "k")
        a.find(".palette input", 5)
        a.keys("road")
        time.sleep(0.4)
        check(a.js("return Array.from(document.querySelectorAll('.palette-item .grow')).map(e=>e.textContent)") == ["Roadmap"], "quick switcher finds notes")
        a.chord(KEYS["escape"])
        a.chord(KEYS["ctrl"], KEYS["shift"], "F")
        a.find(".search-bar input", 5)
        a.keys("tag:garden")
        time.sleep(0.6)
        check(a.js("return document.querySelectorAll('.result').length") == 1, "full-text search filters by tag")
        a.chord(KEYS["escape"])
        share(a, ".sb-bottom button:nth-child(2)", vn, role="viewer")
        time.sleep(2)
        v.click(".vault-btn")
        v.find(".vault-menu .badge[title*='read only']", 15)
        v.js("document.querySelector('.vault-menu .badge[title*=\"read only\"]').closest('button').click()")
        v.click_el(v.find_text(".note-card", "Ideas", 15))
        v.find(".ed .ln", 10)
        time.sleep(0.8)
        check(v.js("return document.querySelector('.ed').getAttribute('contenteditable')") == "false" and not v.js("return !!document.querySelector('.toolbar')"), "viewers get a read-only editor")
        check(not v.js("return !!document.querySelector('.note-icon.add')"), "viewers aren't offered to add an icon")
        v.js("document.querySelector('.ed').focus()")
        v.keys("HACK")
        time.sleep(0.4)
        check("HACK" not in editor_text(v), "viewer keystrokes are ignored")
        a.click_el(a.find_text(".tree-item", "Roadmap"))
        time.sleep(0.5)
        a.click(".note-head .tb-drop .icon-btn")
        a.click_el(a.find_text(".menu button", "Move to trash"))
        time.sleep(0.6)
        check("Roadmap" not in a.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent)"), "trashing removes the note from the tree")
        a.click_el(a.find_text(".sb-link", "Trash"))
        a.click_el(a.find_text(".list-row .btn", "Restore", 10))
        time.sleep(0.6)
        check("Roadmap" in a.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent)"), "restore brings it back")
        a.click_el(a.find_text(".tree-item", "Ideas"))
        time.sleep(0.5)
        a.click_el(a.find_text(".tree-item", "Roadmap"))
        time.sleep(4)
        a.click_el(a.find_text(".tree-item", "Ideas"))
        time.sleep(0.5)
        a.click(".note-head button[title='Version history']")
        time.sleep(1.5)
        check(a.js("return document.querySelectorAll('.h-item').length") >= 1, "a revision was stored after editing")
        a.click(".h-item")
        time.sleep(1)
        check("garden" in (a.js("return (document.querySelector('.h-preview .reading')||{}).textContent") or ""), "revisions decrypt for preview")
        a.chord(KEYS["escape"])
        # An update: the server restarts. The page says "Connecting", not
        # "Offline", and is back within seconds on its own.
        env.stop_server()
        time.sleep(0.5)
        check((a.js("return document.querySelector('.sync .lbl').textContent") or "") == "Connecting", "a restart shows connecting, not offline")
        env.start_server()
        a.wait("return (document.querySelector('.sync .lbl')||{}).textContent == 'Synced'", 8, "back after the restart")
        check(True, "back within seconds after a restart")
        # Offline editing across a crash.
        env.kill_server()
        time.sleep(1)
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        a.keys(KEYS["enter"] + "written while offline")
        time.sleep(1)
        check((a.js("return document.querySelector('.sync .lbl').textContent") or "").startswith("Offline"), "offline state is shown with pending changes")
        env.start_server()
        a.wait("return (document.querySelector('.sync .lbl')||{}).textContent == 'Synced'", 60, "resync")
        v.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('written while offline')", 60, "viewer gets offline edit")
        check(True, "offline edits sync after reconnecting")
        check(not severe_logs(a, v), "no browser errors")
    finally:
        a.close()
        v.close()


def test_mobile(env):
    m = Browser(390, 844)
    try:
        signup(m, uniq("mo"))
        time.sleep(0.6)
        check(not m.js("return getComputedStyle(document.querySelector('.panel') || document.body).position == 'fixed' && !!document.querySelector('.note-body.overlay')"), "side panel does not cover the note on phones")
        m.shot("mobile")
        # The toolbar scrolls sideways; its menus still show whole.
        m.click(".tb[title='Heading']")
        hit = m.js("const b = [...document.querySelectorAll('.tb-drop .menu button')].find(x => x.textContent.includes('Heading 1'));"
                   "if (!b) return false; const r = b.getBoundingClientRect();"
                   "const e = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!e && b.contains(e);")
        check(hit, "the toolbar's menus show whole on a phone")
        m.chord(KEYS["escape"])
        m.click(".note-head .mobile-only")
        time.sleep(0.4)
        check(m.js("return document.querySelector('.app').classList.contains('drawer')"), "sidebar opens as a drawer")
        m.shot("mobile-drawer")
    finally:
        m.close()


def test_attachments(env):
    b = Browser()
    try:
        signup(b, uniq("pia"))
        new_note(b, "Photos", "Look:" + KEYS["enter"])
        # Paste a tiny PNG: the editor uploads it encrypted and embeds it.
        b.js("""
            const png = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=='), c => c.charCodeAt(0));
            const dt = new DataTransfer();
            dt.items.add(new File([png], 'dot.png', {type: 'image/png'}));
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """)
        b.wait("return document.querySelector('.ed').textContent.includes('![[dot.png]]')", 15, "embed inserted")
        check(True, "pasted image is uploaded and embedded")
        b.js("document.querySelector('.note-title').focus()")
        b.wait("return !!document.querySelector('.ed .w img[src^=\"blob:\"]')", 15, "image preview")
        check(True, "embedded image is decrypted and previewed inline")
        check("attachments" in b.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent).join(' ')"), "attachment is filed under attachments/ in the tree")
        # Opening attachments: raster images open in a new tab (noopener); anything that
        # could run as a page of this origin (SVG, HTML) is downloaded instead.
        b.js("""
            window.__opened = []; window.__downloads = [];
            window.open = (u, t, f) => { window.__opened.push([String(u), String(f)]); return null; };
            const click = HTMLAnchorElement.prototype.click;
            HTMLAnchorElement.prototype.click = function () { if (this.download) { window.__downloads.push(this.download); } else { click.call(this); } };
            const ed = document.querySelector('.ed'); ed.focus(); const s = getSelection(); s.selectAllChildren(ed); s.collapseToEnd();
            const dt = new DataTransfer();
            dt.items.add(new File(['<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>'], 'pic.svg', {type: 'image/svg+xml'}));
            ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """)
        b.wait("return document.querySelector('.ed').textContent.includes('[[pic.svg]]')", 15, "svg embed inserted")
        time.sleep(0.5)
        if "dot.png" not in b.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent).join(' ')"):
            b.click_el(b.find_text(".tree-item", "attachments", 10))
        b.click_el(b.find_text(".tree-item", "dot.png", 10))
        b.wait("return window.__opened.length == 1", 15, "png opened")
        opened = b.js("return window.__opened")[0]
        check(opened[0].startswith("blob:") and "noopener" in opened[1], "a raster image opens in a new tab without an opener")
        b.click_el(b.find_text(".tree-item", "pic.svg", 10))
        b.wait("return window.__downloads.includes('pic.svg')", 15, "svg downloaded")
        check(b.js("return window.__opened.length") == 1, "an SVG attachment is downloaded, not opened as a page")
        b.click(".seg button[title^='Reading']")
        b.wait("return !!document.querySelector('.reading img.att[src^=\"blob:\"]')", 15, "reading view image")
        check(True, "reading view shows the decrypted image")
        check(not severe_logs(b), "no browser errors")
        # Signing out revokes the decrypted attachments' blob URLs.
        blob = b.js("return document.querySelector('.reading img.att').src")
        def blob_loads():
            b.js("window.__img = null; const i = new Image(); i.onload = () => window.__img = 'ok'; i.onerror = () => window.__img = 'gone'; i.src = arguments[0];", blob)
            b.wait("return window.__img !== null", 10, "image load attempt")
            return b.js("return window.__img") == "ok"
        check(blob_loads(), "the decrypted image is a readable blob while signed in")
        b.js("history.pushState(null, '', '/settings/account'); dispatchEvent(new PopStateEvent('popstate'));")
        b.click_el(b.find_text(".settings-body button", "Export My notes", 10))
        b.wait("return window.__downloads.includes('My notes.zip')", 15, "vault export downloaded")
        check(True, "account settings export a vault as a .zip")
        b.click_el(b.find_text(".settings-body button", "Sign out", 10))
        b.find(".dev-box input", 15)
        check(not blob_loads(), "signing out revokes decrypted attachment URLs")
    finally:
        b.close()


API_GET = """
window.__api = undefined;
fetch('/api/v1' + arguments[0], {headers: {Authorization: 'Bearer ' + sessionStorage.getItem('notekpr.token')}})
  .then(r => r.ok ? r.json() : null).then(j => { window.__api = j; }, () => { window.__api = null; });
"""


API_DELETE = """
window.__api = undefined;
fetch('/api/v1' + arguments[0], {method: 'DELETE', headers: {Authorization: 'Bearer ' + sessionStorage.getItem('notekpr.token')}})
  .then(r => { window.__api = r.status; }, () => { window.__api = 0; });
"""


def api_delete(b, path):
    """DELETE an API path with the browser's session; the status (0 on error)."""
    b.js(API_DELETE, path)
    b.wait("return window.__api !== undefined", 10, "API answer")
    return b.js("return window.__api")


def cdp(b, cmd, params=None):
    """A Chrome DevTools command, through ChromeDriver."""
    return wd.req("POST", b.p("/goog/cdp/execute"), {"cmd": cmd, "params": params or {}})


# Keeps the page's WebSockets in window.__sockets (installed before it loads),
# so a test can drop them as a lost connection would.
TRACK_SOCKETS = """
(() => {
  const W = window.WebSocket;
  window.__sockets = [];
  window.WebSocket = class extends W {
    constructor(...a) { super(...a); window.__sockets.push(this); }
  };
})();
"""


# Like TRACK_SOCKETS, and new WebSockets fail while window.__wsBlocked is set
# (as while a server restarts), without cutting the page's other requests.
BLOCKABLE_SOCKETS = """
(() => {
  const W = window.WebSocket;
  window.__sockets = [];
  window.__wsBlocked = false;
  window.WebSocket = class extends W {
    constructor(...a) {
      if (window.__wsBlocked) throw new DOMException('blocked by the test', 'NetworkError');
      super(...a);
      window.__sockets.push(this);
    }
  };
})();
"""


def set_offline(b, offline):
    """Cut this browser off the network (ChromeDriver's network conditions), or reconnect it."""
    if offline:
        wd.req("POST", b.p("/chromium/network_conditions"),
               {"network_conditions": {"offline": True, "latency": 0, "download_throughput": 0, "upload_throughput": 0}})
    else:
        wd.req("DELETE", b.p("/chromium/network_conditions"))


def api_get(b, path):
    """GET an API path with the browser's session (parsed JSON, None on error)."""
    b.js(API_GET, path)
    b.wait("return window.__api !== undefined", 10, "API answer")
    return b.js("return window.__api")


DOC_EPOCH_1 = 0x8000_0001


def payload_epoch(b, payload):
    """Key epoch of a base64 ciphertext (bytes 1..5, big endian)."""
    return b.js("const s=atob(arguments[0]); return ((s.charCodeAt(1)<<24)>>>0) + (s.charCodeAt(2)<<16) + (s.charCodeAt(3)<<8) + s.charCodeAt(4);", payload)


def test_note_sharing(env):
    a, b = Browser(), Browser()
    end = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
    start = "const ed=document.querySelector('.ed'); ed.focus(); const r=document.createRange(); r.setStart(ed.firstChild.firstChild || ed.firstChild, 0); r.collapse(true); const s=getSelection(); s.removeAllRanges(); s.addRange(r);"
    try:
        an, bn = uniq("olga"), uniq("pete")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Secret plans", "only for me")
        new_note(a, "Trip", "Pack sunscreen")
        path = a.js("return location.pathname")
        vid, nid = path.split("/")[2], path.split("/")[3]
        # The note has its own key from the start: none of it under the vault key.
        a.wait("return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.') || k.startsWith('notekpr.newkeys.')).length === 0", 15, "olga's edits are sent")
        keys = None
        for _ in range(50):
            keys = api_get(a, f"/vaults/{vid}/keys")
            if keys and any(o["object_id"] == nid for o in keys["objects"]):
                break
            time.sleep(0.2)
        check(any(o["object_id"] == nid and o["epoch"] == DOC_EPOCH_1 for o in keys["objects"]) and keys["shared"] == [],
              "a new note gets its own private key")
        ch = api_get(a, f"/vaults/{vid}/objects/{nid}/changes?since=0")
        check(ch["updates"] and all(payload_epoch(a, u["payload"]) >= DOC_EPOCH_1 for u in ch["updates"]),
              "none of its content is under the vault key")
        # Share just this note with Pete: from the header, the note's "More"
        # menu or its "…" in the tree.
        a.click(".note-head button[title='Share this note']")
        a.find_text(".modal h2", "Trip", 10)
        a.chord(KEYS["escape"])
        a.click(".note-head button[title='More']")
        a.click_el(a.find_text(".note-head .menu button", "Share", 5))
        a.find_text(".modal h2", "Trip", 10)
        a.chord(KEYS["escape"])
        row = "Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === 'Trip')"
        a.js(row + ".querySelector('.tree-more').click()")
        a.click_el(a.find_text(".tree-row .menu button", "Share", 5))
        a.find_text(".modal h2", "Trip", 10)
        check(True, "the header, the note's More menu and its menu in the tree all open sharing")
        a.type(".share-add input", bn)
        a.find(".found .person", 10)
        a.click(".found .person .btn.primary")
        a.find_text(".people .person", bn.capitalize(), 15)
        check(True, "owner shares a single note")
        a.shot("note-share-dialog")
        a.chord(KEYS["escape"])
        check(a.js("return document.querySelectorAll('.tree-row .shared-mark').length") == 1, "the tree marks the shared note")
        # Pete is told right away and finds it in the sidebar, even while working in his own vault.
        b.find_text(".toast", "shared “Trip” with you", 20)
        check(True, "the recipient is notified")
        b.click_el(b.find_text(".sb-link", "Trip", 10))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Pack sunscreen')", 20, "pete reads the note")
        check(b.js("return document.querySelector('.note-title').textContent") == "Trip", "the title is decrypted for the recipient")
        check(b.js("return location.pathname").startswith("/s/"), "the note opens as a shared note")
        ch = api_get(b, f"/vaults/{vid}/objects/{nid}/changes?since=0&snapshots=true")
        got = [x["payload"] for x in ch["updates"] + ch["snapshots"]]
        check(got and all(payload_epoch(b, p) > DOC_EPOCH_1 for p in got),
              "sharing started a fresh key: nothing from before reaches the recipient")
        check(api_get(a, f"/vaults/{vid}/keys")["shared"] == [nid], "the server lists the note as shared")
        b.click(".sb-search")
        b.find(".palette input", 5)
        b.keys("secret")
        time.sleep(0.4)
        check(b.js("return document.querySelectorAll('.palette-item').length") == 0, "the rest of the vault is not reachable")
        b.chord(KEYS["escape"])
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "All vaults"))
        b.find_text(".card", "Trip", 10)
        check(True, "the home page lists it too")
        b.click_el(b.find_text(".sb-link", "Trip", 10))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Pack sunscreen')", 20, "pete reopens the note")
        # Live editing in both directions.
        b.js(end)
        b.keys(KEYS["enter"] + "and hats")
        a.wait("return document.querySelector('.ed').textContent.includes('and hats')", 10, "olga sees pete's edit")
        a.js(start)
        a.keys("Beach: ")
        b.wait("return document.querySelector('.ed').textContent.includes('Beach: ')", 10, "pete sees olga's edit")
        time.sleep(1)
        check(editor_text(a) == editor_text(b), "both editors converge")
        a.wait("return document.querySelectorAll('.presence .avatar').length >= 1", 10, "presence")
        b.wait("return document.querySelectorAll('.presence .avatar').length >= 1", 10, "presence")
        check(True, "both see each other on the note")
        b.shot("note-sharing-recipient")
        # Renaming updates the recipient's title (encrypted note metadata).
        a.js("const t=document.querySelector('.note-title'); t.focus(); t.select();")
        a.keys("Beach trip" + KEYS["enter"])
        b.wait("return document.querySelector('.note-title').textContent == 'Beach trip'", 10, "renamed title reaches pete")
        check(True, "renaming updates the recipient's title")
        # So does its icon, set from the note's menu.
        a.click(".note-head button[title='More']")
        a.click_el(a.find_text(".note-head .menu button", "Set icon", 5))
        a.find(".modal .icon-pick", 5)
        a.js("document.querySelector('.modal .emoji-btn[aria-label=\"🌍\"]').click()")
        b.wait("return (document.querySelector('.note-title-row .note-icon') || {}).textContent === '🌍'"
               " && Array.from(document.querySelectorAll('.sb-link')).some(e => e.textContent.includes('Beach trip') && (e.querySelector('.entry-icon') || {}).textContent === '🌍')",
               10, "the icon reaches pete")
        check(True, "and its icon, shown by the title and in Shared with me")
        # A reload restores the shared note.
        b.go(env.url.rstrip("/") + b.js("return location.pathname"))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('and hats')", 20, "pete reloads")
        check(True, "the shared note reopens after a reload")
        time.sleep(3)
        copy = local_copy(b)
        check(any("~shares" in k and nid in k for k in copy["keys"]), "the recipient's browser keeps a copy of the shared note")
        check("Pack sunscreen" not in copy["text"] and "and hats" not in copy["text"], "the copy is encrypted")
        restored = [l.get("message", "") for l in b.logs() or [] if "shared note" in l.get("message", "") and "failed" in l.get("message", "")]
        check(not restored, "the copy is read and written without errors")
        # Revoking removes access right away and renews the note's key.
        a.click(".note-head button[title^='Shared individually']")
        a.find(".people .person .icon-btn", 10)
        a.js("window.confirm = () => true")
        a.click(".people .person .icon-btn")
        b.wait("return !location.pathname.startsWith('/s/')", 15, "pete loses access")
        check(b.js("return !Array.from(document.querySelectorAll('.card, .sb-link')).some(e => e.textContent.includes('Beach trip'))"),
              "revoking removes the note from the recipient's lists")
        b.find_text(".toast", "no longer shared", 5)
        check(True, "the recipient is told")
        time.sleep(1)
        check(not any("~shares" in k and nid in k for k in local_copy(b)["keys"]), "and its copy is deleted")
        a.find_text(".people p", "Not shared", 10)
        a.chord(KEYS["escape"])
        time.sleep(0.5)
        a.js(end)
        a.keys(KEYS["enter"] + "after revoke")
        time.sleep(2)
        a.go(env.url.rstrip("/") + path)
        a.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('after revoke')", 20, "olga's later edit")
        check(True, "the owner keeps editing with the renewed key")
        a.shot("note-sharing-owner")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_tables(env):
    b = Browser()
    try:
        signup(b, uniq("tia"))
        new_note(b, "Budget", "")
        # Typing a table: Enter after the delimiter row starts a row, Tab moves between cells.
        b.keys("| Item | Cost |" + KEYS["enter"] + "| :--- | ---: |" + KEYS["enter"])
        b.keys("Bread")
        b.chord(KEYS["tab"])
        b.keys("3")
        b.chord(KEYS["tab"])
        b.keys("Milk")
        b.chord(KEYS["tab"])
        b.keys("2")
        time.sleep(0.6)
        check(editor_text(b) == "| Item | Cost |\n| :--- | ---: |\n| Bread | 3 |\n| Milk | 2 |", "Enter and Tab build table rows")
        b.shot("tables-editing")
        cell = "Array.from(document.querySelectorAll('.ed .ln.table'))"
        check(b.js(f"return {cell}.every(r => getComputedStyle(r).display === 'table-row' || r.classList.contains('tdl'))"), "rows are laid out as a table")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.tdl')).display") == "none", "the delimiter row is hidden")
        lefts = b.js(f"return {cell}.filter(r => !r.classList.contains('tdl')).map(r => Math.round(r.querySelectorAll('.td')[1].getBoundingClientRect().left))")
        check(len(set(lefts)) == 1, f"columns line up ({lefts})")
        check(b.js("return getComputedStyle(document.querySelectorAll('.ed .ln.table .td')[1]).textAlign") == "right", "column alignment from the delimiter row")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.th .td')).fontWeight") >= "600", "header row is bold")
        b.chord(KEYS["enter"])
        b.chord(KEYS["enter"])
        b.keys("Total: 5")
        time.sleep(0.6)
        check(editor_text(b).endswith("| Milk | 2 |\n\nTotal: 5"), "Enter on an empty row ends the table")
        b.shot("tables")
        check(not severe_logs(b), "no browser errors")
        tables_on_a_phone()
    finally:
        b.close()


def tables_on_a_phone():
    """A table wider than a phone keeps readable columns and scrolls
    sideways; the text around it still wraps at the screen's width."""
    m = Browser(390, 844)
    try:
        signup(m, uniq("tip"))
        m.click(".note-head .mobile-only")
        time.sleep(0.4)
        new_note(m, "Trip", "Who does what on the trip, and how to reach them before we leave." + KEYS["enter"])
        m.keys("| Name | Role | Email | Notes |" + KEYS["enter"] + "| --- | --- | --- | --- |" + KEYS["enter"])
        for i, cell in enumerate(["Alice Johnson", "Owner", "alice@example.com", "Plans the trip and books the hotels in advance",
                                  "Bob", "Editor", "bob@example.org", "Drives"]):
            if i:
                m.chord(KEYS["tab"])
            m.keys(cell)
        time.sleep(0.6)
        wrap = "document.querySelector('.ed-wrap')"
        caret_shows = f"""const c = getSelection().getRangeAt(0).getBoundingClientRect(), w = {wrap}.getBoundingClientRect();
            return c.height > 0 && c.left >= w.left && c.right <= w.right;"""
        check(m.js(f"return {wrap}.scrollLeft > 0") and m.js(caret_shows), "the text scrolls sideways to the caret in the table's last column")
        m.chord(KEYS["enter"])
        m.chord(KEYS["enter"])
        m.keys("See you there.")
        time.sleep(0.6)
        check(editor_text(m).endswith("| Bob | Editor | bob@example.org | Drives |\n\nSee you there."), "typed a wide table on a phone")
        check(m.js(caret_shows), "and back to the caret after the table")
        m.js(f"{wrap}.scrollLeft = 0")
        sizes = m.js(f"""const w = {wrap}, ln = [...document.querySelectorAll('.ed > .ln')];
            const rows = el => {{ const r = document.createRange(); r.selectNodeContents(el);
                return new Set([...r.getClientRects()].filter(x => x.width > 0).map(x => Math.round(x.top))).size; }};
            const tc = [...document.querySelectorAll('.ed .ln.table:not(.tdl) .tc')];
            const text = el => el.textContent.replace(/\\|/g, '').trim();
            return {{ scroll: w.scrollWidth, width: w.clientWidth, page: document.querySelector('.main').scrollWidth <= innerWidth,
                      para: ln[0].getBoundingClientRect().right <= w.getBoundingClientRect().right && rows(ln[0]) > 1,
                      words: tc.filter(c => !text(c).includes(' ')).map(c => rows(c)),
                      long: tc.filter(c => text(c).startsWith('Plans')).map(c => [rows(c), Math.round(c.getBoundingClientRect().width)])[0] }};""")
        check(sizes["scroll"] > sizes["width"], f"a table wider than the screen makes the text scroll sideways ({sizes['scroll']} > {sizes['width']})")
        check(sizes["page"], "the page itself doesn't scroll sideways")
        check(sizes["para"], "the line before the table still wraps at the screen's width")
        check(set(sizes["words"]) == {1}, f"no word in a cell is broken ({sizes['words']})")
        rows, width = sizes["long"]
        check(rows > 1 and width <= 12 * 17, f"a long cell wraps at a readable width ({rows} rows, {width}px)")
        m.shot("tables-phone")
        m.click(".seg button[title^='Reading']")
        m.find(".reading table", 10)
        reading = m.js("""const t = document.querySelector('.reading table');
            const rows = el => { const r = document.createRange(); r.selectNodeContents(el);
                return new Set([...r.getClientRects()].filter(x => x.width > 0).map(x => Math.round(x.top))).size; };
            return [t.scrollWidth > t.clientWidth, [...t.querySelectorAll('.tc')].filter(c => !c.textContent.includes(' ')).map(rows),
                    document.querySelector('.main').scrollWidth <= innerWidth];""")
        check(reading[0] and set(reading[1]) == {1} and reading[2], f"the reading view's table scrolls on its own, words whole ({reading})")
        m.shot("tables-phone-reading")
        check(not severe_logs(m), "no browser errors on the phone")
    finally:
        m.close()


def test_code_blocks(env):
    b = Browser()
    try:
        signup(b, uniq("cba"))
        new_note(b, "Scripts", "")
        # The toolbar inserts a block with the caret on the fence, for the language and options.
        b.click_el(b.find(".tb[title^='Code block']", 10))
        b.keys("bash title=deploy.sh hl=2")
        b.chord(KEYS["down"])
        b.keys('echo "hi"' + KEYS["enter"] + "curl -s $URL" + KEYS["enter"] + "  sleep 1")
        time.sleep(0.6)
        check(editor_text(b).startswith('```bash title=deploy.sh hl=2\necho "hi"\ncurl -s $URL\n  sleep 1\n```'), "the toolbar inserts a code block")
        check(b.text(".ed .ln.fo .code-title") == "deploy.sh", "the header shows the file name")
        check(b.text(".ed .ln.fo .code-lang") == "bash", "the header shows the language")
        check(b.js("return document.querySelectorAll('.ed .ln.code.chl').length") == 1
              and "echo" not in b.js("return document.querySelector('.ed .ln.code.chl').textContent"), "hl=2 highlights the second line")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.code'), '::before').content") != "none", "code lines are numbered")
        check(b.js("return Array.from(document.querySelectorAll('.ed .ln.code .fn')).map(e => e.textContent).join(' ')") == "echo curl sleep", "commands are highlighted")
        check(b.js("return Array.from(document.querySelectorAll('.ed .igs')).map(e => e.className).join()") == "w igs ig1 iu2", "the indented line has a guide")
        check(b.js("return getComputedStyle(document.querySelector('.ed .igs')).position") == "absolute", "the guide is drawn beside the text")
        check(b.js("return document.querySelector('.ed .ln.code .va').textContent") == "$URL", "variables are highlighted")
        check(b.js("return document.querySelector('.ed .ln.fo').classList.contains('active') && document.querySelector('.ed .ln.fc').classList.contains('active')"),
              "with the caret in the block both fences show")
        b.shot("code-blocks-editing")
        # Away from the block only the header shows.
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.keys(KEYS["enter"] + "after")
        time.sleep(0.4)
        check(not b.js("return document.querySelector('.ed .ln.fo').classList.contains('active')"), "away from the block its fences hide")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.fo .mk')).fontSize") == "0px", "the fence's source is hidden")
        b.shot("code-blocks")
        # Copy buttons copy the code only, in the editor and the reading view.
        b.js("window.__copied = []; navigator.clipboard.writeText = t => { window.__copied.push(t); return Promise.resolve(); };")
        before = editor_text(b)
        b.click_el(b.find(".ed .code-copy", 5))
        b.wait("return window.__copied.length == 1", 5, "editor copy")
        check(b.js("return window.__copied[0]") == 'echo "hi"\ncurl -s $URL\n  sleep 1', "the editor's copy button copies the code")
        check(b.js("return document.querySelector('.ed .code-copy').classList.contains('copied')"), "the button shows it copied")
        check(editor_text(b) == before, "copying leaves the text alone")
        b.click(".seg button[title^='Reading']")
        b.find(".reading .codeblock", 10)
        check(b.text(".reading .code-title") == "deploy.sh", "the reading view shows the file name")
        check(b.js("return document.querySelectorAll('.reading .cl').length") == 3
              and b.js("return document.querySelectorAll('.reading .cl.chl').length") == 1, "the reading view numbers and highlights lines")
        check(b.js("return document.querySelectorAll('.reading .cl .igs').length") == 1, "the reading view draws indentation guides")
        b.click_el(b.find(".reading .code-copy", 5))
        b.wait("return window.__copied.length == 2", 5, "reading copy")
        check(b.js("return window.__copied[1]") == 'echo "hi"\ncurl -s $URL\n  sleep 1', "the reading view's copy button copies the code")
        b.shot("code-blocks-reading")
        # Line numbers can be turned off.
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.click_el(b.find(".code-numbers-toggle", 10))
        b.wait("return document.documentElement.classList.contains('no-code-numbers')", 5, "numbers off")
        check(True, "the setting turns line numbers off")
        b.click_el(b.find(".code-numbers-toggle", 10))
        b.click_el(b.find(".code-guides-toggle", 10))
        b.wait("return document.documentElement.classList.contains('no-code-guides')", 5, "guides off")
        b.js("history.back()")
        b.find(".reading .cl .igs", 10)
        check(b.js("return getComputedStyle(document.querySelector('.reading .cl .igs')).display") == "none", "the setting turns indentation guides off")
        b.js("history.forward()")
        b.click_el(b.find(".code-guides-toggle", 10))
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_code_wrapping(env):
    b = Browser(1100, 900)
    try:
        signup(b, uniq("cwr"))
        new_note(b, "Long lines", "")
        long = "y = " + " + ".join(f"value{i}" for i in range(36))
        # Enter after the opening fence closes the block.
        b.keys("```py" + KEYS["enter"])
        time.sleep(0.3)
        check(editor_text(b) == "```py\n\n```", "Enter after an opening fence closes the block")
        b.keys("if x:" + KEYS["enter"] + "    " + long)
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.keys(KEYS["enter"] + "after")
        time.sleep(0.6)
        check(editor_text(b) == f"```py\nif x:\n    {long}\n```\nafter", "typed a block with a long line")
        rows = """
            const lc = document.querySelectorAll('.ed .ln.code')[1].querySelector('.lc');
            const r = document.createRange(); r.selectNodeContents(lc);
            // The leftmost piece of each row.
            const rows = new Map();
            for (const x of r.getClientRects()) {
                const t = Math.round(x.top);
                rows.set(t, Math.min(rows.get(t) ?? Infinity, Math.round(x.left)));
            }
            const lefts = [...new Set(rows.values())];
            const tops = rows;
            const guide = lc.parentElement.querySelector('.igs').getBoundingClientRect();
            return [tops.size, lefts, Math.round(guide.right), getComputedStyle(lc).whiteSpace];
        """
        n, lefts, guide_right, ws = b.js(rows)
        check(n > 1, f"a long line wraps by default ({n} rows)")
        check(len(lefts) == 1, f"wrapped rows start under the text, not under the indentation ({lefts})")
        check(guide_right <= lefts[0], "the indentation guide stays clear of the text")
        b.shot("code-wrapping")
        # wrap=false: the block scrolls sideways, its lines together.
        # Ctrl+Home reaches the fence even while it is hidden behind the header.
        b.chord(KEYS["ctrl"], KEYS["home"])
        b.chord(KEYS["end"])
        b.keys(" wrap=false")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.6)
        check(editor_text(b).startswith("```py wrap=false\n"), "Ctrl+Home goes to the fence: wrap=false after the language")
        n, lefts, guide_right, ws = b.js(rows)
        check(n == 1, f"wrap=false: the long line doesn't wrap ({n} rows)")
        lx = "Array.from(document.querySelectorAll('.ed .ln.code > .lx'))"
        widths = b.js(f"return {lx}.map(x => [x.scrollWidth, x.clientWidth])")
        check(widths[1][0] > widths[1][1], "the long line overflows")
        check(widths[0][0] == widths[1][0], f"every line of the block is as wide as its longest ({widths})")
        check(b.js(f"return {lx}.map(x => getComputedStyle(x).scrollbarWidth).join()") == "none,thin", "only the block's last line shows a scrollbar")
        b.js(f"{lx}[1].scrollLeft = 120")
        b.wait(f"return {lx}[0].scrollLeft == 120", 5, "the other line scrolls along")
        check(True, "scrolling one line scrolls the block")
        # Typing at the end of the long line: the block follows the caret.
        b.click_el(b.find(".ed > .ln:nth-child(3) .lc", 5))
        b.chord(KEYS["end"])
        b.keys(" + end")
        time.sleep(0.6)
        check(editor_text(b).splitlines()[2].endswith("value35 + end"), "typed at the end of the long line")
        offsets = b.js(f"return {lx}.map(x => x.scrollLeft)")
        check(offsets[1] > 120 and len(set(offsets)) == 1, f"the block scrolls to the caret, its lines together ({offsets})")
        b.shot("code-nowrap-editing")
        b.click(".seg button[title^='Reading']")
        b.find(".reading .codeblock.nowrap", 10)
        check(b.js("const p = document.querySelector('.reading pre.code'); return p.scrollWidth > p.clientWidth"), "the reading view scrolls an unwrapped block sideways")
        # The setting: blocks without wrap= follow it.
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.click_el(b.find(".code-wrap-toggle", 10))
        b.wait("return document.documentElement.classList.contains('no-code-wrap')", 5, "wrapping off")
        check(True, "the setting turns wrapping off")
        b.click_el(b.find(".code-wrap-toggle", 10))
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


# Viewport coordinates of the start and end of `text` in the editor.
TEXT_ENDS = """
const w = document.createTreeWalker(document.querySelector('.ed'), NodeFilter.SHOW_TEXT);
let n;
while ((n = w.nextNode())) {
  const i = n.data.indexOf(arguments[0]);
  if (i >= 0) {
    const r = document.createRange();
    r.setStart(n, i); r.setEnd(n, i + arguments[0].length);
    const rs = r.getClientRects(), a = rs[0], b = rs[rs.length - 1];
    return [a.left + 1, a.top + a.height / 2, b.right - 1, b.top + b.height / 2];
  }
}
return null;
"""


def test_editor_mouse(env):
    b = Browser()
    try:
        signup(b, uniq("ems"))
        new_note(b, "Mouse", "- first item" + KEYS["enter"] + "second item" + KEYS["enter"] + KEYS["enter"] + "plain text")
        time.sleep(0.5)
        check(editor_text(b) == "- first item\n- second item\n\nplain text", "typed a list and a paragraph")
        # The caret is in the paragraph; the list's markers are hidden. A drag
        # over an item selects it, although pressing the mouse there would
        # show the item's marker and move its text.
        x1, y1, x2, y2 = b.js(TEXT_ENDS, "second item")
        wd.req("POST", b.p("/actions"), {"actions": [{"type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
            "actions": [{"type": "pointerMove", "origin": "viewport", "x": int(x1), "y": int(y1)},
                        {"type": "pointerDown", "button": 0}, {"type": "pause", "duration": 100},
                        {"type": "pointerMove", "origin": "viewport", "x": int(x2), "y": int(y2), "duration": 400},
                        {"type": "pointerUp", "button": 0}]}]})
        time.sleep(0.5)
        check(b.js("return getSelection().toString()") == "second item", f"a drag selects the text it goes over ({b.js('return getSelection().toString()')!r})")
        check(b.js("return document.querySelectorAll('.ed .ln')[1].classList.contains('active')"), "after the drag the line shows its markers")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_editor_safety(env):
    b = Browser()
    try:
        signup(b, uniq("sam"))
        new_note(b, "Links", "[evil](javascript:window.__pwned=1) [ok](https://example.org/)")
        # Links in the live editor: only web and e-mail links may be opened.
        b.js("window.__opened = []; window.open = (u) => { window.__opened.push(String(u)); return null; };")
        click = "document.querySelector(`.ed [data-link=\"${arguments[0]}\"]`).dispatchEvent(new MouseEvent('mousedown', {bubbles: true, cancelable: true, ctrlKey: true}));"
        b.js(click, 0)
        b.find_text(".toast", "Only web and e-mail links", 5)
        b.js(click, 1)
        time.sleep(0.3)
        check(b.js("return window.__opened") == ["https://example.org/"] and not b.js("return !!window.__pwned"),
              "the editor opens web links but not javascript: links")
        # A paste larger than the server accepts in one change is refused; syncing goes on.
        end = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
        b.js(end)
        b.js("const dt = new DataTransfer(); dt.setData('text/plain', 'x'.repeat(2200000)); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));")
        b.find_text(".toast", "too large to add at once", 15)
        check(len(editor_text(b)) < 1000, "a paste over the server's update limit is refused")
        b.js(end)
        b.keys(" after")
        b.wait("return (document.querySelector('.sync .lbl')||{}).textContent == 'Synced'", 20, "synced")
        path = b.js("return location.pathname")
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('after')", 20, "edit after the refused paste")
        check(True, "later edits still sync")
        # A file still uploading when you switch notes is not linked into the other note.
        new_note(b, "Other", "other body")
        b.click_el(b.find_text(".tree-item", "Links", 10))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('after')", 10, "back in Links")
        b.js("""
            const f = window.fetch;
            window.fetch = (r, o) => String(r.url || r).includes('/attachments/')
                ? new Promise(res => setTimeout(res, 2000)).then(() => f(r, o)) : f(r, o);
            const dt = new DataTransfer();
            dt.items.add(new File(['slow'], 'late.txt', {type: 'text/plain'}));
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """)
        b.click_el(b.find_text(".tree-item", "Other", 10))
        b.find_text(".toast", "The note was closed meanwhile", 15)
        check("late.txt" not in editor_text(b) and "other body" in editor_text(b), "a finished upload is not linked into the note opened meanwhile")
        # A native browser edit (reconciled from the DOM) keeps the note's no-break spaces.
        b.js(end)
        b.js("const dt = new DataTransfer(); dt.setData('text/plain', ' 10\\u00a0km'); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));")
        b.wait("return document.querySelector('.ed').textContent.includes('10\\u00a0km')", 5, "no-break space pasted")
        b.js("""
            const ed = document.querySelector('.ed');
            const n = document.createTreeWalker(ed.firstElementChild, NodeFilter.SHOW_TEXT).nextNode();
            n.data = 'X\\u00a0' + n.data;
            ed.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText', data: 'X'}));
        """)
        time.sleep(0.5)
        t = editor_text(b)
        check(t.startswith("X other") and "10 km" in t, "native edits keep existing no-break spaces and plain-space new ones")
        # Nodes the browser adds to the editor itself become lines of ours.
        b.js("""
            const ed = document.querySelector('.ed');
            const d = document.createElement('div'); d.innerHTML = '<span style="color:red">stray</span>'; ed.appendChild(d);
            ed.appendChild(document.createTextNode('loose'));
            ed.dispatchEvent(new InputEvent('input', {bubbles: true, inputType: 'insertText'}));
        """)
        time.sleep(0.5)
        check(editor_text(b).endswith("\nstray\nloose")
              and b.js("return Array.from(document.querySelector('.ed').childNodes).every(n => n.nodeType === 1 && n.classList.contains('ln') && !n.querySelector('[style]'))"),
              "after native edits the editor holds only its own lines")
        # Ticking a task in reading view changes its own box, not a later "[ ]".
        new_note(b, "Tasks", "")
        b.js("const dt = new DataTransfer(); dt.setData('text/plain', '- [x] buy [ ] boxes\\n> - [ ] quoted [x]'); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));")
        b.click(".seg button[title^='Reading']")
        b.find(".reading input.task", 10)
        b.js("document.querySelectorAll('.reading input.task')[0].click()")
        time.sleep(0.4)
        b.js("document.querySelectorAll('.reading input.task')[1].click()")
        time.sleep(0.4)
        b.click(".seg button[title='Live preview']")
        time.sleep(0.4)
        check(editor_text(b) == "- [ ] buy [ ] boxes\n> - [x] quoted [x]", "reading view toggles the task's own box")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


IDB_READ = """
window.__idb = null;
const r = indexedDB.open('notekpr');
r.onsuccess = () => {
  const db = r.result;
  const st = db.transaction('cache').objectStore('cache');
  const k = st.getAllKeys(), v = st.getAll();
  v.onsuccess = () => {
    window.__idb = {keys: k.result, text: v.result.map(x => new TextDecoder('latin1').decode(x)).join('')};
    db.close();
  };
};
"""


def local_copy(b):
    """Keys and (latin1) contents of the browser's IndexedDB vault copies."""
    b.js(IDB_READ)
    b.wait("return window.__idb !== null", 10, "reading IndexedDB")
    return b.js("return window.__idb")


def test_local_copy(env):
    a, b = Browser(), Browser()
    try:
        an, bn = uniq("cora"), uniq("dan")
        signup(a, an)
        signup(b, bn)
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Trip")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Trip", 15)
        a.click_el(a.find_text(".qa", "New note"))
        a.find(".note-title", 10)
        time.sleep(0.5)
        a.keys("Packing" + KEYS["enter"])
        time.sleep(0.5)
        a.keys("tent and stove")
        time.sleep(0.8)
        url = a.js("return location.pathname")
        vault = url.split("/")[2]
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('tent and stove')", 20, "dan sees the note")
        time.sleep(3)
        copy = local_copy(b)
        check(any(vault in k for k in copy["keys"]), "the browser keeps a local copy of the vault")
        check("tent and stove" not in copy["text"] and "Packing" not in copy["text"], "the local copy is encrypted")
        b.js("window.__locks = null; navigator.locks.query().then(q => window.__locks = q.held.map(l => l.name))")
        b.wait("return window.__locks !== null", 10, "querying locks")
        check(sum(n.startswith("notekpr-cache-") for n in b.js("return window.__locks")) == 1, "one tab holds the right to write the copy")
        # Away while the note changes, then back: the copy is shown and catches up.
        b.go("about:blank")
        start = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
        a.js(start)
        a.keys(" and a map")
        time.sleep(1)
        b.logs()
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('and a map')", 20, "dan gets the edit made while away")
        restored = [l.get("message", "") for l in b.logs() or [] if "from this browser's copy" in l.get("message", "")]
        check(any(vault in m and "catching up from 0" not in m for m in restored), "the vault opens from the local copy and catches up from where it was")
        check(True, "changes made while away arrive")
        b.js(start)
        b.keys(" and boots")
        a.wait("return document.querySelector('.ed').textContent.includes('and boots')", 10, "cora gets dan's edit")
        check(True, "editing after opening from the local copy syncs")
        # Losing access deletes the copy.
        a.click(".sb-bottom button:nth-child(2)")
        a.find_text(".people .person", bn.capitalize(), 10)
        a.js("window.confirm = () => true")
        a.click(".people .person .icon-btn[title^='Remove access']")
        b.wait("return !location.pathname.includes('%s')" % vault, 15, "dan is sent away from the vault")
        time.sleep(1)
        check(not any(vault in k for k in local_copy(b)["keys"]), "losing access deletes the local copy")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_ui_basics(env):
    b = Browser()
    try:
        signup(b, uniq("uma"))
        b.click(".vault-btn")
        b.find(".vault-menu", 5)
        b.click(".note-title")
        time.sleep(0.3)
        check(not b.js("return !!document.querySelector('.vault-menu')"), "a menu closes on a click outside it")
        b.click(".user-btn")
        b.find(".menu.up", 5)
        b.chord(KEYS["escape"])
        time.sleep(0.3)
        check(not b.js("return !!document.querySelector('.menu.up')"), "Escape closes an open menu")
        # Help from inside the editor, a modal dialog that keeps the focus.
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.chord(KEYS["ctrl"], "/")
        b.find(".modal[aria-modal=true]", 5)
        check(True, "Ctrl+/ opens the shortcut help from the editor")
        for _ in range(12):
            b.keys(KEYS["tab"])
        check(b.js("return document.querySelector('.modal').contains(document.activeElement)"), "Tab stays inside the dialog")
        b.chord(KEYS["escape"])
        time.sleep(0.3)
        check(b.js("return !!document.activeElement.closest('.ed')"), "closing the dialog gives the focus back")
        # [[Note#Heading]] opens the note at that heading.
        paste = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();" \
                "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));"
        new_note(b, "Plan", "")
        b.js(paste, "intro\n" + "filler\n" * 120 + "## Next steps\nship it")
        time.sleep(0.8)
        new_note(b, "Links", "")
        b.js(paste, "see [[Plan#Next steps]]")
        time.sleep(0.8)
        b.chord(KEYS["ctrl"], "e")
        b.click("article.reading a.wl[data-heading]")
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Plan'", 10, "Plan opens")
        b.wait("const h = Array.from(document.querySelectorAll('article.reading h2')).find(h => h.textContent.includes('Next steps'));"
               "return h && h.getBoundingClientRect().top >= 0 && h.getBoundingClientRect().top < innerHeight", 10, "the heading in view")
        check(True, "a link to a heading opens the note at that heading")
        b.chord(KEYS["ctrl"], "e")
        # However far down a long note, its header and toolbar stay at the top.
        b.js("const m = document.querySelector('.main'); m.scrollTop = m.scrollHeight;")
        time.sleep(0.3)
        check(b.js("return document.querySelector('.main').scrollTop > innerHeight"
                   " && document.querySelector('.note-head').getBoundingClientRect().top === 0"
                   " && document.querySelector('.toolbar-wrap').getBoundingClientRect().top > 0"),
              "the header and toolbar stay at the end of a long note")
        # A favorite's star reads as on at a glance: filled, with a tooltip to undo it.
        star = "return (x => [x.getAttribute('aria-pressed'), x.title, getComputedStyle(x.querySelector('svg')).fill])" \
               "(document.querySelector(\".note-head button[aria-label='Favorite']\"))"
        pressed, title, fill = b.js(star)
        check(pressed == "false" and title == "Add to favorites" and fill == "none", "a note that isn't a favorite has an outlined star")
        b.click(".note-head button[aria-label='Favorite']")
        b.wait("return document.querySelector(\".note-head button[aria-label='Favorite']\").getAttribute('aria-pressed') === 'true'", 5, "the star on")
        pressed, title, fill = b.js(star)
        check(title == "Remove from favorites" and fill != "none", "a favorite's star is filled")
        b.wait("return Array.from(document.querySelectorAll('.sb-section')).some(x => x.querySelector('.sb-label')?.textContent === 'Favorites' && x.textContent.includes('Plan'))",
               5, "Plan under Favorites")
        order = b.js("return Array.from(document.querySelectorAll('.sb-scroll > .sb-section'))"
                     ".map(x => x.classList.contains('sb-quick') ? 'quick' : x.querySelector('.sb-label')?.textContent).slice(0, 2)")
        check(order == ["quick", "Favorites"], f"Favorites come below the Quick notes entry ({order})")
        b.click(".note-head button[aria-label='Favorite']")
        b.wait("return document.querySelector(\".note-head button[aria-label='Favorite']\").getAttribute('aria-pressed') === 'false'", 5, "the star off")
        unlabelled = b.js("return Array.from(document.querySelectorAll('button')).filter(x => !x.textContent.trim() && !x.getAttribute('aria-label')).length")
        check(unlabelled == 0, f"icon buttons have accessible names ({unlabelled} without)")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_offline_tabs(env):
    b = Browser()
    try:
        signup(b, uniq("ola"))
        new_note(b, "Draft", "start")
        url = b.js("return location.href")
        tab_a = b.tab()
        tab_b = b.new_tab()
        b.go(url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('start')", 20, "tab B opens the note")
        end = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
        env.stop_server()
        time.sleep(1)
        # Both tabs edit while offline; then the first one is closed.
        b.switch(tab_a)
        b.js(end)
        b.keys(" fromA")
        time.sleep(1)
        b.switch(tab_b)
        b.js(end)
        b.keys(" fromB")
        time.sleep(1)
        b.switch(tab_a)
        b.close_tab()
        b.switch(tab_b)
        env.start_server()
        b.go(url)
        b.wait("const t = document.querySelector('.ed') && document.querySelector('.ed').textContent; return t && t.includes('fromA') && t.includes('fromB')", 30,
               "both offline edits after reconnecting")
        check(True, "unsent changes of a closed tab survive another tab's")
        # Acknowledged by the server: nothing unsent is left, the closed tab's
        # stored changes included.
        b.wait("return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.')).length === 0", 30, "everything sent")
        check(True, "the taken-over changes are sent and their storage is cleared")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_offline_start(env):
    b = Browser()
    try:
        signup(b, uniq("otto"))
        new_note(b, "Trip", "pack the tent")
        note_url = b.js("return location.href")
        time.sleep(3)  # the local copy is written
        b.go(env.url + "settings/devices")
        b.find(".offline-toggle", 15)
        b.js("window.confirm = () => true")
        b.click(".offline-toggle")
        b.wait("return !!navigator.serviceWorker.controller && Object.keys(localStorage).some(k => k.startsWith('notekpr.offline.data.'))", 20,
               "the offline setting to take effect")
        check(True, "offline use can be turned on")
        env.stop_server()
        b.go(note_url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('pack the tent')", 30,
               "the note without the server")
        check(True, "the app starts and shows notes without the server")
        end = "const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
        b.js(end)
        b.keys(" and a stove")
        time.sleep(1)
        check(b.js("return Object.keys(localStorage).some(k => k.startsWith('notekpr.pending.'))"), "offline edits are kept")
        env.start_server()
        b.wait("return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.')).length === 0", 60, "the edit to sync")
        check(True, "edits made after an offline start sync when the server is back")
        b.go(note_url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('and a stove')", 20, "the synced note")
        b.go(env.url + "settings/devices")
        b.find(".offline-toggle", 15)
        check(b.js("return document.querySelector('.offline-toggle').checked"), "the setting shows as on")
        b.click(".offline-toggle")
        b.js("window.__regs = null; navigator.serviceWorker.getRegistrations().then(r => window.__regs = r.length)")
        b.wait("return !Object.keys(localStorage).some(k => k.startsWith('notekpr.offline.')) && window.__regs !== null", 20, "the offline data to go")
        b.wait("navigator.serviceWorker.getRegistrations().then(r => window.__regs = r.length); return window.__regs === 0", 20, "the app copy to go")
        check(True, "turning it off removes the stored keys and the app copy")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_quick_notes(env):
    b, m = Browser(), None
    cards = "return Array.from(document.querySelectorAll('.qn-card .qn-text')).map(e => e.textContent)"
    marked = "return Array.from(document.querySelectorAll('.sidebar .active, .sidebar .holds-open')).map(e => e.textContent.trim())"
    try:
        signup(b, uniq("quinn"))
        # The shortcut, even from inside the editor: a new quick note, ready to type.
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.chord(KEYS["alt"], "q")
        b.wait("return /^\\d{4}-\\d\\d-\\d\\d \\d\\d\\.\\d\\d$/.test((document.querySelector('.note-title') || {}).value || '')", 10, "a quick note opens")
        b.wait("return !!document.activeElement.closest('.ed')", 5, "the caret in its editor")
        check(True, "Alt+Q creates a quick note named by the time and puts the caret in its editor")
        check(b.js("return document.querySelector('.crumbs').textContent").endswith("Quick notes"), "it is an ordinary note in the Quick notes folder")
        b.keys("Call the dentist")
        time.sleep(0.8)
        check(editor_text(b) == "Call the dentist", "typing goes into the quick note")
        # The sidebar marks where the note is once, and nothing moves by itself:
        # the tree keeps the Quick notes folder closed and marks it; the
        # shortcut lists (Recent, the Quick notes entry) mark nothing.
        title = b.js("return document.querySelector('.note-title').value")
        b.wait("return Array.from(document.querySelectorAll('.sidebar .sb-link')).some(e => e.textContent.includes('" + title + "'))", 10, "the note in Recent")
        check(b.js(marked) == ["Quick notes"],
              "the tree marks the closed Quick notes folder instead of opening it, and nothing else is marked")
        folder = "document.querySelector('.tree-row.folder[aria-level=\"1\"]').getAttribute('aria-expanded')"
        check(b.js("return " + folder) == "false", "the folder tells assistive tech it is closed")
        b.click(".tree-row.holds-open .tree-item")
        b.wait("return !!document.querySelector('.tree-row.active')", 5, "the folder opens")
        check(b.js(marked) == [title], "opened, the folder shows the note itself as the one marked place")
        check(b.js("return " + folder) == "true" and b.js("return document.querySelector('.tree-row.active').getAttribute('aria-level')") == "2",
              "and that it is open, the note its level")
        check(b.js("return document.querySelectorAll('.sb-quick .sb-link').length") == 1,
              "the Quick notes entry in the sidebar doesn't list the notes")
        # The overview page: capture several thoughts without leaving it.
        b.click(".sb-quick .sb-link")
        b.find(".quick-capture textarea", 10)
        check(b.js("return location.pathname") == "/quick", "the Quick notes entry opens /quick")
        check(b.js("return !!document.querySelector('.sb-actions .btn.primary') && !!document.querySelector('.sidebar .tree')"
                   " && document.querySelector('.vault-btn .vname').textContent === 'My notes'"),
              "the sidebar stays as it was: same vault, its tree and the New note buttons")
        b.click(".quick-capture textarea")
        b.keys("Garden plan" + KEYS["enter"] + "tomatoes and beans")
        b.chord(KEYS["ctrl"], KEYS["enter"])
        b.wait("return document.querySelector('.quick-capture textarea').value === ''", 5, "the box is cleared")
        b.wait(cards + ".join('|') === 'Garden plan|Call the dentist'", 10, "two cards, newest first")
        check(b.js("return location.pathname") == "/quick", "Ctrl+Enter saves a quick note without leaving the page and clears the box")
        check("tomatoes and beans" in b.js("return document.querySelector('.qn-card .qn-rest').textContent"), "cards show more of the text")
        b.keys("Old thought")
        b.click(".quick-capture button[type=submit]")
        b.wait(cards + ".join('|') === 'Old thought|Garden plan|Call the dentist'", 10, "three cards")
        b.wait("return document.activeElement === document.querySelector('.quick-capture textarea')", 5, "focus back in the box")
        check(True, "the Save button saves too and leaves the focus in the box")
        # Promote: a name (default: the first line) and a folder.
        b.click_el(b.find_text(".qn-open", "Garden plan"))
        b.wait("return !!document.querySelector('.note-title') && document.querySelector('.note-title').value !== ''", 10, "the card opens its note")
        check(editor_text(b) == "Garden plan\ntomatoes and beans", "a card opens its note")
        b.js("history.back()")
        b.find(".quick-capture", 10)
        b.js("Array.from(document.querySelectorAll('.qn-card')).find(c => c.textContent.includes('Garden plan')).querySelector('.qn-promote').click()")
        b.find(".modal .promote-name", 5)
        time.sleep(0.3)
        check(b.js("return document.querySelector('.promote-name').value") == "Garden plan", "promoting suggests the first line as the name")
        b.js("const i=document.querySelector('.promote-name'); i.focus(); i.select();")
        b.keys("Vegetable garden")
        b.click(".promote-folder")
        b.keys("Projects")
        b.click(".modal button[type=submit]")
        b.wait(cards + ".join('|') === 'Old thought|Call the dentist'", 10, "the promoted note leaves the quick notes")
        check(not b.js("return !!document.querySelector('.modal')"), "promoting closes the dialog")
        b.click_el(b.find_text(".tree-item", "Projects", 10))
        b.click_el(b.find_text(".tree-item", "Vegetable garden", 10))
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Vegetable garden'", 10, "the promoted note")
        check(b.js("return Array.from(document.querySelectorAll('.tree-row.folder')).find(r => r.textContent.trim() === 'Projects').getAttribute('aria-expanded')") == "true"
              and b.js("return document.querySelector('.tree-row.active').textContent.trim()") == "Vegetable garden",
              "a folder stays open when a note in it is opened")
        check(b.js("return document.querySelector('.crumbs').textContent").endswith("Projects") and editor_text(b) == "Garden plan\ntomatoes and beans",
              "promoting moves and renames it out of Quick notes, keeping its text")
        # Archive: hidden from the list, unless asked for.
        b.click(".sb-quick .sb-link")
        b.find(".qn-card", 10)
        b.js("Array.from(document.querySelectorAll('.qn-card')).find(c => c.textContent.includes('Call the dentist')).querySelector('.qn-archive').click()")
        b.wait(cards + ".join('|') === 'Old thought'", 10, "archived note hidden")
        check(True, "archiving hides a quick note from the list")
        b.click(".qn-archived-toggle")
        b.wait(cards + ".join('|') === 'Old thought|Call the dentist'", 10, "archived shown")
        check(b.js("return Array.from(document.querySelectorAll('.qn-card.archived')).map(c => c.querySelector('.qn-text').textContent)") == ["Call the dentist"],
              "“Show archived” lists archived ones, marked as such")
        # Shortcut help and the settings toggle.
        b.chord(KEYS["ctrl"], "/")
        b.find(".modal table.keys", 5)
        check("Alt+Q" in b.js("return document.querySelector('.modal table.keys').textContent"), "the shortcut help lists Alt+Q")
        b.chord(KEYS["escape"])
        b.click(".user-btn")
        b.click_el(b.find_text(".menu.up button", "Settings"))
        b.click_el(b.find_text(".settings-nav button", "Editor"))
        b.click(".quick-sidebar-toggle")
        b.wait("return !document.querySelector('.sb-quick')", 5, "the section is gone")
        check(True, "the sidebar section can be turned off in Settings")
        b.click(".quick-sidebar-toggle")
        b.find(".sb-quick", 5)
        check(not severe_logs(b), "no browser errors")
        # Phone width: the overview page and the capture box fit and work.
        m = Browser(390, 844)
        signup(m, uniq("quip"))
        m.go(env.url + "quick")
        m.wait("const t = document.querySelector('.quick-capture textarea'); return t && !t.disabled", 20, "the capture box")
        m.click(".quick-capture textarea")
        m.keys("Idea on the go")
        m.click(".quick-capture button[type=submit]")
        m.wait(cards + ".join('|') === 'Idea on the go'", 10, "the card on a phone")
        fits = m.js("""const w = innerWidth, r = e => e.getBoundingClientRect();
            const els = [document.querySelector('.quick-capture'), document.querySelector('.quick-capture .btn'),
                         document.querySelector('.qn-card'), document.querySelector('.qn-card .qn-archive')];
            return document.querySelector('.main').scrollWidth <= w && els.every(e => e && r(e).left >= 0 && r(e).right <= w);""")
        check(fits, "at 390px the capture box, its Save button and the cards fit the screen")
        m.shot("quick-mobile")
        check(not severe_logs(m), "no browser errors on the phone")
    finally:
        b.close()
        if m:
            m.close()


def png_file(path, w, h):
    """Write a w×h RGB gradient PNG (stdlib only)."""
    import struct, zlib
    raw = b"".join(b"\x00" + bytes(v for x in range(w) for v in (x * 255 // w, y * 255 // h, 128)) for y in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def test_vault_icons(env):
    a, b = Browser(), Browser()
    btn_icon = "(document.querySelector('.vault-btn .vault-emoji') || {}).textContent"
    shared_card = "Array.from(document.querySelectorAll('.card')).find(c => c.textContent.includes('Shared with you'))"
    try:
        an, bn = uniq("ivy"), uniq("ike")
        signup(a, an)
        signup(b, bn)
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "All vaults", 10))
        b.wait("return !!" + shared_card, 15, "the shared vault's card")
        a.click(".sb-label button[title='Vault settings']")
        a.find(".icon-pick", 5)
        a.js("document.querySelector('.emoji-btn[aria-label=\"🌱\"]').click()")
        a.wait("return " + btn_icon + " === '🌱'", 10, "the emoji in the switcher")
        check(True, "an emoji from the list becomes the vault's icon")
        b.wait("const c = " + shared_card + "; return (c.querySelector('.vault-emoji') || {}).textContent === '🌱'", 15, "the member's card")
        check(True, "members see it right away")
        # WebDriver can't type characters outside the BMP: set the box's value.
        a.js("const i = document.querySelector('.icon-input'); i.value = '🦊'; i.dispatchEvent(new Event('input'))")
        a.click(".icon-actions button[type=submit]")
        a.wait("return " + btn_icon + " === '🦊'", 10, "the typed emoji")
        check(True, "any emoji can be typed instead")
        # A picture: made a small square from the image, shown instead of the emoji.
        text = os.path.join(env.data, "not-an-image.png")
        with open(text, "w") as f:
            f.write("hello")
        a.type(".vault-picture-input", text)
        a.find_text(".toast.error", "isn't an image", 10)
        check(a.js("return " + btn_icon) == "🦊", "a file that isn't an image is refused with a message")
        pic = os.path.join(env.data, "wide.png")
        png_file(pic, 300, 200)
        a.type(".vault-picture-input", pic)
        a.wait("const i = document.querySelector('.vault-btn img.vault-pic'); return !!i && i.complete && i.naturalWidth > 0", 15, "the picture")
        w, h, src = a.js("const i = document.querySelector('.vault-btn img.vault-pic'); return [i.naturalWidth, i.naturalHeight, i.src.slice(0, 11)]")
        check(w == h == 128 and src == "data:image/", f"an uploaded image becomes a 128 px square picture ({w}×{h})")
        b.wait("const c = " + shared_card + "; return !!c.querySelector('img.vault-pic')", 15, "the member's card")
        check(True, "members see the picture too")
        stored = b"".join(open(os.path.join(env.data, f), "rb").read() for f in os.listdir(env.data) if f.startswith("notekpr.db"))
        check("🦊".encode() not in stored and "🌱".encode() not in stored and b'"picture"' not in stored,
              "the server only ever stores icon and picture encrypted")
        a.click(".icon-remove")
        a.wait("return !document.querySelector('.vault-btn .vault-pic, .vault-btn .vault-emoji')", 10, "the generic icon")
        check(True, "Remove goes back to the generic icon")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_entry_icons(env):
    a, b = Browser(), Browser()
    row = "Array.from(document.querySelectorAll('.tree-row')).filter(r => r.textContent.trim().endsWith(arguments[0]))"
    def row_icon(br, name):
        return br.js("const r = " + row + "[0]; return r ? (r.querySelector('.entry-icon') || {}).textContent || null : 'missing'", name)
    def set_icon(br, name, emoji):
        br.js(row + "[0].querySelector('.tree-more').click()", name)
        br.click_el(br.find_text(".tree-row .menu button", "Set icon", 5))
        br.find(".modal .icon-pick", 5)
        br.js("document.querySelector('.modal .emoji-btn[aria-label=\"' + arguments[0] + '\"]').click()", emoji)
        br.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
    try:
        an, bn = uniq("ora"), uniq("oto")
        signup(a, an)
        signup(b, bn)
        share(a, ".sb-bottom button:nth-child(2)", bn)
        new_note(a, "Trip", "Packing list")
        url = a.js("return location.pathname")
        # Before the title, a button to add one; the title stays where it is.
        title_x = "return document.querySelector('.note-title').getBoundingClientRect().left"
        x = a.js(title_x)
        a.click(".note-title-row .note-icon.add")
        a.find(".modal .icon-pick", 5)
        a.js("document.querySelector('.modal .emoji-btn[aria-label=\"🌱\"]').click()")
        a.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        check(a.js("return document.querySelector('.note-title-row .note-icon').textContent") == "🌱" and a.js(title_x) == x,
              "the button before the title adds an icon, without moving the title")
        check(row_icon(a, "Trip") == "🌱", "a note's icon shows in the tree too")
        a.wait("return Array.from(document.querySelectorAll('.sb-link')).some(e => e.textContent.includes('Trip') && (e.querySelector('.entry-icon') || {}).textContent === '🌱')", 5, "Recent")
        check(True, "and in Recent")
        b.go(env.url.rstrip("/") + url)
        b.wait("return " + row + ".length === 1", 20, "the tree", "Trip")
        b.wait("const r = " + row + "[0]; return (r.querySelector('.entry-icon') || {}).textContent === '🌱'", 15, "the member's tree", "Trip")
        check(True, "members see it")
        # Folders: one with an entry, and one only there because notes are in it.
        a.click(".sb-actions button[title='New folder']")
        a.type(".modal input", "Travel")
        a.click(".modal button[type=submit]")
        a.wait("return " + row + ".length === 1", 10, "the folder", "Travel")
        a.js(row + "[0].querySelector('.tree-more').click()", "Travel")
        a.click_el(a.find_text(".tree-row .menu button", "Set icon", 5))
        a.find(".modal .icon-pick", 5)
        # WebDriver can't type characters outside the BMP: set the box's value.
        a.js("const i = document.querySelector('.modal .icon-input'); i.value = '🧳'; i.dispatchEvent(new Event('input'))")
        a.click(".modal .icon-actions button[type=submit]")
        a.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        check(row_icon(a, "Travel") == "🧳", "a folder gets an icon, any emoji can be typed")
        a.chord(KEYS["alt"], "q")
        a.wait("return " + row + ".length === 1", 10, "the Quick notes folder", "Quick notes")
        set_icon(a, "Quick notes", "💡")
        check(row_icon(a, "Quick notes") == "💡" and a.js("return " + row + ".length", "Quick notes") == 1,
              "so does a folder that only held notes, without showing twice")
        # Clicking the icon by the title changes it; Remove takes it away.
        a.go(env.url.rstrip("/") + url)
        a.find(".note-title-row .note-icon", 15)
        a.click(".note-title-row .note-icon")
        a.find(".modal .icon-pick", 5)
        a.click(".modal .icon-remove")
        a.wait("return !document.querySelector('.modal') && !document.querySelector('.note-title-row .note-icon:not(.add)')", 5, "removed")
        check(row_icon(a, "Trip") is None and a.js("return !!document.querySelector('.note-title-row .note-icon.add')"),
              "the icon by the title opens the choice; Remove takes it away and the button to add one is back")
        b.wait("const r = " + row + "[0]; return !r.querySelector('.entry-icon')", 15, "removed for the member", "Trip")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_tree_order(env):
    a, b = Browser(), Browser()
    names = "Array.from(document.querySelectorAll('.tree-row')).map(r => r.textContent.trim()).filter(n => ['Garden', 'Kitchen', 'Alpha', 'Bravo', 'Charlie'].includes(n))"
    row = "Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === arguments[0])"
    def wait_order(br, want, what):
        try:
            br.wait("return JSON.stringify(" + names + ") === arguments[0]", 15, what, json.dumps(want, separators=(",", ":")))
        except RuntimeError:
            raise RuntimeError(f"{what}: {br.js('return ' + names)}, not {want}")
        check(True, what)
    # WebDriver can't drive HTML5 drag and drop: fire the events a real drag
    # fires, `frac` of the way down the target row.
    def drag_over(br, src, dst, frac):
        br.js("""const find = n => Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === n);
            const src = find(arguments[0]), dst = find(arguments[1]), r = dst.getBoundingClientRect();
            window.__drag = {src, dst, dt: new DataTransfer()};
            window.__drag.at = {bubbles: true, cancelable: true, dataTransfer: window.__drag.dt, clientX: r.left + r.width / 2, clientY: r.top + r.height * arguments[2]};
            src.dispatchEvent(new DragEvent('dragstart', {bubbles: true, dataTransfer: window.__drag.dt}));
            dst.querySelector('.tree-item').dispatchEvent(new DragEvent('dragover', window.__drag.at));""", src, dst, frac)
    def drop(br):
        br.js("""const d = window.__drag;
            d.dst.querySelector('.tree-item').dispatchEvent(new DragEvent('drop', d.at));
            d.src.dispatchEvent(new DragEvent('dragend', {bubbles: true, dataTransfer: d.dt}));""")
    def drag(br, src, dst, frac, mark):
        drag_over(br, src, dst, frac)
        br.wait("return " + row + ".classList.contains(arguments[1])", 5, "the drop mark", dst, mark)
        drop(br)
    try:
        an, bn = uniq("nadia"), uniq("nils")
        signup(a, an)
        signup(b, bn)
        share(a, ".sb-bottom button:nth-child(2)", bn)
        for t in ["Bravo", "Alpha", "Charlie"]:
            new_note(a, t, t.lower())
        url = a.js("return location.pathname")
        for f in ["Kitchen", "Garden"]:
            a.click(".sb-actions button[title='New folder']")
            a.type(".modal input", f)
            a.click(".modal button[type=submit]")
            a.wait("return !!" + row, 10, "the folder", f)
        wait_order(a, ["Garden", "Kitchen", "Alpha", "Bravo", "Charlie"], "folders first, then notes, each A–Z")
        check(not a.js("return !!document.querySelector('.sb-sort')"), "nothing to sort back yet")

        # Between two notes: a line shows where it goes.
        drag_over(a, "Charlie", "Alpha", 0.2)
        a.wait("return " + row + ".classList.contains('drop-before')", 5, "the line", "Alpha")
        check(a.js("return getComputedStyle(" + row + ", '::before').height", "Alpha") == "2px", "a line above the row shows where it will go")
        drop(a)
        wait_order(a, ["Garden", "Kitchen", "Charlie", "Alpha", "Bravo"], "dropped above Alpha, it stays there")
        drag(a, "Alpha", "Bravo", 0.8, "drop-after")
        wait_order(a, ["Garden", "Kitchen", "Charlie", "Bravo", "Alpha"], "and below Bravo")
        drag(a, "Kitchen", "Garden", 0.1, "drop-before")
        wait_order(a, ["Kitchen", "Garden", "Charlie", "Bravo", "Alpha"], "folders are put in order among folders")
        # Folders stay above notes: over a note, a folder goes into the note's folder.
        drag_over(a, "Garden", "Charlie", 0.5)
        a.wait("return document.querySelector('.tree').classList.contains('drop') && !document.querySelector('.drop-before, .drop-after')", 5, "the top level marked")
        drop(a)
        check(a.js("return JSON.stringify(" + names + ")") == json.dumps(["Kitchen", "Garden", "Charlie", "Bravo", "Alpha"], separators=(",", ":")),
              "a folder can't go between notes")

        # Members see the same order.
        b.go(env.url.rstrip("/") + url)
        wait_order(b, ["Kitchen", "Garden", "Charlie", "Bravo", "Alpha"], "members see the same order")

        # Onto a folder: into it.
        drag(a, "Bravo", "Garden", 0.5, "drop")
        a.wait("return " + row + ".getAttribute('aria-level') === '2'", 10, "Bravo inside Garden", "Bravo")
        check(True, "dropped on a folder, a note goes into it")

        # Back to A–Z.
        a.click(".sb-sort")
        wait_order(a, ["Garden", "Bravo", "Kitchen", "Alpha", "Charlie"], "Sort A–Z puts the top level back in order")
        wait_order(b, ["Garden", "Kitchen", "Alpha", "Charlie"], "for members too")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_session_ended_while_away(env):
    b = Browser()
    try:
        cdp(b, "Page.addScriptToEvaluateOnNewDocument", {"source": BLOCKABLE_SOCKETS})
        signup(b, uniq("sam"))
        # A quiet page: settings saved (1.5 s after a change), nothing else
        # due — a request answered 401 would sign it out by another way.
        time.sleep(3)
        # Away (no WebSocket) while the session ends — say, a demo server
        # that started over: no event can tell the page.
        b.js("window.__wsBlocked = true; window.__sockets.forEach(s => s.close())")
        sessions = api_get(b, "/me/sessions") or []
        check(len(sessions) == 1 and api_delete(b, f"/me/sessions/{sessions[0]['id']}") == 204,
              "the session ends while the page has no connection")
        b.js("window.__wsBlocked = false")
        b.wait("return !!document.querySelector('.dev-box')", 30, "the sign-in page")
        check("Your session ended" in b.js("return document.body.innerText"),
              "back, the server refuses the socket's session and the page signs out (not \"offline\" for ever)")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


# Select `text` (its first occurrence) in the open note's editor.
SELECT_TEXT = """
const ed = document.querySelector('.ed');
const w = document.createTreeWalker(ed, NodeFilter.SHOW_TEXT);
let n;
while ((n = w.nextNode())) {
  const i = n.data.indexOf(arguments[0]);
  if (i >= 0) {
    const r = document.createRange();
    r.setStart(n, i);
    r.setEnd(n, i + arguments[0].length);
    const s = getSelection();
    s.removeAllRanges();
    s.addRange(r);
    return true;
  }
}
return false;
"""


def test_comments(env):
    a, b = Browser(), Browser()
    def comment_on(br, text, body):
        check(br.js(SELECT_TEXT, text), f"“{text}” is in the note")
        br.click(".cm-offer", 5)
        br.wait("return !!document.activeElement && document.activeElement.matches('.cm-compose textarea')", 5, "the comment form")
        br.keys(body)
        br.chord(KEYS["ctrl"], KEYS["enter"])
        br.wait("return !document.querySelector('.cm-compose')", 5, "the comment is sent")
    def open_highlight(br, nth=0):
        """Click the middle of a comment's highlight."""
        r = br.js("const h = document.querySelectorAll('.ed-marks .hl.cm')[arguments[0]]; if (!h) return null; const b = h.getBoundingClientRect(); return [b.left + b.width / 2, b.top + b.height / 2];", nth)
        check(r is not None, "a highlight is there")
        br.js("const [x, y] = arguments; const el = document.elementFromPoint(x, y);"
              "const p = document.caretPositionFromPoint ? document.caretPositionFromPoint(x, y) : null;"
              "if (p) { const s = getSelection(); s.collapse(p.offsetNode, p.offset); }"
              "el.dispatchEvent(new MouseEvent('click', {bubbles: true, clientX: x, clientY: y}));", r[0], r[1])
        br.find(".cm-thread", 5)
    texts_expr = "Array.from(document.querySelectorAll('.cm-thread .cm-text')).map(e => e.textContent)"
    texts = "return " + texts_expr
    try:
        an, bn = uniq("cora"), uniq("cid")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Groceries", "Buy milk and bread today")
        url = a.js("return location.pathname")
        share(a, ".sb-bottom button:nth-child(2)", bn, role="viewer")

        # The owner comments on some of the text: highlighted, thread open.
        comment_on(a, "milk", "Oat milk?")
        a.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 1", 5, "the highlight")
        a.find(".cm-thread", 5)
        check(a.js(texts) == ["Oat milk?"], "the text gets a highlight and the thread opens by it")
        check(a.js("return document.querySelector('.ed').textContent.includes('Buy milk and bread today')"),
              "the note's text is untouched")
        a.chord(KEYS["escape"])

        # A viewer sees it, opens it from the text and replies.
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 1", 20, "the viewer's highlight")
        open_highlight(b)
        check(b.js(texts) == ["Oat milk?"] and not b.js("return !!document.querySelector('.cm-thread .cm-unverified')"),
              "a viewer opens the thread from its text (author verified)")
        b.js("document.querySelector('.cm-reply textarea').focus()")
        b.keys("Yes please")
        b.click(".cm-reply .btn.primary")
        b.wait("return document.querySelectorAll('.cm-thread .cm-item').length === 2", 5, "the reply")
        a.wait("return document.querySelector('.cm-count .badge') && document.querySelector('.cm-count .badge').textContent === '1'", 15, "the owner's count")
        open_highlight(a)
        a.wait("return " + texts_expr + ".length === 2", 15, "the reply reaches the owner")
        check(a.js(texts) == ["Oat milk?", "Yes please"], "the viewer's reply reaches the owner live")
        a.chord(KEYS["escape"])

        # The viewer comments too; the side panel lists both.
        comment_on(b, "bread", "Whole grain")
        a.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 2", 15, "the viewer's comment")
        a.click(".cm-count")
        a.wait("return document.querySelectorAll('.cm-section .cm-entry').length === 2", 5, "the list")
        check(True, "viewers comment too; the side panel lists the threads")

        # Resolved: the highlight goes, for everyone.
        open_highlight(a)
        a.click(".cm-thread .cm-head button[title='Resolve']")
        a.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 1", 5, "resolved")
        b.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 1", 15, "resolved for the viewer")
        check(True, "a resolved thread loses its highlight, for everyone")

        # The owner turns comments by viewers off.
        a.click(".sb-label button[title='Vault settings']")
        a.find(".modal .viewers-comment input", 5)
        a.js("document.querySelector('.modal .viewers-comment input').click()")
        time.sleep(0.8)
        a.chord(KEYS["escape"])
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 1", 20, "reloaded")
        b.js(SELECT_TEXT, "today")
        time.sleep(0.6)
        check(not b.js("return !!document.querySelector('.cm-offer')"), "once the owner turns it off, viewers can't comment")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_graph(env):
    b = Browser()
    canvas = ".graph-page canvas"
    data = lambda css, key: "return (document.querySelector('" + css + "') || {dataset: {}}).dataset." + key
    try:
        signup(b, uniq("gil"))
        new_note(b, "Garden", "Plant [[Tomatoes]], see [[Compost]]")
        new_note(b, "Tomatoes", "Needs [[Compost]] and back to [[Garden]]")
        new_note(b, "Compost", "Scraps")
        new_note(b, "Trips", "Lisbon")
        b.click(".sb-graph")
        b.find(canvas, 10)
        b.wait(data(canvas, "edges") + " === '3'", 10, "the links")
        listed = b.js("return Array.from(document.querySelectorAll('.graph-page .graph-list button')).map(e => e.textContent)")
        check(b.js(data(canvas, "nodes")) == "5" and all(any(l.startswith(n + " (") for l in listed) for n in ["Garden", "Tomatoes", "Compost", "Trips", "Welcome"]),
              "the graph shows every note and the links between them")
        check("Garden (2 links)" in listed and "Compost (2 links)" in listed, "a link counts once whichever way it goes")
        b.click(".graph-orphans input")
        b.wait(data(canvas, "nodes") + " === '3'", 5, "only linked notes")
        check(True, "unlinked notes can be hidden")
        b.click(".graph-orphans input")
        # One note left: once laid out it is in the middle, where we click.
        b.js("const i = document.querySelector('.graph-filter'); i.value = 'tomat'; i.dispatchEvent(new Event('input'))")
        b.wait(data(canvas, "nodes") + " === '1'", 5, "the filter")
        time.sleep(1.5)
        b.click_at(canvas)
        b.wait("return (document.querySelector('.note-title') || {}).value === 'Tomatoes'", 10, "the note opens")
        check(True, "the filter narrows the graph, and clicking a note opens it")
        b.wait(data(".local-graph canvas", "nodes") + " === '3'", 10, "the local graph")
        check(True, "the side panel shows the note and its neighbours")
        b.chord(KEYS["ctrl"], "g")
        b.wait("return location.pathname.endsWith('/graph')", 5, "Ctrl+G")
        check(True, "Ctrl+G opens the graph")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_recovered_copy(env):
    a, b = Browser(), Browser()
    try:
        an, bn = uniq("rho"), uniq("rex")
        signup(a, an)
        signup(b, bn)
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Kitchen")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Kitchen", 15)
        new_note(a, "Soup", "leeks and potatoes")
        path = a.js("return location.pathname")
        vid, oid = path.split("/")[2], path.split("/")[3]
        share(a, ".sb-bottom button:nth-child(2)", bn)
        cdp(b, "Page.addScriptToEvaluateOnNewDocument", {"source": TRACK_SOCKETS})
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('potatoes')", 20, "rex sees the note")
        # Rex loses his connection and keeps writing; meanwhile the owner
        # deletes the note for good.
        set_offline(b, True)
        b.js("window.__sockets.forEach(s => s.close())")
        b.wait("return (document.querySelector('.sync .lbl')||{}).textContent.startsWith('Offline')", 20, "rex offline")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(" and a pinch of salt")
        time.sleep(1)
        check(api_delete(a, f"/vaults/{vid}/objects/{oid}") == 204, "the owner deletes the note permanently")
        set_offline(b, False)
        b.wait("return Array.from(document.querySelectorAll('.tree-row')).some(r => r.textContent.includes('Soup (recovered)'))", 40,
               "the recovered copy")
        check(True, "unsent edits of a note deleted meanwhile are kept as a \"(recovered)\" copy")
        b.js("Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.includes('Soup (recovered)')).querySelector('.tree-item').click()")
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('pinch of salt')", 20, "its text")
        check(True, "the copy holds what was written offline")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_html(env):
    b = Browser()
    try:
        signup(b, uniq("hta"))
        new_note(b, "Html", "")
        body = ("<details>\n<summary>More</summary>\n\nHidden **bold**\n\n</details>\n\n"
                "Press <kbd>Ctrl</kbd>+<kbd>K</kbd>\n\n"
                "<div align=\"center\" style=\"position:fixed;inset:0;background:red\" class=\"modal-back\" onclick=\"window.__pwned=1\">\n"
                "<b>centered</b> <a href=\"javascript:window.__pwned=2\">bad</a> <a href=\"https://example.org/\">ok</a>\n</div>\n\n"
                "<script>window.__pwned = 3</script>\n\n<img src=x onerror=\"window.__pwned = 4\">\n\n"
                "<iframe src=\"https://example.org\"></iframe><form><input name=pw></form>\n\nend")
        b.js("const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));", body)
        time.sleep(0.6)
        check(editor_text(b) == body, "the editor keeps HTML as its source")
        # Live preview: away from the caret an HTML block shows only its preview.
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        check(b.js("return !!document.querySelector('.ed .ln.html .hw div[align] b')"), "an HTML block shows its preview in the editor")
        check(b.js("return getComputedStyle(document.querySelector('.ed .hw')).display") != "none", "the preview shows away from the caret")
        source_folded = """return Array.from(document.querySelectorAll('.ed .ln.html')).every(l =>
            Array.from(l.childNodes).filter(n => !(n.dataset && 'w' in n.dataset)).every(n => {
                const r = document.createRange(); r.selectNodeContents(n); return r.getBoundingClientRect().height === 0; }))"""
        check(b.js(source_folded), "its source folds away")
        check(b.js("return Array.from(document.querySelectorAll('.ed .kbd')).map(k => k.textContent).join('+')") == "Ctrl+K", "inline HTML shows its look in the editor")
        check(b.js("return Array.from(document.querySelectorAll('.ed .mk.ht')).every(t => getComputedStyle(t).display === 'none')"), "and its tags hide away from the caret")
        check(not b.js("return !!document.querySelector('.ed .hw [style], .ed .hw [onclick], .ed .hw [class=modal-back], .ed .hw script, .ed .hw iframe, .ed .hw input')"),
              "the preview drops what isn't allowed")
        b.shot("html-editing")
        # A click on the preview shows the source; leaving the block shows the preview again.
        b.click(".ed .hw div[align] b")
        time.sleep(0.4)
        check(b.js("const l = document.querySelector('.ed .ln.html.active'); return !!l && l.textContent.startsWith('<div align')"), "a click on the preview shows the source")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.active .hw')).display") == "none", "without the preview")
        check(not b.js(source_folded), "the source shows")
        b.shot("html-source")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        check(b.js(source_folded), "away from it, the preview again")
        check(editor_text(b) == body, "clicking changed nothing")
        # The reading view.
        b.click(".seg button[title^='Reading']")
        b.find(".reading details", 10)
        check(b.js("return document.querySelector('.reading details summary').textContent") == "More", "details and summary render")
        check(b.js("return document.querySelector('.reading details strong').textContent") == "bold", "Markdown inside HTML renders")
        check(b.js("return Array.from(document.querySelectorAll('.reading kbd')).map(k => k.textContent).join('+')") == "Ctrl+K", "inline HTML renders")
        check(b.js("return getComputedStyle(document.querySelector('.reading div[align]')).textAlign").endswith("center"), "align is kept")
        check(not b.js("return !!document.querySelector('.reading [style], .reading [onclick], .reading .modal-back, .reading script, .reading iframe, .reading form, .reading input:not(.task), .reading img[src]')"),
              "scripts, frames, forms, styles and event handlers are dropped")
        check(not b.js("return !!document.querySelector('.reading a[href^=\"javascript\"]')"), "javascript: links are dropped")
        check(b.js("return document.querySelector('.reading a[href=\"https://example.org/\"]').rel").startswith("noopener"), "web links open on their own")
        b.js("document.querySelector('.reading div[align] b').click()")
        time.sleep(0.3)
        check(not b.js("return !!window.__pwned"), "nothing in the note ran")
        b.shot("html-reading")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_colors(env):
    b, m = Browser(), Browser(390, 844)
    red, yellow = '<span style="color: #e44045">', '<span style="background-color: #ffd0004d">'
    def pick(br, title):
        br.click(".tb[title='Text and background color']")
        br.click(f".menu.colors button[title='{title}']")
        time.sleep(0.4)
    try:
        signup(b, uniq("co"))
        # Below the first lines: the comment button over a selection would cover the toolbar.
        new_note(b, "Colors", "Colors\n\n\nMake this word red and that one blue.")
        check(b.js(SELECT_TEXT, "word"), "the word is in the note")
        pick(b, "Red text")
        check(f"this {red}word</span> red" in editor_text(b), "the toolbar writes a color other editors show too")
        check(b.js("return Array.from(document.querySelectorAll('.ed .fg-red:not(.mk)')).map(e => e.textContent).join('')") == "word", "the editor shows it")
        check(b.js("const e = document.querySelector('.ed .fg-red:not(.mk)'); return getComputedStyle(e).color != getComputedStyle(e.closest('.ln')).color"), "in color")
        b.shot("colors-menu-closed")
        # The selection stays on the colored text: a background for it, then none.
        pick(b, "Yellow background")
        check(f"this {red}{yellow}word</span></span> red" in editor_text(b), "text and background colors are separate")
        pick(b, "No background")
        pick(b, "Default text color")
        check("Make this word red and that one blue." in editor_text(b), "and both come off again, leaving no tags")
        check(b.js(SELECT_TEXT, "blue"), "the other word is in the note")
        pick(b, "Blue text")
        # Colors other editors write.
        foreign = '\n\n<font color="#ff0000">legacy</font> and <mark style="background: #BBFABBA6;">marked</mark>'
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.js("const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));", foreign)
        time.sleep(0.6)
        check(b.js("return document.querySelector('.ed .bg-green:not(.mk)')?.textContent") == "marked", "a highlight colored in another editor shows its color")
        b.click(".seg button[title^='Reading']")
        b.find(".reading p", 10)
        check(b.js("return document.querySelector('.reading .fg-blue')?.textContent") == "blue", "the reading view shows colors")
        check(b.js("return document.querySelector('.reading font.fg-red')?.textContent") == "legacy", "<font color> too")
        check(b.js("const e = document.querySelector('.reading mark.bg-green'); return !!e && getComputedStyle(e).backgroundColor != getComputedStyle(document.querySelector('.reading')).backgroundColor"), "and colored highlights")
        check(not b.js("return !!document.querySelector('.reading [style]')"), "never with the note's own CSS")
        b.shot("colors-reading")
        check(not severe_logs(b), "no browser errors")
        # On a phone the menu isn't cut off by the scrolling toolbar.
        signup(m, uniq("cm"))
        m.click(".note-head .mobile-only")
        time.sleep(0.4)
        new_note(m, "Phone", "Colors\n\n\ntap here")
        check(m.js(SELECT_TEXT, "here"), "the word is in the note")
        m.click(".tb[title='Text and background color']")
        hit = m.js("const b = document.querySelector(\".menu.colors button[title='Pink background']\"); if (!b) return false; const r = b.getBoundingClientRect(); const e = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!e && b.contains(e);")
        m.shot("colors-phone")
        check(hit, "the color menu shows whole on a phone")
        m.click(".menu.colors button[title='Pink background']")
        time.sleep(0.4)
        check('<span style="background-color: #d6409f4d">here</span>' in editor_text(m), "and works there")
    finally:
        b.close()
        m.close()


TESTS = [test_onboarding, test_collab, test_devices_and_recovery, test_features, test_attachments, test_note_sharing, test_tables, test_code_blocks, test_html, test_colors, test_code_wrapping, test_editor_mouse, test_editor_safety, test_local_copy, test_ui_basics, test_offline_tabs, test_offline_start, test_mobile, test_quick_notes, test_vault_icons, test_entry_icons, test_tree_order, test_session_ended_while_away, test_comments, test_graph, test_recovered_copy]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/notekpr-server")
    ap.add_argument("--web", default="dist/web")
    ap.add_argument("-k", default="")
    args = ap.parse_args()
    env = Env(args.server, args.web)
    failed = 0
    try:
        for t in TESTS:
            if args.k and args.k not in t.__name__:
                continue
            print(f"• {t.__name__}")
            try:
                t(env)
            except Exception:
                failed += 1
                traceback.print_exc()
                if env.server is None:
                    env.start_server()
    finally:
        env.close()
    print("FAILED" if failed else "ALL PASSED", f"({failed} failing)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
