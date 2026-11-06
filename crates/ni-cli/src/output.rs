//! Printing engine events: JSON lines, or short English lines for people.

use ni_engine::{DeviceState, EngineEvent, Failure, Finding, Severity, WaitReason};
use ni_proto::Mac;
use std::collections::HashMap;

/// Turns events into output lines; keeps a little state so progress is
/// printed in 10 % steps instead of for every chunk.
#[derive(Default)]
pub struct Printer {
    json: bool,
    /// Last printed progress step (0-10) per device / per download.
    devices: HashMap<Mac, (String, u64)>,
    downloads: HashMap<String, u64>,
}

fn percent(done: u64, total: u64) -> u64 {
    (done.saturating_mul(100)).checked_div(total).unwrap_or(100)
}

impl Printer {
    pub fn new(json: bool) -> Self {
        Printer {
            json,
            ..Default::default()
        }
    }

    /// The line to print for `e`, if any.
    pub fn line(&mut self, e: &EngineEvent) -> Option<String> {
        if self.json {
            return Some(serde_json::to_string(e).expect("events serialize"));
        }
        match e {
            EngineEvent::Started(s) => Some(format!(
                "serving on {} ({}), fetching boot kernels...",
                s.interface, s.server_ip
            )),
            EngineEvent::Stopped => Some("stopped".into()),
            // `kernels_ready` / the `kernels_unavailable` error say it.
            EngineEvent::Kernels { .. } => None,
            EngineEvent::KernelsReady { version } => Some(format!(
                "boot kernels {version} ready; waiting for devices (Etherboot)"
            )),
            EngineEvent::Download { file, done, total } => {
                let total = (*total)?;
                let step = percent(*done, total) / 10;
                let last = self.downloads.insert(file.clone(), step);
                (last != Some(step) && step.is_multiple_of(5))
                    .then(|| format!("downloading {file}: {}%", step * 10))
            }
            EngineEvent::Discovered { mac, info } => Some(format!(
                "{mac}  found {} ({}), license {}",
                info.model, info.arch, info.license_id
            )),
            EngineEvent::Ignored { mac, reason } => Some(format!("{mac}  ignored: {reason}")),
            EngineEvent::Forgotten { mac } => Some(format!("{mac}  forgotten")),
            EngineEvent::OnlyMacChanged { mac: Some(mac) } => Some(format!("serving only {mac}")),
            EngineEvent::OnlyMacChanged { mac: None } => Some("serving every device".into()),
            EngineEvent::Device { mac, state } => self.device(*mac, state),
            EngineEvent::Error(f) => Some(format!("error: {}", failure(f))),
            EngineEvent::Warning(f) => Some(format!("warning: {}", failure(f))),
        }
    }

    fn device(&mut self, mac: Mac, state: &DeviceState) -> Option<String> {
        let text = match state {
            DeviceState::BootpSeen { boot_arch, ip } => {
                format!("booting ({boot_arch}), address {ip}")
            }
            DeviceState::KernelServing { sent, total } => {
                let step = percent(*sent, *total) / 10;
                let key = ("kernel".to_string(), step);
                if self.devices.get(&mac) == Some(&key) {
                    return None;
                }
                self.devices.insert(mac, key);
                format!("sending boot kernel: {}%", step * 10)
            }
            DeviceState::Discovered => return None,
            DeviceState::Waiting {
                reason: WaitReason::NoProfile,
            } => "waiting (no profile)".into(),
            DeviceState::Waiting {
                reason: WaitReason::Concurrency,
            } => "waiting for a free slot".into(),
            DeviceState::Preparing { version: None } => "preparing packages".into(),
            DeviceState::Preparing { version: Some(v) } => format!("preparing RouterOS {v}"),
            DeviceState::Flashing {
                phase, sent, total, ..
            } => {
                let step = percent(*sent, *total) / 10;
                let key = (phase.clone(), step);
                if self.devices.get(&mac) == Some(&key) {
                    return None;
                }
                self.devices.insert(mac, key);
                if phase == "uploading" {
                    format!("uploading: {}%", step * 10)
                } else {
                    phase.clone()
                }
            }
            DeviceState::Done => "done, rebooting".into(),
            DeviceState::Failed { code, params } => {
                let f = Failure {
                    code: code.clone(),
                    params: params.clone(),
                };
                format!("FAILED: {f}")
            }
        };
        Some(format!("{mac}  {text}"))
    }
}

/// What to do about some failures, for people.
pub fn hint(code: &str) -> Option<&'static str> {
    match code {
        "port_in_use" => Some(
            "another DHCP/TFTP/netinstall server is running (Internet Sharing's bootpd?); stop it first",
        ),
        "no_ipv4" => Some("give the interface a static IPv4 address in System Settings > Network"),
        "unsuitable_interface" => Some("RouterBOARDs boot over wired Ethernet only"),
        "no_mac" => Some(
            "the interface's real MAC address is unknown (macOS hides it from some apps); \
             use a wired Ethernet interface",
        ),
        _ => None,
    }
}

