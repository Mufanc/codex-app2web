# CodexApp2Web

Turn the Codex desktop app into a web app accessible from a browser

- Captures the Codex UI and forwards input through the Chrome DevTools Protocol without requiring system screen-recording permission
- Streams video over WebRTC, with WebSocket as a fallback
- Proxies native Codex context menus so they can be used from the remote page

## Build and run

```sh
cargo build --release
./target/release/codex-app2web --cdp-port 9341
```

The server listens on `127.0.0.1:8787` by default

Codex must be started with a CDP debugging port, for example:

```sh
open -na /Applications/ChatGPT.app --args --remote-debugging-address=127.0.0.1 --remote-debugging-port=9341
```

## Public access

This project does not provide user authentication

When exposing it to the public internet, keep it bound to a local address or Unix socket and place it behind a reverse proxy such as Caddy with an authentication service such as Authelia
