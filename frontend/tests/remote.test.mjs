import assert from 'node:assert/strict';
import { test } from 'node:test';

import { startRemote } from '../src/remote.ts';

function setup(t, viewport, source, decoded = source) {
    t.mock.timers.enable({ apis: ['setTimeout'] });

    const originals = new Map();

    function global(name, value) {
        originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
        Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
    }

    let socket;
    let resized;

    class Socket {
        static OPEN = 1;
        readyState = Socket.OPEN;
        sent = [];

        constructor() { socket = this; }

        send(message) { this.sent.push(JSON.parse(message)); }

        close() {}
    }

    global('WebSocket', Socket);
    global('ResizeObserver', class {
        constructor(callback) { resized = callback; }

        observe() {}

        disconnect() {}
    });

    global('location', { protocol: 'http:', host: 'localhost' });
    global('devicePixelRatio', 1);
    global('window', {});

    const stage = Object.assign(new EventTarget(), {
        clientWidth: viewport[0], clientHeight: viewport[1],
        getBoundingClientRect() { return { left: 0, top: 0, width: this.clientWidth, height: this.clientHeight }; },
        setPointerCapture() {}, hasPointerCapture() { return false; },
    });
    const screen = Object.assign(new EventTarget(), { videoWidth: decoded[0], videoHeight: decoded[1] });
    const keyboard = Object.assign(new EventTarget(), { style: {}, focus() {} });
    const remote = startRemote(stage, screen, keyboard, () => {}, () => {});

    t.after(() => {
        remote.dispose();

        for (const [name, descriptor] of originals) {
            if (descriptor) Object.defineProperty(globalThis, name, descriptor);
            else delete globalThis[name];
        }
    });

    const size = (width, height) => socket.onmessage({ data: JSON.stringify({ type: 'size', width, height }) });

    size(...source);

    t.mock.timers.tick(1);
    screen.dispatchEvent(new Event('resize'));

    return {
        stage, screen, keyboard, socket, size,

        resize(width, height) {
            stage.clientWidth = width;
            stage.clientHeight = height;
            resized();
        },

        point(x, y) {
            stage.dispatchEvent(Object.assign(new Event('pointerdown', { cancelable: true }), {
                clientX: x, clientY: y, pointerId: 1, button: 0, buttons: 1,
            }));

            return socket.sent.filter(message => message.type === 'mouse').at(-1);
        },
    };
}

test('upscaled video maps pointer and input-method position to the source', t => {
    const remote = setup(t, [1920, 1080], [960, 540]);
    const point = remote.point(1440, 810);

    assert.equal(point.x, 720);
    assert.equal(point.y, 405);
    assert.equal(remote.keyboard.style.left, '1440px');
    assert.equal(remote.keyboard.style.top, '810px');
});

test('downscaled video ignores letterbox clicks and maps its full visible area', t => {
    const remote = setup(t, [800, 600], [2560, 1440], [800, 450]);

    assert.equal(remote.point(400, 40), undefined);

    const point = remote.point(600, 412.5);

    assert.equal(point.x, 1920);
    assert.equal(point.y, 1080);
    assert.equal(remote.keyboard.style.top, '412.5px');
});

test('only a changed viewport is reported, after one second of inactivity', t => {
    const remote = setup(t, [1920, 1080], [960, 540]);
    const reports = () => remote.socket.sent.filter(message => message.type === 'resize');

    assert.deepEqual(reports(), [{ type: 'resize', width: 1920, height: 1080 }]);

    remote.screen.videoWidth = 1280;
    remote.screen.videoHeight = 720;
    remote.screen.dispatchEvent(new Event('resize'));
    remote.size(1280, 720);
    remote.resize(1920, 1080);
    t.mock.timers.tick(1000);

    assert.equal(reports().length, 1);

    remote.resize(1000, 800);
    t.mock.timers.tick(500);
    remote.resize(800, 600);
    t.mock.timers.tick(999);

    assert.equal(reports().length, 1);

    t.mock.timers.tick(1);

    assert.deepEqual(reports().at(-1), { type: 'resize', width: 800, height: 600 });
    assert.equal(reports().length, 2);
});
