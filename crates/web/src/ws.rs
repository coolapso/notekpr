//! Reconnecting WebSocket to `/api/v1/ws`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use coolpsonotes_core::protocol::{ClientMessage, ServerMessage};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{CloseEvent, MessageEvent, WebSocket};

use crate::util::{self, now_ms};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Open,
    Offline,
}

type OnMessage = Rc<dyn Fn(ServerMessage)>;
type OnState = Rc<dyn Fn(ConnState)>;

pub struct Socket {
    token: RefCell<String>,
    ws: RefCell<Option<WebSocket>>,
    state: Cell<ConnState>,
    ready: Cell<bool>,
    attempts: Cell<u32>,
    last_rx: Cell<i64>,
    stopped: Cell<bool>,
    on_message: OnMessage,
    on_state: OnState,
    // Keep JS callbacks alive for the current connection.
    callbacks: RefCell<Vec<Closure<dyn FnMut(JsValue)>>>,
}

fn ws_url() -> String {
    let loc = util::window().location();
    let proto = if loc.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    format!("{proto}://{}/api/v1/ws", loc.host().unwrap_or_default())
}

impl Socket {
    pub fn new(token: String, on_message: OnMessage, on_state: OnState) -> Rc<Socket> {
        let s = Rc::new(Socket {
            token: RefCell::new(token),
            ws: RefCell::new(None),
            state: Cell::new(ConnState::Connecting),
            ready: Cell::new(false),
            attempts: Cell::new(0),
            last_rx: Cell::new(now_ms()),
            stopped: Cell::new(false),
            on_message,
            on_state,
            callbacks: RefCell::new(Vec::new()),
        });
        Socket::connect(&s);
        Socket::watchdog(&s);
        s
    }

    pub fn state(&self) -> ConnState {
        self.state.get()
    }

    /// True once the server accepted our token on the current connection.
    pub fn is_ready(&self) -> bool {
        self.ready.get() && self.state.get() == ConnState::Open
    }

    fn set_state(&self, st: ConnState) {
        if self.state.get() != st {
            self.state.set(st);
            (self.on_state)(st);
        }
    }

    pub fn send(&self, m: &ClientMessage) -> bool {
        let ws = self.ws.borrow();
        let Some(ws) = ws.as_ref() else { return false };
        if ws.ready_state() != WebSocket::OPEN {
            return false;
        }
        ws.send_with_str(&serde_json::to_string(m).expect("serializable"))
            .is_ok()
    }

    pub fn stop(&self) {
        self.stopped.set(true);
        if let Some(ws) = self.ws.borrow_mut().take() {
            let _ = ws.close();
        }
        self.callbacks.borrow_mut().clear();
    }

    /// Force a reconnect (e.g. after coming back online).
    pub fn kick(self: &Rc<Self>) {
        if self.state.get() != ConnState::Open {
            self.attempts.set(0);
            if let Some(ws) = self.ws.borrow_mut().take() {
                let _ = ws.close();
            }
            Socket::connect(self);
        }
    }

    fn connect(this: &Rc<Socket>) {
        if this.stopped.get() {
            return;
        }
        this.ready.set(false);
        // While retrying after a drop, keep showing "offline" until we're back.
        if this.attempts.get() == 0 {
            this.set_state(ConnState::Connecting);
        }
        let ws = match WebSocket::new(&ws_url()) {
            Ok(ws) => ws,
            Err(_) => {
                Socket::retry(this);
                return;
            }
        };
        let mut cbs: Vec<Closure<dyn FnMut(JsValue)>> = Vec::new();

        let me = Rc::downgrade(this);
        let onopen = Closure::wrap(Box::new(move |_e: JsValue| {
            if let Some(s) = me.upgrade() {
                s.attempts.set(0);
                s.last_rx.set(now_ms());
                let token = s.token.borrow().clone();
                if let Some(ws) = s.ws.borrow().as_ref() {
                    let m = ClientMessage::Auth { token };
                    let _ = ws.send_with_str(&serde_json::to_string(&m).unwrap());
                }
            }
        }) as Box<dyn FnMut(JsValue)>);
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        cbs.push(onopen);

        let me = Rc::downgrade(this);
        let onmessage = Closure::wrap(Box::new(move |e: JsValue| {
            let Some(s) = me.upgrade() else { return };
            s.last_rx.set(now_ms());
            let Ok(e) = e.dyn_into::<MessageEvent>() else {
                return;
            };
            let Some(text) = e.data().as_string() else {
                return;
            };
            match serde_json::from_str::<ServerMessage>(&text) {
                Ok(m) => {
                    if let ServerMessage::Ready { .. } = &m {
                        s.ready.set(true);
                        s.set_state(ConnState::Open);
                    }
                    (s.on_message)(m);
                }
                Err(err) => util::warn(&format!("bad server message: {err}")),
            }
        }) as Box<dyn FnMut(JsValue)>);
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        cbs.push(onmessage);

        let me = Rc::downgrade(this);
        let onclose = Closure::wrap(Box::new(move |e: JsValue| {
            let Some(s) = me.upgrade() else { return };
            let _ = e.dyn_into::<CloseEvent>();
            s.ready.set(false);
            s.ws.borrow_mut().take();
            s.set_state(ConnState::Offline);
            Socket::retry(&s);
        }) as Box<dyn FnMut(JsValue)>);
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        cbs.push(onclose);

        *this.ws.borrow_mut() = Some(ws);
        *this.callbacks.borrow_mut() = cbs;
    }

    fn retry(this: &Rc<Socket>) {
        if this.stopped.get() {
            return;
        }
        let n = this.attempts.get() + 1;
        this.attempts.set(n);
        // Exponential backoff with jitter, capped at 30 s.
        let base = (500.0 * 2f64.powi(n.min(6) as i32)).min(30_000.0);
        let delay = (base * (0.5 + js_sys::Math::random() * 0.5)) as i32;
        let me = Rc::downgrade(this);
        util::set_timeout(delay, move || {
            if let Some(s) = me.upgrade()
                && s.ws.borrow().is_none()
            {
                Socket::connect(&s);
            }
        });
    }

    /// Ping regularly; drop connections that have gone silent (half-open TCP).
    fn watchdog(this: &Rc<Socket>) {
        let me = Rc::downgrade(this);
        util::set_interval(20_000, move || {
            let Some(s) = me.upgrade() else { return };
            if s.stopped.get() || s.state.get() != ConnState::Open {
                return;
            }
            if now_ms() - s.last_rx.get() > 65_000 {
                if let Some(ws) = s.ws.borrow().as_ref() {
                    let _ = ws.close();
                }
                return;
            }
            s.send(&ClientMessage::Ping {
                nonce: now_ms() as u64,
            });
        });
    }
}
