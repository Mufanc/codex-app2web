use std::time::{Duration, Instant};

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchMouseEventParams, InsertTextParams,
};
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
    StopScreencastParams,
};
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

const CURSOR_SCRIPT: &str = r#"(() => {
  const el = document.activeElement;
  if (!el) return null;
  const selection = getSelection();
  if (selection?.rangeCount && el.contains(selection.anchorNode)) {
    const range = selection.getRangeAt(0).cloneRange();
    range.collapse(true);
    let rect = range.getBoundingClientRect();
    if (!rect.height && range.startContainer.nodeType === Node.ELEMENT_NODE) {
      rect = range.startContainer.childNodes[range.startOffset]?.getBoundingClientRect?.() || rect;
    }
    if (rect.height) return { x: rect.left, y: rect.bottom };
  }
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    const rect = el.getBoundingClientRect();
    return { x: rect.left + 8, y: rect.bottom };
  }
  return null;
})()"#;

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) jpeg: Vec<u8>,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

pub(crate) enum CdpInput {
    Mouse(DispatchMouseEventParams),
    Key(DispatchKeyEventParams),
    Text(InsertTextParams),
    Resize(u32, u32),
    Stop,
}

pub(crate) struct CdpSession {
    pub(crate) frames: watch::Receiver<Option<Frame>>,
    pub(crate) cursor: watch::Receiver<Option<Value>>,
    pub(crate) commands: mpsc::Sender<CdpInput>,
    pub(crate) ready: watch::Receiver<bool>,
}

pub(crate) fn connect() -> CdpSession {
    let cdp_port = std::env::var("CODEX_CDP_PORT").unwrap_or_else(|_| "9341".into());

    let (commands, command_rx) = mpsc::channel(64);
    let (frame_tx, frames) = watch::channel(None);
    let (cursor_tx, cursor) = watch::channel(None);
    let (ready_tx, ready) = watch::channel(false);

    tokio::spawn(reconnect(
        cdp_port, command_rx, frame_tx, cursor_tx, ready_tx,
    ));

    CdpSession {
        frames,
        cursor,
        commands,
        ready,
    }
}

async fn reconnect(
    cdp_port: String,
    mut commands: mpsc::Receiver<CdpInput>,
    frames: watch::Sender<Option<Frame>>,
    cursor: watch::Sender<Option<Value>>,
    ready: watch::Sender<bool>,
) {
    let mut delay = Duration::from_secs(1);

    loop {
        while commands.try_recv().is_ok() {}

        let result = run_connection(&cdp_port, &mut commands, &frames, &cursor, &ready).await;
        let was_connected = *ready.borrow();

        ready.send_replace(false);
        frames.send_replace(None);
        cursor.send_replace(None);

        if was_connected {
            delay = Duration::from_secs(1);
        }

        eprintln!(
            "CDP disconnected: {}; retrying in {}s",
            result
                .err()
                .map_or_else(|| "session ended".into(), |error| error.to_string()),
            delay.as_secs()
        );

        tokio::time::sleep(delay).await;
        delay = next_retry(delay);
    }
}

fn next_retry(delay: Duration) -> Duration {
    Duration::from_secs((delay.as_secs() * 2).min(30))
}

async fn run_connection(
    cdp_port: &str,
    commands: &mut mpsc::Receiver<CdpInput>,
    frames: &watch::Sender<Option<Frame>>,
    cursor: &watch::Sender<Option<Value>>,
    ready: &watch::Sender<bool>,
) -> anyhow::Result<()> {
    let (mut browser, mut handler) =
        Browser::connect(format!("http://127.0.0.1:{cdp_port}")).await?;

    let mut handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(error) = event {
                eprintln!("CDP handler error: {error}");
                break;
            }
        }
    });

    let result = tokio::select! {
        result = async {
            let targets = browser.fetch_targets().await?;
            let target = targets
                .iter()
                .filter(|target| target.r#type == "page" && target.url.starts_with("app://"))
                .max_by_key(|target| {
                    (
                        target.url == "app://-/index.html",
                        target.url.contains("/index.html"),
                        target.title.contains("Codex"),
                    )
                })
                .ok_or_else(|| anyhow::anyhow!("No Codex app:// page found on the CDP port"))?;
            let target_id = target.target_id.clone();
            let page = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(page) = browser.get_page(target_id.clone()).await {
                        break page;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("Codex page did not attach within 5 seconds"))?;

            run_cdp(page, commands, frames.clone(), cursor.clone(), ready.clone()).await
        } => result,
        _ = &mut handler_task => Err(anyhow::anyhow!("CDP transport closed")),
    };
    handler_task.abort();
    result
}

