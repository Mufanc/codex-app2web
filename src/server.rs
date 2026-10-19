use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{
    close_code, CloseFrame, Message as BrowserMessage, WebSocket, WebSocketUpgrade,
};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use axum_extra::extract::cookie::Key;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, Mutex};

use crate::cdp::{CdpInput, CdpSession, Frame};
use crate::input::{input_command, Input};
use crate::webrtc_video;
use crate::webrtc_video::VideoSignal;

mod session_cookie;

const SIGNAL_QUEUE_CAPACITY: usize = 128;
const MAX_CLIENT_MESSAGE_BYTES: usize = 132 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum ClientMessage {
    Video(VideoSignal),
    Input(Input),
}

static FRONTEND: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/frontend/dist");

#[derive(Clone)]
pub(crate) struct AppState {
    cookie_key: Key,
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

//noinspection HttpUrlsUsage
pub(crate) async fn serve(
    cdp: CdpSession,
    listen: Option<SocketAddr>,
    unix_socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    let CdpSession {
        frames,
        cursor,
        commands,
        ready,
    } = cdp;
    let (controller_changed, _) = watch::channel(None);
    let (video_changed, _) = watch::channel(0);
    let state = AppState {
        cookie_key: Key::generate(),
        frames,
        cursor,
        commands,
        ready,
        controller: Arc::new(Mutex::new(None)),
        controller_changed,
        video_changed,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/ws", get(ws))
        .with_state(state);

    if let Some(path) = unix_socket {
        let listener = tokio::net::UnixListener::bind(&path)?;

        println!("CodexApp2Web: unix:{}", path.display());
        axum::serve(listener, app).await?;
    } else {
        let address = listen.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));
        let listener = tokio::net::TcpListener::bind(address).await?;

        println!("CodexApp2Web: http://{}", listener.local_addr()?);
        axum::serve(listener, app).await?;
    }

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

async fn index(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let jar = session_cookie::issue(&headers, state.cookie_key);

    (
        jar,
        [(header::CACHE_CONTROL, "no-store")],
        asset(Uri::from_static("/")).await,
    )
}

async fn ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(session): Query<Session>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    if !same_origin(&headers) || !session_cookie::valid(&headers, state.cookie_key.clone()) {
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

    Ok(ws
        .max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            viewer(socket, state, session.id, session.takeover == Some(true))
        }))
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Uri>().ok())
    else {
        return false;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    matches!(origin.scheme_str(), Some("http" | "https"))
        && origin
            .authority()
            .is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
        && origin.path() == "/"
        && origin.query().is_none()
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

    let (signals_tx, mut signals_rx) = broadcast::channel::<Value>(SIGNAL_QUEUE_CAPACITY);
    let mut video_session = None;
    let mut frames = state.frames.clone();
    let mut cursor = state.cursor.clone();
    let initial_frame = { frames.borrow_and_update().clone() };
    let mut last_size = initial_frame
        .as_ref()
        .map(|frame| (frame.width, frame.height));

    if let Some(frame) = initial_frame {
        let _ = send_size(&mut socket, frame).await;
    }

    let point = cursor.borrow_and_update().clone();
    let _ = socket
        .send(BrowserMessage::Text(
            json!({"type":"cursor","point":point}).to_string().into(),
        ))
        .await;

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
                    let size = (frame.width, frame.height);
                    if last_size != Some(size) {
                        if send_size(&mut socket, frame).await.is_err() { break; }
                        last_size = Some(size);
                    }
                }
            }
            result = cursor.changed() => {
                if result.is_err() { break; }
                let point = cursor.borrow_and_update().clone();
                if socket.send(BrowserMessage::Text(json!({"type":"cursor","point":point}).to_string().into())).await.is_err() { break; }
            }
            signal = signals_rx.recv() => {
                let Ok(signal) = signal else { break; };
                if socket.send(BrowserMessage::Text(signal.to_string().into())).await.is_err() { break; }
            }
            message = socket.recv() => {
                if state.controller.lock().await.as_deref() != Some(id.as_str()) { break; }
                let Some(Ok(BrowserMessage::Text(text))) = message else { break; };
                let Ok(message) = serde_json::from_str::<ClientMessage>(&text) else { continue; };

                match message {
                    ClientMessage::Input(input) => {
                        if let Some(command) = input_command(input) {
                            if state.commands.send(command).await.is_err() { break; }
                        }
                    }
                    ClientMessage::Video(message) => {
                        match webrtc_video::signal(&state, &id, &mut video_session, message, &signals_tx).await {
                            Ok(Some(answer)) => {
                                if socket.send(BrowserMessage::Text(answer.to_string().into())).await.is_err() { break; }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                eprintln!("WebRTC signaling failed: {error:#}");
                                let _ = socket.send(BrowserMessage::Text(json!({
                                    "type": "video_error", "message": error.to_string(),
                                }).to_string().into())).await;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(session) = video_session {
        session.close().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_must_match_request_host() {
        for (host, origin, expected) in [
            ("127.0.0.1:8787", "http://127.0.0.1:8787", true),
            ("desktop.example.com", "https://desktop.example.com", true),
            ("[::1]:9000", "http://[::1]:9000", true),
            ("localhost:8787", "http://localhost:9000", false),
            ("desktop.example.com", "https://evil.example", false),
            ("desktop.example.com", "null", false),
            (
                "desktop.example.com",
                "https://desktop.example.com/path",
                false,
            ),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert_eq!(same_origin(&headers), expected, "{origin}");
        }

        assert!(!same_origin(&HeaderMap::new()));
    }

    #[test]
    fn websocket_accepts_trickle_signals_and_existing_input() {
        let offer = json!({
            "type": "offer", "video_id": "test", "width": 1920, "height": 1080, "sdp": "offer",
        });
        assert!(matches!(
            serde_json::from_value::<ClientMessage>(offer).unwrap(),
            ClientMessage::Video(VideoSignal::Offer(_))
        ));

        let candidate = json!({
            "type": "ice", "video_id": "test", "candidate": {
                "candidate": "candidate:test", "sdpMid": "0", "sdpMLineIndex": 0,
                "usernameFragment": "test",
            },
        });
        let ClientMessage::Video(VideoSignal::Ice {
            candidate: Some(candidate),
            ..
        }) = serde_json::from_value::<ClientMessage>(candidate).unwrap()
        else {
            panic!("expected ICE candidate")
        };

        assert_eq!(candidate.sdp_mline_index, Some(0));
        assert_eq!(candidate.username_fragment.as_deref(), Some("test"));
        assert!(matches!(
            serde_json::from_value::<ClientMessage>(json!({
                "type": "ice", "video_id": "test", "candidate": null,
            }))
            .unwrap(),
            ClientMessage::Video(VideoSignal::Ice {
                candidate: None,
                ..
            })
        ));
        assert!(matches!(
            serde_json::from_value::<ClientMessage>(json!({
                "type": "text", "text": "hello",
            }))
            .unwrap(),
            ClientMessage::Input(Input::Text { .. })
        ));
    }
}
