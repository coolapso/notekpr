//! Routes and browser history.

use super::*;

// ------------------------------------------------------------------ navigation

pub fn navigate(route: Route) {
    push_route(route, false);
}

pub fn navigate_replace(route: Route) {
    push_route(route, true);
}

pub(super) fn push_route(route: Route, replace: bool) {
    let ui = ui();
    let path = route.path();
    if let Ok(h) = util::window().history() {
        let _ = if replace {
            h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
        } else {
            h.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
        };
    }
    let uid = step(|c| c.user_id.clone());
    if (route.vault().is_some() || route.shared().is_some()) && !uid.is_empty() {
        storage::set_last_location(&uid, &path);
    }
    if let Route::Note(v, n) = &route {
        step(|c| {
            let r = (v.clone(), n.clone());
            c.prefs.recent.retain(|x| x != &r);
            c.prefs.recent.insert(0, r);
            c.prefs.recent.truncate(30);
        });
        save_prefs();
    }
    ui.sidebar.set(false);
    ui.route.set(route);
    send_presence();
    publish_presence();
}

pub fn listen_popstate() {
    use wasm_bindgen::JsCast;
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(|| {
        let ui = ui();
        ui.sidebar.set(false);
        ui.route.set(current_route());
        // Collaborators see which note we are on.
        send_presence();
        publish_presence();
    }) as Box<dyn FnMut()>);
    let _ =
        util::window().add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    cb.forget();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(|e: web_sys::Event| {
        flush_now();
        if step(|c| c.pending_count()) > 0
            && step(|c| c.socket.as_ref().is_some_and(|s| !s.is_ready()))
        {
            // Unsent changes are kept in local storage; warn anyway.
            e.prevent_default();
            let _ = js_sys::Reflect::set(&e, &"returnValue".into(), &"".into());
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = util::window()
        .add_event_listener_with_callback("beforeunload", cb.as_ref().unchecked_ref());
    cb.forget();
}
