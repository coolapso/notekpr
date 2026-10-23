//! Encrypted per-user preferences (synced) and local UI preferences.

use super::*;

// ------------------------------------------------------------------ prefs

/// Choices made here and not saved yet: on a merge with another device's
/// preferences, these win.
#[derive(Clone, Debug, Default)]
pub(super) struct Unsaved {
    pub quick_vault: bool,
    pub default_width: bool,
    /// Notes whose own width was set or removed.
    pub widths: BTreeSet<String>,
}

thread_local! {
    static UNSAVED: RefCell<Unsaved> = RefCell::default();
}

/// Choose the vault quick notes go to (`None`: the default).
pub fn set_quick_vault(c: &mut Core, vault: Option<String>) {
    c.prefs.quick_vault = vault;
    UNSAVED.with_borrow_mut(|u| u.quick_vault = true);
    save_prefs(c);
}

// ------------------------------------------------------------------ note width

/// How wide notes are by default: the same on all devices if chosen so,
/// else `local`, this browser's (`UiPrefs::note_width`).
pub fn default_width(c: &Core, local: NoteWidth) -> NoteWidth {
    c.prefs.note_width.unwrap_or(local)
}

/// The width `note` was given (`None`: it uses the default).
pub fn own_width(c: &Core, note: &str) -> Option<NoteWidth> {
    crate::width::own(&c.prefs.note_widths, note)
}

/// How wide `note` is shown (`local` as in [`default_width`]).
pub fn note_width(c: &Core, local: NoteWidth, note: &str) -> NoteWidth {
    own_width(c, note).unwrap_or_else(|| default_width(c, local))
}

/// Give `note` a width of its own (synced, only for us), or `None` to use
/// the default again.
pub fn set_note_width(c: &mut Core, note: &str, width: Option<NoteWidth>) {
    if own_width(c, note) == width {
        return;
    }
    crate::width::set_own(&mut c.prefs.note_widths, note, width);
    UNSAVED.with_borrow_mut(|u| u.widths.insert(note.to_string()));
    save_prefs(c);
}

/// Change the default width: this browser's, and all devices' if they share it.
pub fn set_default_width(c: &mut Core, width: NoteWidth) {
    set_ui_prefs(|p| p.note_width = width);
    if c.prefs.note_width.is_some_and(|w| w != width) {
        c.prefs.note_width = Some(width);
        UNSAVED.with_borrow_mut(|u| u.default_width = true);
        save_prefs(c);
    }
}

/// Use the same default width on all devices (this browser's, from now on),
/// or let each browser have its own again (this one keeps the shared one).
pub fn share_default_width(c: &mut Core, on: bool) {
    let local = ui().ui_prefs.with_untracked(|p| p.note_width);
    if on == c.prefs.note_width.is_some() {
        return;
    }
    if on {
        c.prefs.note_width = Some(local);
    } else if let Some(w) = c.prefs.note_width.take() {
        set_ui_prefs(|p| p.note_width = w);
    }
    UNSAVED.with_borrow_mut(|u| u.default_width = true);
    save_prefs(c);
}

pub(super) async fn load_prefs() {
    let Ok(blob) = api::get::<PrefsBlob>("/me/prefs").await else {
        return;
    };
    step(|c| {
        offline::note(c, |b| b.prefs = Some(blob.clone()));
        apply_prefs(c, &blob);
    });
}

pub(super) fn apply_prefs(c: &mut Core, blob: &PrefsBlob) {
    let uid = c.user_id.clone();
    let local = storage::prefs_copy(&uid);
    let open = |c: &Core, ct: &[u8]| {
        let pt = c.account.as_ref()?.open_private(&c.user_id, ct).ok()?;
        serde_json::from_slice::<Prefs>(&pt).ok()
    };
    let server = blob.ciphertext.as_deref().and_then(|ct| open(c, ct));
    let mine = local.as_deref().and_then(|ct| open(c, ct));
    let choice = choose_prefs(server, mine);
    c.prefs_version = blob.version;
    if let Some(p) = choice.prefs {
        c.prefs = p;
    }
    if choice.from_server
        && let Some(ct) = &blob.ciphertext
    {
        storage::set_prefs_copy(&uid, ct);
    }
    if choice.rolled_back {
        util::warn("the server sent settings older than this browser's; keeping these");
        save_prefs(c);
    }
    ui().prefs_rev.update(|v| *v += 1);
}

/// Which preferences to use: the server's, or this browser's copy.
#[derive(Debug, PartialEq)]
pub(super) struct PrefsChoice {
    pub prefs: Option<Prefs>,
    pub from_server: bool,
    /// The server's copy is older than this browser's: it was not used.
    pub rolled_back: bool,
}

/// The newer copy wins (by the counter inside the encryption): the server
/// can't hand out an older one, say without a pin, in place of what this
/// browser saw [W-M10].
pub(super) fn choose_prefs(server: Option<Prefs>, mine: Option<Prefs>) -> PrefsChoice {
    match (server, mine) {
        (Some(s), Some(m)) if s.counter < m.counter => PrefsChoice {
            prefs: Some(m),
            from_server: false,
            rolled_back: true,
        },
        (Some(s), _) => PrefsChoice {
            prefs: Some(s),
            from_server: true,
            rolled_back: false,
        },
        (None, m) => PrefsChoice {
            prefs: m,
            from_server: false,
            rolled_back: false,
        },
    }
}

