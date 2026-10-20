use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures_util::StreamExt;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};
use tokio::sync::{mpsc, watch};

use crate::cdp::{CdpInput, CdpSession, Command, Frame};
use crate::session::controller::Controller;

fn frame(width: u32, height: u32, color: u8) -> Frame {
    let image = RgbImage::from_pixel(width, height, Rgb([color, 80, 160]));
    let mut image_bytes = Vec::new();
    PngEncoder::new(&mut image_bytes)
        .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
        .unwrap();

    Frame {
        image: image_bytes.into(),
        width: width as f64,
        height: height as f64,
    }
}

async fn wait(page: &Page, expression: &str) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if page.evaluate(expression).await?.into_value::<bool>()? {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Browser condition timed out: {expression}"))??;
    Ok(())
}

#[tokio::test]
async fn browser_video_takeover_resize_and_cdp_reconnect() -> anyhow::Result<()> {
    let (frames_tx, frames) = watch::channel(Some(frame(640, 360, 120)));
    let (_cursor, cursor) = watch::channel(None);
    let (title_tx, title) = watch::channel("Initial task".to_owned());
    let (ready_tx, ready) = watch::channel(true);
    let (commands, mut command_rx) = mpsc::channel::<Command>(64);
    let (menu_results, mut menu_result_rx) = mpsc::channel(8);
    let command_task = tokio::spawn(async move {
        while let Some(command) = command_rx.recv().await {
            if let CdpInput::MenuResult { id, selected } = command.input {
                let _ = menu_results.send((id, selected)).await;
            }
        }
    });
    let controller = Controller::new();
    let ownership = controller.subscribe();
    let (menu_tx, menu) = watch::channel(None);
    let cdp = CdpSession {
        menu,
        frames,
        cursor,
        title,
        ready,
        commands,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, super::router(cdp, controller)).await });
    let directory = std::env::temp_dir().join(format!("codex-session-test-{}", rand::random::<u64>()));
    let config = BrowserConfig::builder().user_data_dir(&directory).build().map_err(anyhow::Error::msg)?;
    let (mut browser, mut handler) = Browser::launch(config).await?;
    let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let result = async {
        let first = browser.new_page("about:blank").await?;
        first.evaluate_on_new_document(include_str!("session.test.js")).await?;
        first.goto(&url).await?;
        wait(&first, "document.querySelector('.fallback-screen')?.width === 640 && document.querySelector('.connection-state')?.textContent.includes('WebSocket') && !document.querySelector('.session-overlay')").await?;
        let first_owner = *ownership.borrow();
        anyhow::ensure!(first_owner.is_some(), "Missing controller");
        wait(&first, "document.title === 'Initial task'").await?;

        let second = browser.new_page("about:blank").await?;
        second.evaluate_on_new_document(include_str!("session.test.js")).await?;
        second.goto(&url).await?;
        wait(&second, "!!document.querySelector('.primary-button')").await?;
        anyhow::ensure!(*ownership.borrow() == first_owner, "Rejected viewer replaced owner");
        second.find_element(".primary-button").await?.click().await?;
        wait(&second, "document.querySelector('.fallback-screen')?.width === 640 && !document.querySelector('.session-overlay')").await?;
        wait(&first, "!!document.querySelector('.primary-button') && document.title === 'CodexApp2Web'").await?;
        anyhow::ensure!(*ownership.borrow() != first_owner, "Takeover did not change controller");

        menu_tx.send_replace(Some(serde_json::json!({
            "id": "menu-test", "x": 630, "y": 350, "items": [
                { "id": "disabled", "label": "Disabled", "enabled": false, "accelerator": "CmdOrCtrl+R" },
                { "id": "submenu", "label": "Submenu", "submenu": [
                    { "id": "first", "label": "First" }, { "id": "choose", "label": "Choose" }
                ] }
            ]
        })));
        wait(&second, "!!document.querySelector('.native-menu')").await?;
        anyhow::ensure!(second.evaluate("document.querySelector('.native-menu button').disabled").await?.into_value::<bool>()?, "Disabled menu item became interactive");
        anyhow::ensure!(second.evaluate("document.querySelector('.native-menu-shortcut').textContent === '⌘+R'").await?.into_value::<bool>()?, "Command modifier should use the macOS symbol");
        second.find_element(".native-menu button[data-index='1']").await?.click().await?;
        wait(&second, "document.querySelectorAll('.native-menu').length === 2").await?;
        anyhow::ensure!(second.evaluate("document.activeElement?.textContent === 'Submenu›'").await?.into_value::<bool>()?, "Hovering a parent menu must not focus the first submenu item");
        second.find_element(".native-menu button[data-index='1']").await?.press_key("ArrowRight").await?;
        wait(&second, "document.activeElement?.textContent === 'First'").await?;
        anyhow::ensure!(second.evaluate("getComputedStyle(document.activeElement).outlineStyle === 'none' && getComputedStyle(document.querySelector('.native-menu')).borderTopWidth === '0px'").await?.into_value::<bool>()?, "Menu focus and container must not draw outlines or borders");
        second.find_element(".native-menu .native-menu button").await?.press_key("ArrowLeft").await?;
        wait(&second, "document.activeElement?.textContent === 'Submenu›'").await?;
        second.find_element(".native-menu button[data-index='1']").await?.press_key("ArrowRight").await?;
        wait(&second, "document.querySelectorAll('.native-menu').length === 2").await?;
        let target = second.evaluate("(() => { const rect = document.querySelector('.native-menu .native-menu button[data-index=\"1\"]').getBoundingClientRect(); return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }; })()").await?.into_value::<serde_json::Value>()?;
        second.execute(chromiumoxide::cdp::browser_protocol::input::DispatchMouseEventParams::builder()
            .r#type(chromiumoxide::cdp::browser_protocol::input::DispatchMouseEventType::MouseMoved)
            .x(target["x"].as_f64().unwrap()).y(target["y"].as_f64().unwrap()).build().map_err(anyhow::Error::msg)?).await?;
        wait(&second, "document.activeElement?.textContent === 'Choose'").await?;
        // Keyboard navigation must also override the previously hovered item.
        second.find_element(".native-menu .native-menu button[data-index='1']").await?.press_key("ArrowUp").await?;
        wait(&second, "document.activeElement?.textContent === 'First'").await?;
        second.find_element(".native-menu .native-menu button[data-index='1']").await?.click().await?;
        let (id, selected) = tokio::time::timeout(Duration::from_secs(2), menu_result_rx.recv()).await?.unwrap();
        anyhow::ensure!(id == "menu-test" && selected.as_deref() == Some("choose"), "Menu selection did not reach the server");
        wait(&second, "!document.querySelector('.native-menu-overlay')").await?;
        menu_tx.send_replace(None);

        for color in [140, 160, 180, 200] {
            frames_tx.send_replace(Some(frame(640, 360, color)));
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        wait(&second, "window.maxPendingFrames >= 2 && window.maxPendingFrames <= 3").await?;
        frames_tx.send_replace(Some(frame(320, 240, 220)));
        title_tx.send_replace("Changed task".to_owned());
        wait(&second, "document.querySelector('.fallback-screen')?.width === 320 && document.title === 'Changed task'").await?;

        ready_tx.send_replace(false);
        wait(&second, "!!document.querySelector('.session-overlay') && document.title === 'CodexApp2Web'").await?;
        ready_tx.send_replace(true);
        wait(&second, "document.querySelector('.fallback-screen')?.width === 320 && !document.querySelector('.session-overlay')").await?;
        second.close().await?;

        let third = browser.new_page("about:blank").await?;
        third.goto(&url).await?;
        // Also exercise the normal WebRTC path in Chrome with hardware HEVC support.
        let supported: bool = third.evaluate("!!RTCRtpReceiver.getCapabilities('video')?.codecs.some(codec => codec.mimeType.toLowerCase() === 'video/h265')").await?.into_value()?;
        anyhow::ensure!(supported, "Test Chrome does not support WebRTC HEVC");
        wait(&third, "document.querySelector('video')?.videoWidth === 320 && document.querySelector('.connection-state')?.textContent.includes('WebRTC') && !document.querySelector('.session-overlay')").await?;
        third.close().await?;
        first.close().await?;
        Ok::<_, anyhow::Error>(())
    }.await;

    let _ = browser.close().await;
    let _ = browser.wait().await;
    driver.abort();
    server.abort();
    command_task.abort();
    let _ = std::fs::remove_dir_all(directory);
    result
}
