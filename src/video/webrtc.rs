use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, instrument, warn, Instrument};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder, RTCIceGatheringState,
    RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};

use super::vnc::VncVideo;
use crate::cdp::Frame;
use crate::protocol::{ServerMessage, VideoErrorCode, WebRtcSignal};

const MAX_SDP_BYTES: usize = 128 * 1024;
const MAX_CANDIDATE_BYTES: usize = 4096;
const MAX_VIDEO_ID_BYTES: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const DATA_CHANNEL_LABEL: &str = "rfb";
const DATA_CHANNEL_CHUNK: usize = 16 * 1024;
const DATA_CHANNEL_BUFFER: usize = 256 * 1024;

#[derive(Clone)]
pub(crate) struct Signal {
    video_id: String,
    pub(crate) message: ServerMessage,
}

pub(crate) struct VideoSession {
    id: String,
    peer: Arc<dyn PeerConnection>,
    task: Option<tokio::task::JoinHandle<()>>,
    stop: watch::Sender<bool>,
}

impl VideoSession {
    pub(crate) fn accepts(&self, signal: &Signal) -> bool {
        self.id == signal.video_id
    }

    pub(crate) async fn close(mut self) {
        self.stop.send_replace(true);

        if let Some(task) = self.task.take() {
            let _ = task.await;
        } else {
            let _ = self.peer.close().await;
        }
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

struct PeerCleanup(Option<Arc<dyn PeerConnection>>);

impl Drop for PeerCleanup {
    fn drop(&mut self) {
        if let Some(peer) = self.0.take() {
            tokio::spawn(async move {
                let _ = peer.close().await;
            });
        }
    }
}

#[derive(Clone)]
struct ConnectionHandler {
    video_id: String,
    signals: broadcast::Sender<Signal>,
    connected: watch::Sender<RTCPeerConnectionState>,
    channels: mpsc::Sender<Arc<dyn DataChannel>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ConnectionHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(mut candidate) => {
                candidate.sdp_mid = None;

                let _ = self.signals.send(Signal {
                    video_id: self.video_id.clone(),
                    message: ServerMessage::Ice {
                        video_id: self.video_id.clone(),
                        candidate: Some(candidate),
                    },
                });
            }
            Err(error) => warn!(video_id = %self.video_id, %error, "ICE candidate serialization failed"),
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.signals.send(Signal {
                video_id: self.video_id.clone(),
                message: ServerMessage::Ice {
                    video_id: self.video_id.clone(),
                    candidate: None,
                },
            });
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        if let Err(error) = self.channels.try_send(channel) {
            warn!(%error, "Unexpected additional data channel");
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        info!(video_id = %self.video_id, ?state, transport = "WebRTC", "Peer connection state changed");
        self.connected.send_replace(state);
    }
}

pub(crate) async fn signal(
    frames: watch::Receiver<Option<Frame>>,
    session: &mut Option<VideoSession>,
    message: WebRtcSignal,
    signals: &broadcast::Sender<Signal>,
) -> anyhow::Result<Option<ServerMessage>> {
    match message {
        WebRtcSignal::Offer(request) => {
            anyhow::ensure!(
                !request.video_id.is_empty()
                    && request.video_id.len() <= MAX_VIDEO_ID_BYTES
                    && request.sdp.len() <= MAX_SDP_BYTES,
                "Invalid WebRTC offer"
            );

            let offer = RTCSessionDescription::offer(request.sdp)?;

            if let Some(previous) = session.take() {
                previous.close().await;
            }

            let (next, answer) = start_session(frames, request.video_id.clone(), offer, signals.clone()).await?;

            *session = Some(next);

            Ok(Some(ServerMessage::Answer {
                video_id: request.video_id,
                description: Box::new(answer),
            }))
        }
        WebRtcSignal::Ice { video_id, candidate } => {
            let Some(current) = session.as_ref().filter(|current| current.id == video_id) else {
                return Ok(None);
            };
            let candidate = candidate.unwrap_or_default();

            anyhow::ensure!(
                serde_json::to_vec(&candidate)?.len() <= MAX_CANDIDATE_BYTES,
                "ICE candidate too large"
            );
            current.peer.add_ice_candidate(candidate).await?;

            Ok(None)
        }
    }
}

#[instrument(name = "webrtc_vnc", skip_all, fields(%video_id, transport = "WebRTC"))]
async fn start_session(
    frames: watch::Receiver<Option<Frame>>,
    video_id: String,
    offer: RTCSessionDescription,
    signals: broadcast::Sender<Signal>,
) -> anyhow::Result<(VideoSession, RTCSessionDescription)> {
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: serde_json::from_str(include_str!("../../shared/stun-servers.json"))?,
            ..Default::default()
        }])
        .build();
    let (connected_tx, mut connected) = watch::channel(RTCPeerConnectionState::New);
    let (channels, mut incoming) = mpsc::channel(1);
    let peer = PeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_data_channel_send_buffer_limit(DATA_CHANNEL_BUFFER)
        .with_handler(Arc::new(ConnectionHandler {
            video_id: video_id.clone(),
            signals: signals.clone(),
            connected: connected_tx,
            channels,
        }))
        .with_udp_addrs(vec!["0.0.0.0:0"])
        .build()
        .await?;
    let peer: Arc<dyn PeerConnection> = Arc::new(peer);
    let mut cleanup = PeerCleanup(Some(peer.clone()));

    peer.set_remote_description(offer).await?;

    let answer = peer.create_answer(None).await?;

    peer.set_local_description(answer).await?;

    let answer = peer.local_description().await.ok_or_else(|| anyhow::anyhow!("missing local SDP"))?;
    let (stop, mut stopped) = watch::channel(false);
    let session_id = video_id.clone();
    let task = tokio::spawn(
        async move {
            let run = async {
                let channel = tokio::time::timeout(CONNECT_TIMEOUT, incoming.recv())
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Missing RFB data channel"))?;

                anyhow::ensure!(
                    channel.label().await? == DATA_CHANNEL_LABEL,
                    "Unexpected data channel label"
                );
                anyhow::ensure!(
                    channel.ordered().await?
                        && channel.max_retransmits().await?.is_none()
                        && channel.max_packet_life_time().await?.is_none(),
                    "RFB requires a reliable ordered channel"
                );

                tokio::time::timeout(CONNECT_TIMEOUT, async {
                    loop {
                        match channel.poll().await {
                            Some(DataChannelEvent::OnOpen) => return Ok::<_, anyhow::Error>(()),
                            Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) | None => {
                                anyhow::bail!("RFB channel failed to open")
                            }
                            _ => {}
                        }
                    }
                })
                .await??;

                relay(channel, frames).await
            };

            let failure = async {
                loop {
                    connected.changed().await?;

                    if matches!(
                        *connected.borrow_and_update(),
                        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                    ) {
                        anyhow::bail!("Peer connection failed");
                    }
                }
            };

            let result = tokio::select! {
                _ = stopped.changed() => Ok(()),
                result = run => result,
                result = failure => result,
            };

            if let Err(error) = result {
                warn!(error = %format_args!("{error:#}"), "WebRTC RFB transport failed");
                let _ = signals.send(Signal {
                    video_id: session_id,
                    message: ServerMessage::VideoError {
                        code: VideoErrorCode::Transport,
                    },
                });
            }

            let _ = peer.close().await;
        }
        .in_current_span(),
    );
    let session = VideoSession {
        id: video_id,
        peer: cleanup.0.take().unwrap(),
        task: Some(task),
        stop,
    };

    Ok((session, answer))
}

