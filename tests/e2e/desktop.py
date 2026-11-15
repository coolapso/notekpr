#!/usr/bin/env python3
"""The desktop app (crates/desktop, docs/desktop.md), driven by WebKitGTK's
WebDriver in a headless sway: choosing the server, signing in, its own
settings, its initial setup, the Markdown folder both ways, starting again,
signing out and forgetting the computer — against a development server.
Then notes only on this computer: notes already in the folder brought in,
the app's own server, the folder following its new address at each start,
and switching to the server and back, where the setup takes over the
Markdown folder a running CLI keeps for that account.

    python3 tests/e2e/desktop.py --server target/debug/notekpr-server \
        --app target/debug/notekpr-desktop

The app must be a debug build with the web client inside (run.sh builds
it): its system dialogs (the folder picker, its questions) are answered by
NOTEKPR_DESKTOP_TEST_FOLDER, which only debug builds read. Needs sway,
WebKitWebDriver (Arch: webkit2gtk-4.1; Debian, Ubuntu: webkit2gtk-driver)
and dbus-run-session. Nothing reaches the desktop it runs on: its own
display, D-Bus and home folder (autostart entries go in ~/.config/autostart
whatever XDG_CONFIG_HOME says), and a profile of its own.
"""
import argparse, json, os, shutil, signal, socket, subprocess, sys, tempfile, time
import urllib.error, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import wd  # noqa: E402
from suite import check, uniq  # noqa: E402

TOOLS = ("sway", "WebKitWebDriver", "dbus-run-session")


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def http_status(url, method="GET", headers=None, body=None):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def restarting(act):
    """Do `act`, which starts the app again: it may end the WebDriver
    session before its reply."""
    try:
        act()
    except RuntimeError as e:
        if "terminated" not in str(e):
            raise


def app_pids(profile):
    return set(subprocess.run(["pgrep", "-f", f"notekpr-desktop --profile {profile}"],
                              capture_output=True, text=True).stdout.split())


def kill_restarted(profile, before):
    """The app started again by itself (a process not among `before`, the
    ones WebKitWebDriver started), once what ran stopped: then it goes, as
    WebKitWebDriver doesn't know of it."""
    wait_for("the app to start again by itself", lambda: app_pids(profile) - before)
    for p in app_pids(profile):
        subprocess.run(["kill", p])
    wait_for("the app to quit", lambda: not app_pids(profile))


