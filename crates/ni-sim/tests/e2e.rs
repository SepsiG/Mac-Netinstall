use bytes::Bytes;
use ni_proto::{
    message, AfterInstall, DeviceConfig, Event, FailReason, Frame, InstallPlan, Mac, Offer, Phase,
    PlanError, PlanFile, ServerSession, SessionConfig, MAX_CHUNK,
};
use ni_sim::harness::{discover, pump, pump_with, Direction};
use ni_sim::{AfterFiles, DeviceSim, ReceivedFile, SimConfig};
use std::time::{Duration, Instant};

const DEVICE: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x01]);

/// Deterministic test data without short periods (so duplicated or swapped
/// chunks are detectable).
fn pattern(len: usize, seed: u8) -> Bytes {
    Bytes::from(
        (0..len as u32)
            .map(|i| ((i.wrapping_mul(2_654_435_761) >> 24) as u8).wrapping_add(seed))
            .collect::<Vec<u8>>(),
    )
}

/// NPK-looking test data: the NPK magic followed by [`pattern`] bytes.
fn npk(len: usize, seed: u8) -> Bytes {
    assert!(len >= message::NPK_MAGIC.len());
    let mut v = message::NPK_MAGIC.to_vec();
    v.extend_from_slice(&pattern(len - message::NPK_MAGIC.len(), seed));
    Bytes::from(v)
}

fn setup(sim_cfg: SimConfig, plan: &InstallPlan) -> (ServerSession, DeviceSim, Instant) {
    let now = Instant::now();
    let mut sim = DeviceSim::new(sim_cfg, now);
    let info = discover(&mut sim, now);
    assert_eq!(info.mac, DEVICE);
    let server = ServerSession::new(info, plan, SessionConfig::default(), now).unwrap();
    (server, sim, now)
}

fn assert_no_violations(sim: &DeviceSim) {
    assert!(
        sim.violations().is_empty(),
        "protocol violations: {:?}",
        sim.violations()
    );
}

fn one_package(data: Bytes) -> InstallPlan {
    InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data,
        }],
        ..InstallPlan::default()
    }
}

fn events(server: &mut ServerSession) -> Vec<Event> {
    std::iter::from_fn(|| server.poll_event()).collect()
}

#[test]
fn installs_package_and_config_script() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros-7.16-mmips.npk".into(),
            data: npk(3000, 1),
        }],
        config: DeviceConfig::Script(Bytes::from_static(b"/system identity set name=lab\n")),
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );

    assert!(sim.is_terminated());
    assert_no_violations(&sim);
    assert_eq!(
        sim.received_files(),
        &[
            ReceivedFile {
                name: "routeros-7.16-mmips.npk".into(),
                data: npk(3000, 1).to_vec()
            },
            ReceivedFile {
                name: message::CONFIG_SCRIPT_NAME.into(),
                data: b"/system identity set name=lab\n".to_vec()
            },
        ]
    );
    let ev = events(&mut server);
    assert_eq!(ev.last(), Some(&Event::Done));
    assert_eq!(server.phase(), &Phase::Done);
    let last_progress = ev.iter().rev().find_map(|e| match e {
        Event::Progress {
            bytes_sent,
            bytes_total,
            ..
        } => Some((*bytes_sent, *bytes_total)),
        _ => None,
    });
    assert_eq!(
        last_progress,
        Some((plan.total_bytes(), plan.total_bytes()))
    );
    let phases: Vec<&str> = ev
        .iter()
        .filter_map(|e| match e {
            Event::PhaseChanged(p) => Some(p.name()),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            "offering",
            "formatting",
            "preparing",
            "uploading",
            "uploading",
            "finishing",
            "rebooting",
        ]
    );
}

#[test]
fn installs_multiple_packages_including_empty_and_exact_chunk_sizes() {
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: npk(2048, 2),
            },
            PlanFile {
                name: "wifi-qcom.npk".into(),
                data: pattern(1, 3),
            },
            PlanFile {
                name: "empty.npk".into(),
                data: Bytes::new(),
            },
        ],
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hAP ax2", "arm64"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );

    let names: Vec<&str> = sim
        .received_files()
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(names, ["routeros.npk", "wifi-qcom.npk", "empty.npk"]);
    assert_eq!(sim.received_files()[0].data, npk(2048, 2).to_vec());
    assert!(sim.received_files()[2].data.is_empty());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
}

