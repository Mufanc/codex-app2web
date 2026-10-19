use std::time::{Duration, Instant};

use bytes::Bytes;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchMouseEventParams, InsertTextParams};
use chromiumoxide::cdp::browser_protocol::page::{
    EventDomContentEventFired, EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat,
    StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EvaluateParams, EventBindingCalled};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

mod held_input;
use self::held_input::HeldInput;

#[cfg(test)]
mod cursor_test;

const TITLE_SCRIPT: &str = include_str!("cdp/title.js");
const TITLE_STOP_SCRIPT: &str = "globalThis.__codexApp2WebTitle?.disconnect()";
const TITLE_BINDING: &str = "__codexApp2WebTitleChanged";

const CURSOR_SCRIPT: &str = include_str!("cdp/cursor.js");
const CURSOR_STOP_SCRIPT: &str = "globalThis.__codexApp2WebCursor?.stop()";
const CURSOR_BINDING: &str = "__codexApp2WebCaret";

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) jpeg: Bytes,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

pub(crate) enum CdpInput {
    Mouse(DispatchMouseEventParams),
    Key(DispatchKeyEventParams),
    Text(InsertTextParams),
    Resize(u32, u32),
    Reset,
}

pub(crate) struct Command {
    pub(crate) owner: u64,
    pub(crate) input: CdpInput,
}

#[derive(Clone)]
pub(crate) struct CdpSession {
    pub(crate) frames: watch::Receiver<Option<Frame>>,
    pub(crate) cursor: watch::Receiver<Option<Value>>,
    pub(crate) title: watch::Receiver<String>,
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) ready: watch::Receiver<bool>,
}

#[derive(Clone)]
struct Updates {
    frames: watch::Sender<Option<Frame>>,
    cursor: watch::Sender<Option<Value>>,
    title: watch::Sender<String>,
    ready: watch::Sender<bool>,
}

pub(crate) fn connect(cdp_port: u16, ownership: watch::Receiver<Option<u64>>) -> CdpSession {
    let (commands, command_rx) = mpsc::channel(64);
    let (frame_tx, frames) = watch::channel(None);
    let (cursor_tx, cursor) = watch::channel(None);
    let (title_tx, title) = watch::channel(String::new());
    let (ready_tx, ready) = watch::channel(false);

    tokio::spawn(reconnect(
        cdp_port,
        command_rx,
        Updates {
            frames: frame_tx,
            cursor: cursor_tx,
            title: title_tx,
            ready: ready_tx,
        },
        ownership,
    ));

    CdpSession {
        frames,
        cursor,
        title,
        commands,
        ready,
    }
}

async fn reconnect(
    cdp_port: u16,
    mut commands: mpsc::Receiver<Command>,
    updates: Updates,
    ownership: watch::Receiver<Option<u64>>,
) {
    let mut delay = Duration::from_secs(1);
    let mut held = HeldInput::default();

    loop {
        while commands.try_recv().is_ok() {}

        let result = run_connection(cdp_port, &mut commands, &updates, ownership.clone(), &mut held).await;
        let was_connected = *updates.ready.borrow();

        updates.ready.send_replace(false);
        updates.frames.send_replace(None);
        updates.cursor.send_replace(None);
        updates.title.send_replace(String::new());

        if was_connected {
            delay = Duration::from_secs(1);
        }

        eprintln!(
            "CDP disconnected: {}; retrying in {}s",
            result.err().map_or_else(|| "session ended".into(), |error| error.to_string()),
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
    cdp_port: u16,
    commands: &mut mpsc::Receiver<Command>,
    updates: &Updates,
    ownership: watch::Receiver<Option<u64>>,
    held: &mut HeldInput,
) -> anyhow::Result<()> {
    let (mut browser, mut handler) = Browser::connect(format!("http://127.0.0.1:{cdp_port}")).await?;

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

            run_cdp(page, commands, updates.clone(), ownership, held).await
        } => result,
        _ = &mut handler_task => Err(anyhow::anyhow!("CDP transport closed")),
    };

    handler_task.abort();
    result
}

