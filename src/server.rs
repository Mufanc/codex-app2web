use std::net::SocketAddr;
use std::path::PathBuf;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use axum_extra::extract::cookie::Key;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use tracing::info;

use crate::cdp::CdpSession;
use crate::session;
use crate::session::controller::Controller;

mod session_cookie;

const MAX_CLIENT_MESSAGE_BYTES: usize = 132 * 1024;

static FRONTEND: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/frontend/dist");

#[derive(Clone)]
struct AppState {
    cookie_key: Key,
    cdp: CdpSession,
    controller: Controller,
}

#[derive(Deserialize)]
struct Session {
    takeover: Option<bool>,
}

//noinspection HttpUrlsUsage
pub(crate) async fn serve(
    cdp: CdpSession,
    controller: Controller,
    listen: Option<SocketAddr>,
    unix_socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    let app = router(cdp, controller);

    if let Some(path) = unix_socket {
        let listener = tokio::net::UnixListener::bind(&path)?;

        info!(path = %path.display(), "Listening on Unix socket");
        axum::serve(listener, app).await?;
    } else {
        let address = listen.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));
        let listener = tokio::net::TcpListener::bind(address).await?;

        info!(address = %listener.local_addr()?, "Listening on HTTP");
        axum::serve(listener, app).await?;
    }

    Ok(())
}

fn router(cdp: CdpSession, controller: Controller) -> Router {
    let state = AppState {
        cookie_key: Key::generate(),
        cdp,
        controller,
    };

    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/ws", get(ws))
        .with_state(state)
}

async fn asset(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let Some(file) = FRONTEND.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let content_type = mime_guess::from_path(path).first_raw().unwrap_or("application/octet-stream");

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

    Ok(ws
        .max_message_size(MAX_CLIENT_MESSAGE_BYTES)
        .on_upgrade(move |socket| session::viewer(socket, state.cdp, state.controller, session.takeover == Some(true))))
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Uri>().ok())
    else {
        return false;
    };

    let Some(host) = headers.get(header::HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };

    matches!(origin.scheme_str(), Some("http" | "https"))
        && origin.authority().is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
        && origin.path() == "/"
        && origin.query().is_none()
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
            ("desktop.example.com", "https://desktop.example.com/path", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            assert_eq!(same_origin(&headers), expected, "{origin}");
        }

        assert!(!same_origin(&HeaderMap::new()));
    }
}

#[cfg(test)]
mod session_test;
