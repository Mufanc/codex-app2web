use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use rtc::media::Sample;

use rtc::interceptor::{BandwidthEstimator, EstimatorStats, Gcc, PacketReport, Registry};
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::{
    configure_congestion_control, register_default_interceptors, CongestionFeedback,
};
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_HEVC;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::{broadcast, watch};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::rtp_transceiver::RtpSender;

use super::{EncoderSession, FRAME_INTERVAL, INITIAL_BITRATE, MAX_BITRATE, MIN_BITRATE};
use crate::cdp::Frame;
use crate::protocol::{ServerMessage, VideoErrorCode, VideoSignal};

const MAX_SDP_BYTES: usize = 128 * 1024;
const MAX_CANDIDATE_BYTES: usize = 4096;
const MAX_VIDEO_ID_BYTES: usize = 64;

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
            Err(error) => eprintln!("ICE candidate serialization failed: {error}"),
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

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        eprintln!("WebRTC {}: {state:?}", self.video_id);
        self.connected.send_replace(state);
    }
}

struct ReportingEstimator {
    inner: Gcc,
    target: Arc<AtomicU64>,
}

impl ReportingEstimator {
    fn new() -> (Self, Arc<AtomicU64>) {
        let inner = Gcc::new(INITIAL_BITRATE as f64, MIN_BITRATE as f64, MAX_BITRATE as f64);
        let target = Arc::new(AtomicU64::new(inner.target_bitrate() as u64));

        (
            Self {
                inner,
                target: target.clone(),
            },
            target,
        )
    }

    fn publish(&self) {
        self.target.store(self.inner.target_bitrate() as u64, Ordering::Relaxed);
    }
}

impl BandwidthEstimator for ReportingEstimator {
    fn on_reports(&mut self, now: Instant, reports: &[PacketReport]) {
        self.inner.on_reports(now, reports);
        self.publish();
    }

    fn target_bitrate(&self) -> f64 {
        self.inner.target_bitrate()
    }

    fn handle_timeout(&mut self, now: Instant) {
        self.inner.handle_timeout(now);
        self.publish();
    }

    fn poll_timeout(&self) -> Option<Instant> {
        self.inner.poll_timeout()
    }

    fn stats(&self) -> EstimatorStats {
        self.inner.stats()
    }
}

