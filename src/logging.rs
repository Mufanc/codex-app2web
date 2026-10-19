use std::io::{self, IsTerminal};

use tracing_subscriber::EnvFilter;

pub(crate) fn init() -> anyhow::Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(tracing::Level::INFO.into())
        .with_regex(false)
        .from_env()?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .try_init()
        .map_err(|error| anyhow::anyhow!("Cannot initialize logging: {error}"))?;

    Ok(())
}
