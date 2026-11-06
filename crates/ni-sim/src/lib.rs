//! Simulated MikroTik device in Netinstall mode (sans-IO).
//!
//! Behaviour mirrors the device the official `netinstall-cli` 7.25.1
//! expects (the spike's `sim.py`, which that server accepted in every
//! captured run): broadcasts 8-field hello frames until offered, answers
//! each server frame with `seq = ack = <server seq>`, puts zeros in
//! `[6:12]` of its replies, and periodically re-sends its last reply (real
//! devices do this too).
//!
//! The simulator is deliberately strict: it only accepts the official
//! frame sequence,
//!
//! ```text
//! OFFR\n<key>\n<keep>\n<brand>\n\0 → empty (format) → empty (prepare)
//!   → loop { FILE header → chunks (none for an empty file) → empty poll }
//!   → empty polls until it answers WTRM → TERM\n<message>\0
//! ```
//!
//! How it answers the polls after a file is [`SimConfig::after_files`]:
//! besides the official behaviour, a pyNetinstall-era device (`WTRM` only
//! to `FILE\n`), one that never says `WTRM`, and one that stops early.
//! Frames whose `ack` is not `seq - 2` (0 for the first two), stale frames
//! older than the last one accepted and frames from other devices are
//! ignored or flagged. Anything else out of line (wrong order, an offer
//! with malformed fields, oversized chunks, non-final chunks shorter than
//! `MAX_CHUNK`, more data than announced, a script before any package) is
//! answered with `NACK` and recorded in [`DeviceSim::violations`], so
//! step-order regressions fail tests.

pub mod harness;
#[cfg(feature = "lan")]
pub mod lan;

use bytes::Bytes;
use ni_proto::message::{self, AfterInstall, Offer};
use ni_proto::{DeviceInfo, Frame, Mac, MAX_CHUNK};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How the device answers the empty polls that follow a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFiles {
    /// `RETR` to the first poll after a file, `WTRM` to a poll right after
    /// another poll (inferred from the official server's behaviour; what
    /// `sim.py --after-files-reply auto` does).
    Official,
    /// `RETR` to every poll, `WTRM` only to pyNetinstall's `FILE\n`.
    PyNetinstall,
    /// `RETR` to every poll and to `FILE\n`: never `WTRM`.
    NeverWtrm,
    /// `WTRM` to the first poll after the first file: stops early.
    EarlyWtrm,
    /// `RETR` to the polls until this long after the last file was received
    /// (a device busy writing its flash), then `WTRM`. `WTRM` to `FILE\n`.
    SlowWtrm(Duration),
    /// `RETR` to every poll, `NACK` to `FILE\n`; then waits for `TERM`.
    NackFileEnd,
    /// Never answers a poll or `FILE\n`; then waits for `TERM`. Only for
    /// single-file plans (it cannot tell the last file).
    Silent,
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub info: DeviceInfo,
    pub hello_interval: Duration,
    pub resend_interval: Duration,
    /// Treat a re-sent server frame (same seq) as a duplicate and only repeat
    /// the previous answer. When `false` it is processed again as if new
    /// (a re-sent chunk is appended twice), modelling a device that does not
    /// de-duplicate. Default `true`.
    pub dedup: bool,
    /// How long formatting takes before the device answers `STRT`. Until
    /// then it only re-sends its previous reply. Default zero.
    pub format_delay: Duration,
    /// Refuse the install: answer `OFFR` with `NACK` (as a device short of
    /// space would) and keep saying hello. Default `false`.
    pub reject_offer: bool,
    /// Default [`AfterFiles::Official`].
    pub after_files: AfterFiles,
    /// `[6:12]` of its replies (`[6:12]` of a hello is
    /// [`DeviceInfo::hello_dst`], the broadcast address unless changed).
    /// Default zeros, the only value the official server handled cleanly
    /// (see the research doc, §5). A server that mirrors it (ours, the
    /// official one) then sends frames whose `[0:6]` is this value; with the
    /// device's own MAC those carry the device as `src` (see
    /// [`DeviceSim::handle_frame`]).
    pub reply_dst: Mac,
}

