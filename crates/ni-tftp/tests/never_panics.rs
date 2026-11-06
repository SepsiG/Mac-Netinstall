//! Property tests: no datagram and no config makes the codec, `accept` or
//! a running `Transfer` panic.

use bytes::Bytes;
use ni_tftp::{accept, Accept, FileMap, Packet, Request, Status, TransferConfig};
use proptest::prelude::*;
use std::time::{Duration, Instant};

fn files() -> FileMap {
    FileMap::from([
        ("kernel".to_string(), Bytes::from(vec![0x5a; 3000])),
        ("small".to_string(), Bytes::from_static(b"hello")),
        ("empty".to_string(), Bytes::new()),
    ])
}

fn transfer_config() -> impl Strategy<Value = TransferConfig> {
    (
        prop_oneof![
            Just(Duration::ZERO),
            Just(Duration::MAX),
            (0u64..5_000).prop_map(Duration::from_millis),
            any::<u64>().prop_map(Duration::from_nanos),
        ],
        prop_oneof![0u32..10, Just(u32::MAX)],
        any::<bool>(),
        prop_oneof![0usize..20, 0usize..70_000, Just(usize::MAX)],
        any::<bool>(),
    )
        .prop_map(
            |(timeout, max_retransmits, honor_blksize, max_blksize, honor_timeout)| {
                TransferConfig {
                    timeout,
                    max_retransmits,
                    honor_blksize,
                    max_blksize,
                    honor_timeout,
                }
            },
        )
}

fn option_value() -> impl Strategy<Value = String> {
    prop_oneof![
        (0u64..70_000).prop_map(|v| v.to_string()),
        any::<u64>().prop_map(|v| v.to_string()),
        Just("-1".to_string()),
        Just(String::new()),
        "[a-z0-9]{0,6}",
    ]
}

/// An RRQ (sometimes WRQ) with plausible and implausible options.
fn rrq() -> impl Strategy<Value = Vec<u8>> {
    (
        any::<bool>(),
        prop::sample::select(vec![
            "kernel",
            "/kernel",
            "small",
            "empty",
            "nope",
            "../kernel",
        ]),
        prop::sample::select(vec!["octet", "OCTET", "netascii", "mail", ""]),
        prop::collection::vec(
            (
                prop::sample::select(vec!["tsize", "blksize", "BLKSIZE", "timeout", "x"]),
                option_value(),
            ),
            0..6,
        ),
        0usize..4,
    )
        .prop_map(|(write, name, mode, opts, pad)| {
            let req = Request {
                filename: name.to_string(),
                mode: mode.to_string(),
                options: opts.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            };
            let mut b = if write {
                Packet::Wrq(req)
            } else {
                Packet::Rrq(req)
            }
            .encode();
            b.resize(b.len() + pad, 0);
            b
        })
}

/// What the client does next while a transfer runs.
#[derive(Debug, Clone)]
enum Step {
    Datagram(Vec<u8>),
    Ack(u16),
    Error,
    Advance(Duration),
    Poll,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..40).prop_map(Step::Datagram),
        (0u16..12).prop_map(Step::Ack),
        any::<u16>().prop_map(Step::Ack),
        Just(Step::Error),
        (0u64..10_000).prop_map(|ms| Step::Advance(Duration::from_millis(ms))),
        Just(Step::Poll),
    ]
}

proptest! {
    #[test]
    fn parse_never_panics(buf in prop::collection::vec(any::<u8>(), 0..600)) {
        let _ = Packet::parse(&buf);
    }

    #[test]
    fn parse_never_yields_empty_option_names(buf in rrq()) {
        if let Ok(Packet::Rrq(r) | Packet::Wrq(r)) = Packet::parse(&buf) {
            prop_assert!(r.options.iter().all(|(k, _)| !k.is_empty()));
        }
    }

    #[test]
    fn accept_never_panics_on_random_bytes(
        buf in prop::collection::vec(any::<u8>(), 0..600),
        cfg in transfer_config(),
    ) {
        let _ = accept(&buf, &files(), &cfg, Instant::now());
    }

    #[test]
    fn accepted_transfers_never_panic(
        buf in rrq(),
        cfg in transfer_config(),
        steps in prop::collection::vec(step(), 0..60),
    ) {
        let mut now = Instant::now();
        let Accept::Start(mut t) = accept(&buf, &files(), &cfg, now) else {
            return Ok(());
        };
        prop_assert!((8..=65464).contains(&t.blksize()) || t.blksize() == 512);
        for s in steps {
            match s {
                Step::Datagram(d) => t.handle_packet(&d, now),
                Step::Ack(n) => t.handle_packet(&Packet::Ack(n).encode(), now),
                Step::Error => t.handle_packet(b"\x00\x05\x00\x00stop\x00", now),
                Step::Advance(d) => {
                    now = now.checked_add(d).unwrap_or(now);
                    t.handle_timeout(now);
                }
                Step::Poll => {
                    let _ = t.poll_transmit(now);
                }
            }
            let _ = t.poll_timeout();
            prop_assert!(t.acked_bytes() <= t.size());
            if t.is_finished() {
                prop_assert_eq!(t.poll_timeout(), None);
                prop_assert_ne!(t.status(), &Status::Running);
            }
        }
    }
}