#[test]
fn recovers_from_lost_device_replies() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(5000, 4),
        }],
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);
    let mut to_server = 0;

    // Drop every 3rd device frame.
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |dir, _| {
            if dir == Direction::ToServer {
                to_server += 1;
                to_server % 3 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(5000, 4).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn recovers_from_lost_server_frames() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(5000, 5),
        }],
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);
    let mut to_device = 0;

    // Drop every 4th server frame, except TERM (which is never retransmitted).
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |dir, f| {
            if dir == Direction::ToDevice && message::parse_term(&f.payload).is_none() {
                to_device += 1;
                to_device % 4 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(5000, 5).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn file_header_without_reply_is_tolerated() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(100, 6),
        }],
        ..InstallPlan::default()
    };
    let mut cfg = SimConfig::new(DEVICE, "RB951", "mipsbe");
    cfg.resend_interval = Duration::from_secs(3600); // no device-side resends
    let (mut server, mut sim, now) = setup(cfg, &plan);
    let mut last_to_device = Bytes::new();

    // Drop the device's answer to every FILE header.
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| match dir {
            Direction::ToDevice => {
                last_to_device = f.payload.clone();
                true
            }
            Direction::ToServer => message::parse_file_header(&last_to_device).is_none(),
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(100, 6).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn silent_device_fails_with_timeout_in_offering() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(10, 7),
        }],
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "RB951", "mipsbe"), &plan);

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(600),
        |dir, _| dir == Direction::ToDevice,
    );

    assert_eq!(server.phase(), &Phase::Failed);
    let ev = events(&mut server);
    assert_eq!(
        ev.last(),
        Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
    );
    assert!(!ev.contains(&Event::PhaseChanged(Phase::Failed)));
}

/// Classify a server → device frame for the transcript.
fn kind(payload: &[u8]) -> String {
    if let Some(o) = message::parse_offer(payload) {
        format!("OFFR keep={}", o.keep_config)
    } else if payload.is_empty() {
        "EMPTY".into()
    } else if payload == message::FILE_END {
        "FILE_END".into()
    } else if let Some(after) = message::parse_term(payload) {
        format!("TERM {after:?}")
    } else if let Some((name, size)) = message::parse_file_header(payload) {
        format!("FILE {name} {size}")
    } else {
        format!("CHUNK {}", payload.len())
    }
}

#[test]
fn transcript_follows_the_official_flow() {
    let script = Bytes::from_static(b"/system identity set name=gold\n");
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: npk(1500, 10),
            },
            PlanFile {
                name: "extra.npk".into(),
                data: pattern(10, 11),
            },
        ],
        config: DeviceConfig::Script(script.clone()),
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    let mut sent: Vec<(String, u16, u16)> = Vec::new();

    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| {
            if dir == Direction::ToDevice {
                sent.push((kind(&f.payload), f.seq, f.ack));
            }
            true
        },
    );

    // OFFR, format, prepare; per file: header, chunks, poll; one more poll
    // (answered WTRM); TERM. The k-th frame has seq k, ack k - 2.
    let script_len = script.len();
    let expected: Vec<(String, u16, u16)> = [
        "OFFR keep=false".to_string(),
        "EMPTY".into(),
        "EMPTY".into(),
        "FILE routeros.npk 1500".into(),
        "CHUNK 1024".into(),
        "CHUNK 476".into(),
        "EMPTY".into(),
        "FILE extra.npk 10".into(),
        "CHUNK 10".into(),
        "EMPTY".into(),
        format!("FILE autorun.scr {script_len}"),
        format!("CHUNK {script_len}"),
        "EMPTY".into(),
        "EMPTY".into(),
        "TERM Reboot".into(),
    ]
    .into_iter()
    .zip(1u16..)
    .map(|(k, seq)| (k, seq, seq.saturating_sub(2)))
    .collect();
    assert_eq!(sent, expected);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn every_configuration_reaches_the_device() {
    let script = Bytes::from_static(b"/system identity set name=s\n");
    let mode = Bytes::from_static(b":log info mode\n");
    /// Configuration, mode script, files after the package.
    type Case = (DeviceConfig, Option<Bytes>, Vec<(&'static str, usize)>);
    let cases: Vec<Case> = vec![
        (DeviceConfig::Keep, None, vec![]),
        (DeviceConfig::Default, None, vec![]),
        (
            DeviceConfig::Default,
            Some(mode.clone()),
            vec![("mode.scr", 15)],
        ),
        (DeviceConfig::Empty, None, vec![("autorun.scr", 0)]),
        (
            DeviceConfig::Empty,
            Some(mode.clone()),
            vec![("mode.scr", 15), ("autorun.scr", 0)],
        ),
        (
            DeviceConfig::Script(script.clone()),
            Some(mode.clone()),
            vec![("autorun.scr", 28), ("mode.scr", 15)],
        ),
    ];
    for (config, mode_script, extra) in cases {
        let keep = config == DeviceConfig::Keep;
        let plan = InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: npk(1100, 20),
            }],
            config: config.clone(),
            mode_script,
            remove_branding: true,
            license_key: "KEY-1".into(),
            after_install: AfterInstall::Shutdown,
        };
        let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
        pump(
            &mut server,
            &mut sim,
            now,
            Duration::from_secs(60),
            |_, _| true,
        );
        assert_eq!(events(&mut server).last(), Some(&Event::Done), "{config:?}");
        assert_no_violations(&sim);
        let mut want = vec![("routeros.npk", 1100)];
        want.extend(extra);
        let got: Vec<(&str, usize)> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.len()))
            .collect();
        assert_eq!(got, want, "{config:?}");
        assert_eq!(
            sim.offer(),
            Some(&Offer {
                license_key: "KEY-1".into(),
                keep_config: keep,
                remove_branding: true
            })
        );
        assert_eq!(sim.after_install(), Some(AfterInstall::Shutdown));
    }
}

