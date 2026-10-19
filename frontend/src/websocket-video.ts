import { t } from './i18n.ts';

const CODEC = 'hev1.1.6.L153.B0';
const HEADER_BYTES = 9;

export function createWebSocketVideo(
    screen: HTMLVideoElement,
    signal: AbortSignal,
    onFrame: (width: number, height: number) => void,
    acknowledge: (sequence: number) => void,
    onError: (message: string) => void,
) {
    if (typeof VideoDecoder === 'undefined') throw new Error(t('video.unsupported'));

    const canvas = document.createElement('canvas');

    canvas.className = 'fallback-screen';
    canvas.setAttribute('aria-label', t('accessibility.screen'));

    const context = canvas.getContext('2d', { alpha: false });

    if (!context) throw new Error(t('video.canvasError'));

    screen.after(canvas);
    screen.hidden = true;

    let size = '';
    const pending = new Map<number, number>();
    let timestamp = 0;
    const decoder = new VideoDecoder({
        output(frame) {
            try {
                if (signal.aborted) return;

                if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
                    canvas.width = frame.displayWidth;
                    canvas.height = frame.displayHeight;
                }

                context.drawImage(frame, 0, 0);
                onFrame(frame.displayWidth, frame.displayHeight);

                const sequence = pending.get(frame.timestamp);

                if (sequence !== undefined) {
                    pending.delete(frame.timestamp);
                    acknowledge(sequence);
                }
            } finally {
                frame.close();
            }
        },
        error(error) {
            if (!signal.aborted) onError(t('video.decodeError', { error: error.message }));
        },
    });

    signal.addEventListener(
        'abort',
        () => {
            if (decoder.state !== 'closed') decoder.close();
            pending.clear();
            canvas.remove();
            screen.hidden = false;
        },
        { once: true },
    );

    return (data: ArrayBuffer) => {
        if (signal.aborted) return;

        try {
            if (data.byteLength <= HEADER_BYTES) throw new Error(t('video.incomplete'));

            const header = new DataView(data);
            const sequence = header.getUint32(0);
            const width = header.getUint16(4);
            const height = header.getUint16(6);
            const keyframe = header.getUint8(8) === 1;
            const nextSize = `${width}x${height}`;

            if (nextSize !== size) {
                if (!keyframe) throw new Error(t('video.missingKeyframe'));

                decoder.configure({ codec: CODEC, codedWidth: width, codedHeight: height, optimizeForLatency: true });
                size = nextSize;
            }

            const frameTimestamp = timestamp++;

            pending.set(frameTimestamp, sequence);
            decoder.decode(
                new EncodedVideoChunk({
                    type: keyframe ? 'key' : 'delta',
                    timestamp: frameTimestamp,
                    data: new Uint8Array(data, HEADER_BYTES),
                }),
            );
        } catch (error) {
            onError(t('video.decodeError', { error: String(error) }));
        }
    };
}
