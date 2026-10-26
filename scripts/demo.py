#!/usr/bin/env python3
"""A demo environment: a development-mode server with users, vaults, shared
vaults, shared notes, linked notes for the graph, attachments, icons, quick
notes, history and the trash — all written through the real CLI, so
everything is end-to-end encrypted like any other data.

    task demo            # builds everything, then runs this
    task demo:reset      # start over

The first run creates the data (data/demo/ by default); later runs just start
the server on it. A new browser is a new device of its user: choose "Approve
from another device" when signing in, and while this runs it approves the
demo users' new devices for you (--no-approve turns that off; the recovery
keys are in data/demo/recovery-keys.txt).

Keep it complete: every user-facing feature should have something here to
see it with (AGENTS.md, "The demo shows every feature"). What the CLI can't
create goes into "Things to try" in the *Start here* note.

Python standard library only. Development mode lets anyone sign in as
anyone: never expose this server — except as a public demo that starts over
on its own (the `demo` stage of the Dockerfile, deploy/kubernetes/demo.yaml,
docs/operations.md §10):

    demo.py --fresh --lifetime 1h --dev-mode-anywhere --public-url https://demo.example.com ...
"""

import argparse
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import textwrap
import time
import urllib.request
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
USERS = ["alice", "bob", "carol", "dave", "eve"]
AGENT_PROFILE = "assistant"


def note(text):
    """Dedented, and with each paragraph on one line, as editors write them
    (wrapped here only to keep this file readable)."""
    out, code = [], False
    for line in textwrap.dedent(text).lstrip("\n").splitlines():
        if line.startswith("```"):
            code = not code
        elif not code and out and continues(out[-1], line):
            out[-1] += " " + line.strip().removeprefix("> ")
            continue
        out.append(line)
    return "\n".join(out) + "\n"


def continues(prev, line):
    """Whether `line` goes on with the paragraph, list item or quote `prev`."""
    if not prev.strip() or not line.strip():
        return False
    if line.startswith("  "):  # an indented continuation
        return True
    if re.match(r"(> ?)+\[!", prev):  # a callout's first line: its title
        return False
    if prev.startswith("> ") and line.startswith("> "):
        return True
    starts_block = re.compile(r"([-*+] |\d+\. |\||#|>|```|---|\[\^|<)")
    return not starts_block.match(prev) and not starts_block.match(line)


# ---------------------------------------------------------------- pictures


