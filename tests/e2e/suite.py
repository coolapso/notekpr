#!/usr/bin/env python3
"""Browser end-to-end tests for notekpr (stdlib + chromedriver only).

Starts a development-mode server and chromedriver, then drives several
independent headless Chromium profiles as different users.

    python3 tests/e2e/suite.py --server target/debug/notekpr-server --web dist/web
    python3 tests/e2e/suite.py -k collab        # run only matching tests

Screenshots are written to $NOTEKPR_E2E_OUT (default target/e2e).
"""
import argparse, base64, json, os, shutil, socket, subprocess, sys, tempfile, time, traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


ADMIN = "ada"
# Another administrator, for a test after the one that made ADMIN's keys
# (a second browser can't sign up as the same person).
ADMIN2 = "otto"


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
        # ADMIN and ADMIN2 are administrators whichever test signs in first.
        self.server = subprocess.Popen([self.server_bin, "--dev-mode", "--web-dir", self.web, "--data-dir", self.data,
                                        "--listen", f"127.0.0.1:{self.port}", "--public-url", self.url[:-1],
                                        "--admin", f"dev:{ADMIN},dev:{ADMIN2}"],
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


# In the open sharing dialog: give the member named arguments[0] the role
# arguments[1].
SET_MEMBER_ROLE = """
const r = Array.from(document.querySelectorAll('.people .person')).find(r => r.textContent.includes(arguments[0]));
const s = r.querySelector('.member-role'); s.value = arguments[1]; s.dispatchEvent(new Event('change'));
"""


def share(owner, vault_btn_css, user, role="editor"):
    owner.click(vault_btn_css)
    owner.type(".share-add input", user)
    owner.find(".found .person", 10)
    if role != "editor":
        owner.js("const s=document.querySelector('.share-add select'); s.value=arguments[0]; s.dispatchEvent(new Event('change'))", role)
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
        # As on a deployed server: the quick notes, made at the same time,
        # don't keep the first vault from being made.
        b.latency(80)
        signup(b, uniq("ada"))
        check(b.js("return document.querySelector('.note-title').value") == "Welcome", "welcome note is created and opened")
        check(b.find_text(".sb-drawings", "Drawings", 5), "the first vault is there, with its drawings")
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
        a.find(".device-banner", 15)
        a.js("document.querySelector('.main').scrollTop = 600")
        time.sleep(0.3)
        seen = a.js("""const r = document.querySelector('.device-banner').getBoundingClientRect();
            return [r.top >= 0 && r.bottom <= innerHeight, document.title];""")
        check(seen == [True, "(1) notekpr"], f"the waiting device stays in view, and in the tab's title ({seen})")
        a.click_el(a.find(".device-banner .btn", 15))
        a.wait(f"return (document.querySelector('.modal .verify-code')||{{}}).textContent == '{code_new}'", 10, "matching code in approval dialog")
        check(True, "both devices show the same verification code")
        a.click_el(a.find_text(".modal button", "approve"))
        a.wait("return !document.querySelector('.device-banner') && document.title == 'notekpr'", 10, "the banner and the count go")
        b2.find_text(".card", "My notes", 20)
        b2.click_el(b2.find_text(".card", "My notes"))
        b2.click_el(b2.find_text(".note-card", "Welcome", 15))
        b2.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('Work together')", 15, "device 2 decrypts")
        check(True, "approved device decrypts existing notes")
        a.go(env.url + "settings/security")
        a.find_text(".settings-body button", "Create a new recovery key", 15)
        a.click_el(a.find_text(".settings-body button", "Create a new recovery key"))
        a.answer()
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
        b3.click_el(forget)
        asked = b3.answer()
        check("Forget this browser?" in asked and "recovery key" in asked, "forgetting the browser asks first, in the app's own dialog")
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
        # The header doesn't make the page scroll sideways: its row scrolls on its own.
        sizes = m.js("""const m = document.querySelector('.main'), r = document.querySelector('.note-head .head-row');
            return [m.scrollWidth <= m.clientWidth, getComputedStyle(r).overflowX];""")
        check(sizes == [True, "auto"], f"the page doesn't scroll sideways, the header's row would ({sizes})")
        m.click(".note-head .icon-btn[title=More]")
        hit = m.js("const b = [...document.querySelectorAll('.note-head .menu button')].pop();"
                   "if (!b) return false; const r = b.getBoundingClientRect();"
                   "const e = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!e && b.contains(e);")
        check(hit, "the header's menus show whole on a phone")
        m.chord(KEYS["escape"])
        # The side panel covers the note: what is chosen in it, or a tap beside it, closes it.
        panel = ".note-head .icon-btn[title^='Side panel']"
        m.click(panel)
        m.find(".note-body.overlay .panel-item.outline", 10)
        m.js("[...document.querySelectorAll('.panel-item.outline')].pop().click()")
        m.wait("return !document.querySelector('.note-body.overlay') && document.querySelector('.main').scrollTop > 300", 5,
               "the panel closes and the note scrolls")
        check(True, "a heading chosen in the side panel closes it and shows the heading")
        m.click(panel)
        m.find(".note-body.overlay .panel", 10)
        m.js("document.elementFromPoint(20, 400).click()")
        m.wait("return !document.querySelector('.note-body.overlay')", 5, "the panel closes")
        check(True, "a tap beside the side panel closes it")
        # The page is what shows of the window (an iPhone's keyboard covers
        # the page instead of making it shorter, and slides it up).
        app_box = """const a = document.querySelector('.app').getBoundingClientRect();
            return [Math.round(a.top), Math.round(a.height), document.documentElement.clientHeight,
                    getComputedStyle(document.documentElement).getPropertyValue('--visible-h').trim()];"""
        fit = m.js(app_box)
        check(fit[0] == 0 and fit[1] == fit[2] and fit[3] == f"{fit[2]}.0px", f"the app is what shows of the window ({fit})")
        # The window's own scroll (the keyboard's, on an iPhone) is undone.
        slid = m.js("""const tall = document.createElement('div'); tall.id = 'tall'; tall.style.height = '3000px';
            document.body.append(tall); window.scrollTo(0, 400); return window.scrollY;""")
        m.wait("return window.scrollY == 0 && document.querySelector('.app').getBoundingClientRect().top == 0", 5,
               "the window's scroll is undone")
        m.js("document.getElementById('tall').remove()")
        check(slid == 400, f"a scrolled window comes back, header and all (it was at {slid})")
        # Pinch-zoomed in, what shows is a part of the page: the page keeps its size.
        cdp(m, "Emulation.setPageScaleFactor", {"pageScaleFactor": 2})
        try:
            m.wait("return visualViewport.scale == 2", 5, "zoomed in")
            time.sleep(0.3)
            zoomed = m.js(app_box)
        finally:
            cdp(m, "Emulation.setPageScaleFactor", {"pageScaleFactor": 1})
        check(zoomed[:3] == fit[:3], f"zoomed in, the app keeps its size ({zoomed}, {fit})")
        m.click(".note-head .mobile-only")
        time.sleep(0.4)
        check(m.js("return document.querySelector('.app').classList.contains('drawer')"), "sidebar opens as a drawer")
        m.shot("mobile-drawer")
    finally:
        m.close()


ANDROID_UA = ("Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) "
              "Chrome/156.0.0.0 Mobile Safari/537.36")
IPHONE_UA = ("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 "
             "(KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1")
# An iPad's Safari says it is a Mac.
MAC_SAFARI_UA = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 "
                 "(KHTML, like Gecko) Version/18.0 Safari/605.1.15")

# Stands in for the browser's dialog (it can't be answered here): the
# event boot.js keeps, answering `outcome`.
FAKE_DIALOG = """const outcome = arguments[0]; window.__dialogs = 0;
    window.notekprHomeScreen = { prompt() { window.__dialogs++; return Promise.resolve({ outcome }); } };"""


def test_home_screen(env):
    """A phone's sign-in screen offers to add notekpr to the home screen:
    one tap opens Android's dialog, an iPhone or iPad gets the steps. Not on
    a computer, nor in notekpr opened from the home screen."""
    offer = "return !!document.querySelector('.home-offer')"

    def sign_in_page(b):
        b.go(env.url)
        b.find(".auth-card h1", 20)
        time.sleep(0.3)

    b = Browser(390, 844)
    try:
        sign_in_page(b)
        check(not b.js(offer), "a computer isn't offered it")
        # Chromium says notekpr can go on the home screen, without a service
        # worker; boot.js keeps that for the offer.
        b.wait("return !!window.notekprHomeScreen && window.notekprHomeScreen.type === 'beforeinstallprompt'", 20,
               "the browser's event")
        check(True, "boot.js keeps the browser's beforeinstallprompt")

        cdp(b, "Emulation.setUserAgentOverride", {"userAgent": ANDROID_UA})
        sign_in_page(b)
        b.find(".home-offer", 10)
        check("Add it first, then sign in there" in b.text(".home-offer"), "Android: offered on the sign-in screen")
        b.shot("home-screen-android")
        add = lambda: b.click_el(b.find_text(".home-offer button", "Add to home screen"))
        b.wait("return !!window.notekprHomeScreen", 20, "the browser's event")
        b.js(FAKE_DIALOG, "dismissed")
        add()
        b.wait("return window.__dialogs === 1", 5, "the dialog")
        time.sleep(0.3)
        check(b.js(offer) and not b.js("return !!document.querySelector('.home-offer .steps')"),
              "one tap opens the browser's dialog; said no to, the offer stays")
        add()
        b.find(".home-offer .steps", 5)
        check("Add to Home screen" in b.text(".home-offer .steps") and b.js("return window.__dialogs") == 1,
              "a dialog opens once: then the steps for the browser's menu")
        b.click_el(b.find_text(".home-offer button", "Done"))
        b.wait("return !document.querySelector('.home-offer')", 5, "the offer goes")
        sign_in_page(b)
        check(not b.js(offer), "put away, it stays away in this browser")

        b.js("localStorage.removeItem('notekpr.homescreen')")
        sign_in_page(b)
        b.wait("return !!window.notekprHomeScreen", 20, "the browser's event")
        b.js(FAKE_DIALOG, "accepted")
        add()
        b.find_text(".home-offer", "Added", 5)
        check(True, "added from the dialog: it says so")
        sign_in_page(b)
        check(not b.js(offer), "once added, it isn't offered again")

        b.js("localStorage.removeItem('notekpr.homescreen')")
        cdp(b, "Emulation.setUserAgentOverride", {"userAgent": IPHONE_UA})
        # Opened from the home screen: Chromium can't pretend display-mode:
        # standalone, but an iPhone's Safari also says so itself.
        flag = cdp(b, "Page.addScriptToEvaluateOnNewDocument",
                   {"source": "Object.defineProperty(navigator, 'standalone', { value: true })"})
        sign_in_page(b)
        check(not b.js(offer), "not offered in notekpr opened from the home screen")
        cdp(b, "Page.removeScriptToEvaluateOnNewDocument", {"identifier": flag["identifier"]})

        sign_in_page(b)
        b.find(".home-offer", 10)
        b.js("window.notekprHomeScreen = null")  # Safari has no dialog for it
        add()
        b.find(".home-offer .steps", 5)
        steps = b.text(".home-offer .steps")
        check("Add to Home Screen" in steps and "Open as Web App" in steps
              and b.js("return !!document.querySelector('.home-offer .steps svg')"),
              "iPhone: the steps, with Safari's Share icon")
        b.shot("home-screen-iphone")
        b.click_el(b.find_text(".home-offer button", "Done"))
        b.js("localStorage.removeItem('notekpr.homescreen')")

        cdp(b, "Emulation.setUserAgentOverride", {"userAgent": MAC_SAFARI_UA})
        sign_in_page(b)
        check(not b.js(offer), "a Mac isn't offered it")
        cdp(b, "Emulation.setTouchEmulationEnabled", {"enabled": True, "maxTouchPoints": 5})
        sign_in_page(b)
        b.find(".home-offer", 10)
        b.click_el(b.find_text(".home-offer button", "Not now"))
        b.wait("return !document.querySelector('.home-offer')", 5, "the offer goes")
        sign_in_page(b)
        check(not b.js(offer), "an iPad (Safari says it is a Mac, with a touch screen) is offered it; Not now puts it away")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


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
        # Anything that could run as a page of this origin (SVG, HTML) is
        # never shown here nor opened in a tab: it is downloaded.
        catch_opening = """
            window.__opened = []; window.__downloads = [];
            window.open = (u, t, f) => { window.__opened.push([String(u), String(f)]); return null; };
            const click = HTMLAnchorElement.prototype.click;
            HTMLAnchorElement.prototype.click = function () { if (this.download) { window.__downloads.push(this.download); } else { click.call(this); } };
        """
        b.js(catch_opening + """
            const ed = document.querySelector('.ed'); ed.focus(); const s = getSelection(); s.selectAllChildren(ed); s.collapseToEnd();
            const dt = new DataTransfer();
            dt.items.add(new File(['<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>'], 'pic.svg', {type: 'image/svg+xml'}));
            ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """)
        b.wait("return document.querySelector('.ed').textContent.includes('[[pic.svg]]')", 15, "svg embed inserted")
        # A video, pasted on a line of its own, is embedded and plays where it is.
        with open(os.path.join(HERE, "../../scripts/demo-files/lisbon-sunset.webm"), "rb") as f:
            clip = base64.b64encode(f.read()).decode()
        to_end = "const ed = document.querySelector('.ed'); ed.focus(); const s = getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
        b.js(to_end)
        b.keys(KEYS["enter"])
        b.js("""
            const clip = Uint8Array.from(atob(arguments[0]), c => c.charCodeAt(0));
            const dt = new DataTransfer();
            dt.items.add(new File([clip], 'sunset.webm', {type: 'video/webm'}));
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """, clip)
        b.wait("return document.querySelector('.ed').textContent.includes('![[sunset.webm]]')", 15, "video embed inserted")
        check(True, "a pasted video is uploaded and embedded")
        b.js(to_end)
        b.keys(KEYS["enter"] + "- [ ] seen it")
        b.js("document.querySelector('.note-title').focus()")
        b.wait("const v = document.querySelector('.ed .w video[src^=\"blob:\"]'); return !!v && v.readyState >= 1 && Math.round(v.duration) == 6", 15, "editor player")
        check(b.js("return document.querySelector('.ed .w video').controls"), "the editor shows a player for the decrypted video")
        # The player's keys are its own: Ctrl+B there doesn't make the note bold.
        before = editor_text(b)
        check(b.js("const v = document.querySelector('.ed .w video'); v.focus(); return document.activeElement === v"), "a player takes the focus")
        b.chord(KEYS["ctrl"], "b")
        time.sleep(0.3)
        check(editor_text(b) == before, "keys pressed in a player don't edit the note")
        # A file opens on its own page, in the app: what it shows, and the
        # notes using it.
        time.sleep(0.5)
        if "dot.png" not in b.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent).join(' ')"):
            b.click_el(b.find_text(".tree-item", "attachments", 10))
        b.click_el(b.find_text(".tree-item", "dot.png", 10))
        b.wait("const i = document.querySelector('.attachment-page .att-preview img[src^=\"blob:\"]'); return !!i && i.naturalWidth > 0", 15, "the picture's page")
        check(b.js("return document.querySelector('.attachment-page .note-title').value") == "dot.png", "a picture opens on a page of its own, with its name")
        check(b.find_text(".att-used .list-row", "Photos", 10) is not None, "its page lists the notes using it")
        b.click_el(b.find_text(".tree-item", "pic.svg", 10))
        b.wait("return !!document.querySelector('.attachment-page .att-card')", 15, "the SVG's page")
        check(not b.js("return !!document.querySelector('.att-preview img')"), "an SVG isn't shown on its page")
        b.click_el(b.find_text(".att-card button", "Download", 5))
        b.wait("return window.__downloads.includes('pic.svg')", 15, "svg downloaded")
        check(b.js("return window.__opened.length") == 0, "an SVG attachment is downloaded, never opened as a page")
        b.click_el(b.find_text(".tree-item", "sunset.webm", 10))
        b.wait("const v = document.querySelector('.attachment-page video.att-view[src^=\"blob:\"]'); return !!v && v.readyState >= 1 && v.controls", 15, "the video's page")
        check(True, "a video plays on its page")
        b.click_el(b.find_text(".att-used .list-row", "Photos", 10))
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Photos' && !!document.querySelector('.ed')", 15, "back to the note")
        check(True, "Used in opens the note")
        b.click(".seg button[title^='Reading']")
        b.wait("return !!document.querySelector('.reading img.att[src^=\"blob:\"]')", 15, "reading view image")
        check(True, "reading view shows the decrypted image")
        b.wait("const v = document.querySelector('.reading video.att[src^=\"blob:\"]'); return !!v && v.readyState >= 1", 15, "reading view player")
        # A new rendering (a task ticked) keeps the player, playing.
        b.js("window.__v = document.querySelector('.reading video.att'); __v.muted = true; __v.playbackRate = 0.5; __v.play();")
        b.wait("return !__v.paused && __v.currentTime > 0.3", 10, "video playing")
        b.click(".reading input.task")
        b.wait("return document.querySelector('.reading input.task').checked", 10, "task ticked")
        time.sleep(0.5)
        check(b.js("return document.querySelector('.reading video.att') === __v && !__v.paused && __v.currentTime > 0.3"),
              "the reading view's player keeps playing through a new rendering")
        # Opened in the reading view, the note's files are asked for by the (hidden) editor
        # too, at the same time: the reading view gets them from that load.
        b.go(b.js("return location.href").rsplit("/", 1)[0])
        b.find(".tree-item", 20)
        time.sleep(1.5)
        b.click_el(b.find_text(".tree-item", "Photos", 10))
        b.wait("const i = document.querySelector('.reading img.att'), v = document.querySelector('.reading video.att'); return !!i && i.naturalWidth > 0 && !!v && v.readyState >= 1", 20, "reading view after a reload")
        check(True, "a note opened in the reading view shows its picture and video")
        b.js(catch_opening)
        check(not severe_logs(b), "no browser errors")
        # Signing out revokes the decrypted attachments' blob URLs.
        blob = b.js("return document.querySelector('.reading img.att').src")
        def blob_loads():
            b.js("window.__img = null; const i = new Image(); i.onload = () => window.__img = 'ok'; i.onerror = () => window.__img = 'gone'; i.src = arguments[0];", blob)
            b.wait("return window.__img !== null", 10, "image load attempt")
            return b.js("return window.__img") == "ok"
        check(blob_loads(), "the decrypted image is a readable blob while signed in")
        b.js("history.pushState(null, '', '/settings/account'); dispatchEvent(new PopStateEvent('popstate'));")
        b.click(".export-account")
        b.wait("return window.__downloads.some(n => n.startsWith('notekpr ') && n.endsWith('.zip'))", 15, "export downloaded")
        check(True, "account settings export the vaults as a .zip")
        b.wait("return (document.querySelector('.app-version')?.textContent || '').includes('the server:')", 10, "the versions")
        version = api_get(b, "/info")["version"]
        check(b.text(".app-version") == f"This app: {version} · the server: {version}", "account settings say which version the app and the server are")
        b.wait("return !document.querySelector('.toast')", 15, "the export's notices gone")
        b.click_el(b.find_text(".settings-body button", "Sign out", 10))
        b.find(".dev-box input", 15)
        check(not blob_loads(), "signing out revokes decrypted attachment URLs")
    finally:
        b.close()


