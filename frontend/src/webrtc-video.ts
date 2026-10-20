import { t } from './i18n.ts';
import type { ClientMessage, WebRtcSignal } from './protocol.ts';
import stunServers from '../../shared/stun-servers.json' with { type: 'json' };

const VIDEO_CONNECT_TIMEOUT_MS = 8_000;
const DATA_CHANNEL_CHUNK = 16 * 1024;
const MAX_BUFFERED_BYTES = 256 * 1024;

export function startWebRtc(
    signal: AbortSignal,
    send: (message: ClientMessage) => void,
    onOpen: (sendData: (data: ArrayBufferView<ArrayBuffer>) => void) => void,
    onData: (data: ArrayBuffer) => void,
    onFailure: (message: string) => void,
) {
    const peer = new RTCPeerConnection({
        iceServers: [{ urls: stunServers }],
    });
    const channel = peer.createDataChannel('rfb', { ordered: true });
    const videoId = crypto.randomUUID();
    const pendingCandidates: (RTCIceCandidateInit | null)[] = [];
    let offerSent = false;
    let hasUdpCandidate = false;
    let incomingSignals = Promise.resolve();
    const timeout = setTimeout(() => fail(t('connection.timeout')), VIDEO_CONNECT_TIMEOUT_MS);

    function fail(message: string) {
        if (!signal.aborted) onFailure(message);
    }

    signal.addEventListener(
        'abort',
        () => {
            clearTimeout(timeout);
            peer.close();
            channel.close();
        },
        { once: true },
    );

    peer.onicecandidate = (event) => {
        if (signal.aborted) return;

        const candidate = event.candidate?.toJSON() ?? null;

        if (candidate && / udp /i.test(candidate.candidate ?? '')) hasUdpCandidate = true;

        if (!candidate && !hasUdpCandidate) {
            fail(t('connection.noUdp'));
            return;
        }

        if (offerSent) send({ type: 'ice', video_id: videoId, candidate });
        else pendingCandidates.push(candidate);
    };

    const receive = (message: WebRtcSignal) => {
        if (signal.aborted || message.video_id !== videoId) return;

        // Candidates must wait for the preceding answer to finish being applied.
        incomingSignals = incomingSignals
            .then(async () => {
                if (signal.aborted) return;

                if (message.type === 'answer') await peer.setRemoteDescription(message.description);
                else await peer.addIceCandidate(message.candidate ?? undefined);
            })
            .catch((error) => fail(t('connection.signalingError', { error: String(error) })));
    };

    channel.binaryType = 'arraybuffer';

    channel.onopen = () => {
        if (signal.aborted) return;
        clearTimeout(timeout);

        onOpen((data) => {
            if (signal.aborted) return;

            if (channel.readyState !== 'open' || channel.bufferedAmount > MAX_BUFFERED_BYTES) {
                fail(t('connection.webrtcFailed'));
                return;
            }

            const bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);

            try {
                for (let offset = 0; offset < bytes.length; offset += DATA_CHANNEL_CHUNK) {
                    channel.send(bytes.subarray(offset, offset + DATA_CHANNEL_CHUNK));
                }
            } catch (error) {
                fail(t('connection.webrtcError', { error: String(error) }));
            }
        });
    };

    channel.onmessage = (event) => {
        if (!signal.aborted && event.data instanceof ArrayBuffer) onData(event.data);
    };

    channel.onerror = () => fail(t('connection.webrtcFailed'));
    channel.onclose = () => fail(t('connection.webrtcFailed'));

    peer.onconnectionstatechange = () => {
        if (peer.connectionState === 'failed' && !signal.aborted) {
            fail(t('connection.webrtcFailed'));
        }
    };

    void (async () => {
        try {
            const offer = await peer.createOffer();

            await peer.setLocalDescription(offer);

            if (signal.aborted) return;

            send({
                type: 'offer',
                video_id: videoId,
                sdp: offer.sdp ?? '',
            });
            offerSent = true;

            for (const candidate of pendingCandidates) {
                send({ type: 'ice', video_id: videoId, candidate });
            }

            pendingCandidates.length = 0;
        } catch (error) {
            fail(t('connection.webrtcError', { error: error instanceof Error ? error.message : String(error) }));
        }
    })();

    return receive;
}
