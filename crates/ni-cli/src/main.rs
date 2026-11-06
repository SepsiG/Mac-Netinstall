//! `ni-cli`: MikroTik Netinstall for macOS, on the command line.
//!
//! A thin layer over `ni-engine`; see `ni-cli --help`. Exit codes are in
//! [`cli::exit`]. Errors go to stderr as `error: <code> (<params>)`; with
//! `--json` they are also printed to stdout as one JSON object
//! `{"type":"error","code":..,"params":{..},"message":..}` (usage errors
//! have code `usage`), so a JSON consumer never has to parse stderr.

mod cli;
mod commands;
mod flash;
mod logging;
mod output;

use clap::Parser;
use std::process::ExitCode;

/// An error for people (stderr) and, with `--json`, also as one JSON
/// object on stdout: `{"type":"error","code":..,"params":{..},"message":..}`
/// (`message` is English, for logs; translate `code`).
fn report_error(json: bool, f: &ni_engine::Failure) {
    eprintln!("error: {}", output::failure(f));
    if json {
        let v = serde_json::json!({
            "type": "error",
            "code": f.code,
            "params": f.params,
            "message": f.to_string(),
        });
        println!("{v}");
    }
}

fn main() -> ExitCode {
    let json_requested = std::env::args().any(|a| a == "--json");
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // `--help` and `--version` are "errors" printed to stdout.
            let _ = e.print();
            let code = if e.use_stderr() {
                if json_requested {
                    let v = serde_json::json!({
                        "type": "error",
                        "code": "usage",
                        "params": {},
                        "message": e.kind().to_string(),
                    });
                    println!("{v}");
                }
                cli::exit::USAGE
            } else {
                cli::exit::OK
            };
            return ExitCode::from(code);
        }
    };
    let testing = std::env::var(cli::TESTING_ENV).is_ok_and(|v| v == "1");
    if let Err(f) = cli::check_testing_flags(&cli, testing) {
        report_error(cli.json, &f);
        return ExitCode::from(cli::exit::USAGE);
    }
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
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            return ExitCode::from(cli::exit::ERROR);
        }
    };
    let cli_json = cli.json;
    let code = match runtime.block_on(commands::run(cli)) {
        Ok(code) => code,
        Err(f) => {
            tracing::error!("{f}");
            report_error(cli_json, &f);
            cli::exit::ERROR
        }
    };
    ExitCode::from(code)
}
