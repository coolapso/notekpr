#!/usr/bin/env python3
"""Browser end-to-end tests for coolpsonotes (stdlib + chromedriver only).

Starts a development-mode server and chromedriver, then drives several
independent headless Chromium profiles as different users.

    python3 tests/e2e/suite.py --server target/debug/coolpsonotes-server --web dist/web
    python3 tests/e2e/suite.py -k collab        # run only matching tests

Screenshots are written to $CPN_E2E_OUT (default target/e2e).
"""
import argparse, os, shutil, socket, subprocess, sys, tempfile, time, traceback

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
        self.data = tempfile.mkdtemp(prefix="cpn-e2e-")
        self.port, self.wd_port = free_port(), free_port()
        self.url = f"http://localhost:{self.port}/"
        os.environ["CPN_URL"] = self.url
        os.environ["CPN_WEBDRIVER"] = f"http://127.0.0.1:{self.wd_port}"
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
        if self.server:
            self.server.terminate()
            self.server.wait(10)
            self.server = None

    def close(self):
        self.stop_server()
        self.driver.terminate()
        shutil.rmtree(self.data, ignore_errors=True)


from wd import Browser, KEYS, editor_text, signup  # noqa: E402  (after env vars are set)


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
            if l.get("level") == "SEVERE" and "WebSocket connection" not in m and "ERR_CONNECTION_REFUSED" not in m:
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
        check(editor_text(a) == "- milk\n- eggs\nThanks!", "list continuation and list exit work")
        url = a.js("return location.pathname")
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.click(".vault-btn")
        b.find_text(".vault-menu button", "Family", 15)
        check(True, "a newly shared vault appears for the recipient without reloading")
        b.chord(KEYS["escape"])
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Thanks!')", 20, "ben sees the note")
        check(True, "shared note decrypts for the second user")
        log = api_get(b, f"/vaults/{url.split('/')[2]}/log")
        check([e["body"]["kind"] for e in log["entries"]] == ["create", "add"],
              "the owner signed the vault's creation and ben's addition")
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
        check(b3.js("return Object.keys(localStorage).filter(k => k.startsWith('cpn.device.') || k.startsWith('cpn.pending.') || k.startsWith('cpn.newkeys.') || k.startsWith('cpn.head.')).length") == 0,
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
        check(a.js("return document.querySelector('.panel section .count').textContent") == "1", "backlink is detected")
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
        # Offline editing across a server restart.
        env.stop_server()
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
        b.click_el(b.find_text(".settings-body button", "Sign out", 10))
        b.find(".dev-box input", 15)
        check(not blob_loads(), "signing out revokes decrypted attachment URLs")
    finally:
        b.close()


API_GET = """
window.__api = undefined;
fetch('/api/v1' + arguments[0], {headers: {Authorization: 'Bearer ' + sessionStorage.getItem('cpn.token')}})
  .then(r => r.ok ? r.json() : null).then(j => { window.__api = j; }, () => { window.__api = null; });
"""


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
        a.wait("return Object.keys(localStorage).filter(k => k.startsWith('cpn.pending.') || k.startsWith('cpn.newkeys.')).length === 0", 15, "olga's edits are sent")
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
        # Share just this note with Pete.
        a.click(".note-head button[title='Share this note']")
        a.find_text(".modal h2", "Trip", 10)
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
        # A reload restores the shared note.
        b.go(env.url.rstrip("/") + b.js("return location.pathname"))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('and hats')", 20, "pete reloads")
        check(True, "the shared note reopens after a reload")
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
const r = indexedDB.open('coolpsonotes');
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
        check(sum(n.startswith("coolpsonotes-cache-") for n in b.js("return window.__locks")) == 1, "one tab holds the right to write the copy")
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
        b.wait("return Object.keys(localStorage).filter(k => k.startsWith('cpn.pending.')).length === 0", 30, "everything sent")
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
        b.wait("return !!navigator.serviceWorker.controller && Object.keys(localStorage).some(k => k.startsWith('cpn.offline.data.'))", 20,
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
        check(b.js("return Object.keys(localStorage).some(k => k.startsWith('cpn.pending.'))"), "offline edits are kept")
        env.start_server()
        b.wait("return Object.keys(localStorage).filter(k => k.startsWith('cpn.pending.')).length === 0", 60, "the edit to sync")
        check(True, "edits made after an offline start sync when the server is back")
        b.go(note_url)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('and a stove')", 20, "the synced note")
        b.go(env.url + "settings/devices")
        b.find(".offline-toggle", 15)
        check(b.js("return document.querySelector('.offline-toggle').checked"), "the setting shows as on")
        b.click(".offline-toggle")
        b.js("window.__regs = null; navigator.serviceWorker.getRegistrations().then(r => window.__regs = r.length)")
        b.wait("return !Object.keys(localStorage).some(k => k.startsWith('cpn.offline.')) && window.__regs !== null", 20, "the offline data to go")
        b.wait("navigator.serviceWorker.getRegistrations().then(r => window.__regs = r.length); return window.__regs === 0", 20, "the app copy to go")
        check(True, "turning it off removes the stored keys and the app copy")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_quick_notes(env):
    b, m = Browser(), None
    cards = "return Array.from(document.querySelectorAll('.qn-card .qn-text')).map(e => e.textContent)"
    sidebar = "return Array.from(document.querySelectorAll('.sb-quick .sb-link')).map(e => e.textContent.trim())"
    try:
        signup(b, uniq("quinn"))
        # The shortcut, even from inside the editor: a new quick note, ready to type.
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.chord(KEYS["ctrl"], KEYS["alt"], "q")
        b.wait("return /^\\d{4}-\\d\\d-\\d\\d \\d\\d\\.\\d\\d$/.test((document.querySelector('.note-title') || {}).value || '')", 10, "a quick note opens")
        b.wait("return !!document.activeElement.closest('.ed')", 5, "the caret in its editor")
        check(True, "Ctrl+Alt+Q creates a quick note named by the time and puts the caret in its editor")
        check(b.js("return document.querySelector('.crumbs').textContent").endswith("Quick notes"), "it is an ordinary note in the Quick notes folder")
        b.keys("Call the dentist")
        time.sleep(0.8)
        check(editor_text(b) == "Call the dentist", "typing goes into the quick note")
        b.wait(sidebar + ".includes('Call the dentist')", 10, "the sidebar entry")
        check(True, "the sidebar shows the quick note by its first line")
        # The overview page: capture several thoughts without leaving it.
        b.click(".sb-quick .sb-label-link")
        b.find(".quick-capture textarea", 10)
        check(b.js("return location.pathname") == "/quick", "the section header opens /quick")
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
        b.wait(sidebar + ".join('|') === 'Old thought|Garden plan|Call the dentist'", 10, "sidebar order")
        check(True, "the sidebar lists the latest quick notes, newest first")
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
        b.click_el(b.find_text(".sb-link", "My notes", 10))
        b.click_el(b.find_text(".tree-item", "Projects", 10))
        b.click_el(b.find_text(".tree-item", "Vegetable garden", 10))
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Vegetable garden'", 10, "the promoted note")
        check(b.js("return document.querySelector('.crumbs').textContent").endswith("Projects") and editor_text(b) == "Garden plan\ntomatoes and beans",
              "promoting moves and renames it out of Quick notes, keeping its text")
        # Archive: hidden from the sidebar and the list, unless asked for.
        b.click(".sb-quick .sb-label-link")
        b.find(".qn-card", 10)
        b.js("Array.from(document.querySelectorAll('.qn-card')).find(c => c.textContent.includes('Call the dentist')).querySelector('.qn-archive').click()")
        b.wait(cards + ".join('|') === 'Old thought'", 10, "archived note hidden")
        b.wait(sidebar + ".join('|') === 'Old thought'", 10, "archived note gone from the sidebar")
        check(True, "archiving hides a quick note from the list and the sidebar")
        b.click(".qn-archived-toggle")
        b.wait(cards + ".join('|') === 'Old thought|Call the dentist'", 10, "archived shown")
        check(b.js("return Array.from(document.querySelectorAll('.qn-card.archived')).map(c => c.querySelector('.qn-text').textContent)") == ["Call the dentist"],
              "“Show archived” lists archived ones, marked as such")
        # Shortcut help and the settings toggle.
        b.chord(KEYS["ctrl"], "/")
        b.find(".modal table.keys", 5)
        check("Ctrl+Alt+Q" in b.js("return document.querySelector('.modal table.keys').textContent"), "the shortcut help lists Ctrl+Alt+Q")
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


TESTS = [test_onboarding, test_collab, test_devices_and_recovery, test_features, test_attachments, test_note_sharing, test_tables, test_editor_safety, test_local_copy, test_ui_basics, test_offline_tabs, test_offline_start, test_mobile, test_quick_notes]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/coolpsonotes-server")
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
