use serde::{Deserialize, Serialize};
use serde_json::Value;
use webrtc::peer_connection::{RTCIceCandidateInit, RTCSessionDescription};

use crate::input::Input;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    RequestFallback,
    #[serde(untagged)]
    WebRtc(WebRtcSignal),
    #[serde(untagged)]
    Input(Input),
}

#[derive(Deserialize)]
pub(crate) struct WebRtcOffer {
    pub(crate) video_id: String,
    pub(crate) sdp: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WebRtcSignal {
    Offer(WebRtcOffer),
    Ice {
        video_id: String,
        candidate: Option<RTCIceCandidateInit>,
    },
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    Menu {
        menu: Option<Value>,
    },
    Title {
        title: String,
    },
    Cursor {
        point: Option<Value>,
    },
    Size {
        width: f64,
        height: f64,
    },
    Answer {
        video_id: String,
        description: Box<RTCSessionDescription>,
    },
    Ice {
        video_id: String,
        candidate: Option<RTCIceCandidateInit>,
    },
    VideoError {
        code: VideoErrorCode,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VideoErrorCode {
    #[serde(rename = "signaling_failed")]
    Signaling,
    #[serde(rename = "transport_failed")]
    Transport,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn websocket_accepts_trickle_signals_and_existing_input() {
        assert!(matches!(
            serde_json::from_value::<ClientMessage>(json!({ "type": "request_fallback" })).unwrap(),
            ClientMessage::RequestFallback
        ));
        assert!(serde_json::from_value::<ClientMessage>(json!({ "type": "unknown" })).is_err());
        let offer = json!({
            "type": "offer", "video_id": "test", "sdp": "offer",
        });
        assert!(matches!(
            serde_json::from_value::<ClientMessage>(offer).unwrap(),
            ClientMessage::WebRtc(WebRtcSignal::Offer(_))
        ));

        let candidate = json!({
            "type": "ice", "video_id": "test", "candidate": {
                "candidate": "candidate:test", "sdpMid": "0", "sdpMLineIndex": 0,
                "usernameFragment": "test",
            },
        });
        let ClientMessage::WebRtc(WebRtcSignal::Ice {
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
            ClientMessage::WebRtc(WebRtcSignal::Ice { candidate: None, .. })
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
