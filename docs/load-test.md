# Load test with a live view

A crowd of simulated people works with a real server while you watch: they
arrive, form teams, share vaults, type together in the same notes, upload
pictures and share notes. Midway the server is killed and comes back. At
the end every copy of every note is compared. Inspired by TigerBeetle's
demos: nothing on screen is made up — every dot that moves is a request
that happened.

```sh
task load                                  # 100 people, 3 minutes, server limited to 1 CPU / 512 MB
task load USERS=300 DURATION=5m CPUS=2 MEMORY=1g
task load -- --seed 42                     # repeat a run (same people, teams and choices)
task load:native                           # without Docker: the server as a local process, no limits
```

`task load` builds the server image (`task docker:build`), the dashboard and
the load runner, opens <http://localhost:8098> and starts. When the run is
over the report stays on screen (and in `target/load/report-*.json`) until
you press Ctrl+C; the container is removed either way.

## What you see

* **The world**: the server in the middle, people around it, grouped by
  team. Each edit flies up to the server in its writer's colour and on to
  everyone else in that vault; pictures are squares. People typing pulse;
  people offline turn into red rings.
* **The server box**: its CPU and memory against the container's limits, and
  requests per second.
* **Three screens**: the shared note of the three biggest teams, as one of
  their people sees it, with the cursors of whoever is typing. Under each,
  *on the server*: the newest change to that note as the server stores it —
  read back from the server, so it is exactly what it has. Ciphertext.
* **Charts**: server CPU and memory (with their limits), requests per second
  with the server's p99, edits per second, the time from one person's
  keystroke to everyone else's copy (p50 and p99), and who is online.
* **The phases**: arriving → teams sharing vaults → busy → *server crashed*
  (SIGKILL: no shutdown, no last writes) → recovering → busy → checking →
  the report.

## What happens

1. **Arriving** (`--arrive`, 15 s): each person signs up in development
   mode — account keys and a device like any new user — and creates a vault
   of their own.
2. **Teams**: people form teams of 3 to 7. Each team's first member creates
   the team's vault, shares it with the others (signed into the vault's
   membership log, keys shared with HPKE) and writes the first notes: a
   shared one with a part for each member, and a few that link to each
   other.
3. **Busy** (`--duration`): everyone keeps doing what people do in notes:
   mostly typing — a sentence or two, a few characters at a time, into their
   part of a team note (often the shared one, so several people type in it
   at once) — and also starting notes, uploading pictures (real PNGs,
   20–180 KB), quick notes, renaming notes (links in other notes follow),
   trashing old quick notes, sharing a note with someone from another team,
   and saving revisions.
4. **The crash** (halfway, unless `--no-crash`): `docker kill` (SIGKILL),
   six seconds of nothing, `docker start` on the same data. People notice,
   keep writing offline, reconnect with backoff and upload what they wrote.
5. **Checking**: work stops; once everyone is online with nothing left to
   upload, every person catches up once more, and then:
   * every copy of every note that more than one person has is compared —
     they must all be the same;
   * every sentence anyone finished typing must be in the note.

   The run passes (exit code 0) only if both hold; otherwise it exits 1.

## What is real and what isn't

* **The server is the real image**, limited with Docker's `--cpus` and
  `--memory` (no swap), in development mode (`--dev-mode`, published on
  127.0.0.1 only) so the crowd can sign in without an identity provider.
  Its per-address rate limits are raised: every simulated person comes from
  this machine, and the limits would treat them as one.
* **The people are real clients**: the same library as the `coolpsonotes`
  CLI — real account and device keys, end-to-end encryption, signed changes,
  the membership log, a WebSocket each for live changes. Their copies live
  in `target/load/people/`.
* **Their uploads go over HTTP**, as the CLI's do; the browser sends edits
  over its WebSocket instead. They send no presence (cursors) to the
  server, which the browser does.
* **Everything runs on one machine**: the clients compete with the server
  for this machine's CPU (the server can't use more than its limit), and the
  network is loopback, so latencies are mostly the server's own work and
  queueing. Treat the numbers as a picture of the server's behaviour under a
  given load, not of a deployment on the internet.
* **Keystroke → others' copies** is measured exactly: only its writer
  adds to their part of a note, so each keystroke is recorded with the length
  of the part after it, and another person's copy having that much of the
  part means the keystroke reached them. The report's p50/p99 leave out the
  crash and the recovery (the live chart doesn't).
* **CPU and memory** come from `docker stats` (with `--native`, from
  `/proc`); **requests, request times and stored changes** from the server's
  own `/metrics`.
* The dashboard shows note text because the load runner *is* the clients:
  it reads its people's decrypted copies, as their own apps would. The
  server's side is the ciphertext panel.

## Options

`coolpsonotes-load --help` lists them all; `task load -- --flag` passes them
on.

| Flag | Default | |
|---|---|---|
| `--users N` | 100 | how many people |
| `--duration D` | 3m | how long they work (`90s`, `3m`) |
| `--arrive D` | 15s | over how long they arrive (`0` = all at once) |
| `--cpus N`, `--memory M` | 1, 512m | the container's limits |
| `--image I` | coolpsonotes:latest | the server image |
| `--native` | | run `target/release/coolpsonotes-server` as a process instead (Linux; no limits) |
| `--seed N` | random | repeat a run |
| `--no-crash` | | don't kill the server midway |
| `--dashboard ADDR` | 127.0.0.1:8098 | where the live view is served |
| `--open` | | open the live view in the browser (`task load` does) |
| `--exit` | | stop when done instead of keeping the live view up (scripts, CI) |
| `--out DIR` | target/load | people's copies and the reports |

Failed actions are logged with `LOAD_LOG=coolpsonotes_load=warn`.

## The code

| Crate | |
|---|---|
| `crates/load` | the runner: the server ([`server.rs`](../crates/load/src/server.rs)), the people ([`people.rs`](../crates/load/src/people.rs), [`world.rs`](../crates/load/src/world.rs)), the server's metrics, the check, the dashboard's WebSocket |
| `crates/load-events` | what the runner streams to the dashboard |
| `crates/load-ui` | the dashboard: Rust compiled to WebAssembly, drawing on a canvas (`scripts/build-load-ui.sh` → `dist/load`) |
