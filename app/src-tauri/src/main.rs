//! Mac Netinstall. `--list-interfaces` prints the interface list as JSON
//! and exits (to check, from a terminal, what the signed app sees — e.g.
//! whether macOS masks MAC addresses for it).

use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--list-interfaces") {
        return match mac_netinstall_app::core::list_interfaces() {
            Ok(list) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&list).expect("interfaces serialize")
                );
                ExitCode::SUCCESS
            }
            Err(f) => {
                eprintln!("{f}");
                ExitCode::FAILURE
            }
        };
    }
    mac_netinstall_app::run();
    ExitCode::SUCCESS
}
