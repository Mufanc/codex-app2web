use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use tokio::sync::watch;

use super::{EncodedSample, EncoderSession, INITIAL_BITRATE, MAX_BITRATE, MIN_BITRATE};
use crate::cdp::Frame;

const MAX_IN_FLIGHT_FRAMES: usize = 3;
const MAX_IN_FLIGHT_BYTES: usize = 512 * 1024;
const RATE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);
const QUEUE_DELAY_TOLERANCE: Duration = Duration::from_millis(50);
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct WebSocketVideo {
    encoder: EncoderSession,
    bitrate: Arc<AtomicU64>,
    pending: VecDeque<(u32, Instant, usize)>,
    queued: Option<EncodedSample>,
    baseline_delay: Option<Duration>,
    last_rate_update: Instant,
    sequence: u32,
}

impl WebSocketVideo {
    pub(crate) fn start(frames: watch::Receiver<Option<Frame>>) -> Self {
        let bitrate = Arc::new(AtomicU64::new(INITIAL_BITRATE));
        let encoder = EncoderSession::start(frames, 4096, 2160, bitrate.clone());

        Self {
            encoder,
            bitrate,
            pending: VecDeque::new(),
            queued: None,
            baseline_delay: None,
            last_rate_update: Instant::now(),
            sequence: 0,
        }
    }

    fn can_send(&self, bytes: usize) -> bool {
        // A large keyframe may exceed the byte budget, but must be the only frame in flight.
        self.pending.is_empty()
            || (self.pending.len() < MAX_IN_FLIGHT_FRAMES
                && self.pending.iter().map(|entry| entry.2).sum::<usize>() + bytes <= MAX_IN_FLIGHT_BYTES)
    }

    pub(crate) async fn next(&mut self) -> anyhow::Result<Bytes> {
        let deadline = self.pending.front().map(|entry| entry.1 + ACK_TIMEOUT);

        if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
            anyhow::bail!("WebSocket video acknowledgment timed out");
        }

        let timeout = async {
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(deadline.into()).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::pin!(timeout);

        if self.queued.is_none() && self.can_send(0) {
            let sample = tokio::select! {
                _ = &mut timeout => anyhow::bail!("WebSocket video acknowledgment timed out"),
                sample = self.encoder.next() => sample?,
            };
            self.queued = Some(sample);
        }

        if self.queued.as_ref().is_none_or(|sample| !self.can_send(sample.data.len() + 9)) {
            timeout.await;
            anyhow::bail!("WebSocket video acknowledgment timed out");
        }

        let sample = self.queued.take().expect("frame ready above");

        self.sequence = self.sequence.wrapping_add(1);

        let packet = packet(self.sequence, sample);

        self.pending.push_back((self.sequence, Instant::now(), packet.len()));

        Ok(packet)
    }

    pub(crate) fn acknowledge(&mut self, sequence: u32) {
        let Some(index) = self.pending.iter().position(|entry| entry.0 == sequence) else {
            return;
        };
        let (_, sent, _) = self.pending.remove(index).expect("pending entry exists");
        let elapsed = sent.elapsed();
        let baseline = self.baseline_delay.map_or(elapsed, |baseline| baseline.min(elapsed));

        self.baseline_delay = Some(baseline);

        if self.last_rate_update.elapsed() < RATE_UPDATE_INTERVAL {
            return;
        }

        let current = self.bitrate.load(Ordering::Relaxed);
        // Compare with this connection's baseline; a distant client is not inherently congested.
        let next = if elapsed > baseline.mul_f64(1.5) + QUEUE_DELAY_TOLERANCE {
            current * 4 / 5
        } else if !self.pending.is_empty() {
            current + current / 20
        } else {
            current
        };

        self.bitrate.store(next.clamp(MIN_BITRATE, MAX_BITRATE), Ordering::Relaxed);
        self.last_rate_update = Instant::now();
    }

    pub(crate) async fn close(self) {
        if let Err(error) = self.encoder.close().await {
            eprintln!("WebSocket video task: {error:#}");
        }
    }
}

fn packet(sequence: u32, sample: EncodedSample) -> Bytes {
    // Big endian: sequence u32, width u16, height u16, keyframe u8, Annex B access unit.
    let mut bytes = BytesMut::with_capacity(9 + sample.data.len());

    bytes.put_u32(sequence);
    bytes.put_u16(sample.width as u16);
    bytes.put_u16(sample.height as u16);
    bytes.put_u8(u8::from(sample.keyframe));
    bytes.extend_from_slice(&sample.data);

    bytes.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn window_bounds_frames_bytes_and_times_out_even_when_not_full() {
        let (_frames, frames) = watch::channel(None);
        let mut video = WebSocketVideo::start(frames);
        for sequence in 1..=3 {
            assert!(video.can_send(10_000));
            video.pending.push_back((sequence, Instant::now(), 10_000));
        }
        assert!(!video.can_send(1));
        video.acknowledge(99);
        assert_eq!(video.pending.len(), 3);
        video.acknowledge(2);
        assert!(video.can_send(10_000));
        video.acknowledge(2);
        assert_eq!(video.pending.len(), 2);
        assert!(!video.can_send(MAX_IN_FLIGHT_BYTES));
        video.pending.clear();
        assert!(video.can_send(MAX_IN_FLIGHT_BYTES * 2));
        video.pending.push_back((9, Instant::now(), MAX_IN_FLIGHT_BYTES * 2));
        assert!(!video.can_send(1));
        video.pending.clear();
        video.pending.push_back((10, Instant::now() - ACK_TIMEOUT, 10));
        assert!(video.next().await.is_err());
        video.close().await;
    }

    #[tokio::test]
    async fn stable_high_latency_does_not_cut_bitrate_but_queue_growth_does() {
        let (_frames, frames) = watch::channel(None);
        let mut video = WebSocketVideo::start(frames);
        video.last_rate_update = Instant::now() - RATE_UPDATE_INTERVAL;
        video.pending.push_back((1, Instant::now() - Duration::from_millis(300), 10_000));
        video.acknowledge(1);
        assert_eq!(video.bitrate.load(Ordering::Relaxed), INITIAL_BITRATE);
        video.last_rate_update = Instant::now() - RATE_UPDATE_INTERVAL;
        video.pending.push_back((2, Instant::now() - Duration::from_millis(800), 10_000));
        video.acknowledge(2);
        assert!(video.bitrate.load(Ordering::Relaxed) < INITIAL_BITRATE);
        video.close().await;
    }

    #[test]
    fn packet_preserves_frame_boundaries_and_resize_metadata() {
        let data = packet(
            42,
            EncodedSample {
                data: vec![0, 0, 0, 1, 0x26],
                duration: Duration::ZERO,
                width: 1280,
                height: 720,
                keyframe: true,
            },
        );
        assert_eq!(&data[..9], &[0, 0, 0, 42, 5, 0, 2, 208, 1]);
        assert_eq!(&data[9..], &[0, 0, 0, 1, 0x26]);
    }
}
