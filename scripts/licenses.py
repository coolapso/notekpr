#!/usr/bin/env python3
"""Write the third-party notices of everything notekpr ships, one list for every program
together: docs/licenses.md (the docs site's Licences page, and THIRD_PARTY_LICENSES.md in the
server's image and next to LICENSE in the packages) and crates/web/static/licenses.html (the
web app's Licences page, which scripts/build-web.sh stamps with the version).

What they list:
  * every crate the programs are built from (PROGRAMS: each for the targets it is released
    for; build dependencies too, development ones not), with the licence and notice files the
    crate ships (LICENSE*, COPYING*, NOTICE*, THIRD_PARTY_*, … and folders of them). Of a
    choice of licences ("MIT OR Apache-2.0"), only the files of the one notekpr takes: the
    first in PREFERRED that deny.toml allows.
  * for a crate that ships no licence text, the standard one (scripts/licenses/<SPDX id>.txt),
    with its authors as its Cargo.toml names them.
  * what notekpr's own code holds from elsewhere (OWN), and Rust's standard library: its
    licence, and the crates it is built from, which cargo doesn't show. Those are a copy of
    the standard library's notices, for the Rust the releases are built with
    (scripts/licenses/std/, which --std writes and the check compares with the Dockerfiles).

    python3 scripts/licenses.py              write both files (task licenses)
    python3 scripts/licenses.py --check      fail if either isn't what it would write
                                             (task licenses:check, which CI runs)
    python3 scripts/licenses.py --std 1.99.1 copy the standard library's notices of that Rust
                                             (task licenses:std: from GitHub and crates.io)

Python's standard library and cargo only (cargo downloads the crates it doesn't have yet).
docs/development.md, "Third-party notices", says more."""

import argparse
import hashlib
import html
import io
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
REPO = "https://github.com/coolapso/notekpr"
MARKDOWN = ROOT / "docs/licenses.md"
PAGE = ROOT / "crates/web/static/licenses.html"
STANDARD = ROOT / "scripts/licenses"
STD = STANDARD / "std"
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"

# The crates of crates.io Rust's standard library is built from (library/Cargo.lock) for the
# targets notekpr is released for: its backtraces (addr2line, gimli, object, rustc-demangle,
# miniz_oxide, adler2, memchr), its HashMap (hashbrown), libc and cfg-if, and dlmalloc, its
# allocator in WebAssembly. And the notices of its own compiler-builtins, ported from LLVM's
# compiler-rt and musl's libm, from the rust repository.
STD_CRATES = ["addr2line", "adler2", "cfg-if", "dlmalloc", "gimli", "hashbrown", "libc",
              "memchr", "miniz_oxide", "object", "rustc-demangle"]
STD_FILES = ["library/compiler-builtins/LICENSE.txt",
             "library/compiler-builtins/libm/LICENSE.txt"]
# The Rust the releases are built with: the Dockerfiles' (the image's, GoReleaser's and, from
# packaging/Dockerfile, macOS's).
RUST_IN = ["Dockerfile", "packaging/Dockerfile"]

LINUX = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
WINDOWS = ["x86_64-pc-windows-msvc"]
MACOS = ["aarch64-apple-darwin"]
WASM = ["wasm32-unknown-unknown"]

# The programs notekpr hands out: what each is called here, the crate it is built from (in
# the workspace of that Cargo.toml) and the targets it is released for (Dockerfile,
# .goreleaser.yaml, task build:macos). The web app is two WebAssembly modules: the app and
# the diagram module, a workspace of its own. The server's image and the desktop app have
# the web app inside.
PROGRAMS = [
    ("server", "Cargo.toml", "notekpr-server", LINUX),
    ("web app", "Cargo.toml", "notekpr-web", WASM),
    ("web app", "crates/diagram/Cargo.toml", "notekpr-diagram", WASM),
    ("CLI", "Cargo.toml", "notekpr-cli", LINUX + WINDOWS + MACOS),
    ("desktop app", "Cargo.toml", "notekpr-desktop", LINUX + WINDOWS + MACOS),
]
NAMES = ["server", "web app", "CLI", "desktop app"]

