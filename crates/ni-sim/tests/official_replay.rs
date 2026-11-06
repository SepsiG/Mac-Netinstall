//! Replay: the official `netinstall-cli` 7.25.1's frames from every
//! captured run, fed to our simulated device, draw exactly the replies the
//! device gave in the capture (the official server accepted them), and the
//! device ends up with the files, offer and `TERM` of that run.

#[path = "../../ni-proto/tests/official/mod.rs"]
mod official;

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall};
use ni_proto::{Frame, Mac};
use ni_sim::{DeviceSim, SimConfig};
use official::{Line, Payload};
use std::time::Instant;

#[test]
fn our_device_answers_the_official_server_like_the_capture() {
    for cap in official::all() {
        let name = cap.name.as_str();
        let plan = cap.plan();
        let files = plan.files();
        let now = Instant::now();
        let mut cfg = SimConfig::new(cap.device, "", "");
        cfg.info = cap.info();
        let mut sim = DeviceSim::new(cfg, now);
        sim.handle_timeout(now);
        let hello = sim.poll_transmit().expect("hello");
        assert_eq!(hello.payload.as_ref(), cap.hello.as_slice(), "{name}");
        // The file being sent and the bytes of it sent so far.
        let mut current: Option<(Bytes, usize)> = None;
        for line in &cap.lines {
            match line {
                Line::Server {
                    src,
                    dst,
                    seq,
                    ack,
                    payload,
                } => {
                    let bytes = match payload {
                        Payload::Text(p) => {
                            if let Some((file, _)) = message::parse_file_header(p) {
                                let data = files.iter().find(|(n, _)| *n == file).unwrap();
                                current = Some((data.1.clone(), 0));
                            }
                            Bytes::from(p.clone())
                        }
                        Payload::Data(len) => {
                            let (data, at) = current.as_mut().expect("data after a header");
                            let chunk = data.slice(*at..*at + len);
                            *at += len;
                            chunk
                        }
                    };
                    sim.handle_frame(&Frame::new(*src, *dst, *seq, *ack, bytes), now);
                }
                Line::Hello { .. } => unreachable!("complete captures have no re-sent hello"),
                Line::Device { seq, payload } => {
                    let reply = sim
                        .poll_transmit()
                        .unwrap_or_else(|| panic!("{name}: no reply to {seq}"));
                    assert_eq!(
                        (reply.src, reply.dst, reply.seq, reply.ack),
                        (cap.device, Mac::ZERO, *seq, *seq),
                        "{name}"
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&reply.payload),
                        String::from_utf8_lossy(payload),
                        "{name}: reply to {seq}"
                    );
                }
            }
            assert!(
                sim.violations().is_empty(),
                "{name}: {:?}",
                sim.violations()
            );
        }
        assert_eq!(sim.poll_transmit(), None, "{name}: TERM is not answered");
        assert!(sim.is_terminated(), "{name}");
        let got: Vec<(&str, &[u8])> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.as_slice()))
            .collect();
        let want: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
        assert!(got == want, "{name}: received files differ");
        assert_eq!(sim.offer(), Some(&plan.offer()), "{name}");
        let after = match cap.args.last().map(String::as_str) {
            Some("--reboot") => AfterInstall::RebootWithMessage,
            Some("--shutdown") => AfterInstall::Shutdown,
            _ => AfterInstall::Reboot,
        };
        assert_eq!(sim.after_install(), Some(after), "{name}");
    }
}
