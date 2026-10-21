//! Serves the live dashboard (`crates/load-ui`, built into `dist/load`) and
//! streams the run to it over a WebSocket.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tower_http::services::ServeDir;

use coolpsonotes_load_events::Command;
use tokio::sync::mpsc;

use crate::bus::Bus;

/// Serve the dashboard; what it asks for (start a run, back to the start
/// screen) goes to `commands`.
pub async fn serve(
    addr: SocketAddr,
    dir: PathBuf,
    bus: Bus,
    commands: mpsc::UnboundedSender<Command>,
) -> Result<()> {
    if !dir.join("index.html").exists() {
        eprintln!(
            "warning: no dashboard in {} — build it with ./scripts/build-load-ui.sh (`task load` does)",
            dir.display()
        );
    }
    let app = Router::new()
        .route("/events", get(events))
        .fallback_service(ServeDir::new(dir))
        .with_state((bus, commands));
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| {
        format!("listening on {addr} for the dashboard (in use? try --dashboard)")
    })?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(())
}

async fn events(
    ws: WebSocketUpgrade,
    State((bus, commands)): State<(Bus, mpsc::UnboundedSender<Command>)>,
) -> Response {
    ws.on_upgrade(move |socket| stream(socket, bus, commands))
}

async fn stream(socket: WebSocket, bus: Bus, commands: mpsc::UnboundedSender<Command>) {
    let (mut tx, mut rx) = socket.split();
    // Subscribe first, then send what happened so far: nothing falls in
    // between (the page ignores what it already has).
    let mut live = bus.subscribe();
    if tx.send(Message::Text(bus.snapshot().into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            m = live.recv() => match m {
                Ok(batch) => {
                    if tx.send(Message::Text((*batch).clone().into())).await.is_err() {
                        return;
                    }
                }
                // Too slow to keep up: start over from the current state.
                Err(RecvError::Lagged(_)) => {
                    if tx.send(Message::Text(bus.snapshot().into())).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            m = rx.next() => match m {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(Message::Text(t))) => {
                    if let Ok(c) = serde_json::from_str::<Command>(&t) {
                        let _ = commands.send(c);
                    }
                }
                _ => {}
            },
        }
    }
}