# What notekpr's own crates hold from elsewhere: the crate, a heading, a line about it and
# the files with its notices (a file and a line: that file from the line holding it to the
# end of its first comment).
OWN = [
    ("notekpr-draw", "Drawings",
     "Drawings port parts of rough.js, perfect-freehand, Excalidraw and other libraries (MIT), "
     "and of fdlibm and V8.",
     ["crates/draw/PORTED.md"]),
    ("notekpr-mhchem", "Chemistry",
     "Chemical equations and physical units (`\\ce{…}`, `\\pu{…}`) are a port of mhchemParser, "
     "under the Apache License 2.0.",
     ["crates/mhchem/LICENSE.txt"]),
    ("notekpr-web", "Fonts",
     "The fonts drawings and math are written in, under the SIL Open Font License 1.1 and MIT.",
     ["crates/web/static/fonts/LICENSES.md", "crates/web/static/fonts/OFL.txt",
      "crates/web/static/fonts/ComicShanns/LICENSE.txt"]),
    ("notekpr-web", "Emoji",
     "The emoji picker's names and keywords are Unicode's data (Unicode's emoji and CLDR).",
     [("crates/web/src/emoji/table.rs", "UNICODE LICENSE V3")]),
]

# Of a choice of licences, the one notekpr takes: the first of these deny.toml allows.
# Shortest notices first.
PREFERRED = ["MIT", "MIT-0", "ISC", "BSD-2-Clause", "BSD-3-Clause", "Zlib", "CC0-1.0",
             "BSL-1.0", "Unicode-3.0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception",
             "CDLA-Permissive-2.0", "MPL-2.0", "AGPL-3.0-or-later"]
# Licences whose files must be available as source: where notekpr's copy of it is.
SOURCE_AVAILABLE = {"MPL-2.0"}

# A crate's licence and notice files, by name: its own at its top (or in a LICENSES folder),
# and those of what it has inside from elsewhere anywhere below (a THIRD_PARTY_LICENSES
# folder whole).
NOTICE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense|patents|third[-_]party)", re.I)
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|unlicense)", re.I)
OWN_FOLDER = re.compile(r"^licen[cs]es$", re.I)
NOTICE_FOLDER = re.compile(r"^third[-_]party[-_]licen[cs]es$", re.I)
NOT_NOTICES = {".rs", ".py", ".toml", ".json", ".yml", ".yaml", ".sh", ".c", ".h", ".js", ".ts"}

# Notices a crate needs but doesn't ship: what it has inside from elsewhere, and the file in
# scripts/licenses/ with its notice (from where it comes).
EXTRA = {
    # Microsoft's WebView2 loader, linked into the Windows desktop app (its static library,
    # for the MSVC target). The SDK's LICENSE.txt, as its NuGet package (Microsoft.Web.WebView2)
    # has it.
    "webview2-com-sys": ("WebView2LoaderStatic.lib", "WebView2-SDK.txt"),
}
# The licence a file's name says it holds (LICENSE-APACHE, LICENSE.MIT, UNLICENSE, …).
FAMILIES = ["APACHE", "MIT", "0BSD", "BSD", "ZLIB", "ISC", "UNLICENSE", "CC0", "BSL", "BOOST",
            "MPL", "UNICODE"]


def family(name):
    name = name.upper()
    for f in FAMILIES:
        if f in name:
            return {"0BSD": "BSD", "BOOST": "BSL"}.get(f, f)
    return None


@dataclass
class Crate:
    name: str
    version: str
    licence: str
    licence_file: str | None
    source: str | None
    dir: Path
    authors: list
    programs: set = field(default_factory=set)
    std: bool = False  # in Rust's standard library

    def key(self):
        return (self.name.lower(), version_key(self.version))

    def label(self):
        return f"{self.name} {self.version}"

    def link(self):
        """Where its source is: crates.io, or notekpr's repository for a copy it changed."""
        if self.std or (self.source and self.source.startswith("registry+")):
            return f"https://crates.io/crates/{self.name}/{self.version}"
        return f"{REPO}/tree/main/{self.dir.relative_to(ROOT).as_posix()}"


def version_key(v):
    return [(0, int(p), "") if p.isdigit() else (1, 0, p) for p in re.split(r"[.+-]", v)]


def cargo_metadata(manifest, target):
    command = ["cargo", "metadata", "--format-version", "1", "--locked",
               "--filter-platform", target, "--manifest-path", str(ROOT / manifest)]
    run = subprocess.run(command, capture_output=True, text=True, cwd=ROOT)
    if run.returncode:
        sys.exit(f"{run.stderr}cargo metadata failed ({manifest}, {target})")
    return json.loads(run.stdout)


