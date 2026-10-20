use std::ffi::{c_char, c_void, CStr};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use image::RgbaImage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use tracing::{debug, trace, Instrument, Span};

use crate::cdp::Frame;

// TigerVNC configuration and timers are process globals. Only the controller streams.
static CORE: Mutex<()> = Mutex::new(());
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_FRAME_WIDTH: u32 = 4096;
const MAX_FRAME_HEIGHT: u32 = 2160;

unsafe extern "C" {
    fn vnc_create(fd: i32) -> *mut c_void;
    fn vnc_frame(handle: *mut c_void, rgba: *const u8, width: i32, height: i32) -> bool;
    fn vnc_poll(handle: *mut c_void) -> bool;
    fn vnc_destroy(handle: *mut c_void);
    fn vnc_error() -> *const c_char;
}

struct Native(*mut c_void);

impl Drop for Native {
    fn drop(&mut self) {
        // The handle is created, used and destroyed on the same worker thread.
        unsafe { vnc_destroy(self.0) };
    }
}

pub(crate) struct VncVideo {
    pub(super) stream: tokio::net::UnixStream,
    stop: Arc<AtomicBool>,
    worker: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl VncVideo {
    pub(crate) fn start(frames: watch::Receiver<Option<Frame>>) -> anyhow::Result<Self> {
        let (client, server) = UnixStream::pair()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let span = Span::current();

        client.set_nonblocking(true)?;
        server.set_nonblocking(true)?;

        let stream = tokio::net::UnixStream::from_std(client)?;
        let worker = tokio::task::spawn_blocking(move || span.in_scope(|| run(server, frames, stopped)));

        Ok(Self {
            stream,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) async fn next(&mut self) -> anyhow::Result<Bytes> {
        let mut buffer = vec![0; 64 * 1024];
        let count = self.stream.read(&mut buffer).await?;

        if count == 0 {
            if let Some(worker) = self.worker.take() {
                worker.await??;
            }

            anyhow::bail!("VNC stream ended");
        }

        buffer.truncate(count);

        Ok(buffer.into())
    }

    pub(crate) async fn receive(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(1), self.stream.write_all(bytes)).await??;
        Ok(())
    }

    pub(crate) async fn close(mut self) {
        self.stop.store(true, Ordering::Release);

        if let Some(worker) = self.worker.take() {
            let _ = worker.instrument(Span::current()).await;
        }
    }
}

impl Drop for VncVideo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn native_error() -> anyhow::Error {
    // The C++ bridge catches exceptions and retains the message on this worker thread.
    let message = unsafe { CStr::from_ptr(vnc_error()) }.to_string_lossy();
    anyhow::anyhow!("TigerVNC: {message}")
}

fn run(stream: UnixStream, frames: watch::Receiver<Option<Frame>>, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    let _guard = CORE.lock().map_err(|_| anyhow::anyhow!("VNC worker lock poisoned"))?;

    if stop.load(Ordering::Acquire) {
        return Ok(());
    }

    // PNG decompression must not delay RFB requests, fence replies or socket flushing.
    std::thread::scope(|scope| {
        let (images, decoded) = watch::channel(None);
        let decoder_stop = stop.clone();
        let span = Span::current();

        scope.spawn(move || span.in_scope(|| decode(frames, images, decoder_stop)));

        run_native(stream, decoded, stop)
    })
}

type DecodedImage = Result<Arc<RgbaImage>, String>;

fn decode(
    mut frames: watch::Receiver<Option<Frame>>,
    images: watch::Sender<Option<DecodedImage>>,
    stop: Arc<AtomicBool>,
) {
    frames.mark_changed();

    while !stop.load(Ordering::Acquire) && !images.is_closed() {
        if frames.has_changed().unwrap_or(false) {
            let frame = frames.borrow_and_update().clone();

            if let Some(frame) = frame {
                let started = Instant::now();
                let image = image::load_from_memory_with_format(&frame.image, image::ImageFormat::Png)
                    .map(|image| Arc::new(image.into_rgba8()))
                    .map_err(|error| error.to_string());

                trace!(
                    elapsed_ms = started.elapsed().as_millis(),
                    bytes = frame.image.len(),
                    "VNC PNG decoded"
                );
                images.send_replace(Some(image));
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn run_native(
    stream: UnixStream,
    mut images: watch::Receiver<Option<DecodedImage>>,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    // C++ duplicates this descriptor; the Rust endpoint remains owned until the worker exits.
    let native = Native(unsafe { vnc_create(stream.as_raw_fd()) });

    if native.0.is_null() {
        return Err(native_error());
    }

    while !stop.load(Ordering::Acquire) {
        if images.has_changed().unwrap_or(false) {
            let image = images.borrow_and_update().clone();

            if let Some(image) = image {
                let image = image.map_err(anyhow::Error::msg)?;
                let (width, height) = image.dimensions();

                anyhow::ensure!(
                    width > 0 && height > 0 && width <= MAX_FRAME_WIDTH && height <= MAX_FRAME_HEIGHT,
                    "Invalid VNC framebuffer dimensions: {width}x{height}"
                );

                // The bridge copies the pixels before returning; no Rust allocation crosses the ABI.
                if !unsafe { vnc_frame(native.0, image.as_ptr(), width as i32, height as i32) } {
                    return Err(native_error());
                }
            }
        }

        let started = Instant::now();

        if !unsafe { vnc_poll(native.0) } {
            if stop.load(Ordering::Acquire) {
                break;
            }

            return Err(native_error());
        }

        if started.elapsed() > Duration::from_millis(50) {
            debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "VNC encoding delayed protocol processing"
            );
        }
    }

    debug!("VNC worker stopped");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decode_failure_and_consumer_shutdown_do_not_leave_a_worker_running() {
        let (_frames, frames) = watch::channel(Some(Frame {
            image: Bytes::from_static(b"invalid png"),
            width: 640.0,
            height: 480.0,
        }));
        let (images, mut decoded) = watch::channel(None);
        let stop = Arc::new(AtomicBool::new(false));
        let worker = tokio::task::spawn_blocking(move || decode(frames, images, stop));
        tokio::time::timeout(Duration::from_secs(1), decoded.changed()).await.unwrap().unwrap();
        assert!(decoded.borrow_and_update().as_ref().unwrap().is_err());
        drop(decoded);
        tokio::time::timeout(Duration::from_secs(1), worker).await.unwrap().unwrap();
    }
}
