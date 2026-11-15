//! The desktop app's initial setup (docs/desktop.md "Getting started"):
//! after the keys, before the notes open, once for each account — and
//! what it shares with *Settings → Desktop app*: choosing the Markdown
//! folder, bringing notes in (docs/desktop.md "Notes already in the
//! folder"), taking over the CLI's (docs/desktop.md "From the CLI"). The
//! app asks in a system dialog before any of them changes anything.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::desktop::{self, CliFolder, FolderStatus, Picked};
use crate::state::{self, Phase, step, ui};
use crate::util;
use crate::views::icons::Icon;

/// The window's device and session, for the app's folder.
fn handoff() -> Result<desktop::Handoff, String> {
    step(|c| state::desktop_handoff(c))
        .ok_or_else(|| "This computer isn't set up as one of your devices yet.".to_string())
}

/// After an action: `done` once the app keeps a folder; a refusal said
/// where it happened (this may show before the notes open, where there are
/// no toasts); a "no" in the app's dialog is no error.
fn finish(r: Result<bool, String>, err: RwSignal<Option<String>>, done: Callback<()>) {
    match r {
        Ok(true) => done.run(()),
        Ok(false) => {}
        Err(e) if e == "cancelled" => {}
        Err(e) => err.set(Some(e)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum How {
    Move,
    Copy,
}

/// Choose the Markdown folder: an empty one, one notekpr kept, or one with
/// notes of its own, which are brought in as `notekpr setup` does — moved
/// into a vault's folder there, or copied into one in another folder.
/// `done` runs once the app keeps one.
#[component]
pub fn FolderChooser(#[prop(into)] done: Callback<()>) -> impl IntoView {
    let busy = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);
    // A folder with notes of its own, waiting for what to do with them.
    let with_notes = RwSignal::new(None::<Picked>);
    let how = RwSignal::new(How::Move);
    let vault = RwSignal::new("My notes".to_string());
    let choose = move |_| {
        busy.set(true);
        err.set(None);
        with_notes.set(None);
        spawn_local(async move {
            let r: Result<bool, String> = async {
                let Some(p) = desktop::choose_folder().await? else {
                    return Ok(false);
                };
                if let Some(why) = p.refused {
                    return Err(why);
                }
                if p.notes.is_some() {
                    how.set(How::Move);
                    with_notes.set(Some(p));
                    return Ok(false);
                }
                desktop::folder_start(handoff()?, p.pick).await?;
                Ok(true)
            }
            .await;
            busy.set(false);
            finish(r, err, done);
        });
    };
    let bring = move |_| {
        let Some(p) = with_notes.get_untracked() else {
            return;
        };
        let copy = how.get_untracked() == How::Copy;
        let name = vault.get_untracked().trim().to_string();
        if name.is_empty() {
            err.set(Some("Name the vault for them.".into()));
            return;
        }
        busy.set(true);
        err.set(None);
        spawn_local(async move {
            let r: Result<bool, String> = async {
                let to = if copy {
                    let Some(t) = desktop::choose_folder().await? else {
                        return Ok(false);
                    };
                    if let Some(why) = t.refused {
                        return Err(why);
                    }
                    Some(t.pick)
                } else {
                    None
                };
                desktop::folder_bring(handoff()?, p.pick, to, &name).await?;
                Ok(true)
            }
            .await;
            busy.set(false);
            if matches!(r, Ok(true)) {
                with_notes.set(None);
            }
            finish(r, err, done);
        });
    };
    view! {
        {move || err.get().map(|e| view! { <div class="alert error folder-error">{e}</div> })}
        {move || match with_notes.get() {
            None => view! {
                <div class="row">
                    <button class="btn choose-folder" disabled=move || busy.get() on:click=choose><Icon name="folder"/>" Choose a folder…"</button>
                    <Show when=move || busy.get()><span class="spinner inline"></span></Show>
                </div>
                <p class="muted small">"An empty folder, one notekpr kept before, or one with notes in it already (an Obsidian vault, say): they can be brought in. The files in it are decrypted: readable by any program you run on this computer."</p>
            }.into_any(),
            Some(p) => {
                let shown = p.shown.clone();
                let described = p.notes.as_ref().map(|h| h.described.clone()).unwrap_or_default();
                view! {
                    <div class="bring-in">
                        <p class="folder-notes">{format!("{shown} holds notes already: {described}. What should happen to them?")}</p>
                        <div class="choices">
                            <label class="check">
                                <input type="radio" name="bring" class="bring-move" prop:checked=move || how.get() == How::Move on:change=move |_| how.set(How::Move)/>
                                <span><b>"Move them into a vault there"</b>
                                <span class="muted small">{format!("Everything in {shown}, hidden files too (.obsidian), moves as it is into a folder of its own there, which syncs from then on. Close Obsidian and other apps using it first.")}</span></span>
                            </label>
                            <label class="check">
                                <input type="radio" name="bring" class="bring-copy" prop:checked=move || how.get() == How::Copy on:change=move |_| how.set(How::Copy)/>
                                <span><b>"Copy them into a vault in another folder"</b>
                                <span class="muted small">{format!("You choose that folder next: it becomes the Markdown folder, and {shown} stays as it is.")}</span></span>
                            </label>
                        </div>
                        <label class="field">
                            <span>"Vault"</span>
                            <input class="input vault-name" prop:value=move || vault.get() on:input=move |ev| vault.set(event_target_value(&ev))/>
                        </label>
                        <p class="muted small">"One of your vaults by that name (the notes join what it has), or a new one."</p>
                        <div class="row">
                            <button class="btn primary bring-in-button" disabled=move || busy.get() on:click=bring>
                                {move || if how.get() == How::Copy { "Choose where to copy them…" } else { "Move them in" }}
                            </button>
                            <button class="btn ghost" disabled=move || busy.get() on:click=choose>"Choose another folder"</button>
                            <Show when=move || busy.get()><span class="spinner inline"></span></Show>
                        </div>
                    </div>
                }.into_any()
            }
        }}
    }
}

/// The Markdown folders the CLI keeps for this account, offered to take
/// over (docs/desktop.md "From the CLI"); nothing shown when there are
/// none. `done` runs once the app keeps one.
#[component]
pub fn CliOffer(#[prop(into)] done: Callback<()>) -> impl IntoView {
    let found = RwSignal::new(Vec::<CliFolder>::new());
    let busy = RwSignal::new(false);
    let err = RwSignal::new(None::<String>);
    spawn_local(async move {
        if let Some(token) = crate::storage::token()
            && let Ok(v) = desktop::cli_folders(&token).await
        {
            found.set(v);
        }
    });
    let take = move |profile: String| {
        busy.set(true);
        err.set(None);
        spawn_local(async move {
            let r = async {
                desktop::folder_take_from_cli(handoff()?, &profile).await?;
                Ok(true)
            }
            .await;
            busy.set(false);
            if matches!(r, Ok(true)) {
                found.set(vec![]);
            }
            finish(r, err, done);
        });
    };
    view! {
        {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
        {move || found.get().into_iter().map(|f| {
            let profile = f.profile.clone();
            let how = match (f.service, f.running) {
                (Some(s), _) if s.active => "it syncs in the background",
                (Some(s), _) if s.enabled => "it starts when you log in",
                (_, true) => "it runs now",
                _ => "it isn't running now",
            };
            view! {
                <div class="cli-offer">
                    <div class="cli-offer-title"><Icon name="folder"/>{format!(" The CLI keeps {} in sync for you", f.shown)}</div>
                    <p class="small">{format!("Its profile '{}': {how}. The app can keep it instead, as it is: it stops the CLI's background sync and turns it off at login; the CLI forgets the folder and stays signed in, for its commands and gateway.", f.profile)}</p>
                    <div class="row">
                        <button class="btn primary take-over" disabled=move || busy.get() on:click=move |_| take(profile.clone())>{format!("Take over {}", f.shown)}</button>
                    </div>
                </div>
            }
        }).collect_view()}
        <CliWaiting/>
    }
}

/// While taking over the CLI's folder, a CLI still runs it: what to do,
/// and a way out.
#[component]
fn CliWaiting() -> impl IntoView {
    move || {
        ui().desktop.get().cli_wait.map(|w| {
            let command = w.command.clone();
            view! {
                <div class="cli-wait">
                    <p><span class="spinner inline"></span>{format!(" Waiting for the CLI to stop keeping {}…", w.dir)}</p>
                    {command.map(|c| {
                        let copy = c.clone();
                        view! {
                            <p class="small">"notekpr can't stop the CLI's background sync from here. Run this in a terminal:"</p>
                            <div class="row cli-command-row">
                                <code class="cli-command">{c}</code>
                                <button class="btn small" on:click=move |_| util::copy_to_clipboard(&copy)><Icon name="copy"/>" Copy"</button>
                            </div>
                        }
                    })}
                    <p class="muted small">{format!("If notekpr sync or notekpr gateway runs for the profile '{}' in a terminal, stop it there (Ctrl-C). notekpr goes on by itself then.", w.profile)}</p>
                    <div class="row"><button class="btn cancel-take" on:click=move |_| desktop::cancel_take()>"Cancel"</button></div>
                </div>
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Folder,
    Background,
}

/// The initial setup: the Markdown folder, then — the first time only —
/// the tray and starting at login. Each can be left for later.
#[component]
pub fn DesktopSetup(again: bool) -> impl IntoView {
    let ui = ui();
    let asked = !again && desktop::info().is_some_and(|i| i.background_asked);
    let at = RwSignal::new(Step::Folder);
    let next = move || {
        if at.get_untracked() == Step::Folder && !asked {
            at.set(Step::Background);
        } else {
            spawn_local(state::finish_desktop_setup());
        }
    };
    // Once the app keeps a folder, the step shows it, with Next.
    let done = Callback::new(|()| {});
    // Only the folder's own changes re-draw its step (not, say, waiting for
    // the CLI, which the step shows itself).
    let folder = Memo::new(move |_| ui.desktop.get().folder);
    view! {
        <div class="auth">
            <div class="auth-card desktop-setup">
                <div class="brand"><span class="logo">"◆"</span>" notekpr"</div>
                {move || match at.get() {
                    Step::Folder => view! {
                        <h1>"Your notes as Markdown files"</h1>
                        <p>"notekpr can keep your notes as a folder of ordinary Markdown files too, synced both ways while it runs: for Obsidian, VS Code, Neovim and other editors, and for AI agents. Each vault is a folder in it."</p>
                        {move || match folder.get() {
                            FolderStatus::Off => view! {
                                <CliOffer done/>
                                <FolderChooser done/>
                                // Not while the app waits for the CLI: Cancel ends that.
                                <button class="btn ghost not-now" disabled=move || ui.desktop.get().cli_wait.is_some()
                                    on:click=move |_| next()>"Not now"</button>
                                <p class="muted small">"Settings → Desktop app has it, and this setup, any time."</p>
                            }.into_any(),
                            FolderStatus::Stopped { dir, error } => view! {
                                <div class="alert error">{format!("{dir} isn't kept in sync: {error}")}</div>
                                <button class="btn primary" on:click=move |_| next()>"Next"</button>
                            }.into_any(),
                            FolderStatus::Starting { dir } => view! {
                                <p class="folder-status"><span class="spinner inline"></span>{format!(" Starting {dir}…")}</p>
                                <button class="btn primary setup-next" on:click=move |_| next()>"Next"</button>
                            }.into_any(),
                            FolderStatus::Syncing { dir, .. } => view! {
                                <p class="folder-status">{format!("Keeping {dir} in sync.")}</p>
                                <button class="btn primary setup-next" on:click=move |_| next()>"Next"</button>
                            }.into_any(),
                        }}
                    }.into_any(),
                    Step::Background => view! { <Background next/> }.into_any(),
                }}
            </div>
        </div>
    }
}

/// The app's own settings, asked once: closing to the tray, starting at
/// login. Changed as they are ticked, as in Settings.
#[component]
fn Background(next: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let info = RwSignal::new(desktop::info());
    let err = RwSignal::new(None::<String>);
    let set = move |what: &'static str, on: bool| {
        spawn_local(async move {
            let r = match what {
                "tray" => desktop::set_close_to_tray(on).await,
                _ => desktop::set_autostart(on).await,
            };
            err.set(r.err());
            if let Ok(i) = desktop::refresh().await {
                info.set(Some(i));
            }
        });
    };
    let tray = move || info.get().is_some_and(|i| i.tray);
    view! {
        <h1>"In the background"</h1>
        <p>"notekpr can keep your notes in sync while its window is closed, and from the moment you log in."</p>
        {move || err.get().map(|e| view! { <div class="alert error">{e}</div> })}
        <label class="check">
            <input type="checkbox" class="autostart" prop:checked=move || info.get().is_some_and(|i| i.autostart)
                on:change=move |ev| set("autostart", event_target_checked(&ev))/>
            " Start notekpr when you log in"
        </label>
        <p class="muted small">"In the tray, without a window."</p>
        <label class="check">
            <input type="checkbox" class="close-to-tray" prop:checked=move || info.get().is_some_and(|i| i.close_to_tray && i.tray)
                disabled=move || !tray()
                on:change=move |ev| set("tray", event_target_checked(&ev))/>
            " Close to the tray"
        </label>
        <p class="muted small">{move || if tray() {
            "Closing the window keeps notekpr running in the tray. Quit from the tray's menu."
        } else {
            "This desktop shows no tray icons, so closing the window quits notekpr."
        }}</p>
        <button class="btn primary big setup-done" on:click=move |_| next()>"Done"</button>
    }
}

/// *Settings → Desktop app*: the setup again, all of it.
pub fn again() {
    ui().phase.set(Phase::DesktopSetup { again: true });
}
