//! Golden conformance: for every captured run of the official
//! `netinstall-cli` 7.25.1, our [`ServerSession`] with the same options,
//! answered with the captured device replies, sends the same frames: MAC
//! fields, seq/ack, every control payload byte for byte, data frames of the
//! same sizes (package contents differ: ours are synthetic).

mod official;

use ni_proto::{Event, Frame, Mac, ServerSession, SessionConfig};
use official::{Line, Payload};
use std::time::Instant;

/// The session's next frame; time only moves on while it is pacing
/// (never past a reply deadline: the capture has every reply).
fn next_frame(s: &mut ServerSession, now: &mut Instant, capture: &str) -> Frame {
    if let Some(f) = s.poll_transmit(*now) {
        return f;
    }
    let at = s.poll_timeout().expect("the session has not ended");
    s.poll_transmit(at).map_or_else(
        || panic!("{capture}: the session waits for a reply the capture does not have"),
        |f| {
            *now = at;
            f
        },
    )
}

#[test]
fn the_fixtures_cover_every_option() {
    let names: Vec<String> = official::all().into_iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "e01-keep",
            "e02-r",
            "e03-e",
            "e04-b",
            "e05-s",
            "e06-r-s",
            "e09-r-sm",
            "e10-e-sm",
            "e11-r-s-sm",
            "e12-k",
            "e13-reboot",
            "e14-shutdown",
            "e15-r-b",
            "e16-e-b",
            "x09-two-packages-s",
            "x18-container-first"
        ]
    );
}

#[test]
fn our_session_sends_what_the_official_server_sends() {
    for cap in official::all() {
        let name = cap.name.as_str();
        let plan = cap.plan();
        let mut now = Instant::now();
        let mut s = ServerSession::new(cap.info(), &plan, SessionConfig::default(), now).unwrap();
        let mut sent = 0;
        for line in &cap.lines {
            match line {
                Line::Server {
                    src,
                    dst,
                    seq,
                    ack,
                    payload,
                } => {
                    let f = next_frame(&mut s, &mut now, name);
                    let at = format!("{name}: frame {seq}");
                    assert_eq!(
                        (f.src, f.dst, f.seq, f.ack),
                        (*src, *dst, *seq, *ack),
                        "{at}"
                    );
                    match payload {
                        Payload::Text(want) => assert_eq!(
                            String::from_utf8_lossy(&f.payload),
                            String::from_utf8_lossy(want),
                            "{at}"
                        ),
                        Payload::Data(len) => assert_eq!(f.payload.len(), *len, "{at}"),
                    }
                    // The wire form: the length field counts the NUL too.
                    let wire = f.encode();
                    assert_eq!(
                        usize::from(u16::from_le_bytes([wire[14], wire[15]])),
                        f.payload.len()
                    );
                    sent += 1;
                }
                Line::Device { seq, payload } => {
                    let reply = Frame::new(cap.device, Mac::ZERO, *seq, *seq, payload.clone());
                    s.handle_frame(&reply, now);
                }
            }
        }
        assert!(s.is_finished(), "{name}: more frames than the capture");
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done), "{name}");
        assert!(sent > 13_000, "{name}: {sent} frames compared");
    }
}
