//! JSON-over-fetch client for the server REST API.

use std::cell::RefCell;

use notekpr_core::protocol::{APP_HEADER, ApiError, ERROR_UPDATE_REQUIRED, app_id};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

use crate::util::window;

pub const BASE: &str = "/api/v1";

/// What this app says it is in [`APP_HEADER`] and the WebSocket's `auth`
/// message.
pub fn app() -> String {
    app_id("web")
}

#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl Error {
    fn network(e: JsValue) -> Self {
        let message = e
            .as_string()
            .unwrap_or_else(|| "network error — are you offline?".into());
        Error {
            status: 0,
            code: "network".into(),
            message,
        }
    }

    pub fn is_offline(&self) -> bool {
        self.status == 0
    }

    /// The server can't be used right now: no network, or a proxy answering
    /// for a server that is down.
    pub fn is_unreachable(&self) -> bool {
        self.status == 0 || matches!(self.status, 502..=504)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

thread_local! {
    static TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
    static ON_UNAUTHORIZED: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    static ON_OUTDATED: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

pub fn set_token(t: Option<String>) {
    TOKEN.with(|x| *x.borrow_mut() = t);
}

pub fn token() -> Option<String> {
    TOKEN.with(|x| x.borrow().clone())
}

/// Called whenever a request fails with 401 (session expired or revoked).
pub fn on_unauthorized(f: impl Fn() + 'static) {
    ON_UNAUTHORIZED.with(|x| *x.borrow_mut() = Some(Box::new(f)));
}

/// The server no longer knows our session (a request failed with 401, or
/// the WebSocket refused it).
pub fn unauthorized() {
    if token().is_some() {
        ON_UNAUTHORIZED.with(|x| {
            if let Some(f) = &*x.borrow() {
                f()
            }
        });
    }
}

/// Called whenever the server says it needs a newer version of this page
/// ([`ERROR_UPDATE_REQUIRED`], over REST or the WebSocket).
pub fn on_outdated(f: impl Fn() + 'static) {
    ON_OUTDATED.with(|x| *x.borrow_mut() = Some(Box::new(f)));
}

pub fn outdated() {
    ON_OUTDATED.with(|x| {
        if let Some(f) = &*x.borrow() {
            f()
        }
    });
}

async fn send(
    method: &str,
    path: &str,
    body: Option<JsValue>,
    content_type: Option<&str>,
) -> Result<Response> {
    let opts = RequestInit::new();
    opts.set_method(method);
    let headers = Headers::new().map_err(Error::network)?;
    headers.set(APP_HEADER, &app()).map_err(Error::network)?;
    if let Some(t) = token() {
        headers
            .set("Authorization", &format!("Bearer {t}"))
            .map_err(Error::network)?;
    }
    if let Some(ct) = content_type {
        headers.set("Content-Type", ct).map_err(Error::network)?;
    }
    opts.set_headers(&headers);
    if let Some(b) = &body {
        opts.set_body(b);
    }
    let url = format!("{BASE}{path}");
    let req = Request::new_with_str_and_init(&url, &opts).map_err(Error::network)?;
    let resp: Response = JsFuture::from(window().fetch_with_request(&req))
        .await
        .map_err(Error::network)?
        .dyn_into()
        .map_err(Error::network)?;
    if resp.ok() {
        return Ok(resp);
    }
    let status = resp.status();
    let text = match resp.text() {
        Ok(p) => JsFuture::from(p)
            .await
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    let (code, message) = match serde_json::from_str::<ApiError>(&text) {
        Ok(e) => (e.error, e.message),
        Err(_) => ("http".into(), format!("request failed ({status})")),
    };
    if status == 401 {
        unauthorized();
    }
    if code == ERROR_UPDATE_REQUIRED {
        outdated();
    }
    Err(Error {
        status,
        code,
        message,
    })
}

async fn text_of(resp: Response) -> Result<String> {
    let p = resp.text().map_err(Error::network)?;
    Ok(JsFuture::from(p)
        .await
        .map_err(Error::network)?
        .as_string()
        .unwrap_or_default())
}

async fn call<B: Serialize, T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<&B>,
) -> Result<T> {
    let body = body.map(|b| JsValue::from_str(&serde_json::to_string(b).expect("serializable")));
    let ct = body.as_ref().map(|_| "application/json");
    let resp = send(method, path, body, ct).await?;
    let text = text_of(resp).await?;
    let text = if text.trim().is_empty() {
        "null"
    } else {
        text.as_str()
    };
    serde_json::from_str(text).map_err(|e| Error {
        status: 200,
        code: "decode".into(),
        message: format!("unexpected response: {e}"),
    })
}

pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T> {
    call::<(), T>("GET", path, None).await
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T> {
    call("POST", path, Some(body)).await
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T> {
    call("PUT", path, Some(body)).await
}

pub async fn patch<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> Result<T> {
    call("PATCH", path, Some(body)).await
}

pub async fn delete(path: &str) -> Result<()> {
    call::<(), ()>("DELETE", path, None).await
}

/// `DELETE` with a JSON body (e.g. a membership log entry).
pub async fn delete_with<B: Serialize>(path: &str, body: &B) -> Result<()> {
    call::<B, ()>("DELETE", path, Some(body)).await
}

pub async fn post_empty<T: DeserializeOwned>(path: &str) -> Result<T> {
    call::<(), T>("POST", path, None).await
}

pub async fn get_bytes(path: &str) -> Result<Vec<u8>> {
    let resp = send("GET", path, None, None).await?;
    let buf = JsFuture::from(resp.array_buffer().map_err(Error::network)?)
        .await
        .map_err(Error::network)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// One of the app's own files (`/fonts/…`), not the API: no credentials.
pub async fn app_file(path: &str) -> Result<Vec<u8>> {
    let resp: Response = JsFuture::from(window().fetch_with_str(path))
        .await
        .map_err(Error::network)?
        .dyn_into()
        .map_err(Error::network)?;
    if !resp.ok() {
        return Err(Error {
            status: resp.status(),
            code: "http".into(),
            message: format!("{path}: {}", resp.status()),
        });
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(Error::network)?)
        .await
        .map_err(Error::network)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

pub async fn put_bytes(path: &str, data: &[u8]) -> Result<()> {
    let arr = js_sys::Uint8Array::from(data);
    send(
        "PUT",
        path,
        Some(arr.into()),
        Some("application/octet-stream"),
    )
    .await?;
    Ok(())
}

/// Encode a value for use in a query string.
pub fn q(s: &str) -> String {
    js_sys::encode_uri_component(s).into()
}
