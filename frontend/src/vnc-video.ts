import RFB from '@novnc/novnc';
import { encodings } from '@novnc/encodings';

// shortcut: noVNC 1.7 lacks a public lossless switch; keep it pinned until an upstream API exists.
class LosslessRfb extends RFB {
    protected _sendEncodings() {
        const names = [
            'encodingCopyRect',
            'encodingTight',
            'encodingZRLE',
            'encodingHextile',
            'encodingRRE',
            'encodingRaw',
            'pseudoEncodingDesktopSize',
            'pseudoEncodingLastRect',
            'pseudoEncodingExtendedDesktopSize',
            'pseudoEncodingFence',
            'pseudoEncodingContinuousUpdates',
        ];
        const supported = names.map((name) => encodings[name]);

        // Omitting JPEG and its quality pseudo-encodings keeps Tight updates lossless.
        supported.push(encodings.pseudoEncodingCompressLevel0 + this.compressionLevel);
        RFB.messages.clientEncodings(this._sock, supported);
    }
}

export function createVncVideo(
    screen: HTMLElement,
    send: (data: ArrayBufferView<ArrayBuffer>) => void,
    signal: AbortSignal,
    onFrame: (width: number, height: number) => void,
    onError: () => void,
) {
    const pending: ArrayBuffer[] = [];
    let receiver: ((event: MessageEvent) => void) | null = null;
    let active = true;
    const channel = {
        binaryType: 'arraybuffer',
        protocol: '',
        readyState: WebSocket.OPEN as number,
        onopen: null,
        onerror: null,
        onclose: null as ((event: CloseEvent) => void) | null,
        get onmessage() {
            return receiver;
        },
        set onmessage(value: ((event: MessageEvent) => void) | null) {
            receiver = value;
            queueMicrotask(() => {
                if (!active || !receiver) return;
                for (const data of pending.splice(0)) receiver(new MessageEvent('message', { data }));
            });
        },
        send: (data: ArrayBufferView<ArrayBuffer>) => {
            if (active) send(data);
        },
        close: () => {
            channel.readyState = WebSocket.CLOSED;
            queueMicrotask(() => channel.onclose?.(new CloseEvent('close', { wasClean: true, code: 1000 })));
        },
    };
    const rfb = new LosslessRfb(screen, channel);

    rfb.viewOnly = true;
    rfb.focusOnClick = false;
    rfb.scaleViewport = true;
    rfb.resizeSession = false;
    rfb.background = 'transparent';
    rfb.compressionLevel = 2;

    const observer = new MutationObserver(() => {
        const canvas = screen.querySelector('canvas');

        if (canvas?.width && canvas.height) onFrame(canvas.width, canvas.height);
    });

    observer.observe(screen, {
        subtree: true,
        childList: true,
        attributes: true,
        attributeFilter: ['width', 'height'],
    });
    rfb.addEventListener('disconnect', () => {
        if (active) onError();
    });
    signal.addEventListener(
        'abort',
        () => {
            active = false;
            observer.disconnect();
            rfb.disconnect();
            pending.length = 0;
        },
        { once: true },
    );

    return (data: ArrayBuffer) => {
        if (!active) return;

        if (receiver) receiver(new MessageEvent('message', { data }));
        else pending.push(data);
    };
}
