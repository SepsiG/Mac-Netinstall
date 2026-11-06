use bytes::Bytes;
use ni_proto::{
    message, Event, FailReason, InstallPlan, Mac, Phase, PlanFile, ServerSession, SessionConfig,
};
use ni_sim::harness::{discover, pump, Direction};
use ni_sim::{DeviceSim, ReceivedFile, SimConfig};
use std::time::{Duration, Instant};

const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
const DEVICE: Mac = Mac([0x48, 0xa9, 0x8a, 0, 0, 0x01]);

fn pattern(len: usize, seed: u8) -> Bytes {
    Bytes::from(
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect::<Vec<u8>>(),
    )
}

fn setup(sim_cfg: SimConfig, plan: &InstallPlan) -> (ServerSession, DeviceSim, Instant) {
    let now = Instant::now();
    let mut sim = DeviceSim::new(sim_cfg, now);
    let info = discover(&mut sim, now);
    assert_eq!(info.mac, DEVICE);
    let server = ServerSession::new(SERVER, info, plan, SessionConfig::default(), now);
    (server, sim, now)
}

fn events(server: &mut ServerSession) -> Vec<Event> {
    std::iter::from_fn(|| server.poll_event()).collect()
}

#[test]
fn installs_package_and_config_script() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros-7.16-mmips.npk".into(),
            data: pattern(3000, 1),
        }],
        config_script: Some(Bytes::from_static(b"/system identity set name=lab\n")),
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
    assert_eq!(
        sim.received_files(),
        &[
            ReceivedFile {
                name: "routeros-7.16-mmips.npk".into(),
                data: pattern(3000, 1).to_vec()
            },
            ReceivedFile {
                name: message::CONFIG_SCRIPT_NAME.into(),
                data: b"/system identity set name=lab\n".to_vec()
            },
        ]
    );
    let ev = events(&mut server);
    assert_eq!(ev.last(), Some(&Event::Done));
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
            "done"
        ]
    );
}

#[test]
fn installs_multiple_packages_including_empty_and_exact_chunk_sizes() {
    let plan = InstallPlan {
        packages: vec![
            PlanFile {
                name: "routeros.npk".into(),
                data: pattern(2048, 2),
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
        config_script: None,
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
    assert_eq!(sim.received_files()[0].data, pattern(2048, 2).to_vec());
    assert!(sim.received_files()[2].data.is_empty());
    assert!(sim.is_terminated());
    assert_eq!(events(&mut server).last(), Some(&Event::Done));
}

#[test]
fn recovers_from_lost_device_replies() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(5000, 4),
        }],
        config_script: None,
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
    assert_eq!(sim.received_files()[0].data, pattern(5000, 4).to_vec());
}

#[test]
fn recovers_from_lost_server_frames() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(5000, 5),
        }],
        config_script: None,
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
            if dir == Direction::ToDevice && &f.payload[..] != message::TERM {
                to_device += 1;
                to_device % 4 != 0
            } else {
                true
            }
        },
    );

    assert_eq!(events(&mut server).last(), Some(&Event::Done));
    assert_eq!(sim.received_files()[0].data, pattern(5000, 5).to_vec());
    assert!(sim.is_terminated());
}

#[test]
fn file_header_without_reply_is_tolerated() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(100, 6),
        }],
        config_script: None,
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
    assert_eq!(sim.received_files()[0].data, pattern(100, 6).to_vec());
}

#[test]
fn silent_device_fails_with_timeout_in_offering() {
    let plan = InstallPlan {
        packages: vec![PlanFile {
            name: "routeros.npk".into(),
            data: pattern(10, 7),
        }],
        config_script: None,
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
    assert_eq!(
        events(&mut server).last(),
        Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
    );
}
