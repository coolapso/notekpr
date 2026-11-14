#!/usr/bin/env python3
"""Write the desktop app's Flatpak manifest, and what it builds from, into OUT.

    packaging/flatpak/generate.py OUT --local        # this checkout as it is (task build:flatpak)
    packaging/flatpak/generate.py OUT --tag v0.17.0  # a release, for Flathub (task flathub)

OUT gets md.notekpr.Notekpr.yml (the template beside this script, filled in),
cargo-sources.json and shared-modules/ (Flathub's, at the commit below). The
build has no network, so cargo-sources.json lists every crate it needs: those
the source's Cargo.locks pin (the workspace's and crates/diagram's) and those
of the wasm-bindgen CLI the web client's crates pin.

The web client is wasm32, which the Rust SDK extension has no standard library
for: the manifest brings Rust's official one, of the extension's version — the
one installed here, which must be Flathub's current one (`task flatpak:deps`
updates it). This checks the extension's rustc is that very build.

Only the standard library: it runs where Python 3.11 and flatpak do.
"""
import argparse
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
APP_ID = "md.notekpr.Notekpr"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
# Flathub's shared modules (the tray's library): the commit used, until moved on.
SHARED_MODULES = "https://github.com/flathub/shared-modules.git"
SHARED_MODULES_COMMIT = "c91a81101cf9bcf9fc8132e8c3ebed741ad8bae6"
REPO_URL = "https://github.com/coolapso/notekpr.git"


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], check=True, capture_output=True,
                          text=True).stdout.strip()


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read()


def crates(lock_text):
    """(name, version, sha256) of every crates.io package a Cargo.lock pins."""
    out = set()
    for p in tomllib.loads(lock_text).get("package", []):
        if p.get("source") == CRATES_IO:
            out.add((p["name"], p["version"], p["checksum"]))
        elif p.get("source", "").startswith("git+"):
            sys.exit(f"{p['name']} comes from git: generate.py vendors crates.io's crates only")
    return out


def wasm_bindgen():
    """The wasm-bindgen CLI the web client's crates pin: its version, its crate's sha256 and the
    crates its own Cargo.lock pins."""
    cargo = (ROOT / "crates" / "web" / "Cargo.toml").read_text()
    version = re.search(r'^wasm-bindgen = "=([0-9.]+)"', cargo, re.M).group(1)
    index = fetch("https://index.crates.io/wa/sm/wasm-bindgen-cli").decode()
    sha = next(e["cksum"] for e in map(json.loads, index.splitlines()) if e["vers"] == version)
    crate = fetch(f"https://static.crates.io/crates/wasm-bindgen-cli/wasm-bindgen-cli-{version}.crate")
    if hashlib.sha256(crate).hexdigest() != sha:
        sys.exit("the wasm-bindgen-cli crate doesn't match crates.io's checksum")
    with tarfile.open(fileobj=io.BytesIO(crate)) as t:
        lock = t.extractfile(f"wasm-bindgen-cli-{version}/Cargo.lock").read().decode()
    return version, sha, crates(lock)


def rust_std(runtime_version):
    """The official wasm32 standard library of the Rust SDK extension's rustc: (url, sha256)."""
    out = subprocess.run(
        ["flatpak", "run", "--command=sh", f"org.gnome.Sdk//{runtime_version}", "-c",
         "/usr/lib/sdk/rust-stable/bin/rustc -vV"],
        check=True, capture_output=True, text=True).stdout
    release = re.search(r"^release: (\S+)", out, re.M).group(1)
    commit = re.search(r"^commit-hash: (\S+)", out, re.M).group(1)
    channel = tomllib.loads(fetch(f"https://static.rust-lang.org/dist/channel-rust-{release}.toml").decode())
    if channel["pkg"]["rustc"]["git_commit_hash"] != commit:
        sys.exit(f"the Rust SDK extension's rustc {release} ({commit[:9]}) isn't Rust's own build "
                 "of it: its standard library for wasm32 wouldn't fit")
    std = channel["pkg"]["rust-std"]["target"]["wasm32-unknown-unknown"]
    print(f"Rust {release}: the SDK extension's, with its wasm32 standard library")
    return std["xz_url"], std["xz_hash"]


