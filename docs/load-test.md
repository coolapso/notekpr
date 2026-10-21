# Load test with a live view

A crowd of simulated people works with a real server while you watch: they
arrive, form teams, share vaults, type together in the same notes, upload
pictures and share notes. The crowd grows level by level, and in every
level the server is killed and stays down a little longer. At the end every
copy of every note is compared. Inspired by TigerBeetle's demos: nothing on
screen is made up — every dot that moves is a request that happened.

```sh
task load                                  # 10 levels: 100, 200, … 1000 people; server limited to 1 CPU / 512 MB
task load LEVELS=1 DURATION=2m             # one crowd of 100, one outage of 6 s
task load USERS=200 STEP=200 LEVELS=5 CPUS=2 MEMORY=1g SPEED=2
task load -- --seed 42                     # repeat a run (same people, teams and choices)
task load -- --downtimes 5s,10s,20s        # choose the outages (the last repeats)
task load FROM=6 CPUS=2 MEMORY=1g          # start right at level 6 (600 people), e.g. after a game over there
task load REALISTIC=1 USERS=500 STEP=500   # people as busy as real ones: 500, 1000, … 5000
task load:native                           # without Docker: the server as a local process, no limits
task load:remote SERVER=https://notes-test.example.com   # a server elsewhere (see below)
```

`task load` builds the server image (`task docker:build`), the dashboard and
the load runner and opens <http://localhost:8098> on the **start screen**:
choose the level to start at, how many levels, people per level, the
crowd (busy or [realistic](#realistic-people)), the server's CPU and
memory, typing speed, how long each level lasts, whether there are outages
and whether a game over ends the run or it keeps going
(arrow keys or clicks; the `task load` variables are the starting choices),
then PRESS START. After the report — or a GAME
OVER — *insert coin* takes you back to the start screen, starting at the
level that ended it, to try again with more CPU or memory. `task load --
--now` skips the start screen (and `--exit` stops after one run, for
scripts). Ten levels take about 20 minutes; reports are also written to
`target/load/report-*.json`. Ctrl+C stops everything and removes the
container and the people's copies.

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
* **Charts** over the whole run (a line where each level began): server
  CPU and memory (with their limits), requests per second with the server's
  p99, edits per second, the time from one person's keystroke to everyone
  else's copy (p50 and p99), and who is online.
* **Levels** (top right): each finished level's people, requests per
  second and CPU while everyone works (not counting arrivals, the outage and
  the comeback; then CPU's peak, usually the comeback or the arrivals),
  keystroke latency, outage, and the comeback: *online in*
  (everyone connected again) and *caught up* (the changes people made
  during the outage are uploaded: no more waiting on devices than before
  the crash; "never" if not within two minutes — the server can't keep up
  at that level). The final report has the full table, also printed in the
  terminal.
* **The phases**, in each level: arriving → teams sharing vaults → busy →
  *server crashed* (SIGKILL: no shutdown, no last writes; a countdown to its
  restart) → recovering → busy. Then checking and the report.
* At high rates the dots are a sample (about 1,200 edits and 1,200 arrivals
  a second); the numbers count everything.

## What happens

Each level (`--levels`; the first has `--users` people, each next one
`--step` more):

1. **Arriving** (`--arrive`, 10 s): the level's newcomers sign up in
   development mode — account keys and a device like any new user — and
   create a vault of their own.
2. **Teams**: they form teams of 3 to 7. Each team's first member creates
   the team's vault, shares it with the others (signed into the vault's
   membership log, keys shared with HPKE) and writes the first notes: a
   shared one with a part for each member, and a few that link to each
   other. People from earlier levels keep working meanwhile.
3. **Busy** (`--duration` per level): everyone keeps doing what people do
   in notes: mostly typing — a sentence or two at about 18 characters a
   second, a few characters per upload, into their part of a team note
   (mostly the shared one, so several people type in it at once) — and also
   starting notes, uploading pictures (real PNGs, 15–90 KB), quick notes,
   renaming notes (links in other notes follow), trashing old quick notes,
   sharing a note with someone from another team, and saving revisions.
   When a shared note passes about 16 KB, its team starts a fresh page.
   `--speed` makes everyone faster or slower; `--realistic` makes them as
   busy as [real people](#realistic-people).
4. **The crash** (halfway through the level, unless `--no-crash`): `docker
   kill` (SIGKILL), the level's outage (`--downtimes`; by default from 2 s
   in the first level up to 90 s in the tenth), `docker start` on the same
   data. People notice, keep writing offline, reconnect with backoff (which
   grows up to a minute, so long outages also mean slower comebacks) and
   upload what they wrote.

**Game over**: when the server can't take a level — after the outage not
everyone is connected again within two minutes, or the changes made during
it aren't worked off within two minutes, or it dies by itself (stops
answering for 20 s, or Docker kills it for running out of memory), or most
of a level's newcomers can't even sign up — there are no more levels: the
live view says GAME OVER, with the level, the reason and the best level it
survived (`--keep-going` goes on regardless). The copies are still checked —
with the container's limits lifted (`docker update`: 4 CPUs and 4 GB, or
four times its limits), so the server can take what everyone wrote meanwhile
and the check shows whether anything was *lost*, not whether the server
could keep up: "game over, but nothing was lost" is the interesting part. To try that
level again with more CPU or memory without replaying the ones before it,
start there: `task load FROM=6 CPUS=2` — everyone up to level 6 arrives at
once (spread over a warm-up as long as their levels' arrivals together),
forms their teams, and level 6 begins with its own outage.

At the end:

5. **Checking**: work stops; once everyone is online with nothing left to
   upload, every person catches up once more, and then:
   * every copy of every note that more than one person has is compared —
     they must all be the same;
   * every sentence anyone finished typing must be in the note.

   The run passes (exit code 0) only if both hold — a game over is a
   capacity limit, not a failure. If changes are still waiting on people's
   devices when the copies are compared (the server couldn't take them even
   so; it is started again whenever it dies meanwhile), copies can differ
   for now and sentences can be only on their writer's device: the check
   couldn't finish (exit code 3, amber), which is not a loss. Otherwise it
   exits 1 and says, for each missing sentence, who typed it into which note, what
   every member's copy holds of that note (in the index, pending, held,
   waiting for a key), and a replay of the note's changes as the server
   stores them — when the sentence appeared and whose change removed it.
   (This is how the load test found that opening a newly shared vault twice
   at once could lose what was written into the first copy.)

