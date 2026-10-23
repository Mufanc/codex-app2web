import assert from 'node:assert/strict';
import { test, mock } from 'node:test';

mock.module('../src/vnc-video.ts', {
    namedExports: {
        createVncVideo: (screen, socket, signal, onFrame) => {
            const resized = () => onFrame(screen.videoWidth, screen.videoHeight);
            screen.addEventListener('resize', resized);
            signal.addEventListener('abort', () => screen.removeEventListener('resize', resized));
            return () => {};
        },
    },
});

const { startRemote } = await import('../src/remote.ts');

function setup(t, viewport, source, decoded = source, Peer) {
    t.mock.timers.enable({ apis: ['setTimeout'] });

    const originals = new Map();

    function global(name, value) {
        originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
        Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
    }

    let socket;
    let resized;
    const transports = [];

    class Socket {
        static OPEN = 1;
        readyState = Socket.OPEN;
        sent = [];

        constructor() {
            socket = this;
        }

        send(message) {
            this.sent.push(JSON.parse(message));
        }

        close() {}
    }

    global('WebSocket', Socket);
    global(
        'ResizeObserver',
        class {
            constructor(callback) {
                resized = callback;
            }

            observe() {}

            disconnect() {}
        },
    );

    global('document', Object.assign(new EventTarget(), { title: 'CodexApp2Web' }));
    global('location', { protocol: 'http:', host: 'localhost' });
    global('devicePixelRatio', 1);
    global('window', Object.assign(new EventTarget(), { RTCPeerConnection: Peer }));
    global('RTCPeerConnection', Peer);
    global('RTCRtpReceiver', { getCapabilities: () => ({ codecs: [{ mimeType: 'video/H265' }] }) });

    const stage = Object.assign(new EventTarget(), {
        clientWidth: viewport[0],
        clientHeight: viewport[1],
        getBoundingClientRect() {
            return { left: 0, top: 0, width: this.clientWidth, height: this.clientHeight };
        },
        setPointerCapture() {},
        hasPointerCapture() {
            return false;
        },
    });
    const screen = Object.assign(new EventTarget(), { videoWidth: decoded[0], videoHeight: decoded[1] });
    const keyboard = Object.assign(new EventTarget(), { style: {}, dataset: {}, scrollWidth: 100, focus() {} });
    const remote = startRemote(
        stage,
        screen,
        keyboard,
        () => {},
        () => {},
        (transport) => transports.push(transport),
    );

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
        stage,
        screen,
        keyboard,
        transports,
        size,
        dispose: remote.dispose,
        claim: remote.claim,

        get socket() {
            return socket;
        },

        resize(width, height) {
            stage.clientWidth = width;
            stage.clientHeight = height;
            resized();
        },

        point(x, y) {
            stage.dispatchEvent(
                Object.assign(new Event('pointerdown', { cancelable: true }), {
                    clientX: x,
                    clientY: y,
                    pointerId: 1,
                    button: 0,
                    buttons: 1,
                }),
            );

            return socket.sent.filter((message) => message.type === 'mouse').at(-1);
        },
    };
}

test('upscaled video maps pointer and input-method position to the source', (t) => {
    const remote = setup(t, [1920, 1080], [960, 540]);
    const point = remote.point(1440, 810);

    assert.equal(point.x, 720);
    assert.equal(point.y, 405);
    assert.equal(remote.keyboard.style.left, '1440px');
    assert.equal(remote.keyboard.style.top, '790px');
});

test('downscaled video ignores letterbox clicks and maps its full visible area', (t) => {
    const remote = setup(t, [800, 600], [2560, 1440], [800, 450]);

    assert.equal(remote.point(400, 40), undefined);

    const point = remote.point(600, 412.5);

    assert.equal(point.x, 1920);
    assert.equal(point.y, 1080);
    assert.equal(remote.keyboard.style.top, '392.5px');
});

test('remote caret bottom anchors the local input within the viewport', (t) => {
    const remote = setup(t, [800, 600], [800, 600]);
    const cursor = (y) => remote.socket.onmessage({ data: JSON.stringify({ type: 'cursor', point: { x: 100, y } }) });

    cursor(300);
    assert.equal(remote.keyboard.style.top, '280px');
    cursor(10);
    assert.equal(remote.keyboard.style.top, '0px');
    cursor(600);
    assert.equal(remote.keyboard.style.top, '580px');
});

