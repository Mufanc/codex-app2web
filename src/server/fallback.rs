use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{BufMut, Bytes, BytesMut};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::cdp::Frame;
use crate::video::{self, EncodedSample, Output};

const INITIAL_BITRATE: u64 = 3_000_000;
const MIN_BITRATE: u64 = 150_000;
const MAX_BITRATE: u64 = 20_000_000;
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct Fallback {
    samples: mpsc::Receiver<EncodedSample>,
    stop: watch::Sender<bool>,
    task: JoinHandle<anyhow::Result<()>>,
    bitrate: Arc<AtomicU64>,
    pending: Option<(u32, Instant, usize)>,
    sequence: u32,
}

impl Fallback {
    pub(super) fn start(frames: watch::Receiver<Option<Frame>>) -> Self {
        let (tx, samples) = mpsc::channel(1);
        let (stop, stopped) = watch::channel(false);
        let bitrate = Arc::new(AtomicU64::new(INITIAL_BITRATE));
        let task = tokio::spawn(video::stream(
            frames,
            Output::WebSocket(tx),
            4096,
            2160,
            bitrate.clone(),
            stopped,
        ));

        Self {
            samples,
            stop,
            task,
            bitrate,
            pending: None,
            sequence: 0,
        }
    }

    pub(super) async fn next(&mut self) -> anyhow::Result<Bytes> {
        if let Some((_, sent, _)) = self.pending {
            tokio::time::sleep_until((sent + ACK_TIMEOUT).into()).await;
            anyhow::bail!("WebSocket video acknowledgment timed out");
        }

        let sample = self
            .samples
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("WebSocket encoder stopped"))?;

        self.sequence = self.sequence.wrapping_add(1);

        let packet = packet(self.sequence, sample);

        self.pending = Some((self.sequence, Instant::now(), packet.len()));

        Ok(packet)
    }

    pub(super) fn acknowledge(&mut self, sequence: u32) {
        if let Some((expected, sent, bytes)) = self.pending {
            if sequence != expected {
                return;
            }

            self.pending = None;

            // Delivery plus decode latency bounds the amount of video we can sustain over TCP.
            let elapsed = sent.elapsed().as_secs_f64().max(0.001);
            let current = self.bitrate.load(Ordering::Relaxed);
            let next = if elapsed > 0.15 {
                ((bytes as f64 * 8.0 / elapsed * 0.8) as u64).min(current * 4 / 5)
            } else {
                current + current / 20
            };

            self.bitrate
                .store(next.clamp(MIN_BITRATE, MAX_BITRATE), Ordering::Relaxed);
        }
    }

    pub(super) async fn close(self) {
        self.stop.send_replace(true);
        drop(self.samples);

        if let Err(error) = self.task.await {
            eprintln!("WebSocket video task: {error}");
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
    async fn acknowledgments_bound_backlog_and_reduce_bitrate_on_slow_delivery() {
        let (_frames, frames) = watch::channel(None);
        let mut video = Fallback::start(frames);
        video.pending = Some((7, Instant::now() - Duration::from_millis(500), 10_000));
        video.acknowledge(6);
        assert!(video.pending.is_some());
        video.acknowledge(7);
        assert!(video.pending.is_none());
        assert!(video.bitrate.load(Ordering::Relaxed) < INITIAL_BITRATE);

        video.pending = Some((8, Instant::now() - ACK_TIMEOUT, 10_000));
        assert!(video.next().await.is_err());
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