impl SimConfig {
    pub fn new(mac: Mac, model: &str, arch: &str) -> Self {
        Self {
            info: DeviceInfo {
                mac,
                tag: ni_proto::HELLO_TAG.into(),
                license_id: "SIM-0000".into(),
                license_key: String::new(),
                model: model.into(),
                arch: arch.into(),
                min_os: "7.0".into(),
                extra: vec!["0".into(), "0".into()],
                hello_dst: Mac::BROADCAST,
            },
            hello_interval: Duration::from_secs(1),
            resend_interval: Duration::from_millis(500),
            dedup: true,
            format_delay: Duration::ZERO,
            reject_offer: false,
            after_files: AfterFiles::Official,
            reply_dst: Mac::ZERO,
        }
    }
}

/// A file the simulated device received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedFile {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Broadcasting hellos; expects `OFFR`.
    Discovering,
    /// Expects the empty "format" frame.
    Offered,
    /// Formatting; silent (apart from re-sends) until `until`.
    Formatting {
        until: Instant,
    },
    /// Expects the empty "prepare" frame.
    Formatted,
    /// Expects the first `FILE` header.
    Ready,
    Receiving {
        name: String,
        size: u64,
        data: Vec<u8>,
    },
    /// A file was received; expects polls, the next `FILE` header (after a
    /// poll) or `FILE\n`. `polls`: polls since the file.
    AfterFile {
        polls: u32,
    },
    /// Said `WTRM`; expects `TERM`.
    Finished,
    Terminated,
}

