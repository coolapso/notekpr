//! The load test's live dashboard: a canvas that draws what the run streams
//! over its WebSocket (`/events`, see `notekpr-load-events`). Rust
//! compiled to WebAssembly, no JavaScript packages.

pub mod state;
pub mod wrap;

#[cfg(target_arch = "wasm32")]
mod draw;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::cell::RefCell;
    use std::rc::Rc;

    use notekpr_load_events::{Command, Event};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use web_sys::{
        CanvasRenderingContext2d, HtmlCanvasElement, KeyboardEvent, MessageEvent, MouseEvent,
        WebSocket,
    };

    use crate::draw;
    use crate::state::{App, Input};

    /// The connection to the load test, for sending it commands.
    type Socket = Rc<RefCell<Option<WebSocket>>>;

    fn send(socket: &Socket, c: &Command) {
        if let (Some(ws), Ok(json)) = (socket.borrow().as_ref(), serde_json::to_string(c)) {
            let _ = ws.send_with_str(&json);
        }
    }

    /// The function drawing each frame (it asks for the next one itself).
    type FrameCallback = Closure<dyn FnMut(f64)>;

    fn now() -> f64 {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0)
    }

    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        let window = web_sys::window().ok_or("no window")?;
        let document = window.document().ok_or("no document")?;
        let canvas: HtmlCanvasElement = document
            .get_element_by_id("view")
            .ok_or("no canvas")?
            .dyn_into()?;
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or("no 2d context")?
            .dyn_into()?;
        let app = Rc::new(RefCell::new(App::new()));
        let socket: Socket = Rc::default();
        connect(app.clone(), socket.clone());

        // Keys (the start screen, insert coin) and clicks.
        let (a, s) = (app.clone(), socket.clone());
        let on_key = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
            let input = match e.key().as_str() {
                "ArrowUp" => Input::Up,
                "ArrowDown" => Input::Down,
                "ArrowLeft" => Input::Left,
                "ArrowRight" => Input::Right,
                "Enter" | " " => Input::Enter,
                _ => return,
            };
            e.prevent_default();
            let cmd = a.borrow_mut().input(input);
            if let Some(c) = cmd {
                send(&s, &c);
            }
        });
        window.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref())?;
        on_key.forget();
        let (a, s) = (app.clone(), socket.clone());
        let on_click = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
            let input = Input::Click(e.offset_x() as f64, e.offset_y() as f64);
            let cmd = a.borrow_mut().input(input);
            if let Some(c) = cmd {
                send(&s, &c);
            }
        });
        canvas.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
        on_click.forget();

        // Draw every frame, at the screen's resolution.
        let frame: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
        let again = frame.clone();
        let mut size = (0.0, 0.0, 0.0);
        *again.borrow_mut() = Some(Closure::new(move |_: f64| {
            let window = web_sys::window().expect("window");
            let dpr = window.device_pixel_ratio().max(1.0);
            let (w, h) = (canvas.client_width() as f64, canvas.client_height() as f64);
            if (w, h, dpr) != size {
                size = (w, h, dpr);
                canvas.set_width((w * dpr) as u32);
                canvas.set_height((h * dpr) as u32);
                app.borrow_mut().layout_dirty = true;
            }
            let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
            draw::frame(&mut app.borrow_mut(), &ctx, w, h, now());
            if let Some(f) = frame.borrow().as_ref() {
                let _ = window.request_animation_frame(f.as_ref().unchecked_ref());
            }
        }));
        if let Some(f) = again.borrow().as_ref() {
            window.request_animation_frame(f.as_ref().unchecked_ref())?;
        }
        Ok(())
    }

    /// Follow the run; after a lost connection, try again every second.
    fn connect(app: Rc<RefCell<App>>, socket: Socket) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let loc = window.location();
        let scheme = if loc.protocol().as_deref() == Ok("https:") {
            "wss"
        } else {
            "ws"
        };
        let url = format!("{scheme}://{}/events", loc.host().unwrap_or_default());
        let Ok(ws) = WebSocket::new(&url) else {
            retry(app, socket);
            return;
        };
        *socket.borrow_mut() = Some(ws.clone());
        let a = app.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let Some(text) = e.data().as_string() else {
                return;
            };
            match serde_json::from_str::<Vec<Event>>(&text) {
                Ok(events) => {
                    let t = now();
                    let mut app = a.borrow_mut();
                    for ev in events {
                        app.apply(ev, t);
                    }
                }
                Err(err) => web_sys::console::warn_1(&format!("bad events: {err}").into()),
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();
        let a = app.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || a.borrow_mut().connected = true);
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
        let a = app;
        let on_close = Closure::<dyn FnMut()>::new(move || {
            a.borrow_mut().connected = false;
            retry(a.clone(), socket.clone());
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();
    }

    fn retry(app: Rc<RefCell<App>>, socket: Socket) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let cb = Closure::once_into_js(move || connect(app, socket));
        let _ =
            window.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 1000);
    }
}
