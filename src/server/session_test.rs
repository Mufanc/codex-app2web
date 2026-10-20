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
        wait(&first, "document.querySelector('#screen canvas')?.width === 640 && document.querySelector('.connection-state')?.textContent.includes('WebSocket') && !document.querySelector('.session-overlay')").await?;
        let first_owner = *ownership.borrow();
        anyhow::ensure!(first_owner.is_some(), "Missing controller");
        wait(&first, "document.title === 'Initial task'").await?;

        let second = browser.new_page("about:blank").await?;
        second.evaluate_on_new_document(include_str!("session.test.js")).await?;
        second.goto(&url).await?;
        wait(&second, "!!document.querySelector('.primary-button')").await?;
        anyhow::ensure!(*ownership.borrow() == first_owner, "Rejected viewer replaced owner");
        second.find_element(".primary-button").await?.click().await?;
        wait(&second, "document.querySelector('#screen canvas')?.width === 640 && !document.querySelector('.session-overlay')").await?;
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
        wait(&second, "document.querySelector('#screen canvas')?.getContext('2d').getImageData(0, 0, 1, 1).data[0] === 200").await?;
        // Repeated identical captures must not retransmit the framebuffer.
        let bytes: u64 = second.evaluate("window.vncBytes").await?.into_value()?;
        for _ in 0..10 {
            frames_tx.send_replace(Some(frame(640, 360, 200)));
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        let idle_bytes: u64 = second.evaluate("window.vncBytes").await?.into_value()?;
        anyhow::ensure!(idle_bytes - bytes < 128, "Static captures generated {} bytes", idle_bytes - bytes);
        frames_tx.send_replace(Some(frame(320, 240, 220)));
        title_tx.send_replace("Changed task".to_owned());
        wait(&second, "document.querySelector('#screen canvas')?.width === 320 && document.title === 'Changed task'").await?;

        // High-entropy pixels expose accidental JPEG negotiation; sparse changes must stay sparse.
        let mut noise = RgbImage::new(320, 240);
        let mut state = 1_u32;
        for pixel in noise.pixels_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *pixel = Rgb([state as u8, (state >> 8) as u8, (state >> 16) as u8]);
        }
        let encode_noise = |image: &RgbImage| {
            let mut bytes = Vec::new();
            PngEncoder::new(&mut bytes).write_image(image.as_raw(), 320, 240, ExtendedColorType::Rgb8).unwrap();
            Frame { image: bytes.into(), width: 320.0, height: 240.0 }
        };
        let original_noise = noise.clone();
        let noise_matches = "(() => { const canvas = document.querySelector('#screen canvas'); if (!canvas) return false; const data = canvas.getContext('2d').getImageData(0, 0, 320, 240).data; let state = 1; for (let i = 0; i < data.length; i += 4) { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; if (data[i] !== (state & 255) || data[i + 1] !== ((state >>> 8) & 255) || data[i + 2] !== ((state >>> 16) & 255)) return false; } return true; })()";
        frames_tx.send_replace(Some(encode_noise(&noise)));
        wait(&second, noise_matches).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let before: u64 = second.evaluate("window.vncBytes").await?.into_value()?;
        for y in 0..16 { for x in 0..2 { noise.put_pixel(x, y, Rgb([255, 0, 0])); } }
        frames_tx.send_replace(Some(encode_noise(&noise)));
        wait(&second, "document.querySelector('#screen canvas').getContext('2d').getImageData(0, 0, 1, 1).data[0] === 255").await?;
        let after: u64 = second.evaluate("window.vncBytes").await?.into_value()?;
        anyhow::ensure!(after > before && after - before < 4096, "Sparse update generated {} bytes", after - before);
        eprintln!("VNC: repeated static captures = {} bytes, 2x16 pixel update = {} bytes", idle_bytes - bytes, after - before);

        ready_tx.send_replace(false);
        wait(&second, "!!document.querySelector('.session-overlay') && document.title === 'CodexApp2Web'").await?;
        ready_tx.send_replace(true);
        wait(&second, "document.querySelector('#screen canvas')?.width === 320 && !document.querySelector('.session-overlay')").await?;
        second.close().await?;

        frames_tx.send_replace(Some(frame(321, 241, 88)));
        let third = browser.new_page("about:blank").await?;
        third.evaluate_on_new_document("(() => { const Peer = window.RTCPeerConnection; window.RTCPeerConnection = class extends Peer { createDataChannel(...args) { const channel = super.createDataChannel(...args); window.rfbChannel = channel; return channel; } }; })()").await?;
        third.goto(&url).await?;
        wait(&third, "document.querySelector('#screen canvas')?.width === 321 && document.querySelector('#screen canvas')?.height === 241 && document.querySelector('.connection-state')?.textContent.includes('WebRTC') && !document.querySelector('.session-overlay')").await?;
        frames_tx.send_replace(Some(encode_noise(&original_noise)));
        wait(&third, noise_matches).await?;
        frames_tx.send_replace(Some(frame(321, 241, 99)));
        wait(&third, "document.querySelector('#screen canvas')?.getContext('2d').getImageData(0, 0, 1, 1).data[0] === 99").await?;
        third.evaluate("window.rfbChannel.close()").await?;
        wait(&third, "document.querySelector('#screen canvas')?.width === 321 && document.querySelector('.connection-state')?.textContent.includes('WebSocket') && !document.querySelector('.session-overlay')").await?;
        frames_tx.send_replace(Some(frame(321, 241, 109)));
        wait(&third, "document.querySelector('#screen canvas')?.getContext('2d').getImageData(0, 0, 1, 1).data[0] === 109").await?;

        let fourth = browser.new_page(&url).await?;
        wait(&fourth, "!!document.querySelector('.primary-button')").await?;
        fourth.find_element(".primary-button").await?.click().await?;
        wait(&fourth, "document.querySelector('#screen canvas')?.width === 321 && document.querySelector('.connection-state')?.textContent.includes('WebRTC') && !document.querySelector('.session-overlay')").await?;
        wait(&third, "!!document.querySelector('.primary-button')").await?;
        fourth.close().await?;

        // A real peer with no usable ICE route must fall back to WebSocket.
        let fifth = browser.new_page("about:blank").await?;
        fifth.evaluate_on_new_document("(() => { const Peer = window.RTCPeerConnection; window.RTCPeerConnection = class extends Peer { constructor(config) { super({ ...config, iceServers: [], iceTransportPolicy: 'relay' }); } }; })()").await?;
        fifth.goto(&url).await?;
        wait(&fifth, "document.querySelector('.primary-button') || !document.querySelector('.session-overlay')").await?;
        fifth.evaluate("document.querySelector('.primary-button')?.click()").await?;
        wait(&fifth, "document.querySelector('#screen canvas')?.width === 321 && document.querySelector('.connection-state')?.textContent.includes('WebSocket') && !document.querySelector('.session-overlay')").await?;
        fifth.close().await?;

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

