mod cdp;
mod input;
mod server;
mod video;
mod webrtc_video;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    server::serve(cdp::connect()).await
}