def png(width, height, pixel):
    """A PNG (RGB) from pixel(x, y) -> (r, g, b)."""
    rows = b"".join(
        b"\x00" + bytes(c for x in range(width) for c in pixel(x, y))
        for y in range(height)
    )

    def chunk(kind, data):
        crc = zlib.crc32(kind + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", crc)

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def garden_layout():
    """Four raised beds with plants, a path and a compost corner."""
    beds = [(30, 30, 190, 120), (250, 30, 450, 120), (30, 170, 190, 270), (250, 170, 450, 270)]
    plants = [(1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1)]  # tomato red, basil green, …

    def pixel(x, y):
        if 455 < x and 230 < y:
            return (110, 80, 50)  # compost
        for i, (x0, y0, x1, y1) in enumerate(beds):
            if x0 <= x < x1 and y0 <= y < y1:
                cx, cy = (x - x0) % 40 - 20, (y - y0) % 45 - 22
                if cx * cx + cy * cy < 120:
                    r, g, b = plants[i]
                    return (200 if r else 60, 160 if g else 90, 120 if b else 50)
                return (122, 82, 48)
        if 205 <= x < 235 or 135 <= y < 155:
            return (216, 201, 163)  # path
        return (143, 191, 106)  # grass

    return png(480, 300, pixel)


def sunset():
    """Sky, sun and sea."""

    def pixel(x, y):
        if (x - 240) ** 2 + (y - 175) ** 2 < 55**2 and y < 180:
            return (255, 214, 102)
        if y < 180:
            t = y / 180
            return (int(255 - 60 * t), int(120 + 40 * t), int(90 + 60 * t))
        shimmer = 20 if (x // 12 + y // 6) % 5 == 0 and abs(x - 240) < 80 else 0
        return (30 + shimmer, 80 + shimmer, 140 + shimmer)

    return png(480, 270, pixel)


# ---------------------------------------------------------------- content

# (path, icon, text). Alice's personal vault: densely linked, for the graph.
HOME = [
    ("Start here.md", "👋", note("""
        # Start here

        Welcome to the **notekpr demo**. A script wrote all of this through
        the command-line client — encrypted on the client like any other note.
        The server only ever stored ciphertext.

        ## Things to try

        - [ ] Open the graph with `Ctrl+G` (or *Graph* in the sidebar): drag notes, zoom, click one to open it
        - [ ] Jump anywhere with the quick switcher: `Ctrl+P`
        - [ ] Search everything with `Ctrl+Shift+F`: try `tag:recipe` or `"tomato sauce"`
        - [ ] Switch to the reading view with `Ctrl+E`; `Ctrl+/` lists every shortcut
        - [ ] Capture a quick note with `Alt+Q`
        - [ ] Open [[Garden plan]] and its history (the clock button at the top) to compare versions
        - [ ] Sign in as **bob** in another tab and edit *Shopping list* in the *Family* vault together
        - [ ] Click a link to a note that doesn't exist yet: [[Greenhouse]]
        - [ ] Look in the trash: a shopping list is waiting to be restored

        ## More to try

        - [ ] Star a note (☆ at the top, it turns gold): it shows under *Favorites* in the sidebar
        - [ ] Rename a note, or drag it to another folder in the sidebar: links to it follow
        - [ ] Comment on a note: select some text and click *Comment* (or press `Alt+M`), in the editor or the reading view; click highlighted text to read and answer its thread. *Holiday plans* in *Family* has comments — even Carol's, who only views that vault
        - [ ] Dates: type `@today`, `@tomorrow` or `@yesterday` in a note, or `@` and pick a day: the note gets the date, shown as Today, Tomorrow, Yesterday, and as the date once that has passed. *Chores* in *Family* has some
        - [ ] Mentions: the **@** next to *Search* shows the comments that mention you — Bob and Carol mentioned you; open one to jump to its thread. Type `@` in a comment to mention someone in the vault. Settings → Editor → *Mentions* turns on notifications from the browser
        - [ ] Drag a note between two others in the sidebar: a line shows where it goes, and it stays there for everyone (this note and the *Recipes* overview were put first that way, from the command line); *Sort A–Z* in a folder's ⋯ menu undoes it
        - [ ] HTML in notes: the fold-out section at the end of [[Sourdough bread]] (`<details>`), the centered photo with a caption in [[Lisbon 2026]], a table with merged cells in [[Weekly meal plan]], keys like <kbd>Ctrl</kbd>+<kbd>E</kbd>; click one in the editor to see its HTML. Scripts, styles, frames and forms are removed
        - [ ] Callouts, written as GitHub and Obsidian write them (`> [!tip]`, `> [!warning] A title`): a tip and a warning in [[Sourdough bread]], one that folds in [[Japan ideas]] (`[!faq]-`), GitHub's `[!CAUTION]` in *Deploy script* in *Work*, one inside another in *Emergency contacts* in *Family*. The toolbar's callout button starts one, or gives the one the caret is in another type
        - [ ] Colors: select some text and pick a text or background color with the toolbar's **A** button. They are saved as HTML that Obsidian, Typora and VS Code show too ([[Tomatoes]]), and colors written there show here ([[Packing list]] has a highlight from Obsidian's Highlightr and a `<font color>`)
        - [ ] Code blocks in *Snippets*: a file name and highlighted lines (`title=… hl=…` after the language), line numbers, indentation guides, a copy button, `wrap=false` for long lines; Settings → Editor turns the numbers, guides and wrapping off
        - [ ] Drop or paste a picture into a note
        - [ ] Give a note or folder an icon: *Set icon…* in its ⋯ menu in the sidebar
        - [ ] *Quick notes* in the sidebar: yours alone, in no vault — the same whichever vault you're in. Move one to a vault (*Move to vault…*: it takes its first line as the name) or delete it for good; there's room for 20
        - [ ] Move a note, or a folder with everything in it, to another vault: *Move to vault…* in its menu in the file tree (a note's also in its ⋯ menu at the top); the originals go to the trash
        - [ ] *Vault settings* (⚙ next to a vault): its icon or a picture, *Export as Markdown (.zip)*, *Import notes*, the *Activity log*
        - [ ] *Share vault* at the bottom of the sidebar: members, roles (*Admin*, *Can edit*, *Can view*) and key fingerprints
        - [ ] You're an admin of Bob's *Book club*: its *Trash* has a draft Carol threw away — as an editor she can restore it but not delete it for good; you can (*Empty trash*), and you see the vault's *Activity log* in *Vault settings*. Sign in as **carol** to see her side
        - [ ] Share a single note with **dave** (the share button at the top of a note)
        - [ ] Settings → Editor: the theme, Vim keybindings, and how wide notes are (a slider from narrow to wide). A note's ⋯ menu gives it a width of its own, only for you: try *Wide* on *Server checks* in *Work*, whose log lines don't wrap
        - [ ] Settings → Devices: the CLI and this browser; *Keep this browser usable offline*
        - [ ] Sign the command line in with a code: `task demo:cli -- --profile mine login --server http://localhost:8099` prints a link; opening it here shows when, and from which address, the code was asked for
        - [ ] Settings → Administration: you run this server in the demo. *Instance* sets limits (accounts, vaults, agents, and per person; *Limits* next to someone gives them their own) and read-only mode: switch it on with **bob** signed in in another tab, edit there, and his edits wait in his browser until you switch it off
        - [ ] Settings → AI agents: the *Home assistant* agent, which writes in *Assistant*
        - [ ] Your notes as plain Markdown files, synced both ways: `task demo:cli -- --profile alice sync --dir /tmp/alice-notes`

        ## Map of this vault

        - 🌱 [[Garden plan]]: [[Tomatoes]], [[Basil]], [[Composting]]
        - 🍳 [[Recipes]], from the garden to the table
        - ✈️ [[Lisbon 2026]] and [[Japan ideas]]
        - 📚 [[Reading list]]
        - 📓 Journal: [[2026-10-12]], [[2026-10-15]], [[2026-10-19]]
        - 💡 [[Ideas]]

        ## Other vaults

        | Vault | Owner | Who else |
        |---|---|---|
        | Work | Alice | Dave (editor) |
        | Family | Alice | Bob (editor), Carol (viewer) |
        | Book club | Bob | Alice (admin), Carol (editor) |
        | Garden club | Eve | Alice (editor) |
        | Assistant | Alice | the *Home assistant* agent |
        | Old projects | Alice | archived |

        Notes shared with you on their own (Bob's training plan, Carol's logo
        ideas) are under **Shared with me**.
        """)),
    ("Garden/Tomatoes.md", "🍅", note("""
        # Tomatoes

        Planted in bed 1 of the [[Garden plan]]. #garden #vegetables

        | Variety | Type | Days to harvest | Notes |
        |---|:---:|---:|---|
        | San Marzano | paste | 80 | the one for [[Tomato sauce]] |
        | Sungold | cherry | 60 | eaten before they reach the kitchen |
        | Brandywine | beefsteak | 90 | <span style="color: #e44045">needs tall stakes</span> |

        - Water deeply, not often: see [[Watering schedule]]
        - Basil next to them keeps whiteflies away ([[Basil]], [[Pests]])
        - <span style="background-color: #ffd0004d">Save seeds from the Sungolds</span> for the [[Seed swap 2026]]
        """)),
    ("Garden/Basil.md", "🌿", note("""
        # Basil

        Genovese for [[Pesto]], Thai basil for curries. Grows between the
        [[Tomatoes]] — good companions. #garden #herbs

        ## Care

        1. Pinch off the flower buds, or it stops growing leaves
        2. Water in the morning ([[Watering schedule]])
        3. Watch for slugs after rain ([[Pests]])
        """)),
    ("Garden/Composting.md", None, note("""
        # Composting

        Two bins: one filling, one resting. Brown (leaves, cardboard) and green
        (kitchen scraps, [[Sourdough starter]] discard) in roughly equal parts.
        #garden

        > "The land is the real teacher." — from [[Braiding Sweetgrass]]

        The finished compost goes on the beds in spring (see [[Garden plan]]).
        """)),
    ("Garden/Watering schedule.md", None, note("""
        # Watering schedule

        | Bed | Plants | Mon | Wed | Fri | Sun |
        |---|---|:---:|:---:|:---:|:---:|
        | 1 | [[Tomatoes]] | ✓ | | ✓ | |
        | 2 | [[Basil]], herbs | ✓ | ✓ | ✓ | ✓ |
        | 3 | beans | | ✓ | | ✓ |
        | 4 | squash | ✓ | | | ✓ |

        While we're in Lisbon the neighbours water (see [[Lisbon 2026]]).
        One day this will be automatic: [[Home automation]].
        """)),
    ("Garden/Pests.md", None, note("""
        # Pests

        - [x] Slugs on the [[Basil]]: beer traps worked
        - [x] Aphids on the beans: soapy water
        - [ ] Whiteflies on the [[Tomatoes]]: try more marigolds
        - [ ] Find out what eats the squash leaves at night

        #garden #todo
        """)),
    ("Garden/Seed swap 2026.md", None, note("""
        # Seed swap 2026

        Saturday 14 March, community hall. Carol is bringing her chilli seeds.

        To bring:
        - [ ] Sungold seeds ([[Tomatoes]])
        - [ ] Thai basil seeds ([[Basil]])
        - [ ] Labels and envelopes

        #garden #event
        """)),
    ("Recipes/Recipes.md", "🍳", note("""
        # Recipes

        From the garden to the table. #recipe

        - [[Tomato sauce]] and [[Pesto]] — summer in a jar
        - [[Sourdough bread]] (with its [[Sourdough starter]])
        - [[Apple pie]] — Carol has the original card
        - What we cook this week: [[Weekly meal plan]]
        """)),
    ("Recipes/Tomato sauce.md", None, note("""
        # Tomato sauce

        *Makes 4 jars.* #recipe #vegetarian

        ## Ingredients

        - 2 kg San Marzano [[Tomatoes]]
        - 4 cloves of garlic, olive oil, salt
        - a handful of [[Basil]]

        ## Method

        1. Score, blanch and peel the tomatoes
        2. Soften the garlic in the oil, add the tomatoes
        3. Simmer for **an hour**, stirring now and then
        4. Basil at the very end; jar while hot

        Goes with everything on the [[Weekly meal plan]].
        """)),
    ("Recipes/Pesto.md", None, note("""
        # Pesto

        #recipe #vegetarian

        Blend 60 g [[Basil]] leaves, 30 g pine nuts, 1 garlic clove, 50 g
        parmesan and olive oil until it looks right. Freeze in an ice cube tray.

        Pairs well with fresh [[Sourdough bread]].
        """)),
    ("Recipes/Sourdough bread.md", None, note("""
        # Sourdough bread

        #recipe #baking

        ## Ingredients

        | | Amount | Baker's % |
        |---|---:|---:|
        | Flour | 500 g | 100 % |
        | Water | 350 g | 70 % |
        | Active [[Sourdough starter]] | 100 g | 20 % |
        | Salt | 10 g | 2 % |

        ## Schedule

        | When | What |
        |---|---|
        | 09:00 | feed the starter |
        | 13:00 | mix, rest 30 min, add salt |
        | 13:30–16:00 | four sets of stretch and folds |
        | 16:00 | shape, into the fridge overnight |
        | next day 08:00 | bake at 250 °C, 20 min lid on, 20 min off |

        > [!tip] Wet hands, not more flour
        > The dough is sticky at 70 % water: wet your hands for the stretch and
        > folds instead of flouring the dough.

        > [!warning]
        > The pot is at **250 °C**: thick oven gloves for the lid.

        <details>
        <summary>Flat loaf? What went wrong</summary>

        - The starter wasn't active enough: feed it twice the day before
        - Over-proofed: shorten the bulk rise when the kitchen is warm
        - Shape tighter, and score <i>deeper</i> than feels right

        </details>
        """)),
    ("Recipes/Sourdough starter.md", None, note("""
        # Sourdough starter

        Her name is *Clint Yeastwood*. Fed daily with equal parts flour and
        water; the discard goes on the compost ([[Composting]]).

        Used for [[Sourdough bread]]. #baking
        """)),
    ("Recipes/Apple pie.md", "🥧", note("""
        # Apple pie

        Grandma's recipe, shared with Carol. #recipe #baking #dessert

        - 6 apples (tart ones), sliced
        - 150 g sugar, cinnamon, a pinch of nutmeg
        - double pie crust

        Bake 50 minutes at 190 °C until the juice bubbles through the vents.
        """)),
    ("Recipes/Weekly meal plan.md", None, note("""
        # Weekly meal plan

        | Day | Dinner |
        |---|---|
        | Monday | pasta with [[Tomato sauce]] |
        | Tuesday | gnocchi with [[Pesto]] |
        | Wednesday | soup and bread (see [[Sourdough bread#Schedule]]) |
        | Thursday | leftovers |
        | Friday | pizza night |
        | Sunday | roast, then [[Apple pie]] |

        ## Who cooks

        <table>
        <tr><th rowspan="2">Day</th><th colspan="2" align="center">Meals</th></tr>
        <tr><th>Lunch</th><th>Dinner</th></tr>
        <tr><td>Monday</td><td rowspan="5" align="center"><i>at work and school</i></td><td>Alice</td></tr>
        <tr><td>Tuesday</td><td>Bob</td></tr>
        <tr><td>Wednesday</td><td>Alice</td></tr>
        <tr><td>Thursday</td><td><i>leftovers</i></td></tr>
        <tr><td>Friday</td><td>Bob — pizza night 🍕</td></tr>
        <tr><td>Saturday</td><td colspan="2" align="center">out with Carol</td></tr>
        <tr><td>Sunday</td><td>Bob</td><td>Alice and Bob</td></tr>
        </table>

        - [ ] Buy pine nuts
        - [x] Make a double batch of sauce

        #recipe #plan
        """)),
    ("Travel/Lisbon 2026.md", "🌅", note("""
        # Lisbon 2026

        Five days in May. #travel

        <div align="center">
        <figure>
        <img src="lisbon-sunset.png" width="360" alt="Sunset over the river">
        <figcaption>The view from the <b>Miradouro</b> we want to find again</figcaption>
        </figure>
        </div>

        ## Plan

        - [x] Book the flights
        - [x] Book the flat in Alfama
        - [ ] Tram 28 early in the morning, before the crowds
        - [ ] Day trip to Sintra
        - [ ] Pastéis de nata in Belém (obviously)

        ## Budget

        | Item | Cost |
        |---|---:|
        | Flights | 240 € |
        | Flat (5 nights) | 450 € |
        | Food and trams | 300 € |
        | **Total** | **990 €** |

        Before leaving: [[Packing list]], ask the neighbours about the
        [[Watering schedule]]. Totals live in [[Travel budget]].
        """)),
    ("Travel/Packing list.md", None, note("""
        # Packing list

        - [ ] <mark style="background: #FFB86CA6;">Passports</mark>
        - [ ] Chargers and adapter
        - [ ] Comfortable shoes (Lisbon is all hills)
        - [ ] A book from the [[Reading list]]
        - [x] <font color="#2f9e5a">Sunscreen</font>

        Used for [[Lisbon 2026]]; reuse for [[Japan ideas]]. #travel
        """)),
    ("Travel/Japan ideas.md", None, note("""
        # Japan ideas

        Maybe autumn 2027. #travel #someday

        - [[Kyoto]] for the temples and the maple leaves
        - [[Tokyo]] for everything else
        - A rail pass — check the [[Travel budget]]
        - Read something Japanese first (see [[Reading list]])

        > [!faq]- Why autumn?
        > The maples in [[Kyoto]] turn red in November, and it's cooler than
        > the summer.
        """)),
    ("Travel/Kyoto.md", None, note("""
        # Kyoto

        - Fushimi Inari at sunrise
        - Philosopher's Path
        - Arashiyama bamboo grove

        Part of [[Japan ideas]]; two hours from [[Tokyo]] by train. #travel
        """)),
    ("Travel/Tokyo.md", None, note("""
        # Tokyo

        - Yanaka for old Tokyo
        - Tsukiji outer market for breakfast
        - A day at the Ghibli museum (book months ahead!)

        Then on to [[Kyoto]]. Back to [[Japan ideas]]. #travel
        """)),
    ("Travel/Travel budget.md", None, note("""
        # Travel budget

        | Trip | Planned | Spent |
        |---|---:|---:|
        | [[Lisbon 2026]] | 1 000 € | 990 € |
        | [[Japan ideas]] | 4 500 € | — |

        #travel #money
        """)),
    ("Reading/Reading list.md", "📚", note("""
        # Reading list

        - [x] [[The Overstory]] — Richard Powers
        - [x] [[Braiding Sweetgrass]] — Robin Wall Kimmerer
        - [ ] [[Project Hail Mary]] — Andy Weir
        - [ ] Something for the trip ([[Japan ideas]])

        Favourite lines go to [[Quotes]]. For the book club see the
        *Book club* vault. #reading
        """)),
    ("Reading/The Overstory.md", None, note("""
        # The Overstory

        ★★★★★ — nine people, and the trees that connect them. Made me look at the
        [[Garden plan]] differently. Quotes in [[Quotes]].

        Back to the [[Reading list]]. #reading #fiction
        """)),
    ("Reading/Braiding Sweetgrass.md", None, note("""
        # Braiding Sweetgrass

        ★★★★★ — botany and gratitude. The chapter on the Three Sisters is why
        bed 3 has beans, corn and squash ([[Garden plan]]), and the reason I
        started [[Composting]] properly.

        See [[Quotes]]. #reading #nonfiction
        """)),
    ("Reading/Project Hail Mary.md", None, note("""
        # Project Hail Mary

        Not started yet. Everyone says *don't read anything about it first*.

        On the [[Reading list]]. #reading #fiction
        """)),
    ("Reading/Quotes.md", None, note("""
        # Quotes

        > The best arguments in the world won't change a person's mind. The only
        > thing that can do that is a good story.
        >
        > — [[The Overstory]]

        > All flourishing is mutual.
        >
        > — [[Braiding Sweetgrass]]

        #reading #quotes
        """)),
    ("Journal/2026-10-12.md", None, note("""
        # Sunday 12 October

        Last [[Tomatoes]] of the season picked green — they'll ripen on the
        windowsill. Fed the [[Sourdough starter]] and baked twice. Started
        planning next year's [[Garden plan]]. #journal
        """)),
    ("Journal/2026-10-15.md", None, note("""
        # Wednesday 15 October

        Booked [[Lisbon 2026]]! Started the [[Packing list]] far too early.
        Something is still eating the squash ([[Pests]]). #journal
        """)),
    ("Journal/2026-10-19.md", None, note("""
        # Sunday 19 October

        Carol came by; we baked the [[Apple pie]] together and she wants to swap
        seeds in March ([[Seed swap 2026]]). Spent the evening on
        [[Learning Rust]] — the borrow checker and I are slowly becoming
        friends. #journal
        """)),
    ("Ideas/Ideas.md", "💡", note("""
        # Ideas

        - [[Home automation]]: water the garden by itself
        - [[Homelab]]: one small server for everything
        - [[Rust]]: finally learn it properly
        - A [[Greenhouse]] (no note yet — click the link to start one)

        #ideas
        """)),
    ("Ideas/Home automation.md", None, note("""
        # Home automation

        A soil moisture sensor per bed, a valve, and a rule instead of the
        [[Watering schedule]]. Runs on the [[Homelab]].

        ```yaml
        automation:
          - alias: Water bed 1
            trigger:
              platform: numeric_state
              entity_id: sensor.bed_1_moisture
              below: 30
            action:
              service: switch.turn_on
              target: { entity_id: switch.valve_bed_1 }
        ```

        #ideas #homelab
        """)),
    ("Ideas/Homelab.md", None, note("""
        # Homelab

        An old mini PC in the cupboard, running the [[Home automation]] and
        backups.

        ```sh
        # nightly backup of the notes server
        notekpr-server backup --data-dir /srv/notes --out-dir /mnt/backup --keep 14
        ```

        #ideas #homelab
        """)),
    ("Ideas/Rust.md", "🦀", note("""
        # Learning Rust

        Ownership in one sentence: every value has one owner, and it is dropped
        when the owner goes away.[^book]

        ```rust
        fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
            if a.len() >= b.len() { a } else { b }
        }
        ```

        - [x] The book, chapters 1–10
        - [ ] Write a small CLI tool
        - [ ] Rustlings

        Back to [[Ideas]]. #learning #rust

        [^book]: *The Rust Programming Language*, chapter 4.
        """)),
    ("Wi-Fi guest network.md", None, note("""
        # Wi-Fi guest network

        Name: `garden-guests`. The password is on the fridge, not in here.
        """)),
    ("Random thoughts.md", None, note("""
        ---
        tags: [thoughts]
        ---

        # Random thoughts

        A note that links to nothing, so it floats alone in the graph (while
        *Unlinked notes* is ticked).
        """)),
]

GARDEN_PLAN_VERSIONS = [
    "# Garden plan\n\n![[garden-layout.png]]\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n3. Beans\n4. Squash\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Squash\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n## Care\n\n- [[Watering schedule]]\n- [[Pests]]\n- [[Composting]] in the corner\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]]\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n## Care\n\n- [[Watering schedule]]\n- [[Pests]]\n- [[Composting]] in the corner\n\n## Next year\n\n- [ ] Swap seeds ([[Seed swap 2026]])\n- [ ] Build a [[Greenhouse]]?\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]] — three varieties\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n## Care\n\n- [[Watering schedule]]\n- [[Pests]]\n- [[Composting]] in the corner\n\n## Next year\n\n- [ ] Swap seeds ([[Seed swap 2026]])\n- [ ] Build a [[Greenhouse]]?\n- [ ] Read [[The Overstory]] again in the garden\n\n#garden #plan\n",
    "# Garden plan\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]] — three varieties\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n## Care\n\n- [[Watering schedule]]\n- [[Pests]]\n- [[Composting]] in the corner\n\n## Next year\n\n- [x] Swap seeds ([[Seed swap 2026]]) — date set\n- [ ] Build a [[Greenhouse]]?\n- [ ] Read [[The Overstory]] again in the garden\n\n#garden #plan\n",
    "# Garden plan\n\nFour raised beds, a path and the compost corner.\n\n![[garden-layout.png]]\n\n## Beds\n\n1. [[Tomatoes]] — three varieties\n2. [[Basil]] and other herbs\n3. Beans, corn and squash — the Three Sisters ([[Braiding Sweetgrass]])\n4. Strawberries\n\n## Care\n\n- [[Watering schedule]]\n- [[Pests]]\n- [[Composting]] in the corner\n\n## Next year\n\n- [x] Swap seeds ([[Seed swap 2026]]) — date set\n- [ ] Build a [[Greenhouse]]?\n- [ ] Read [[The Overstory]] again in the garden\n\n#garden #plan\n",
]

WORK = [
    ("Team handbook.md", "📘", note("""
        # Team handbook

        How we work. Start with the current project, [[Website relaunch]].

        - Meetings: [[2026-10-14 Kickoff]], [[2026-10-21 Review]]
        - Decisions are written down as ADRs: [[ADR-001 Static site]],
          [[ADR-002 Hosting]]
        - People: [[Dave]], [[Carol]]
        - Useful snippets: [[Deploy script]], [[Error handling in Rust]]
        """)),
    ("Projects/Website relaunch.md", "🚀", note("""
        # Website relaunch

        New site by the end of November. #project

        | Milestone | Owner | Due | Status |
        |---|---|---|---|
        | Design | [[Carol]] | 31 Oct | in progress |
        | Static site generator | [[Dave]] | 7 Nov | done |
        | Content migration | Alice | 14 Nov | not started |
        | Launch | everyone | 28 Nov | — |

        - Design: [[Design brief]] (shared with Carol)
        - [[Launch checklist]]
        - Why static: [[ADR-001 Static site]]; where: [[ADR-002 Hosting]]
        - Meetings: [[2026-10-14 Kickoff]], [[2026-10-21 Review]]

        Raw numbers: [milestones.csv](milestones.csv)
        """)),
    ("Projects/Launch checklist.md", None, note("""
        # Launch checklist

        - [x] Domain renewed
        - [ ] Redirects from the old URLs
        - [ ] Lighthouse score above 95
        - [ ] Deploy with the [[Deploy script]]
        - [ ] Tell everyone

        > [!IMPORTANT]
        > Not on a Friday.

        For the [[Website relaunch]]. #project #todo
        """)),
    ("Projects/Design brief.md", "🎨", note("""
        # Design brief

        For Carol — this note is shared with her on its own; she can't see
        anything else in this vault.

        ## Goals

        - Calm, readable, fast
        - Works on phones first
        - Our green, but less of it

        ## Pages

        Home, About, Blog, Contact.

        Context: [[Website relaunch]].
        """)),
    ("Meetings/2026-10-14 Kickoff.md", None, note("""
        # Kickoff — 14 October

        Present: Alice, [[Dave]]

        - Agreed on a static site ([[ADR-001 Static site]])
        - Carol does the design ([[Design brief]])
        - Next meeting: [[2026-10-21 Review]]

        ## Actions

        - [x] Dave: try two static site generators
        - [ ] Alice: list the pages to migrate

        #meeting
        """)),
    ("Decisions/ADR-001 Static site.md", None, note("""
        # ADR-001: A static site

        **Status:** accepted

        **Context.** The old CMS needs updates every week and nobody edits the
        site more than once a month.

        **Decision.** Generate a static site from Markdown.

        **Consequences.** Cheap hosting ([[ADR-002 Hosting]]), no database, but
        a build step before every change.
        """)),
    ("Decisions/ADR-002 Hosting.md", None, note("""
        # ADR-002: Hosting

        **Status:** proposed

        Follows from [[ADR-001 Static site]]: any static file host works. We
        deploy with the [[Deploy script]] to a small VPS behind a reverse proxy.
        """)),
    ("People/Dave.md", None, note("""
        # Dave

        Developer. Builds the static site for the [[Website relaunch]]. Editor
        in this vault.
        """)),
    ("People/Carol.md", None, note("""
        # Carol

        Freelance designer. Works on the [[Design brief]] — the only note of
        this vault she can see.
        """)),
    ("Snippets/Deploy script.md", None, note("""
        # Deploy script

        ```bash title=deploy.sh hl=6
        #!/usr/bin/env bash
        set -euo pipefail
        hugo --minify

        for host in web1 web2; do
            if ping -c 1 "$host" > /dev/null; then
                rsync -az --delete public/ "deploy@$host:/srv/site/"
            else
                echo "skipped $host (down)"
            fi
        done
        echo "deployed $(git rev-parse --short HEAD)"
        ```

        > [!CAUTION]
        > `rsync --delete` removes anything on the servers that isn't in
        > `public/`.

        Used by the [[Launch checklist]]. #snippet
        """)),
    ("Snippets/Error handling in Rust.md", None, note("""
        # Error handling in Rust

        ```rust title=src/config.rs
        use anyhow::{Context, Result};

        fn read_config(path: &str) -> Result<String> {
            std::fs::read_to_string(path).with_context(|| format!("reading {path}"))
        }
        ```

        `?` passes errors up; `context` says what we were doing. #snippet #rust
        """)),
    ("Snippets/Server checks.md", None, note("""
        # Server checks

        Is the mail server up? From a Windows laptop:

        ```powershell title=check-mail.ps1 hl=4-5
        <# Warns when the server
           doesn't answer. #>
        $server = "mail.example.com"
        $ok = Test-NetConnection -ComputerName $server -Port 993
        if (-not $ok.TcpTestSucceeded) { Write-Warning "$server is down" }
        ```

        The web server, pinned to a version:

        ```yaml title=compose.yaml ln=false vl=false
        services:
          web:
            image: nginx:1.27  # pinned
            ports: ["8080:80"]
        ```

        What the mail server logged (long lines, not wrapped):

        ```text title=mail.log wrap=false
        2026-10-22 06:14:02 imapd[812]: Login: user=alice, method=PLAIN, rip=192.168.1.20, lip=192.168.1.5, mpid=4411, TLS, session=<Kq3bJz5A>
        2026-10-22 06:14:09 imapd[812]: Disconnected: Logged out in=412 out=10233 deleted=0 expunged=0 trashed=0 hdr_count=12 body_count=1
        ```

        What changed for TLS:

        ```diff
        - listen 80;
        + listen 443 ssl;
        ```

        Each block's header has a copy button. #snippet
        """)),
]

MILESTONES_CSV = """milestone,owner,due,status
Design,Carol,2026-10-31,in progress
Static site generator,Dave,2026-11-07,done
Content migration,Alice,2026-11-14,not started
Launch,everyone,2026-11-28,
"""

FAMILY = [
    ("Family.md", "🏠", note("""
        # Family

        Shared by Alice with Bob (editor) and Carol (viewer).

        - [[Shopping list]] — whoever goes shopping ticks things off
        - [[Holiday plans]]
        - [[Birthdays]] and [[Chores]]
        - The house: [[Maintenance log]], [[Wi-Fi and devices]]
        - [[Emergency contacts]]
        """)),
    ("Birthdays.md", "🎂", note("""
        # Birthdays

        | Who | When | Gift ideas |
        |---|---|---|
        | Alice | 3 March | seeds, books |
        | Bob | 21 June | running socks |
        | Carol | 9 September | good pencils |
        | Grandma | 30 November | photo album |

        Back to [[Family]].
        """)),
    ("Chores.md", None, note("""
        # Chores

        | | Alice | Bob |
        |---|:---:|:---:|
        | Dishes | Mon–Wed | Thu–Sun |
        | Laundry | ✓ | |
        | Bins | | ✓ |
        | Garden | ✓ | ✓ |

        - [ ] Clean the gutters (see [[Maintenance log]]) @tomorrow
        - [x] Fix the bike light @yesterday
        - [ ] Bins out tonight, @today
        """)),
    ("House/Maintenance log.md", "🔧", note("""
        # Maintenance log

        | Date | What | Who |
        |---|---|---|
        | 2026-09-02 | boiler service | Bob |
        | 2026-10-05 | replaced the kitchen tap washer | Alice |
        """)),
    ("House/Wi-Fi and devices.md", None, note("""
        # Wi-Fi and devices

        - Router in the hall cupboard; restart it before calling anyone
        - Printer: works on Tuesdays
        - Smart plugs: living room lamp, fan

        See [[Maintenance log]].
        """)),
    ("Emergency contacts.md", None, note("""
        # Emergency contacts

        | Who | Number |
        |---|---|
        | Plumber | 555-0142 |
        | Doctor | 555-0199 |
        | Neighbours (watering the garden) | 555-0123 |

        > [!info] If the power goes out
        > The fuse box is behind the coats in the hall.
        >
        > > [!danger]
        > > Never touch it with wet hands.

        Back to [[Family]].
        """)),
]

SHOPPING_VERSIONS = [
    ("alice", "# Shopping list\n\n- [ ] Milk\n- [ ] Eggs\n- [ ] Flour for the sourdough\n"),
    ("bob", "# Shopping list\n\n- [ ] Milk\n- [ ] Eggs\n- [ ] Flour for the sourdough\n- [ ] Coffee\n"),
    ("bob", "# Shopping list\n\n- [ ] Milk\n- [ ] Eggs\n- [ ] Flour for the sourdough\n- [ ] Coffee\n- [ ] Bin bags\n"),
    ("alice", "# Shopping list\n\n- [ ] Milk\n- [ ] Eggs\n- [ ] Flour for the sourdough\n- [ ] Coffee\n- [ ] Bin bags\n- [ ] Apples (for the pie)\n"),
    ("bob", "# Shopping list\n\n- [x] Milk\n- [ ] Eggs\n- [ ] Flour for the sourdough\n- [ ] Coffee\n- [ ] Bin bags\n- [ ] Apples (for the pie)\n"),
    ("bob", "# Shopping list\n\n- [x] Milk\n- [x] Eggs\n- [ ] Flour for the sourdough\n- [ ] Coffee\n- [ ] Bin bags\n- [ ] Apples (for the pie)\n"),
    ("bob", "# Shopping list\n\n- [x] Milk\n- [x] Eggs\n- [ ] Flour for the sourdough\n- [x] Coffee\n- [ ] Bin bags\n- [ ] Apples (for the pie)\n"),
    ("bob", "# Shopping list\n\n- [x] Milk\n- [x] Eggs\n- [ ] Flour for the sourdough (they were out)\n- [x] Coffee\n- [x] Bin bags\n- [ ] Apples (for the pie)\n\nBack to [[Family]].\n"),
]

HOLIDAY_VERSIONS = [
    ("alice", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n"),
    ("alice", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n"),
    ("bob", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n"),
    ("alice", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n"),
    ("bob", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n"),
    ("alice", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n\n## Summer\n\n- Lisbon in May\n"),
    ("bob", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n\n## Summer\n\n- Lisbon in May\n- Camping in August?\n"),
    ("bob", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n\n## Summer\n\n- Lisbon in May\n- Camping in August? Bob checks the tent\n\nSee also [[Birthdays]].\n"),
    ("bob", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n- Board games after dinner\n\n## Summer\n\n- Lisbon in May\n- Camping in August? Bob checks the tent\n\nSee also [[Birthdays]].\n"),
    ("alice", "# Holiday plans\n\n## Christmas\n\n- At ours this year\n- Grandma arrives on the 23rd\n- Bob picks her up from the station\n\n## Menu\n\n- Roast, then apple pie\n- Mulled wine (Bob's job)\n- Board games after dinner\n\n## Summer\n\n- Lisbon in May\n- Camping in August? Bob checks the tent\n\nSee also [[Birthdays]] and [[Family]].\n"),
]

BOOK_CLUB = [
    ("Book club.md", "📖", note("""
        # Book club

        Bob's vault; Alice is its admin (she tidies up the trash), Carol an
        editor. We meet on the last Thursday of the month.

        - [[Schedule]]
        - Now reading: [[Piranesi]]
        - Next: [[Klara and the Sun]], then [[The Left Hand of Darkness]]
        - [[Members' picks]] for next year
        """)),
    ("Schedule.md", None, note("""
        # Schedule

        | Month | Book | Host |
        |---|---|---|
        | October | [[Piranesi]] | Bob |
        | November | [[Klara and the Sun]] | Alice |
        | December | [[The Left Hand of Darkness]] | Carol |
        """)),
    ("Books/Piranesi.md", None, note("""
        # Piranesi — Susanna Clarke

        Host: Bob. #book

        ## Questions

        1. Is the House a prison or a paradise?
        2. What does Piranesi lose by remembering?

        ## Notes
        """)),
    ("Books/Klara and the Sun.md", None, note("""
        # Klara and the Sun — Kazuo Ishiguro

        Host: Alice. Back to the [[Schedule]]. #book
        """)),
    ("Books/The Left Hand of Darkness.md", None, note("""
        # The Left Hand of Darkness — Ursula K. Le Guin

        Host: Carol. Back to the [[Schedule]]. #book
        """)),
    ("Members' picks.md", None, note("""
        # Members' picks

        - Bob: *The Martian*
        - Alice: *The Overstory*
        """)),
]

GARDEN_CLUB = [
    ("Garden club.md", "🌻", note("""
        # Garden club

        Eve's vault for the allotment garden club; Alice is an editor. We
        meet on the first Saturday of the month at the shed.

        - [[Seed swap]]: what we have, what we want
        - [[Tool library]]: who has which tool
        - Next meetup: [[2026-11-07]]
        """)),
    ("Seed swap.md", "🌱", note("""
        # Seed swap

        | Seeds | From | For |
        |---|---|---|
        | Runner beans | Eve | anyone |
        | Sunflowers (giant) | Eve | Alice |
        | Rocket | Eve | anyone |

        - [x] Eve: dry the bean pods
        - [ ] Eve: label the envelopes
        """)),
    ("Tool library.md", "🧰", note("""
        # Tool library

        - [ ] Hedge trimmer — Eve, lent to the Parkers
        - [x] Wheelbarrow — back in the shed
        - [ ] Long-handled loppers — Eve
        """)),
    ("Meetups/2026-11-07.md", None, note("""
        # Meetup 7 November

        Host: Eve. Back to the [[Garden club]].

        1. Seed swap ([[Seed swap]])
        2. Leaf mould: who wants a bag?
        3. Spring plan for the shared beds
        """)),
]

RUNNING = [
    ("Training plan.md", "🏃", note("""
        # Training plan — half marathon

        Shared with Alice, who keeps asking how it's going.

        | Week | Long run | Tempo | Easy |
        |---|---:|---:|---:|
        | 1 | 10 km | 5 km | 2 × 6 km |
        | 2 | 12 km | 6 km | 2 × 6 km |
        | 3 | 14 km | 6 km | 2 × 7 km |
        | 4 | 10 km (rest week) | 5 km | 2 × 5 km |

        Race day: 15 March. Log: [[Running log]].
        """)),
    ("Running log.md", None, note("""
        # Running log

        | Date | Distance | Time | Feeling |
        |---|---:|---:|---|
        | 2026-10-14 | 8 km | 44:10 | 🙂 |
        | 2026-10-16 | 5 km | 24:30 | 😅 |
        | 2026-10-18 | 12 km | 1:08:45 | 🥵 |

        Plan: [[Training plan]].
        """)),
]

DESIGN = [
    ("Design.md", "🎨", note("""
        # Design

        Carol's studio notes.

        - [[Color palette]]
        - [[Logo ideas]] (shared with Alice)
        """)),
    ("Color palette.md", None, note("""
        # Color palette

        | Name | Hex |
        |---|---|
        | Moss | `#4f6f52` |
        | Sand | `#e8dfca` |
        | Ink | `#1d2a2f` |
        | Clay | `#c2734a` |

        Used in the [[Logo ideas]].
        """)),
    ("Logo ideas.md", "✏️", note("""
        # Logo ideas

        Shared with Alice for the website relaunch.

        1. A leaf folded like a page
        2. Lower-case wordmark in Moss ([[Color palette]])
        3. A small sprout above the *i*

        Alice: which one do you like?
        """)),
]

OLD_PROJECTS = [
    ("Blog.md", None, note("""
        # Blog

        The old blog. Archived — still readable, out of the way.
        """)),
    ("Thesis notes.md", None, note("""
        # Thesis notes

        Never again. (Archived.)
        """)),
]

QUICK_NOTES = [
    "Call the plumber about the dripping kitchen tap",
    "Book idea from Carol: The Hidden Life of Trees",
    "Try garlic in the bed next to the strawberries?",
]


# ---------------------------------------------------------------- running things


class Cli:
    def __init__(self, exe, env, verbose):
        self.exe, self.env, self.verbose = exe, env, verbose

    def run(self, profile, *args, stdin=b"", check=True):
        """Run the CLI as `profile`; returns (stdout, stderr)."""
        cmd = [str(self.exe), "--profile", profile, *args]
        if self.verbose:
            print("  $", " ".join(cmd[1:]), flush=True)
        p = subprocess.run(cmd, input=stdin, capture_output=True, env=self.env)
        out, err = p.stdout.decode(), p.stderr.decode()
        if check and p.returncode != 0:
            sys.exit(f"\nnotekpr {' '.join(cmd[1:])} failed:\n{err}{out}")
        return out, err

    def write(self, profile, vault, path, text):
        self.run(profile, "-q", "write", vault, path, stdin=text.encode())

    def comment(self, profile, vault, note, on, text):
        """Comment on the text `on` of a note; returns the comment's id."""
        out, _ = self.run(profile, "comments", "add", vault, note, "--on", on, text)
        return re.search(r"\[([0-9a-f]+)\]", out).group(1)

    def attach(self, profile, vault, path, data):
        self.run(profile, "-q", "write", "--attachment", vault, path, stdin=data)

    def icon(self, profile, vault, path, icon):
        self.run(profile, "-q", "icon", vault, path, icon)


# What a demo's data directory holds; --fresh deletes only these.
DEMO_FILES = ("demo.json", "server", "cli", "server.log", "recovery-keys.txt", "assistant.secret")


def duration(text):
    """Seconds in "90", "45s", "30m" or "1h"."""
    m = re.fullmatch(r"(\d+)([smh]?)", text.strip())
    if not m or int(m.group(1)) == 0:
        raise argparse.ArgumentTypeError(f"not a duration: {text!r} (e.g. 30m, 1h)")
    return int(m.group(1)) * {"": 1, "s": 1, "m": 60, "h": 3600}[m.group(2)]


def wipe(data):
    """Delete a demo's data; refuse a directory holding anything else."""
    if not data.exists():
        return
    other = sorted(p.name for p in data.iterdir() if p.name not in DEMO_FILES)
    if other:
        sys.exit(f"{data} holds more than a demo ({', '.join(other)}): not deleting it")
    for name in DEMO_FILES:
        p = data / name
        if p.is_dir():
            shutil.rmtree(p)
        elif p.exists():
            p.unlink()


def wait_for_server(url, server, timeout=60):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if server.poll() is not None:
            sys.exit(f"the server stopped (exit {server.returncode}); see its log")
        try:
            with urllib.request.urlopen(f"{url}/api/v1/info", timeout=2) as r:
                return json.load(r)
        except OSError:
            time.sleep(0.3)
    sys.exit("the server didn't start within a minute; see its log")


def port_in_use(host, port):
    with socket.socket() as s:
        return s.connect_ex((host, port)) == 0


def step(text):
    print(f"• {text}", flush=True)


# ---------------------------------------------------------------- seeding


def seed(cli, url, data):
    keys = {}
    step("signing in alice, bob, carol, dave and eve (each gets account keys)")
    for user in USERS:
        _, err = cli.run(
            user, "login", "--server", url, "--dev-user", user,
            "--device-name", "Demo seed (CLI)",
        )
        lines = err.splitlines()
        for i, line in enumerate(lines):
            if "YOUR RECOVERY KEY" in line:
                keys[user] = next(l.strip() for l in lines[i + 1:] if l.strip())
    keyfile = data / "recovery-keys.txt"
    keyfile.write_text(
        "Recovery keys of the demo users (development data only).\n\n"
        + "".join(f"{u:<8} {k}\n" for u, k in keys.items())
    )
    keyfile.chmod(0o600)

    a, b, c, d, e = USERS
    step("alice: vault Home, ~35 linked notes, pictures, icons")
    cli.run(a, "-q", "vault", "create", "Home")
    cli.run(a, "-q", "vault", "icon", "Home", "🏡")
    for path, icon, text in HOME:
        cli.write(a, "Home", path, text)
        if icon:
            cli.icon(a, "Home", path, icon)
    cli.attach(a, "Home", "Garden/garden-layout.png", garden_layout())
    cli.attach(a, "Home", "Travel/lisbon-sunset.png", sunset())
    for folder, icon in [("Garden", "🌱"), ("Recipes", "🍳"), ("Travel", "✈️"),
                         ("Reading", "📚"), ("Journal", "📓"), ("Ideas", "💡")]:
        cli.icon(a, "Home", folder, icon)
    # A rename that rewrites the links pointing at it.
    cli.run(a, "-q", "mv", "--update-links", "Home", "Ideas/Rust.md", "Ideas/Learning Rust.md")
    # An order of her own: this note on top, the recipes' overview and plan first.
    cli.run(a, "-q", "order", "Home", "Start here", "--before", "Random thoughts")
    cli.run(a, "-q", "order", "Home", "Recipes/Recipes", "--before", "Recipes/Apple pie")
    cli.run(a, "-q", "order", "Home", "Recipes/Weekly meal plan", "--after", "Recipes/Recipes")

    step("alice: the garden plan's history (eleven edits)")
    for text in GARDEN_PLAN_VERSIONS:
        cli.write(a, "Home", "Garden/Garden plan.md", text)
    cli.icon(a, "Home", "Garden/Garden plan.md", "🗺️")

    step("alice: quick notes and something in the trash")
    for text in QUICK_NOTES:
        cli.run(a, "-q", "quick", text)
    cli.write(a, "Home", "Old shopping list.md", "# Old shopping list\n\n- [x] Batteries\n- [x] Light bulbs\n")
    cli.run(a, "-q", "rm", "Home", "Old shopping list.md")

    step("alice: vault Work, shared with dave")
    cli.run(a, "-q", "vault", "create", "Work")
    cli.run(a, "-q", "vault", "icon", "Work", "💼")
    for path, icon, text in WORK:
        cli.write(a, "Work", path, text)
        if icon:
            cli.icon(a, "Work", path, icon)
    cli.attach(a, "Work", "Projects/milestones.csv", MILESTONES_CSV.encode())
    for folder, icon in [("Projects", "📂"), ("Meetings", "🗓️"), ("Decisions", "⚖️"),
                         ("People", "👥"), ("Snippets", "🧩")]:
        cli.icon(a, "Work", folder, icon)
    cli.run(a, "-q", "vault", "share", "Work", d, "--role", "editor")
    step("dave: writes the review meeting and ticks off tasks")
    cli.write(d, "Work", "Meetings/2026-10-21 Review.md", note("""
        # Review — 21 October

        Present: Alice, Dave. (Notes by Dave.)

        - The static site builds in 2 seconds 🎉 ([[ADR-001 Static site]])
        - Hosting still open: [[ADR-002 Hosting]]
        - Carol sent first ideas for the [[Design brief]]

        ## Actions

        - [ ] Dave: redirects ([[Launch checklist]])
        - [ ] Alice: decide on hosting

        #meeting
        """))
    kickoff = dict((p, t) for p, _, t in WORK)["Meetings/2026-10-14 Kickoff.md"]
    cli.write(d, "Work", "Meetings/2026-10-14 Kickoff.md",
              kickoff.replace("- [ ] Alice: list the pages to migrate",
                              "- [ ] Alice: list the pages to migrate\n- [x] Dave: set up the repository"))

    step("alice: vault Family, shared with bob (editor) and carol (viewer)")
    cli.run(a, "-q", "vault", "create", "Family")
    cli.run(a, "-q", "vault", "icon", "Family", "👨‍👩‍👧")
    for path, icon, text in FAMILY:
        cli.write(a, "Family", path, text)
        if icon:
            cli.icon(a, "Family", path, icon)
    cli.icon(a, "Family", "House", "🏡")
    cli.run(a, "-q", "vault", "share", "Family", b, "--role", "editor")
    cli.run(a, "-q", "vault", "share", "Family", c, "--role", "viewer")
    step("alice and bob: editing the shopping list and holiday plans together")
    for who, text in SHOPPING_VERSIONS:
        cli.write(who, "Family", "Shopping list.md", text)
    cli.icon(a, "Family", "Shopping list.md", "🛒")
    for who, text in HOLIDAY_VERSIONS:
        cli.write(who, "Family", "Holiday plans.md", text)
    cli.icon(b, "Family", "Holiday plans.md", "🎄")

    step("carol, bob and alice: comments on the holiday plans (carol only views the vault)")
    tent = cli.comment(c, "Family", "Holiday plans", "Camping in August?",
                       "Count me in — I have a spare tent!")
    cli.run(b, "-q", "comments", "reply", "Family", "Holiday plans", tent,
            "Great, then we have two. @Alice, can you book the campsite?")
    train = cli.comment(b, "Family", "Holiday plans", "Grandma arrives on the 23rd",
                        "Her train gets in at 14:05.")
    cli.run(a, "-q", "comments", "resolve", "Family", "Holiday plans", train)
    cli.comment(a, "Family", "Holiday plans", "Roast, then apple pie",
                "I'll bake the pie — it's the recipe I shared with Carol.")
    log = dict((p, t) for p, _, t in FAMILY)["House/Maintenance log.md"]
    cli.write(b, "Family", "House/Maintenance log.md",
              log + "| 2026-10-18 | cleaned the gutters | Bob |\n")

    step("alice: an archived vault")
    cli.run(a, "-q", "vault", "create", "Old projects")
    cli.run(a, "-q", "vault", "icon", "Old projects", "🗄️")
    for path, _, text in OLD_PROJECTS:
        cli.write(a, "Old projects", path, text)
    cli.run(a, "-q", "vault", "archive", "Old projects")

    step("bob: vault Book club, shared with alice (admin) and carol (editor)")
    cli.run(b, "-q", "vault", "create", "Book club")
    cli.run(b, "-q", "vault", "icon", "Book club", "📖")
    for path, icon, text in BOOK_CLUB:
        cli.write(b, "Book club", path, text)
        if icon:
            cli.icon(b, "Book club", path, icon)
    cli.icon(b, "Book club", "Books", "📚")
    cli.run(b, "-q", "vault", "share", "Book club", a, "--role", "admin")
    cli.run(b, "-q", "vault", "share", "Book club", c, "--role", "editor")
    cli.run(a, "-q", "write", "--append", "Book club", "Books/Piranesi.md",
            stdin="\n- Alice: the statues! Every chapter made me want to draw them.\n".encode())
    cli.run(c, "-q", "write", "--append", "Book club", "Books/Piranesi.md",
            stdin="- Carol: I think it's a paradise *because* he doesn't remember.\n".encode())
    cli.run(c, "-q", "write", "--append", "Book club", "Members' picks.md",
            stdin="- Carol: *Station Eleven*\n".encode())
    # Something in the trash only the owner and Alice (admin) can delete for good.
    cli.write(c, "Book club", "Draft questions.md", "# Draft questions\n\n- Who is the Other?\n")
    cli.run(c, "-q", "rm", "Book club", "Draft questions.md")

    step("eve: vault Garden club, shared with alice (editor)")
    cli.run(e, "-q", "vault", "create", "Garden club")
    cli.run(e, "-q", "vault", "icon", "Garden club", "🌻")
    for path, icon, text in GARDEN_CLUB:
        cli.write(e, "Garden club", path, text)
        if icon:
            cli.icon(e, "Garden club", path, icon)
    cli.icon(e, "Garden club", "Meetups", "📅")
    cli.run(e, "-q", "vault", "share", "Garden club", a, "--role", "editor")
    cli.run(a, "-q", "write", "--append", "Garden club", "Seed swap.md",
            stdin="- [ ] Alice: bring tomato seeds (Sungold) and basil\n".encode())
    cli.run(a, "-q", "write", "--append", "Garden club", "Meetups/2026-11-07.md",
            stdin="4. Alice: compost bins, can we share a shredder?\n".encode())

    step("bob: vault Running; his training plan shared with alice on its own")
    cli.run(b, "-q", "vault", "create", "Running")
    cli.run(b, "-q", "vault", "icon", "Running", "🏃")
    for path, icon, text in RUNNING:
        cli.write(b, "Running", path, text)
        if icon:
            cli.icon(b, "Running", path, icon)
    cli.run(b, "-q", "share-note", "Running", "Training plan", a, "--role", "viewer")

    step("carol: vault Design; her logo ideas shared with alice on their own")
    cli.run(c, "-q", "vault", "create", "Design")
    cli.run(c, "-q", "vault", "icon", "Design", "🎨")
    for path, icon, text in DESIGN:
        cli.write(c, "Design", path, text)
        if icon:
            cli.icon(c, "Design", path, icon)
    cli.run(c, "-q", "share-note", "Design", "Logo ideas", a, "--role", "viewer")

    step("alice: single notes shared with carol (the apple pie, the design brief)")
    # Shared with its comments (the default); Carol answers from the
    # command line, mentioning Alice.
    pie = cli.comment(a, "Home", "Recipes/Apple pie", "a pinch of nutmeg", "Carol, is this right? I can't read the card here.")
    cli.run(a, "-q", "share-note", "Home", "Recipes/Apple pie", c, "--role", "editor")
    cli.run(c, "-q", "shared", "comments", "reply", "Apple pie", pie,
            "Yes, a pinch — and @Alice, Grandma wrote “a little cinnamon” next to it.")
    cli.run(a, "-q", "share-note", "Work", "Projects/Design brief", c, "--role", "editor")
    cli.run(c, "-q", "shared", "write", "--append", "Apple pie",
            stdin="\n> Carol: Grandma always added a spoon of lemon juice. 🍋\n".encode())
    cli.run(c, "-q", "shared", "write", "--append", "Design brief",
            stdin="\n## From Carol\n\nFirst sketches are in my *Logo ideas* note (shared with you).\n".encode())

    step("alice: an AI agent with its own identity and its own vault")
    secret = data / "assistant.secret"
    out, _ = cli.run(a, "agent", "create", "Home assistant", "--secret-out", str(secret))
    client_id = next(l.split(":", 1)[1].strip() for l in out.splitlines() if l.startswith("client id:"))
    cli.run(AGENT_PROFILE, "-q", "agent", "setup", "--server", url,
            "--client-id", client_id, "--client-secret-file", str(secret))
    cli.run(a, "-q", "vault", "create", "Assistant")
    cli.run(a, "-q", "vault", "icon", "Assistant", "🤖")
    cli.run(a, "-q", "vault", "share", "Assistant", "Home assistant", "--role", "editor")
    cli.write(AGENT_PROFILE, "Assistant", "Weekly digest.md", note("""
        # Weekly digest

        *Written by the Home assistant agent, which can only see this vault.*

        - 3 quick notes are waiting to be sorted
        - The garden needs water on Friday
        - Book club meets on Thursday: *Piranesi*
        """))
    return keys


# ---------------------------------------------------------------- approving browsers


def approve_pending(cli):
    """Approve pending devices (browsers signing in) of the demo users."""
    for user in USERS:
        out, _ = cli.run(user, "--json", "devices", "list", check=False)
        try:
            devices = json.loads(out)
        except ValueError:
            continue
        for dev in devices:
            if dev.get("status") == "pending":
                cli.run(user, "-q", "devices", "approve", dev["id"], "--yes", check=False)
                print(f"  approved {user}'s new device '{dev.get('name', '?')}'", flush=True)


# ---------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--listen", default="127.0.0.1:8099")
    ap.add_argument("--data", default=str(ROOT / "data" / "demo"), help="where the demo lives")
    ap.add_argument("--no-approve", action="store_true", help="don't approve browsers automatically")
    ap.add_argument("--seed-only", action="store_true", help="stop once the data is there")
    ap.add_argument("--fresh", action="store_true", help="delete the demo's data first and create it again")
    ap.add_argument("--lifetime", type=duration, metavar="DURATION",
                    help="stop after this long (e.g. 1h; seeding included) and tell visitors when; "
                         "with --fresh and something that restarts it, a demo that starts over")
    ap.add_argument("--public-url", help="the address people open, if not the --listen one (behind a reverse proxy)")
    ap.add_argument("--dev-mode-anywhere", action="store_true",
                    help="let others reach it: the server refuses development mode on a public "
                         "address or an https URL without this. Anyone who reaches it can sign in as anyone")
    ap.add_argument("--server-log", metavar="PATH", help="where the server logs to ('-': here; default: server.log in --data)")
    ap.add_argument("--server-bin", type=Path, default=ROOT / "target" / "debug" / "notekpr-server")
    ap.add_argument("--cli-bin", type=Path, default=ROOT / "target" / "debug" / "notekpr")
    ap.add_argument("--web-dir", type=Path, default=ROOT / "dist" / "web")
    ap.add_argument("-v", "--verbose", action="store_true", help="print every CLI command")
    args = ap.parse_args()

    host, port = args.listen.rsplit(":", 1)
    url = f"http://{'localhost' if host in ('127.0.0.1', '0.0.0.0') else host}:{port}"
    public_url = (args.public_url or url).rstrip("/")
    # Seeding included, so visitors can be told when it ends.
    ends = time.time() + args.lifetime if args.lifetime else None
    data = Path(args.data).resolve()
    marker = data / "demo.json"
    server_bin, cli_bin = args.server_bin.resolve(), args.cli_bin.resolve()
    for p in (server_bin, cli_bin, args.web_dir / "index.html"):
        if not p.exists():
            sys.exit(f"{p} is missing: run `task demo` (it builds everything first)")
    if port_in_use(host, int(port)):
        sys.exit(
            f"something already listens on {args.listen} (another program, or a demo that is running).\n"
            f"Choose another address: task demo DEMO_LISTEN=127.0.0.1:8097  (or --listen)"
        )
    if args.fresh:
        wipe(data)
    if data.exists() and not marker.exists() and any(data.iterdir()):
        sys.exit(f"{data} holds an unfinished demo: run `task demo:reset` (or --fresh) and try again")
    data.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ)
    env["NOTEKPR_CONFIG_DIR"] = str(data / "cli" / "config")
    env["NOTEKPR_DATA_DIR"] = str(data / "cli" / "data")
    env.pop("NOTEKPR_PROFILE", None)
    env.pop("NOTEKPR_RECOVERY_KEY", None)
    cli = Cli(cli_bin, env, args.verbose)

    log_path = Path(args.server_log) if args.server_log not in (None, "-") else data / "server.log"
    log = None if args.server_log == "-" else open(log_path, "ab")
    extra = (["--dev-mode-anywhere"] if args.dev_mode_anywhere else []) + (
        ["--demo-resets-at", str(int(ends))] if ends else [])

    def start_server(listen):
        return subprocess.Popen(
            [str(server_bin), "--dev-mode", "--web-dir", str(args.web_dir.resolve()),
             "--data-dir", str(data / "server"), "--listen", listen,
             "--public-url", public_url, "--log-format", "pretty",
             # alice sees the administration page.
             "--admin", "dev:alice",
             # The script edits faster than anyone types: without this, a note
             # gets one revision per 30 s however often it changes.
             "--snapshot-cooldown-secs", "0", *extra],
            stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
        )

    # Reachable by others: seed while only this machine can connect (the
    # same port on loopback, so the CLI profiles' address stays right), then
    # open up — nobody signs in as alice before the script made her.
    exposed = host not in ("127.0.0.1", "localhost", "[::1]")
    server = start_server(f"127.0.0.1:{port}" if exposed and not marker.exists() else args.listen)
    # Stopped by a signal too (not only Ctrl+C): stop the server with us.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        info = wait_for_server(url, server)
        if not info.get("dev_mode"):
            sys.exit("the server isn't in development mode; refusing to create demo users")
        if marker.exists():
            step(f"demo data already there ({data.relative_to(ROOT) if data.is_relative_to(ROOT) else data})")
        else:
            started = time.time()
            seed(cli, url, data)
            marker.write_text(json.dumps({"users": USERS, "server": url, "created": int(time.time())}))
            step(f"done in {time.time() - started:.0f} s")
            if exposed:
                stop(server)
                server = start_server(args.listen)
                wait_for_server(url, server)
        if args.seed_only:
            return
        approve = (
            "A new browser: approve it with the CLI or the recovery key."
            if args.no_approve
            else "A new browser: choose “Approve from another device” — the demo approves it."
        )
        warning = ("Reachable by others (--dev-mode-anywhere): anyone who opens it can sign in as anyone."
                   if args.dev_mode_anywhere else
                   "Development mode: anyone can sign in as anyone. Never expose it.")
        until = f"\n  It stops at {time.strftime('%H:%M', time.localtime(ends))} (--lifetime)." if ends else ""
        print(textwrap.dedent(f"""
            Demo running on {public_url}  (server log: {'here' if log is None else log_path})

              Sign in as alice (most to see), bob, carol, dave or eve — one user per tab.
              {approve}
              Recovery keys: {data / 'recovery-keys.txt'}
              CLI as a demo user:  task demo:cli -- --profile alice ls Home

            {warning}
            Ctrl+C stops the server; `task demo:reset` starts over.""") + until + "\n", flush=True)
        while server.poll() is None:
            if ends and time.time() >= ends:
                step("lifetime over: stopping")
                return
            if not args.no_approve:
                approve_pending(cli)
            time.sleep(2)
        sys.exit(f"the server stopped (exit {server.returncode}); see {data / 'server.log'}")
    except KeyboardInterrupt:
        print("\nstopping", flush=True)
    finally:
        stop(server)


def stop(server):
    if server.poll() is None:
        server.send_signal(signal.SIGTERM)
        try:
            server.wait(10)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    main()
