#!/usr/bin/env python3
"""Browser user ↔ desktop CLI user with a live Markdown mirror.

    python3 tests/e2e/cross_client.py --server target/debug/coolpsonotes-server \
        --cli target/debug/coolpsonotes --web dist/web
"""
import argparse, os, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from suite import Env, uniq, check, share, new_note, severe_logs  # noqa: E402
from wd import Browser, KEYS, editor_text, signup  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/debug/coolpsonotes-server")
    ap.add_argument("--cli", default="target/debug/coolpsonotes")
    ap.add_argument("--web", default="dist/web")
    args = ap.parse_args()
    env = Env(args.server, args.web)
    home = tempfile.mkdtemp(prefix="cpn-cli-")
    cenv = dict(os.environ, COOLPSONOTES_CONFIG_DIR=f"{home}/config", COOLPSONOTES_DATA_DIR=f"{home}/data")
    cli = lambda *a, **kw: subprocess.run([os.path.abspath(args.cli), "-p", "bob", *a], env=cenv, capture_output=True, text=True, timeout=60, **kw)
    a = Browser()
    daemon = None
    try:
        an, bn = uniq("amy"), uniq("bert")
        signup(a, an)
        r = cli("login", "--server", env.url.rstrip("/"), "--dev-user", bn, "--device-name", "laptop")
        check(r.returncode == 0, f"CLI login + key setup ({r.stdout.strip().splitlines()[-1] if r.stdout.strip() else r.stderr[-200:]})")
        a.click(".vault-btn")
        a.click_el(a.find_text(".vault-menu button", "New vault"))
        a.type(".modal input", "Shared")
        a.click(".modal button[type=submit]")
        a.find_text(".page-head h1", "Shared", 15)
        new_note(a, "Plan", "from the browser")
        share(a, ".sb-bottom button:nth-child(2)", bn)
        mirror = f"{home}/notes"
        r = cli("sync", "--dir", mirror, "--once", "--plaintext-ok")
        check(r.returncode == 0, "CLI one-shot sync with plaintext mirror")
        path = f"{mirror}/Shared/Plan.md"
        check(os.path.exists(path) and open(path).read() == "from the browser", "note shared by the browser user appears as a Markdown file")
        r = cli("cat", "Shared", "Plan")
        check(r.stdout.strip() == "from the browser", "cli cat decrypts the note")
        daemon = subprocess.Popen([os.path.abspath(args.cli), "-p", "bob", "sync", "--dir", mirror], env=cenv,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(3)
        a.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();")
        a.keys(KEYS["enter"] + "typed live in the browser")
        for _ in range(60):
            if "typed live in the browser" in open(path).read():
                break
            time.sleep(0.25)
        check("typed live in the browser" in open(path).read(), "browser edits reach the mirrored file live")
        with open(path, "a") as f:
            f.write("\nedited in an external editor\n")
        a.wait("return document.querySelector('.ed').textContent.includes('edited in an external editor')", 20, "file edit reaches browser")
        check(True, "external file edits reach the browser live")
        with open(f"{mirror}/Shared/From disk.md", "w") as f:
            f.write("# Created on disk\n")
        a.find_text(".tree-item", "From disk", 20)
        check(True, "a new file on disk becomes a note for other users")
        time.sleep(1)
        check(editor_text(a).count("typed live in the browser") == 1, "no duplicated content after round trips")
        check(not severe_logs(a), "no browser errors")
        print("ALL PASSED")
    finally:
        if daemon:
            daemon.terminate()
        a.close()
        env.close()


if __name__ == "__main__":
    main()