def collect():
    """Every crate in each program (without notekpr's own) and, for OWN, which programs have
    each of notekpr's own crates."""
    crates, own = {}, {}
    runs = {}
    for name, manifest, root, targets in PROGRAMS:
        for target in targets:
            runs.setdefault((manifest, target), []).append((name, root))
    for (manifest, target), programs in runs.items():
        meta = cargo_metadata(manifest, target)
        packages = {p["id"]: p for p in meta["packages"]}
        deps = {n["id"]: n["deps"] for n in meta["resolve"]["nodes"]}
        members = set(meta["workspace_members"])
        for name, root in programs:
            start = [i for i in members if packages[i]["name"] == root]
            if len(start) != 1:
                sys.exit(f"{root} isn't in the workspace of {manifest}")
            seen, todo = set(), start
            while todo:
                i = todo.pop()
                if i in seen:
                    continue
                seen.add(i)
                todo += [d["pkg"] for d in deps[i]
                         if any(k["kind"] in (None, "build") for k in d["dep_kinds"])]
            for i in seen:
                p = packages[i]
                if i in members:
                    own.setdefault(p["name"], set()).add(name)
                    continue
                d = Path(p["manifest_path"]).parent
                k = (p["name"], p["version"], p["source"] or str(d))
                if k not in crates:
                    authors = [re.sub(r"\s*<[^>]*>", "", a).strip() for a in p["authors"]]
                    crates[k] = Crate(p["name"], p["version"], p["license"] or "",
                                      p["license_file"], p["source"], d, [a for a in authors if a])
                crates[k].programs.add(name)
    for c in std_crates():
        k = (c.name, c.version, REGISTRY)
        if k in crates:
            crates[k].std = True
        else:
            crates[k] = c
    return sorted(crates.values(), key=Crate.key), own


def std_rust():
    """The Rust whose standard library scripts/licenses/std has, after checking the releases
    are built with it."""
    index = STD / "crates.json"
    if not index.is_file():
        sys.exit("scripts/licenses/std/crates.json is missing: run `task licenses:std RUST=…`")
    rust = json.loads(index.read_text(encoding="utf-8"))["rust"]
    for dockerfile in RUST_IN:
        for used in re.findall(r"^FROM\s.*\brust:(\d+\.\d+)", (ROOT / dockerfile).read_text(encoding="utf-8"), re.M):
            if not rust.startswith(used + "."):
                sys.exit(f"scripts/licenses/std has Rust {rust}'s standard library, but {dockerfile} "
                         f"builds with Rust {used}: run `task licenses:std RUST={used}.<its latest patch>`")
    return rust


def std_crates():
    """The crates of Rust's standard library, from scripts/licenses/std."""
    index = json.loads((STD / "crates.json").read_text(encoding="utf-8"))
    return [Crate(c["name"], c["version"], c["license"], None, REGISTRY,
                  STD / "crates" / f"{c['name']}-{c['version']}", c["authors"], std=True)
            for c in index["crates"]]


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read()


def copy_std(rust):
    """Copy the notices of Rust `rust`'s standard library into scripts/licenses/std: its
    crates' licence and notice files, from crates.io (checked against library/Cargo.lock),
    and STD_FILES, from the rust repository."""
    tree = f"https://raw.githubusercontent.com/rust-lang/rust/{rust}"
    lock = tomllib.loads(fetch(f"{tree}/library/Cargo.lock").decode())
    pinned = {p["name"]: p for p in lock["package"] if p.get("source") == REGISTRY}
    missing = [n for n in STD_CRATES if n not in pinned]
    if missing:
        sys.exit(f"Rust {rust}'s library/Cargo.lock has no {', '.join(missing)}: update STD_CRATES")
    if STD.exists():
        shutil.rmtree(STD)
    index = []
    for name in STD_CRATES:
        version, checksum = pinned[name]["version"], pinned[name]["checksum"]
        data = fetch(f"https://static.crates.io/crates/{name}/{name}-{version}.crate")
        if hashlib.sha256(data).hexdigest() != checksum:
            sys.exit(f"{name} {version}: the download isn't what library/Cargo.lock pins")
        top = PurePosixPath(f"{name}-{version}")
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
            manifest = None
            for member in tar.getmembers():
                path = PurePosixPath(member.name)
                if not member.isfile() or path.parts[:1] != top.parts or ".." in path.parts:
                    continue
                inner = path.relative_to(top)
                if inner == PurePosixPath("Cargo.toml"):
                    manifest = tomllib.loads(tar.extractfile(member).read().decode())["package"]
                notice = NOTICE_FILE.match(inner.name) and inner.suffix.lower() not in NOT_NOTICES
                in_folder = len(inner.parts) > 1 and (OWN_FOLDER.match(inner.parts[0])
                                                       or NOTICE_FOLDER.match(inner.parts[0]))
                if notice or in_folder:
                    out = STD / "crates" / f"{name}-{version}" / inner
                    out.parent.mkdir(parents=True, exist_ok=True)
                    out.write_bytes(tar.extractfile(member).read())
        authors = [re.sub(r"\s*<[^>]*>", "", a).strip() for a in manifest.get("authors", [])]
        index.append({"name": name, "version": version, "license": manifest.get("license", ""),
                      "authors": [a for a in authors if a]})
    for path in STD_FILES:
        out = STD / "rust" / path
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(fetch(f"{tree}/{path}"))
    (STD / "crates.json").write_text(json.dumps({"rust": rust, "crates": index}, indent=2) + "\n",
                                     encoding="utf-8")
    print(f"wrote scripts/licenses/std (Rust {rust}): now run task licenses")


