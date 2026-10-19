import assert from 'node:assert/strict';
import { test } from 'node:test';

import { createWebSocketVideo } from '../src/websocket-video.ts';

test('WebSocket decoder handles keyframes, resize, acknowledgments and disposal', t => {
    let callbacks;
    const configs = [];
    const chunks = [];
    const acknowledgments = [];
    const frames = [];
    const errors = [];
    const canvas = { width: 0, height: 0, setAttribute() {}, getContext: () => ({ drawImage() {} }), remove() { this.removed = true; } };
    const screen = { after() {}, hidden: false };
    const originals = new Map();

    for (const [name, value] of Object.entries({
        document: { createElement: () => canvas },
        VideoDecoder: class {
            state = 'unconfigured';
            constructor(value) { callbacks = value; }
            configure(config) { configs.push(config); this.state = 'configured'; }
            decode(chunk) { chunks.push(chunk); }
            close() { this.state = 'closed'; }
        },
        EncodedVideoChunk: class { constructor(config) { Object.assign(this, config); } },
    })) {
        originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
        Object.defineProperty(globalThis, name, {configurable:true, writable:true, value});
    }

    t.after(() => {
        for (const [name, descriptor] of originals) {
            if (descriptor) Object.defineProperty(globalThis, name, descriptor);
            else delete globalThis[name];
        }
    });

    const abort = new AbortController();
    const receive = createWebSocketVideo(screen, abort.signal, (...size) => frames.push(size), sequence => acknowledgments.push(sequence), error => errors.push(error));
    const packet = (sequence, width, height, key) => {
        const data = new ArrayBuffer(14);
        const view = new DataView(data);
        view.setUint32(0, sequence); view.setUint16(4, width); view.setUint16(6, height); view.setUint8(8, key ? 1 : 0);
        new Uint8Array(data, 9).set([0,0,0,1,0x26]);
        return data;
    };
    receive(packet(1, 1280, 720, true));
    assert.equal(configs.length, 1);
    assert.equal(chunks[0].type, 'key');
    assert.deepEqual(acknowledgments, []);
    let closed = 0;
    callbacks.output({displayWidth:1280, displayHeight:720, close() { closed++; }});
    assert.deepEqual(acknowledgments, [1]);
    assert.deepEqual(frames, [[1280,720]]);
    receive(packet(2, 1280, 720, false));
    assert.equal(configs.length, 1);
    receive(packet(3, 640, 360, true));
    assert.equal(configs.length, 2);
    receive(packet(4, 800, 600, false));
    assert.equal(errors.length, 1);
    abort.abort();
    callbacks.output({close() { closed++; }});
    assert.equal(closed, 2);
    assert.equal(canvas.removed, true);
    assert.equal(screen.hidden, false);
    assert.deepEqual(acknowledgments, [1]);
});
