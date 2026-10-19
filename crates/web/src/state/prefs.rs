//! Encrypted per-user preferences (synced) and local UI preferences.

use super::*;

// ------------------------------------------------------------------ prefs

pub(super) async fn load_prefs() {
    let Ok(blob) = api::get::<PrefsBlob>("/me/prefs").await else {
        return;
    };
    with(|c| {
        c.prefs_version = blob.version;
        if let (Some(ct), Some(acct)) = (&blob.ciphertext, &c.account)
            && let Ok(pt) = acct.open_private(&c.user_id, ct)
            && let Ok(p) = serde_json::from_slice::<Prefs>(&pt)
        {
            c.prefs = p;
        }
    });
    ui().prefs_rev.update(|v| *v += 1);
}

pub fn save_prefs() {
    ui().prefs_rev.update(|v| *v += 1);
    let d = with(|c| c.prefs_save.clone());
    d.run(1500, || {
        leptos::task::spawn_local(async {
            for _ in 0..3 {
                let req = with(|c| {
                    let acct = c.account.as_ref()?;
                    let pt = serde_json::to_vec(&c.prefs).ok()?;
                    Some(PutPrefsRequest {
                        ciphertext: acct.seal_private(&c.user_id, &pt),
                        expected_version: c.prefs_version,
                    })
                });
                let Some(req) = req else { return };
                match api::put::<_, PrefsBlob>("/me/prefs", &req).await {
                    Ok(b) => {
                        with(|c| c.prefs_version = b.version);
                        return;
                    }
                    Err(e) if e.status == 409 => {
                        // Changed on another device: merge and retry.
                        let mine = with(|c| c.prefs.clone());
                        load_prefs().await;
                        with(|c| {
                            for f in mine.favorites {
                                if !c.prefs.favorites.contains(&f) {
                                    c.prefs.favorites.push(f);
                                }
                            }
                            for (u, fp) in mine.pins {
                                c.prefs.pins.entry(u).or_insert(fp);
                            }
                            let mut recent = mine.recent;
                            for r in c.prefs.recent.drain(..) {
                                if !recent.contains(&r) {
                                    recent.push(r);
                                }
                            }
                            recent.truncate(30);
                            c.prefs.recent = recent;
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