def test_attachments_beside_notes(env):
    """A note's files go to attachments/ in its folder; moved, it takes along
    those only it uses (they still show), not those another note uses too."""
    b = Browser()
    row = "Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === arguments[0])"
    to_end = "const ed = document.querySelector('.ed'); ed.focus(); const s = getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
    paste_pngs = to_end + """
        const png = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=='), c => c.charCodeAt(0));
        const dt = new DataTransfer();
        for (const name of arguments[0]) dt.items.add(new File([png], name, {type: 'image/png'}));
        ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
    """
    paste_text = to_end + """
        const dt = new DataTransfer();
        dt.setData('text/plain', arguments[0]);
        ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
    """
    def note_in(folder, title):
        b.js(row + ".querySelector('.tree-more').click()", folder)
        b.click_el(b.find_text(".tree-row .menu button", "New note here", 5))
        b.wait("return document.activeElement === document.querySelector('.note-title')", 10, "a new note")
        b.keys(title + KEYS["enter"])
        b.wait("return document.querySelector('.note-title').value === arguments[0]", 10, "its title", title)
        time.sleep(0.4)
    def open_file(name):
        b.click_el(b.find_text(".tree-item", name, 10))
        b.wait("return document.querySelector('.attachment-page .note-title')?.value === arguments[0]", 10, "its page", name)
        return b.js("return document.querySelector('.attachment-page .crumbs').textContent")
    def expand(name, level):
        b.wait("const r = " + row + "; return !!r && r.getAttribute('aria-level') === arguments[1]", 10, f"{name} at level {level}", name, str(level))
        if b.js("return " + row + ".getAttribute('aria-expanded')", name) != "true":
            b.js(row + ".querySelector('.tree-item').click()", name)
    try:
        signup(b, uniq("att"))
        b.click(".sb-actions button[title='New folder']")
        b.type(".modal input", "Trips")
        b.click(".modal button[type=submit]")
        b.wait("return !!" + row, 10, "the folder", "Trips")
        note_in("Trips", "Lisbon")
        b.js(paste_pngs, ["sunset.png", "map.png"])
        b.wait("return document.querySelector('.ed').textContent.includes('![[map.png]]')", 15, "the pictures' links")
        check(editor_text(b) == "![[sunset.png]]\n![[map.png]]", "both are linked, each on a line")
        expand("attachments", 2)
        check(open_file("sunset.png") == "My notes / Trips / attachments", "a file added to a note goes to attachments/ in the note's folder")
        # Porto shows the map too.
        note_in("Trips", "Porto")
        b.js(paste_text, "The same map:\n![[map.png]]")
        b.js("document.querySelector('.note-title').focus()")
        b.wait("return !!document.querySelector('.ed .w img[src^=\"blob:\"]')", 15, "Porto shows the map")
        check(open_file("map.png") == "My notes / Trips / attachments", "the map stays where it was")
        b.find_text(".att-used .list-row", "Porto", 10)
        check(b.js("return Array.from(document.querySelectorAll('.att-used .list-row .ua-title')).map(e => e.textContent)") == ["Lisbon", "Porto"],
              "the file's page says which notes use it")
        b.shot("attachment-page")
        # Lisbon moves to the top: its own picture goes along, the map stays.
        b.click_el(b.find_text(".att-used .list-row", "Lisbon", 10))
        b.wait("return document.querySelector('.note-title')?.value === 'Lisbon' && !!document.querySelector('.ed')", 10, "Lisbon")
        b.click(".note-head .tb-drop button[title='More']")
        b.click_el(b.find_text(".note-head .menu button", "Move to…", 5))
        b.click_el(b.find_text(".modal .list-row", "(vault root)", 5))
        b.find_text(".toast", "with its attachment", 10)
        check(True, "moved, it says its attachment went along")
        check(editor_text(b).count("![[sunset.png]]") == 1 and "![[map.png]]" in editor_text(b), "its links are as they were")
        b.js("document.querySelector('.note-title').focus()")
        b.wait("return document.querySelectorAll('.ed .w img[src^=\"blob:\"]').length === 2", 15, "both pictures")
        check(True, "the note still shows both pictures")
        expand("attachments", 1)
        check(open_file("sunset.png") == "My notes / attachments", "its own picture went along, to attachments/ beside it")
        check(open_file("map.png") == "My notes / Trips / attachments", "the map Porto uses too stayed")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


# Keep what the page downloads (name and bytes, base64) in `__download`.
CAPTURE_DOWNLOADS = """
window.__downloads = []; window.__download = null;
// (The page's policy doesn't let it fetch blob: URLs; the blob is read instead.)
const blobs = new Map();
const create = URL.createObjectURL;
URL.createObjectURL = function (b) { const u = create.call(URL, b); blobs.set(u, b); return u; };
const click = HTMLAnchorElement.prototype.click;
HTMLAnchorElement.prototype.click = function () {
  if (!this.download) return click.call(this);
  const name = this.download;
  window.__downloads.push(name);
  blobs.get(this.href).arrayBuffer().then(b => {
    let s = '';
    new Uint8Array(b).forEach(c => s += String.fromCharCode(c));
    window.__download = [name, btoa(s)];
  });
};
"""

# The next file picker the page opens gets this file (name, base64 bytes).
CHOOSE_FILE = """
const bytes = Uint8Array.from(atob(arguments[1]), c => c.charCodeAt(0));
window.__file = new File([bytes], arguments[0], {type: 'application/zip'});
const click = HTMLInputElement.prototype.click;
HTMLInputElement.prototype.click = function () {
  if (this.type !== 'file' || !window.__file) return click.call(this);
  const dt = new DataTransfer();
  dt.items.add(window.__file);
  this.files = dt.files;
  this.dispatchEvent(new Event('change'));
};
"""

TO_ACCOUNT_SETTINGS = "history.pushState(null, '', '/settings/account'); dispatchEvent(new PopStateEvent('popstate'));"


def test_export_import(env):
    """Moving to another server by export and import: the vaults chosen and
    the quick notes in one .zip, brought back on another account."""
    a, b = Browser(), Browser()
    vaults = "return Array.from(document.querySelectorAll('.export-choice .export-vault')).map(i => i.closest('label').textContent)"
    try:
        signup(a, uniq("ivy"))
        new_note(a, "Packing", "socks and boots")
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Secret")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Secret", 15)
        new_note(a, "Hidden", "not exported")
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        a.chord(KEYS["alt"], "q")
        a.wait("return /^\\d{4}-/.test((document.querySelector('.note-title') || {}).value || '')", 10, "a quick note")
        a.keys("Call mum")
        a.wait("return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.') && localStorage.getItem(k) !== '[]').length === 0", 20, "synced")
        a.js(TO_ACCOUNT_SETTINGS)
        a.find(".export-account", 10)
        check(a.js("return document.querySelectorAll('.export-choice input:checked').length") == 3,
              "every vault and the quick notes are ticked for export")
        a.js("Array.from(document.querySelectorAll('.export-choice label')).find(l => l.textContent.includes('Secret')).querySelector('input').click()")
        a.js(CAPTURE_DOWNLOADS)
        a.click(".export-account")
        a.wait("return window.__download !== null", 20, "the export")
        name, data = a.js("return window.__download")
        check(name.startswith("notekpr ") and name.endswith(".zip"), "the vaults chosen and the quick notes go into one .zip")
        a.shot("export-choice")

        # Another account — as on another server — imports it.
        signup(b, uniq("ivo"))
        b.js(TO_ACCOUNT_SETTINGS)
        b.find(".import-account", 10)
        b.js(CHOOSE_FILE, name, data)
        b.click(".import-account")
        b.answer(timeout=20)
        b.find_text(".toast", "Imported 1 vault", 30)
        b.wait("return Array.from(document.querySelectorAll('.export-choice .export-vault')).length === 2", 10, "the new vault listed")
        names = b.js(vaults)
        check(any("My notes 2" in n for n in names) and not any("Secret" in n for n in names),
              "the vault chosen comes back (a name already taken gets a number), the other doesn't")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "My notes 2"))
        b.click_el(b.find_text(".tree-item", "Packing", 15))
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('socks and boots')", 15, "the note")
        check(True, "with its notes")
        b.click(".sb-quick .sb-link")
        b.wait("return Array.from(document.querySelectorAll('.qn-card .nc-snip')).some(e => e.textContent.includes('Call mum'))", 15, "the quick note")
        check(True, "the quick notes join that account's")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def notion_export():
    """A database exported from Notion as HTML, laid out and written as
    Notion does: its page, its CSV, a row with a database of its own."""
    import io, zipfile
    def uid(n):
        h = f"{n:032x}"
        return f"{h[:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:]}"
    def page(n, icon, title, props, body):
        icon_attr = f' data-notion-page-icon="{icon}"' if icon else ""
        return (f'<html><head><meta charset="utf-8"/><style>p {{ x: 1 }}</style></head><body>'
                f'<article id="{uid(n)}" class="page sans"{icon_attr}><header><h1 class="page-title">{title}</h1>'
                f'<table class="properties"><tbody>{props}</tbody></table></header>'
                f'<div class="page-body">{body}</div></article></body></html>')
    def prop(kind, name, cell):
        return f'<tr class="property-row property-row-{kind}"><th>{name}</th><td>{cell}</td></tr>'
    def rows(columns, items):
        head = "".join(f"<th>{c}</th>" for c in columns)
        body = "".join(f'<tr id="{uid(n)}"><td class="cell-title"><a href="x.html" data-notion-page-id="{uid(n)}">{t}</a></td></tr>'
                       for n, t in items)
        return f'<table class="collection-content"><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>'
    def status(name, color):
        return prop("status", "Status", f'<span class="status-value select-value-color-{color}"><div class="status-dot"></div>{name}</span>')
    entry = ('<h2>To do</h2>'
             f'<div id="{uid(5)}" class="collection-content"><h4 class="collection-title">Today</h4>'
             f'{rows(["Name"], [(6, "Write tests"), (7, "Buy milk")])}</div>'
             f'<p>Planned in <a href="https://app.notion.com/p/Week-18-{9:032x}" data-notion-page-id="{uid(9)}">Week 18</a>.</p>')
    files = {
        f"Journal {1:032x}.html": page(1, "📓", "Journal", "", f'<div class="collection-content-wrapper">{rows(["Name"], [(2, "@May 2, 2023")])}</div>'),
        f"Journal {1:032x}.csv": "﻿Name,Tags\n\"@May 2, 2023 \",Daily\n",
        f"Journal/@May 2, 2023 {2:032x}.html": page(2, "🤖", '<time datetime="2023-05-02">@May 2, 2023</time> ',
                                                      prop("multi_select", "Tags", '<span class="selected-value">Daily</span>'), entry),
        f"Journal/@May 2, 2023/Today {5:032x}.csv": "Name,Status\nWrite tests,Done\nBuy milk,Not started\n",
        f"Journal/@May 2, 2023/Today/Write tests {6:032x}.html": page(6, "", "Write tests", status("Done", "green"), "<p>Run them twice</p>"),
        f"Journal/@May 2, 2023/Today/Buy milk {7:032x}.html": page(7, "", "Buy milk", status("Not started", "default"), ""),
    }
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, text in files.items():
            z.writestr(name, text)
    return base64.b64encode(buf.getvalue()).decode()


