//! On the real machine, `list_interfaces` must report the real MAC.
//!
//! macOS hands `02:00:00:00:00:00` to ad-hoc (linker-) signed binaries such
//! as this test from both `getifaddrs` and the routing sysctl, and to their
//! child processes: `/sbin/ifconfig` started from here may print the
//! placeholder too. So the reference is `ifconfig` where it prints a real
//! address, else IOKit's `IOMACAddress` as printed by `/usr/sbin/ioreg`.

use ni_net::{list_interfaces, InterfaceKind};
use std::collections::HashMap;
use std::process::Command;

const PLACEHOLDERS: [&str; 2] = ["02:00:00:00:00:00", "00:00:00:00:00:00"];

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `ether xx:xx:xx:xx:xx:xx` address `ifconfig <name>` prints, if any.
fn ifconfig_ether(name: &str) -> Option<String> {
    run("/sbin/ifconfig", &[name])?.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("ether"))
            .then(|| words.next().map(str::to_uppercase))
            .flatten()
    })
}

/// BSD name -> `IOMACAddress` of its `IOEthernetController`, from
/// `ioreg -r -c IOEthernetController -l`: each controller prints its
/// `"IOMACAddress" = <001122334455>` before its interface's `"BSD Name"`.
fn ioreg_macs() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(text) = run(
        "/usr/sbin/ioreg",
        &["-r", "-c", "IOEthernetController", "-l"],
    ) else {
        return out;
    };
    let mut mac: Option<String> = None;
    for line in text.lines() {
        if let Some((_, v)) = line.split_once("\"IOMACAddress\" = <") {
            let hex = v.trim_end_matches('>');
            if hex.len() == 12 {
                let bytes: Vec<String> = (0..6)
                    .map(|i| hex[2 * i..2 * i + 2].to_uppercase())
                    .collect();
                mac = Some(bytes.join(":"));
            }
        } else if let Some((_, v)) = line.split_once("\"BSD Name\" = \"") {
            if let Some(m) = mac.take() {
                out.insert(v.trim_end_matches('"').to_string(), m);
            }
        }
    }
    out
}

/// CI runners are virtual machines with other NICs and MAC masking (see
/// `ni-net`'s `test_env`): skipped where `CI` is set, unless
/// `NI_HARDWARE_TESTS=1`.
fn skip_on_ci() -> bool {
    let ci = std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "false" && v != "0");
    let forced = std::env::var("NI_HARDWARE_TESTS").is_ok_and(|v| v == "1");
    let skip = ci && !forced;
    if skip {
        eprintln!("skipped on CI: needs the development Mac's NICs (NI_HARDWARE_TESTS=1 runs it)");
    }
    skip
}

#[test]
fn macs_match_ifconfig_or_iokit() {
    if skip_on_ci() {
        return;
    }
    if run("/sbin/ifconfig", &["lo0"]).is_none() {
        eprintln!("skipped: /sbin/ifconfig is unavailable");
        return;
    }
    let ioreg = ioreg_macs();
    let mut compared = 0;
    for iface in list_interfaces().unwrap() {
        let got = iface.mac.map(|m| m.to_string());
        assert!(
            !got.as_deref().is_some_and(|m| PLACEHOLDERS.contains(&m)),
            "{}: placeholder MAC must be reported as unknown",
            iface.name
        );
        let Some(ether) = ifconfig_ether(&iface.name) else {
            continue;
        };
        let expected = if !PLACEHOLDERS.contains(&ether.as_str()) {
            Some(ether.clone())
        } else if iface.kind == InterfaceKind::WiFi {
            // Masked, and the hardware address may not be the active
            // (private) one: must be unknown.
            None
        } else if let Some(hw) = ioreg.get(&iface.name) {
            Some(hw.clone())
        } else {
            eprintln!(
                "{}: no reference (ifconfig {ether}), got {got:?}",
                iface.name
            );
            continue;
        };
        eprintln!(
            "{}: ifconfig {ether}, expected {expected:?}, got {got:?}",
            iface.name
        );
        assert_eq!(got, expected, "{}", iface.name);
        compared += 1;
    }
    assert!(compared > 0, "no interface could be compared");
}
