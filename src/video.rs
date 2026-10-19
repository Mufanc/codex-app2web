use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use image::imageops::{self, FilterType};
use image::{ImageBuffer, Rgb, RgbImage};
use tokio::sync::{mpsc, watch};
use video_toolbox::{
    CodecConfig, EncodeOptions, EncodedFrame, Encoder, EncoderConfig, FrameData, HevcEncoderConfig, HevcProfile,
    PixelFormat,
};

use crate::cdp::Frame;

pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(67);
const FRAME_RATE: u32 = 15;
const KEYFRAME_INTERVAL: u32 = 60;
const ENCODE_TIMEOUT: Duration = Duration::from_millis(500);
pub(super) const MIN_BITRATE: u64 = 150_000;
pub(super) const MAX_BITRATE: u64 = 20_000_000;

pub(crate) struct EncodedSample {
    pub(crate) data: Vec<u8>,
    pub(crate) duration: Duration,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) keyframe: bool,
}

pub(crate) mod webrtc;
pub(crate) mod websocket;

pub(super) const INITIAL_BITRATE: u64 = 3_000_000;

pub(super) struct EncoderSession {
    samples: mpsc::Receiver<EncodedSample>,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl EncoderSession {
    pub(super) fn start(
        frames: watch::Receiver<Option<Frame>>,
        width: u32,
        height: u32,
        bitrate: Arc<AtomicU64>,
    ) -> Self {
        let (tx, samples) = mpsc::channel(1);
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(stream(frames, tx, width, height, bitrate, stopped));

        Self {
            samples,
            stop,
            task: Some(task),
        }
    }

    pub(super) async fn next(&mut self) -> anyhow::Result<EncodedSample> {
        if let Some(sample) = self.samples.recv().await {
            return Ok(sample);
        }

        if let Some(task) = self.task.as_mut() {
            let result = task.await;
            self.task = None;
            result??;
        }

        anyhow::bail!("Encoder stopped")
    }

    pub(super) async fn close(mut self) -> anyhow::Result<()> {
        self.stop.send_replace(true);
        drop(self.samples);

        if let Some(task) = self.task.take() {
            task.await??;
        }

        Ok(())
    }
}

async fn stream(
    frames: watch::Receiver<Option<Frame>>,
    output: mpsc::Sender<EncodedSample>,
    max_width: u32,
    max_height: u32,
    target_bitrate: Arc<AtomicU64>,
    stop: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (jpeg_tx, jpeg_rx) = mpsc::channel(1);
    let (sample_tx, mut sample_rx) = mpsc::channel::<EncodedSample>(1);
    let worker = tokio::task::spawn_blocking(move || encode(jpeg_rx, sample_tx, max_width, max_height, target_bitrate));
    let mut ticker = tokio::time::interval(FRAME_INTERVAL);
    let mut frames = frames;

    frames.mark_changed();

    let mut stop = stop;

    let result = async {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = ticker.tick() => {
                    if frames.has_changed().unwrap_or(false) {
                        let frame = frames.borrow_and_update().clone();
                        if let Some(frame) = frame {
                            if jpeg_tx.try_send(frame.jpeg).is_err() {
                                frames.mark_changed();
                            }
                        }
                    }
                }
                sample = sample_rx.recv() => {
                    let Some(sample) = sample else { break };

                    tokio::select! {
                        _ = stop.changed() => break,
                        result = output.send(sample) => { if result.is_err() { break; } }
                    }
                }
            }
        }

        Ok::<_, anyhow::Error>(())
    }
    .await;

    drop(jpeg_tx);
    drop(sample_rx);
    worker.await??;

    result
}

