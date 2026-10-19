use std::time::Duration;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use bytes::Bytes;
use futures_util::{Sink, SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc, watch};

use crate::cdp::{CdpSession, Command};
use crate::input;
use crate::protocol::{ClientMessage, FallbackCommand, ServerMessage, VideoErrorCode};
use crate::video::webrtc::{self, VideoSession};
use crate::video::websocket::WebSocketVideo;

pub(crate) mod controller;
use self::controller::Controller;

const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const SIGNAL_QUEUE_CAPACITY: usize = 128;
const CONTROL_QUEUE_CAPACITY: usize = 32;

struct ControlLease {
    controller: Controller,
    id: u64,
}

impl Drop for ControlLease {
    fn drop(&mut self) {
        self.controller.release(self.id);
    }
}

enum Video {
    WebRtc(VideoSession),
    WebSocket(WebSocketVideo),
}

impl Video {
    async fn close(self) {
        match self {
            Self::WebRtc(video) => video.close().await,
            Self::WebSocket(video) => video.close().await,
        }
    }
}

pub(crate) async fn viewer(mut socket: WebSocket, cdp: CdpSession, controller: Controller, takeover: bool) {
    let mut ready = cdp.ready.clone();
    let mut ownership = controller.subscribe();
    let rejection = if !*ready.borrow() {
        Some("cdp disconnected")
    } else {
        None
    };
    let id = if rejection.is_none() {
        controller.claim(takeover)
    } else {
        None
    };
    let Some(id) = id else {
        let _ = tokio::time::timeout(SEND_TIMEOUT, socket.send(close(rejection.unwrap_or("occupied")))).await;
        return;
    };
    let lease = ControlLease { controller, id };
    let (sink, mut source) = socket.split();
    let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
    let (video_tx, video_rx) = mpsc::channel(1);
    let (stop, stopped) = watch::channel(None);
    let mut writer = tokio::spawn(write_socket(sink, control_rx, video_rx, stopped));
    let (signals_tx, mut signals_rx) = broadcast::channel(SIGNAL_QUEUE_CAPACITY);
    let mut video = None::<Video>;
    let mut frames = cdp.frames.clone();
    let mut cursor = cdp.cursor.clone();
    let mut title = cdp.title.clone();

    let run = async {
        send(
            &control_tx,
            ServerMessage::Title {
                title: title.borrow_and_update().clone(),
            },
        )?;
        send(
            &control_tx,
            ServerMessage::Cursor {
                point: cursor.borrow_and_update().clone(),
            },
        )?;
        frames.mark_changed();

        let mut last_size = None;
        loop {
            tokio::select! {
                result = frames.changed() => {
                    result?;
                    let frame = frames.borrow_and_update().clone();

                    if let Some(frame) = frame {
                        let size = (frame.width, frame.height);

                        if last_size != Some(size) {
                            send(&control_tx, ServerMessage::Size { width: frame.width, height: frame.height })?;
                            last_size = Some(size);
                        }
                    }
                }
                result = cursor.changed() => {
                    result?;
                    send(&control_tx, ServerMessage::Cursor { point: cursor.borrow_and_update().clone() })?;
                }
                result = title.changed() => {
                    result?;
                    send(&control_tx, ServerMessage::Title { title: title.borrow_and_update().clone() })?;
                }
                message = signals_rx.recv() => {
                    let signal = message?;

                    if let Some(Video::WebRtc(current)) = &video {
                        if current.accepts(&signal) { send(&control_tx, signal.message)?; }
                    }
                }
                packet = async {
                    if let Some(Video::WebSocket(current)) = video.as_mut() {
                        // Reserve capacity before consuming a frame; cancellation must not lose an encoded frame.
                        let permit = video_tx.reserve().await?;
                        let packet = current.next().await?;

                        permit.send(packet);

                        Ok::<_, anyhow::Error>(())
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Err(error) = packet {
                        eprintln!("WebSocket video: {error:#}");
                        send(&control_tx, ServerMessage::VideoError { code: VideoErrorCode::Transport })?;

                        if let Some(current) = video.take() { current.close().await; }
                    }
                }
                message = source.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(message) = serde_json::from_str::<ClientMessage>(&text) else { continue; };

                            match message {
                                ClientMessage::Input(value) => {
                                    if let Some(input) = input::input_command(value) {
                                        cdp.commands.send(Command { owner: id, input }).await?;
                                    }
                                }
                                ClientMessage::Fallback(FallbackCommand::VideoAck { sequence }) => {
                                    if let Some(Video::WebSocket(current)) = video.as_mut() { current.acknowledge(sequence); }
                                }
                                ClientMessage::Fallback(FallbackCommand::Fallback) => {
                                    if !matches!(video, Some(Video::WebSocket(_))) {
                                        if let Some(current) = video.take() { current.close().await; }
                                        video = Some(Video::WebSocket(WebSocketVideo::start(cdp.frames.clone())));
                                        eprintln!("Video transport: WebSocket");
                                    }
                                }
                                ClientMessage::Video(message) => {
                                    if matches!(video, Some(Video::WebSocket(_))) { continue; }

                                    let mut current = match video.take() {
                                        Some(Video::WebRtc(current)) => Some(current),
                                        _ => None,
                                    };
                                    let result = webrtc::signal(cdp.frames.clone(), &mut current, message, &signals_tx).await;

                                    video = current.map(Video::WebRtc);

                                    match result {
                                        Ok(Some(answer)) => send(&control_tx, answer)?,
                                        Ok(None) => {},
                                        Err(error) => {
                                            eprintln!("WebRTC signaling: {error:#}");
                                            send(&control_tx, ServerMessage::VideoError { code: VideoErrorCode::Signaling })?;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => { control_tx.try_send(Message::Pong(data))?; }
                        Some(Ok(Message::Pong(_))) => {},
                        _ => return Ok::<_, anyhow::Error>(()),
                    }
                }
            }
        }
    };

    let reason = tokio::select! {
        result = run => {
            if let Err(error) = result { eprintln!("Control session: {error:#}"); }
            "session ended"
        }
        _ = ownership.wait_for(|current| *current != Some(id)) => "taken over",
        _ = ready.wait_for(|ready| !*ready) => "cdp disconnected",
        result = &mut writer => {
            if let Ok(Err(error)) = result { eprintln!("WebSocket writer: {error:#}"); }
            "writer stopped"
        }
    };

    // Revocation releases remote input immediately, independently of transport shutdown.
    drop(lease);
    stop.send_replace(Some(reason));

    if let Some(current) = video {
        current.close().await;
    }

    if !writer.is_finished() {
        let _ = writer.await;
    }

    eprintln!("Control session {id}: {reason}");
}

fn close(reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::POLICY,
        reason: reason.into(),
    }))
}

