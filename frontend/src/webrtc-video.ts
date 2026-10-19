import { t } from './i18n.ts';
import { MAX_VIDEO_WIDTH, MAX_VIDEO_HEIGHT, type ClientMessage, type VideoSignal } from './protocol.ts';
import stunServers from '../../shared/stun-servers.json' with { type: 'json' };

const VIDEO_CONNECT_TIMEOUT_MS = 8_000;

export function startWebRtc(
    screen: HTMLVideoElement,
    signal: AbortSignal,
    codecs: RTCRtpCodec[],
    send: (message: ClientMessage) => void,
    onFrame: () => void,
    onFailure: (message: string) => void,
) {
    const peer = new RTCPeerConnection({
        iceServers: [{ urls: stunServers }],
    });
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
            screen.srcObject = null;
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

    const receive = (message: VideoSignal) => {
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

    const transceiver = peer.addTransceiver('video', { direction: 'recvonly' });

    transceiver.setCodecPreferences(codecs);

    peer.ontrack = (event) => {
        screen.srcObject = event.streams[0] ?? new MediaStream([event.track]);

        screen.requestVideoFrameCallback(() => {
            if (!signal.aborted) {
                clearTimeout(timeout);
                onFrame();
            }
        });

        void screen.play().catch((error) => {
            fail(t('connection.playbackError', { error: String(error) }));
        });
    };

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
                width: MAX_VIDEO_WIDTH,
                height: MAX_VIDEO_HEIGHT,
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