def allowed():
    """The licences deny.toml allows, for every crate and for some."""
    deny = tomllib.loads((ROOT / "deny.toml").read_text(encoding="utf-8"))["licenses"]
    exceptions = {e["name"]: set(e["allow"]) for e in deny.get("exceptions", [])}
    return set(deny["allow"]), exceptions


def choose(expression, ok):
    """The licences notekpr uses a crate under: all an AND names; of an OR, the alternative
    whose licences deny.toml allows (`ok`) and come first in PREFERRED."""
    tokens = re.findall(r"[()]|[^\s()]+", expression.replace("/", " OR "))
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        pos += 1
        return tokens[pos - 1]

    def rank(ids):
        return max((PREFERRED.index(i) if i in PREFERRED else len(PREFERRED)) if i in ok else 1000
                   for i in ids)

    def any_of():
        options = [all_of()]
        while peek() == "OR":
            take()
            options.append(all_of())
        return min(options, key=rank)

    def all_of():
        ids = one()
        while peek() == "AND":
            take()
            ids |= one()
        return ids

    def one():
        t = take()
        if t == "(":
            ids = any_of()
            take()
            return ids
        if peek() == "WITH":
            take()
            t = f"{t} WITH {take()}"
        return {t}

    return any_of() if tokens else set()


def clean(text):
    """A file's text as it is shown: Unix line ends, no control characters, no trailing
    spaces, no blank lines around it."""
    text = text.lstrip("﻿").replace("\r\n", "\n").replace("\r", "\n")
    text = re.sub(r"[\x00-\x08\x0b-\x1f\x7f]", "", text)
    return "\n".join(line.rstrip() for line in text.split("\n")).strip("\n")


def read(path):
    return clean(path.read_bytes().decode("utf-8", errors="replace"))


def is_notice(f):
    return f.is_file() and NOTICE_FILE.match(f.name) and f.suffix.lower() not in NOT_NOTICES


def notice_files(crate, chosen):
    """The licence and notice files a crate ships: its own, but those of licences it isn't
    used under (unless that would leave none), and those of what it has inside from
    elsewhere (vendored code, data), all of them."""
    own, inside = [], []
    for f in sorted(crate.dir.iterdir()):
        if f.is_dir() and OWN_FOLDER.match(f.name):
            own += sorted(g for g in f.rglob("*") if g.is_file())
        elif f.is_dir() and NOTICE_FOLDER.match(f.name):
            inside += sorted(g for g in f.rglob("*") if g.is_file())
        elif f.is_dir():
            inside += sorted(g for g in f.rglob("*") if is_notice(g))
        elif is_notice(f):
            own.append(f)
    if crate.licence_file:
        f = crate.dir / crate.licence_file
        if f.is_file() and f.resolve() not in {g.resolve() for g in own + inside}:
            own.append(f)
    taken = {family(c) for c in chosen}

    def other(f):
        named = {family(t) for t in re.split(r"[^A-Za-z0-9]+", f.name)} - {None}
        return LICENCE_FILE.match(f.name) and named and not (named & taken)

    kept = [f for f in own if not other(f)]
    if not any(LICENCE_FILE.match(f.name) for f in kept):
        kept = own
    return kept, inside