pub(crate) async fn signal(
    frames: watch::Receiver<Option<Frame>>,
    session: &mut Option<VideoSession>,
    message: VideoSignal,
    signals: &broadcast::Sender<Signal>,
) -> anyhow::Result<Option<ServerMessage>> {
    match message {
        VideoSignal::Offer(request) => {
            anyhow::ensure!(
                !request.video_id.is_empty()
                    && request.video_id.len() <= MAX_VIDEO_ID_BYTES
                    && (16..=4096).contains(&request.width)
                    && (16..=2160).contains(&request.height)
                    && request.sdp.len() <= MAX_SDP_BYTES,
                "Invalid WebRTC offer"
            );

            let offer = RTCSessionDescription::offer(request.sdp)?;

            if let Some(previous) = session.take() {
                previous.close().await;
            }

            let (next, answer) = start_session(
                frames,
                request.video_id.clone(),
                request.width & !1,
                request.height & !1,
                offer,
                signals.clone(),
            )
            .await?;

            *session = Some(next);

            Ok(Some(ServerMessage::Answer {
                video_id: request.video_id,
                description: Box::new(answer),
            }))
        }
        VideoSignal::Ice { video_id, candidate } => {
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

async fn start_session(
    frames: watch::Receiver<Option<Frame>>,
    video_id: String,
    width: u32,
    height: u32,
    offer: RTCSessionDescription,
    signals: broadcast::Sender<Signal>,
) -> anyhow::Result<(VideoSession, RTCSessionDescription)> {
    let codec = RTCRtpCodecParameters {
        rtp_codec: RTCRtpCodec {
            mime_type: MIME_TYPE_HEVC.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: String::new(),
            rtcp_feedback: vec![],
        },
        payload_type: 98,
    };
    let mut media_engine = MediaEngine::default();

    media_engine.register_codec(codec.clone(), RtpCodecKind::Video)?;

    let (estimator, target_bitrate) = ReportingEstimator::new();
    let registry =
        configure_congestion_control(Registry::new(), estimator, CongestionFeedback::Twcc, &mut media_engine)?;
    let registry = register_default_interceptors(registry, &mut media_engine)?;
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: serde_json::from_str(include_str!("../../shared/stun-servers.json"))?,
            ..Default::default()
        }])
        .build();
    let (connected_tx, connected) = watch::channel(RTCPeerConnectionState::New);
    let peer = PeerConnectionBuilder::new()
        .with_configuration(configuration)
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(ConnectionHandler {
            video_id: video_id.clone(),
            signals: signals.clone(),
            connected: connected_tx,
        }))
        .with_udp_addrs(vec!["0.0.0.0:0"])
        .build()
        .await?;
    let peer: Arc<dyn PeerConnection> = Arc::new(peer);
    let mut cleanup = PeerCleanup(Some(peer.clone()));
    let setup = async {
        let ssrc = rand::random::<u32>();
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "codex".into(),
                "screen".into(),
                "Codex screen".into(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: codec.rtp_codec,
                    ..Default::default()
                }],
            ),
        )?);
        let sender = peer.add_track(track.clone() as Arc<dyn TrackLocal>).await?;

        peer.set_remote_description(offer).await?;

        let answer = peer.create_answer(None).await?;

        peer.set_local_description(answer).await?;

        let answer = peer.local_description().await.ok_or_else(|| anyhow::anyhow!("missing local SDP"))?;

        Ok::<_, anyhow::Error>((sender, track, ssrc, answer))
    }
    .await;

    let (sender, track, ssrc, answer) = match setup {
        Ok(result) => result,
        Err(error) => {
            let _ = peer.close().await;
            cleanup.0 = None;
            return Err(error);
        }
    };
    let (stop, stopped) = watch::channel(false);
    let mut session = VideoSession {
        id: video_id,
        peer: peer.clone(),
        task: None,
        stop,
    };

    let video_id = session.id.clone();

    session.task = Some(tokio::spawn(async move {
        if let Err(error) = run_session(
            sender,
            track,
            ssrc,
            target_bitrate,
            frames,
            connected,
            stopped,
            width,
            height,
        )
        .await
        {
            eprintln!("WebRTC video: {error:#}");
            let _ = signals.send(Signal {
                video_id,
                message: ServerMessage::VideoError {
                    code: VideoErrorCode::Encoder,
                },
            });
        }

        let _ = peer.close().await;
    }));

    cleanup.0 = None;

    Ok((session, answer))
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    sender: Arc<dyn RtpSender>,
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    target_bitrate: Arc<AtomicU64>,
    frames: watch::Receiver<Option<Frame>>,
    mut connected: watch::Receiver<RTCPeerConnectionState>,
    mut stopped: watch::Receiver<bool>,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    loop {
        if *stopped.borrow() {
            return Ok(());
        }

        match *connected.borrow_and_update() {
            RTCPeerConnectionState::Connected => break,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => return Ok(()),
            _ => {}
        }

        tokio::select! {
            _ = stopped.changed() => return Ok(()),
            result = connected.changed() => { result?; }
        }
    }

    let payload_type = sender
        .get_parameters()
        .await?
        .rtp_parameters
        .codecs
        .first()
        .ok_or_else(|| anyhow::anyhow!("HEVC codec was not negotiated"))?
        .payload_type;
    let mut encoder = EncoderSession::start(frames, width, height, target_bitrate);
    let result = async {
        loop {
            if *stopped.borrow() { break; }

            tokio::select! {
                _ = stopped.changed() => break,
                result = connected.changed() => {
                    if result.is_err() || matches!(*connected.borrow_and_update(), RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) { break; }
                }
                sample = encoder.next() => {
                    let sample = sample?;
                    let send = async {
                        let gap = sample.duration.saturating_sub(FRAME_INTERVAL);

                        if !gap.is_zero() {
                            // Advance RTP time over idle periods before sending the next frame.
                            track.sample_writer(ssrc, payload_type).write_sample(&Sample {
                                data: Bytes::new(), duration: gap, ..Sample::new(Instant::now())
                            }).await?;
                        }

                        track.sample_writer(ssrc, payload_type).write_sample(&Sample {
                            data: Bytes::from(sample.data), duration: FRAME_INTERVAL, ..Sample::new(Instant::now())
                        }).await
                    };

                    tokio::select! {
                        _ = stopped.changed() => break,
                        result = send => { result?; }
                    }
                }
            }
        }

        Ok::<_, anyhow::Error>(())
    }
    .await;

    let closed = encoder.close().await;

    result.and(closed)
}
