import { t } from './i18n.ts';
import { createWebSocketVideo } from './websocket-video.ts';
import { createInput } from './input.ts';
import { startWebRtc } from './webrtc-video.ts';
import { MAX_VIDEO_WIDTH, MAX_VIDEO_HEIGHT, type ClientMessage, type ServerMessage, type VideoSignal } from './protocol.ts';

export type VideoTransport = 'WebRTC' | 'WebSocket';
export type ConnectionState = 'connecting' | 'streaming' | 'occupied' | 'disconnected' | 'error';

const FALLBACK_TIMEOUT_MS = 15_000;

export function startRemote(
    stage: HTMLElement,
    screen: HTMLVideoElement,
    keyboard: HTMLTextAreaElement,
    onState: (state: ConnectionState, detail?: string) => void,
    onSize: (width: number, height: number) => void,
    onTransport: (transport: VideoTransport | null) => void = () => {},
) {
    const defaultTitle = document.title;

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
    let disposed = false;
    let generation = 0;

    function onScreenResize() {
        decodedSize = screen.videoWidth && screen.videoHeight ? [screen.videoWidth, screen.videoHeight] : null;
    }

    screen.addEventListener('resize', onScreenResize);

    const input = createInput(stage, keyboard, () => ({ sourceWidth, sourceHeight, decodedSize }), send);

    function send(message: ClientMessage) {
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

            if (sourceWidth && sourceHeight) startVideo();
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
        onState('connecting', t('connection.switching'));
        const timeout = setTimeout(() => fail(t('connection.websocketTimeout')), FALLBACK_TIMEOUT_MS);

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

    function startVideo() {
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
        onState('connecting', t('connection.webrtcConnecting'));

        videoStarted = true;
        signal.addEventListener('abort', () => {
            receiveVideoSignal = undefined;
            decodedSize = null;
        }, { once: true });

        try {
            receiveVideoSignal = startWebRtc(screen, signal, hevc, send, () => {
                onScreenResize();
                onState('streaming');
            }, message => {
                console.info(t('connection.fallbackLog', { error: message }));
                startFallback();
            });
        } catch (error) {
            console.info(error);
            startFallback();
        }
    }

    function connect(claim = false) {
        if (disposed) return;

        generation++;
        clearTimeout(reconnectTimer);
        clearTimeout(resizeTimer);
        input.reset(false);
        videoAbort?.abort();
        videoStarted = false;
        sentViewport = null;
        const previous = socket;
        socket = undefined;
        previous?.close();
        document.title = defaultTitle;
        onTransport(null);
        onState('connecting');
        const current = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws${claim ? '?takeover=true' : ''}`);
        socket = current;

        socket.binaryType = 'arraybuffer';

        socket.onopen = () => { if (!disposed && socket === current) resizeVideo(true); };

        socket.onmessage = event => {
            if (disposed || socket !== current) return;
            if (event.data instanceof ArrayBuffer) {
                receiveVideoFrame?.(event.data);
                return;
            }

            let message: ServerMessage;
            try { message = JSON.parse(event.data); } catch { return; }

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

            if (message.type === 'title' && typeof message.title === 'string') document.title = message.title.trim() || defaultTitle;

            if (message.type === 'cursor') input.placeKeyboard(message.point);

            if (message.type === 'answer' || message.type === 'ice') receiveVideoSignal?.(message);

            if (message.type === 'video_error') {
                if (!receiveVideoFrame) {
                    startFallback();
                    return;
                }

                videoAbort?.abort();
                videoStarted = false;
                onState('error', t(`errors.${message.code}`));
            }
        };

        socket.onclose = event => {
            if (disposed || socket !== current) return;
            socket = undefined;
            input.reset(false);
            document.title = defaultTitle;
            videoAbort?.abort();
            videoStarted = false;
            sentViewport = null;

            const occupied = event.reason === 'occupied' || event.reason === 'taken over';

            onState(occupied ? 'occupied' : 'disconnected');

            if (!occupied) reconnectTimer = setTimeout(refreshSession, 1000);
        };
    }

    async function refreshSession() {
        if (disposed) return;
        const attempt = generation;

        try {
            // Reload the session cookie after a server restart without reloading the video UI.
            const response = await fetch('/', { credentials: 'same-origin', cache: 'no-store' });

            if (!response.ok) throw new Error('Session refresh failed');
            if (!disposed && attempt === generation) connect();
        } catch {
            if (!disposed && attempt === generation) reconnectTimer = setTimeout(refreshSession, 1000);
        }
    }

    connect();

    return {
        claim: () => connect(true),

        dispose: () => {
            input.dispose();
            disposed = true;
            document.title = defaultTitle;

            clearTimeout(resizeTimer);
            clearTimeout(reconnectTimer);

            resizeObserver.disconnect();
            screen.removeEventListener('resize', onScreenResize);
            videoAbort?.abort();
            socket?.close();
        },
    };
}
