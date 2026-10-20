declare module '@novnc/novnc' {
    export default class RFB extends EventTarget {
        constructor(target: HTMLElement, channel: object);
        viewOnly: boolean;
        focusOnClick: boolean;
        scaleViewport: boolean;
        resizeSession: boolean;
        background: string;
        compressionLevel: number;
        disconnect(): void;
        protected _sock: unknown;
        protected _sendEncodings(): void;
        static messages: { clientEncodings(socket: unknown, encodings: number[]): void };
    }
}

declare module '@novnc/encodings' {
    export const encodings: Record<string, number>;
}
