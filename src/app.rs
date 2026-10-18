use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

mod process;

const BUNDLE_ID: &str = "com.openai.codex";
const DEFAULT_CDP_PORT: u16 = 9341;

pub(crate) fn resolve_port(explicit: Option<u16>) -> anyhow::Result<u16> {
    if let Some(port) = explicit {
        return Ok(port);
    }

    let bundle_id = NSString::from_str(BUNDLE_ID);
    let workspace = NSWorkspace::sharedWorkspace();
    let installed = workspace.URLsForApplicationsWithBundleIdentifier(&bundle_id);
    let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let mut paths = BTreeSet::new();

    for url in installed.iter() {
        if let Some(path) = url.path() {
            if let Ok(path) = PathBuf::from(path.to_string()).canonicalize() {
                paths.insert(path);
            }
        }
    }

    for app in running.iter() {
        let path = app
            .bundleURL()
            .and_then(|url| url.path())
            .context("Cannot locate the running Codex app; specify --cdp-port manually")?;
        paths.insert(PathBuf::from(path.to_string()).canonicalize()?);
    }

    ensure!(paths.len() <= 1 && running.len() <= 1,
        "Multiple Codex installations or instances found. Start the desired app manually and specify --cdp-port.\n{}",
        paths.iter().map(|path| launch_command(path, DEFAULT_CDP_PORT)).collect::<anyhow::Result<Vec<_>>>()?.join("\n"));

    let path = paths.into_iter().next().context(
        "Codex is not installed or registered with macOS. Start it manually and specify --cdp-port",
    )?;
    let command = launch_command(&path, DEFAULT_CDP_PORT)?;

    if let Some(app) = running.firstObject() {
        let args = process::arguments(app.processIdentifier())
            .context("Cannot read Codex launch arguments; specify --cdp-port manually")?;
        let port = debugging_port(&args)
            .with_context(|| format!("Quit Codex manually, then run:\n{command}"))?;

        if let Some(port) = port {
            eprintln!("Detected Codex CDP port: {port}");
            return Ok(port);
        }

        bail!("Codex is running without a remote debugging port. Quit it manually, then run:\n{command}");
    }

    eprintln!("Codex is not running.\n{command}");
    ensure!(confirm_start()?, "Codex was not started");

    let status = Command::new("/usr/bin/open")
        .args(["-na"])
        .arg(&path)
        .args(["--args", "--remote-debugging-address=127.0.0.1"])
        .arg(format!("--remote-debugging-port={DEFAULT_CDP_PORT}"))
        .status()
        .context("Failed to launch Codex")?;
    ensure!(status.success(), "open failed to launch Codex: {status}");

    Ok(DEFAULT_CDP_PORT)
}

fn confirm_start() -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        return Ok(false);
    }

    eprint!("Start Codex with remote debugging enabled? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;

    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn launch_command(path: &Path, port: u16) -> anyhow::Result<String> {
    let path = path
        .to_str()
        .context("Cannot display a non-UTF-8 app path as a shell command")?;
    let path = shlex::try_quote(path)?;

    Ok(format!("open -na {path} --args --remote-debugging-address=127.0.0.1 --remote-debugging-port={port}"))
}

fn debugging_port(args: &[String]) -> anyhow::Result<Option<u16>> {
    let mut result = None;
    let mut args = args.iter();

    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }

        let value = if arg == "--remote-debugging-port" {
            Some(
                args.next()
                    .context("Missing remote debugging port value")?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--remote-debugging-port=")
        };

        if let Some(value) = value {
            let port = value
                .parse::<u16>()
                .context("Invalid remote debugging port")?;
            ensure!(
                port != 0,
                "Dynamic remote debugging ports (0) are not supported"
            );
            ensure!(
                result.is_none(),
                "Multiple remote debugging port arguments; specify --cdp-port manually"
            );
            result = Some(port);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<u16>> {
        debugging_port(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn detects_only_valid_explicit_debugging_ports() {
        assert_eq!(
            parse(&["Codex", "--remote-debugging-port=9341"]).unwrap(),
            Some(9341)
        );
        assert_eq!(
            parse(&["Codex", "--remote-debugging-port", "9342"]).unwrap(),
            Some(9342)
        );
        assert_eq!(
            parse(&["Codex", "--", "--remote-debugging-port=9341"]).unwrap(),
            None
        );
        assert_eq!(parse(&["Codex"]).unwrap(), None);

        for args in [
            vec!["--remote-debugging-port=0"],
            vec!["--remote-debugging-port=65536"],
            vec!["--remote-debugging-port"],
            vec![
                "--remote-debugging-port=9341",
                "--remote-debugging-port=9342",
            ],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn quotes_app_path_for_shell_without_execution() {
        for path in [
            "/Apps/Codex.app",
            "/Apps/John's Codex.app",
            "/Apps/$(echo test) `test` \"app\".app",
        ] {
            let command = launch_command(Path::new(path), 9341).unwrap();
            assert_eq!(
                shlex::split(&command).unwrap(),
                [
                    "open",
                    "-na",
                    path,
                    "--args",
                    "--remote-debugging-address=127.0.0.1",
                    "--remote-debugging-port=9341",
                ]
            );
        }
        assert_eq!(resolve_port(Some(9342)).unwrap(), 9342);
    }
}
