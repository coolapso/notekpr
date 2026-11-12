#!/usr/bin/env python3
"""How the web client copes while a big import arrives (not part of the
suite: timings depend on the machine, so nothing is asserted).

    python3 tests/e2e/import_perf.py --server target/release/notekpr-server \
        --cli target/release/notekpr --web dist/web [--notes 1200] [--files 1200]

A browser has a vault open, a long note in the editor and its panel
(backlinks, graph) beside it. Another user's CLI mirrors the vault and
receives an export the way a tool writing one does: NOTES linked Markdown
notes in nested folders and FILES other files, BATCH files every EVERY
seconds. Each arrives in the browser as live updates. Printed: how busy the
page was per 10 s, its longest task, the longest it didn't answer, and how
long after the last file it settled. Before the fix for this, a 2,400-file
import kept the page frozen for many minutes.
"""
import argparse, collections, json, os, random, shutil, signal, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import wd  # noqa: E402
from suite import Env, uniq, check, share, cli_accepts, new_note  # noqa: E402
from wd import Browser, signup  # noqa: E402

OBSERVE = """
window.__lt = [];
new PerformanceObserver(l => { for (const e of l.getEntries()) window.__lt.push([e.startTime, e.duration]); })
  .observe({type: 'longtask'});
window.__lag = 0;
let last = performance.now();
setInterval(() => { const now = performance.now(); window.__lag = Math.max(window.__lag, now - last - 100); last = now; }, 100);
"""


def export(root, notes, files, seed=7):
    """An export like Notion's: pages with ids in their names, in nested
    folders, linking each other by relative Markdown links and by name;
    the database rows' files beside them."""
    rnd = random.Random(seed)
    folders = [""] + [f"Area {a}" for a in range(8)] + [f"Area {a}/Project {p}" for a in range(8) for p in range(4)]
    pages = [(rnd.choice(folders), f"Page {i} {rnd.getrandbits(64):016x}") for i in range(notes)]
    for folder, name in pages:
        links = []
        for _ in range(3):
            f, n = rnd.choice(pages)
            rel = os.path.relpath(os.path.join(f, n + ".md"), folder or ".").replace(" ", "%20")
            links.append(f"- [{n}]({rel})")
        links.append(f"- [[{rnd.choice(pages)[1]}]]")
        body = f"# {name}\n\n" + "\n".join(links) + "\n\n" + "Some text of the page. " * 20 + "\n"
        path = os.path.join(root, folder, name + ".md")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(body)
    for i in range(files):
        folder = rnd.choice(folders)
        path = os.path.join(root, folder, "Source", f"row-{i}.json")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            json.dump({"id": i, "title": f"Row {i}", "props": list(range(20))}, f)
    return sorted(os.path.relpath(os.path.join(d, n), root) for d, _, ns in os.walk(root) for n in ns)