A run that was killed outright (not Ctrl+C) leaves its container and
people's copies behind; the next run removes them.

When the server is too busy to answer, sign-ups are tried again (five
times, waiting longer each time); someone who still can't get in is counted
as an error and the run goes on without them, as does a team whose vault
couldn't be set up.

## Realistic people

By default the crowd is busier than anyone, on purpose: everyone types
nonstop, about 4–5 requests a second each. That finds the server's limits
with a few hundred people, but says little about how many *real* people a
server can take. With `--realistic` (`REALISTIC=1`, or CROWD on the start
screen) people behave like someone with the app open:

* most of the time they read, think or are elsewhere: between two things,
  10 s to a minute (60 %), one to three minutes (30 %) or three to ten
  minutes away (10 %) — about a minute and a half on average;
* when they write, it's a sentence or two at about 40 words a minute, each
  key sent on its own (as the browser does at that pace);
* they don't start in step: each one first reads for up to 90 s.

That's about one upload every 3 s per person — with syncing, about half a
request a second, a tenth of a busy one — so levels need more people: `task
load REALISTIC=1 USERS=500 STEP=500`. What servers of which size took this
way: [operations](operations.md#how-big-a-machine). Every connected person still holds a
WebSocket, and still reconnects and catches up after an outage, so
connections and comebacks weigh relatively more than with busy people.

## Against a server elsewhere

`--server URL` (`task load:remote SERVER=…`) runs the crowd against a
server you run somewhere else — the real network, TLS and proxy in
between — instead of starting one here. It has to be a **throwaway server
in development mode**: the people sign in without an identity provider.
Anyone who can reach it can sign in as anyone, so give it no real data,
limit who can reach it (a firewall rule for the load machine's address)
and take it down afterwards. For example, on the test machine:

```sh
docker run -d --name coolpsonotes-loadtest -p 8080:8080 -v /data \
  -e COOLPSONOTES_DEV_MODE=true -e COOLPSONOTES_DEV_MODE_ANYWHERE=true \
  -e COOLPSONOTES_RATE_LIMIT_RPS=1000000 -e COOLPSONOTES_AUTH_RATE_LIMIT_PER_MIN=1000000 \
  -e COOLPSONOTES_PUBLIC_URL=https://notes-test.example.com \
  --cpus 1 --memory 512m coolpsonotes:latest
```

(behind your HTTPS proxy, or `http://` and a port for a quick try). The
rate limits must be raised because every simulated person comes from the
load machine; if they aren't, the run says so as soon as the server turns
requests away.

* **Outages** need a way to kill and start it: `--kill-command "ssh
  test-box docker kill coolpsonotes-loadtest" --start-command "ssh test-box
  docker start coolpsonotes-loadtest"` (run with `sh -c`). Without them
  there are none.
* **CPU and memory** come from the server's own `/metrics`
  (`process_cpu_seconds_total`, `process_resident_memory_bytes`; Linux
  servers): 100 % is one core, there are no limit lines. If `/metrics` needs
  a token, pass `--metrics-token` (or `COOLPSONOTES_METRICS_TOKEN`); if the
  proxy hides `/metrics` (the bundled Caddyfiles do), the CPU, memory and
  request charts stay empty, and the rest works.
* **Accounts stay** on that server: each run's people sign in with names of
  their own (`anika-…`), so runs don't collide.
* The clients still run on this machine, so it needs the CPU and memory for
  them (see [below](#what-is-real-and-what-isnt)).

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
  queueing. A thousand people need several cores of their own; the runner
  raises its open-files limit for their connections.
* **People type fast**: about 18 characters a second while they type, each
  few characters an upload — busier than most people, on purpose (unless
  [realistic](#realistic-people)). Treat the numbers as a picture of the
  server's behaviour under a given load; against a server here, not of a
  deployment on the internet.
* **Keystroke → others' copies** is measured exactly: only its writer
  adds to their part of a note, so each keystroke is recorded with the length
  of the part after it, and another person's copy having that much of the
  part means the keystroke reached them. The report's p50/p99 leave out the
  crash and the recovery (the live chart doesn't).
* **CPU and memory** come from `docker stats` (with `--native`, from
  `/proc`; with `--server`, from the server's `/metrics`); **requests,
  request times and stored changes** from the server's own `/metrics`.
* The dashboard shows note text because the load runner *is* the clients:
  it reads its people's decrypted copies, as their own apps would. The
  server's side is the ciphertext panel.

## Options

`coolpsonotes-load --help` lists them all; `task load -- --flag` passes them
on.

| Flag | Default | |
|---|---|---|
| `--users N` | 100 | how many people (in the first level) |
| `--levels N`, `--step N` | 1, 100 | levels, and people added in each (`task load`: 10 levels) |
| `--from-level N` | 1 | start at level N (its people and everyone before arrive at once) |
| `--duration D` | 60s | how long everyone works in each level (`90s`, `3m`) |
| `--downtimes LIST` | see above | each level's outage, e.g. `5s,10s,30s` (the last repeats) |
| `--speed X` | 1 | how fast people are (2 = twice as fast) |
| `--realistic` | | people as busy as [real ones](#realistic-people) (`task load REALISTIC=1`) |
| `--arrive D` | 10s | over how long each level's newcomers arrive (`0` = all at once) |
| `--cpus N`, `--memory M` | 1, 512m | the container's limits |
| `--image I` | coolpsonotes:latest | the server image |
| `--native` | | run `target/release/coolpsonotes-server` as a process instead (Linux; no limits) |
| `--server URL` | | a [server elsewhere](#against-a-server-elsewhere) instead (`task load:remote SERVER=…`) |
| `--kill-command C`, `--start-command C` | | with `--server`: how to kill and start it for the outages |
| `--metrics-token T` | `$COOLPSONOTES_METRICS_TOKEN` | with `--server`: its `/metrics` token |
| `--seed N` | random | repeat a run |
| `--no-crash` | | don't kill the server midway |
| `--dashboard ADDR` | 127.0.0.1:8098 | where the live view is served |
| `--open` | | open the live view in the browser (`task load` does) |
| `--now` | | start right away with these settings, no start screen |
| `--exit` | | stop when done instead of keeping the live view up (scripts, CI; implies `--now`) |
| `--keep-going` | | go on with the next levels after the server couldn't take one |
| `--out DIR` | target/load | the reports |
| `--people-dir DIR` | /dev/shm/… | people's local copies (in memory where possible; removed afterwards) |

Failed actions are logged with `LOAD_LOG=coolpsonotes_load=warn`.

## The code

| Crate | |
|---|---|
| `crates/load` | the runner: the server ([`server.rs`](../crates/load/src/server.rs)), the people ([`people.rs`](../crates/load/src/people.rs), [`world.rs`](../crates/load/src/world.rs)), the server's metrics, the check, the dashboard's WebSocket |
| `crates/load-events` | what the runner streams to the dashboard |
| `crates/load-ui` | the dashboard: Rust compiled to WebAssembly, drawing on a canvas (`scripts/build-load-ui.sh` → `dist/load`) |
