use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use serde::Deserialize;

use crate::cdp::CdpInput;

const SUPPORTED_MOUSE_BUTTONS: u8 = 0b111;
const SUPPORTED_MODIFIERS: u8 = 0b1111;
const MAX_INPUT_STRING_BYTES: usize = 4096;
const MIN_VIEWPORT_WIDTH: u32 = 320;
const MAX_VIEWPORT_WIDTH: u32 = 4096;
const MIN_VIEWPORT_HEIGHT: u32 = 240;
const MAX_VIEWPORT_HEIGHT: u32 = 2160;
const SINGLE_CLICK_COUNT: i64 = 1;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Input {
    Reset,
    Mouse {
        action: String,
        x: f64,
        y: f64,
        button: Option<String>,
        buttons: u8,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Key {
        action: String,
        key: String,
        code: String,
        key_code: u32,
        modifiers: u8,
    },
    Text {
        text: String,
    },
    Resize {
        width: u32,
        height: u32,
    },
}

pub(crate) fn input_command(input: Input) -> Option<CdpInput> {
    match input {
        Input::Reset => Some(CdpInput::Reset),
        Input::Mouse {
            action,
            x,
            y,
            button,
            buttons,
        } if x.is_finite()
            && y.is_finite()
            && x >= 0.0
            && y >= 0.0
            && (buttons & !SUPPORTED_MOUSE_BUTTONS) == 0 =>
        {
            let action = match action.as_str() {
                "mouseMoved" => DispatchMouseEventType::MouseMoved,
                "mousePressed" => DispatchMouseEventType::MousePressed,
                "mouseReleased" => DispatchMouseEventType::MouseReleased,
                _ => return None,
            };

            let button = match button.as_deref().unwrap_or("none") {
                "none" => MouseButton::None,
                "left" => MouseButton::Left,
                "middle" => MouseButton::Middle,
                "right" => MouseButton::Right,
                _ => return None,
            };

            Some(CdpInput::Mouse(
                DispatchMouseEventParams::builder()
                    .r#type(action)
                    .x(x)
                    .y(y)
                    .button(button)
                    .buttons(i64::from(buttons))
                    .click_count(SINGLE_CLICK_COUNT)
                    .build()
                    .ok()?,
            ))
        }
        Input::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } if [x, y, delta_x, delta_y].iter().all(|n| n.is_finite()) && x >= 0.0 && y >= 0.0 => {
            Some(CdpInput::Mouse(
                DispatchMouseEventParams::builder()
                    .r#type(DispatchMouseEventType::MouseWheel)
                    .x(x)
                    .y(y)
                    .delta_x(delta_x)
                    .delta_y(delta_y)
                    .build()
                    .ok()?,
            ))
        }
        Input::Key {
            action,
            key,
            code,
            key_code,
            modifiers,
        } if key.len() <= MAX_INPUT_STRING_BYTES
            && code.len() <= MAX_INPUT_STRING_BYTES
            && (modifiers & !SUPPORTED_MODIFIERS) == 0 =>
        {
            let action = match action.as_str() {
                "keyDown" => DispatchKeyEventType::KeyDown,
                "keyUp" => DispatchKeyEventType::KeyUp,
                _ => return None,
            };

            Some(CdpInput::Key(
                DispatchKeyEventParams::builder()
                    .r#type(action)
                    .key(key)
                    .code(code)
                    .windows_virtual_key_code(i64::from(key_code))
                    .modifiers(i64::from(modifiers))
                    .build()
                    .ok()?,
            ))
        }
        Input::Text { text } if !text.is_empty() && text.len() <= MAX_INPUT_STRING_BYTES => {
            Some(CdpInput::Text(InsertTextParams::new(text)))
        }
        Input::Resize { width, height }
            if (MIN_VIEWPORT_WIDTH..=MAX_VIEWPORT_WIDTH).contains(&width)
                && (MIN_VIEWPORT_HEIGHT..=MAX_VIEWPORT_HEIGHT).contains(&height) =>
        {
            Some(CdpInput::Resize(width, height))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_input_becomes_typed_cdp_command() {
        let command = input_command(Input::Mouse {
            action: "mousePressed".into(),
            x: 12.0,
            y: 34.0,
            button: Some("left".into()),
            buttons: 1,
        });
        assert!(matches!(command, Some(CdpInput::Mouse(_))));
        assert!(input_command(Input::Text {
            text: String::new()
        })
        .is_none());
        assert!(matches!(
            input_command(Input::Resize {
                width: 1920,
                height: 1080
            }),
            Some(CdpInput::Resize(1920, 1080))
        ));
    }

    #[test]
    fn key_names_longer_than_forty_bytes_are_valid() {
        assert!(matches!(
            input_command(Input::Key {
                action: "keyDown".into(),
                key: "a".repeat(41),
                code: "KeyA".repeat(11),
                key_code: 65,
                modifiers: SUPPORTED_MODIFIERS,
            }),
            Some(CdpInput::Key(_))
        ));
        assert!(input_command(Input::Key {
            action: "keyDown".into(),
            key: "a".repeat(MAX_INPUT_STRING_BYTES + 1),
            code: "KeyA".into(),
            key_code: 65,
            modifiers: 0,
        })
        .is_none());
    }
}