#[test]
fn a_pynetinstall_era_device_finishes_through_the_file_end_fallback() {
    for after in [AfterFiles::PyNetinstall, AfterFiles::NeverWtrm] {
        let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
        sim_cfg.after_files = after;
        let plan = one_package(npk(2000, 21));
        let (mut server, mut sim, now) = setup(sim_cfg, &plan);
        let mut sent = Vec::new();
        let end = pump(
            &mut server,
            &mut sim,
            now,
            Duration::from_secs(60),
            |dir, f| {
                if dir == Direction::ToDevice {
                    sent.push(kind(&f.payload));
                }
                true
            },
        );
        assert_eq!(events(&mut server).last(), Some(&Event::Done), "{after:?}");
        assert!(sim.is_terminated());
        assert_no_violations(&sim);
        let cfg = SessionConfig::default();
        let polls = sent.iter().skip(5).filter(|k| *k == "EMPTY").count();
        assert_eq!(polls, cfg.finish_polls as usize, "{after:?}");
        assert_eq!(sent[sent.len() - 2..], ["FILE_END", "TERM Reboot"]);
        // The polls were paced: the fallback costs finish_polls - 1 intervals.
        assert!(end.now >= now + cfg.poll_interval * (cfg.finish_polls - 1));
    }
}

#[test]
fn a_device_that_stops_early_fails_loudly() {
    let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
    sim_cfg.after_files = AfterFiles::EarlyWtrm;
    let mut plan = one_package(npk(2000, 22));
    plan.config = DeviceConfig::Script(Bytes::from_static(b"/x\n"));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |_, _| true,
    );
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::EndedEarly { remaining: 1 }))
    );
    assert!(!sim.is_terminated(), "no TERM after a failure");
}

#[test]
fn a_refused_offer_is_repeated_then_rejected() {
    let mut sim_cfg = SimConfig::new(DEVICE, "hEX", "mmips");
    sim_cfg.reject_offer = true;
    let plan = one_package(npk(100, 23));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    let mut offers = 0;
    let end = pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| {
            if dir == Direction::ToDevice {
                assert_eq!((f.seq, f.ack), (1, 0));
                offers += 1;
            }
            true
        },
    );
    let cfg = SessionConfig::default();
    assert_eq!(offers, 1 + cfg.max_retransmits);
    assert!(end.now >= now + cfg.offer_retry * cfg.max_retransmits);
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
    );
    assert_no_violations(&sim);
}

