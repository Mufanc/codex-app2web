use std::sync::Arc;

use axum::extract::ws::{
    close_code, CloseFrame, Message as BrowserMessage, WebSocket, WebSocketUpgrade,
};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch, Mutex};

use crate::cdp::{CdpInput, CdpSession, Frame};
use crate::input::{input_command, Input};
use crate::webrtc_video;

static FRONTEND: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/frontend/dist");

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) frames: watch::Receiver<Option<Frame>>,
    pub(crate) cursor: watch::Receiver<Option<Value>>,
    pub(crate) commands: mpsc::Sender<CdpInput>,
    pub(crate) ready: watch::Receiver<bool>,
    pub(crate) controller: Arc<Mutex<Option<String>>>,
    pub(crate) controller_changed: watch::Sender<Option<String>>,
    pub(crate) video_changed: watch::Sender<u64>,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    takeover: Option<bool>,
}

pub(crate) async fn serve(cdp: CdpSession) -> anyhow::Result<()> {
    let CdpSession {
        frames,
        cursor,
        commands,
        ready,
    } = cdp;
    let (controller_changed, _) = watch::channel(None);
    let (video_changed, _) = watch::channel(0);
    let state = AppState {
        frames,
        cursor,
        commands,
        ready,
        controller: Arc::new(Mutex::new(None)),
        controller_changed,
        video_changed,
    };
    let app = Router::new()
        .route("/", get(asset))
        .route("/assets/{*path}", get(asset))
        .route("/offer", post(webrtc_video::offer))
        .route("/ws", get(ws))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8787").await?;

    println!("CodexApp2Web: http://127.0.0.1:8787");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn asset(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(file) = FRONTEND.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");

    ([(header::CONTENT_TYPE, content_type)], file.contents()).into_response()
}

async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(session): Query<Session>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if headers.get("origin").and_then(|value| value.to_str().ok()) != Some("http://127.0.0.1:8787")
    {
        return Err(StatusCode::FORBIDDEN);
    }

    if session.id.len() != 36
        || !session
            .id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok(ws.on_upgrade(move |socket| {
        viewer(socket, state, session.id, session.takeover == Some(true))
    }))
}

async fn viewer(mut socket: WebSocket, state: AppState, id: String, takeover: bool) {
    let mut ready = state.ready.clone();

    if !*ready.borrow_and_update() {
        let _ = socket
            .send(BrowserMessage::Close(Some(CloseFrame {
                code: close_code::POLICY,
                reason: "cdp disconnected".into(),
            })))
            .await;

        return;
    }

    let mut ownership = state.controller_changed.subscribe();
    {
        let mut controller = state.controller.lock().await;

        if controller.is_some() && !takeover {
            let _ = socket
                .send(BrowserMessage::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "occupied".into(),
                })))
                .await;
            return;
        }

        *controller = Some(id.clone());
        state.controller_changed.send_replace(Some(id.clone()));
    }

    let mut frames = state.frames.clone();
    let mut cursor = state.cursor.clone();
    let initial_frame = { frames.borrow_and_update().clone() };

    if let Some(frame) = initial_frame {
        let _ = send_size(&mut socket, frame).await;
    }

    loop {
        tokio::select! {
            result = ready.changed() => {
                if result.is_err() || !*ready.borrow_and_update() {
                    let _ = socket.send(BrowserMessage::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "cdp disconnected".into(),
                    }))).await;

                    break;
                }
            }
            result = ownership.changed() => {
                if result.is_err() { break; }
                if ownership.borrow_and_update().as_deref() != Some(id.as_str()) {
                    let _ = socket.send(BrowserMessage::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "taken over".into(),
                    }))).await;

                    break;
                }
            }
            result = frames.changed() => {
                if result.is_err() { break; }
                let frame = frames.borrow_and_update().clone();
                if let Some(frame) = frame {
                    if send_size(&mut socket, frame).await.is_err() { break; }
                }
            }
            result = cursor.changed() => {
                if result.is_err() { break; }
                let point = cursor.borrow_and_update().clone();
                if socket.send(BrowserMessage::Text(json!({"type":"cursor","point":point}).to_string().into())).await.is_err() { break; }
            }
            message = socket.recv() => {
                if state.controller.lock().await.as_deref() != Some(id.as_str()) { break; }
                let Some(Ok(BrowserMessage::Text(text))) = message else { break; };
                let Ok(input) = serde_json::from_str::<Input>(&text) else { continue; };
                if let Some(command) = input_command(input) {
                    if state.commands.send(command).await.is_err() { break; }
                }
            }
        }
    }

    let mut controller = state.controller.lock().await;

    if controller.as_deref() == Some(id.as_str()) {
        if *ready.borrow() {
            tokio::select! {
                _ = state.commands.send(CdpInput::Stop) => {}
                _ = ready.changed() => {}
            }
        }

        *controller = None;
        state.controller_changed.send_replace(None);
    }
}

async fn send_size(socket: &mut WebSocket, frame: Frame) -> Result<(), axum::Error> {
    socket
        .send(BrowserMessage::Text(
            json!({"type":"size","width":frame.width,"height":frame.height})
                .to_string()
                .into(),
        ))
        .await
}