def test_notion_import(env):
    """Importing from Notion: what an export holds is shown before it comes
    in, into a folder of its own; the same export again brings nothing new."""
    b = Browser()
    items = "return Array.from(document.querySelectorAll('.notion-item')).map(l => l.textContent)"
    try:
        signup(b, uniq("nia"))
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "Imports")
        b.click(".modal button[type=submit]")
        b.find_text(".page-head h1", "Imports", 15)
        home = b.js("return location.pathname")
        b.js(CHOOSE_FILE, "Export.zip", notion_export())
        b.click_el(b.find_text(".qa", "Import notes"))
        b.click(".notion-pick")
        b.find(".notion-item", 15)
        listed = b.js(items)
        check(len(listed) == 1 and "Journal" in listed[0] and "1 row" in listed[0], "what the export holds is listed first, its rows counted")
        check(b.js("return document.querySelector('.modal b').textContent") == "Notion/", "it goes into a folder of its own")
        b.shot("notion-import")
        b.click(".notion-import")
        b.find_text(".toast", "from Notion into Notion", 30)
        b.wait("return (document.querySelector('.note-title') || {}).value === 'Imported from Notion'", 15, "the report")
        check("Week 18" in editor_text(b), "then the report opens, naming the page linked to but not exported")
        b.click_el(b.find_text(".tree-item", "Notion", 15))
        b.find_text(".tree-item", "Journal", 15)
        check(b.js("return Array.from(document.querySelectorAll('.tree-row')).some(r => r.textContent.includes('📓') && r.textContent.includes('Journal'))"),
              "the database is a folder, with its icon")
        b.click_el(b.find_text(".tree-item", "Journal", 15))
        b.click_el(b.find_text(".tree-item", "2023-05-02", 15))
        b.wait("return (document.querySelector('.note-title') || {}).value === '2023-05-02'", 15, "the row")
        text = editor_text(b)
        check("Write tests" in text and "Buy milk" in text, "a row is a note named after its day, its tasks a checklist")
        check("Run them twice" in text, "a task's own text is in the day's note, under it")
        check("[[Week 18]]" in text, "with a link that waits for the page it names")

        # The same export again: what came already isn't imported again.
        b.js("history.pushState(null, '', arguments[0]); dispatchEvent(new PopStateEvent('popstate'));", home)
        b.click_el(b.find_text(".qa", "Import notes", 15))
        b.click(".notion-pick")
        b.find(".notion-item", 15)
        check("already here" in b.js(items)[0] and not b.js("return document.querySelector('.notion-item input').checked"),
              "importing it again, what came already is unticked")
        check(b.js("return document.querySelector('.notion-import').disabled"), "and there's nothing to import")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_delete_account(env):
    """Deleting one's account: what goes is said first; signing in again
    starts a new, empty account."""
    b = Browser()
    try:
        name = uniq("del")
        signup(b, name)
        new_note(b, "Last words", "goodbye")
        b.js(TO_ACCOUNT_SETTINGS)
        b.click(".delete-account")
        b.find(".delete-what", 10)
        check("My notes" in b.js("return document.querySelector('.delete-what').textContent"), "the dialog says which vaults go")
        check(b.js("return document.querySelector('.delete-account-now').disabled"), "nothing goes before \"delete\" is typed")
        b.type(".delete-confirm", "delete")
        b.shot("delete-account")
        b.click(".delete-account-now")
        b.find(".dev-box input", 15)
        check("Your account was deleted" in b.js("return document.body.innerText"), "then it is signed out, saying so")
        b.type(".dev-box input", name)
        b.click(".dev-box button[type=submit]")
        b.find_text(".auth-card button", "Create my keys", 15)
        check(True, "signing in again starts a new account")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_move_to_vault(env):
    b = Browser()
    try:
        signup(b, uniq("mia"))
        new_note(b, "Trip photos", "Beach:" + KEYS["enter"])
        b.js("""
            const png = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg=='), c => c.charCodeAt(0));
            const dt = new DataTransfer();
            dt.items.add(new File([png], 'dot.png', {type: 'image/png'}));
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));
        """)
        b.wait("return document.querySelector('.ed').textContent.includes('![[dot.png]]')", 15, "embed inserted")
        time.sleep(0.6)
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "Archive")
        b.click(".modal button[type=submit]")
        b.find_text(".page-head h1", "Archive", 15)
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "My notes", 10))
        b.find_text(".page-head h1", "My notes", 15)
        # Any note, from its menu in the tree: not only quick notes.
        row = "Array.from(document.querySelectorAll('.tree-row')).find(r => r.textContent.trim() === arguments[0])"
        b.js(row + ".querySelector('.tree-more').click()", "Trip photos")
        b.click_el(b.find_text(".tree-row .menu button", "Move to vault", 5))
        b.find(".modal .mtv-name", 5)
        time.sleep(0.3)
        check(b.js("return document.querySelector('.mtv-name').value") == "Trip photos"
              and b.js("return Array.from(document.querySelectorAll('.mtv-vault option')).map(o => o.textContent)") == ["Archive"],
              "a vault's note is offered the other vaults, keeping its name")
        b.click(".mtv-folder")
        b.keys("2026")
        b.click(".modal button[type=submit]")
        b.wait("return !document.querySelector('.modal') && !Array.from(document.querySelectorAll('.tree-item .name')).some(e => e.textContent === 'Trip photos')",
               15, "the note leaves the vault")
        check(True, "a vault's note moves to another vault from the tree")
        b.click_el(b.find_text(".sb-link", "Trash"))
        b.find_text(".list-row", "Trip photos", 10)
        check(b.find_text(".list-row", "dot.png", 5) is not None, "the original and the picture only it used go to the trash")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "Archive", 10))
        b.click_el(b.find_text(".tree-item", "2026", 10))
        b.click_el(b.find_text(".tree-item", "Trip photos", 10))
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Trip photos'", 10, "the moved note")
        check(b.js("return document.querySelector('.crumbs').textContent") == "Archive / 2026" and editor_text(b) == "Beach:\n![[dot.png]]",
              "it is in the folder picked, with its text")
        b.wait("return !!document.querySelector('.ed .w img[src^=\"blob:\"]')", 15, "image preview")
        # In attachments/ beside it there.
        b.click_el(b.find_text(".tree-item", "attachments", 10))
        b.find_text(".tree-item", "dot.png", 10)
        check("dot.png" in b.js("return Array.from(document.querySelectorAll('.tree-item .name')).map(e=>e.textContent).join(' ')"),
              "the picture it links to comes along and opens")
        # Attachments stay in their vault: only notes are offered the move.
        b.js(row + ".querySelector('.tree-more').click()", "dot.png")
        b.find(".tree-row .menu button", 5)
        check("Move to vault" not in b.js("return document.querySelector('.tree-row .menu').textContent"), "an attachment isn't offered to move to a vault")
        b.chord(KEYS["escape"])
        # A folder, with everything in it, from the note open in it.
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "My notes", 10))
        b.find_text(".page-head h1", "My notes", 15)
        b.click(".sb-actions button[title='New folder']")
        b.type(".modal input", "Recipes")
        b.click(".modal button[type=submit]")
        b.wait("return !!" + row, 10, "the folder", "Recipes")
        b.js(row + ".querySelector('.tree-more').click()", "Recipes")
        b.click_el(b.find_text(".tree-row .menu button", "New note here", 5))
        b.find(".note-title", 10)
        time.sleep(0.4)
        b.keys("Soup" + KEYS["enter"])
        time.sleep(0.4)
        b.keys("leeks and potatoes")
        time.sleep(0.6)
        b.js(row + ".querySelector('.tree-more').click()", "Recipes")
        b.click_el(b.find_text(".tree-row .menu button", "Move to vault", 5))
        b.find(".modal .mtv-name", 5)
        time.sleep(0.3)
        check(b.js("return document.querySelector('.mtv-name').value") == "Recipes" and "Everything in it moves" in b.text(".modal"),
              "a folder is offered the move too, keeping its name")
        # The folders there: a list right under the field, as wide as it.
        menu = "document.querySelector('.suggest .menu')"
        b.click(".suggest-toggle")
        b.find(".suggest .menu", 5)
        placed = b.js("const f = document.querySelector('.mtv-folder').getBoundingClientRect(), m = " + menu + ".getBoundingClientRect();"
                      "return Math.abs(m.left - f.left) < 1 && Math.abs(m.right - f.right) < 1 && m.top >= f.bottom && m.top - f.bottom < 8;")
        check(placed and b.js("return Array.from(document.querySelectorAll('.suggest [role=option]')).map(o => o.textContent)") == ["2026", "2026/attachments"],
              "the other vault's folders open in a list under the folder field")
        b.keys(KEYS["down"] + KEYS["enter"])
        check(b.js("return document.querySelector('.mtv-folder').value") == "2026" and b.js("return !" + menu),
              "a folder is picked from the list with the keys")
        b.js("const i=document.querySelector('.mtv-folder'); i.focus(); i.select();")
        b.keys("zz")
        check(b.js("return !" + menu), "typing narrows the list (nothing matches: no list)")
        b.keys(KEYS["backspace"] + KEYS["backspace"])
        b.find(".suggest .menu", 5)
        b.chord(KEYS["escape"])
        check(b.js("return !" + menu + " && !!document.querySelector('.modal')"), "Escape closes the list, not the dialog")
        b.click(".modal button[type=submit]")
        b.wait("return !document.querySelector('.modal') && document.querySelector('.crumbs') && document.querySelector('.crumbs').textContent === 'Archive / Recipes'",
               20, "the moved folder's note")
        check(b.js("return document.querySelector('.note-title').value") == "Soup" and editor_text(b) == "leeks and potatoes",
              "a folder moves with its notes, and the note open in it is followed there")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "My notes", 10))
        b.find_text(".page-head h1", "My notes", 15)
        time.sleep(0.5)
        check(not b.js("return !!" + row, "Recipes"), "the folder leaves the vault")
        check(not severe_logs(b), "no browser errors")
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


API_PATCH = """
window.__api = undefined;
fetch('/api/v1' + arguments[0], {method: 'PATCH', body: JSON.stringify(arguments[1]),
  headers: {'Content-Type': 'application/json', Authorization: 'Bearer ' + sessionStorage.getItem('notekpr.token')}})
  .then(r => { window.__api = r.status; }, () => { window.__api = 0; });
"""