/// Save the preferences soon (changes come in bursts).
pub fn save_prefs(c: &Core) {
    ui().prefs_rev.update(|v| *v += 1);
    c.prefs_save.run(1500, || {
        leptos::task::spawn_local(async {
            for _ in 0..3 {
                let req = step(|c| {
                    let acct = c.account.as_ref()?;
                    c.prefs.counter += 1;
                    let pt = serde_json::to_vec(&c.prefs).ok()?;
                    Some(PutPrefsRequest {
                        ciphertext: acct.seal_private(&c.user_id, &pt),
                        expected_version: c.prefs_version,
                    })
                });
                let Some(req) = req else { return };
                match api::put::<_, PrefsBlob>("/me/prefs", &req).await {
                    Ok(b) => {
                        let uid = step(|c| {
                            c.prefs_version = b.version;
                            c.user_id.clone()
                        });
                        storage::set_prefs_copy(&uid, &req.ciphertext);
                        UNSAVED.take();
                        return;
                    }
                    Err(e) if e.status == 409 => {
                        // Changed on another device: merge and retry.
                        let mine = step(|c| c.prefs.clone());
                        load_prefs().await;
                        let unsaved = UNSAVED.with_borrow(Unsaved::clone);
                        step(|c| merge_prefs(&mut c.prefs, mine, &unsaved));
                    }
                    Err(_) => return,
                }
            }
        });
    });
}

/// Merge our preferences into `theirs`, saved meanwhile on another device:
/// lists are joined, and what was changed here and not saved yet wins.
pub(super) fn merge_prefs(theirs: &mut Prefs, mine: Prefs, unsaved: &Unsaved) {
    for f in mine.favorites {
        if !theirs.favorites.contains(&f) {
            theirs.favorites.push(f);
        }
    }
    for (u, fp) in mine.pins {
        theirs.pins.entry(u).or_insert(fp);
    }
    theirs.counter = theirs.counter.max(mine.counter);
    let mut recent = mine.recent;
    for r in theirs.recent.drain(..) {
        if !recent.contains(&r) {
            recent.push(r);
        }
    }
    recent.truncate(30);
    theirs.recent = recent;
    if unsaved.quick_vault {
        theirs.quick_vault = mine.quick_vault;
    }
    if unsaved.default_width {
        theirs.note_width = mine.note_width;
    }
    // Ours go first, as the latest choices; removed ones stay removed.
    let changed = |n: &String| unsaved.widths.contains(n);
    theirs.note_widths.retain(|(n, _)| !changed(n));
    let ours = mine.note_widths.into_iter().filter(|(n, _)| changed(n));
    theirs.note_widths.splice(0..0, ours);
    theirs.note_widths.truncate(crate::width::MAX_OWN);
}

pub fn set_ui_prefs(f: impl FnOnce(&mut UiPrefs)) {
    let ui = ui();
    ui.ui_prefs.update(|p| {
        f(p);
        storage::set_ui_prefs(p);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(counter: u64, pin: Option<&str>) -> Prefs {
        let mut p = Prefs {
            counter,
            ..Default::default()
        };
        if let Some(fp) = pin {
            p.pins.insert("bob".into(), fp.into());
        }
        p
    }

    #[test]
    fn an_older_copy_from_the_server_is_not_used() {
        // The server replays settings from before Bob's key was pinned.
        let c = choose_prefs(Some(prefs(3, None)), Some(prefs(5, Some("AAAA"))));
        assert!(c.rolled_back && !c.from_server);
        assert_eq!(c.prefs.unwrap().pins["bob"], "AAAA");
        // Newer from another device: used.
        let c = choose_prefs(Some(prefs(6, Some("AAAA"))), Some(prefs(5, None)));
        assert!(c.from_server && !c.rolled_back);
        // A fresh browser has nothing to compare with.
        let c = choose_prefs(Some(prefs(1, None)), None);
        assert!(c.from_server);
        // Nothing on the server (yet): keep ours.
        let c = choose_prefs(None, Some(prefs(2, Some("AAAA"))));
        assert!(!c.from_server && !c.rolled_back && c.prefs.is_some());
    }

    #[test]
    fn a_merge_keeps_the_widths_chosen_here() {
        let w = NoteWidth::new;
        // Another device saved meanwhile: its own widths, a shared default.
        let mut theirs = Prefs {
            note_width: Some(w(30.0)),
            note_widths: vec![
                ("x".into(), w(20.0)),
                ("a".into(), w(90.0)),
                ("b".into(), w(10.0)),
            ],
            ..Default::default()
        };
        // Here: "a" was made wide, "b" uses the default again, "c" is new.
        let mine = Prefs {
            note_widths: vec![
                ("c".into(), w(60.0)),
                ("a".into(), NoteWidth::WIDE),
                ("y".into(), w(5.0)),
            ],
            ..Default::default()
        };
        let unsaved = Unsaved {
            widths: ["a", "b", "c"].map(String::from).into(),
            ..Default::default()
        };
        merge_prefs(&mut theirs, mine.clone(), &unsaved);
        assert_eq!(
            theirs.note_widths,
            [
                ("c".into(), w(60.0)),
                ("a".into(), NoteWidth::WIDE),
                ("x".into(), w(20.0))
            ],
            "ours first, theirs kept; what we didn't change of ours (y) isn't brought back"
        );
        assert_eq!(
            theirs.note_width,
            Some(w(30.0)),
            "their default: not changed here"
        );
        // Turning the shared default off here wins too.
        let unsaved = Unsaved {
            default_width: true,
            ..Default::default()
        };
        merge_prefs(&mut theirs, mine, &unsaved);
        assert_eq!(theirs.note_width, None);
    }
}
