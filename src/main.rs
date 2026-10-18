mod app;
mod cdp;
mod cli;
mod input;
mod server;
mod video;
mod webrtc_video;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let port = app::resolve_port(cli.cdp_port)?;

    server::serve(cdp::connect(port), cli.listen, cli.unix_socket).await
}