/// Outcome of processing one in-order server frame.
type Step = Result<(State, Vec<&'static [u8]>), (State, String)>;

pub struct DeviceSim {
    cfg: SimConfig,
    state: State,
    last_seq: Option<u16>,
    last_reply: Option<Frame>,
    tx: VecDeque<Frame>,
    next_timer: Option<Instant>,
    received: Vec<ReceivedFile>,
    /// When the last file was complete.
    file_done_at: Option<Instant>,
    offer: Option<Offer>,
    after_install: Option<AfterInstall>,
    violations: Vec<String>,
}

impl DeviceSim {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        Self {
            cfg,
            state: State::Discovering,
            last_seq: None,
            last_reply: None,
            tx: VecDeque::new(),
            next_timer: Some(now),
            received: Vec::new(),
            file_done_at: None,
            offer: None,
            after_install: None,
            violations: Vec::new(),
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.cfg.info
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    pub fn received_files(&self) -> &[ReceivedFile] {
        &self.received
    }

    /// The offer the device accepted.
    pub fn offer(&self) -> Option<&Offer> {
        self.offer.as_ref()
    }

    /// What the `TERM` frame asked for, once received.
    pub fn after_install(&self) -> Option<AfterInstall> {
        self.after_install
    }

    /// Protocol violations by the server, in order of occurrence.
    pub fn violations(&self) -> &[String] {
        &self.violations
    }

    pub fn is_terminated(&self) -> bool {
        self.state == State::Terminated
    }

    pub fn poll_transmit(&mut self) -> Option<Frame> {
        self.tx.pop_front()
    }

    pub fn poll_timeout(&self) -> Option<Instant> {
        self.next_timer
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        match self.next_timer {
            Some(t) if t <= now => {}
            _ => return,
        }
        match self.state {
            State::Terminated => self.next_timer = None,
            State::Discovering => {
                let hello = Frame::new(
                    self.cfg.info.mac,
                    self.cfg.info.hello_dst,
                    1,
                    0,
                    self.cfg.info.to_payload(),
                );
                self.tx.push_back(hello);
                self.next_timer = Some(now + self.cfg.hello_interval);
            }
            State::Formatting { until } if until <= now => {
                self.state = State::Formatted;
                if let Some(seq) = self.last_seq {
                    self.reply(seq, message::STRT);
                }
                self.schedule(now);
            }
            _ => {
                if let Some(reply) = &self.last_reply {
                    self.tx.push_back(reply.clone());
                }
                self.schedule(now);
            }
        }
    }

    /// Feed a frame seen on the wire. Frames for other devices and the
    /// device's own frames looped back (a reply, `seq == ack`, or a hello)
    /// are ignored. A server frame may carry the device's MAC as `src`: a
    /// mirroring server echoes [`SimConfig::reply_dst`] there, and a server
    /// frame never has `seq == ack` (its `ack` is `seq - 2`, 0 for the first
    /// two).
    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let me = self.cfg.info.mac;
        let own =
            frame.src == me && (frame.seq == frame.ack || frame.payload.starts_with(b"DSCV\n"));
        if self.state == State::Terminated || frame.dst != me || own {
            return;
        }
        if self.last_seq.is_some_and(|last| frame.seq < last) {
            // Stale or delayed duplicate of an older frame.
            return;
        }
        let ack = frame.seq.saturating_sub(2);
        if frame.ack != ack {
            self.violation(format!(
                "frame seq {} carries ack {}, expected {ack}",
                frame.seq, frame.ack
            ));
            self.nack(frame.seq);
            return;
        }
        let repeated = self.last_seq == Some(frame.seq);
        if repeated && self.cfg.dedup {
            // Server re-sent its last frame: repeat our answer.
            if let Some(reply) = &self.last_reply {
                self.tx.push_back(reply.clone());
            }
            return;
        }
        let next = self.last_seq.map_or(1, |s| s.wrapping_add(1));
        if frame.seq != next && !repeated {
            self.violation(format!("frame seq {}, expected {next}", frame.seq));
            self.nack(frame.seq);
            return;
        }
        let state = std::mem::replace(&mut self.state, State::Discovering);
        let replies = match self.process(state, &frame.payload, now) {
            Ok((next, replies)) => {
                self.state = next;
                replies
            }
            Err((prev, why)) => {
                self.state = prev;
                self.violation(format!("seq {}: {why}", frame.seq));
                self.nack(frame.seq);
                return;
            }
        };
        if self.state == State::Discovering && replies.first() == Some(&message::NACK) {
            // A refused offer: answered, but the same frame may come again.
            self.nack(frame.seq);
            self.schedule(now);
            return;
        }
        self.last_seq = Some(frame.seq);
        for payload in replies {
            self.reply(frame.seq, payload);
        }
        self.schedule(now);
    }

    fn process(&mut self, state: State, payload: &[u8], now: Instant) -> Step {
        use message::{RETR, STRT, WTRM, YACK};
        let what = describe(payload);
        match state {
            State::Discovering => match message::parse_offer(payload) {
                Some(_) if self.cfg.reject_offer => Ok((State::Discovering, vec![message::NACK])),
                Some(offer) => {
                    self.offer = Some(offer);
                    Ok((State::Offered, vec![YACK]))
                }
                None => Err((State::Discovering, format!("expected OFFR, got {what}"))),
            },
            State::Offered if payload.is_empty() => {
                if self.cfg.format_delay.is_zero() {
                    Ok((State::Formatted, vec![STRT]))
                } else {
                    let until = now + self.cfg.format_delay;
                    Ok((State::Formatting { until }, vec![]))
                }
            }
            State::Formatted if payload.is_empty() => Ok((State::Ready, vec![RETR])),
            State::Ready => self.header(State::Ready, payload, now),
            State::AfterFile { polls } if payload.is_empty() => {
                let wtrm = match self.cfg.after_files {
                    AfterFiles::Official => polls > 0,
                    AfterFiles::PyNetinstall | AfterFiles::NeverWtrm | AfterFiles::NackFileEnd => {
                        false
                    }
                    AfterFiles::EarlyWtrm => true,
                    AfterFiles::SlowWtrm(after) => self
                        .file_done_at
                        .is_some_and(|t| now.saturating_duration_since(t) >= after),
                    AfterFiles::Silent => {
                        return Ok((State::AfterFile { polls: polls + 1 }, vec![]));
                    }
                };
                if wtrm {
                    Ok((State::Finished, vec![WTRM]))
                } else {
                    Ok((State::AfterFile { polls: polls + 1 }, vec![RETR]))
                }
            }
            State::AfterFile { polls } if payload == message::FILE_END => {
                match self.cfg.after_files {
                    AfterFiles::PyNetinstall | AfterFiles::SlowWtrm(_) => {
                        Ok((State::Finished, vec![WTRM]))
                    }
                    AfterFiles::NeverWtrm => Ok((State::Finished, vec![RETR])),
                    AfterFiles::NackFileEnd => Ok((State::Finished, vec![message::NACK])),
                    AfterFiles::Silent => Ok((State::Finished, vec![])),
                    AfterFiles::Official | AfterFiles::EarlyWtrm => Err((
                        State::AfterFile { polls },
                        "FILE\\n although the device would have said WTRM".into(),
                    )),
                }
            }
            State::AfterFile { polls } if polls == 0 => Err((
                State::AfterFile { polls },
                format!("expected the poll after a file, got {what}"),
            )),
            State::AfterFile { polls } => self.header(State::AfterFile { polls }, payload, now),
            State::Receiving {
                name,
                size,
                mut data,
            } => {
                let error = if payload.len() > MAX_CHUNK {
                    Some(format!(
                        "chunk of {} bytes exceeds {MAX_CHUNK}",
                        payload.len()
                    ))
                } else if payload.is_empty() {
                    Some("empty chunk while receiving".to_string())
                } else if ((data.len() + payload.len()) as u64) < size && payload.len() != MAX_CHUNK
                {
                    Some(format!(
                        "non-final chunk of {} bytes, expected {MAX_CHUNK}",
                        payload.len()
                    ))
                } else if (data.len() + payload.len()) as u64 > size {
                    Some(format!(
                        "{name}: {} bytes received, only {size} announced",
                        data.len() + payload.len()
                    ))
                } else {
                    None
                };
                if let Some(why) = error {
                    return Err((State::Receiving { name, size, data }, why));
                }
                data.extend_from_slice(payload);
                if (data.len() as u64) < size {
                    return Ok((State::Receiving { name, size, data }, vec![RETR]));
                }
                self.received.push(ReceivedFile { name, data });
                self.file_done_at = Some(now);
                Ok((State::AfterFile { polls: 0 }, vec![RETR]))
            }
            State::Finished => match message::parse_term(payload) {
                Some(after) => {
                    self.after_install = Some(after);
                    Ok((State::Terminated, vec![]))
                }
                None => Err((State::Finished, format!("expected TERM, got {what}"))),
            },
            other => {
                let why = format!("unexpected {what} in state {}", state_name(&other));
                Err((other, why))
            }
        }
    }

    /// A `FILE` header: start receiving (an empty file is complete at once).
    fn header(&mut self, state: State, payload: &[u8], now: Instant) -> Step {
        let Some((name, size)) = message::parse_file_header(payload) else {
            let why = format!("expected FILE header, got {}", describe(payload));
            return Err((state, why));
        };
        let script = name == message::CONFIG_SCRIPT_NAME || name == message::MODE_SCRIPT_NAME;
        if self.received.is_empty() && script {
            return Err((state, "script before any package".into()));
        }
        if size == 0 {
            self.received.push(ReceivedFile {
                name,
                data: Vec::new(),
            });
            self.file_done_at = Some(now);
            return Ok((State::AfterFile { polls: 0 }, vec![message::RETR]));
        }
        let data = Vec::new();
        Ok((State::Receiving { name, size, data }, vec![message::RETR]))
    }

    fn reply(&mut self, seq: u16, payload: &'static [u8]) {
        let reply = Frame::new(
            self.cfg.info.mac,
            self.cfg.reply_dst,
            seq,
            seq,
            Bytes::from_static(payload),
        );
        self.last_reply = Some(reply.clone());
        self.tx.push_back(reply);
    }

    /// Answer `NACK` without treating the frame as accepted (it does not
    /// become the reply that is re-sent).
    fn nack(&mut self, seq: u16) {
        let reply = Frame::new(
            self.cfg.info.mac,
            self.cfg.reply_dst,
            seq,
            seq,
            Bytes::from_static(message::NACK),
        );
        self.tx.push_back(reply);
    }

    fn violation(&mut self, why: String) {
        self.violations.push(why);
    }

    fn schedule(&mut self, now: Instant) {
        self.next_timer = match self.state {
            State::Terminated => None,
            State::Discovering => Some(now + self.cfg.hello_interval),
            State::Formatting { until } => Some(until.min(now + self.cfg.resend_interval)),
            _ => Some(now + self.cfg.resend_interval),
        };
    }
}

fn state_name(state: &State) -> &'static str {
    match state {
        State::Discovering => "Discovering",
        State::Offered => "Offered",
        State::Formatting { .. } => "Formatting",
        State::Formatted => "Formatted",
        State::Ready => "Ready",
        State::Receiving { .. } => "Receiving",
        State::AfterFile { .. } => "AfterFile",
        State::Finished => "Finished",
        State::Terminated => "Terminated",
    }
}