async fn run_cdp(
    page: Page,
    commands: &mut mpsc::Receiver<CdpInput>,
    frames: watch::Sender<Option<Frame>>,
    cursor: watch::Sender<Option<Value>>,
    ready: watch::Sender<bool>,
) -> anyhow::Result<()> {
    let mut events = page.event_listener::<EventScreencastFrame>().await?;

    ready.send_replace(true);

    let mut capture_size = None;
    let mut viewport_size = None;
    let mut window_size = None;
    let mut window_changed_at: Option<Instant> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let command = EvaluateParams::builder().expression(CURSOR_SCRIPT).return_by_value(true).build().map_err(anyhow::Error::msg)?;
                let point = page.execute(command).await.ok().and_then(|response| response.result.result.value).unwrap_or(Value::Null);
                cursor.send_replace(if point.is_object() { Some(point) } else { None });
            }
            _ = async {
                if let Some(changed_at) = window_changed_at {
                    tokio::time::sleep_until((changed_at + Duration::from_secs(1)).into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                if let (Some(viewport), Some(window)) = (viewport_size, window_size) {
                    let desired = capture_limit(viewport, Some(window));
                    if capture_size != Some(desired) {
                        configure_capture(&page, &frames, &mut capture_size, desired).await?;
                    }
                }
                window_changed_at = None;
            }
            Some(command) = commands.recv() => {
                match command {
                    CdpInput::Mouse(params) => { if let Err(error) = page.execute(params).await { eprintln!("Mouse input failed: {error}"); } }
                    CdpInput::Key(params) => { if let Err(error) = page.execute(params).await { eprintln!("Key input failed: {error}"); } }
                    CdpInput::Text(params) => { if let Err(error) = page.execute(params).await { eprintln!("Text input failed: {error}"); } }
                    CdpInput::Resize(width, height) => {
                        viewport_size = Some((width, height));
                        let desired = capture_limit((width, height), window_size);

                        if capture_size != Some(desired) {
                            configure_capture(&page, &frames, &mut capture_size, desired).await?;
                        }
                    }
                    CdpInput::Stop => {
                        if capture_size.take().is_some() { page.execute(StopScreencastParams::default()).await?; }

                        frames.send_replace(None);
                        viewport_size = None;
                        window_size = None;
                        window_changed_at = None;
                    }
                }
            }
            event = events.next() => {
                let Some(event) = event else { break; };
                let encoded: &str = event.data.as_ref();
                let jpeg = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)?;
                let width = event.metadata.device_width;
                let height = event.metadata.device_height;

                if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                    let native_size = (width.round() as u32, height.round() as u32);

                    if window_size != Some(native_size) {
                        window_size = Some(native_size);
                        window_changed_at = Some(Instant::now());
                    }

                    if frames.borrow().as_ref().is_none_or(|old| old.jpeg != jpeg) {
                        frames.send_replace(Some(Frame { jpeg, width, height }));
                    }
                }

                page.execute(ScreencastFrameAckParams::new(event.session_id)).await?;
            }
        }
    }

    Ok(())
}

fn capture_limit(viewport: (u32, u32), window: Option<(u32, u32)>) -> (u32, u32) {
    match window {
        Some((width, height)) => (viewport.0.min(width), viewport.1.min(height)),
        None => viewport,
    }
}

async fn configure_capture(
    page: &Page,
    frames: &watch::Sender<Option<Frame>>,
    capture_size: &mut Option<(u32, u32)>,
    desired: (u32, u32),
) -> anyhow::Result<()> {
    if capture_size.is_some() {
        page.execute(StopScreencastParams::default()).await?;
    }

    frames.send_replace(None);
    start_screencast(page, desired.0, desired.1).await?;
    *capture_size = Some(desired);

    Ok(())
}

async fn start_screencast(page: &Page, width: u32, height: u32) -> anyhow::Result<()> {
    page.execute(
        StartScreencastParams::builder()
            .format(StartScreencastFormat::Jpeg)
            .quality(100)
            .max_width(width as i64)
            .max_height(height as i64)
            .every_nth_frame(1)
            .build(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{capture_limit, next_retry};

    #[test]
    fn capture_limit_tracks_smaller_window_or_viewport() {
        assert_eq!(capture_limit((1920, 1080), None), (1920, 1080));
        assert_eq!(
            capture_limit((1920, 1080), Some((2560, 1410))),
            (1920, 1080)
        );
        assert_eq!(capture_limit((1920, 1080), Some((1280, 900))), (1280, 900));
        assert_eq!(capture_limit((900, 800), Some((1200, 600))), (900, 600));
    }

    #[test]
    fn reconnect_backoff_caps_at_thirty_seconds() {
        let mut delay = Duration::from_secs(1);
        for expected in [1, 2, 4, 8, 16, 30, 30] {
            assert_eq!(delay.as_secs(), expected);
            delay = next_retry(delay);
        }
    }
}
