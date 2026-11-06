//! Golden conformance: for every captured run of the official
//! `netinstall-cli` 7.25.1, our [`ServerSession`] with the same options,
//! answered with the captured device replies, sends the same frames: MAC
//! fields, seq/ack, every control payload byte for byte, data frames of the
//! same sizes, and the data of every file is the plan's (package contents
//! are synthetic; the scripts are the fixture files `user.rsc` and
//! `mode.rsc`, byte-identical to what the official server delivered in the
//! spike's runs).

mod official;

use bytes::Bytes;
use ni_proto::{message, Event, FailReason, Frame, Mac, ServerSession, SessionConfig};
use official::{Line, Payload};
use std::time::{Duration, Instant};

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
        let files = plan.files();
        let mut now = Instant::now();
        let mut s = ServerSession::new(cap.info(), &plan, SessionConfig::default(), now).unwrap();
        let mut sent = 0;
        // The file being sent and the bytes of it sent so far.
        let mut current: Option<(String, Bytes, usize)> = None;
        let mut scripts_checked = 0;
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
                        Payload::Text(want) => {
                            assert_eq!(
                                String::from_utf8_lossy(&f.payload),
                                String::from_utf8_lossy(want),
                                "{at}"
                            );
                            if let Some((file, _)) = message::parse_file_header(want) {
                                let data = files.iter().find(|(n, _)| *n == file).unwrap();
                                current = Some((file, data.1.clone(), 0));
                            }
                        }
                        Payload::Data(len) => {
                            assert_eq!(f.payload.len(), *len, "{at}");
                            let (file, data, done) = current.as_mut().expect("data after a header");
                            assert_eq!(f.payload, data.slice(*done..*done + len), "{at}: {file}");
                            *done += len;
                            if file.ends_with(".scr") {
                                scripts_checked += 1;
                            }
                        }
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
                Line::Hello { .. } => unreachable!("complete captures have no re-sent hello"),
            }
        }
        // Every script with data was compared byte for byte.
        let scripts = files
            .iter()
            .filter(|(n, d)| n.ends_with(".scr") && !d.is_empty())
            .count();
        assert_eq!(scripts_checked, scripts, "{name}");
        assert!(s.is_finished(), "{name}: more frames than the capture");
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done), "{name}");
        assert!(sent > 13_000, "{name}: {sent} frames compared");
    }
}

/// x04: a device that refuses every offer. The official server offers
/// again (the same frame) each time the device says hello again, about once
/// a second, forever. Ours offers again on a timer
/// ([`SessionConfig::offer_retry`], 1 s, so the pace matches) whether or
/// not a hello came, and gives up after [`SessionConfig::max_retransmits`]
/// more offers (`flash_rejected`), so a device that will never accept does
/// not hold a slot forever. Deliberate difference; every offer we do send
/// is byte-identical to the captured ones.
#[test]
fn a_nack_to_the_offer_is_retried_like_the_official_server_but_bounded() {
    let cap = official::load(&official::dir().join("nack/x04-nack-offer.txt"));
    let cfg = SessionConfig::default();
    let start = Instant::now();
    let mut now = start;
    let mut s = ServerSession::new(cap.info(), &cap.plan(), cfg.clone(), now).unwrap();
    let (mut ours, mut theirs, mut hellos) = (Vec::new(), 0, Vec::new());
    for line in &cap.lines {
        match line {
            Line::Server {
                src,
                dst,
                seq,
                ack,
                payload,
            } => {
                theirs += 1;
                if s.is_finished() {
                    continue;
                }
                let f = next_frame(&mut s, &mut now, &cap.name);
                let Payload::Text(want) = payload else {
                    panic!("only offers")
                };
                assert_eq!(
                    (f.src, f.dst, f.seq, f.ack, &f.payload[..]),
                    (*src, *dst, *seq, *ack, &want[..])
                );
                ours.push(now - start);
            }
            Line::Device { seq, payload } => {
                let reply = Frame::new(cap.device, Mac::ZERO, *seq, *seq, payload.clone());
                s.handle_frame(&reply, now);
            }
            Line::Hello { at_ms } => hellos.push(*at_ms),
        }
    }
    // The official server: one offer per hello, about 1 s apart, still
    // going when the capture stopped.
    assert_eq!(theirs, 29);
    assert_eq!(
        hellos.len(),
        theirs,
        "a hello after each NACK (the last one unanswered)"
    );
    assert!(hellos
        .windows(2)
        .all(|w| (990..=1020).contains(&(w[1] - w[0]))));
    // Ours: the first offer and max_retransmits more, 1 s apart, then
    // rejected.
    let want: Vec<Duration> = (0..=cfg.max_retransmits)
        .map(|i| cfg.offer_retry * i)
        .collect();
    assert_eq!(ours, want);
    assert!(s.is_finished());
    let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
    assert_eq!(
        events.last(),
        Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
    );
}