def api_patch(b, path, body):
    """PATCH an API path with the browser's session; the status (0 on error)."""
    b.js(API_PATCH, path, body)
    b.wait("return window.__api !== undefined", 10, "API answer")
    return b.js("return window.__api")


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
        a.click(".people .person .icon-btn")
        a.answer()
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
            const ind = lc.parentElement.querySelector('.ind');
            const i = ind.getBoundingClientRect();
            return [tops.size, lefts, [guide.left, guide.right], [i.left, i.width, ind.textContent.length]];
        """
        n, lefts, guide, ind = b.js(rows)
        check(n > 1, f"a long line wraps by default ({n} rows)")
        check(len(lefts) == 1, f"wrapped rows start under the text, not under the indentation ({lefts})")
        # Measured against the code's own font: a guide drawn with another
        # width of a column (Chromium 154 took `ch` as 0.5em) drifts.
        column = ind[1] / ind[2]
        check(abs(guide[0] - (ind[0] + column / 2)) <= 1 and abs(guide[1] - lefts[0]) <= 1,
              f"the indentation guide is drawn half a column into the indentation and ends where the text starts ({guide}, {ind}, {lefts})")
        b.shot("code-wrapping")
        # wrap=false: the block scrolls sideways, its lines together.
        # Ctrl+Home reaches the fence even while it is hidden behind the header.
        b.chord(KEYS["ctrl"], KEYS["home"])
        b.chord(KEYS["end"])
        b.keys(" wrap=false")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.6)
        check(editor_text(b).startswith("```py wrap=false\n"), "Ctrl+Home goes to the fence: wrap=false after the language")
        n, lefts, guide, ind = b.js(rows)
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


def test_vim(env):
    b = Browser(1100, 700)
    try:
        signup(b, uniq("vim"))
        new_note(b, "Before", "the note before")
        new_note(b, "Long", KEYS["enter"].join(f"line {i}" for i in range(60)))
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.find(".vim-toggle", 10)
        b.js("document.querySelector('.vim-toggle').scrollIntoView({block: 'center'})")
        b.click_el(b.find(".vim-toggle", 10))
        b.js("history.back()")
        b.find(".ed.vim-normal", 10)
        b.click_el(b.find(".ed > .ln:nth-child(3)", 5))
        time.sleep(0.3)
        b.wait("return !!document.querySelector('.vim-cursor')", 5, "a block cursor")
        check(True, "normal mode shows a block cursor")
        # Where the caret is, and where the toolbar covering the text ends.
        where = """
            const r = getSelection().getRangeAt(0).getBoundingClientRect();
            const tb = document.querySelector('.toolbar-wrap').getBoundingClientRect();
            return [r.top, r.bottom, tb.bottom, innerHeight, document.querySelector('.main').scrollTop];
        """
        b.keys("G")
        time.sleep(0.4)
        top, bottom, covered, vh, _ = b.js(where)
        check(top >= covered and bottom <= vh, f"G: the last line shows ({top}, {bottom}, {covered}, {vh})")
        # Going up line by line, the caret never hides under the toolbar.
        hidden = []
        for _ in range(40):
            b.keys("k")
            time.sleep(0.05)
            top, _, covered, _, _ = b.js(where)
            if top < covered:
                hidden.append(round(covered - top))
        check(not hidden, f"k: the caret stays below the toolbar ({hidden})")
        b.keys("gg")
        time.sleep(0.4)
        check(b.js(where)[4] == 0, "gg: up to the top, where the title shows")
        b.chord(KEYS["ctrl"], "d")
        time.sleep(0.4)
        check(b.js(where)[4] > 0, "Ctrl+D scrolls down")
        # Ctrl+O: back through the jumps, then to the note before.
        for _ in range(6):
            if b.js("return document.querySelector('.note-title')?.value") == "Before":
                break
            b.chord(KEYS["ctrl"], "o")
            time.sleep(0.4)
        check(b.js("return document.querySelector('.note-title')?.value") == "Before", "Ctrl+O ends in the note before")
        check(not b.js("return !!document.querySelector('.modal')"), "Ctrl+O in normal mode doesn't open the quick switcher")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_line_numbers(env):
    b = Browser(1100, 800)
    try:
        signup(b, uniq("lnum"))
        body = KEYS["enter"].join(["first", "## A heading", "", "a paragraph long enough to wrap " * 4, "last"])
        new_note(b, "Numbered", body)
        left = "return document.querySelector('.ed > .ln').getBoundingClientRect().left"
        before = b.js(left)
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.find(".line-numbers-toggle", 10)
        b.js("document.querySelector('.line-numbers-toggle').scrollIntoView({block: 'center'})")
        b.click_el(b.find(".line-numbers-toggle", 10))
        b.wait("return document.documentElement.classList.contains('line-numbers')", 5, "line numbers on")
        b.js("history.back()")
        b.find(".ed .ln", 10)
        time.sleep(0.4)
        check(b.js("return getComputedStyle(document.querySelector('.ed > .ln'), '::after').content") == "counter(ln)", "each line gets its number")
        check(abs(b.js(left) - before) < 1, "the text stays where it was: the numbers go in the margin")
        # A table's hidden delimiter row and a code block's folded fences
        # still count: the line after them has its own number.
        b.click_el(b.find(".ed > .ln:last-child", 5))
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.js("""const dt = new DataTransfer();
            dt.setData('text/plain', '\\n| a | b |\\n| --- | --- |\\n| 1 | 2 |\\n\\n```py\\nx = 1\\n```\\nend');
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));""")
        time.sleep(0.4)
        b.click_el(b.find(".ed > .ln:first-child", 5))
        time.sleep(0.4)
        b.shot("line-numbers")
        n = b.js("""const lines = [...document.querySelectorAll('.ed > .ln')];
            let n = 0;
            for (const l of lines) {
                const cs = getComputedStyle(l);
                if (cs.display == 'none') continue;
                const inc = cs.counterIncrement.match(/\\bln (\\d+)/);
                n += inc ? +inc[1] : 0;
            }
            return [n, lines.length];""")
        check(n[0] == n[1], f"every line of the Markdown counts once ({n})")
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.keys(KEYS["enter"] + "last")
        time.sleep(0.4)
        # Typing goes on as before: the numbers aren't text.
        b.click_el(b.find(".ed > .ln:last-child", 5))
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.keys(" line")
        time.sleep(0.6)
        check(editor_text(b).endswith("last line"), f"typing is unaffected ({editor_text(b)!r})")
        # Relative: counted from the caret's line, which keeps its own.
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.find(".relative-numbers-toggle", 10)
        b.js("document.querySelector('.relative-numbers-toggle').scrollIntoView({block: 'center'})")
        b.click_el(b.find(".relative-numbers-toggle", 10))
        b.wait("return document.documentElement.classList.contains('relative-numbers')", 5, "relative numbers on")
        b.js("history.back()")
        b.click_el(b.find(".ed > .ln:nth-child(4)", 10))
        time.sleep(0.4)
        shown = b.js("""const ed = document.querySelector('.ed');
            return [ed.style.getPropertyValue('--ln-cur'),
                    [...ed.children].map(l => getComputedStyle(l, '::after').content)];""")
        check(shown[0] == "4", f"the editor says the caret is on line 4 ({shown[0]})")
        check(shown[1][:5] == ["counter(up)"] * 3 + ["counter(ln)", "counter(down)"],
              f"above the caret's line counted down to it, below it counted on ({shown[1][:5]})")
        b.shot("relative-numbers")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_bullets(env):
    b = Browser(1000, 800)
    try:
        signup(b, uniq("bul"))
        new_note(b, "Nested", "")
        b.js("""const dt = new DataTransfer();
            dt.setData('text/plain', '- one\\n  - two\\n    - three\\n      - four\\n\\n  - after a blank line\\n- back out\\n\\ttabbed\\n- [ ] a task\\n1. numbered\\n   - under it');
            document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));""")
        time.sleep(0.4)
        b.click_el(b.find(".note-title", 5))
        time.sleep(0.4)
        # Each bullet's look (what draws it); null where a line has none.
        looks = b.js("""return [...document.querySelectorAll('.ed > .ln')].map(l => {
            const bl = l.querySelector('.bl');
            return bl ? getComputedStyle(bl, '::before').backgroundImage : null;
        });""")
        dot, ring, square = looks[:3]
        check(len({dot, ring, square}) == 3 and looks[3] == square, "nested bullets: a dot, a ring, then a square further in")
        check(looks[5:7] == [ring, dot], "a blank line keeps the list's levels")
        check(looks[-1] == ring, "a bullet under a numbered item is one level in")
        b.shot("bullets")
        b.chord(KEYS["ctrl"], "e")
        b.find(".reading ul", 10)
        b.shot("bullets-reading")
        kinds = b.js("return [...document.querySelectorAll('.reading ul ul li, .reading > ul > li')].map(l => getComputedStyle(l).listStyleType)")
        check({"disc", "circle", "square"} <= set(kinds), f"the reading view's bullets take turns too ({kinds})")
        b.chord(KEYS["ctrl"], "e")
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
        a.click(".people .person .icon-btn[title^='Remove access']")
        a.answer()
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
        before = editor_text(b)
        b.chord(KEYS["ctrl"], "/")
        # Keys typed right away go to the dialog, not to the note behind it.
        b.keys(KEYS["tab"] * 12)
        b.find(".modal[aria-modal=true]", 5)
        check(True, "Ctrl+/ opens the shortcut help from the editor")
        check(b.js("return document.querySelector('.modal').contains(document.activeElement)"), "Tab stays inside the dialog")
        check(editor_text(b) == before, "keys typed as it opens don't reach the note")
        b.chord(KEYS["escape"])
        time.sleep(0.3)
        check(b.js("return !!document.activeElement.closest('.ed')"), "closing the dialog gives the focus back")
        # Tab in the title goes on to the text, past the toolbar; Shift+Tab still goes back.
        b.js("const t=document.querySelector('.note-title'); t.focus(); t.select();")
        b.keys("Agenda" + KEYS["tab"])
        time.sleep(0.3)
        check(b.js("return !!document.activeElement.closest('.ed')") and b.js("return document.querySelector('.note-title').value") == "Agenda",
              "Tab in the title moves on to the text")
        b.js("document.querySelector('.note-title').focus()")
        b.chord(KEYS["shift"], KEYS["tab"])
        check(b.js("return document.activeElement.classList.contains('note-icon')"), "Shift+Tab in the title goes back to the icon")
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
                     ".map(x => x.classList.contains('sb-quick') ? 'quick' : x.classList.contains('sb-drawings') ? 'drawings'"
                     " : x.querySelector('.sb-label')?.textContent).slice(0, 3)")
        check(order == ["quick", "drawings", "Favorites"], f"Favorites come below the Quick notes and Drawings entries ({order})")
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
        b.find_text(".toast", "Sending changes saved while offline", 30)
        check(True, "taking over changes saved while offline says so")
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
        b.click(".offline-toggle")
        b.answer(yes=False)
        b.wait("return !document.querySelector('.offline-toggle').checked && !document.querySelector('.offline-toggle').disabled", 5, "the toggle back off")
        check(not any(k.startswith("notekpr.offline.") for k in b.js("return Object.keys(localStorage)")), "cancelling leaves offline use off")
        b.click(".offline-toggle")
        b.answer()
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
    cards = "return Array.from(document.querySelectorAll('.qn-card .nc-snip')).map(e => e.textContent)"
    try:
        signup(b, uniq("quinn"))
        # The shortcut, even from inside the editor: a new quick note, ready to type.
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.chord(KEYS["alt"], "q")
        b.wait("return /^\\d{4}-\\d\\d-\\d\\d \\d\\d\\.\\d\\d$/.test((document.querySelector('.note-title') || {}).value || '')", 10, "a quick note opens")
        b.wait("return !!document.activeElement.closest('.ed')", 5, "the caret in its editor")
        check(True, "Alt+Q creates a quick note named by the time and puts the caret in its editor")
        check(b.js("return document.querySelector('.crumbs').textContent") == "Quick notes", "it is in the quick notes, not in a vault")
        b.keys("Call the dentist")
        time.sleep(0.8)
        check(editor_text(b) == "Call the dentist", "typing goes into the quick note")
        # Never in a vault: the sidebar keeps the vault's tree, without them.
        check(b.js("return document.querySelector('.vault-btn .vname').textContent") == "My notes"
              and not b.js("return Array.from(document.querySelectorAll('.tree-row')).some(r => r.textContent.includes('Quick notes'))"),
              "the sidebar keeps the vault and its tree, with no Quick notes folder")
        b.click(".vault-btn")
        check("Quick notes" not in b.js("return document.querySelector('.vault-menu, .menu').textContent"),
              "the vault switcher doesn't list them")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "quick notes")
        b.click(".modal button[type=submit]")
        b.find_text(".toast", "is the name of your quick notes", 10)
        check(b.js("return !!document.querySelector('.modal input')"), "no vault may be named like them")
        b.chord(KEYS["escape"])
        # Not shared, no comments; they leave by moving to a vault or being deleted.
        check(not b.js("return !!document.querySelector('.note-head button[aria-label=\"Share this note\"]')")
              and not b.js("return !!document.querySelector('.note-head button[aria-label=\"Comments\"]')"),
              "a quick note can't be shared and has no comments")
        check(b.js("return document.querySelectorAll('.sb-quick .sb-link').length") == 1,
              "the Quick notes entry in the sidebar doesn't list the notes")
        # The overview page: capture several thoughts without leaving it.
        b.click(".sb-quick .sb-link")
        b.find(".quick-capture textarea", 10)
        check(b.js("return location.pathname") == "/quick", "the Quick notes entry opens /quick")
        check(b.js("return !!document.querySelector('.sb-drawings .sb-link')"),
              "the sidebar keeps the vault's Drawings entry on the Quick notes page")
        check(b.js("return document.querySelector('.qn-head .qn-count').textContent") == "1 of 20", "the list's heading counts them against the limit")
        b.click(".quick-capture textarea")
        b.keys("Garden plan" + KEYS["enter"] + "tomatoes and beans")
        b.chord(KEYS["ctrl"], KEYS["enter"])
        b.wait("return document.querySelector('.quick-capture textarea').value === ''", 5, "the box is cleared")
        b.wait(cards + ".join('|') === 'Garden plan tomatoes and beans|Call the dentist'", 10, "two cards, newest first")
        check(b.js("return location.pathname") == "/quick", "Ctrl+Enter saves a quick note without leaving the page and clears the box")
        check(b.js("return Array.from(document.querySelectorAll('.qn-card .nc-title')).every(t => /^\\d{4}-\\d\\d-\\d\\d \\d\\d\\.\\d\\d/.test(t.textContent))"),
              "cards are a vault's note cards: the note's name, then the start of its text")
        b.keys("Old thought")
        b.click(".quick-capture button[type=submit]")
        b.wait(cards + ".join('|') === 'Old thought|Garden plan tomatoes and beans|Call the dentist'", 10, "three cards")
        b.wait("return document.activeElement === document.querySelector('.quick-capture textarea')", 5, "focus back in the box")
        check(True, "the Save button saves too and leaves the focus in the box")
        clash = b.js("""return Array.from(document.querySelectorAll('.qn-card')).filter(c => {
            const meta = c.querySelector('.nc-meta'), r = document.createRange();
            r.selectNodeContents(meta);
            const t = r.getBoundingClientRect(), a = c.querySelector('.qn-actions').getBoundingClientRect();
            const overlap = t.right > a.left && t.bottom > a.top && t.top < a.bottom;
            return overlap || meta.querySelector('span').getClientRects().length != 1;
        }).length""")
        b.shot("quick-notes-cards")
        check(clash == 0, "on narrow cards the time stays on one line, clear of the buttons")
        # A card opens its note.
        b.click_el(b.find_text(".qn-card .note-card", "Garden plan"))
        b.wait("return !!document.querySelector('.note-title') && document.querySelector('.note-title').value !== ''", 10, "the card opens its note")
        check(editor_text(b) == "Garden plan\ntomatoes and beans", "a card opens its note")
        b.js("history.back()")
        b.find(".quick-capture", 10)
        # Move to a vault: a name (default: the first line), the vault and a folder.
        b.js("Array.from(document.querySelectorAll('.qn-card')).find(c => c.textContent.includes('Garden plan')).querySelector('.qn-move').click()")
        b.find(".modal .mtv-name", 5)
        time.sleep(0.3)
        check(b.js("return document.querySelector('.mtv-name').value") == "Garden plan", "moving suggests the first line as the name")
        check(b.js("return Array.from(document.querySelectorAll('.mtv-vault option')).map(o => o.textContent)") == ["My notes"],
              "the vaults one can write to are offered")
        b.js("const i=document.querySelector('.mtv-name'); i.focus(); i.select();")
        b.keys("Vegetable garden")
        b.click(".mtv-folder")
        b.keys("Projects")
        b.click(".modal button[type=submit]")
        b.wait(cards + ".join('|') === 'Old thought|Call the dentist'", 15, "the moved note leaves the quick notes")
        check(not b.js("return !!document.querySelector('.modal')"), "moving closes the dialog")
        check(b.js("return document.querySelector('.qn-count').textContent") == "2 of 20", "and makes room")
        b.click_el(b.find_text(".tree-item", "Projects", 10))
        b.click_el(b.find_text(".tree-item", "Vegetable garden", 10))
        b.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Vegetable garden'", 10, "the moved note")
        check(b.js("return Array.from(document.querySelectorAll('.tree-row.folder')).find(r => r.textContent.trim() === 'Projects').getAttribute('aria-expanded')") == "true"
              and b.js("return document.querySelector('.tree-row.active').textContent.trim()") == "Vegetable garden",
              "a folder stays open when a note in it is opened")
        check(b.js("return document.querySelector('.crumbs').textContent") == "My notes / Projects" and editor_text(b) == "Garden plan\ntomatoes and beans",
              "moving makes it a note of the vault, keeping its text")
        # Deleted for good: no trash.
        b.click(".sb-quick .sb-link")
        b.find(".qn-card", 10)
        b.js("Array.from(document.querySelectorAll('.qn-card')).find(c => c.textContent.includes('Call the dentist')).querySelector('.qn-delete').click()")
        b.answer()
        b.wait(cards + ".join('|') === 'Old thought'", 10, "deleted")
        check(True, "deleting a quick note takes it away for good")
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
        # Loaded right after signing up: what that page hadn't sent yet is
        # sent without claiming anyone was offline.
        time.sleep(1)
        check(not m.js("return Array.from(document.querySelectorAll('.toast')).some(t => t.textContent.includes('offline'))"),
              "a page taking over changes an online page hadn't sent doesn't say \"offline\"")
        m.click(".quick-capture textarea")
        m.keys("Idea on the go")
        m.click(".quick-capture button[type=submit]")
        m.wait(cards + ".join('|') === 'Idea on the go'", 10, "the card on a phone")
        fits = m.js("""const w = innerWidth, r = e => e.getBoundingClientRect();
            const els = [document.querySelector('.quick-capture'), document.querySelector('.quick-capture .btn'),
                         document.querySelector('.qn-card'), document.querySelector('.qn-card .qn-move'),
                         document.querySelector('.qn-card .qn-delete')];
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