def own_text(spec):
    if isinstance(spec, str):
        return spec, read(ROOT / spec)
    path, line = spec
    lines = (ROOT / path).read_text(encoding="utf-8").split("\n")
    start = next((i for i, l in enumerate(lines) if line in l), None)
    if start is None:
        sys.exit(f"OWN: {path} has no line with {line!r}")
    end = next(i for i in range(start, len(lines)) if not lines[i].startswith("//"))
    return path, clean("\n".join(re.sub(r"^// ?", "", l) for l in lines[start:end]))


def programs_of(names):
    names = [n for n in NAMES if n in names]
    return names[0] if len(names) == 1 else ", ".join(names[:-1]) + " and " + names[-1]


def escape(text):
    """Text as Markdown shows it literally."""
    return re.sub(r"([\\`*_\[\]<>|#])", r"\\\1", text)


def build():
    """The document: a list of blocks, ("h1"…"h3" | "p", markdown), ("table", header, rows),
    ("pre", text), and ("version", markdown): what only the web app's page says."""
    ok, exceptions = allowed()
    crates, own = collect()
    doc = [
        ("h1", "Licences"),
        ("version", "This is notekpr @VERSION@. Its source, at this version: [@SOURCE@](@SOURCE@)."),
        ("p", "notekpr is free software: you may use, share and change it under the GNU Affero "
              "General Public License, version 3 or later (AGPL-3.0-or-later), whose text is the "
              f"[LICENSE]({REPO}/blob/main/LICENSE) file that comes with it. Its source is at "
              f"[github.com/coolapso/notekpr]({REPO})."),
        ("p", "It is also made of other people's work, under licences of their own that ask for "
              "their notices to go with every copy. Here they are, for all of notekpr's programs "
              "together: the server, the web app, the CLI and the desktop app (the server's "
              "image and the desktop app have the web app inside). Every one of them comes "
              "with this list: the web app as its *Licences* page (Settings → Account), the "
              "server's image as `/usr/share/notekpr/THIRD_PARTY_LICENSES.md`, the packages and "
              "installers as a `THIRD_PARTY_LICENSES` file next to notekpr's own licence."),
        ("p", "`scripts/licenses.py` writes it from the crates' own files: change that, not this."),
    ]

    rust = std_rust()
    doc.append(("h2", "Rust's standard library"))
    doc.append(("p", "Every program has Rust's standard library in it (`std`, `core`, `alloc`), "
                     "under MIT OR Apache-2.0, here under MIT:"))
    doc.append(("pre", "Copyright (c) The Rust Project Contributors\n\n" + read(STANDARD / "MIT.txt")))
    doc.append(("p", f"It is also built from crates of its own, listed with the others below (Rust "
                     f"{rust}'s, the Rust notekpr's releases are built with), and its "
                     "`compiler_builtins` has parts of LLVM's compiler-rt and of musl's libm in it:"))
    for path in STD_FILES:
        doc.append(("p", f"`{path}`:"))
        doc.append(("pre", read(STD / "rust" / path)))

    doc.append(("h2", "In notekpr's own code"))
    for crate, heading, about, specs in OWN:
        if crate not in own:
            sys.exit(f"OWN names {crate}, which no program has")
        doc.append(("h3", heading))
        doc.append(("p", f"{about} In the {programs_of(own[crate])}."))
        for spec in specs:
            path, text = own_text(spec)
            doc.append(("p", f"`{path}`:"))
            doc.append(("pre", text))

    texts, standard, available, problems = {}, {}, [], []
    for c in crates:
        if not c.licence and not c.licence_file:
            problems.append(f"{c.label()} names no licence")
            continue
        chosen = choose(c.licence, ok | exceptions.get(c.name, set())) if c.licence else set()
        own_files, inside = notice_files(c, chosen)
        if not any(LICENCE_FILE.match(f.name) for f in own_files):
            for licence in sorted(chosen):
                if not (STANDARD / f"{licence}.txt").is_file():
                    problems.append(f"{c.label()} ships no licence text, and there is no "
                                    f"scripts/licenses/{licence}.txt for its {licence}")
                standard.setdefault(licence, []).append(c)
        files = [(f, f.relative_to(c.dir).as_posix() if f.is_relative_to(c.dir) else f.name)
                 for f in own_files + inside]
        if c.name in EXTRA:
            what, notice = EXTRA[c.name]
            files.append((STANDARD / notice, f"{what}: not in the crate, {notice} from scripts/licenses"))
        for f, label in files:
            text = read(f)
            entry = texts.setdefault(" ".join(text.split()), (text, []))
            entry[1].append((c, label))
        if chosen & SOURCE_AVAILABLE:
            available.append((c, chosen & SOURCE_AVAILABLE))
    problems += [f"EXTRA names {name}, which no program has" for name in EXTRA
                 if not any(c.name == name for c in crates)]
    if problems:
        sys.exit("\n".join(problems))

    doc.append(("h2", "Crates"))
    doc.append(("p", f"The {len(crates)} Rust crates notekpr's programs are built from, Rust's "
                     "standard library's among them, and which programs have each. Their licence "
                     "texts follow, each with the crates it is for."))

    def where(c):
        if not c.std:
            return programs_of(c.programs)
        if not c.programs:
            return "Rust's standard library"
        return f"{programs_of(c.programs)}, and Rust's standard library"

    rows = [[f"[{escape(c.name)}]({c.link()})", escape(c.version), escape(c.licence or "see its files"),
             where(c)] for c in crates]
    doc.append(("table", ["Crate", "Version", "Licence", "In"], rows))
    changed = [c for c in crates if not c.std and not (c.source or "").startswith("registry+")]
    if changed:
        doc.append(("p", "Copies notekpr changed, whose source is in notekpr's: " + ", ".join(
            f"{escape(c.label())} ([{c.dir.relative_to(ROOT).as_posix()}]({c.link()}))" for c in changed) + "."))
    if available:
        doc.append(("h2", "Source of the MPL-2.0 crates"))
        doc.append(("p", "The Mozilla Public License 2.0 asks that its files' source be available. "
                         "notekpr has these crates as they are published on crates.io, unchanged; "
                         "their source is there: " + ", ".join(
                             f"[{escape(c.label())}]({c.link()})" for c, _ in available) + "."))

    doc.append(("h2", "Licence texts"))
    doc.append(("p", "Each crate's licence and notice files, as it ships them. Of a choice of "
                     "licences (MIT OR Apache-2.0, say), notekpr takes MIT where it can, and "
                     "only that one's text is here."))
    for text, users in sorted(texts.values(), key=lambda t: (t[1][0][0].key(), t[1][0][1])):
        doc.append(("p", ", ".join(f"**{escape(c.label())}** ({escape(f)})" for c, f in users) + ":"))
        doc.append(("pre", text))
    for licence, users in sorted(standard.items()):
        doc.append(("p", f"These crates are under {licence} but ship no text of it: this is the "
                         "standard one, their authors (as their Cargo.toml names them) holding "
                         "the copyright: " + ", ".join(
                             f"**{escape(c.label())}** ({escape(', '.join(c.authors) or 'its authors')})"
                             for c in users) + "."))
        doc.append(("pre", read(STANDARD / f"{licence}.txt")))
    return doc


