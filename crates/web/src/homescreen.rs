//! Adding notekpr to a phone's home screen. The sign-in screen offers it
//! (`views::auth`), before signing in, so that one signs in from the new
//! icon. On an iPhone or iPad that matters: the app on the home screen
//! keeps its own storage, apart from the browser's, so signing in in Safari
//! first would make it a second device to approve.
//!
//! Android's browsers (Chrome, Edge, Samsung Internet) say when the page
//! can go on the home screen, with a `beforeinstallprompt` event that
//! boot.js keeps in `window.notekprHomeScreen`; [`open_dialog`] opens the
//! browser's own dialog with it. No page can add itself on an iPhone or
//! iPad, or in Android's other browsers (Firefox): the offer shows the steps
//! instead.

use std::future::Future;

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::{storage, util};

/// A phone or tablet, by the steps for adding notekpr to its home screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phone {
    /// An iPhone or iPad: Share, then Add to Home Screen.
    Apple,
    /// Android: the browser's dialog, or its menu.
    Android,
}

/// The phone notekpr runs on, when the sign-in screen should offer to add
/// it to the home screen: never on a computer, nor when notekpr already
/// runs from the home screen, nor once the offer was put away in this
/// browser.
pub fn offer() -> Option<Phone> {
    if storage::home_screen_offer_hidden() || from_home_screen() {
        return None;
    }
    phone()
}

fn phone() -> Option<Phone> {
    let nav = util::window().navigator();
    let ua = nav.user_agent().unwrap_or_default();
    if ua.contains("iPhone")
        || ua.contains("iPad")
        || ua.contains("iPod")
        // An iPad's Safari says it is a Mac; a Mac has no touch screen.
        || (ua.contains("Macintosh") && nav.max_touch_points() > 1)
    {
        Some(Phone::Apple)
    } else if ua.contains("Android") {
        Some(Phone::Android)
    } else {
        None
    }
}

/// notekpr opened from its icon on the home screen, in a window of its own.
fn from_home_screen() -> bool {
    let w = util::window();
    w.match_media("(display-mode: standalone)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
        // Safari's own flag, on an iPhone.
        || js_sys::Reflect::get(&w.navigator(), &"standalone".into())
            .ok()
            .and_then(|v| v.as_bool())
            == Some(true)
}

/// What the person chose in the browser's dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Added,
    NotAdded,
    /// The dialog didn't open: show the steps instead.
    Failed,
}

/// Opens the browser's dialog for adding notekpr to the home screen, when
/// the browser offered one (Android's, never an iPhone's), and gives what
/// the person chose there once they have. Call it while handling a tap: a
/// dialog opens only for one. `None`: no dialog to open. Each offer opens
/// one dialog; the browser makes another on a later visit.
pub fn open_dialog() -> Option<impl Future<Output = Choice>> {
    let w = util::window();
    let kept = JsValue::from_str("notekprHomeScreen");
    let event = js_sys::Reflect::get(&w, &kept)
        .ok()
        .filter(|e| e.is_object())?;
    let _ = js_sys::Reflect::set(&w, &kept, &JsValue::NULL);
    let prompt = js_sys::Reflect::get(&event, &"prompt".into())
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    let opened = prompt.call0(&event);
    Some(async move {
        // Today's browsers answer with a promise of the choice; it fails
        // when the dialog can't open (then the event's own promise, from
        // older ones, would never settle).
        let answer = match opened {
            Ok(p) if p.is_instance_of::<js_sys::Promise>() => {
                match JsFuture::from(p.unchecked_into::<js_sys::Promise>()).await {
                    Ok(a) => a,
                    Err(_) => return Choice::Failed,
                }
            }
            Ok(_) => JsValue::UNDEFINED,
            Err(_) => return Choice::Failed,
        };
        let answer = if outcome(&answer).is_some() {
            answer
        } else {
            let Some(choice) = js_sys::Reflect::get(&event, &"userChoice".into())
                .ok()
                .and_then(|c| c.dyn_into::<js_sys::Promise>().ok())
            else {
                return Choice::Failed;
            };
            match JsFuture::from(choice).await {
                Ok(a) => a,
                Err(_) => return Choice::Failed,
            }
        };
        match outcome(&answer).as_deref() {
            Some("accepted") => Choice::Added,
            Some(_) => Choice::NotAdded,
            None => Choice::Failed,
        }
    })
}

fn outcome(answer: &JsValue) -> Option<String> {
    js_sys::Reflect::get(answer, &"outcome".into())
        .ok()?
        .as_string()
}