#[tokio::test]
#[ignore = "manual scrolling latency benchmark"]
async fn scrolling_latency() -> anyhow::Result<()> {
    use base64::Engine;
    use std::time::Instant;

    let _ = tracing_subscriber::fmt().with_env_filter("codex_app2web=debug").with_test_writer().try_init();

    let (frames_tx, frames) = watch::channel(Some(frame(1920, 1080, 120)));
    let (_cursor, cursor) = watch::channel(None);
    let (_title, title) = watch::channel(String::new());
    let (_ready, ready) = watch::channel(true);
    let (_menu, menu) = watch::channel(None);
    let (commands, _command_rx) = mpsc::channel(64);
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
    let server = tokio::spawn(async move { axum::serve(listener, super::router(cdp, Controller::new())).await });
    let directory = std::env::temp_dir().join(format!("codex-scroll-test-{}", rand::random::<u64>()));
    let config = BrowserConfig::builder().user_data_dir(&directory).build().map_err(anyhow::Error::msg)?;
    let (mut browser, mut handler) = Browser::launch(config).await?;
    let driver = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let result = async {
        let page = browser.new_page("about:blank").await?;
        let png: String = page.evaluate(r#"(() => {
            const canvas = document.createElement('canvas'); canvas.width = 1920; canvas.height = 1800;
            const ctx = canvas.getContext('2d'); ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, 1920, 1800);
            ctx.font = '16px monospace';
            for (let row = 0; row < 90; row++) {
                ctx.fillStyle = ['#333', '#1967d2', '#666', '#b22'][row % 4];
                ctx.fillText(`${row}: async function handleMessage(request) { return await session.execute(request.command); }`, 300, 20 * row + 18);
            }
            ctx.fillStyle = '#eee'; ctx.fillRect(0, 0, 260, 1800);
            return canvas.toDataURL('image/png').split(',')[1];
        })()"#).await?.into_value()?;
        let image = image::load_from_memory(&base64::engine::general_purpose::STANDARD.decode(png)?)?.to_rgb8();
        let samples: Vec<Frame> = (0..30).map(|index| {
            let mut viewport = image::imageops::crop_imm(&image, 0, index * 20, 1920, 1080).to_image();
            for y in 0..8 { for x in 0..8 { viewport.put_pixel(x, y, Rgb([index as u8 + 1, 0, 0])); } }
            let mut bytes = Vec::new();
            PngEncoder::new(&mut bytes).write_image(viewport.as_raw(), 1920, 1080, ExtendedColorType::Rgb8).unwrap();
            Frame { image: bytes.into(), width: 1920.0, height: 1080.0 }
        }).collect();
        page.evaluate_on_new_document(include_str!("session.test.js")).await?;
        page.goto(&url).await?;
        wait(&page, "!document.querySelector('.session-overlay')").await?;
        page.evaluate("window.rendered = []; window.recording = true; (() => { let previous = -1; function tick() { const canvas = document.querySelector('#screen canvas'); if (canvas) { const value = canvas.getContext('2d').getImageData(0, 0, 1, 1).data[0]; if (value !== previous) { window.rendered.push([performance.now(), value]); previous = value; } } if (window.recording) requestAnimationFrame(tick); } tick(); })()").await?;
        let started = Instant::now();
        for sample in samples {
            frames_tx.send_replace(Some(sample));
            tokio::time::sleep(Duration::from_millis(67)).await;
        }
        let stopped = Instant::now();
        wait(&page, "document.querySelector('#screen canvas').getContext('2d').getImageData(0, 0, 1, 1).data[0] === 30").await?;
        let catch_up = stopped.elapsed();
        let rendered: serde_json::Value = page.evaluate("(async () => { await new Promise(requestAnimationFrame); window.recording = false; return { frames: window.rendered, bytes: window.vncBytes }; })()").await?.into_value()?;
        eprintln!("SCROLL produced={:?}, catch_up={:?}, rendered={rendered}", stopped.duration_since(started), catch_up);
        anyhow::ensure!(catch_up < Duration::from_millis(500), "Scrolling left a stale-frame backlog: {catch_up:?}");
        anyhow::ensure!(rendered["frames"].as_array().unwrap().len() >= 10, "Too few scrolling updates: {rendered}");
        page.close().await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    let _ = browser.close().await;
    let _ = browser.wait().await;
    driver.abort();
    server.abort();
    let _ = std::fs::remove_dir_all(directory);
    result
}
