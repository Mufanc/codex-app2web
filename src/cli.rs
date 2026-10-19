use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about = "Stream the Codex desktop to a web browser")]
pub(crate) struct Cli {
    /// CDP port on localhost; detect it from the running Codex app when omitted
    #[arg(long, env = "CODEX_CDP_PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) cdp_port: Option<u16>,

    /// TCP listening address (defaults to 127.0.0.1:8787 without --unix-socket)
    #[arg(long, value_name = "ADDR:PORT", conflicts_with = "unix_socket")]
    pub(crate) listen: Option<SocketAddr>,

    /// Unix socket to bind; an existing path is never removed automatically
    #[arg(long, value_name = "PATH")]
    pub(crate) unix_socket: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ports_and_exclusive_listeners() {
        assert!(Cli::try_parse_from(["app", "--cdp-port", "0"]).is_err());
        assert!(Cli::try_parse_from(["app", "--cdp-port", "65536"]).is_err());
        assert!(Cli::try_parse_from(["app", "--listen", "bad"]).is_err());
        assert!(
            Cli::try_parse_from(["app", "--listen", "127.0.0.1:8787", "--unix-socket", "/tmp/test.sock",]).is_err()
        );

        let cli = Cli::try_parse_from(["app", "--cdp-port", "9341", "--unix-socket", "/tmp/test.sock"]).unwrap();
        assert_eq!(cli.cdp_port, Some(9341));
        assert!(cli.listen.is_none());
        assert!(!Cli::try_parse_from(["app", "--help"]).unwrap_err().use_stderr());
    }
}
