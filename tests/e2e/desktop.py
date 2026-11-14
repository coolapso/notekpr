#!/usr/bin/env python3
"""The desktop app (crates/desktop, docs/desktop.md), driven by WebKitGTK's
WebDriver in a headless sway: choosing the server, signing in, its own
settings, the Markdown folder both ways, starting again, signing out and
forgetting the computer — against a development server.

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
        found = wait_for(f"{css} containing {text!r}", lambda: self.js(
            "return Array.from(document.querySelectorAll(arguments[0])).find(e => e.textContent.includes(arguments[1])) || null",
            css, text), timeout)
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

    folder_profile = f"{desk_data}/{profile}/folder/config/profiles/folder.json"
    a = None
    try:
        # ---- the server: chosen once, then the app starts again
        a = App(app_bin, profile)
        a.find("input.server-address", 20)
        a.type("input.server-address", url)
        a.click(".auth-card button[type=submit]")
        settings = f"{desk_config}/{profile}/settings.json"
        wait_for("the server to be kept", lambda: os.path.exists(settings) and json.load(open(settings)).get("server") == url)
        check(True, "the server is chosen in the app, which starts again with it")
        a.quit()
        # The started-again app, which WebKitWebDriver doesn't know of.
        for p in subprocess.run(["pgrep", "-f", f"notekpr-desktop --profile {profile}"],
                                capture_output=True, text=True).stdout.split():
            subprocess.run(["kill", p])

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
        a.find(".ed .ln", 20)
        check(True, "signed in through the server's API from the app's own page (CORS), keys made")

        # ---- what a script in the window can ask of the app
        def invoke(cmd, args):
            return a.js("return window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1])"
                        ".then(v => ({ok: v === undefined ? null : v}), e => ({err: String(e)}))", cmd, args)
        r = invoke("open_external", {"url": "file:///etc/passwd"})
        check("err" in r, f"no file opens from the app, only web and mail addresses ({r})")
        r = invoke("folder_start", {"handoff": {"user_id": "x", "display_name": "x", "device_id": "x", "device_secret": "",
                                                "account": "", "token": "x", "expires_at": 0}, "pick": 7})
        check("err" in r and "choose the folder again" in r["err"], f"no folder without the system's picker ({r})")
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
        autostart = f"{home}/home/.config/autostart/notekpr ({profile}).desktop"
        wait_for("the autostart entry", lambda: os.path.exists(autostart))
        check("--hidden" in open(autostart).read(), "start at login: an autostart entry that starts it in the tray")
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
        shutil.rmtree(runtime, ignore_errors=True)


def _open(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
        return True
    except OSError:
        return False


if __name__ == "__main__":
    main()