async fn run_cdp(
    page: Page,
    commands: &mut mpsc::Receiver<Command>,
    updates: Updates,
    mut ownership: watch::Receiver<Option<u64>>,
    held: &mut HeldInput,
) -> anyhow::Result<()> {
    let Updates {
        frames,
        cursor,
        title,
        ready,
    } = updates;
    let mut events = page.event_listener::<EventScreencastFrame>().await?;

    let mut caret_events = page.event_listener::<EventBindingCalled>().await?;
    let mut loaded = page.event_listener::<EventDomContentEventFired>().await?;

    page.execute(AddBindingParams::new(CURSOR_BINDING)).await?;
    evaluate_script(&page, CURSOR_STOP_SCRIPT).await?;

    page.execute(AddBindingParams::new(TITLE_BINDING)).await?;
    evaluate_script(&page, TITLE_SCRIPT).await?;

    held.release(&page).await?;
    ready.send_replace(true);
    eprintln!("CDP connected: Codex page ready");

    let mut capture_size = None;
    let mut viewport_size = None;
    let mut window_size = None;
    let mut window_changed_at: Option<Instant> = None;

    let mut owner = None;
    let mut pending_command = None;
    let result = async {
    loop {
        let current = *ownership.borrow_and_update();

        if current != owner {
            held.release(&page).await?;
            evaluate_script(&page, CURSOR_STOP_SCRIPT).await?;
            cursor.send_replace(None);
            if capture_size.take().is_some() { page.execute(StopScreencastParams::default()).await?; }
            frames.send_replace(None);
            viewport_size = None;
            window_size = None;
            window_changed_at = None;
            owner = current;
        }

        tokio::select! {
            _ = ownership.changed() => continue,
            Some(event) = caret_events.next() => {
                if event.name == TITLE_BINDING {
                    title.send_if_modified(|current| {
                        if *current == event.payload { return false; }
                        current.clone_from(&event.payload);
                        true
                    });

                    continue;
                }

                if viewport_size.is_none() || event.name != CURSOR_BINDING { continue; }

                let Ok(point) = serde_json::from_str::<Value>(&event.payload) else { continue; };
                let point = if point.is_null() {
                    None
                } else if point.get("x").and_then(Value::as_f64).is_some_and(f64::is_finite)
                    && point.get("y").and_then(Value::as_f64).is_some_and(f64::is_finite) {
                    Some(point)
                } else { continue; };

                cursor.send_if_modified(|current| {
                    if *current == point { return false; }
                    *current = point;
                    true
                });
            }
            Some(_) = loaded.next() => {
                evaluate_script(&page, TITLE_SCRIPT).await?;
                if viewport_size.is_some() { evaluate_script(&page, CURSOR_SCRIPT).await?; }
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
            Some(command) = async {
                if pending_command.is_some() { pending_command.take() } else { commands.recv().await }
            } => {
                if Some(command.owner) != *ownership.borrow() { continue; }
                if Some(command.owner) != owner { pending_command = Some(command); continue; }

                match command.input {
                    CdpInput::Mouse(params) => { held.mouse(&params); if let Err(error) = page.execute(params).await { eprintln!("Mouse input failed: {error}"); } }
                    CdpInput::Key(params) => { held.key(&params); if let Err(error) = page.execute(params).await { eprintln!("Key input failed: {error}"); } }
                    CdpInput::Text(params) => { if let Err(error) = page.execute(params).await { eprintln!("Text input failed: {error}"); } }
                    CdpInput::Resize(width, height) => {
                        if viewport_size.is_none() { evaluate_script(&page, CURSOR_SCRIPT).await?; }
                        viewport_size = Some((width, height));

                        let desired = capture_limit((width, height), window_size);

                        if capture_size != Some(desired) {
                            configure_capture(&page, &frames, &mut capture_size, desired).await?;
                        }
                    }
                    CdpInput::Reset => { held.release(&page).await?; }
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

                    if owner.is_some() && frames.borrow().as_ref().is_none_or(|old| old.jpeg.as_ref() != jpeg || old.width != width || old.height != height) {
                        frames.send_replace(Some(Frame { jpeg: jpeg.into(), width, height }));
                    }
                }

                page.execute(ScreencastFrameAckParams::new(event.session_id)).await?;
            }
        }
    }

    Ok::<_, anyhow::Error>(())
    }.await;

    let _ = held.release(&page).await;
    let _ = evaluate_script(&page, CURSOR_STOP_SCRIPT).await;
    let _ = evaluate_script(&page, TITLE_STOP_SCRIPT).await;

    result
}

async fn evaluate_script(page: &Page, script: &str) -> anyhow::Result<()> {
    let result = page
        .execute(EvaluateParams::builder().expression(script).build().map_err(anyhow::Error::msg)?)
        .await?;

    anyhow::ensure!(result.result.exception_details.is_none(), "Page tracking script failed");
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
        assert_eq!(capture_limit((1920, 1080), Some((2560, 1410))), (1920, 1080));
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
