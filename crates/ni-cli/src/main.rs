//! `ni-cli`: MikroTik Netinstall for macOS, on the command line.
//!
//! A thin layer over `ni-engine`; see `ni-cli --help`. Exit codes are in
//! [`cli::exit`].

mod cli;
mod logging;
mod output;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // `--help` and `--version` are "errors" printed to stdout.
            let _ = e.print();
            let code = if e.use_stderr() {
                cli::exit::USAGE
            } else {
                cli::exit::OK
            };
            return ExitCode::from(code);
        }
    };
    let _log_guard = cli
        .log_dir
        .clone()
        .or_else(logging::default_log_dir)
        .and_then(|dir| match logging::init(&dir, &cli.log_level) {
            Ok(guard) => Some(guard),
            Err(e) => {
                eprintln!("warning: logging disabled: {e}");
                None
            }
        });
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "ni-cli started");
    eprintln!("not implemented yet: {:?}", cli.command);
    ExitCode::from(cli::exit::ERROR)
}