def test_vault_cards(env):
    b = Browser()
    try:
        signup(b, uniq("card"))
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "All vaults", 10))
        card = b.find(".card-wrap", 10)
        b.click_el(b.find(".card-wrap .card-cog", 5))
        b.find_text(".modal", "Vault settings", 5)
        check(b.js("return document.querySelector('.modal input')?.value") == "My notes", "the cog opens that vault's settings")
        b.chord(KEYS["escape"])
        b.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        b.click_el(b.find(".card-wrap .card", 5))
        b.wait("return location.pathname.startsWith('/v/')", 10, "the vault opens")
        check(True, "the rest of the card still opens the vault")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


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
        # WebDriver can't type characters outside the BMP: set the box's value,
        # as pasting it does.
        a.js("const i = document.querySelector('.emoji-search'); i.value = '🦊'; i.dispatchEvent(new Event('input'))")
        a.click(".emoji-use")
        a.wait("return " + btn_icon + " === '🦊'", 10, "the pasted emoji")
        check(True, "any emoji can be pasted or typed into the search box instead")
        # A picture: made a small square from the image, shown instead of the emoji.
        text = os.path.join(env.data, "not-an-image.png")
        with open(text, "w") as f:
            f.write("hello")
        a.type(".picture-input", text)
        a.find_text(".toast.error", "isn't an image", 10)
        check(a.js("return " + btn_icon) == "🦊", "a file that isn't an image is refused with a message")
        pic = os.path.join(env.data, "wide.png")
        png_file(pic, 300, 200)
        a.type(".picture-input", pic)
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
    def row_pic(br, name):
        return br.js("const i = " + row + "[0].querySelector('img.entry-pic'); return !!i && i.complete && i.naturalWidth > 0", name)
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
        # WebDriver can't type characters outside the BMP: set the box's
        # value, as pasting it does.
        a.js("const i = document.querySelector('.modal .emoji-search'); i.value = '🧳'; i.dispatchEvent(new Event('input'))")
        a.click(".modal .emoji-use")
        a.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        check(row_icon(a, "Travel") == "🧳", "a folder gets an icon, any emoji can be pasted")
        # A note made by following a link to one that doesn't exist yet: its
        # folder is only there because the note is in it.
        new_note(a, "Links", "[[Ideas/Spark]]")
        a.chord(KEYS["ctrl"], "e")
        a.click("article.reading a.wl[data-create]")
        a.wait("return document.querySelector('.note-title') && document.querySelector('.note-title').value === 'Spark'", 10, "the new note")
        a.chord(KEYS["ctrl"], "e")
        a.wait("return " + row + ".length === 1", 10, "the Ideas folder", "Ideas")
        set_icon(a, "Ideas", "💡")
        check(row_icon(a, "Ideas") == "💡" and a.js("return " + row + ".length", "Ideas") == 1,
              "so does a folder that only held notes, without showing twice")
        # Every emoji, by group, the recent ones first; found by name or
        # keyword, Enter takes the first found.
        a.js(row + "[0].querySelector('.tree-more').click()", "Ideas")
        a.click_el(a.find_text(".tree-row .menu button", "Set icon", 5))
        a.find(".modal .emoji-search", 5)
        a.wait("return document.activeElement === document.querySelector('.modal .emoji-search')", 5, "the focus")
        n = a.js("return document.querySelectorAll('.modal .emoji-scroll .emoji-btn').length")
        check(n > 1800, f"every emoji is offered, by group ({n})")
        recent = a.js("return Array.from(document.querySelectorAll('.modal .emoji-recent .emoji-btn')).map(b => b.dataset.e)")
        check(recent == ["💡", "🧳", "🌱"], f"the recent ones first, the search box focused ({recent})")
        a.shot("icon-picker")
        a.click(".modal .emoji-group-btn[title='Flags']")
        check(a.js("return document.querySelector('.modal .emoji-scroll').scrollTop") > 1000, "a group's button jumps to it")
        a.click(".modal .emoji-search")
        a.keys("tada")
        a.wait("return (document.querySelector('.modal .emoji-scroll .emoji-btn') || {}).dataset?.e === '🎉'", 5, "found")
        check(not a.js("return !!document.querySelector('.modal .emoji-recent')"), "found by a keyword")
        a.keys(KEYS["backspace"] * 4 + "seedl")
        a.wait("return (document.querySelector('.modal .emoji-scroll .emoji-btn') || {}).dataset?.e === '🌱'", 5, "found")
        a.keys(KEYS["enter"])
        a.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        check(row_icon(a, "Ideas") == "🌱", "found by the start of its name; Enter takes it")
        # A picture: an uploaded image becomes a small square picture, shown
        # where the emoji was — by the title, in the tree, for members too.
        a.go(env.url.rstrip("/") + url)
        a.find(".note-title-row .note-icon", 15)
        a.click(".note-title-row .note-icon")
        a.find(".modal .icon-pick", 5)
        check(not a.js("return !!document.querySelector('.modal .pic-btn')"), "no recent pictures to offer yet")
        pic = os.path.join(env.data, "trip.png")
        png_file(pic, 300, 200)
        a.type(".modal .picture-input", pic)
        a.wait("return !document.querySelector('.modal')", 10, "the dialog closes")
        by_title = "document.querySelector('.note-title-row .note-icon img.entry-pic')"
        a.wait("const i = " + by_title + "; return !!i && i.complete && i.naturalWidth > 0", 10, "the picture by the title")
        w, h = a.js("const i = " + by_title + "; return [i.naturalWidth, i.naturalHeight]")
        check(w == h == 64, f"an uploaded image becomes a 64 px square picture as the note's icon ({w}×{h})")
        check(row_pic(a, "Trip") and row_icon(a, "Trip") is None, "it shows in the tree instead of the emoji")
        b.wait("const r = " + row + "[0]; return !!r.querySelector('img.entry-pic')", 15, "the member's tree", "Trip")
        check(True, "members see the picture too")
        # The folder: the picture given lately is offered again.
        a.js(row + "[0].querySelector('.tree-more').click()", "Travel")
        a.click_el(a.find_text(".tree-row .menu button", "Set icon", 5))
        a.find(".modal .pic-btn", 5)
        a.click(".modal .pic-btn")
        a.wait("return !document.querySelector('.modal')", 5, "the dialog closes")
        a.wait("return !!" + row + "[0].querySelector('img.entry-pic')", 5, "the folder's picture", "Travel")
        check(row_icon(a, "Travel") is None, "a recent picture is offered again, for a folder too")
        # Clicking the icon by the title changes it; Remove takes it away.
        a.go(env.url.rstrip("/") + url)
        a.find(".note-title-row .note-icon", 15)
        a.click(".note-title-row .note-icon")
        a.find(".modal .icon-pick", 5)
        a.click(".modal .icon-remove")
        a.wait("return !document.querySelector('.modal') && !document.querySelector('.note-title-row .note-icon:not(.add)')", 5, "removed")
        check(row_icon(a, "Trip") is None and not row_pic(a, "Trip") and a.js("return !!document.querySelector('.note-title-row .note-icon.add')"),
              "the icon by the title opens the choice; Remove takes it away and the button to add one is back")
        b.wait("const r = " + row + "[0]; return !r.querySelector('.entry-icon')", 15, "removed for the member", "Trip")
        # SVG files: drawn at the picture's full size, whatever size they
        # give themselves (24 pixels, or only a viewBox, which browsers
        # draw small) — the circle fills its picture as it fills the file.
        for name, attrs in [("sized.svg", 'width="24" height="24" viewBox="0 0 24 24"'), ("loose.svg", 'viewBox="0 0 24 24"')]:
            svg = os.path.join(env.data, name)
            with open(svg, "w") as f:
                f.write(f'<svg xmlns="http://www.w3.org/2000/svg" {attrs}><circle cx="12" cy="12" r="11" fill="red"/></svg>')
            a.click(".note-title-row .note-icon")
            a.find(".modal .icon-pick", 5)
            a.type(".modal .picture-input", svg)
            a.wait("return !document.querySelector('.modal')", 10, "the dialog closes")
            a.wait("const i = " + by_title + "; return !!i && i.complete && i.naturalWidth > 0", 10, "the picture by the title")
            w, filled = a.js("const i = " + by_title + "; const c = document.createElement('canvas'); c.width = c.height = i.naturalWidth;"
                             "const x = c.getContext('2d'); x.drawImage(i, 0, 0); const d = x.getImageData(0, 0, c.width, c.height).data;"
                             "let n = 0; for (let k = 3; k < d.length; k += 4) if (d[k] > 128) n++; return [i.naturalWidth, n / (c.width * c.height)]")
            check(w == 64 and filled > 0.6, f"an SVG ({name}) becomes a 64 px picture its drawing fills ({w} px, {filled:.2f} filled)")
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


def test_outdated(env):
    """The server needs a newer app: the page says so, stops syncing, keeps
    edits in the browser and sends them once reloaded."""
    b = Browser()
    pending = "return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.') && localStorage.getItem(k) !== '[]').length"
    try:
        cdp(b, "Page.addScriptToEvaluateOnNewDocument", {"source": BLOCKABLE_SOCKETS})
        signup(b, uniq("otto"))
        new_note(b, "Kept", "first")
        b.wait(pending + " === 0", 20, "the note is synced")
        # As a server answers a page too old for it (development builds are
        # never refused, so the test hands the page that answer itself).
        b.js("""const m = JSON.stringify({type: 'error', code: 'update_required', vault_id: null,
                  message: 'this server needs notekpr 9.0.0 or newer: update this app'});
                window.__sockets.forEach(s => s.dispatchEvent(new MessageEvent('message', {data: m})));""")
        b.find(".outdated-banner", 10)
        check("out of date" in b.text(".outdated-banner"), "the page says it is out of date and to reload")
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        b.keys(" second")
        time.sleep(1.5)
        check(b.js(pending) > 0 and "Out of date" in b.text(".sync"), "edits made meanwhile wait in the browser, not sent")
        b.shot("outdated-banner")
        b.click(".outdated-banner button")
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('first second')", 30, "the reloaded page has the edit")
        b.wait(pending + " === 0", 30, "the kept edit is sent")
        check(not b.js("return !!document.querySelector('.outdated-banner')"), "reloaded: the banner is gone")
        check(True, "reloaded, the kept edits are sent")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


# Select `text` (its first occurrence) in the open note's editor, or in
# the element given second (e.g. the reading view).
SELECT_TEXT = """
const ed = document.querySelector(arguments[1] || '.ed');
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

        # The reading view: highlights there too; a click opens a thread, the
        # list opens one in place, and selected text is commented on.
        a.click(".seg button[aria-label='Reading view']")
        a.wait("return document.querySelectorAll('.reading .cmt').length === 1", 5, "the reading view's highlight")
        a.js("document.querySelector('.reading .cmt').click()")
        a.find(".cm-thread", 5)
        check(a.js(texts) == ["Whole grain"] and a.js("return !!document.querySelector('.reading .cmt.cmt-open')"),
              "the reading view highlights threads; a click opens one")
        a.chord(KEYS["escape"])
        check(a.js(SELECT_TEXT, "today", ".reading"), "“today” is in the reading view")
        a.click(".cm-offer", 5)
        a.wait("return !!document.activeElement && document.activeElement.matches('.cm-compose textarea')", 5, "the comment form")
        a.keys("Or tomorrow?")
        a.chord(KEYS["ctrl"], KEYS["enter"])
        a.wait("return document.querySelectorAll('.reading .cmt').length === 2", 5, "the new highlight")
        a.find(".cm-thread", 5)
        marked = a.js("return Array.from(document.querySelectorAll('.reading .cmt')).map(e => e.textContent)")
        check("today" in marked and a.js(texts) == ["Or tomorrow?"],
              "text selected in the reading view is commented on, and its thread opens there")
        b.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 2", 15, "the comment from the reading view reaches the viewer")
        a.chord(KEYS["escape"])
        a.click(".cm-count")
        a.wait("return document.querySelectorAll('.cm-section .cm-entry').length === 2", 5, "the list")
        a.js("document.querySelectorAll('.cm-section .cm-entry')[0].click()")
        a.find(".cm-thread", 5)
        check(a.js("return !document.querySelector('.reading').classList.contains('hidden')"),
              "the list opens a thread in the reading view (without switching to the editor)")
        a.chord(KEYS["escape"])
        a.click(".seg button[aria-label='Live preview']")

        # The owner turns comments by viewers off.
        a.click(".sb-label button[title='Vault settings']")
        a.find(".modal .viewers-comment input", 5)
        a.js("document.querySelector('.modal .viewers-comment input').click()")
        time.sleep(0.8)
        a.chord(KEYS["escape"])
        b.go(env.url.rstrip("/") + url)
        b.wait("return document.querySelectorAll('.ed-marks .hl.cm').length === 2", 20, "reloaded")
        b.js(SELECT_TEXT, "today")
        time.sleep(0.6)
        check(not b.js("return !!document.querySelector('.cm-offer')"), "once the owner turns it off, viewers can't comment")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_mentions(env):
    a, b = Browser(), Browser()
    texts = "return Array.from(document.querySelectorAll('.cm-thread .cm-text')).map(e => e.textContent)"
    badge = "return (document.querySelector('.sb-mentions .badge') || {}).textContent || ''"
    try:
        an, bn = uniq("mia"), uniq("max")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Fence", "Paint the fence green")
        url = a.js("return location.pathname")
        share(a, ".sb-bottom button:nth-child(2)", bn)

        # Typing @ offers the vault's members; picking one mentions them.
        check(a.js(SELECT_TEXT, "fence"), "“fence” is in the note")
        a.click(".cm-offer", 5)
        a.wait("return !!document.activeElement && document.activeElement.matches('.cm-compose textarea')", 5, "the comment form")
        a.keys("Ask @" + bn[:3])
        a.find(".mention-menu button", 5)
        offered = a.js("return Array.from(document.querySelectorAll('.mention-menu button > span:not(.avatar)')).map(e => e.textContent)")
        check(offered == [bn.capitalize()], f"@ offers the vault's members, not oneself: {offered}")
        a.keys(KEYS["enter"])
        a.keys("about the color")
        a.chord(KEYS["ctrl"], KEYS["enter"])
        a.wait("return !document.querySelector('.cm-compose')", 5, "the comment is sent")
        a.find(".cm-thread", 5)
        check(a.js(texts) == [f"Ask @{bn.capitalize()} about the color"]
              and a.js("return document.querySelector('.cm-thread .mention').textContent") == "@" + bn.capitalize(),
              "the mention is marked in the comment")
        a.chord(KEYS["escape"])

        # The member sees it in the sidebar's mentions, and opens its thread.
        b.wait(badge + " === '1'", 20, "the member's unread mention")
        b.click(".sb-mentions button.icon-btn")
        b.find(".mentions-menu .mention-item.unread", 5)
        item = b.js("return document.querySelector('.mentions-menu .mention-item').textContent")
        check(an.capitalize() in item and "Fence" in item, f"the inbox says who mentioned them, where: {item}")
        b.click(".mentions-menu .mention-item")
        b.wait("return location.pathname === arguments[0]", 10, "the note opens", url)
        b.find(".cm-thread", 10)
        check(b.js(texts) == [f"Ask @{bn.capitalize()} about the color"]
              and b.js("return document.querySelector('.cm-thread .mention.me') !== null"),
              "a mention opens its thread, the mention marked as theirs")
        b.wait(badge + " === ''", 10, "read")
        check(True, "an opened mention is read")

        # Mentioned again while elsewhere: announced as it arrives.
        b.go(env.url)
        b.find(".sb-mentions", 20)
        time.sleep(1.5)
        a.js("document.querySelector('.cm-count').click()")
        a.wait("return document.querySelectorAll('.cm-section .cm-entry').length === 1", 5, "the list")
        a.js("document.querySelector('.cm-section .cm-entry').click()")
        a.find(".cm-thread .cm-reply textarea", 5)
        a.js("document.querySelector('.cm-reply textarea').focus()")
        a.keys("Thanks @" + bn[:2])
        a.find(".mention-menu button", 5)
        a.keys(KEYS["tab"])
        a.click(".cm-reply .btn.primary")
        a.wait("return document.querySelectorAll('.cm-thread .cm-item').length === 2", 5, "the reply")
        b.wait(badge + " === '1'", 20, "the new mention")
        b.wait("return Array.from(document.querySelectorAll('.toast')).some(t => t.textContent.includes('mentioned you'))", 10, "the toast")
        check(True, "a new mention is announced live")
        b.click(".sb-mentions button.icon-btn")
        b.click_el(b.find_text(".mentions-head button", "Mark all as read", 5))
        b.wait(badge + " === ''", 5, "all read")
        check(True, "all mentions can be marked read at once")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_dates(env):
    b = Browser()
    day = """const d = new Date(); d.setDate(d.getDate() + arguments[0]);
             return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0')"""
    try:
        signup(b, uniq("dora"))
        new_note(b, "Plans", "Dentist ")
        # @ offers days; Enter puts the date.
        b.keys("@to")
        b.find(".ed-menu button", 5)
        offered = b.js("return Array.from(document.querySelectorAll('.ed-menu button')).map(e => e.firstChild.textContent)")
        check(offered == ["Today", "Tomorrow"], f"@to offers today and tomorrow: {offered}")
        b.keys(KEYS["enter"])
        # A finished word becomes the date too.
        b.keys("and gym @yesterday.")
        today, yesterday = b.js(day, 0), b.js(day, -1)
        text = editor_text(b)
        check(f"Dentist @{today} and gym @{yesterday}." in text, f"the note holds the dates: {text!r}")
        # Away from the caret, they read as days.
        b.keys(KEYS["enter"] + "next")
        time.sleep(0.4)
        shown = b.js("return Array.from(document.querySelectorAll('.ed .dw')).filter(w => getComputedStyle(w).display !== 'none').map(w => w.textContent)")
        check(shown == ["Today", "Yesterday"], f"the editor shows Today and Yesterday: {shown}")
        b.click(".seg button[aria-label='Reading view']")
        b.find(".reading time.date", 5)
        read = b.js("return Array.from(document.querySelectorAll('.reading time.date')).map(t => [t.textContent, t.getAttribute('datetime')])")
        check(read == [["Today", today], ["Yesterday", yesterday]], f"so does the reading view: {read}")
        b.click(".seg button[aria-label='Live preview']")
        check(not severe_logs(b), "no browser errors")
    finally:
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


