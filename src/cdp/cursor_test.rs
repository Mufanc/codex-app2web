use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType, InsertTextParams,
    MouseButton,
};
use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EvaluateParams, EventBindingCalled};
use futures_util::StreamExt;
use std::time::Duration;

use super::{CURSOR_BINDING, CURSOR_SCRIPT};

#[tokio::test]
async fn caret_geometry_and_listener_lifecycle_in_chrome() -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!("codex-caret-test-{}", rand::random::<u64>()));
    let config = BrowserConfig::builder().user_data_dir(&directory).build().map_err(anyhow::Error::msg)?;
    let (mut browser, mut handler) = Browser::launch(config).await?;
    let task = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let result = async {
        let page = browser.new_page("about:blank").await?;
        page.set_content(r#"<style>
            input, textarea, [contenteditable] { font: 20px/28px monospace; padding: 13px 19px; border: 3px solid; box-sizing: border-box; width: 240px; }
            textarea { height: 100px; display: block; }
            #editable { min-height: 70px; }
        </style><input id='input' value='abcdefghij'><textarea id='textarea'></textarea>
        <div id='editable' contenteditable='true'>abcdefghij</div>"#).await?;
        page.evaluate("globalThis.releases = []; document.addEventListener('keyup', event => releases.push(event.code)); document.addEventListener('mouseup', event => releases.push('mouse'))").await?;
        let mut held = super::HeldInput::default();
        let key = DispatchKeyEventParams::builder()
            .r#type(DispatchKeyEventType::KeyDown)
            .key("Shift").code("ShiftLeft").windows_virtual_key_code(16).build().map_err(anyhow::Error::msg)?;
        held.key(&key);
        page.execute(key).await?;
        let mouse = DispatchMouseEventParams::builder()
            .r#type(DispatchMouseEventType::MousePressed)
            .x(20.0).y(20.0).button(MouseButton::Left).buttons(1).build().map_err(anyhow::Error::msg)?;
        held.mouse(&mouse);
        page.execute(mouse).await?;
        held.release(&page).await?;
        held.release(&page).await?;
        let releases: Vec<String> = page.evaluate("releases").await?.into_value()?;
        anyhow::ensure!(releases == ["ShiftLeft", "mouse"], "Input cleanup must release each key and button once: {:?}", releases);
        let mut title_events = page.event_listener::<EventBindingCalled>().await?;
        page.execute(AddBindingParams::new(super::TITLE_BINDING)).await?;
        page.evaluate("globalThis.titles = []; const publishTitle = globalThis.__codexApp2WebTitleChanged; globalThis.__codexApp2WebTitleChanged = title => { titles.push(title); publishTitle(title); }").await?;
        page.evaluate(super::TITLE_SCRIPT).await?;
        page.evaluate(super::TITLE_SCRIPT).await?;
        page.evaluate("titles = [titles.at(-1)]").await?;
        let title_response = page.execute(EvaluateParams::builder()
            .expression(include_str!("title.test.js"))
            .await_promise(true).build().map_err(anyhow::Error::msg)?).await?;
        anyhow::ensure!(title_response.result.exception_details.is_none(), "Browser title test failed: {:?}", title_response.result.exception_details);
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(event) = title_events.next().await {
                if event.name == super::TITLE_BINDING && event.payload == "Task B" {
                    return Ok::<_, anyhow::Error>(());
                }
            }
            anyhow::bail!("Missing CDP title notification")
        }).await??;
        let mut events = page.event_listener::<EventBindingCalled>().await?;
        page.execute(AddBindingParams::new(CURSOR_BINDING)).await?;
        page.evaluate("globalThis.points = []; const publish = globalThis.__codexApp2WebCaret; globalThis.__codexApp2WebCaret = value => { points.push(JSON.parse(value)); publish(value); }").await?;
        page.evaluate(CURSOR_SCRIPT).await?;
        let response = page.execute(EvaluateParams::builder()
            .expression(include_str!("cursor.test.js"))
            .await_promise(true).return_by_value(true).build().map_err(anyhow::Error::msg)?).await?;
        anyhow::ensure!(response.result.exception_details.is_none(), "Browser caret test failed: {:?}", response.result.exception_details);
        let event = tokio::time::timeout(Duration::from_secs(2), events.next()).await?
            .ok_or_else(|| anyhow::anyhow!("Missing CDP caret notification"))?;
        anyhow::ensure!(event.name == CURSOR_BINDING, "Unexpected CDP binding");
        serde_json::from_str::<serde_json::Value>(&event.payload)?;
        page.evaluate("document.querySelector('#input').value = ''; document.querySelector('#input').focus()").await?;
        let (commands, mut receiver) = tokio::sync::mpsc::channel(4);
        let (owner, ownership) = tokio::sync::watch::channel(Some(2));
        let (ready, mut readiness) = tokio::sync::watch::channel(false);
        let updates = super::Updates {
            frames: tokio::sync::watch::channel(None).0,
            cursor: tokio::sync::watch::channel(None).0,
            title: tokio::sync::watch::channel(String::new()).0,
            ready,
        };
        for (owner, text) in [(1, "stale"), (2, "fresh")] {
            commands.send(super::Command { owner, input: super::CdpInput::Text(InsertTextParams::new(text)) }).await?;
        }
        let target = page.clone();
        let cdp_task = tokio::spawn(async move {
            super::run_cdp(target, &mut receiver, updates, ownership, &mut super::HeldInput::default()).await
        });
        let checked = tokio::time::timeout(Duration::from_secs(3), async {
            readiness.wait_for(|ready| *ready).await?;
            loop {
                let value: String = page.evaluate("document.querySelector('#input').value").await?.into_value()?;
                if value == "fresh" { break; }
                anyhow::ensure!(!value.contains("stale"), "Revoked input reached the page");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            owner.send_replace(None);
            Ok::<_, anyhow::Error>(())
        }).await;
        cdp_task.abort();
        checked??;
        Ok::<_, anyhow::Error>(())
    }.await;

    let _ = browser.close().await;
    let _ = browser.wait().await;
    task.abort();
    let _ = std::fs::remove_dir_all(directory);
    result
}
