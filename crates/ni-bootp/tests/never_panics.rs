//! Property tests: no datagram (and no accepted config) makes the parser
//! or the responder panic.

use ni_bootp::{BootArch, BootpConfig, BootpResponder, Outcome, Request};
use proptest::prelude::*;
use std::collections::HashMap;
use std::net::Ipv4Addr;

const VENDOR_CLASSES: &[&str] = &[
    "Mips_boot",
    "Mips_boot\0",
    "MMipsBoot",
    "ARM64__boot",
    "ARM__boot ",
    "X86_64__boot",
    "PXEClient:Arch:00000",
    "MSFT 5.0",
    "",
];

/// One DHCP/BOOTP option, biased towards the ones the responder reads.
fn option(server_ip: Ipv4Addr) -> impl Strategy<Value = Vec<u8>> {
    let server = server_ip.octets();
    prop_oneof![
        (0u8..=10).prop_map(|t| vec![53, 1, t]),
        prop::sample::select(VENDOR_CLASSES).prop_map(|vc| {
            let mut o = vec![60, vc.len() as u8];
            o.extend_from_slice(vc.as_bytes());
            o
        }),
        any::<[u8; 4]>().prop_map(|ip| [&[50u8, 4][..], &ip].concat()),
        Just([&[50u8, 4][..], &server].concat()),
        any::<[u8; 4]>().prop_map(|ip| [&[54u8, 4][..], &ip].concat()),
        Just([&[54u8, 4][..], &server].concat()),
        prop::collection::vec(any::<u8>(), 0..8),
        (any::<u8>(), prop::collection::vec(any::<u8>(), 0..20)).prop_map(|(code, data)| {
            let mut o = vec![code, data.len() as u8];
            o.extend_from_slice(&data);
            o
        }),
    ]
}

/// A request-shaped datagram: valid fixed header, random fields and options.
fn request(server_ip: Ipv4Addr) -> impl Strategy<Value = Vec<u8>> {
    (
        0u8..4,
        any::<u8>(),
        any::<[u8; 4]>(),
        any::<[u8; 4]>(),
        any::<bool>(),
        0u8..4,
        prop::collection::vec(option(server_ip), 0..8),
        0usize..80,
    )
        .prop_map(
            |(mac_low, hops, ciaddr, giaddr, cookie, extra_hops, opts, pad)| {
                let mut b = vec![0u8; 236];
                b[0] = 1;
                b[1] = 1;
                b[2] = 6;
                // Mostly direct requests, sometimes relayed.
                b[3] = if extra_hops == 0 { hops } else { 0 };
                b[12..16].copy_from_slice(&ciaddr);
                if extra_hops == 1 {
                    b[24..28].copy_from_slice(&giaddr);
                }
                b[28..34].copy_from_slice(&[2, 0, 0, 0, 0, mac_low]);
                if cookie {
                    b.extend_from_slice(&[99, 130, 83, 99]);
                }
                for o in opts {
                    b.extend_from_slice(&o);
                }
                b.resize(b.len() + pad, 0);
                b
            },
        )
}

/// A config that may or may not be accepted by `BootpResponder::new`.
fn config() -> impl Strategy<Value = BootpConfig> {
    (
        any::<u32>(),
        0u32..=32,
        any::<u32>(),
        any::<u32>(),
        any::<bool>(),
        any::<u32>(),
    )
        .prop_map(|(server, prefix, a, b, in_subnet, lease_secs)| {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            let (a, b) = if in_subnet {
                let net = server & mask;
                (net | (a & !mask), net | (b & !mask))
            } else {
                (a, b)
            };
            BootpConfig {
                server_ip: server.into(),
                netmask: mask.into(),
                pool_start: a.min(b).into(),
                pool_end: a.max(b).into(),
                boot_files: HashMap::from([
                    (BootArch::Mips, "mips".to_string()),
                    (BootArch::MMips, "mmips".to_string()),
                    (BootArch::Arm64, "arm64".to_string()),
                ]),
                lease_secs,
            }
        })
}

proptest! {
    #[test]
    fn parse_never_panics(buf in prop::collection::vec(any::<u8>(), 0..700)) {
        let _ = Request::parse(&buf);
    }

    #[test]
    fn handle_never_panics_on_random_bytes(
        cfg in config(),
        datagrams in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..400), 1..10),
    ) {
        if let Ok(mut r) = BootpResponder::new(cfg) {
            for d in datagrams {
                let _ = r.handle(&d);
            }
        }
    }

    #[test]
    fn handle_never_panics_on_request_sequences(
        (cfg, datagrams) in config().prop_flat_map(|cfg| {
            let reqs = prop::collection::vec(request(cfg.server_ip), 1..30);
            (Just(cfg), reqs)
        }),
    ) {
        let Ok(mut r) = BootpResponder::new(cfg.clone()) else {
            return Ok(());
        };
        for d in datagrams {
            if let Ok(Outcome::Reply(reply)) = r.handle(&d) {
                // Never hand out the server, network or broadcast address.
                let ip = u32::from(reply.assigned_ip);
                let mask = u32::from(cfg.netmask);
                prop_assert_ne!(reply.assigned_ip, cfg.server_ip);
                if mask.leading_ones() <= 30 {
                    prop_assert_ne!(ip, u32::from(cfg.server_ip) & mask);
                    prop_assert_ne!(ip, u32::from(cfg.server_ip) | !mask);
                }
                prop_assert!(reply.bytes.len() >= 300);
                prop_assert_eq!(r.lease_of(&reply.client_mac), Some(reply.assigned_ip));
            }
        }
    }
}

#[test]
fn whole_ipv4_space_config_is_handled() {
    // prefix /0, pool covering everything: allocation must not scan the range.
    let cfg = BootpConfig {
        server_ip: Ipv4Addr::new(10, 0, 0, 1),
        netmask: Ipv4Addr::UNSPECIFIED,
        pool_start: Ipv4Addr::UNSPECIFIED,
        pool_end: Ipv4Addr::BROADCAST,
        boot_files: HashMap::from([(BootArch::Mips, "mips".to_string())]),
        lease_secs: 60,
    };
    let mut r = BootpResponder::new(cfg).unwrap();
    let mut b = vec![0u8; 236];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b.extend_from_slice(&[99, 130, 83, 99, 60, 9]);
    b.extend_from_slice(b"Mips_boot");
    b.push(255);
    match r.handle(&b).unwrap() {
        Outcome::Reply(rep) => assert_eq!(rep.assigned_ip, Ipv4Addr::new(0, 0, 0, 1)),
        other => panic!("{other:?}"),
    }
}