def test_callouts(env):
    b = Browser()
    paste = "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));"
    def pick(title):
        b.click(".tb[title='Callout']")
        b.click(f".menu.callouts button[title='{title} callout']")
        time.sleep(0.4)
    # A box's height, and where a text in it starts: its lines in the editor, its element in the reading view.
    measure = """const [lines, text] = arguments;
        const ls = Array.from(document.querySelectorAll(lines)), box = ls[0].getBoundingClientRect();
        const w = document.createTreeWalker(ls[0].closest('.ed, .reading'), NodeFilter.SHOW_TEXT);
        let n; while ((n = w.nextNode()) && !(n.data.includes(text) && ls.some(l => l.contains(n))));
        const r = document.createRange(); r.setStart(n, n.data.indexOf(text)); r.setEnd(n, n.data.indexOf(text) + 1);
        const t = r.getBoundingClientRect();
        return [ls[ls.length - 1].getBoundingClientRect().bottom - box.top, t.left - box.left, t.top - box.top];"""
    def same_box(editor, reading, height=True):
        return all(abs(x - y) <= 1.5 for x, y in list(zip(editor, reading))[0 if height else 1:])
    try:
        signup(b, uniq("cal"))
        new_note(b, "Callouts", "")
        body = ("Intro\n\n> [!WARNING] Mind **the** gap\n> Text in it,\nand a lazy line.\n\n"
                "> [!tip]\n> No title of its own.\n\n> [!faq]- Folded?\n> Hidden at first.\n\n"
                "> [!note]\n> > [!bug] Nested\n> > inside\n>\n> outer again\n\n> plain quote\n\nend")
        b.js(paste, body)
        time.sleep(0.6)
        check(editor_text(b) == body, "the editor keeps callouts as their Markdown")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        # Live preview: every line of one is a line of its box.
        check(b.js("return document.querySelectorAll('.ed .ln.callout-warning').length") == 3, "a callout's lines, the lazy one too, are its box")
        check(b.js("const l = document.querySelector('.ed .ln.callout-warning.cot'); return Array.from(l.querySelectorAll('.mk')).every(m => getComputedStyle(m).display === 'none')"), "its type hides away from the caret")
        check(b.js("return document.querySelector('.ed .ln.callout-warning.cot .b').textContent") == "the", "its title is Markdown")
        check(b.js("return document.querySelector('.ed .ln.callout-tip.cot .cot-d').textContent") == "Tip", "without a title, the type is its title")
        check(b.js("const l = document.querySelector('.ed .ln.callout-warning'); return getComputedStyle(l).backgroundColor != getComputedStyle(document.querySelector('.ed')).backgroundColor"), "in a box of its color")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.callout-warning.cot'), '::before').maskImage.includes('svg')"), "with its icon")
        check(b.js("return document.querySelectorAll('.ed .ln.callout-bug.nest.in-note').length") == 2, "a callout in a callout")
        check(b.js("return !!document.querySelector('.ed .ln.callout-note.cob') && document.querySelector('.ed .ln.callout-note.cob').textContent == '> outer again'"), "the outer one goes on after it")
        check(b.js("return document.querySelectorAll('.ed .ln.quote:not(.callout)').length") == 1, "a plain quote stays a quote")
        editor_box = b.js(measure, ".ed .ln.callout-tip", "No title")
        editor_nested = b.js(measure, ".ed .ln.callout-note, .ed .ln.in-note", "inside")
        b.shot("callouts-editing")
        # The toolbar: a new one on an empty line, then another type for it.
        b.keys(KEYS["enter"] + KEYS["enter"])
        pick("Success")
        b.keys("Done")
        check(editor_text(b).endswith("end\n\n> [!success]\n> Done"), "the toolbar starts a callout, the caret in its text")
        check(b.js("return document.querySelector('.ed .ln.callout-success.cot .cot-d').textContent") == "Success", "shown with its type as its title")
        pick("Danger")
        check(editor_text(b).endswith("end\n\n> [!danger]\n> Done"), "and changes the type of the one the caret is in")
        b.keys(KEYS["enter"] + "more")
        check(editor_text(b).endswith("> [!danger]\n> Done\n> more"), "Enter continues it")
        # The reading view.
        b.click(".seg button[title^='Reading']")
        b.find(".reading .callout", 10)
        # (Its text also says where it is in the note, for comments: compare
        # what is shown.)
        title = b.js("const t = document.querySelector('.reading .callout-warning .co-name');"
                     "return [t.textContent, Array.from(t.querySelectorAll('strong')).map(s => s.textContent)]")
        check(title == ["Mind the gap", ["the"]], "the reading view shows the title")
        check(b.js("return document.querySelector('.reading .callout-warning .co-body').textContent.trim()") == "Text in it,\nand a lazy line.", "and the text")
        check(b.js("return document.querySelector('.reading .callout-tip .co-name').textContent") == "Tip", "or the type as the title")
        check(b.js("const i = document.querySelector('.reading .callout-tip .co-icon').getBoundingClientRect(); return i.width > 8 && i.height > 8"), "with an icon")
        reading_box = b.js(measure, ".reading .callout-tip", "No title")
        check(same_box(editor_box, reading_box), f"a callout's box looks the same in the editor and the reading view ({editor_box} / {reading_box})")
        # In the outer one (its height differs: a `>` line in the editor is a line).
        reading_nested = b.js(measure, ".reading .callout-note", "inside")
        check(same_box(editor_nested, reading_nested, height=False), f"a nested one sits where it does in the reading view ({editor_nested} / {reading_nested})")
        check(not b.js("return document.querySelector('.reading details.callout-question').open"), "a callout with - starts folded")
        check(not b.js("return document.querySelector('.reading .callout-question .co-body').checkVisibility()"), "its text hidden")
        b.click(".reading .callout-question summary")
        time.sleep(0.3)
        check(b.js("return document.querySelector('.reading details.callout-question').open"), "and opens with a click")
        check(b.js("return !!document.querySelector('.reading .callout-note .co-body > .callout-bug')"), "nested callouts nest")
        check(b.js("return document.querySelectorAll('.reading blockquote').length") == 1, "a plain quote stays a quote")
        check(not b.js("return document.querySelector('.reading').textContent.includes('[!')"), "no type markers show")
        b.shot("callouts-reading")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_math(env):
    b = Browser()
    paste = "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));"
    # Whether the text (not the widgets) of the lines matching a selector takes no room.
    folded = """return Array.from(document.querySelectorAll(arguments[0])).every(l =>
        Array.from(l.childNodes).filter(n => !(n.dataset && 'w' in n.dataset)).every(n => {
            const r = document.createRange(); r.selectNodeContents(n); return r.getBoundingClientRect().height === 0; }))"""
    try:
        signup(b, uniq("mth"))
        new_note(b, "Math", "")
        body = ("Euler: $e^{i\\pi} + 1 = 0$, prices $5 and $10.\n\n\\$x$ is money.\n\n"
                "$$\n\\frac{a}{b} = \\sqrt{x^2}\n$$\n\n"
                "Water: $\\ce{2H2 + O2 -> 2H2O}$ and $\\frac{1}$\n\nend")
        b.js(paste, body)
        time.sleep(0.6)
        check(editor_text(b) == body, "the editor keeps math as its LaTeX")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        # Inline: typeset away from the caret's line, one per formula.
        first = ".ed .ln:first-child"
        check(b.js("return document.querySelectorAll(arguments[0] + ' .w.mw math').length", first) == 1, "a formula in a line is typeset, the prices aren't")
        check(b.js("return getComputedStyle(document.querySelector(arguments[0] + ' .w.mw')).display", first) == "inline", "away from the caret")
        check(b.js("return Array.from(document.querySelectorAll(arguments[0] + ' .mth')).every(m => getComputedStyle(m).display === 'none')", first), "its LaTeX hidden")
        # A block of display math folds to its formula.
        check(b.js("return !!document.querySelector('.ed .ln.math.mc .w.mb math[display=block] mfrac')"), "a block of math shows its formula on its last line")
        check(b.js(folded, ".ed .ln.math"), "its source folds away")
        # Chemistry, and a formula that can't be typeset.
        water = b.js("return Array.from(document.querySelectorAll('.ed .ln')).find(l => l.textContent.startsWith('Water'))"
                     ".querySelectorAll('.w.mw').length")
        check(water == 2, "chemistry and a broken formula each get a widget")
        check(b.js("return !!document.querySelector('.ed .w.mw math') && !document.querySelector('.ed .w.mw').textContent.includes('\\\\ce')"), "chemistry is typeset")
        check(b.js("return document.querySelector('.ed .w.mw .merr').textContent") == "$\\frac{1}$", "a broken formula shows its source")
        check(bool(b.js("return document.querySelector('.ed .w.mw .merr').title")), "saying why")
        b.shot("math-editing")
        # A click on a block's formula: its source, the formula still under it.
        b.click(".ed .w.mb math")
        time.sleep(0.4)
        check(b.js("return document.querySelectorAll('.ed .ln.math.active').length") == 3, "a click on a formula shows its source")
        check(not b.js(folded, ".ed .ln.math"), "the source shows")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.math.active .w.mb')).display") != "none", "and the formula stays under it")
        b.keys(KEYS["end"] + " + 1")
        time.sleep(0.4)
        check("\\sqrt{x^2} + 1\n$$" in editor_text(b), "typing goes into the formula")
        check(b.js("return document.querySelector('.ed .w.mb math').textContent.includes('1')"), "which is typeset again as it changes")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        check(b.js(folded, ".ed .ln.math"), "away from it, folded again")
        # The reading view.
        b.click(".seg button[title^='Reading']")
        b.find(".reading math", 10)
        maths = b.js("return Array.from(document.querySelectorAll('.reading math')).map(m => m.textContent)")
        check(len(maths) == 3, f"the reading view typesets the same formulas ({maths})")
        check(b.js("return !!document.querySelector('.reading .mathblock math[display=block]')"), "a block as a block")
        text = b.js("return document.querySelector('.reading').textContent")
        check("prices $5 and $10." in text and "$x$ is money." in text, "prices and escaped dollars stay text")
        check(b.js("return !!document.querySelector('.reading .merr')"), "a broken formula shows as one")
        b.shot("math-reading")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_diagrams(env):
    b = Browser()
    paste = "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]); document.querySelector('.ed').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));"
    loaded = "return performance.getEntriesByType('resource').some(e => e.name.includes('notekpr_diagram_bg.wasm'))"
    folded = """return Array.from(document.querySelectorAll('.ed .ln.code.mmd')).every(l => l.getBoundingClientRect().height === 0)"""
    try:
        signup(b, uniq("dgm"))
        new_note(b, "Diagrams", "")
        check(not b.js(loaded), "what draws diagrams isn't loaded with the app")
        body = ("Intro\n\n```mermaid\nflowchart LR\n  A[Start] --> B{Shared?}\n  click B href \"https://example.org/\"\n  A --> L[Linked note]\n  class L internal-link\n```\n\n"
                "```mermaid\nflowchart LR\n  A -->\n```\n\n"
                "```mermaid\nflowchart LR\n  X[\"<div class='cover' style='position:fixed;inset:0;background:red'>fake</div>\"] --> Y\n```\n\nend")
        b.js(paste, body)
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.find(".ed .w.dg svg", 30)
        check(b.js(loaded), "it loads when a note shows a diagram")
        check(editor_text(b) == body, "the editor keeps a diagram as its code")
        check("Start" in b.js("return document.querySelector('.ed .w.dg svg').textContent"), "the diagram is drawn")
        check(b.js(folded), "its code folds away")
        check(b.js("return !!document.querySelector('.ed .w.dg svg a[href=\"https://example.org/\"][target=_blank]')"), "its links open in a new tab")
        check(b.js("return !!document.querySelector('.ed .w.dg .dgerr')"), "one that can't be drawn says why")
        covers = """const c = document.querySelector('.ed .w.dg .cover'); if (!c) return 'gone';
            const r = c.getBoundingClientRect(), box = c.closest('.w.dg').getBoundingClientRect();
            const shown = (x, y) => document.elementFromPoint(x, y)?.closest('.cover') === c;
            return shown(5, 5) || shown(innerWidth - 5, innerHeight - 5) ? 'covers' : 'inside'"""
        check(b.js(covers) != "covers", "a diagram can't draw over the app")
        b.shot("diagrams-editing")
        # A click on it: its code, the diagram still under it, following edits.
        b.click(".ed .w.dg svg .node")
        time.sleep(0.4)
        check(b.js("return document.querySelectorAll('.ed .ln.mmd.active').length") >= 5, "a click on a diagram shows its code")
        check(not b.js(folded), "the code shows")
        check(b.js("return getComputedStyle(document.querySelector('.ed .ln.mmd.active .w.dg')).display") != "none", "the diagram stays under it")
        # At the end of the line `A[Start] --> B{Shared?}`, a line more.
        b.js("""const w = document.createTreeWalker(document.querySelector('.ed'), NodeFilter.SHOW_TEXT);
            let n, last; while ((n = w.nextNode())) if (n.data.includes('Shared?}')) last = n;
            getSelection().collapse(last, last.data.length);""")
        time.sleep(0.2)
        b.keys(KEYS["enter"] + "B --> C[Done]")
        time.sleep(0.6)
        check("C[Done]" in editor_text(b), "typing goes into its code")
        check("Done" in b.js("return document.querySelector('.ed .w.dg').textContent"), "and the diagram follows")
        b.chord(KEYS["ctrl"], KEYS["end"])
        time.sleep(0.4)
        check(b.js(folded), "away from it, folded again")
        # The reading view.
        b.click(".seg button[title^='Reading']")
        b.find(".reading .diagram svg", 10)
        check("Done" in b.js("return document.querySelector('.reading .diagram svg').textContent"), "the reading view shows the diagram")
        arrows = """const svg = document.querySelector('.reading .diagram svg');
            const path = svg.querySelector('[marker-end]'); const id = path.getAttribute('marker-end').slice(5, -1);
            return svg.contains(document.getElementById(id))"""
        check(b.js(arrows), "its arrows point at its own markers, not the editor's hidden copy")
        check(b.js("return !!document.querySelector('.reading .codeblock + .dgerr, .reading .dgerr')"), "and the code of one that can't be drawn, saying why")
        b.shot("diagrams-reading")
        # A box linked to a note, as in Obsidian (`class A internal-link`):
        # it opens the note its label names, made if there's none yet.
        title = "return document.querySelector('.note-title')?.value"
        check(b.js("return getComputedStyle(document.querySelector('.reading .diagram .node.internal-link')).cursor") == "pointer",
              "a box linked to a note shows it's a link")
        b.click(".reading .diagram .node.internal-link")
        b.wait(title + " === 'Linked note'", 10, "the note it names")
        b.js("history.back()")
        b.wait(title + " === 'Diagrams'", 10, "back")
        b.click(".seg button[title='Live preview']")
        b.find(".ed .w.dg svg .node.internal-link", 30)
        b.chord(KEYS["ctrl"], KEYS["end"])
        b.click(".ed .w.dg svg .node.internal-link")
        b.wait(title + " === 'Linked note'", 10, "the editor's diagram opens it too")
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