def wait_for(what, cond, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        v = cond()
        if v:
            return v
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


class App(wd.Browser):
    """A window of the desktop app, through WebKitWebDriver (which starts
    the app itself)."""

    def __init__(self, binary, profile):
        caps = {"capabilities": {"alwaysMatch": {"webkitgtk:browserOptions": {
            "binary": binary, "args": ["--profile", profile]}}}}
        self.sid = wd.req("POST", "/session", caps)["sessionId"]

    def quit(self):
        try:
            wd.req("DELETE", f"/session/{self.sid}")
        except RuntimeError:
            pass

    def find_text(self, css, text, timeout=10):
        # WebKitWebDriver's element text is empty for these: find it in the page.
        def look():
            try:
                return self.js(
                    "return Array.from(document.querySelectorAll(arguments[0])).find(e => e.textContent.includes(arguments[1])) || null",
                    css, text)
            except RuntimeError as e:
                # Re-rendered while it was being handed over: look again.
                if "stale element" in str(e):
                    return None
                raise
        found = wait_for(f"{css} containing {text!r}", look, timeout)
        return list(found.values())[0]

    def route(self, path):
        self.js("history.pushState(null, '', arguments[0]); dispatchEvent(new PopStateEvent('popstate'))", path)

    def folder_status(self):
        return self.js("return document.querySelector('.folder-status, .settings-body .alert')?.textContent || ''") or ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/notekpr-server")
    ap.add_argument("--app", default="target/debug/notekpr-desktop")
    ap.add_argument("--cli", default="target/debug/notekpr")
    args = ap.parse_args()
    missing = [t for t in TOOLS if not shutil.which(t)]
    if missing:
        print(f"desktop: skipped, needs {', '.join(missing)}")
        return
    app_bin = os.path.abspath(args.app)
    a_cli = args.cli
    home = tempfile.mkdtemp(prefix="notekpr-desktop-")
    # A Wayland socket's path must be short: its own folder under /tmp.
    runtime = tempfile.mkdtemp(prefix="nk-", dir="/tmp")
    procs = []

    def start(cmd, env=None, log="log"):
        f = open(os.path.join(home, log), "a")
        # A group of its own, so what it starts goes with it (dbus-run-session's
        # WebKitWebDriver, and the app).
        p = subprocess.Popen(cmd, env=env, stdout=f, stderr=f, start_new_session=True)
        procs.append(p)
        return p

    port = free_port()
    url = f"http://localhost:{port}"
    start([os.path.abspath(args.server), "--dev-mode", "--data-dir", f"{home}/server",
           "--listen", f"127.0.0.1:{port}", "--public-url", url], log="server.log")
    wait_for("the server", lambda: _open(port))

    with open(f"{home}/sway.conf", "w") as f:
        f.write("output HEADLESS-1 resolution 1400x900\ndefault_border none\n")
    start(["sway", "-c", f"{home}/sway.conf"], log="sway.log",
          env=dict(os.environ, XDG_RUNTIME_DIR=runtime, WLR_BACKENDS="headless", WLR_RENDERER="pixman",
                   WLR_LIBINPUT_NO_DEVICES="1"))
    display = wait_for("sway", lambda: next((n for n in os.listdir(runtime)
                                             if n.startswith("wayland-") and not n.endswith(".lock")), None))

    notes = f"{home}/Notes"
    os.makedirs(notes)  # as the system's picker would give it: there already
    os.makedirs(f"{home}/home")
    profile = uniq("e2e")
    desk_config, desk_data = f"{home}/desktop-config", f"{home}/desktop-data"
    wd_port = free_port()
    os.environ["NOTEKPR_WEBDRIVER"] = f"http://127.0.0.1:{wd_port}"
    start(["dbus-run-session", "--", "WebKitWebDriver", f"--port={wd_port}"], log="webdriver.log",
          env=dict(os.environ, XDG_RUNTIME_DIR=runtime, WAYLAND_DISPLAY=display, GDK_BACKEND="wayland",
                   TAURI_WEBVIEW_AUTOMATION="true", NOTEKPR_DESKTOP_TEST_FOLDER=notes,
                   NOTEKPR_DESKTOP_CONFIG_DIR=desk_config, NOTEKPR_DESKTOP_DATA_DIR=desk_data,
                   HOME=f"{home}/home", XDG_CONFIG_HOME=f"{home}/home/.config",
                   XDG_DATA_HOME=f"{home}/home/.local/share", NOTEKPR_LOG="info"))
    wait_for("WebKitWebDriver", lambda: _open(wd_port))

    def settings_of(p):
        """The app's settings, with its accounts (kept apart, in accounts.json)."""
        s = json.load(open(f"{desk_config}/{p}/settings.json"))
        accounts = f"{desk_config}/{p}/accounts.json"
        if os.path.exists(accounts):
            s.update(json.load(open(accounts)))
        return s

    def folder_of(p, account):
        """An account's Markdown folder's profile (one per account)."""
        return f"{desk_data}/{p}/folders/{account}/config/profiles/folder.json"

    def through_setup(a, background):
        """An account's initial setup, all left for later: then its notes."""
        a.find_text(".desktop-setup h1", "Your notes as Markdown files", 20)
        a.click(".desktop-setup .not-now")
        if background:
            a.find_text(".desktop-setup h1", "In the background", 10)
            a.click(".desktop-setup .setup-done")
        a.find(".ed .ln", 20)

    a = None
    try:
        # ---- the server: chosen once, then the app starts again
        a = App(app_bin, profile)
        a.find("input.server-address", 20)
        before = app_pids(profile)
        a.type("input.server-address", url)
        restarting(lambda: a.click(".auth-card button[type=submit]"))
        settings = f"{desk_config}/{profile}/settings.json"
        wait_for("the server to be kept", lambda: os.path.exists(settings) and json.load(open(settings)).get("server") == url)
        check(True, "the server is chosen in the app, which starts again with it")
        folder_profile = folder_of(profile, settings_of(profile)["account"])
        kill_restarted(profile, before)
        a.quit()

        # ---- signing in (development mode), this computer's keys
        a = App(app_bin, profile)
        name = uniq("dora")
        a.find(".dev-box input", 20)
        a.type(".dev-box input", name)
        a.click(".dev-box button[type=submit]")
        a.click_el(a.find_text(".auth-card button", "Create my keys", 15))
        a.find(".recovery-key", 15)
        a.click(".check input")
        a.click_el(a.find_text(".auth-card button", "Continue"))
        check(True, "signed in through the server's API from the app's own page (CORS), keys made")

        # ---- the initial setup: once for the account, before its notes
        a.find_text(".desktop-setup h1", "Your notes as Markdown files", 20)
        check(a.js("return !!document.querySelector('.desktop-setup .choose-folder')"),
              "the account's setup offers a Markdown folder first")
        a.click(".desktop-setup .not-now")
        a.find_text(".desktop-setup h1", "In the background", 10)
        check(a.js("return !!document.querySelector('.desktop-setup .autostart')"),
              "then, the first time, starting at login and the tray")
        a.click(".desktop-setup .setup-done")
        a.find(".ed .ln", 20)
        mine = settings_of(profile)
        check(next(x for x in mine["accounts"] if x["id"] == mine["account"]).get("set_up")
              and mine.get("background_asked"), "each left for later, and not asked again by itself")

        # ---- the window names itself as its desktop entry is named
        # (packaging/desktop): how a desktop finds its name and icon.
        sock = next(os.path.join(runtime, n) for n in os.listdir(runtime) if n.startswith("sway-ipc."))
        tree = json.loads(subprocess.run(["swaymsg", "-s", sock, "-t", "get_tree"],
                                         capture_output=True, text=True).stdout)
        ids, nodes = [], [tree]
        while nodes:
            n = nodes.pop()
            ids += [n["app_id"]] if n.get("app_id") else []
            nodes += n.get("nodes", []) + n.get("floating_nodes", [])
        check(ids == ["md.notekpr.Notekpr"], f"the window's app id is md.notekpr.Notekpr ({ids})")

        # ---- what a script in the window can ask of the app
        def invoke(cmd, args):
            return a.js("return window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1])"
                        ".then(v => ({ok: v === undefined ? null : v}), e => ({err: String(e)}))", cmd, args)
        r = invoke("local_session", {})
        check("err" in r, f"on a server, the app makes no session of its own ({r})")
        r = invoke("open_external", {"url": "file:///etc/passwd"})
        check("err" in r, f"no file opens from the app, only web and mail addresses ({r})")
        r = invoke("folder_start", {"handoff": {"user_id": "x", "display_name": "x", "device_id": "x", "device_secret": "",
                                                "account": "", "token": "x", "expires_at": 0}, "pick": 7})
        check("err" in r and "choose the folder again" in r["err"], f"no folder without the system's picker ({r})")
        r = invoke("cli_folders", {"token": "not-a-session"})
        check("err" in r, f"which folders the CLI keeps, only for the person the server says the window is ({r})")
        r = invoke("plugin:dialog|open", {"options": {}})
        check("err" in r, f"none of the plugins' own commands ({r})")

        # ---- the app speaks of this computer, not a browser
        a.route("/settings/devices")
        a.find_text(".settings-body label", "Keep this computer usable offline", 10)
        check(True, "Devices offers to keep this computer usable offline")

        # ---- its own settings
        a.route("/settings/app")
        a.find_text(".settings-body h3", "In the background", 10)
        check(a.js("return document.querySelector('.close-to-tray').disabled"),
              "without a tray (no StatusNotifier host), closing to it can't be turned on")
        a.click(".autostart")
        entries = f"{home}/home/.config/autostart"
        # (A copy with folders of its own, as here: an entry of its own.)
        found = lambda: next((f"{entries}/{n}" for n in os.listdir(entries)
                              if n.startswith("notekpr (development ") and n.endswith(f", {profile}).desktop")), None) \
            if os.path.isdir(entries) else None
        autostart = wait_for("the autostart entry", found)
        entry = open(autostart).read()
        check("--hidden" in entry, "start at login: an autostart entry that starts it in the tray")
        check(f"--config-dir {desk_config}" in entry and f"--data-dir {desk_data}" in entry,
              "with this copy's own folders, apart from an installed app's")
        a.click(".autostart")
        wait_for("the autostart entry to go", lambda: not os.path.exists(autostart))
        check(True, "turned off, the entry goes")

        # ---- the Markdown folder
        a.click_el(a.find_text(".settings-body button", "Choose a folder", 10))
        wait_for("the folder to sync", lambda: "Keeping" in a.folder_status())
        check("vault" in a.folder_status(), f"the app keeps the Markdown folder in sync ({a.folder_status()})")
        # The CLI on the same computer sees it, as the app sees the CLI's:
        # one folder, one keeper.
        cenv = dict(os.environ, NOTEKPR_CONFIG_DIR=f"{home}/cli/config", NOTEKPR_DATA_DIR=f"{home}/cli/data",
                    NOTEKPR_DESKTOP_CONFIG_DIR=desk_config, NOTEKPR_DESKTOP_DATA_DIR=desk_data)
        cli = lambda *args: subprocess.run([os.path.abspath(a_cli), *args], env=cenv, capture_output=True,
                                           text=True, timeout=60)
        r = cli("login", "--server", url, "--dev-user", uniq("cleo"), "--device-name", "laptop")
        check(r.returncode == 0, "a CLI user signs in on the same computer")
        r = cli("sync", "--dir", notes, "--once", "--plaintext-ok")
        check(r.returncode != 0 and "the desktop app" in r.stderr,
              f"the CLI won't sync the folder the app keeps ({r.stderr.strip()[-160:]})")
        # The folder says so itself: a CLI that doesn't know where this
        # copy of the app keeps its settings is refused too.
        marker = json.load(open(f"{notes}/.notekpr-folder"))
        check(marker["by"].startswith("the desktop app (development "),
              f"the folder names its keeper in .notekpr-folder ({marker['by']})")
        blind = {k: v for k, v in cenv.items() if not k.startswith("NOTEKPR_DESKTOP_")}
        r = subprocess.run([os.path.abspath(a_cli), "sync", "--dir", notes, "--once", "--plaintext-ok"],
                           env=blind, capture_output=True, text=True, timeout=60)
        check(r.returncode != 0 and "the desktop app (development" in r.stderr,
              f"nor a CLI that knows nothing of this copy of the app ({r.stderr.strip()[-160:]})")
        r = subprocess.run([os.path.abspath(a_cli), "sync", "--dir", notes, "--once", "--plaintext-ok",
                            "--take-over"], env=blind, capture_output=True, text=True, timeout=60)
        check(r.returncode != 0 and "right now" in r.stderr,
              f"not even told to take it over, while the app keeps it ({r.stderr.strip()[-160:]})")
        subprocess.run([os.path.abspath(a_cli), "sync", "--no-dir"], env=blind, capture_output=True, timeout=60)
        welcome = f"{notes}/My notes/Welcome.md"
        wait_for("Welcome.md", lambda: os.path.exists(welcome))
        check(True, "the vault's notes are written into it as Markdown files")
        check(json.load(open(folder_profile)).get("device_id") is not None, "with this computer's device")
        # A script's word that the window signed out only stops it for now.
        invoke("signed_out", {})
        check("session_token" in json.load(open(folder_profile)),
              "a window saying it signed out, while the server still takes the session, doesn't end the folder's")
        # What the window says each time it opens (its session, bound to
        # this device) starts it again.
        token = a.js("return sessionStorage.getItem('notekpr.token')")
        invoke("session_changed", {"token": token, "expires_at": 0})
        wait_for("the folder after the window's session", lambda: "Keeping" in a.folder_status())
        check(True, "the window opening again starts it again")
        a.click_el(a.find_text(".tree-item", "Welcome", 10))
        a.find(".ed .ln", 10)
        with open(welcome, "a") as f:
            f.write("\nEdited in my own editor.\n")
        a.wait("return document.querySelector('.ed').textContent.includes('Edited in my own editor.')", 20,
               "the file's edit in the window")
        check(True, "a file edited on disk reaches the app's window")
        a.js("const e = document.querySelector('.ed'); e.focus(); const s = getSelection(); s.selectAllChildren(e); s.collapseToEnd();")
        a.keys(wd.KEYS["enter"] + "Typed in the app.")
        wait_for("the window's edit in the file", lambda: "Typed in the app." in open(welcome).read())
        check(True, "typing in the window reaches the file")

        # ---- starting again: the folder goes on by itself
        a.quit()
        a = App(app_bin, profile)
        a.find(".ed .ln", 20)
        a.route("/settings/app")
        wait_for("the folder after a start", lambda: "Keeping" in a.folder_status())
        check(True, "after starting again the folder is kept in sync without asking")
        # (With a note open: the file tree isn't kept current under Settings.)
        a.click_el(a.find_text(".tree-item", "Welcome", 10))
        a.find(".ed .ln", 10)
        with open(f"{notes}/My notes/From disk.md", "w") as f:
            f.write("# Written while it ran\n")
        a.find_text(".tree-item", "From disk", 20)
        check(True, "a new file becomes a note")

        # ---- signing out stops it; signing in again resumes it
        a.route("/settings/account")
        a.click_el(a.find_text(".settings-body button", "Sign out", 10))
        a.find(".dev-box input", 15)
        wait_for("the folder's session to go", lambda: "session_token" not in json.load(open(folder_profile)))
        check(True, "signing out stops the folder and forgets its session")
        a.type(".dev-box input", name)
        a.click(".dev-box button[type=submit]")
        a.find(".sb-bottom", 20)
        a.route("/settings/app")
        wait_for("the folder after signing in", lambda: "Keeping" in a.folder_status())
        check(True, "signing in again: the folder goes on with the new session")

        # ---- forgetting this computer forgets the folder's keys; the files stay
        a.route("/settings/account")
        a.click_el(a.find_text(".settings-body button", "Forget this computer", 10))
        check("Forget this computer?" in a.answer(), "forgetting this computer asks first, in the app's own dialog")
        a.find(".dev-box input", 15)
        wait_for("the folder's profile to go", lambda: not os.path.exists(folder_profile))
        check(os.path.exists(welcome), "forgetting this computer forgets the folder's keys and copies; the files stay")

        # ---- only on this computer: no server, no account
        a.quit()
        shutil.rmtree(notes)
        os.makedirs(notes)
        here = uniq("here")
        here_settings = f"{desk_config}/{here}/settings.json"
        here_folder = folder_of(here, "this-computer")
        a = App(app_bin, here)
        before = app_pids(here)
        keep_here = a.find_text(".auth-card button", "Keep them on this computer", 20)
        restarting(lambda: a.click_el(keep_here))
        wait_for("this computer to be chosen",
                 lambda: os.path.exists(here_settings) and json.load(open(here_settings)).get("this_computer"))
        kill_restarted(here, before)
        a.quit()
        a = App(app_bin, here)
        a.find_text(".auth-card button", "Create my keys", 20)
        check(not a.js("return [...document.querySelectorAll('.auth-card button')].some(b => b.textContent.includes('Sign out'))"),
              "no sign-in and nothing to sign out of: straight to making the keys")
        a.click_el(a.find_text(".auth-card button", "Create my keys"))
        a.find(".recovery-key", 15)
        a.click(".check input")
        a.click_el(a.find_text(".auth-card button", "Continue"))
        through_setup(a, background=True)
        check(True, "notes only on this computer: keys made, a recovery key shown, the notes open")
        info = invoke("desktop_info", {})["ok"]
        own = info["server"]
        check(info["this_computer"] and own.startswith("http://127.0.0.1:"),
              f"the app runs its own server on 127.0.0.1 ({own})")
        port = own.rsplit(":", 1)[1]
        check(http_status(f"{own}/api/v1/auth/dev-login", "POST", {"Content-Type": "application/json"},
                          b'{"username":"mallory"}') == 404, "its server has no sign-in")
        check(http_status(f"{own}/api/v1/info", headers={"Host": f"localhost:{port}"}) == 421,
              "its server answers only requests to its own address")
        check(http_status(f"{own}/api/v1/info", headers={"Origin": "https://evil.example"}) == 403,
              "its server answers no web page")
        check("err" in invoke("sign_in", {}), "and the app opens no browser to sign in")

        # ---- what needs a server says so
        a.route("/settings/devices")
        a.find_text(".settings-body .needs-server", "needs a notekpr server", 10)
        a.route("/settings/account")
        a.find_text(".settings-body h3", "Version", 10)
        check(not a.js("return [...document.querySelectorAll('.settings-body button')].some(b => /Sign out|Forget this/.test(b.textContent))"),
              "other devices need a server, and there is nothing to sign out of")

        # ---- the Markdown folder, with notes in it already (an Obsidian vault)
        a.route("/settings/app")
        a.find_text(".accounts .account.open", "On this computer", 10)
        with open(f"{notes}/Ideas.md", "w") as f:
            f.write("# Ideas\n\nFrom Obsidian.\n")
        os.makedirs(f"{notes}/.obsidian")
        with open(f"{notes}/.obsidian/app.json", "w") as f:
            f.write("{}\n")
        a.click_el(a.find_text(".settings-body button", "Choose a folder", 10))
        a.find_text(".bring-in .folder-notes", "holds notes already", 10)
        said = a.js("return document.querySelector('.bring-in .folder-notes').textContent")
        check("1 note" in said and ".obsidian" in said, f"a folder with notes: what it holds, and what to do with them ({said})")
        a.click(".bring-in .bring-in-button")
        wait_for("the folder to sync", lambda: "Keeping" in a.folder_status())
        welcome = f"{notes}/My notes/Welcome.md"
        wait_for("Welcome.md", lambda: os.path.exists(welcome))
        check(os.path.exists(f"{notes}/My notes/Ideas.md") and os.path.exists(f"{notes}/My notes/.obsidian/app.json")
              and not os.path.exists(f"{notes}/Ideas.md"),
              "its notes moved as they were, hidden files too, into the vault's folder there")
        check(True, "the Markdown folder works with notes only on this computer")
        a.quit()
        a = App(app_bin, here)
        a.find(".ed .ln", 20)
        again = invoke("desktop_info", {})["ok"]["server"]
        check(again != own, f"another port at each start ({own} → {again})")
        a.find_text(".tree-item", "Ideas", 20)
        check(True, "the notes brought in are the vault's")
        a.click_el(a.find_text(".tree-item", "Welcome", 10))
        a.find(".ed .ln", 10)
        with open(f"{notes}/My notes/Kept here.md", "w") as f:
            f.write("# Only on this computer\n")
        a.find_text(".tree-item", "Kept here", 20)
        check(json.load(open(here_folder)).get("server") == again,
              "the folder follows the server to its new address, without asking")

        # ---- accounts: one on the server too, each keeping its own
        a.route("/settings/app")
        before = app_pids(here)
        a.click_el(a.find_text(".settings-body button", "Add an account", 10))
        a.type(".modal input.server-address", url)
        restarting(lambda: a.click(".modal form.connect button[type=submit]"))
        wait_for("the server's account to open", lambda: settings_of(here).get("server") == url
                 and not settings_of(here).get("this_computer"))
        kill_restarted(here, before)
        a.quit()
        check(os.path.exists(here_folder), "switching keeps this computer's Markdown folder, for coming back")
        a = App(app_bin, here)
        name2 = uniq("ines")
        a.find(".dev-box input", 20)
        a.type(".dev-box input", name2)
        a.click(".dev-box button[type=submit]")
        a.click_el(a.find_text(".auth-card button", "Create my keys", 15))
        a.find(".recovery-key", 15)
        check(True, "the account on the server: signed in there, with keys of its own")

        # ---- coming from the CLI: it keeps a Markdown folder for this
        # account, in the background; the setup takes it over
        with open(f"{home}/ines.key", "w") as f:
            f.write(a.js("return document.querySelector('.recovery-key').textContent"))
        henv = {k: v for k, v in os.environ.items() if not k.startswith("NOTEKPR_")}
        henv.update(HOME=f"{home}/home", XDG_CONFIG_HOME=f"{home}/home/.config",
                    XDG_DATA_HOME=f"{home}/home/.local/share")
        cli_bin = os.path.abspath(a_cli)
        r = subprocess.run([cli_bin, "login", "--server", url, "--dev-user", name2,
                            "--recovery-key-file", f"{home}/ines.key"], env=henv, capture_output=True, text=True,
                           timeout=60)
        check(r.returncode == 0, f"the CLI is signed in as the same person on this computer ({r.stderr.strip()[-200:]})")
        cli_notes = f"{home}/home/CliNotes"
        cli_profile = f"{home}/home/.config/notekpr/profiles/default.json"
        cli_sync = start([cli_bin, "sync", "--dir", cli_notes, "--plaintext-ok"], env=henv, log="cli-sync.log")
        wait_for("the CLI to keep its folder", lambda: os.path.exists(f"{cli_notes}/.notekpr-folder"))
        a.click(".check input")
        a.click_el(a.find_text(".auth-card button", "Continue"))
        cli_welcome = f"{cli_notes}/My notes/Welcome.md"
        wait_for("the CLI to write the first vault", lambda: os.path.exists(cli_welcome), 30)
        a.find_text(".desktop-setup .cli-offer", "The CLI keeps ~/CliNotes in sync for you", 20)
        check(True, "the setup finds the CLI's folder for this account, and offers to take it over")
        a.click(".desktop-setup .take-over")
        a.find_text(".desktop-setup .cli-wait .cli-command", "systemctl --user disable --now notekpr-sync@default", 20)
        check(True, "where it can't stop the CLI's background sync itself, it says how, and waits")
        check(json.load(open(cli_profile))["sync"].get("mirror_dir") == cli_notes,
              "nothing changes while the CLI still runs")
        os.killpg(cli_sync.pid, signal.SIGTERM)
        cli_sync.wait(30)
        wait_for("the app to keep it", lambda: "Keeping" in a.folder_status(), 60)
        check("mirror_dir" not in json.load(open(cli_profile))["sync"],
              "once the CLI stopped: it forgot the folder, as `sync --no-dir` does")
        marker = json.load(open(f"{cli_notes}/.notekpr-folder"))
        check(marker["by"].startswith("the desktop app (development "), f"and the app keeps it ({marker['by']})")
        names = [n for _, ds, fs in os.walk(cli_notes) for n in ds + fs]
        check(os.path.exists(cli_welcome) and not any("(" in n or "conflict" in n.lower() for n in names),
              f"the vaults' folders and files taken over as they were: no second copy, no conflict ({sorted(names)})")
        r = subprocess.run([cli_bin, "sync", "--dir", cli_notes, "--once", "--plaintext-ok"], env=henv,
                           capture_output=True, text=True, timeout=60)
        check(r.returncode != 0 and "the desktop app" in r.stderr, "the CLI is refused it from then on")
        with open(cli_welcome, "a") as f:
            f.write("\nEdited after the app took over.\n")
        a.click(".desktop-setup .setup-next")
        a.find(".ed .ln", 20)
        a.wait("return document.querySelector('.ed').textContent.includes('Edited after the app took over.')", 20,
               "the file's edit in the window")
        check(True, "the app keeps the folder in sync: an edit there reaches the window")
        # (Development sign-in names people with a capital.)
        wait_for("its name in the list", lambda: any(x.get("server") == url and (x.get("name") or "").lower() == name2
                                                     for x in settings_of(here)["accounts"]))
        server_id, shown = next((x["id"], x["name"]) for x in settings_of(here)["accounts"] if x.get("server") == url)
        check(server_id != "this-computer" and len(server_id) > 32, f"a random id of its own ({server_id})")
        before = app_pids(here)
        a.click(".user-btn")
        item = a.find_text(".menu .account-item", "On this computer", 10)
        restarting(lambda: a.click_el(item))
        wait_for("this computer again", lambda: settings_of(here).get("this_computer"))
        kill_restarted(here, before)
        a.quit()
        a = App(app_bin, here)
        a.find_text(".tree-item", "Kept here", 20)
        check(True, "switched from the menu behind one's name: this computer's notes, no sign-in")
        a.route("/settings/app")
        wait_for("its folder again", lambda: "Keeping" in a.folder_status())
        check(True, "and its Markdown folder goes on, without asking")
        a.find_text(".accounts .account", f"{shown} on {url.split('://')[1]}", 10)
        check(True, "the list names the account on the server")

        # ---- removing an account only hides it
        a.click_el(a.find_text(".accounts .remove-account", "Remove", 10))
        wait_for("it to be hidden", lambda: any(x.get("server") == url and x.get("hidden")
                                                for x in settings_of(here)["accounts"]))
        a.wait("return ![...document.querySelectorAll('.accounts .account')].some(e => e.textContent.includes(arguments[0]))",
               10, "it to leave the list", shown)
        check(os.path.isdir(f"{desk_data}/{here}/webview/{server_id}"), "removed from the list: what the app keeps for it stays")
        # ---- a server that can't be reached: which one, and the way out
        a.quit()
        gone = uniq("gone")
        dead = f"http://localhost:{free_port()}"
        os.makedirs(f"{desk_config}/{gone}")
        with open(f"{desk_config}/{gone}/settings.json", "w") as f:
            json.dump({"server": dead}, f)  # as 0.17.0 leaves it: one server
        a = App(app_bin, gone)
        a.find_text(".auth-card .cant-reach", f"can't reach {dead}", 20)
        a.find_text(".auth-card button", "Try again")
        a.find_text(".accounts .account.open", dead.split("://")[1])
        check(True, "a server that can't be reached: the app names it, with Try again and the accounts")
        before = app_pids(gone)
        keep_here = a.find_text(".auth-card button", "Keep them on this computer", 10)
        restarting(lambda: a.click_el(keep_here))
        wait_for("this computer to open", lambda: settings_of(gone).get("this_computer"))
        kill_restarted(gone, before)
        a.quit()
        a = App(app_bin, gone)
        a.find_text(".auth-card button", "Create my keys", 20)
        check(len(settings_of(gone)["accounts"]) == 2, "and leads out of it, keeping the account it can't reach")
        print("desktop: all checks passed")
    except Exception:
        if a:
            shot = os.path.join(home, "failed.png")
            try:
                with open(shot, "wb") as f:
                    f.write(__import__("base64").b64decode(wd.req("GET", a.p("/screenshot"))))
                print(f"desktop: the window at the failure: {shot}")
            except Exception:
                pass
        raise
    finally:
        if a:
            a.quit()
        for p in reversed(procs):
            try:
                os.killpg(p.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for p in procs:
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
        # What the private D-Bus session started for it (portals, gvfs) runs in
        # groups of its own, and would outlive the test with its mounts.
        for pid in _ours(runtime):
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        time.sleep(1)
        with open("/proc/self/mounts") as f:
            for m in [line.split()[1] for line in f if line.split()[1].startswith(runtime + "/")]:
                subprocess.run(["fusermount3", "-u", "-z", m], stderr=subprocess.DEVNULL)
        shutil.rmtree(runtime, ignore_errors=True)


def _ours(runtime):
    """Our processes that run with the test's runtime folder."""
    mark = f"XDG_RUNTIME_DIR={runtime}".encode()
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                with open(f"/proc/{p}/environ", "rb") as f:
                    if mark in f.read().split(b"\0"):
                        yield int(p)
            except OSError:
                pass


def _open(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
        return True
    except OSError:
        return False


if __name__ == "__main__":
    main()
