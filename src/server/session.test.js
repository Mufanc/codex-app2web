(() => {
    window.RTCPeerConnection = undefined;
    const Socket = window.WebSocket;
    window.vncBytes = 0;
    window.WebSocket = class extends Socket {
        constructor(...args) {
            super(...args);
            this.addEventListener('message', event => {
                if (event.data instanceof ArrayBuffer) window.vncBytes += event.data.byteLength;
            });
        }
    };
})();
