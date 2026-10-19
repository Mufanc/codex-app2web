const CODEC = 'hev1.1.6.L153.B0';
const HEADER_BYTES = 9;

export function createWebSocketVideo(
    screen: HTMLVideoElement,
    signal: AbortSignal,
    onFrame: (width: number, height: number) => void,
    acknowledge: (sequence: number) => void,
    onError: (message: string) => void,
) {
    if (typeof VideoDecoder === 'undefined') throw new Error('此 Chrome 不支持 WebCodecs');

    const canvas = document.createElement('canvas');
    canvas.className = 'fallback-screen';
    canvas.setAttribute('aria-label', 'Codex 桌面画面');
    const context = canvas.getContext('2d', { alpha: false });

    if (!context) throw new Error('无法创建视频画布');

    screen.after(canvas);
    screen.hidden = true;
    let size = '';
    let pendingSequence = 0;
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
                acknowledge(pendingSequence);
            } finally {
                frame.close();
            }
        },
        error(error) { if (!signal.aborted) onError(`WebSocket 视频解码失败：${error.message}`); },
    });

    signal.addEventListener('abort', () => {
        if (decoder.state !== 'closed') decoder.close();
        canvas.remove();
        screen.hidden = false;
    }, { once: true });

    return (data: ArrayBuffer) => {
        if (signal.aborted) return;

        try {
            if (data.byteLength <= HEADER_BYTES) throw new Error('视频帧不完整');

            const header = new DataView(data);
            const sequence = header.getUint32(0);
            const width = header.getUint16(4);
            const height = header.getUint16(6);
            const keyframe = header.getUint8(8) === 1;
            const nextSize = `${width}x${height}`;

            if (nextSize !== size) {
                if (!keyframe) throw new Error('视频尺寸变化缺少关键帧');

                decoder.configure({ codec: CODEC, codedWidth: width, codedHeight: height, optimizeForLatency: true });
                size = nextSize;
            }

            pendingSequence = sequence;
            decoder.decode(new EncodedVideoChunk({
                type: keyframe ? 'key' : 'delta', timestamp: timestamp++, data: new Uint8Array(data, HEADER_BYTES),
            }));
        } catch (error) {
            onError(`WebSocket 视频解码失败：${String(error)}`);
        }
    };
}
