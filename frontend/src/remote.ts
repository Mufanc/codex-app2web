import { t } from './i18n.ts';
import { createVncVideo } from './vnc-video.ts';
import { startWebRtc } from './webrtc-video.ts';
import { createInput } from './input.ts';
import {
    MAX_VIDEO_WIDTH,
    MAX_VIDEO_HEIGHT,
    type ClientMessage,
    type ServerMessage,
    type WebRtcSignal,
    type NativeMenu,
} from './protocol.ts';

export type VideoTransport = 'WebRTC' | 'WebSocket';
export type ConnectionState = 'connecting' | 'streaming' | 'occupied' | 'disconnected' | 'error';

const FALLBACK_TIMEOUT_MS = 15_000;

export function startRemote(
    stage: HTMLElement,
    screen: HTMLElement,
    keyboard: HTMLTextAreaElement,
    onState: (state: ConnectionState, detail?: string) => void,
    onSize: (width: number, height: number) => void,
    onTransport: (transport: VideoTransport | null) => void = () => {},
    onMenu: (menu: NativeMenu | null) => void = () => {},
) {
    const defaultTitle = document.title;

    let sourceWidth = 0;
    let sourceHeight = 0;
    let sentViewport: [number, number] | null = null;
    let videoStarted = false;
    let decodedSize: [number, number] | null = null;
    let socket: WebSocket | undefined;
    let videoAbort: AbortController | undefined;
    let receiveWebRtcSignal: ((message: WebRtcSignal) => void) | undefined;
    let transport: VideoTransport | undefined;
    let receiveVideoFrame: ((data: ArrayBuffer) => void) | undefined;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    let generation = 0;

    const input = createInput(stage, keyboard, () => ({ sourceWidth, sourceHeight, decodedSize }), send);

    function send(message: ClientMessage) {
        if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
    }

    function resizeVideo(immediate = false) {
        clearTimeout(resizeTimer);

        resizeTimer = setTimeout(
            () => {
                if (socket?.readyState !== WebSocket.OPEN) return;

                const viewportWidth = Math.max(
                    320,
                    Math.min(MAX_VIDEO_WIDTH, Math.round(stage.clientWidth * devicePixelRatio)),
                );
                const viewportHeight = Math.max(
                    240,
                    Math.min(MAX_VIDEO_HEIGHT, Math.round(stage.clientHeight * devicePixelRatio)),
                );

                if (!sentViewport || sentViewport[0] !== viewportWidth || sentViewport[1] !== viewportHeight) {
                    send({ type: 'resize', width: viewportWidth, height: viewportHeight });
                    sentViewport = [viewportWidth, viewportHeight];
                }

                if (sourceWidth && sourceHeight) startVideo();
            },
            immediate ? 0 : 1000,
        );
    }

    const resizeObserver = new ResizeObserver(() => resizeVideo());

    resizeObserver.observe(stage);

    function startVideo(fallback = false) {
        if (!fallback && videoStarted && !videoAbort?.signal.aborted) return;
        if (!socket || socket.readyState !== WebSocket.OPEN) return;

        videoAbort?.abort();
        videoAbort = new AbortController();
        videoStarted = true;
        transport = !fallback && typeof RTCPeerConnection !== 'undefined' ? 'WebRTC' : 'WebSocket';
        onTransport(transport);
        onState('connecting', t(transport === 'WebRTC' ? 'connection.webrtcConnecting' : 'connection.vncConnecting'));

        const signal = videoAbort.signal;
        const timeout = setTimeout(fail, FALLBACK_TIMEOUT_MS);

        function fail() {
            if (signal.aborted) return;

            if (transport === 'WebRTC') {
                startVideo(true);
                return;
            }

            videoAbort?.abort();
            onState('error', t('connection.vncFailed'));
        }

        function display(sendData: (data: ArrayBufferView<ArrayBuffer>) => void) {
            if (signal.aborted) return;

            receiveVideoFrame = createVncVideo(
                screen,
                sendData,
                signal,
                (width, height) => {
                    clearTimeout(timeout);
                    decodedSize = [width, height];
                    onState('streaming');
                },
                fail,
            );
        }

        signal.addEventListener(
            'abort',
            () => {
                clearTimeout(timeout);
                receiveVideoFrame = undefined;
                receiveWebRtcSignal = undefined;
                decodedSize = null;
            },
            { once: true },
        );

        try {
            if (transport === 'WebRTC') {
                receiveWebRtcSignal = startWebRtc(signal, send, display, (data) => receiveVideoFrame?.(data), fail);
            } else {
                display((data) => {
                    if (socket?.readyState === WebSocket.OPEN) socket.send(data);
                });
                send({ type: 'request_fallback' });
            }
        } catch {
            fail();
        }
    }

    function connect(claim = false) {
        onMenu(null);
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

        const current = new WebSocket(
            `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws${claim ? '?takeover=true' : ''}`,
        );

        socket = current;
        socket.binaryType = 'arraybuffer';

        socket.onopen = () => {
            if (!disposed && socket === current) resizeVideo(true);
        };

        socket.onmessage = (event) => {
            if (disposed || socket !== current) return;

            if (event.data instanceof ArrayBuffer) {
                if (transport === 'WebSocket') receiveVideoFrame?.(event.data);
                return;
            }

            let message: ServerMessage;

            try {
                message = JSON.parse(event.data);
            } catch {
                return;
            }

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

            if (message.type === 'title' && typeof message.title === 'string')
                document.title = message.title.trim() || defaultTitle;

            if (message.type === 'cursor') input.placeKeyboard(message.point);

            if (message.type === 'menu') {
                const rect = input.pictureRect();
                const area = stage.getBoundingClientRect();

                onMenu(
                    message.menu && rect
                        ? {
                              ...message.menu,
                              x: area.left + rect.left + (message.menu.x / sourceWidth) * rect.width,
                              y: area.top + rect.top + (message.menu.y / sourceHeight) * rect.height,
                          }
                        : null,
                );
            }

            if (message.type === 'answer' || message.type === 'ice') receiveWebRtcSignal?.(message);

            if (message.type === 'video_error') {
                if (transport === 'WebRTC') {
                    startVideo(true);
                } else {
                    videoAbort?.abort();
                    videoStarted = false;
                    onState('error', t(`errors.${message.code}`));
                }
            }
        };

        socket.onclose = (event) => {
            if (disposed || socket !== current) return;
            socket = undefined;
            onMenu(null);
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
        selectMenu: (id: string, selected: string | null) => send({ type: 'menu_result', id, selected }),

        dispose: () => {
            onMenu(null);
            input.dispose();
            disposed = true;
            document.title = defaultTitle;
            clearTimeout(resizeTimer);
            clearTimeout(reconnectTimer);
            resizeObserver.disconnect();
            videoAbort?.abort();
            socket?.close();
        },
    };
}
