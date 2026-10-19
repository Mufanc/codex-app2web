(() => {
    const Peer = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends Peer {
        constructor(config) { super({ ...config, iceTransportPolicy: 'relay' }); }
    };

    const Socket = window.WebSocket;
    window.maxPendingFrames = 0;
    window.WebSocket = class extends Socket {
        pending = new Set();

        constructor(...args) {
            super(...args);
            this.addEventListener('message', event => {
                if (!(event.data instanceof ArrayBuffer)) return;
                this.pending.add(new DataView(event.data).getUint32(0));
                window.maxPendingFrames = Math.max(window.maxPendingFrames, this.pending.size);
            });
        }

        send(data) {
            const message = JSON.parse(data);
            if (message.type !== 'video_ack') return super.send(data);

            setTimeout(() => {
                this.pending.delete(message.sequence);
                if (this.readyState === Socket.OPEN) super.send(data);
            }, 200);
        }
    };
})();