fn encode(
    mut jpeg_rx: mpsc::Receiver<Bytes>,
    sample_tx: mpsc::Sender<EncodedSample>,
    max_width: u32,
    max_height: u32,
    target_bitrate: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    let mut encoding: Option<(Encoder, EncoderConfig)> = None;
    let mut last_bitrate_change = Instant::now();
    let mut previous_frame = None;

    while let Some(jpeg) = jpeg_rx.blocking_recv() {
        let Ok(image) = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg) else {
            continue;
        };

        let image = image.to_rgb8();
        let (width, height) = encoder_size(image.dimensions(), (max_width, max_height));

        if width < 16 || height < 16 {
            continue;
        }

        let target = target_bitrate.load(Ordering::Relaxed).clamp(MIN_BITRATE, MAX_BITRATE);
        let mut force_key_frame = false;

        if encoding.is_none() {
            let config = encoder_config(width, height, target);
            encoding = Some((Encoder::new(config.clone())?, config));
            force_key_frame = true;
        }

        let (encoder, config) = encoding.as_mut().expect("encoder initialized above");
        let current = config.average_bitrate.unwrap_or(target);

        if config.width != width
            || config.height != height
            || (target.abs_diff(current) > current / 4 && last_bitrate_change.elapsed() >= Duration::from_secs(2))
        {
            config.width = width;
            config.height = height;
            config.average_bitrate = Some(target);
            encoder.reconfigure(config.clone())?;
            last_bitrate_change = Instant::now();
            force_key_frame = true;
        }

        let (rgb, rgb_stride) = prepare_rgb(image, width, height);
        let mut yuv = yuv::YuvPlanarImageMut::<u8>::alloc(width, height, yuv::YuvChromaSubsampling::Yuv420);

        yuv::rgb_to_yuv420(
            &mut yuv,
            rgb.as_raw(),
            rgb_stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt709,
            yuv::YuvConversionMode::Balanced,
        )?;
        encoder.encode(
            &FrameData::I420 {
                y: yuv.y_plane.borrow(),
                u: yuv.u_plane.borrow(),
                v: yuv.v_plane.borrow(),
            },
            &EncodeOptions { force_key_frame },
        )?;

        let deadline = Instant::now() + ENCODE_TIMEOUT;
        let frame = loop {
            if let Some(frame) = encoder.next_frame()? {
                break frame;
            }

            if Instant::now() >= deadline {
                anyhow::bail!("VideoToolbox did not return an encoded frame");
            }

            std::thread::sleep(Duration::from_millis(1));
        };

        let now = Instant::now();
        let duration = previous_frame.map_or(FRAME_INTERVAL, |then: Instant| now.duration_since(then));

        previous_frame = Some(now);

        let keyframe = frame.keyframe;
        let data = annex_b(frame)?;

        if sample_tx
            .blocking_send(EncodedSample {
                data,
                duration,
                width,
                height,
                keyframe,
            })
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

fn encoder_size(source: (u32, u32), limit: (u32, u32)) -> (u32, u32) {
    if (source.0 & !1) <= limit.0 && (source.1 & !1) <= limit.1 {
        return (source.0 & !1, source.1 & !1);
    }

    let scale = f64::min(limit.0 as f64 / source.0 as f64, limit.1 as f64 / source.1 as f64);

    (
        ((source.0 as f64 * scale) as u32) & !1,
        ((source.1 as f64 * scale) as u32) & !1,
    )
}

fn prepare_rgb(image: RgbImage, width: u32, height: u32) -> (RgbImage, u32) {
    let (src_width, src_height) = image.dimensions();

    if (src_width & !1, src_height & !1) == (width, height) {
        return (image, src_width * 3);
    }

    let scale = f64::min(width as f64 / src_width as f64, height as f64 / src_height as f64);
    let scaled_width = ((src_width as f64 * scale) as u32).max(1);
    let scaled_height = ((src_height as f64 * scale) as u32).max(1);
    let scaled = imageops::resize(&image, scaled_width, scaled_height, FilterType::Triangle);
    let mut canvas = ImageBuffer::from_pixel(width, height, Rgb([0, 0, 0]));

    imageops::overlay(
        &mut canvas,
        &scaled,
        ((width - scaled_width) / 2) as i64,
        ((height - scaled_height) / 2) as i64,
    );

    (canvas, width * 3)
}

fn encoder_config(width: u32, height: u32, bitrate: u64) -> EncoderConfig {
    EncoderConfig {
        width,
        height,
        codec: CodecConfig::Hevc(HevcEncoderConfig {
            profile: HevcProfile::Main,
            allow_open_gop: false,
        }),
        pixel_format: PixelFormat::I420,
        average_bitrate: Some(bitrate.clamp(MIN_BITRATE, MAX_BITRATE)),
        fps_numerator: FRAME_RATE,
        fps_denominator: 1,
        prioritize_encoding_speed_over_quality: true,
        real_time: true,
        maximize_power_efficiency: false,
        allow_frame_reordering: false,
        allow_temporal_compression: true,
        max_key_frame_interval: std::num::NonZeroU32::new(KEYFRAME_INTERVAL),
        max_key_frame_interval_duration: None,
        max_frame_delay_count: std::num::NonZeroU32::new(1),
    }
}

fn annex_b(frame: EncodedFrame) -> anyhow::Result<Vec<u8>> {
    let parameters = if frame.keyframe {
        frame
            .vps_list
            .iter()
            .chain(&frame.sps_list)
            .chain(&frame.pps_list)
            .map(Vec::as_slice)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    avcc_to_annex_b(&frame.data, &parameters)
}

fn avcc_to_annex_b(data: &[u8], parameters: &[&[u8]]) -> anyhow::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(data.len() + 256);

    for parameter in parameters {
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(parameter);
    }

    let mut remaining = data;

    while !remaining.is_empty() {
        if remaining.len() < 4 {
            anyhow::bail!("truncated HEVC NAL length");
        }

        let length = u32::from_be_bytes(remaining[..4].try_into()?) as usize;

        remaining = &remaining[4..];

        if length == 0 || length > remaining.len() {
            anyhow::bail!("invalid HEVC NAL length");
        }

        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(&remaining[..length]);
        remaining = &remaining[length..];
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_tracks_capture_size_without_upscaling() {
        assert_eq!(encoder_size((2560, 1405), (2560, 1404)), (2560, 1404));
        assert_eq!(encoder_size((1001, 701), (1920, 1080)), (1000, 700));
        assert_eq!(encoder_size((2560, 1405), (1280, 720)), (1280, 702));
    }

    #[test]
    fn one_extra_capture_row_does_not_resample_the_frame() {
        let image = ImageBuffer::from_pixel(16, 17, Rgb([128, 128, 128]));
        let (rgb, stride) = prepare_rgb(image, 16, 16);
        assert_eq!(rgb.dimensions(), (16, 17));
        assert_eq!(stride, 48);
        let mut yuv = yuv::YuvPlanarImageMut::<u8>::alloc(16, 16, yuv::YuvChromaSubsampling::Yuv420);
        yuv::rgb_to_yuv420(
            &mut yuv,
            rgb.as_raw(),
            stride,
            yuv::YuvRange::Limited,
            yuv::YuvStandardMatrix::Bt709,
            yuv::YuvConversionMode::Balanced,
        )
        .unwrap();
    }

    #[test]
    fn hevc_parameter_sets_precede_frame_in_annex_b() {
        let data = [0, 0, 0, 2, 0x26, 1];
        let parameters: [&[u8]; 3] = [&[0x40, 1], &[0x42, 1], &[0x44, 1]];
        assert_eq!(
            avcc_to_annex_b(&data, &parameters).unwrap(),
            [0, 0, 0, 1, 0x40, 1, 0, 0, 0, 1, 0x42, 1, 0, 0, 0, 1, 0x44, 1, 0, 0, 0, 1, 0x26, 1,]
        );
        assert!(avcc_to_annex_b(&[0, 0, 0, 3, 1], &[]).is_err());
    }

    #[test]
    fn video_toolbox_encodes_hevc_keyframe() {
        let (width, height) = (1280, 720);
        let mut config = encoder_config(width, height, 1_000_000);
        let mut encoder = Encoder::new(config.clone()).unwrap();
        let y = vec![16; (width * height) as usize];
        let uv = vec![128; (width * height / 4) as usize];
        let image = FrameData::I420 { y: &y, u: &uv, v: &uv };
        encoder.encode(&image, &EncodeOptions { force_key_frame: true }).unwrap();
        let deadline = Instant::now() + ENCODE_TIMEOUT;
        let frame = loop {
            if let Some(frame) = encoder.next_frame().unwrap() {
                break frame;
            }
            assert!(Instant::now() < deadline, "VideoToolbox did not produce HEVC");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(frame.keyframe);
        assert!(!frame.vps_list.is_empty());
        assert!(!frame.sps_list.is_empty());
        assert!(!frame.pps_list.is_empty());
        assert!(!annex_b(frame).unwrap().is_empty());

        config.average_bitrate = Some(500_000);
        encoder.reconfigure(config.clone()).unwrap();
        encoder.encode(&image, &EncodeOptions::default()).unwrap();
        let deadline = Instant::now() + ENCODE_TIMEOUT;
        loop {
            if let Some(frame) = encoder.next_frame().unwrap() {
                assert!(!annex_b(frame).unwrap().is_empty());
                break;
            }
            assert!(Instant::now() < deadline, "reconfigured encoder produced no frame");
            std::thread::sleep(Duration::from_millis(1));
        }

        config.width = 640;
        config.height = 360;
        encoder.reconfigure(config).unwrap();
        let resized_y = vec![16; 640 * 360];
        let resized_uv = vec![128; 640 * 360 / 4];
        encoder
            .encode(
                &FrameData::I420 {
                    y: &resized_y,
                    u: &resized_uv,
                    v: &resized_uv,
                },
                &EncodeOptions { force_key_frame: true },
            )
            .unwrap();
        let deadline = Instant::now() + ENCODE_TIMEOUT;
        loop {
            if let Some(frame) = encoder.next_frame().unwrap() {
                assert!(frame.keyframe);
                assert!(!annex_b(frame).unwrap().is_empty());
                break;
            }
            assert!(Instant::now() < deadline, "resized encoder produced no frame");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