async fn relay(channel: Arc<dyn DataChannel>, frames: watch::Receiver<Option<Frame>>) -> anyhow::Result<()> {
    let mut video = VncVideo::start(frames)?;
    let (mut reader, mut writer) = video.stream.split();
    let read = async {
        let mut buffer = vec![0; DATA_CHANNEL_CHUNK];

        loop {
            let count = reader.read(&mut buffer).await?;

            anyhow::ensure!(count != 0, "VNC stream ended");
            tokio::time::timeout(SEND_TIMEOUT, channel.send(BytesMut::from(&buffer[..count]))).await??;
        }
    };

    let write = async {
        loop {
            match channel.poll().await {
                Some(DataChannelEvent::OnMessage(message)) => {
                    anyhow::ensure!(
                        !message.is_string && message.data.len() <= DATA_CHANNEL_CHUNK,
                        "Invalid RFB data channel message"
                    );
                    tokio::time::timeout(SEND_TIMEOUT, writer.write_all(&message.data)).await??;
                }
                Some(DataChannelEvent::OnClose | DataChannelEvent::OnClosing | DataChannelEvent::OnError) | None => {
                    anyhow::bail!("RFB data channel closed");
                }
                _ => {}
            }
        }
    };

    let result: anyhow::Result<()> = tokio::select! {
        result = read => result,
        result = write => result,
    };

    video.close().await;

    result
}