fn send(tx: &mpsc::Sender<Message>, message: ServerMessage) -> anyhow::Result<()> {
    tx.try_send(Message::Text(serde_json::to_string(&message)?.into()))?;
    Ok(())
}

async fn write_socket<S>(
    mut sink: S,
    mut control: mpsc::Receiver<Message>,
    mut video: mpsc::Receiver<Bytes>,
    mut stopped: watch::Receiver<Option<&'static str>>,
) -> anyhow::Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    loop {
        if stopped.borrow().is_some() {
            break;
        }

        let message = tokio::select! {
            biased;
            _ = stopped.changed() => break,
            Some(message) = control.recv() => message,
            Some(data) = video.recv() => Message::Binary(data),
            else => break,
        };

        tokio::select! {
            _ = stopped.changed() => break,
            result = tokio::time::timeout(SEND_TIMEOUT, sink.send(message)) => { result??; }
        }
    }

    let reason = (*stopped.borrow()).unwrap_or("session ended");

    let _ = tokio::time::timeout(Duration::from_secs(1), sink.send(close(reason))).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::sync::Notify;

    use super::*;

    struct SlowSink {
        messages: Arc<Mutex<Vec<Message>>>,
        sending: Arc<Notify>,
        blocked: bool,
    }

    impl Sink<Message> for SlowSink {
        type Error = std::io::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(mut self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
            self.blocked = matches!(message, Message::Binary(_));
            self.messages.lock().unwrap().push(message);
            self.sending.notify_one();
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if self.blocked {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn takeover_interrupts_blocked_video_send_and_control_has_priority() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sending = Arc::new(Notify::new());
        let sink = SlowSink {
            messages: messages.clone(),
            sending: sending.clone(),
            blocked: false,
        };
        let (control_tx, control_rx) = mpsc::channel(2);
        let (video_tx, video_rx) = mpsc::channel(1);
        let (stop, stopped) = watch::channel(None);
        video_tx.send(Bytes::from_static(b"video")).await.unwrap();
        control_tx.send(Message::Text("title".into())).await.unwrap();
        let task = tokio::spawn(write_socket(sink, control_rx, video_rx, stopped));
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                sending.notified().await;
                if messages.lock().unwrap().len() == 2 {
                    break;
                }
            }
        })
        .await
        .unwrap();
        stop.send_replace(Some("taken over"));
        tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap().unwrap();
        let messages = messages.lock().unwrap();
        assert!(matches!(&messages[0], Message::Text(text) if text.as_str() == "title"));
        assert!(matches!(&messages[1], Message::Binary(_)));
        assert!(matches!(&messages[2], Message::Close(Some(frame)) if frame.reason == "taken over"));
    }
}