# ------------------------------------------------------------------ Markdown

def fence(text):
    longest = max((len(m) for m in re.findall(r"`+", text)), default=0)
    return "`" * max(3, longest + 1)


def markdown(doc):
    out = []
    for block in doc:
        kind = block[0]
        if kind in ("h1", "h2", "h3"):
            out.append("#" * int(kind[1]) + " " + block[1])
        elif kind == "p":
            out.append(block[1])
        elif kind == "table":
            _, header, rows = block
            lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
            lines += ["| " + " | ".join(row) + " |" for row in rows]
            out.append("\n".join(lines))
        elif kind == "pre":
            # No language: the docs site would wrap each line in markup to
            # highlight it, almost doubling the page.
            f = fence(block[1])
            out.append(f"{f}\n{block[1]}\n{f}")
    return "\n\n".join(out) + "\n"


# ------------------------------------------------------------------ the web app's page

def inline(md):
    """HTML for the Markdown this script writes in a line: `code`, [links](…), **strong**,
    *emphasis* and \\-escapes."""
    out, pos = [], 0
    pattern = re.compile(r"`([^`]+)`|\[((?:\\.|[^\]\\])+)\]\(([^)\s]+)\)|\*\*((?:\\.|[^*\\])+)\*\*|\*((?:\\.|[^*\\])+)\*|\\(.)")
    for m in pattern.finditer(md):
        out.append(html.escape(md[pos:m.start()]))
        code, text, url, strong, em, escaped = m.groups()
        if code is not None:
            out.append(f"<code>{html.escape(code)}</code>")
        elif text is not None:
            out.append(f'<a href="{html.escape(url)}">{inline(text)}</a>')
        elif strong is not None:
            out.append(f"<strong>{inline(strong)}</strong>")
        elif em is not None:
            out.append(f"<em>{inline(em)}</em>")
        else:
            out.append(html.escape(escaped))
        pos = m.end()
    out.append(html.escape(md[pos:]))
    return "".join(out)