def test_note_width(env):
    """Notes from narrow to wide: a default (this browser's, or the same on
    all devices) and a note's own width, only for whoever chose it."""
    b = Browser(1900, 900)
    width = "return Math.round(document.querySelector('.note-main').getBoundingClientRect().width)"
    # All the room there is: beside the sidebar and the side panel.
    room = "return Math.round(document.querySelector('.note-body').getBoundingClientRect().width" \
           " - (document.querySelector('.note-body > .panel')?.getBoundingClientRect().width || 0))"
    slide = "const i = document.querySelector(arguments[0]); i.value = arguments[1]; i.dispatchEvent(new Event('input', {bubbles: true}));"

    def settings():
        b.js("history.pushState(null, '', '/settings/editor'); dispatchEvent(new PopStateEvent('popstate'));")
        b.find(".width-prefs input[type=range]", 10)

    def note_is(px, what):
        b.wait(f"return Math.abs(({width[7:]}) - {px}) <= 1", 5, what)
        check(True, what)

    try:
        signup(b, uniq("wendy"))
        new_note(b, "Wide table", "lots of columns")
        note = b.js("return location.pathname")
        full = b.js(room)
        check(b.js(width) == 820 and full > 1200, f"notes are narrow by default (of {full}px)")
        half = 820 + (full - 820) / 2
        settings()
        b.js(slide, ".width-prefs input", "50")
        check(b.text(".width-prefs .width-value") == "50%", "Settings has a slider from narrow to wide")
        b.js("history.back()")
        note_is(half, "notes are halfway between narrow and wide")
        # A note's own width, from its ⋯ menu.
        b.click(".note-head button[aria-label='More']")
        b.find(".menu-width input", 5)
        b.js(slide, ".menu-width input", "100")
        note_is(full, "the ⋯ menu makes this note wide")
        check(b.js("return !!document.querySelector('.menu-width')"), "and stays open to see it change")
        check(b.text(".menu-width .width-default") == "Use the default (50%)", "it offers the default back")
        check(b.js("return [document.querySelector('.menu-width .width-end.on')?.textContent, document.querySelector('.menu-width .width-value').textContent]") == ["Wide", ""],
              "at an end of the slider, its word lights up")
        b.shot("note-width-menu")
        b.chord(KEYS["escape"])
        b.chord(KEYS["ctrl"], "\\")
        b.wait("return !document.querySelector('.note-body > .panel')", 5, "the side panel closes")
        note_is(b.js(room), "wide takes the side panel's room when it closes")
        b.chord(KEYS["ctrl"], "\\")
        b.find(".note-body > .panel", 5)
        new_note(b, "Short", "a few words")
        note_is(half, "other notes keep the default")
        b.click(".note-head button[aria-label='More']")
        b.find(".menu-width input", 5)
        check(b.js("return !document.querySelector('.menu-width .width-default')"), "a note without its own width has nothing to undo")
        b.chord(KEYS["escape"])
        # The same default on all devices: synced, so a browser without its
        # own settings (here: this one, forgotten) uses it too.
        settings()
        b.click(".width-shared")
        time.sleep(3)  # the settings are saved after a moment
        b.js("localStorage.removeItem('notekpr.ui'); location.reload()")
        b.find(".width-prefs input[type=range]", 20)
        b.wait("return document.querySelector('.width-shared').checked", 10, "the setting synced")
        check(b.text(".width-prefs .width-value") == "50%", "a default shared by all devices reaches a new browser")
        b.go(env.url + note[1:])
        b.find(".note-main .ed .ln", 20)
        note_is(b.js(room), "a note's own width is synced too")
        b.click(".note-head button[aria-label='More']")
        b.click(".menu-width .width-default")
        note_is(half, "Use the default undoes it")
        check(b.js("return !!document.querySelector('.menu-width') && !document.querySelector('.menu-width .width-default')"), "the menu stays open, with nothing left to undo")
        b.chord(KEYS["escape"])
        # Back to this browser's own: it keeps the width it had.
        settings()
        b.click(".width-shared")
        b.wait("return !document.querySelector('.width-shared').checked", 5, "not shared")
        check(b.text(".width-prefs .width-value") == "50%" and '"note_width":50' in b.js("return localStorage.getItem('notekpr.ui')"),
              "a browser keeps the shared width as its own when it stops sharing it")
        b.shot("note-width-settings")
        check(not severe_logs(b), "no browser errors")
    finally:
        b.close()


def test_admin(env):
    """Instance limits and read-only mode from the Administration settings."""
    a, b = Browser(), Browser()
    pending = "return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.') && localStorage.getItem(k) !== '[]').length"
    choose = "const s = document.querySelector(arguments[0]); s.value = arguments[1]; s.dispatchEvent(new Event('change', {bubbles: true}));"
    number = "const i = document.querySelector(arguments[0]); i.value = arguments[1]; i.dispatchEvent(new Event('input', {bubbles: true}));"
    try:
        signup(a, ADMIN)
        rob = uniq("rob")
        signup(b, rob)  # owns the vault made at sign-up
        a.go(env.url + "settings/admin")
        a.find(".instance-settings .save-limits", 15)
        check(a.js("return document.querySelector('.instance-status').textContent.startsWith('Accepting changes')"), "the instance settings say the server accepts changes")
        version = api_get(a, "/info")["version"]
        a.find(".server-version", 10)
        check(a.text(".server-version") == f"Server version {version}", "the admin page shows the server's version")
        a.js(choose, ".vaults-per-user .limit-kind", "max")
        a.find(".vaults-per-user .limit-num", 5)
        a.js(number, ".vaults-per-user .limit-num", "1")
        a.click(".save-limits")
        a.find_text(".toast", "Instance settings saved", 10)
        check(True, "administrators set a limit of vaults per person")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "Second")
        b.click(".modal button[type=submit]")
        b.find_text(".toast", "you can own 1 vault on this server", 10)
        check(True, "a second vault is refused, saying why")
        b.chord(KEYS["escape"])
        # Rob's own allowance.
        row = f"[data-user='{rob.capitalize()}']"
        a.find(row, 10)
        check("owns 1 of 1 vault" in a.text(f"{row} .allowance-usage"), "the accounts list shows what someone owns of what they may")
        a.click(f"{row} .limits-btn")
        a.js(choose, f"{row} .vault-allowance .limit-kind", "max")
        a.find(f"{row} .vault-allowance .limit-num", 5)
        a.js(number, f"{row} .vault-allowance .limit-num", "2")
        a.click(f"{row} .save-allowances")
        a.wait(f"return (document.querySelector(\"{row} .allowance-usage\")?.textContent || '').includes('1 of 2 vaults')", 10, "rob may own 2 vaults")
        a.shot("admin-limits")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "Second")
        b.click(".modal button[type=submit]")
        b.find_text(".page-head h1", "Second", 15)
        check(True, "a person's own allowance lets them create more")

        # The oldest app version: older apps are told to update (this page,
        # a development build, never is).
        a.type(".oldest-app-version", "0.6.0")
        a.click(".save-oldest-app")
        deadline = time.time() + 10
        while api_get(a, "/admin/settings")["settings"].get("oldest_app_version") != "0.6.0":
            if time.time() > deadline:
                raise AssertionError("the oldest app version wasn't saved")
            time.sleep(0.2)
        check(True, "administrators raise the oldest app version the server accepts")

        # The server's settings, in force at once (no restart).
        def server_settings(size, directory):
            a.js(number, ".max-attachment-mb .limit-num", str(size))
            a.js(choose, ".user-directory select", directory)
            a.click(".save-server-settings")
            deadline = time.time() + 10
            while (lambda s: (s["max_attachment_mb"], s["user_directory"]))(
                    api_get(a, "/admin/settings")["settings"]) != (size, directory):
                if time.time() > deadline:
                    raise AssertionError("the server settings weren't saved")
                time.sleep(0.2)

        server_settings(1, "exact_email")
        check(api_get(a, "/info")["max_attachment_bytes"] == 1 << 20,
              "administrators change the server's sizes and how people are found, and apps learn them at once")
        a.js("document.querySelector('.server-settings').scrollIntoView({block: 'center'})")
        a.shot("admin-server")
        server_settings(25, "search")

        # An admin client for a billing system: its own token and scopes.
        token_call = """window.__api = undefined;
            fetch('/api/v1' + arguments[1], {method: arguments[2], body: arguments[3] ? JSON.stringify(arguments[3]) : undefined,
              headers: {'Content-Type': 'application/json', Authorization: 'Bearer ' + arguments[0]}})
              .then(r => { window.__api = r.status; }, () => { window.__api = 0; });"""

        def with_token(token, path, method="GET", body=None):
            a.js(token_call, token, path, method, body)
            a.wait("return window.__api !== undefined", 10, "API answer")
            return a.js("return window.__api")

        a.type(".admin-client-new .client-name", "billing")
        a.js("document.querySelector('.admin-client-new .create-client').click()")
        a.find(".new-admin-token pre", 10)
        token = a.text(".new-admin-token pre").strip()
        check(token.startswith("notekpr_a_"), "a new admin client's token is shown once")
        check(with_token(token, "/admin/users") == 200 and with_token(token, "/admin/settings", "PATCH", {"read_only": True}) == 403,
              "its token reads and does what its scopes allow, nothing else")
        check(with_token(token, "/vaults") == 401, "and opens no vault")
        a.find("[data-client='billing'] .revoke-token", 10)
        a.js("document.querySelector('.new-admin-token').scrollIntoView()")
        a.shot("admin-clients")
        a.js("document.querySelector(\"[data-client='billing'] .revoke-token\").click()")
        a.wait("return !document.querySelector(\"[data-client='billing'] .revoke-token\")", 10, "the token is revoked")
        check(with_token(token, "/admin/users") == 401, "a revoked token stops working at once")

        # Read-only: people keep writing, their edits wait in the browser.
        new_note(b, "While read-only", "first words")
        b.wait(pending + " === 0", 20, "the note is synced")
        a.js("document.querySelector('.read-only-toggle').click()")
        b.find(".read-only-banner", 10)
        check("read-only" in b.text(".read-only-banner"), "everyone is told at once that the server is read-only")
        b.keys(" and more")
        time.sleep(1.5)
        check(b.js(pending) > 0 and "Read-only" in b.text(".sync"), "edits are kept in the browser, not sent")
        b.shot("read-only-banner")
        a.js("document.querySelector('.read-only-toggle').click()")
        b.wait("return !document.querySelector('.read-only-banner')", 10, "the banner goes")
        b.wait(pending + " === 0", 20, "the kept edits are sent")
        check(True, "writable again: kept edits are sent right away")
        b.js("location.reload()")
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('first words and more')", 20, "the edit is on the server")
        # Refused on purpose: the second vault, and the admin client's calls.
        refused = ["/api/v1/vaults - Failed to load resource: the server responded with a status of 403",
                   "/api/v1/vaults - Failed to load resource: the server responded with a status of 401",
                   "/api/v1/admin/settings - Failed to load resource: the server responded with a status of 403",
                   "/api/v1/admin/users - Failed to load resource: the server responded with a status of 401"]
        check(all(any(r in m for r in refused) for m in severe_logs(a, b)), "no browser errors but what was refused on purpose")
    finally:
        # Other tests share this server.
        api_patch(a, "/admin/settings", {"read_only": False, "vaults_per_user": "unlimited", "oldest_app_version": ""})
        a.close()
        b.close()


def test_vault_storage(env):
    """A vault's id and size in its settings; a full vault keeps its edits
    until an administrator gives it room."""
    a, b = Browser(), Browser()
    pending = "return Object.keys(localStorage).filter(k => k.startsWith('notekpr.pending.') && localStorage.getItem(k) !== '[]').length"
    choose = "const s = document.querySelector(arguments[0]); s.value = arguments[1]; s.dispatchEvent(new Event('change', {bubbles: true}));"
    number = "const i = document.querySelector(arguments[0]); i.value = arguments[1]; i.dispatchEvent(new Event('input', {bubbles: true}));"
    try:
        signup(a, ADMIN2)
        tina = uniq("tina")
        signup(b, tina)  # owns the vault made at sign-up
        new_note(b, "Full of words", "first words")
        b.wait(pending + " === 0", 20, "the note is synced")
        vid = next(v["id"] for v in api_get(b, "/vaults") if v.get("kind", "regular") == "regular")

        # The administrator finds it by its id and gives it no room.
        a.go(env.url + "settings/admin")
        a.find(".vault-search-id", 15)
        a.type(".vault-search-id", vid)
        a.click(".vault-search button[type=submit]")
        row = f"[data-vault='{vid}']"
        a.wait(f"return document.querySelectorAll('.admin-vaults .list-row').length === 1 && !!document.querySelector(\"{row}\")", 10, "the vault is found by its id")
        check(a.text(f"{row} .vault-usage").startswith(tina.capitalize()), "administrators see its owner and size, not its name")
        a.click(f"{row} .vault-limit-btn")
        a.js(choose, f"{row} .storage-allowance .limit-kind", "max")
        a.find(f"{row} .storage-allowance .limit-num", 5)
        a.js(number, f"{row} .storage-allowance .limit-num", "0")
        a.click(f"{row} .save-vault-limit")
        a.find_text(".toast", "Vault limit saved", 10)
        a.wait(f"return !!document.querySelector(\"{row} .pill.warn\")", 10, "the vault shows as full")
        a.shot("admin-vaults")

        # Its members are told at once; their edits wait in the browser.
        b.find(".vault-full-banner", 10)
        check("is full" in b.text(".vault-full-banner"), "its members are told at once that it is full")
        b.keys(" and more")
        time.sleep(1.5)
        check(b.js(pending) > 0 and "Vault full" in b.text(".sync"), "edits are kept in the browser, not sent")
        b.shot("vault-full")
        b.click(".vault-full-banner .btn")
        # (textContent: WebDriver's text is empty while the dialog fades in)
        b.wait("return document.querySelector('.vault-storage .vault-id')?.textContent === arguments[0]", 10, "the vault's id", vid)
        check(True, "vault settings show its id, to give to an administrator")
        b.wait("return (document.querySelector('.storage-use')?.textContent || '').includes('of 0 B')", 10, "the settings show what it takes of its limit")
        check("It is full" in b.js("return document.querySelector('.storage-help').textContent"), "and what makes room")
        b.shot("vault-storage")
        b.chord(KEYS["escape"])

        # Room again: the kept edits go out.
        a.click(f"{row} .vault-limit-btn")
        a.js(choose, f"{row} .storage-allowance .limit-kind", "default")
        a.click(f"{row} .save-vault-limit")
        b.wait("return !document.querySelector('.vault-full-banner')", 10, "the banner goes")
        b.wait(pending + " === 0", 20, "the kept edits are sent")
        check(True, "with room again, kept edits are sent right away")
        b.js("location.reload()")
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('first words and more')", 20, "the edit is on the server")

        # Quick notes have a limit of their own, the person's: not among the
        # vaults, but under their Limits. Their owner hears of them as such,
        # with no vault settings to show, and their page shows the room.
        qid = next(v["id"] for v in api_get(b, "/vaults") if v.get("kind") == "quick_notes")
        a.click(".vault-search .btn.ghost")
        a.type(".vault-search-id", qid)
        a.click(".vault-search button[type=submit]")
        a.find_text(".admin-vaults p", "No vault found", 10)
        check(a.js("return getComputedStyle(document.querySelector('.admin-vaults')).overflowY") == "auto",
              "quick notes aren't among the vaults, and the vault list scrolls inside its box")
        person = f"[data-user='{tina.capitalize()}']"
        a.click(f"{person} .limits-btn")
        a.js(choose, f"{person} .quick-notes-storage-allowance .limit-kind", "max")
        a.find(f"{person} .quick-notes-storage-allowance .limit-num", 5)
        a.js(number, f"{person} .quick-notes-storage-allowance .limit-num", "0")
        a.click(f"{person} .save-allowances")
        a.find_text(f"{person} .pill", "Personal storage full", 10)
        b.find(".vault-full-banner", 10)
        check("Your personal storage is full" in b.text(".vault-full-banner")
              and not b.js("return !!document.querySelector('.vault-full-banner .btn')"),
              "full quick notes are named as such, without vault settings")
        b.click(".sb-quick .sb-link")
        b.find(".qn-storage .meter.full", 10)
        check("full" in b.js("return document.querySelector('.qn-storage').textContent"),
              "the quick notes page shows their room and what makes more")
        a.click(f"{person} .limits-btn")
        a.js(choose, f"{person} .quick-notes-storage-allowance .limit-kind", "default")
        a.click(f"{person} .save-allowances")
        b.wait("return !document.querySelector('.vault-full-banner')", 10, "the banner goes")

        # The server's storage (a family's or a team's room): every vault is
        # full, and that is said once, without a vault's settings to show.
        api_patch(a, "/admin/settings", {"max_storage_mb": 0})
        b.find_text(".vault-full-banner", "The server's storage is full", 10)
        check(not b.js("return !!document.querySelector('.vault-full-banner .btn')"),
              "a full server is said once, not vault by vault")
        a.go(env.url + "settings/admin")
        a.wait("return (document.querySelector('.instance-status')?.textContent || '').startsWith('Full')", 15, "the instance status")
        check(True, "administrators see the server is full")
        api_patch(a, "/admin/settings", {"max_storage_mb": "unlimited"})
        b.wait("return !document.querySelector('.vault-full-banner')", 10, "the banner goes")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        # Other tests share this server.
        api_patch(a, "/admin/settings", {"max_storage_mb": "unlimited"})
        a.close()
        b.close()