/// Short human-readable description of a server payload.
fn describe(payload: &[u8]) -> String {
    if payload.is_empty() {
        return "empty frame".into();
    }
    if let Some((name, size)) = message::parse_file_header(payload) {
        return format!("FILE header {name} ({size} bytes)");
    }
    let head = &payload[..payload.len().min(8)];
    format!(
        "{} byte frame {:?}",
        payload.len(),
        String::from_utf8_lossy(head)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_proto::MAX_CHUNK;

    const DEVICE: Mac = Mac([0xaa; 6]);
    const OFFER: &[u8] = b"OFFR\n\nkeep\nremoveBranding\n\0";
    const TERM: &[u8] = b"TERM\n\0";

    fn sim_with(f: impl FnOnce(&mut SimConfig)) -> (DeviceSim, Instant) {
        let now = Instant::now();
        let mut cfg = SimConfig::new(DEVICE, "hEX", "mmips");
        f(&mut cfg);
        let mut sim = DeviceSim::new(cfg, now);
        sim.handle_timeout(now);
        assert!(sim.poll_transmit().is_some(), "hello");
        (sim, now)
    }

    /// Feed server frames with consecutive seq numbers starting at `*seq + 1`
    /// (`[0:6]` zeros, `ack = seq - 2`) and return the payloads of all replies.
    fn feed(sim: &mut DeviceSim, seq: &mut u16, payloads: &[&[u8]], now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for p in payloads {
            *seq += 1;
            let f = Frame::new(Mac::ZERO, DEVICE, *seq, seq.saturating_sub(2), p.to_vec());
            sim.handle_frame(&f, now);
            while let Some(r) = sim.poll_transmit() {
                assert_eq!((r.seq, r.ack), (*seq, *seq));
                assert_eq!((r.src, r.dst), (DEVICE, Mac::ZERO));
                out.push(r.payload.to_vec());
            }
        }
        out
    }

    fn header(name: &str, size: u64) -> Vec<u8> {
        message::file_header(name, size)
    }

    fn strs(replies: &[Vec<u8>]) -> Vec<&str> {
        replies
            .iter()
            .map(|r| std::str::from_utf8(r).unwrap())
            .collect()
    }

    #[test]
    fn hello_has_eight_fields_to_the_broadcast_slot() {
        let now = Instant::now();
        let mut sim = DeviceSim::new(SimConfig::new(DEVICE, "hEX", "mmips"), now);
        sim.handle_timeout(now);
        let hello = sim.poll_transmit().unwrap();
        assert_eq!(
            (hello.src, hello.dst, hello.seq, hello.ack),
            (DEVICE, Mac::BROADCAST, 1, 0)
        );
        assert_eq!(
            &hello.payload[..],
            b"DSCV\nSIM-0000\n\nhEX\nmmips\n7.0\n0\n0\n"
        );
    }

    #[test]
    fn accepts_the_official_sequence() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let h2 = header(message::CONFIG_SCRIPT_NAME, 2);
        let h3 = header(message::MODE_SCRIPT_NAME, 0);
        let replies = feed(
            &mut sim,
            &mut seq,
            &[
                OFFER, b"", b"", &h1, b"abc", b"", &h2, b"xy", b"", &h3, b"", b"", TERM,
            ],
            now,
        );
        assert_eq!(
            strs(&replies),
            [
                "YACK\n", "STRT", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR", "RETR",
                "RETR", "WTRM"
            ]
        );
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
        let names: Vec<(&str, usize)> = sim
            .received_files()
            .iter()
            .map(|f| (f.name.as_str(), f.data.len()))
            .collect();
        assert_eq!(names, [("a.npk", 3), ("autorun.scr", 2), ("mode.scr", 0)]);
        assert_eq!(
            sim.offer(),
            Some(&Offer {
                license_key: String::new(),
                keep_config: true,
                remove_branding: false
            })
        );
        assert_eq!(sim.after_install(), Some(AfterInstall::Reboot));
    }

    #[test]
    fn other_after_files_behaviours() {
        let h1 = header("a.npk", 1);
        let h2 = header("b.npk", 1);
        let run = |after: AfterFiles, frames: &[&[u8]]| {
            let (mut sim, now) = sim_with(|c| c.after_files = after);
            let mut seq = 0;
            let mut all: Vec<&[u8]> = vec![OFFER, b"", b"", &h1, b"a"];
            all.extend_from_slice(frames);
            let r = feed(&mut sim, &mut seq, &all, now);
            let tail: Vec<String> = strs(&r[5..]).into_iter().map(String::from).collect();
            (tail, sim.is_terminated(), sim.violations().to_vec())
        };
        let (r, done, v) = run(
            AfterFiles::PyNetinstall,
            &[b"", b"", b"", message::FILE_END, TERM],
        );
        assert_eq!(
            (r, done, v.len()),
            (
                vec!["RETR".into(), "RETR".into(), "RETR".into(), "WTRM".into()],
                true,
                0
            )
        );
        let (r, done, v) = run(AfterFiles::NeverWtrm, &[b"", b"", message::FILE_END, TERM]);
        assert_eq!(
            (r, done, v.len()),
            (vec!["RETR".into(), "RETR".into(), "RETR".into()], true, 0)
        );
        let (r, done, _) = run(AfterFiles::EarlyWtrm, &[b"", &h2]);
        assert_eq!(r, ["WTRM", "NACK"]);
        assert!(!done);
        let (r, _, v) = run(AfterFiles::Official, &[b"", b"", message::FILE_END]);
        assert_eq!(r, ["RETR", "WTRM", "NACK"]);
        assert_eq!(v.len(), 1, "FILE\\n after WTRM is a violation");
    }

    #[test]
    fn slow_nack_and_silent_after_files_behaviours() {
        let h1 = header("a.npk", 1);
        let start: Vec<&[u8]> = vec![OFFER, b"", b"", &h1, b"a"];
        // Slow: RETR until 5 s after the file, then WTRM.
        let (mut sim, now) =
            sim_with(|c| c.after_files = AfterFiles::SlowWtrm(Duration::from_secs(5)));
        let mut seq = 0;
        feed(&mut sim, &mut seq, &start, now);
        let mut tail = Vec::new();
        for t in [0, 2, 4, 5] {
            tail.extend(feed(
                &mut sim,
                &mut seq,
                &[b""],
                now + Duration::from_secs(t),
            ));
        }
        assert_eq!(strs(&tail), ["RETR", "RETR", "RETR", "WTRM"]);
        feed(&mut sim, &mut seq, &[TERM], now);
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
        // NACK to FILE\n, then TERM is accepted.
        let (mut sim, now) = sim_with(|c| c.after_files = AfterFiles::NackFileEnd);
        let mut seq = 0;
        feed(&mut sim, &mut seq, &start, now);
        let r = feed(
            &mut sim,
            &mut seq,
            &[b"", b"", message::FILE_END, TERM],
            now,
        );
        assert_eq!(strs(&r), ["RETR", "RETR", "NACK"]);
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
        // Silent: no answer to polls or FILE\n, then TERM is accepted.
        let (mut sim, now) = sim_with(|c| c.after_files = AfterFiles::Silent);
        let mut seq = 0;
        feed(&mut sim, &mut seq, &start, now);
        let r = feed(
            &mut sim,
            &mut seq,
            &[b"", b"", message::FILE_END, TERM],
            now,
        );
        assert!(r.is_empty(), "{r:?}");
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
    }

    #[test]
    fn server_frames_mirroring_the_device_mac_are_accepted() {
        // reply_dst = the device: a mirroring server puts the device's MAC
        // in [0:6] of every frame after the offer.
        let (mut sim, now) = sim_with(|c| c.reply_dst = DEVICE);
        sim.handle_frame(&Frame::new(Mac::BROADCAST, DEVICE, 1, 0, OFFER), now);
        let yack = sim.poll_transmit().unwrap();
        assert_eq!((yack.src, yack.dst), (DEVICE, DEVICE));
        // Our own reply looped back is not a server frame.
        sim.handle_frame(&yack, now);
        assert_eq!(sim.poll_transmit(), None);
        // The server's next frame, src = the echoed device MAC.
        sim.handle_frame(&Frame::new(DEVICE, DEVICE, 2, 0, &b""[..]), now);
        let strt = sim.poll_transmit().unwrap();
        assert_eq!(strt.payload.as_ref(), message::STRT);
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
    }

    #[test]
    fn a_hello_to_its_own_mac_is_not_taken_for_a_server_frame() {
        let now = Instant::now();
        let mut cfg = SimConfig::new(DEVICE, "hEX", "mmips");
        cfg.info.hello_dst = DEVICE;
        let mut sim = DeviceSim::new(cfg, now);
        sim.handle_timeout(now);
        let hello = sim.poll_transmit().unwrap();
        assert_eq!((hello.src, hello.dst), (DEVICE, DEVICE));
        sim.handle_frame(&hello, now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
    }

    #[test]
    fn reject_offer_answers_every_offer_with_nack() {
        let (mut sim, now) = sim_with(|c| c.reject_offer = true);
        let r = feed(&mut sim, &mut 0, &[OFFER], now);
        assert_eq!(r, [message::NACK.to_vec()]);
        // The server offers again with the same seq: refused again.
        let again = Frame::new(Mac::BROADCAST, DEVICE, 1, 0, OFFER);
        sim.handle_frame(&again, now);
        assert_eq!(
            sim.poll_transmit().map(|f| f.payload.to_vec()),
            Some(b"NACK".to_vec())
        );
        assert!(sim.violations().is_empty(), "a refusal is not a violation");
        assert!(!sim.is_terminated());
        assert!(sim.received_files().is_empty());
        assert!(sim.offer().is_none());
    }

    #[test]
    fn malformed_offers_and_pynetinstall_offers_are_violations() {
        for bad in [message::OFFR, b"OFFR\n\nmaybe\n\n\0".as_slice()] {
            let (mut sim, now) = sim_with(|_| {});
            let r = feed(&mut sim, &mut 0, &[bad], now);
            assert_eq!(r, [message::NACK.to_vec()]);
            assert_eq!(sim.violations().len(), 1);
        }
    }

    #[test]
    fn missing_poll_after_a_file_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 3);
        let h2 = header("b.npk", 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, b"abc", &h2], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn a_script_first_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h = header(message::MODE_SCRIPT_NAME, 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn skipping_the_format_step_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 1);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", &h1], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn wrong_ack_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        sim.handle_frame(&Frame::new(Mac::BROADCAST, DEVICE, 1, 1, OFFER), now);
        assert_eq!(sim.violations().len(), 1);
        assert_eq!(
            sim.poll_transmit().map(|f| f.payload.to_vec()),
            Some(message::NACK.to_vec())
        );
        // pyNetinstall's ack = seq - 1 is wrong from the second frame on.
        let (mut sim, now) = sim_with(|_| {});
        feed(&mut sim, &mut 0, &[OFFER], now);
        sim.handle_frame(&Frame::new(Mac::ZERO, DEVICE, 2, 1, &b""[..]), now);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn empty_chunk_oversized_chunk_and_overshoot_are_violations() {
        let h1 = header("a.npk", 5000);
        let big = vec![0u8; MAX_CHUNK + 1];
        let h2 = header("a.npk", 3);
        for frames in [
            vec![OFFER, b"", b"", h1.as_slice(), big.as_slice()],
            vec![OFFER, b"", b"", h2.as_slice(), b"abcd"],
            vec![OFFER, b"", b"", h2.as_slice(), b""],
        ] {
            let (mut sim, now) = sim_with(|_| {});
            let r = feed(&mut sim, &mut 0, &frames, now);
            assert_eq!(r.last().unwrap().as_slice(), message::NACK);
            assert_eq!(sim.violations().len(), 1);
        }
    }

    #[test]
    fn stale_frames_are_ignored_silently() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        feed(&mut sim, &mut seq, &[OFFER, b"", b""], now);
        // A delayed duplicate of an older frame.
        sim.handle_frame(&Frame::new(Mac::BROADCAST, DEVICE, 1, 0, OFFER), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
    }

    #[test]
    fn frames_for_other_devices_and_own_frames_are_ignored() {
        let (mut sim, now) = sim_with(|_| {});
        let other = Mac([0x02, 0, 0, 0, 0, 0x99]);
        sim.handle_frame(&Frame::new(Mac::BROADCAST, other, 1, 0, OFFER), now);
        // Its own reply (seq = ack) looped back.
        sim.handle_frame(&Frame::new(DEVICE, DEVICE, 1, 1, message::YACK), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
        let r = feed(&mut sim, &mut 0, &[OFFER], now);
        assert_eq!(r, [message::YACK.to_vec()]);
    }

    #[test]
    fn short_non_final_chunk_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 2000);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, &[1u8; 1000]], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn the_last_chunk_is_answered_once() {
        let (mut sim, now) = sim_with(|_| {});
        let h1 = header("a.npk", 3);
        let r = feed(&mut sim, &mut 0, &[OFFER, b"", b"", &h1, b"abc"], now);
        // YACK, STRT, RETR (prepare), RETR (header), one RETR for the chunk.
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn duplicate_frames_are_deduplicated_unless_disabled() {
        for dedup in [true, false] {
            let (mut sim, now) = sim_with(|c| c.dedup = dedup);
            let mut seq = 0;
            let h1 = header("a.npk", 2 * MAX_CHUNK as u64);
            let chunk = vec![7u8; MAX_CHUNK];
            feed(&mut sim, &mut seq, &[OFFER, b"", b"", &h1, &chunk], now);
            // Re-send the chunk with the same seq.
            sim.handle_frame(
                &Frame::new(Mac::ZERO, DEVICE, seq, seq - 2, chunk.clone()),
                now,
            );
            while sim.poll_transmit().is_some() {}
            let twice = [chunk.clone(), chunk.clone()].concat();
            let expected: &[u8] = if dedup { b"" } else { &twice };
            let got: Vec<u8> = sim
                .received_files()
                .first()
                .map_or(Vec::new(), |f| f.data.clone());
            assert_eq!(got, expected, "dedup = {dedup}");
            assert!(sim.violations().is_empty());
        }
    }

    #[test]
    fn format_delay_postpones_strt() {
        let (mut sim, now) = sim_with(|c| c.format_delay = Duration::from_secs(30));
        let r = feed(&mut sim, &mut 0, &[OFFER, b""], now);
        assert_eq!(r, [message::YACK.to_vec()], "no STRT yet");
        // Periodic re-sends before the delay carry the previous reply only.
        let mut t = now;
        let mut strt_at = None;
        while strt_at.is_none() {
            t = sim.poll_timeout().unwrap();
            sim.handle_timeout(t);
            while let Some(f) = sim.poll_transmit() {
                if f.payload.as_ref() == message::STRT {
                    assert_eq!((f.seq, f.ack), (2, 2));
                    strt_at = Some(t);
                }
            }
        }
        assert!(t >= now + Duration::from_secs(30));
        assert!(sim.violations().is_empty());
    }
}
