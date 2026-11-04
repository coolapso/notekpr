#!/usr/bin/env python3
"""Write the web app's emoji table (crates/web/src/emoji/table.rs) from
Unicode's data: every fully-qualified emoji of emoji-test.txt in its group
and order — without the skin-tone variants — up to the newest emoji version
most systems can show (--newest), named as there, with CLDR's English
keywords to find them by.

    python3 scripts/emoji.py              # the versions below, from unicode.org and CLDR
    python3 scripts/emoji.py --unicode 19.0.0 --cldr-release 50 --newest 16.0
    python3 scripts/emoji.py --test emoji-test.txt --cldr en.xml --cldr-derived en-derived.xml

To update: when Unicode releases a new version (each September; CLDR's
release follows), raise UNICODE and CLDR below and run this; raise NEWEST
when most systems can show those emoji. Python standard library only;
formats the result with rustfmt if there is one.
"""

import argparse
import re
import shutil
import subprocess
import sys
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/web/src/emoji/table.rs"

# The data the table is made from.
UNICODE = "18.0.0"
CLDR = "49"
TEST_URL = "https://www.unicode.org/Public/{unicode}/emoji/emoji-test.txt"
CLDR_URL = "https://raw.githubusercontent.com/unicode-org/cldr/release-{cldr}/common/annotations/en.xml"
CLDR_DERIVED_URL = "https://raw.githubusercontent.com/unicode-org/cldr/release-{cldr}/common/annotationsDerived/en.xml"

# The newest emoji version offered: newer ones show as empty boxes on many
# systems for a year or two after they come out. Any emoji can still be
# pasted or typed.
NEWEST = "15.1"

# Unicode's groups the picker shows (not "Component": skin tones and hair).
GROUPS = [
    "Smileys & Emotion",
    "People & Body",
    "Animals & Nature",
    "Food & Drink",
    "Travel & Places",
    "Activities",
    "Objects",
    "Symbols",
    "Flags",
]

SKIN_TONES = {chr(c) for c in range(0x1F3FB, 0x1F400)}

# The data's license (https://www.unicode.org/license.txt): its notice goes
# with the table.
LICENSE = """\
UNICODE LICENSE V3

COPYRIGHT AND PERMISSION NOTICE

Copyright © 1991-2026 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS.

IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder."""

LINE = re.compile(r"^([0-9A-F ]+?)\s*;\s*fully-qualified\s*#\s*\S+\s+E(\d+\.\d+)\s+(.+)$")


def fetch(url_or_path):
    if url_or_path.startswith("https://"):
        req = urllib.request.Request(url_or_path, headers={"User-Agent": "notekpr scripts/emoji.py"})
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.read().decode("utf-8")
    return Path(url_or_path).read_text(encoding="utf-8")


def version(v):
    major, minor = v.split(".")
    return (int(major), int(minor))


def emojis(test, newest):
    """(group, emoji, name) in Unicode's order, and the data's version."""
    data_version = re.search(r"^# Version: (\S+)", test, re.M).group(1)
    group, out = None, []
    for line in test.splitlines():
        if line.startswith("# group: "):
            group = line[len("# group: "):].strip()
            continue
        m = LINE.match(line)
        if not m or group not in GROUPS:
            continue
        cps, ver, name = m.groups()
        emoji = "".join(chr(int(c, 16)) for c in cps.split())
        if SKIN_TONES & set(emoji) or version(ver) > version(newest):
            continue
        out.append((group, emoji, name.strip()))
    return out, data_version


def keywords(*xmls):
    """Emoji (without U+FE0F) → CLDR's keywords for it."""
    out = {}
    for text in xmls:
        root = ET.fromstring(text)
        for a in root.iter("annotation"):
            if a.get("type") == "tts" or not a.text:
                continue
            words = [w.strip() for w in a.text.split("|") if w.strip()]
            out.setdefault(a.get("cp").replace("️", ""), words)
    return out


def words(text):
    return re.findall(r"[\w'’+#*-]+", text.lower())


def line(emoji, name, kw):
    """`emoji|name|words`: the keywords' words that aren't in the name."""
    in_name = set(words(name))
    extra = []
    for k in kw:
        for w in words(k):
            if w not in in_name and w not in extra:
                extra.append(w)
    fields = (emoji, name, " ".join(extra))
    for f in fields:
        if "|" in f or "\n" in f:
            sys.exit(f"unexpected character in {fields!r}")
    return "|".join(fields)


def rust_str(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--unicode", default=UNICODE, help=f"Unicode's version (default {UNICODE})")
    ap.add_argument("--cldr-release", default=CLDR, help=f"CLDR's release (default {CLDR})")
    ap.add_argument("--test", help="emoji-test.txt, a file instead of Unicode's")
    ap.add_argument("--cldr", help="CLDR's annotations/en.xml, a file instead of CLDR's")
    ap.add_argument("--cldr-derived", help="CLDR's annotationsDerived/en.xml, a file instead")
    ap.add_argument("--newest", default=NEWEST, help=f"the newest emoji version to offer (default {NEWEST})")
    ap.add_argument("--out", default=str(OUT))
    args = ap.parse_args()

    test = fetch(args.test or TEST_URL.format(unicode=args.unicode))
    cldr = fetch(args.cldr or CLDR_URL.format(cldr=args.cldr_release))
    derived = fetch(args.cldr_derived or CLDR_DERIVED_URL.format(cldr=args.cldr_release))
    found, data_version = emojis(test, args.newest)
    kw = keywords(cldr, derived)
    by_group = {g: [] for g in GROUPS}
    for group, emoji, name in found:
        by_group[group].append(line(emoji, name, kw.get(emoji.replace("️", ""), [])))

    out = [
        "// Generated by scripts/emoji.py: run it again to update, don't edit.",
        f"// From Emoji {data_version}'s emoji-test.txt: the emoji up to Emoji {args.newest}, without",
        f"// skin tones, with keywords from CLDR {args.cldr_release} (English). The data is Unicode's:",
        "//",
        *[f"// {l}".rstrip() for l in LICENSE.splitlines()],
        "",
        "//! Every emoji the picker offers, by group, in Unicode's order.",
        "",
        "/// The groups: their names and their emoji, a line each:",
        "/// `emoji|name|words` (CLDR's keywords not in the name).",
        f"pub const GROUPS: [(&str, &str); {len(GROUPS)}] = [",
    ]
    for g in GROUPS:
        body = "\n".join(by_group[g])
        out.append(f'    (\n        "{rust_str(g)}",\n        "{rust_str(body)}",\n    ),')
    out.append("];")
    Path(args.out).write_text("\n".join(out) + "\n", encoding="utf-8")
    if shutil.which("rustfmt"):
        subprocess.run(["rustfmt", "--edition", "2024", args.out], check=True)
    total = sum(len(v) for v in by_group.values())
    print(f"{total} emoji in {len(GROUPS)} groups → {args.out}")


if __name__ == "__main__":
    main()
