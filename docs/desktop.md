# The desktop app

notekpr for your computer: the app you know from the browser, in a window
of its own, that keeps running in the background, sits in the tray, can
start when you log in, and keeps your notes as a folder of Markdown files
too. Linux for now; it isn't packaged yet, so it is built from source
([below](#build-it)).

It is a client of your notekpr server like the browser and the CLI: your
notes are encrypted on your computer before anything leaves it.

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

You don't need the [CLI](cli.md) for any of that. It is still the tool for
[AI agents](agents.md) (its gateway), scripts, and computers without a
screen.

## Getting started

1. **Your server.** The first time, the app asks for your notekpr server's
   address — the one you open in a browser, like
   `https://notes.example.com`. It checks the server and starts again with
   it. The server must be recent enough for the app (the app says so if
   not).
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
at login, the Markdown folder and the server.

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
| Its settings (the server, the tray) | `~/.config/notekpr-desktop/default/settings.json` |
| The window's data (this device's key, encrypted copies of your vaults), one folder per server | `~/.local/share/notekpr-desktop/default/webview/` |
| The Markdown folder's keys and encrypted copies | `~/.local/share/notekpr-desktop/default/folder/` |
| Starting at login | `~/.config/autostart/notekpr.desktop` |

What is protected how: [security.md](security.md#local-storage).
`NOTEKPR_DESKTOP_CONFIG_DIR` and `NOTEKPR_DESKTOP_DATA_DIR` replace
`~/.config/notekpr-desktop` and `~/.local/share/notekpr-desktop`.
`NOTEKPR_LOG=debug` shows more of what it does (on standard error).

**Several people side by side** (development, testing):
`notekpr-desktop --profile bob` keeps its own settings, sign-in, data and
folder, like the CLI's `--profile`.

## Build it

The app is `crates/desktop` ([Tauri](https://tauri.app) 2 around the web
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