#[test]
fn slow_format_within_format_timeout_succeeds() {
    let cfg = SessionConfig::default();
    let delay = cfg.reply_timeout * 3;
    assert!(cfg.reply_timeout < delay && delay < cfg.format_timeout);
    let mut sim_cfg = SimConfig::new(DEVICE, "CCR2004", "arm64");
    sim_cfg.format_delay = delay;
    let plan = one_package(npk(2000, 12));
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);

    let end = pump(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(300),
        |_, _| true,
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert!(end.now >= now + delay, "the format delay really elapsed");
    assert_eq!(sim.received_files()[0].data, npk(2000, 12).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

/// Documents a risk rather than desired behaviour: the session retransmits
/// unanswered frames with the same seq, assuming the device de-duplicates
/// them (pyNetinstall never retransmits). If a device does not, a lost reply
/// on a mid-file chunk makes the device append that chunk twice. This has to
/// be verified on real hardware.
#[test]
fn without_device_dedup_a_lost_reply_corrupts_the_file() {
    let mut sim_cfg = SimConfig::new(DEVICE, "RB951", "mipsbe");
    sim_cfg.dedup = false;
    // No device-side re-sends, so the server has to retransmit.
    sim_cfg.resend_interval = Duration::from_secs(3600);
    let data = npk(4 * MAX_CHUNK, 14);
    let plan = one_package(data.clone());
    let (mut server, mut sim, now) = setup(sim_cfg, &plan);
    // OFFR=1, format=2, prepare=3, header=4, chunk 1=5, chunk 2=6.
    let chunk2_seq = 6;
    let mut dropped = false;

    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(120),
        |dir, f| {
            if dir == Direction::ToServer && f.seq == chunk2_seq && !dropped {
                dropped = true;
                return Vec::new();
            }
            vec![f]
        },
    );

    assert!(dropped);
    let received = &sim.received_files()[0].data;
    assert_ne!(received, &data.to_vec(), "corruption is detectable");
    // The device stored chunk 2 twice and considers the file complete
    // after chunk 3.
    let c = |i: usize| &data[i * MAX_CHUNK..(i + 1) * MAX_CHUNK];
    assert_eq!(received, &[c(0), c(1), c(1), c(2)].concat());
}

#[test]
fn frames_from_other_macs_are_ignored() {
    const OTHER: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x02]);
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: npk(2500, 15),
        }],
        config: DeviceConfig::Script(Bytes::from_static(b"/x\n")),
        ..InstallPlan::default()
    };
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    let mut other = DeviceSim::new(SimConfig::new(OTHER, "hEX", "mmips"), now);
    other.handle_timeout(now);
    let other_hello = other.poll_transmit().expect("hello from the second device");
    let mut injected = 0;

    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, f| match dir {
            Direction::ToDevice => vec![f],
            Direction::ToServer => {
                injected += 1;
                // A second device's hello, and a reply from it with matching
                // seq/ack that would abort the session if it were accepted.
                let spoof = Frame::new(OTHER, Mac::ZERO, f.seq, f.ack, message::NACK);
                vec![other_hello.clone(), spoof, f]
            }
        },
    );

    assert!(injected > 10);
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, npk(2500, 15).to_vec());
    assert!(sim.is_terminated());
    assert_no_violations(&sim);
}

#[test]
fn nack_mid_upload_fails_with_rejected() {
    let plan = one_package(npk(3000, 16));
    let (mut server, mut sim, now) = setup(SimConfig::new(DEVICE, "hEX", "mmips"), &plan);
    // Chunk 2 has seq 6 (see above); the device rejects it.
    pump_with(
        &mut server,
        &mut sim,
        now,
        Duration::from_secs(60),
        |dir, mut f| {
            if dir == Direction::ToServer && f.seq == 6 {
                f.payload = Bytes::from_static(b"NACK\nwrite failed\n");
            }
            vec![f]
        },
    );

    assert_eq!(server.phase(), &Phase::Failed);
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Rejected { phase: "uploading" }))
    );
}

#[test]
fn invalid_plans_are_rejected_before_anything_is_sent() {
    let now = Instant::now();
    let mut sim = DeviceSim::new(SimConfig::new(DEVICE, "hEX", "mmips"), now);
    let info = discover(&mut sim, now);
    let new = |plan: &InstallPlan| {
        ServerSession::new(info.clone(), plan, SessionConfig::default(), now).err()
    };
    assert_eq!(new(&InstallPlan::default()), Some(PlanError::NoPackages));
    assert_eq!(
        new(&one_package(pattern(100, 17))),
        Some(PlanError::NotAnNpk {
            name: "routeros.npk".into()
        })
    );
}
