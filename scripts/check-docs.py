#!/usr/bin/env python3
"""Build the docs site (docs/site) and check what it links to: every page and #anchor of the site,
and every file of the repository a doc points at on GitHub, exists; no doc has HTML the site drops.
Hugo itself refuses a doc missing from data/docs.yaml or a link that is neither to a doc nor into the
repository (--panicOnWarning). `task docs:check` runs this; python3 and hugo, nothing to install."""
import subprocess
import sys
import tempfile
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
SITE = ROOT / "docs/site"
BASE = "https://docs.notekpr.md/"
REPO = "https://github.com/coolapso/notekpr/"


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__()
        self.ids = set()
        self.links = []
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if attrs.get("id"):
            self.ids.add(attrs["id"])
        for key in ("href", "src"):
            if attrs.get(key):
                self.links.append(attrs[key])


def check(out):
    command = ["hugo", "--source", str(SITE), "--destination", str(out), "--baseURL", BASE,
               "--panicOnWarning"]
    build = subprocess.run(command, capture_output=True, text=True)
    if build.returncode:
        sys.exit(build.stdout + build.stderr + "The docs site doesn't build")
    pages = {path: Page(path.read_text()) for path in out.rglob("*.html")}
    problems = []
    links = 0
    for path, page in sorted(pages.items()):
        where = path.relative_to(out)
        if "raw HTML omitted" in path.read_text():
            problems.append(f"{where}: a doc has HTML, which the site leaves out (GitHub shows it)")
        for link in page.links:
            links += 1
            url = urlsplit(link)
            if link.startswith(REPO):
                rest = link.removeprefix(REPO)
                for kind in ("blob/main/", "tree/main/", "edit/main/"):
                    if rest.startswith(kind):
                        file = urlsplit(rest.removeprefix(kind)).path
                        if not (ROOT / unquote(file)).exists():
                            problems.append(f"{where}: {link}: there is no {file} in the repository")
                continue
            if url.scheme or url.netloc:
                continue
            target = page
            if url.path:
                if not url.path.startswith("/"):
                    problems.append(f"{where}: {link}: a relative link")
                    continue
                file = out / unquote(url.path.removeprefix("/"))
                if url.path.endswith("/"):
                    file /= "index.html"
                if not file.is_file():
                    problems.append(f"{where}: {link}: no such page")
                    continue
                target = pages.get(file)
            if url.fragment and target is not None and unquote(url.fragment) not in target.ids:
                problems.append(f"{where}: {link}: no #{url.fragment} on that page")
    return len(pages), links, problems


def main():
    with tempfile.TemporaryDirectory(prefix="notekpr-docs-") as out:
        pages, links, problems = check(Path(out))
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        sys.exit(f"{len(problems)} broken links in the docs site")
    print(f"Docs site: {pages} pages, {links} links, all found")


if __name__ == "__main__":
    main()