def test_vault_admins(env):
    """Editors move notes to the trash; the owner and admins delete for good."""
    a, b = Browser(), Browser()
    buttons = "return Array.from(document.querySelectorAll(arguments[0])).map(e => e.textContent.trim())"
    try:
        an, bn = uniq("rita"), uniq("sam")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Old plan", "outdated")
        path = a.js("return location.pathname")
        vid = path.split("/")[2]
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('outdated')", 20, "sam sees the note")
        # Sam, an editor, trashes it: he can restore it, not delete it for good.
        b.click(".note-head .tb-drop .icon-btn")
        b.click_el(b.find_text(".menu button", "Move to trash"))
        time.sleep(0.6)
        b.click_el(b.find_text(".sb-link", "Trash"))
        b.find_text(".list-row .btn", "Restore", 10)
        check("Delete forever" not in b.js(buttons, ".list-row .btn") and "Empty trash" not in b.js(buttons, ".page-head .btn"),
              "an editor isn't offered to delete for good")
        check("owner and its admins" in b.text(".trash-admins"), "the trash says who can")
        b.click(".sb-label button[title='Vault settings']")
        b.find(".modal .settings-grid", 5)
        check(not b.js("return !!document.querySelector('.activity-log')"), "nor the activity log")
        b.chord(KEYS["escape"])
        # Rita makes him an admin; agents and single notes aren't offered it.
        a.click(".sb-bottom button:nth-child(2)")
        a.find_text(".people .person", bn.capitalize(), 10)
        check("Admin" in a.js(buttons, ".share-add select option"), "people can be added as admins")
        a.js(SET_MEMBER_ROLE, bn.capitalize(), "admin")
        time.sleep(1)
        a.chord(KEYS["escape"])
        b.wait("return Array.from(document.querySelectorAll('.list-row .btn')).some(e => e.textContent === 'Delete forever')", 15,
               "sam is offered to delete for good")
        check(not b.js("return !!document.querySelector('.trash-admins')"), "made an admin, sam can delete for good, without reloading")
        log = api_get(b, f"/vaults/{vid}/log")
        last = log["entries"][-1]["body"]
        check(last["kind"] == "role" and last["role"] == "admin", "the owner signed sam's new role")
        b.click_el(b.find_text(".list-row .btn", "Delete forever"))
        # It deletes: Cancel has the focus, so Enter cancels.
        b.find(".ask .ask-no", 10)
        b.wait("return getComputedStyle(document.querySelector('.ask-back')).opacity === '1'", 5, "the question to fade in")
        check(b.js("return document.activeElement === document.querySelector('.ask .ask-no')"), "a question that deletes starts on Cancel")
        b.chord(KEYS["enter"])
        b.wait("return !document.querySelector('.ask')", 5, "the question to go")
        check(b.js("return Array.from(document.querySelectorAll('.list-row .btn')).some(e => e.textContent === 'Delete forever')"),
              "Enter there cancels: nothing is deleted")
        b.click_el(b.find_text(".list-row .btn", "Delete forever"))
        b.answer()
        b.wait("return document.querySelector('.page-body').textContent.includes('The trash is empty.')", 10, "sam's trash is empty")
        a.click_el(a.find_text(".sb-link", "Trash"))
        a.wait("return document.querySelector('.page-body').textContent.includes('The trash is empty.')", 10, "rita's trash is empty")
        check(True, "the note is gone for everyone")
        b.click(".sb-label button[title='Vault settings']")
        b.click(".modal .activity-log")
        b.wait("return Array.from(document.querySelectorAll('.audit-row code')).some(e => e.textContent === 'object.purge')", 10, "the purge in the activity log")
        check(not b.js("return Array.from(document.querySelectorAll('.modal .settings-grid .btn')).some(e => e.textContent.includes('People'))"),
              "an admin sees the activity log, but people and settings stay the owner's")
        b.shot("vault-admin-settings")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_owners(env):
    """A vault can have several owners, all equal; the last one stays, also
    when deleting their account."""
    a, b = Browser(), Browser()
    try:
        an, bn = uniq("nora"), uniq("omar")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Groceries", "milk")
        path = a.js("return location.pathname")
        vid = path.split("/")[2]
        share(a, ".sb-bottom button:nth-child(2)", bn)

        # Shared, with one owner: its home page suggests another owner, until
        # she says she knows what not having one means — on every device.
        a.go(f"{env.url.rstrip('/')}/v/{vid}")
        a.find(".owner-hint", 20)
        a.shot("owner-hint")
        a.click(".owner-hint-dismiss")
        a.find(".owner-hint-hide", 5)
        check(a.js("return document.querySelector('.owner-hint-hide').disabled"), "hiding the hint asks to acknowledge what it means first")
        a.click(".owner-hint-ack")
        a.click(".owner-hint-hide")
        a.wait("return !document.querySelector('.owner-hint')", 5, "the hint goes")
        time.sleep(2.5)  # settings are saved 1.5 s after a change
        a.js("location.reload()")
        a.find(".quick-actions", 20)
        time.sleep(1.5)
        check(not a.js("return !!document.querySelector('.owner-hint')"), "and stays hidden")

        # Her account can't go before the vault is someone else's too, or
        # deleted.
        a.js(TO_ACCOUNT_SETTINGS)
        a.click(".delete-account")
        a.find(".last-owner .last-owner-vault", 10)
        a.type(".delete-confirm", "delete")
        check(a.js("return document.querySelector('.delete-account-now').disabled"),
              "the last owner of a shared vault can't delete her account")
        a.shot("delete-account-last-owner")
        # From there: make someone owner.
        a.click(".last-owner-vault .btn")
        a.find_text(".people .person", bn.capitalize(), 10)
        # Asked in the app's own dialog, over the Share dialog: Escape keeps
        # what it was, Cancel too, and the Share dialog stays.
        role = ("return Array.from(document.querySelectorAll('.people .person')).find(r => r.textContent.includes(arguments[0]))"
                ".querySelector('.member-role').value")
        a.js(SET_MEMBER_ROLE, bn.capitalize(), "owner")
        a.find(".ask .ask-yes", 10)
        check(a.js("return document.querySelector('.ask').getAttribute('role')") == "alertdialog"
              and "an owner?" in a.js("return document.querySelector('.ask h2').textContent")
              and a.js("return document.querySelector('.ask .ask-yes').textContent") == "Make owner"
              and a.js("return document.activeElement === document.querySelector('.ask .ask-yes')"),
              "making an owner asks in the app's own dialog, its button saying what it does")
        a.wait("return getComputedStyle(document.querySelector('.ask-back')).opacity === '1'", 5, "the question to fade in")
        a.shot("ask-make-owner")
        a.chord(KEYS["escape"])
        a.wait("return !document.querySelector('.ask')", 5, "the question to go")
        check(a.js(role, bn.capitalize()) == "editor" and a.js("return !!document.querySelector('.modal .people')"),
              "Escape keeps the role, and the Share dialog stays open")
        a.js(SET_MEMBER_ROLE, bn.capitalize(), "owner")
        a.answer(yes=False)
        check(a.js(role, bn.capitalize()) == "editor" and a.js("return !!document.querySelector('.modal .people')"),
              "so does Cancel")
        a.js(SET_MEMBER_ROLE, bn.capitalize(), "owner")
        a.answer()
        end = time.time() + 10
        while api_get(a, f"/vaults/{vid}/log")["entries"][-1]["body"]["kind"] != "make_owner" and time.time() < end:
            time.sleep(0.3)
        check(api_get(a, f"/vaults/{vid}/log")["entries"][-1]["body"]["kind"] == "make_owner", "making an owner is signed in the log")
        a.chord(KEYS["escape"])

        # Owners are equal: Omar may leave or delete it, and so may Nora.
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('milk')", 20, "omar opens the note")
        b.click(".sb-label button[title='Vault settings']")
        b.find(".modal .leave-vault", 10)
        check(b.js("return !!document.querySelector('.modal .delete-vault')"), "a second owner may leave or delete the vault")
        b.chord(KEYS["escape"])
        a.js(TO_ACCOUNT_SETTINGS)
        a.click(".delete-account")
        a.find(".delete-what", 10)
        # (textContent: WebDriver's text is empty while the dialog fades in)
        check(not a.js("return !!document.querySelector('.last-owner')")
              and "you own with others" in a.js("return document.querySelector('.delete-what').textContent"),
              "with another owner, deleting her account would leave the vault to him")
        a.chord(KEYS["escape"])

        # Nora leaves: the vault is Omar's, and still checks out for him —
        # its membership history starts with her.
        a.go(env.url.rstrip("/") + path)
        a.find(".sb-label button[title='Vault settings']", 20)
        a.click(".sb-label button[title='Vault settings']")
        a.click(".modal .leave-vault")
        a.answer()
        a.wait("return !Array.from(document.querySelectorAll('.sb-tree, .page-body')).some(e => e.textContent.includes('Groceries'))", 15, "nora left")
        b.js("location.reload()")
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('milk')", 20, "omar's vault still opens")
        check(not b.js("return document.body.innerText.includes('Security warning')"), "its log verifies after its creator left")
        b.click(".sb-label button[title='Vault settings']")
        b.find(".modal .delete-vault", 10)
        check(not b.js("return !!document.querySelector('.modal .leave-vault')"), "the last owner can't leave")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_undo_purge(env):
    """Deleting for good can be undone from the trash for a while."""
    a, b = Browser(), Browser()
    try:
        an, bn = uniq("uma"), uniq("vic")
        signup(a, an)
        signup(b, bn)
        new_note(a, "Keeper", "worth keeping")
        path = a.js("return location.pathname")
        share(a, ".sb-bottom button:nth-child(2)", bn)
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('worth keeping')", 20, "vic sees the note")
        a.click(".note-head .tb-drop .icon-btn")
        a.click_el(a.find_text(".menu button", "Move to trash"))
        time.sleep(0.6)
        a.click_el(a.find_text(".sb-link", "Trash"))
        a.click_el(a.find_text(".list-row .btn", "Delete forever"))
        asked = a.answer()
        a.find_text(".trash-gone-list .list-row", "Keeper", 10)
        check("undo it here for 30 days" in asked, "deleting for good says it can be undone, and for how long")
        check("The trash is empty." in a.text(".page-body") and "kept until" in a.text(".trash-gone-list"),
              "gone from the trash, listed as deleted for good")
        check(not a.js("return Array.from(document.querySelectorAll('.sb-link')).some(e => e.textContent.includes('Keeper'))"),
              "and nowhere else (not in Recent)")
        b.wait("return !document.querySelector('.ed')", 15, "gone for vic too")
        a.shot("trash-deleted-for-good")
        a.click_el(a.find_text(".trash-gone-list .btn", "Undo"))
        a.find_text(".toast", "Back in the trash", 10)
        a.wait("return !document.querySelector('.trash-gone-list')", 10, "nothing left to undo")
        a.click_el(a.find_text(".list-row .btn", "Restore", 10))
        a.click_el(a.find_text(".tree-item", "Keeper", 10))
        a.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('worth keeping')", 20,
               "restored with its text")
        b.go(env.url.rstrip("/") + path)
        b.wait("return document.querySelector('.ed') && document.querySelector('.ed').textContent.includes('worth keeping')", 20,
               "and vic has it back too")
        check(not severe_logs(a, b), "no browser errors")
    finally:
        a.close()
        b.close()


def test_device_code(env):
    """Approving a device sign-in shows when and from where it was asked for."""
    import urllib.parse, urllib.request
    a = Browser()
    try:
        signup(a, uniq("wes"))
        body = urllib.parse.urlencode({"client_name": "test laptop"}).encode()
        with urllib.request.urlopen(env.url + "api/v1/oauth/device_authorization", body) as r:
            code = json.load(r)["user_code"]
        a.go(env.url + "device?code=" + code)
        a.find_text(".alert", "test laptop", 10)
        origin = a.text(".device-origin")
        check("just now" in origin and "127.0.0.1" in origin, f"the code was requested just now from 127.0.0.1 ({origin})")
        a.click_el(a.find_text(".alert .btn", "Deny"))
        a.find_text(".alert", "Request denied.", 10)
        check(True, "denied")
    finally:
        a.close()


TESTS = [test_onboarding, test_collab, test_devices_and_recovery, test_features, test_attachments, test_attachments_beside_notes, test_export_import, test_notion_import, test_delete_account, test_move_to_vault, test_note_sharing, test_tables, test_code_blocks, test_html, test_callouts, test_math, test_diagrams, test_colors, test_code_wrapping, test_vim, test_line_numbers, test_bullets, test_editor_mouse, test_editor_safety, test_local_copy, test_ui_basics, test_offline_tabs, test_offline_start, test_mobile, test_home_screen, test_quick_notes, test_vault_cards, test_vault_icons, test_entry_icons, test_tree_order, test_session_ended_while_away, test_outdated, test_comments, test_mentions, test_dates, test_graph, test_recovered_copy, test_note_width, test_admin, test_vault_storage, test_vault_admins, test_owners, test_undo_purge, test_device_code]


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
