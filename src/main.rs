mod app;
mod cdp;
mod cli;
mod input;
mod logging;
mod protocol;
mod server;
mod session;
mod video;

use clap::Parser;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    logging::init()?;

    let port = app::resolve_port(cli.cdp_port)?;

    let controller = session::controller::Controller::new();
    let cdp = cdp::connect(port, controller.subscribe());

    server::serve(cdp, controller, cli.listen, cli.unix_socket).await
}
