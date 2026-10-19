import { createWebSocketVideo } from './websocket-video.ts';

import stunServers from '../../shared/stun-servers.json' with { type: 'json' };

export type VideoTransport = 'WebRTC' | 'WebSocket';

export type ConnectionState = 'connecting' | 'streaming' | 'occupied' | 'disconnected' | 'unsupported' | 'error';

type VideoSignal =
    | { type: 'answer'; video_id: string; description: RTCSessionDescriptionInit }
    | { type: 'ice'; video_id: string; candidate: RTCIceCandidateInit | null };

type Point = { x: number; y: number };
type Picture = { left: number; top: number; width: number; height: number };

const MAX_VIDEO_WIDTH = 4096;
const MAX_VIDEO_HEIGHT = 2160;
const VIDEO_CONNECT_TIMEOUT_MS = 8_000;
const FALLBACK_TIMEOUT_MS = 15_000;

export function startRemote(
    stage: HTMLElement,
    screen: HTMLVideoElement,
    keyboard: HTMLTextAreaElement,
    onState: (state: ConnectionState, detail?: string) => void,
    onSize: (width: number, height: number) => void,
    onTransport: (transport: VideoTransport | null) => void = () => {},
) {
    const sessionId = crypto.randomUUID();
    const pressed = new Set<string>();

    let sourceWidth = 0;
    let sourceHeight = 0;
    let sentViewport: [number, number] | null = null;
    let videoStarted = false;
    let decodedSize: [number, number] | null = null;
    let socket: WebSocket | undefined;
    let videoAbort: AbortController | undefined;
    let receiveVideoFrame: ((data: ArrayBuffer) => void) | undefined;
    let receiveVideoSignal: ((message: VideoSignal) => void) | undefined;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
    let composing = false;
    let disposed = false;

    function pictureRect(): Picture | null {
        const size = decodedSize;

        if (!size || !sourceWidth || !sourceHeight) return null;

        const area = stage.getBoundingClientRect();
        const videoScale = Math.min(area.width / size[0], area.height / size[1]);
        const width = size[0] * videoScale;
        const height = size[1] * videoScale;

        return { left: (area.width - width) / 2, top: (area.height - height) / 2, width, height };
    }

    function coordinates(event: PointerEvent | WheelEvent, clamp = false): Point | null {
        const rect = pictureRect();

        if (!rect) return null;

        const area = stage.getBoundingClientRect();
        const x = event.clientX - area.left - rect.left;
        const y = event.clientY - area.top - rect.top;

        if (!clamp && (x < 0 || y < 0 || x > rect.width || y > rect.height)) return null;

        return {
            x: Math.max(0, Math.min(rect.width, x)) / rect.width * sourceWidth,
            y: Math.max(0, Math.min(rect.height, y)) / rect.height * sourceHeight,
        };
    }

    function placeKeyboard(point: Point | null) {
        const rect = pictureRect();

        if (!rect || !point) return;

        keyboard.style.left = `${Math.max(0, Math.min(stage.clientWidth - 2, rect.left + point.x / sourceWidth * rect.width))}px`;
        keyboard.style.top = `${Math.max(0, Math.min(stage.clientHeight - 20, rect.top + point.y / sourceHeight * rect.height))}px`;
    }

    function onScreenResize() {
        decodedSize = screen.videoWidth && screen.videoHeight ? [screen.videoWidth, screen.videoHeight] : null;
    }

    screen.addEventListener('resize', onScreenResize);

    function send(message: object) {
        if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
    }

    function resizeVideo(immediate = false) {
        clearTimeout(resizeTimer);

        resizeTimer = setTimeout(() => {
            if (socket?.readyState !== WebSocket.OPEN) return;

            const viewportWidth = Math.max(320, Math.min(MAX_VIDEO_WIDTH, Math.round(stage.clientWidth * devicePixelRatio)));
            const viewportHeight = Math.max(240, Math.min(MAX_VIDEO_HEIGHT, Math.round(stage.clientHeight * devicePixelRatio)));

            if (!sentViewport || sentViewport[0] !== viewportWidth || sentViewport[1] !== viewportHeight) {
                send({ type: 'resize', width: viewportWidth, height: viewportHeight });
                sentViewport = [viewportWidth, viewportHeight];
            }

            if (sourceWidth && sourceHeight) void startVideo();
        }, immediate ? 0 : 1000);
    }

    const resizeObserver = new ResizeObserver(() => resizeVideo());

    resizeObserver.observe(stage);

    function startFallback() {
        videoAbort?.abort();
        videoAbort = new AbortController();
        videoStarted = true;
        const signal = videoAbort.signal;
        onTransport('WebSocket');
        onState('connecting', '正在切换 WebSocket');
        const timeout = setTimeout(() => fail('WebSocket 视频连接超时'), FALLBACK_TIMEOUT_MS);

        function fail(message: string) {
            if (signal.aborted) return;
            videoAbort?.abort();
            onState('error', message);
        }

        signal.addEventListener('abort', () => {
            clearTimeout(timeout);
            receiveVideoFrame = undefined;
            decodedSize = null;
        }, { once: true });

        try {
            receiveVideoFrame = createWebSocketVideo(screen, signal, (width, height) => {
                clearTimeout(timeout);
                decodedSize = [width, height];
                onState('streaming');
            }, sequence => send({ type: 'video_ack', sequence }), fail);
            send({ type: 'fallback' });
        } catch (error) {
            fail(String(error));
        }
    }

    async function startVideo() {
        if (videoStarted && !videoAbort?.signal.aborted) return;

        videoAbort?.abort();
        videoAbort = new AbortController();

        const signal = videoAbort.signal;
        const hevc = window.RTCPeerConnection
            ? RTCRtpReceiver.getCapabilities('video')?.codecs.filter(codec => codec.mimeType.toLowerCase() === 'video/h265')
            : undefined;

        if (!hevc?.length) {
            startFallback();

            return;
        }

        onTransport('WebRTC');
        onState('connecting', '正在连接 WebRTC');

        const peer = new RTCPeerConnection({
            iceServers: [{ urls: stunServers }],
        });

        const videoId = crypto.randomUUID();
        const pendingCandidates: (RTCIceCandidateInit | null)[] = [];
        let offerSent = false;
        let hasUdpCandidate = false;
        let incomingSignals = Promise.resolve();
        const timeout = setTimeout(() => fail('连接超时，请重试'), VIDEO_CONNECT_TIMEOUT_MS);

        function fail(message: string) {
            if (signal.aborted) return;

            console.info(`${message}; switching to WebSocket video`);
            startFallback();
        }

        signal.addEventListener('abort', () => {
            clearTimeout(timeout);
            receiveVideoSignal = undefined;
            peer.close();
            screen.srcObject = null;
            decodedSize = null;
        }, { once: true });

        peer.onicecandidate = event => {
            if (signal.aborted) return;

            const candidate = event.candidate?.toJSON() ?? null;

            if (candidate && / udp /i.test(candidate.candidate ?? '')) hasUdpCandidate = true;
            if (!candidate && !hasUdpCandidate) {
                fail('浏览器未提供 UDP 候选');
                return;
            }

            if (offerSent) send({ type: 'ice', video_id: videoId, candidate });
            else pendingCandidates.push(candidate);
        };

        receiveVideoSignal = message => {
            if (signal.aborted || message.video_id !== videoId) return;

            // Candidates must wait for the preceding answer to finish being applied.
            incomingSignals = incomingSignals.then(async () => {
                if (signal.aborted) return;

                if (message.type === 'answer') await peer.setRemoteDescription(message.description);
                else await peer.addIceCandidate(message.candidate ?? undefined);
            }).catch(error => fail(`WebRTC 信令失败：${String(error)}`));
        };

        videoStarted = true;

        const transceiver = peer.addTransceiver('video', { direction: 'recvonly' });

        transceiver.setCodecPreferences(hevc);

        peer.ontrack = event => {
            screen.srcObject = event.streams[0] ?? new MediaStream([event.track]);

            screen.requestVideoFrameCallback(() => {
                if (!signal.aborted) {
                    clearTimeout(timeout);
                    onScreenResize();
                    onState('streaming');
                }
            });

            void screen.play().catch(error => {
                fail(`视频播放失败：${String(error)}`);
            });
        };

        peer.onconnectionstatechange = () => {
            if (peer.connectionState === 'failed' && !signal.aborted) {
                fail('WebRTC 连接失败，可能无法 UDP 打洞');
            }
        };

        try {
            const offer = await peer.createOffer();

            await peer.setLocalDescription(offer);

            if (signal.aborted) return;

            send({
                type: 'offer', video_id: videoId,
                width: MAX_VIDEO_WIDTH, height: MAX_VIDEO_HEIGHT,
                sdp: offer.sdp,
            });
            offerSent = true;

            for (const candidate of pendingCandidates) {
                send({ type: 'ice', video_id: videoId, candidate });
            }

            pendingCandidates.length = 0;
        } catch (error) {
            fail(`WebRTC 视频连接失败：${error instanceof Error ? error.message : String(error)}`);
        }
    }

    function modifiers(event: KeyboardEvent) {
        return (event.altKey ? 1 : 0) | (event.ctrlKey ? 2 : 0) | (event.metaKey ? 4 : 0) | (event.shiftKey ? 8 : 0);
    }

    function onKeyDown(event: KeyboardEvent) {
        if (event.isComposing || composing || event.key === 'Process' || event.key === 'Dead') return;
        if (!(event.metaKey || event.ctrlKey || event.altKey || event.key.length !== 1)) return;

        event.preventDefault();
        pressed.add(event.code);

        send({ type: 'key', action: 'keyDown', key: event.key, code: event.code, key_code: event.keyCode, modifiers: modifiers(event) });
    }

    function onKeyUp(event: KeyboardEvent) {
        if (!pressed.delete(event.code)) return;

        event.preventDefault();
        send({ type: 'key', action: 'keyUp', key: event.key, code: event.code, key_code: event.keyCode, modifiers: modifiers(event) });
    }

    function sendKeyboardText() {
        if (!keyboard.value) return;

        send({ type: 'text', text: keyboard.value });
        keyboard.value = '';
    }

    function onCompositionStart() { composing = true; }

    function onCompositionEnd() { composing = false; setTimeout(sendKeyboardText, 0); }

    function onInput(event: Event) {
        if (!(event as InputEvent).isComposing && !composing) sendKeyboardText();
    }

    function onPointerDown(event: PointerEvent) {
        const point = coordinates(event);

        if (!point) return;

        event.preventDefault();
        placeKeyboard(point);
        keyboard.focus({ preventScroll: true });
        stage.setPointerCapture(event.pointerId);
        send({ type: 'mouse', action: 'mousePressed', ...point, button: event.button === 2 ? 'right' : event.button === 1 ? 'middle' : 'left', buttons: event.buttons });
    }

    function onPointerMove(event: PointerEvent) {
        const point = coordinates(event, stage.hasPointerCapture(event.pointerId));

        if (point) send({ type: 'mouse', action: 'mouseMoved', ...point, button: 'none', buttons: event.buttons });
    }

    function onPointerUp(event: PointerEvent) {
        const point = coordinates(event, true);

        if (point) send({ type: 'mouse', action: 'mouseReleased', ...point, button: event.button === 2 ? 'right' : event.button === 1 ? 'middle' : 'left', buttons: event.buttons });
    }

    function onWheel(event: WheelEvent) {
        const point = coordinates(event);

        if (!point) return;

        event.preventDefault();
        send({ type: 'wheel', ...point, delta_x: event.deltaX, delta_y: event.deltaY });
    }

    function onContextMenu(event: MouseEvent) { event.preventDefault(); }

    keyboard.addEventListener('keydown', onKeyDown);
    keyboard.addEventListener('keyup', onKeyUp);
    keyboard.addEventListener('compositionstart', onCompositionStart);
    keyboard.addEventListener('compositionend', onCompositionEnd);
    keyboard.addEventListener('input', onInput);

    stage.addEventListener('pointerdown', onPointerDown);
    stage.addEventListener('pointermove', onPointerMove);
    stage.addEventListener('pointerup', onPointerUp);
    stage.addEventListener('wheel', onWheel, { passive: false });
    stage.addEventListener('contextmenu', onContextMenu);

    function connect(claim = false) {
        if (disposed) return;

        onTransport(null);
        onState('connecting');
        socket = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws?id=${sessionId}${claim ? '&takeover=true' : ''}`);

        socket.binaryType = 'arraybuffer';

        socket.onopen = () => resizeVideo(true);

        socket.onmessage = event => {
            if (event.data instanceof ArrayBuffer) {
                receiveVideoFrame?.(event.data);
                return;
            }

            const message = JSON.parse(event.data);

            if (message.type === 'size') {
                const firstSize = !sourceWidth || !sourceHeight;
                const changed = sourceWidth !== message.width || sourceHeight !== message.height;

                sourceWidth = message.width;
                sourceHeight = message.height;

                if (changed) {
                    onSize(sourceWidth, sourceHeight);

                    if (firstSize) resizeVideo(true);
                }
            }

            if (message.type === 'cursor') placeKeyboard(message.point);

            if (message.type === 'answer' || message.type === 'ice') receiveVideoSignal?.(message);

            if (message.type === 'video_error') {
                if (!receiveVideoFrame) {
                    startFallback();
                    return;
                }

                videoAbort?.abort();
                videoStarted = false;
                onState('error', message.message);
            }
        };

        socket.onclose = event => {
            videoAbort?.abort();
            videoStarted = false;
            sentViewport = null;

            if (disposed) return;

            const occupied = event.reason === 'occupied' || event.reason === 'taken over';

            onState(occupied ? 'occupied' : 'disconnected');

            if (!occupied) reconnectTimer = setTimeout(refreshSession, 1000);
        };
    }

    async function refreshSession() {
        if (disposed) return;

        try {
            // Reload the session cookie after a server restart without reloading the video UI.
            const response = await fetch('/', { credentials: 'same-origin', cache: 'no-store' });

            if (!response.ok) throw new Error('Session refresh failed');
            if (!disposed) connect();
        } catch {
            if (!disposed) reconnectTimer = setTimeout(refreshSession, 1000);
        }
    }

    connect();

    return {
        claim: () => connect(true),

        dispose: () => {
            disposed = true;

            clearTimeout(resizeTimer);
            clearTimeout(reconnectTimer);

            resizeObserver.disconnect();
            screen.removeEventListener('resize', onScreenResize);
            videoAbort?.abort();
            socket?.close();

            keyboard.removeEventListener('keydown', onKeyDown);
            keyboard.removeEventListener('keyup', onKeyUp);
            keyboard.removeEventListener('compositionstart', onCompositionStart);
            keyboard.removeEventListener('compositionend', onCompositionEnd);
            keyboard.removeEventListener('input', onInput);

            stage.removeEventListener('pointerdown', onPointerDown);
            stage.removeEventListener('pointermove', onPointerMove);
            stage.removeEventListener('pointerup', onPointerUp);
            stage.removeEventListener('wheel', onWheel);
            stage.removeEventListener('contextmenu', onContextMenu);
        },
    };
}