def end_browser(driver_pid):
    """A frozen page can keep its browser from quitting with the session:
    end what chromedriver started (the browser's other processes follow)."""
    for d in filter(str.isdigit, os.listdir("/proc")):
        try:
            with open(f"/proc/{d}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            if ppid == driver_pid:
                os.kill(int(d), signal.SIGKILL)
        except (OSError, ValueError, IndexError):
            pass


def ask(b, script, *args):
    """What the page answers, or None if it didn't in time."""
    try:
        return b.js(script, *args)
    except TimeoutError:
        return None
    except RuntimeError as e:
        if "timeout" in str(e):
            return None
        raise


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", default="target/release/notekpr-server")
    ap.add_argument("--cli", default="target/release/notekpr")
    ap.add_argument("--web", default="dist/web")
    ap.add_argument("--notes", type=int, default=1200)
    ap.add_argument("--files", type=int, default=1200)
    ap.add_argument("--batch", type=int, default=25)
    ap.add_argument("--every", type=float, default=1.0)
    ap.add_argument("--quiet", type=int, default=20, help="seconds without a long task that count as settled")
    ap.add_argument("--limit", type=int, default=1200, help="give up after this many seconds")
    ap.add_argument("--frozen", type=int, default=120, help="a page that doesn't answer for this long is frozen")
    args = ap.parse_args()
    # A busy page takes its time to answer: wait for it, up to a point.
    wd.TIMEOUT = args.frozen
    env = Env(args.server, args.web)
    home = tempfile.mkdtemp(prefix="notekpr-import-")
    cenv = dict(os.environ, NOTEKPR_CONFIG_DIR=f"{home}/config", NOTEKPR_DATA_DIR=f"{home}/data")
    cli_bin = os.path.abspath(args.cli)
    cli = lambda *a: subprocess.run([cli_bin, "-p", "bob", *a], env=cenv, capture_output=True, text=True, timeout=120)
    b = Browser(1600, 1000)
    daemon = None
    try:
        wd.req("POST", b.p("/timeouts"), {"script": args.frozen * 1000})
        an, bn = uniq("amy"), uniq("bert")
        signup(b, an)
        r = cli("login", "--server", env.url.rstrip("/"), "--dev-user", bn, "--device-name", "laptop")
        check(r.returncode == 0, "CLI login")
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "New vault"))
        b.type(".modal input", "Imported")
        b.click(".modal button[type=submit]")
        b.find_text(".page-head h1", "Imported", 15)
        new_note(b, "Plan", "")
        long_note = "\n".join(f"- [ ] task {i} with a [[link {i}]] and some words" for i in range(500))
        b.js("const ed=document.querySelector('.ed'); ed.focus(); const s=getSelection(); s.selectAllChildren(ed); s.collapseToEnd();"
             "const dt = new DataTransfer(); dt.setData('text/plain', arguments[0]);"
             "ed.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true}));", long_note)
        share(b, ".sb-bottom button:nth-child(2)", bn, by=cli_accepts(cli))
        mirror = f"{home}/notes"
        check(cli("sync", "--dir", mirror, "--once", "--plaintext-ok").returncode == 0, "CLI mirror")
        log = open(f"{home}/daemon.log", "w")
        daemon = subprocess.Popen([cli_bin, "-p", "bob", "sync", "--dir", mirror], env=cenv, stdout=log, stderr=log)
        time.sleep(4)
        b.click_el(b.find_text(".tree-item", "Plan", 10))
        b.find(".ed .ln", 10)
        time.sleep(2)
        src = f"{home}/export"
        files = export(src, args.notes, args.files)
        print(f"{args.notes} notes and {len(files) - args.notes} other files, {args.batch} every {args.every}s", flush=True)
        b.js(OBSERVE)
        t0 = time.time()
        dest = f"{mirror}/Imported/Export"
        for i in range(0, len(files), args.batch):
            for f in files[i:i + args.batch]:
                os.makedirs(os.path.dirname(os.path.join(dest, f)), exist_ok=True)
                shutil.copy2(os.path.join(src, f), os.path.join(dest, f))
            time.sleep(args.every)
        copied = time.time() - t0
        print(f"copied in {copied:.0f}s; waiting for the page to settle", flush=True)
        # Settled: for a while, the daemon had nothing more to say (it
        # reports each pass that took files in) and the page no long task.
        settled, size, changed = None, -1, time.time()
        while time.time() - t0 < args.limit:
            time.sleep(2)
            if os.path.getsize(f"{home}/daemon.log") != size:
                size, changed = os.path.getsize(f"{home}/daemon.log"), time.time()
            recent = ask(b, "return window.__lt.filter(x => x[0] > performance.now() - arguments[0] * 1000).length", args.quiet)
            if recent is None:
                print(f"the page didn't answer for {args.frozen}s, {time.time() - t0:.0f}s after the import began: frozen")
                return
            if recent == 0 and time.time() - changed > args.quiet and time.time() - t0 > copied + args.quiet:
                settled = time.time() - t0 - args.quiet
                break
        lt = b.js("return window.__lt")
        origin = b.js("return performance.timeOrigin") / 1000
        busy = collections.Counter()
        for start, d in lt:
            busy[int((origin + start / 1000 - t0) // 10)] += d
        print("busy per 10 s (tasks over 50 ms):", " ".join(f"{int(busy[k] / 100)}%" for k in range(max(busy, default=-1) + 1)) or "none")
        print(f"long tasks: {len(lt)}, {sum(d for _, d in lt) / 1000:.1f}s in all, longest {max((d for _, d in lt), default=0):.0f} ms")
        print(f"longest the page didn't answer: {b.js('return window.__lag'):.0f} ms")
        if settled is None:
            print(f"not settled after {args.limit}s")
        else:
            print(f"settled {max(0, settled - copied):.0f}s after the last file")
        # Everything arrived (as the page has it, no reload).
        b.click(".vault-btn")
        b.click_el(b.find_text(".vault-menu button", "All vaults", 10))
        card = b.find_text(".card", "Imported", 10)
        shown = wd.req("GET", b.p(f"/element/{card}/text"))
        print(f"the vault's card says: {' · '.join(shown.splitlines()[1:])} (the import has {args.notes}, plus Plan)")
    finally:
        if daemon:
            daemon.terminate()
        b.close()
        end_browser(env.driver.pid)
        env.close()
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(env.data, ignore_errors=True)


if __name__ == "__main__":
    main()
