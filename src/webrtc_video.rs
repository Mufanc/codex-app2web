use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rtc::interceptor::{BandwidthEstimator, EstimatorStats, Gcc, PacketReport, Registry};
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::interceptor_registry::{
    configure_congestion_control, register_default_interceptors, CongestionFeedback,
};
use rtc::peer_connection::configuration::media_engine::MIME_TYPE_HEVC;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
    RtpCodecKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCConfigurationBuilder, RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};
use webrtc::rtp_transceiver::RtpSender;

use crate::server::AppState;
use crate::video;

const INITIAL_BITRATE: f64 = 3_000_000.0;
const MIN_BITRATE: f64 = 150_000.0;
const MAX_BITRATE: f64 = 20_000_000.0;
const MAX_SDP_BYTES: usize = 128 * 1024;
const MAX_CANDIDATE_BYTES: usize = 4096;
const MAX_VIDEO_ID_BYTES: usize = 64;

#[derive(Deserialize)]
pub(crate) struct VideoOffer {
    video_id: String,
    width: u32,
    height: u32,
    sdp: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum VideoSignal {
    Offer(VideoOffer),
    Ice {
        video_id: String,
        candidate: Option<RTCIceCandidateInit>,
    },
}

pub(crate) struct VideoSession {
    id: String,
    peer: Arc<dyn PeerConnection>,
}

impl VideoSession {
    pub(crate) async fn close(&self) {
        let _ = self.peer.close().await;
    }
}

#[derive(Clone)]
struct ConnectionHandler {
    video_id: String,
    signals: broadcast::Sender<Value>,
    connected: watch::Sender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for ConnectionHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        match event.candidate.to_json() {
            Ok(mut candidate) => {
                candidate.sdp_mid = None;
                let _ = self.signals.send(json!({
                    "type": "ice", "video_id": self.video_id, "candidate": candidate,
                }));
            }
            Err(error) => eprintln!("ICE candidate serialization failed: {error}"),
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.signals.send(json!({
                "type": "ice", "video_id": self.video_id, "candidate": null,
            }));
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
        let inner = Gcc::new(INITIAL_BITRATE, MIN_BITRATE, MAX_BITRATE);
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
        self.target
            .store(self.inner.target_bitrate() as u64, Ordering::Relaxed);
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
    state: &AppState,
    controller_id: &str,
    session: &mut Option<VideoSession>,
    message: VideoSignal,
    signals: &broadcast::Sender<Value>,
) -> anyhow::Result<Option<Value>> {
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
                state.clone(),
                controller_id.to_owned(),
                request.video_id.clone(),
                request.width & !1,
                request.height & !1,
                offer,
                signals.clone(),
            )
            .await?;

            *session = Some(next);

            Ok(Some(json!({
                "type": "answer", "video_id": request.video_id, "description": answer,
            })))
        }
        VideoSignal::Ice {
            video_id,
            candidate,
        } => {
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
    state: AppState,
    id: String,
    video_id: String,
    width: u32,
    height: u32,
    offer: RTCSessionDescription,
    signals: broadcast::Sender<Value>,
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
        ..Default::default()
    };
    let mut media_engine = MediaEngine::default();

    media_engine.register_codec(codec.clone(), RtpCodecKind::Video)?;

    let (estimator, target_bitrate) = ReportingEstimator::new();
    let registry = configure_congestion_control(
        Registry::new(),
        estimator,
        CongestionFeedback::Twcc,
        &mut media_engine,
    )?;
    let registry = register_default_interceptors(registry, &mut media_engine)?;
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: serde_json::from_str(include_str!("../shared/stun-servers.json"))?,
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
            signals,
            connected: connected_tx,
        }))
        .with_udp_addrs(vec!["0.0.0.0:0"])
        .build()
        .await?;
    let peer: Arc<dyn PeerConnection> = Arc::new(peer);
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
        let answer = peer
            .local_description()
            .await
            .ok_or_else(|| anyhow::anyhow!("missing local SDP"))?;

        Ok::<_, anyhow::Error>((sender, track, ssrc, answer))
    }
    .await;

    let (sender, track, ssrc, answer) = match setup {
        Ok(result) => result,
        Err(error) => {
            let _ = peer.close().await;
            return Err(error);
        }
    };
    let session = VideoSession {
        id: video_id,
        peer: peer.clone(),
    };

    let controller = state.controller.lock().await;

    if controller.as_deref() != Some(id.as_str()) {
        drop(controller);
        session.close().await;
        anyhow::bail!("Controller changed during negotiation");
    }

    state
        .video_changed
        .send_modify(|generation| *generation = generation.wrapping_add(1));

    let generation = *state.video_changed.borrow();

    drop(controller);

    tokio::spawn(async move {
        if let Err(error) = run_session(
            peer.clone(),
            sender,
            track,
            ssrc,
            target_bitrate,
            state,
            id,
            generation,
            connected,
            width,
            height,
        )
        .await
        {
            eprintln!("WebRTC video: {error:#}");
        }

        let _ = peer.close().await;
    });

    Ok((session, answer))
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    peer: Arc<dyn PeerConnection>,
    sender: Arc<dyn RtpSender>,
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    target_bitrate: Arc<AtomicU64>,
    state: AppState,
    id: String,
    generation: u64,
    mut connected: watch::Receiver<RTCPeerConnectionState>,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    let mut ownership = state.controller_changed.subscribe();
    let mut video_changed = state.video_changed.subscribe();
    let mut ready = state.ready.clone();

    loop {
        if ownership.borrow().as_deref() != Some(id.as_str())
            || *video_changed.borrow() != generation
            || !*ready.borrow()
        {
            peer.close().await?;
            return Ok(());
        }

        let connection_state = *connected.borrow_and_update();

        match connection_state {
            RTCPeerConnectionState::Connected => break,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                peer.close().await?;
                return Ok(());
            }
            _ => {}
        }

        tokio::select! {
            result = connected.changed() => { result?; }
            result = ownership.changed() => { result?; }
            result = video_changed.changed() => { result?; }
            result = ready.changed() => { result?; }
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
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut video_task = tokio::spawn(video::stream(
        state.frames.clone(),
        track,
        ssrc,
        payload_type,
        width,
        height,
        target_bitrate,
        stop_rx,
    ));

    loop {
        tokio::select! {
            result = &mut video_task => {
                result??;
                break;
            }
            result = ownership.changed() => {
                if result.is_err() || ownership.borrow_and_update().as_deref() != Some(id.as_str()) { break; }
            }
            result = video_changed.changed() => {
                if result.is_err() || *video_changed.borrow_and_update() != generation { break; }
            }
            result = ready.changed() => {
                if result.is_err() || !*ready.borrow_and_update() { break; }
            }
            result = connected.changed() => {
                if result.is_err() || matches!(*connected.borrow_and_update(), RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) { break; }
            }
        }
    }

    stop_tx.send_replace(true);

    if !video_task.is_finished() {
        let _ = video_task.await;
    }

    peer.close().await?;
    Ok(())
}
