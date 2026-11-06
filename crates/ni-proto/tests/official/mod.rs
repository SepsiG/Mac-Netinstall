//! The official `netinstall-cli` 7.25.1 captures in
//! `crates/ni-proto/tests/fixtures/official-7.25.1/` (format: see the
//! comment at the top of each file). Shared by ni-proto's conformance test
//! and ni-sim's replay test (`#[path]` include).
#![allow(dead_code)] // each test uses a different part

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall};
use ni_proto::{DeviceConfig, DeviceInfo, Frame, InstallPlan, Mac, PlanFile, MAX_CHUNK};
use std::path::{Path, PathBuf};

/// A server frame's payload: exact bytes, or file data of this length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Text(Vec<u8>),
    Data(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Server {
        src: Mac,
        dst: Mac,
        seq: u16,
        ack: u16,
        payload: Payload,
    },
    Device {
        seq: u16,
        payload: Vec<u8>,
    },
}

/// One captured run.
#[derive(Debug, Clone)]
pub struct Capture {
    /// File stem, e.g. `e10-e-sm`.
    pub name: String,
    /// The official tool's options, e.g. `["-e", "-sm", "mode.rsc"]`.
    pub args: Vec<String>,
    /// Packages as given on its command line, with their sizes.
    pub packages: Vec<(String, usize)>,
    pub device: Mac,
    pub hello: Vec<u8>,
    /// Frames after the hello, in order (`R` lines expanded).
    pub lines: Vec<Line>,
}

pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ni-proto/tests/fixtures/official-7.25.1")
}

/// Every capture, by name.
pub fn all() -> Vec<Capture> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    names.sort();
    names.iter().map(|p| load(p)).collect()
}

/// `"OFFR\n\0"`-style quoted text to bytes.
fn unquote(s: &str) -> Vec<u8> {
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("not quoted: {s}"));
    let mut out = Vec::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(u8::try_from(c).expect("ASCII"));
            continue;
        }
        out.push(match chars.next() {
            Some('n') => b'\n',
            Some('0') => 0,
            Some('\\') => b'\\',
            Some('"') => b'"',
            other => panic!("bad escape {other:?}"),
        });
    }
    out
}

fn load(path: &Path) -> Capture {
    let text = std::fs::read_to_string(path).unwrap();
    let mut cap = Capture {
        name: path.file_stem().unwrap().to_string_lossy().into_owned(),
        args: Vec::new(),
        packages: Vec::new(),
        device: Mac::ZERO,
        hello: Vec::new(),
        lines: Vec::new(),
    };
    let mut names = Vec::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
        let words: Vec<&str> = rest.split(' ').filter(|w| !w.is_empty()).collect();
        let num = |i: usize| -> u16 { words[i].parse().unwrap() };
        match tag {
            "args" => cap.args = words.iter().map(|w| w.to_string()).collect(),
            "packages" => names = words.iter().map(|w| w.to_string()).collect(),
            "size" => cap
                .packages
                .push((words[0].to_string(), words[1].parse().unwrap())),
            "device" => cap.device = words[0].parse().unwrap(),
            "hello" => cap.hello = unquote(rest),
            "S" => {
                let payload_text = rest.splitn(5, ' ').nth(4).unwrap();
                let payload = match payload_text.strip_prefix("data ") {
                    Some(n) => Payload::Data(n.parse().unwrap()),
                    None => Payload::Text(unquote(payload_text)),
                };
                cap.lines.push(Line::Server {
                    src: words[0].parse().unwrap(),
                    dst: words[1].parse().unwrap(),
                    seq: num(2),
                    ack: num(3),
                    payload,
                });
            }
            "D" => {
                assert_eq!(words[0], words[1], "a device reply has seq = ack");
                cap.lines.push(Line::Device {
                    seq: num(0),
                    payload: unquote(rest.splitn(3, ' ').nth(2).unwrap()),
                });
            }
            "R" => {
                let (first, count) = (num(0), num(1));
                for seq in first..first + count {
                    cap.lines.push(Line::Server {
                        src: Mac::ZERO,
                        dst: cap.device,
                        seq,
                        ack: seq - 2,
                        payload: Payload::Data(MAX_CHUNK),
                    });
                    cap.lines.push(Line::Device {
                        seq,
                        payload: message::RETR.to_vec(),
                    });
                }
            }
            other => panic!("{}: unknown line {other:?}", cap.name),
        }
    }
    assert_eq!(
        names,
        cap.packages
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
    );
    cap
}

/// NPK-looking bytes of `len` (deterministic, seeded by the name).
pub fn package(name: &str, len: usize) -> Bytes {
    let seed = name.bytes().fold(0u8, u8::wrapping_add);
    let mut v = message::NPK_MAGIC.to_vec();
    v.extend((v.len()..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    Bytes::from(v)
}

fn fixture_file(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

impl Capture {
    /// The device's hello, parsed.
    pub fn info(&self) -> DeviceInfo {
        let hello = Frame::new(self.device, Mac::BROADCAST, 1, 0, self.hello.clone());
        DeviceInfo::parse(&hello).unwrap()
    }

    /// What we install for the same options: synthetic packages of the
    /// captured sizes (system package first), the scripts and key file
    /// from the fixture directory.
    pub fn plan(&self) -> InstallPlan {
        let mut plan = InstallPlan {
            packages: self
                .packages
                .iter()
                .map(|(name, len)| PlanFile {
                    name: name.clone(),
                    data: package(name, *len),
                })
                .collect(),
            config: DeviceConfig::Keep,
            ..InstallPlan::default()
        };
        plan.system_package_first();
        let mut args = self.args.iter();
        let (mut default, mut empty, mut script) = (false, false, None);
        while let Some(a) = args.next() {
            let mut value = || fixture_file(args.next().expect("a file name"));
            match a.as_str() {
                "-r" => default = true,
                "-e" => empty = true,
                "-b" => plan.remove_branding = true,
                "-s" => script = Some(Bytes::from(value())),
                "-sm" => plan.mode_script = Some(Bytes::from(value())),
                "-k" => {
                    let key = String::from_utf8(value()).unwrap();
                    plan.license_key = key.replace(['\n', '\r'], "");
                }
                "--reboot" => plan.after_install = AfterInstall::RebootWithMessage,
                "--shutdown" => plan.after_install = AfterInstall::Shutdown,
                other => panic!("{}: unknown option {other}", self.name),
            }
        }
        plan.config = match (script, empty, default) {
            (Some(s), _, _) => DeviceConfig::Script(s),
            (None, true, _) => DeviceConfig::Empty,
            (None, false, true) => DeviceConfig::Default,
            (None, false, false) => DeviceConfig::Keep,
        };
        plan
    }
}
