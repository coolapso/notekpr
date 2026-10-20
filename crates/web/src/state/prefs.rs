//! Encrypted per-user preferences (synced) and local UI preferences.

use super::*;

// ------------------------------------------------------------------ prefs

thread_local! {
    /// The quick-notes vault was changed here and not saved yet: on a merge
    /// with another device's preferences, this choice wins.
    static QUICK_VAULT_CHANGED: Cell<bool> = const { Cell::new(false) };
}

/// Choose the vault quick notes go to (`None`: the default).
pub fn set_quick_vault(vault: Option<String>) {
    step(|c| c.prefs.quick_vault = vault);
    QUICK_VAULT_CHANGED.with(|f| f.set(true));
    save_prefs();
}

pub(super) async fn load_prefs() {
    let Ok(blob) = api::get::<PrefsBlob>("/me/prefs").await else {
        return;
    };
    offline::note(|b| b.prefs = Some(blob.clone()));
    apply_prefs(&blob);
}

pub(super) fn apply_prefs(blob: &PrefsBlob) {
    let uid = step(|c| c.user_id.clone());
    let local = storage::prefs_copy(&uid);
    let open = |ct: &[u8]| {
        step(|c| {
            let pt = c.account.as_ref()?.open_private(&c.user_id, ct).ok()?;
            serde_json::from_slice::<Prefs>(&pt).ok()
        })
    };
    let server = blob.ciphertext.as_deref().and_then(open);
    let mine = local.as_deref().and_then(open);
    let choice = choose_prefs(server, mine);
    let (use_server, rolled_back) = (choice.from_server, choice.rolled_back);
    step(|c| {
        c.prefs_version = blob.version;
        if let Some(p) = choice.prefs {
            c.prefs = p;
        }
    });
    if use_server && let Some(ct) = &blob.ciphertext {
        storage::set_prefs_copy(&uid, ct);
    }
    if rolled_back {
        util::warn("the server sent settings older than this browser's; keeping these");
        save_prefs();
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

pub fn save_prefs() {
    ui().prefs_rev.update(|v| *v += 1);
    let d = step(|c| c.prefs_save.clone());
    d.run(1500, || {
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
                        QUICK_VAULT_CHANGED.with(|f| f.set(false));
                        return;
                    }
                    Err(e) if e.status == 409 => {
                        // Changed on another device: merge and retry.
                        let mine = step(|c| c.prefs.clone());
                        load_prefs().await;
                        step(|c| {
                            for f in mine.favorites {
                                if !c.prefs.favorites.contains(&f) {
                                    c.prefs.favorites.push(f);
                                }
                            }
                            for (u, fp) in mine.pins {
                                c.prefs.pins.entry(u).or_insert(fp);
                            }
                            c.prefs.counter = c.prefs.counter.max(mine.counter);
                            let mut recent = mine.recent;
                            for r in c.prefs.recent.drain(..) {
                                if !recent.contains(&r) {
                                    recent.push(r);
                                }
                            }
                            recent.truncate(30);
                            c.prefs.recent = recent;
                            if QUICK_VAULT_CHANGED.with(|f| f.get()) {
                                c.prefs.quick_vault = mine.quick_vault;
                            }
                        });
                    }
                    Err(_) => return,
                }
            }
        });
    });
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
}