def cargo_sources(packages):
    """flatpak-builder sources: each crate unpacked into cargo/vendor, as `cargo vendor` would,
    and the cargo configuration that builds from there."""
    out = []
    for name, version, sha in sorted(packages):
        dest = f"cargo/vendor/{name}-{version}"
        out.append({"type": "archive", "archive-type": "tar-gzip",
                    "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                    "sha256": sha, "dest": dest})
        out.append({"type": "inline", "contents": json.dumps({"package": sha, "files": {}}),
                    "dest": dest, "dest-filename": ".cargo-checksum.json"})
    out.append({"type": "inline", "dest": "cargo", "dest-filename": "config.toml",
                "contents": '[source.vendored-sources]\ndirectory = "cargo/vendor"\n\n'
                            '[source.crates-io]\nreplace-with = "vendored-sources"\n'})
    return out


def shared_modules(out):
    d = out / "shared-modules"
    if not (d / ".git").exists():
        subprocess.run(["git", "clone", "-q", SHARED_MODULES, str(d)], check=True)
    if subprocess.run(["git", "-C", str(d), "rev-parse", "HEAD"], capture_output=True,
                      text=True).stdout.strip() != SHARED_MODULES_COMMIT:
        subprocess.run(["git", "-C", str(d), "fetch", "-q", "origin"], check=True)
        subprocess.run(["git", "-C", str(d), "checkout", "-q", SHARED_MODULES_COMMIT], check=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out", type=Path)
    which = ap.add_mutually_exclusive_group(required=True)
    which.add_argument("--local", action="store_true", help="this checkout's last commit")
    which.add_argument("--tag", help="a release's tag (v0.17.0), for Flathub")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    if args.local:
        # The checkout as it is, packed beside the manifest (a sandboxed build, as Flathub's,
        # takes nothing from outside its folder): what git has or would add, not what it ignores.
        tar = args.out / "notekpr-src.tar"
        files = git("ls-files", "--cached", "--others", "--exclude-standard").splitlines()
        with tarfile.open(tar, "w") as t:
            for f in files:
                if os.path.lexists(ROOT / f):
                    t.add(ROOT / f, arcname=f"notekpr/{f}", recursive=False)
        sha = hashlib.sha256(tar.read_bytes()).hexdigest()
        source = json.dumps({"type": "archive", "path": tar.name, "sha256": sha})
        version = git("describe", "--tags", "--match", "v[0-9]*", "--dirty").removeprefix("v")
        date = git("show", "-s", "--format=%cs", "HEAD")

        def lock(path):
            return (ROOT / path).read_text()
    else:
        commit = git("rev-parse", f"{args.tag}^{{commit}}")
        version = args.tag.removeprefix("v")
        source = json.dumps({"type": "git", "url": REPO_URL, "tag": args.tag, "commit": commit})
        date = git("show", "-s", "--format=%cs", commit)

        def lock(path):
            return git("show", f"{commit}:{path}")

    template = (HERE / f"{APP_ID}.yml").read_text()
    runtime = re.search(r"^runtime-version: '(\d+)'", template, re.M).group(1)
    std_url, std_sha = rust_std(runtime)
    wb_version, wb_sha, wb_crates = wasm_bindgen()
    packages = crates(lock("Cargo.lock"))
    packages |= crates(lock("crates/diagram/Cargo.lock"))
    packages |= wb_crates
    # One version of a crate once, whichever lock pins it (their checksums agree).
    seen = {}
    for name, ver, sha in packages:
        if seen.setdefault((name, ver), sha) != sha:
            sys.exit(f"{name} {ver}: two checksums")

    manifest = template
    for key, value in {"SOURCE": source, "VERSION": version, "DATE": date,
                       "RUST_STD_URL": std_url, "RUST_STD_SHA256": std_sha,
                       "WASM_BINDGEN_VERSION": wb_version, "WASM_BINDGEN_SHA256": wb_sha}.items():
        manifest = manifest.replace("{{" + key + "}}", value)
    if re.search(r"\{\{[A-Z_]+\}\}", manifest):
        sys.exit("generate.py: the template names something it doesn't fill in")
    (args.out / f"{APP_ID}.yml").write_text(manifest)
    (args.out / "cargo-sources.json").write_text(json.dumps(cargo_sources(packages), indent=2) + "\n")
    shared_modules(args.out)
    print(f"{args.out}: {APP_ID}.yml ({version}), cargo-sources.json ({len(packages)} crates)")


if __name__ == "__main__":
    main()