/// A failure for people: `code (params)`, plus a hint if there is one.
pub fn failure(f: &Failure) -> String {
    match hint(&f.code) {
        Some(h) => format!("{f}\n  hint: {h}"),
        None => f.to_string(),
    }
}

/// A preflight finding for people.
pub fn finding(f: &Finding) -> String {
    let level = match f.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    format!("{level}: {}", failure(&f.failure))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_engine::{DeviceInfoView, StartInfo};

    const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 1]);

    fn flashing(phase: &str, sent: u64) -> EngineEvent {
        EngineEvent::Device {
            mac: MAC,
            state: DeviceState::Flashing {
                version: "7.25.1".into(),
                phase: phase.into(),
                sent,
                total: 1000,
            },
        }
    }

    #[test]
    fn json_mode_prints_every_event() {
        let mut p = Printer::new(true);
        let line = p.line(&flashing("uploading", 1)).unwrap();
        assert_eq!(
            line,
            r#"{"event":"device","mac":"48:A9:8A:00:00:01","state":"flashing","version":"7.25.1","phase":"uploading","sent":1,"total":1000}"#
        );
        assert!(p.line(&flashing("uploading", 2)).is_some());
    }

    #[test]
    fn human_mode_prints_progress_in_steps() {
        let mut p = Printer::new(false);
        let lines: Vec<String> = [
            flashing("offering", 0),
            flashing("formatting", 0),
            flashing("uploading", 0),
            flashing("uploading", 50),
            flashing("uploading", 120),
            flashing("uploading", 1000),
        ]
        .iter()
        .filter_map(|e| p.line(e))
        .collect();
        assert_eq!(
            lines,
            [
                "48:A9:8A:00:00:01  offering",
                "48:A9:8A:00:00:01  formatting",
                "48:A9:8A:00:00:01  uploading: 0%",
                "48:A9:8A:00:00:01  uploading: 10%",
                "48:A9:8A:00:00:01  uploading: 100%",
            ]
        );
    }

    #[test]
    fn human_lines_for_other_events() {
        let mut p = Printer::new(false);
        let started = EngineEvent::Started(StartInfo {
            interface: "en7".into(),
            server_ip: "192.168.88.1".parse().unwrap(),
            bootp: "192.168.88.1:67".parse().unwrap(),
            tftp: "192.168.88.1:69".parse().unwrap(),
            netinstall: "192.168.88.1:5000".parse().unwrap(),
        });
        assert_eq!(
            p.line(&started).unwrap(),
            "serving on en7 (192.168.88.1), fetching boot kernels..."
        );
        let found = EngineEvent::Discovered {
            mac: MAC,
            info: DeviceInfoView {
                model: "hEX".into(),
                arch: "mmips".into(),
                license_id: "ABCD-1234".into(),
                min_os: "7.0".into(),
            },
        };
        assert_eq!(
            p.line(&found).unwrap(),
            "48:A9:8A:00:00:01  found hEX (mmips), license ABCD-1234"
        );
        assert_eq!(
            p.line(&EngineEvent::Forgotten { mac: MAC }).unwrap(),
            "48:A9:8A:00:00:01  forgotten"
        );
        assert_eq!(
            p.line(&EngineEvent::OnlyMacChanged { mac: Some(MAC) })
                .unwrap(),
            "serving only 48:A9:8A:00:00:01"
        );
        assert_eq!(
            p.line(&EngineEvent::OnlyMacChanged { mac: None }).unwrap(),
            "serving every device"
        );
        let failed = EngineEvent::Device {
            mac: MAC,
            state: DeviceState::failed(Failure::new("flash_timeout").with("phase", "formatting")),
        };
        assert_eq!(
            p.line(&failed).unwrap(),
            "48:A9:8A:00:00:01  FAILED: flash_timeout (phase: formatting)"
        );
        let dl = |done| EngineEvent::Download {
            file: "f.npk".into(),
            done,
            total: Some(100),
        };
        let lines: Vec<String> = [0, 10, 50, 51, 100]
            .iter()
            .filter_map(|d| p.line(&dl(*d)))
            .collect();
        assert_eq!(
            lines,
            [
                "downloading f.npk: 0%",
                "downloading f.npk: 50%",
                "downloading f.npk: 100%"
            ]
        );
        assert_eq!(
            finding(&Finding {
                severity: Severity::Warning,
                failure: Failure::new("link_down").with("interface", "en7"),
            }),
            "warning: link_down (interface: en7)"
        );
        let busy = Failure::new("port_in_use")
            .with("service", "bootp")
            .with("port", 67);
        assert_eq!(
            failure(&busy),
            "port_in_use (port: 67, service: bootp)\n  hint: another DHCP/TFTP/netinstall \
             server is running (Internet Sharing's bootpd?); stop it first"
        );
        assert_eq!(hint("flash_timeout"), None);
        assert!(hint("no_mac").is_some_and(|h| h.contains("MAC")));
    }
}
