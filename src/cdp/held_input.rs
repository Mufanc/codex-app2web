use std::collections::HashMap;

use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
};
use chromiumoxide::page::Page;

#[derive(Default)]
pub(super) struct HeldInput {
    keys: HashMap<String, DispatchKeyEventParams>,
    buttons: Vec<DispatchMouseEventParams>,
}

impl HeldInput {
    pub(super) fn key(&mut self, params: &DispatchKeyEventParams) {
        let code = params.code.clone().unwrap_or_default();

        if params.r#type == DispatchKeyEventType::KeyUp {
            self.keys.remove(&code);
        } else {
            self.keys.insert(code, params.clone());
        }
    }

    pub(super) fn mouse(&mut self, params: &DispatchMouseEventParams) {
        for button in &mut self.buttons {
            button.x = params.x;
            button.y = params.y;
        }

        if params.r#type == DispatchMouseEventType::MouseReleased {
            self.buttons.retain(|button| button.button != params.button);
        } else if params.r#type == DispatchMouseEventType::MousePressed {
            self.buttons.retain(|button| button.button != params.button);
            self.buttons.push(params.clone());
        }
    }

    pub(super) async fn release(&mut self, page: &Page) -> anyhow::Result<()> {
        // Keep entries until CDP confirms release so reconnect can retry unfinished cleanup.
        for code in self.keys.keys().cloned().collect::<Vec<_>>() {
            let mut params = self.keys[&code].clone();

            params.r#type = DispatchKeyEventType::KeyUp;
            params.modifiers = Some(0);
            page.execute(params).await?;
            self.keys.remove(&code);
        }

        while let Some(mut params) = self.buttons.last().cloned() {
            params.r#type = DispatchMouseEventType::MouseReleased;
            params.buttons = Some(0);
            page.execute(params).await?;
            self.buttons.pop();
        }

        Ok(())
    }
}