test('caret font and height follow video scaling and viewport resizing', (t) => {
    const remote = setup(t, [800, 600], [1600, 1200]);
    remote.socket.onmessage({
        data: JSON.stringify({ type: 'cursor', point: { x: 200, y: 600, height: 40, fontSize: 32 } }),
    });
    assert.equal(remote.keyboard.style.fontSize, '16px');
    assert.equal(remote.keyboard.style.height, '20px');
    assert.equal(remote.keyboard.style.lineHeight, '20px');
    assert.equal(remote.keyboard.style.top, '280px');

    remote.resize(1600, 1200);
    assert.equal(remote.keyboard.style.fontSize, '32px');
    assert.equal(remote.keyboard.style.height, '40px');
    assert.equal(remote.keyboard.style.top, '560px');

    remote.socket.onmessage({
        data: JSON.stringify({ type: 'cursor', point: { x: 200, y: 600, height: -1, fontSize: 0 } }),
    });
    assert.equal(remote.keyboard.style.fontSize, '16px');
    assert.equal(remote.keyboard.style.height, '20px');
});

test('only a changed viewport is reported, after one second of inactivity', (t) => {
    const remote = setup(t, [1920, 1080], [960, 540]);
    const reports = () => remote.socket.sent.filter((message) => message.type === 'resize');

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

test('VNC starts once and viewport changes do not recreate the transport', (t) => {
    const remote = setup(t, [800, 600], [960, 540]);
    const starts = () => remote.socket.sent.filter((message) => message.type === 'request_fallback');

    assert.equal(starts().length, 1);
    remote.resize(600, 400);
    t.mock.timers.tick(1000);
    assert.equal(starts().length, 1);
    assert.deepEqual(remote.socket.sent.filter((message) => message.type === 'resize').at(-1), {
        type: 'resize',
        width: 600,
        height: 400,
    });
});

test('reconnect refreshes the session cookie and retries failed refreshes', async (t) => {
    const remote = setup(t, [800, 600], [960, 540]);
    const originalSocket = remote.socket;
    const fetch = t.mock.method(globalThis, 'fetch', async () => ({ ok: false }));

    originalSocket.onclose({ reason: '' });
    t.mock.timers.tick(1000);
    await new Promise(setImmediate);
    assert.equal(remote.socket, originalSocket);

    fetch.mock.mockImplementation(async () => ({ ok: true }));
    t.mock.timers.tick(1000);
    await new Promise(setImmediate);
    assert.notEqual(remote.socket, originalSocket);
    assert.deepEqual(fetch.mock.calls[1].arguments, ['/', { credentials: 'same-origin', cache: 'no-store' }]);

    const socket = remote.socket;
    let finish;
    fetch.mock.mockImplementation(
        () =>
            new Promise((resolve) => {
                finish = resolve;
            }),
    );
    socket.onclose({ reason: '' });
    t.mock.timers.tick(1000);
    remote.dispose();
    finish({ ok: true });
    await new Promise(setImmediate);
    assert.equal(remote.socket, socket);
});

test('task title sync falls back to the original title and resets on disconnect', (t) => {
    const view = setup(t, [1000, 600], [1000, 600]);
    const title = (value) => view.socket.onmessage({ data: JSON.stringify({ type: 'title', title: value }) });

    title('Task A');
    assert.equal(document.title, 'Task A');
    title('');
    assert.equal(document.title, 'CodexApp2Web');
    title('   ');
    assert.equal(document.title, 'CodexApp2Web');
    title('Task B');
    view.socket.onclose({ reason: 'taken over' });
    assert.equal(document.title, 'CodexApp2Web');
    title('Task C');
    view.dispose();
    assert.equal(document.title, 'CodexApp2Web');
});

test('superseded socket callbacks cannot stop or rename the current session', (t) => {
    const remote = setup(t, [800, 600], [960, 540]);
    const previous = remote.socket;
    remote.claim();
    const current = remote.socket;
    assert.notEqual(current, previous);

    current.onmessage({ data: JSON.stringify({ type: 'title', title: 'Current task' }) });
    previous.onmessage({ data: JSON.stringify({ type: 'title', title: 'Old task' }) });
    previous.onclose({ reason: 'taken over' });
    assert.equal(document.title, 'Current task');

    t.mock.timers.tick(1000);
    assert.equal(remote.socket, current);
});

test('composition stays local and visible until committed', (t) => {
    const remote = setup(t, [800, 600], [960, 540]);
    const { keyboard } = remote;
    remote.socket.sent.length = 0;
    keyboard.dispatchEvent(new Event('compositionstart'));
    keyboard.value = 'zhe ge xiao guo';
    keyboard.dispatchEvent(Object.assign(new Event('input'), { isComposing: true }));
    assert.equal(keyboard.dataset.composing, 'true');
    assert.equal(keyboard.style.width, '102px');
    assert.deepEqual(remote.socket.sent, []);

    keyboard.value = '这个效果';
    keyboard.dispatchEvent(new Event('compositionend'));
    keyboard.dispatchEvent(Object.assign(new Event('input'), { isComposing: false }));
    t.mock.timers.tick(1);
    assert.equal(keyboard.dataset.composing, undefined);
    assert.equal(keyboard.style.width, '');
    assert.equal(keyboard.value, '');
    assert.deepEqual(remote.socket.sent, [{ type: 'text', text: '这个效果' }]);
});

test('losing focus releases input and cancels unfinished composition', (t) => {
    const remote = setup(t, [800, 600], [960, 540]);
    const key = (type) =>
        Object.assign(new Event(type, { cancelable: true }), {
            key: 'Shift',
            code: 'ShiftLeft',
            keyCode: 16,
            shiftKey: true,
        });
    remote.keyboard.dispatchEvent(key('keydown'));
    remote.keyboard.dispatchEvent(new Event('compositionstart'));
    remote.keyboard.value = 'unfinished';
    window.dispatchEvent(new Event('blur'));
    assert.equal(remote.keyboard.dataset.composing, undefined);
    assert.equal(remote.keyboard.style.width, '');
    remote.keyboard.dispatchEvent(new Event('compositionend'));
    window.dispatchEvent(new Event('blur'));
    t.mock.timers.tick(1);
    remote.keyboard.dispatchEvent(key('keyup'));

    assert.equal(remote.keyboard.value, '');
    assert.deepEqual(
        remote.socket.sent
            .filter((message) => ['key', 'reset', 'text'].includes(message.type))
            .map((message) => message.type),
        ['key', 'reset', 'reset'],
    );

    remote.stage.dispatchEvent(new Event('pointercancel'));
    assert.equal(remote.socket.sent.at(-1).type, 'reset');
});

for (const failure of ['timeout', 'closed']) {
    test(`WebRTC ${failure} switches once to a fresh WebSocket RFB session`, async (t) => {
        let peer;
        class Peer {
            constructor() {
                peer = this;
            }
            createDataChannel() {
                this.channel = {
                    close() {
                        this.onclose?.();
                    },
                    readyState: 'open',
                    bufferedAmount: 0,
                    send() {},
                };
                return this.channel;
            }
            async createOffer() {
                return { type: 'offer', sdp: 'data channel offer' };
            }
            async setLocalDescription() {}
            close() {
                this.closed = true;
            }
        }
        const remote = setup(t, [800, 600], [960, 540], [800, 450], Peer);
        await new Promise(setImmediate);
        assert.equal(
            remote.socket.sent.some((message) => message.type === 'request_fallback'),
            false,
        );
        if (failure === 'timeout') t.mock.timers.tick(8000);
        else {
            peer.channel.onopen();
            peer.channel.onclose();
        }
        assert.deepEqual(remote.transports, [null, 'WebRTC', 'WebSocket']);
        assert.equal(peer.closed, true);
        assert.equal(remote.socket.sent.filter((message) => message.type === 'request_fallback').length, 1);
        peer.channel.onopen();
        peer.channel.onerror();
        t.mock.timers.tick(1000);
        assert.equal(remote.socket.sent.filter((message) => message.type === 'request_fallback').length, 1);
    });
}