def slug(md):
    return re.sub(r"[^a-z0-9]+", "-", re.sub(r"\\(.)", r"\1", md).lower()).strip("-")


STYLE = """\
:root { color-scheme: light dark; --bg: #ffffff; --fg: #2a2a28; --muted: #6e6e68; --border: #e6e6e1;
  --link: #3e63dd; --code-bg: #f3f2ef;
  --sans: ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
  --mono: ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Code", Menlo, Consolas, monospace; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #1c1c1f; --fg: #e6e6e3; --muted: #8b8b92; --border: #2f2f35; --link: #8fa8ff; --code-bg: #26262b; }
}
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.55 var(--sans); }
main { max-width: 52rem; margin: 0 auto; padding: 1.25rem 1rem 4rem; }
a { color: var(--link); }
h1 { font-size: 1.8rem; margin: 1rem 0; }
h2 { font-size: 1.3rem; margin: 2.5rem 0 .75rem; padding-bottom: .3rem; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.05rem; margin: 1.75rem 0 .5rem; }
p { overflow-wrap: anywhere; }
code, pre { font-family: var(--mono); font-size: 12.5px; background: var(--code-bg); border-radius: 6px; }
code { padding: .1em .3em; }
pre { padding: .75rem 1rem; white-space: pre-wrap; overflow-wrap: anywhere; }
.table { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 13.5px; }
th, td { text-align: left; vertical-align: top; padding: .3rem .6rem .3rem 0; border-bottom: 1px solid var(--border); }
.back { margin: 0; font-size: 14px; }
"""


def page(doc):
    out = ['<!doctype html>', '<html lang="en">', "<head>", '<meta charset="utf-8">',
           '<meta name="viewport" content="width=device-width, initial-scale=1">',
           "<title>Licences · notekpr</title>",
           '<link rel="icon" href="/favicon.svg" type="image/svg+xml">',
           f"<style>\n{STYLE}</style>", "</head>", "<body>", "<main>",
           '<p class="back"><a href="/">← notekpr</a></p>']
    for block in doc:
        kind = block[0]
        if kind in ("h1", "h2", "h3"):
            out.append(f'<{kind} id="{slug(block[1])}">{inline(block[1])}</{kind}>')
        elif kind in ("p", "version"):
            out.append(f"<p>{inline(block[1])}</p>")
        elif kind == "table":
            _, header, rows = block
            out.append('<div class="table"><table>')
            out.append("<thead><tr>" + "".join(f"<th>{inline(h)}</th>" for h in header) + "</tr></thead>")
            out.append("<tbody>")
            out += ["<tr>" + "".join(f"<td>{inline(c)}</td>" for c in row) + "</tr>" for row in rows]
            out.append("</tbody></table></div>")
        elif kind == "pre":
            out.append(f"<pre>{html.escape(block[1])}</pre>")
    out += ["</main>", "</body>", "</html>"]
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true",
                        help="fail if the files aren't what this would write")
    parser.add_argument("--std", metavar="RUST",
                        help="copy the notices of that Rust's standard library (1.99.1) first")
    args = parser.parse_args()
    if args.std:
        copy_std(args.std)
    doc = build()
    files = {MARKDOWN: markdown(doc), PAGE: page(doc)}
    stale = [p for p, text in files.items() if not p.is_file() or p.read_text(encoding="utf-8") != text]
    if args.check:
        if stale:
            names = " and ".join(p.relative_to(ROOT).as_posix() for p in stale)
            sys.exit(f"{names}: out of date with the crates notekpr is built from. "
                     "Run `task licenses` and commit what it changes.")
        return
    for p in stale:
        p.write_text(files[p], encoding="utf-8")
        print(f"wrote {p.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
