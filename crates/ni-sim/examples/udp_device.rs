//! A simulated RouterBOARD in Netinstall mode on a real network: the
//! [`DeviceSim`] state machine on a UDP socket bound to `0.0.0.0:5000`,
//! saying hello to `255.255.255.255:5000` and answering whatever server
//! offers to install it. Used to check the simulator against the official
//! `netinstall-cli` (Linux) in a network namespace; see the Plan 5
//! roadmap entry. Standard library only.
//!
//! ```text
//! udp_device --mac 64:D1:54:AA:BB:02 --arch arm64 --model RB5009UG+S+ \
//!            --out DIR [--after-files official|pynetinstall|never|early] [--timeout 60]
//! ```
//!
//! Received files are written to `DIR`; the last line printed is
//! `done <offer> <TERM>` (exit 0), or `timeout` / `violation: ...` (exit 1).

use ni_proto::{Frame, Mac, PORT};
use ni_sim::{AfterFiles, DeviceSim, SimConfig};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mac: Mac = arg(&args, "--mac")
        .unwrap_or_else(|| "64:D1:54:AA:BB:02".into())
        .parse()
        .expect("--mac");
    let arch = arg(&args, "--arch").unwrap_or_else(|| "arm64".into());
    let model = arg(&args, "--model").unwrap_or_else(|| "RB5009UG+S+".into());
    let out = PathBuf::from(arg(&args, "--out").expect("--out DIR"));
    let timeout =
        Duration::from_secs(arg(&args, "--timeout").map_or(60, |s| s.parse().expect("--timeout")));
    let mut cfg = SimConfig::new(mac, &model, &arch);
    cfg.info.min_os = "7.1".into();
    cfg.after_files = match arg(&args, "--after-files").as_deref() {
        None | Some("official") => AfterFiles::Official,
        Some("pynetinstall") => AfterFiles::PyNetinstall,
        Some("never") => AfterFiles::NeverWtrm,
        Some("early") => AfterFiles::EarlyWtrm,
        Some("nack-file-end") => AfterFiles::NackFileEnd,
        Some("silent") => AfterFiles::Silent,
        Some(other) => panic!("--after-files {other}"),
    };
    std::fs::create_dir_all(&out).expect("output directory");

    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)).expect("bind 0.0.0.0:5000");
    sock.set_broadcast(true).expect("SO_BROADCAST");
    sock.set_read_timeout(Some(Duration::from_millis(10)))
        .expect("read timeout");
    let to = SocketAddr::from((Ipv4Addr::BROADCAST, PORT));
    let start = Instant::now();
    let mut sim = DeviceSim::new(cfg, start);
    let mut buf = vec![0u8; 2048];
    let mut frames_in = 0u64;
    loop {
        let now = Instant::now();
        sim.handle_timeout(now);
        while let Some(f) = sim.poll_transmit() {
            sock.send_to(&f.encode(), to).expect("send");
        }
        if let Some(v) = sim.violations().first() {
            println!("violation: {v}");
            return ExitCode::FAILURE;
        }
        if sim.is_terminated() {
            break;
        }
        if now - start > timeout {
            println!("timeout after {frames_in} frames");
            return ExitCode::FAILURE;
        }
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Ok(frame) = Frame::decode(&buf[..n]) {
                if frame.dst == mac {
                    frames_in += 1;
                }
                sim.handle_frame(&frame, Instant::now());
            }
        }
    }
    for f in sim.received_files() {
        std::fs::write(out.join(&f.name), &f.data).expect("write a received file");
        println!("file {} {}", f.name, f.data.len());
    }
    println!(
        "done {:?} {:?} after {frames_in} frames",
        sim.offer().expect("offered"),
        sim.after_install().expect("terminated")
    );
    ExitCode::SUCCESS
}
