import assert from 'node:assert/strict';
import { test } from 'node:test';

import { startWebRtc } from '../src/webrtc-video.ts';

test('trickle ICE sends the offer immediately and applies candidates after the answer', async (t) => {
    let peer;
    let finishAnswer;
    const candidate = { candidate: 'candidate:test 1 udp 123 127.0.0.1 9999 typ host', sdpMid: '0', sdpMLineIndex: 0 };
    const applied = [];
    const sentBytes = [];
    const receivedBytes = [];
    let sendRfb;

    class Peer {
        iceGatheringState = 'gathering';

        constructor() {
            peer = this;
        }

        createDataChannel(label, options) {
            assert.equal(label, 'rfb');
            assert.deepEqual(options, { ordered: true });
            this.channel = {
                close() {},
                bufferedAmount: 0,
                readyState: 'open',
                send(data) {
                    sentBytes.push(data.slice());
                },
            };
            return this.channel;
        }

        async createOffer() {
            return { type: 'offer', sdp: 'test offer' };
        }

        async setLocalDescription() {
            this.onicecandidate({ candidate: { toJSON: () => candidate } });
            this.onicecandidate({ candidate: null });
        }

        async setRemoteDescription(description) {
            await new Promise((resolve) => {
                finishAnswer = resolve;
            });
            applied.push(description.type);
        }

        async addIceCandidate(value) {
            applied.push(value ?? null);
        }

        close() {
            this.closed = true;
        }
    }

    const messages = [];
    const original = globalThis.RTCPeerConnection;
    globalThis.RTCPeerConnection = Peer;
    t.after(() => {
        globalThis.RTCPeerConnection = original;
    });
    const abort = new AbortController();
    const receive = startWebRtc(
        abort.signal,
        (message) => messages.push(message),
        (send) => {
            sendRfb = send;
        },
        (data) => receivedBytes.push(data),
        () => {},
    );
    const flush = () => new Promise(setImmediate);

    await flush();

    const offer = messages[0];

    assert.equal(peer.iceGatheringState, 'gathering');
    assert.equal(offer.type, 'offer');
    assert.deepEqual(
        messages.map((message) => message.type),
        ['offer', 'ice', 'ice'],
    );
    assert.deepEqual(messages[1].candidate, candidate);
    assert.equal(messages[2].candidate, null);

    receive({ type: 'ice', video_id: 'stale-session', candidate });
    receive({ type: 'answer', video_id: offer.video_id, description: { type: 'answer', sdp: 'test answer' } });
    receive({ type: 'ice', video_id: offer.video_id, candidate });
    receive({ type: 'ice', video_id: offer.video_id, candidate: null });
    await flush();
    assert.deepEqual(applied, []);

    finishAnswer();
    await flush();
    assert.deepEqual(applied, ['answer', candidate, null]);

    peer.channel.onopen();
    const payload = Uint8Array.from({ length: 40_006 }, (_, i) => i % 251).subarray(3, 40_003);
    sendRfb(payload);
    assert.deepEqual(
        sentBytes.map((chunk) => chunk.length),
        [16384, 16384, 7232],
    );
    assert.deepEqual(Buffer.concat(sentBytes), Buffer.from(payload));
    const incoming = new Uint8Array([1, 2, 3]).buffer;
    peer.channel.onmessage({ data: incoming });
    assert.deepEqual(receivedBytes, [incoming]);

    abort.abort();
    receive({ type: 'ice', video_id: offer.video_id, candidate });
    await flush();
    assert.equal(peer.closed, true);
    assert.deepEqual(applied, ['answer', candidate, null]);
});
