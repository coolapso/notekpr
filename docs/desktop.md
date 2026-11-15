# The desktop app

notekpr for your computer: the app you know from the browser, in a window
of its own, that keeps running in the background, sits in the tray, can
start when you log in, and keeps your notes as a folder of Markdown files
too. Linux for now ([install it](#install-it)).

It is a client of your notekpr server like the browser and the CLI: your
notes are encrypted on your computer before anything leaves it. Or it keeps
your notes [only on this computer](#only-on-this-computer), with no server
and no account.

## What it does

* **The app, in its own window.** Everything the web app does: writing,
  drawings, sharing, comments, settings. Its files ship inside the app, so
  what runs on your computer is the version you installed.
* **In the background.** Closing the window hides it in the tray (*Close
  to the tray*, on by default): your notes keep syncing and the window
  opens again at once. *Quit notekpr* in the tray's menu quits. Starting
  notekpr again while it runs shows its window.
* **Start at login.** *Start notekpr when you log in* starts it in the tray,
  without a window.
* **A Markdown folder.** Your vaults as a folder of ordinary Markdown files,
  synced both ways while the app runs, for Obsidian, VS Code, Neovim or any
  other editor (more in [Your own editor](own-editor.md)).
* **Without the server.** Like a browser, it keeps an encrypted copy of
  your vaults so it opens at once, but it asks the server for the keys each
  time it starts. *Settings → Devices → Keep this computer usable offline*
  keeps the keys too, so it opens and edits your notes when the server
  can't be reached ([what that means](security.md#local-storage)). The
  Markdown folder works offline either way.

You don't need the [CLI](cli.md) for any of that. It is still the tool for
[AI agents](agents.md) (its gateway), scripts, and computers without a
screen.

## Install it

From 0.17.0 on, each release on
[GitHub](https://github.com/coolapso/notekpr/releases) carries the app for
Linux: as packages (amd64 and arm64) for systems with glibc 2.35 and
WebKitGTK 4.1 or newer, and as a Flatpak (x86_64) for any Linux:

| System | Install |
|---|---|
| Any Linux | the release's Flatpak, `notekpr-desktop_<version>_x86_64.flatpak`: `flatpak install --user ./notekpr-desktop_<version>_x86_64.flatpak` (it brings GNOME's runtime from Flathub; it doesn't update itself yet: install a newer release's the same way; [below](#the-flatpak)) |
| Arch | from the AUR: `notekpr-desktop-bin` |
| Debian 12+, Ubuntu 22.04+, Mint 21+ | the release's `.deb`: `sudo apt install ./notekpr-desktop_<version>_amd64.deb` (`arm64` on ARM) |
| Fedora | from COPR: `sudo dnf copr enable coolapso/notekpr`, then `sudo dnf install notekpr-desktop`; or the release's `.rpm`: `sudo dnf install ./notekpr-desktop-<version>-1.x86_64.rpm` (`aarch64` on ARM) |
| Other Linux | the release's `notekpr-desktop_<version>_linux_<arch>.tar.gz` (the app, its desktop entry and icons; it needs WebKitGTK 4.1 and libayatana-appindicator installed), or [build it from source](#build-it) |

It shows in your applications as *notekpr*. COPR's package updates with the
rest of Fedora; the release's `.deb` doesn't update itself (no package
repository yet): install a newer release's the same way. The packages
bring what the app needs from your system's own: WebKitGTK, the tray's
library (libayatana-appindicator) and xdg-utils. The CLI is a package of
its own (`notekpr-cli`, [cli.md](cli.md#packages)), and you don't need it
for the app.

## Getting started

1. **Where your notes are.** The first time, the app asks where you keep
   your notes. *On a notekpr server*: its address — the one you open in a
   browser, like `https://notes.example.com`. The app checks the server
   and starts again with it. The server must be recent enough for the app
   (the app says so if not). Or *Only on this computer*: no server and no
   account ([below](#only-on-this-computer)); then there is no step 2.
2. **Sign in.** *Continue with …* opens your browser at your server's
   sign-in page; sign in there as always (passkeys and your password manager
   work). Then your server asks whether to sign the app in, with a code:
   **Continue** only if it is the code the app shows (a sign-in you didn't
   start shows another, or none). The browser sends you back to the app;
   you can close that tab then.
3. **This computer.** As for any new device: on your first device the app
   makes your keys and shows your recovery key; otherwise approve it from a
   device you already use, or unlock it with your recovery key
   ([Your other devices](getting-started.md#your-other-devices)).

*Settings → Desktop app* holds the app's own settings: the tray, starting
at login, the Markdown folder and where your notes are.

## Only on this computer

No server, no account to make: the app keeps your notes on this computer
only. It runs notekpr's server inside itself while it runs, on this
computer only, and signs itself in to it: there is nothing to sign in to,
and nothing else gets in. Everything works as with a server — vaults,
quick notes, drawings, comments, history, attachments, search, the
Markdown folder — and is stored encrypted as on any server. The first
time, the app makes your keys and shows your recovery key, as on a server:
keep it, it opens your notes again if the app loses what it keeps for its
window.

What needs a server: your other devices (another computer, your phone, a
browser), sharing vaults or notes with other people, invitations, and AI
agents with vaults of their own (an assistant can work with the Markdown
folder instead). Where they'd be, the app says they need a server. Signing out and the
list of sessions aren't there either: your account on this computer is
what guards your notes.

Your notes exist only on this computer, so keep a copy: a Markdown folder
(every note as a file, which your backups copy), or an export now and then
(*Settings → Account → Data*: your vaults and quick notes as Markdown files
in a `.zip`; imported, they become new vaults, without their history or
comments).

**One at a time.** The app is either on a server or on this computer.
*Settings → Desktop app → Where your notes are* switches; each keeps its
own notes, and switching back finds them again. Switching stops keeping
the Markdown folder in sync (its files stay): choose one again after,
best a folder of its own for each. To move your notes to a server, export
them here, switch to the server, sign in and import the export there.

## The Markdown folder

*Settings → Desktop app → Markdown folder → Choose a folder…* and pick an
empty folder (or one notekpr kept before). The app asks you, in a window of
the system's, to confirm that it may write your notes there decrypted — any
program you run can read them, and backup or cloud tools may copy them.
Then each vault becomes a folder in it, and:

* changes you make to the files (with any editor) reach your notes and
  everyone in the vault within moments; changes from others are written
  into the files;
* it keeps going while the window is closed to the tray, and after the app
  starts again;
* it works offline: changes wait until the server can be reached.

How changes made at the same time are merged, renames, deletes and
conflicts: the same as the CLI's folder ([cli.md §4–7](cli.md#4-sync-and-the-markdown-mirror)),
since it is the same code.

*Stop…* stops it: the files stay where they are, and the app forgets its own
copies for the folder. The folder follows the same rules as the CLI's: a
folder the CLI already keeps in sync (or one in or around it), one inside
notekpr's own data or your home folder itself is refused — and the CLI
refuses the app's folder in turn. Until the app can bring notes in, the
folder must be empty, too; to bring notes from a folder in, use *Import
notes* in the app or [`notekpr setup`](own-editor.md#coming-from-obsidian).

The folder uses this computer's device and the window's sign-in: signing
out stops it (signing in again resumes it), and *Forget this computer*
asks whether to forget its keys and copies too (never the files). If this
computer is removed from your devices elsewhere, the folder stops; *Stop…*
then forgets what it kept.

Exports and attachments you download land in your Downloads folder, under
a name of their own (never over another file).

## The tray

Most Linux desktops show tray icons (KDE, Cinnamon, Xfce, sway or Hyprland
with a bar that has a tray). GNOME needs the *AppIndicator and
KStatusNotifierItem Support* extension. Without a tray, closing the window
quits the app, and *Close to the tray* says so.

The tray's menu: *Open notekpr*, *Open the Markdown folder*, *Settings* and
*Quit notekpr*.

## Where it keeps things

| What | Where (Linux) |
|---|---|
| Its settings (where your notes are, the tray) | `~/.config/notekpr-desktop/default/settings.json` |
| The window's data (this device's key, encrypted copies of your vaults), one folder per server (`this-computer` for notes only on this computer) | `~/.local/share/notekpr-desktop/default/webview/` |
| Only on this computer: the server's data (its database and attachments, encrypted as on any server) | `~/.local/share/notekpr-desktop/default/this-computer/` |
| The Markdown folder's keys and encrypted copies | `~/.local/share/notekpr-desktop/default/folder/` |
| Starting at login | `~/.config/autostart/notekpr.desktop` |

What is protected how: [security.md](security.md#local-storage).
`NOTEKPR_DESKTOP_CONFIG_DIR` and `NOTEKPR_DESKTOP_DATA_DIR` replace
`~/.config/notekpr-desktop` and `~/.local/share/notekpr-desktop`, as do
`--config-dir` and `--data-dir`. A copy of the app with folders of its own
runs beside the installed one, and starts at login with an entry of its
own (`notekpr (development …)`, with those folders): development runs
(`task desktop`) keep theirs in the repository's `data/`
([development.md](development.md)). Other copies of notekpr on the
computer don't see such a copy's Markdown folders.
`NOTEKPR_LOG=debug` shows more of what it does (on standard error).

**Several people side by side** (development, testing):
`notekpr-desktop --profile bob` keeps its own settings, sign-in, data and
folder, like the CLI's `--profile`.

### The Flatpak

The Flatpak keeps the same things under `~/.var/app/md.notekpr.Notekpr/`
(`config/notekpr-desktop/…`, `data/notekpr-desktop/…`), and works the same,
with these differences:

* It may read and write your home folder, so that the Markdown folder can be
  anywhere in it you choose (and downloads land in Downloads); not the rest
  of the system.
* *Start notekpr when you log in* asks your system, which may ask you once
  whether notekpr may run in the background. Not every desktop can (GNOME
  and KDE can; Hyprland, for one, can't): the app then says so.
* The CLI and the app still refuse each other's Markdown folders, whichever
  way each is installed: each looks for the other in both places.

## Build it

To build it yourself, or to work on it: the app is `crates/desktop` ([Tauri](https://tauri.app) 2 around the web
client, `crates/web`). It needs what the web client needs
([development.md](development.md)), plus WebKitGTK 4.1 and GTK 3 with their
headers, and libayatana-appindicator for the tray:

| System | Packages |
|---|---|
| Arch | `webkit2gtk-4.1 libayatana-appindicator` |
| Debian, Ubuntu | `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev` |

```sh
task build:desktop       # target/release/notekpr-desktop, the optimized web client inside
task desktop             # run it (unoptimized web client), e.g. against `task dev`
```

The app's files are taken from the folder `TAURI_CONFIG` names
(`{"build":{"frontendDist":"…"}}`); without it, a build has a placeholder
page instead (what `task test` and `task lint` build).
