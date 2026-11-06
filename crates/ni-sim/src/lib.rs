//! Simulated MikroTik device in Netinstall mode (sans-IO).
//!
//! Behaviour mirrors what pyNetinstall expects from real hardware:
//! broadcasts hello frames until offered, answers each server frame with
//! `seq = ack = <server seq>`, and periodically re-sends its last reply
//! (real devices do this too).
//!
//! The simulator is deliberately strict: it only accepts the exact frame
//! sequence pyNetinstall sends,
//!
//! ```text
//! OFFR → empty (format) → empty (prepare)
//!   → loop { FILE header → chunks → empty spacer }      (packages)
//!   → [FILE autorun.scr header → chunks]                 (no spacer)
//!   → empty → FILE\n → TERM
//! ```
//!
//! Once the server's MAC is known (from `OFFR`), frames from other sources
//! are ignored, as are stale frames older than the last one accepted.
//! Anything else out of line (wrong order, wrong `ack`, oversized chunks,
//! non-final chunks shorter than `MAX_CHUNK`, more data than announced) is
//! answered with `NACK` and recorded in
//! [`DeviceSim::violations`], so step-order regressions fail tests. Whether a
//! file is a package or the configuration script is decided by its name
//! ([`message::CONFIG_SCRIPT_NAME`]).

pub mod harness;

use bytes::Bytes;
use ni_proto::{message, DeviceInfo, Frame, Mac, MAX_CHUNK};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

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
    /// Answer the last chunk of a file with `RETR` twice (as pyNetinstall
    /// expects); when `false` only once, so the server has to rely on the
    /// periodic re-send. Default `true`.
    pub double_final_retr: bool,
    /// How long formatting takes before the device answers `STRT`. Until
    /// then it only re-sends its previous reply. Default zero.
    pub format_delay: Duration,
}

impl SimConfig {
    pub fn new(mac: Mac, model: &str, arch: &str) -> Self {
        Self {
            info: DeviceInfo {
                mac,
                tag: "DSCV".into(),
                license_id: "SIM-0000".into(),
                license_key: String::new(),
                model: model.into(),
                arch: arch.into(),
                min_os: "7.0".into(),
            },
            hello_interval: Duration::from_secs(1),
            resend_interval: Duration::from_millis(500),
            dedup: true,
            double_final_retr: true,
            format_delay: Duration::ZERO,
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
    /// Expects a `FILE` header, or (after at least one package) the final
    /// empty frame.
    Ready,
    Receiving {
        name: String,
        size: u64,
        data: Vec<u8>,
        script: bool,
    },
    /// A package was received; expects the empty spacer.
    PackageDone,
    /// The configuration script was received; expects the final empty frame.
    ScriptDone,
    /// Expects `FILE\n`.
    Closing,
    /// Expects `TERM`.
    Finished,
    Terminated,
}

/// Outcome of processing one in-order server frame.
type Step = Result<(State, Vec<&'static [u8]>), (State, String)>;

pub struct DeviceSim {
    cfg: SimConfig,
    state: State,
    server: Option<Mac>,
    last_seq: Option<u16>,
    last_reply: Option<Frame>,
    tx: VecDeque<Frame>,
    next_timer: Option<Instant>,
    received: Vec<ReceivedFile>,
    packages_received: usize,
    violations: Vec<String>,
}

impl DeviceSim {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        Self {
            cfg,
            state: State::Discovering,
            server: None,
            last_seq: None,
            last_reply: None,
            tx: VecDeque::new(),
            next_timer: Some(now),
            received: Vec::new(),
            packages_received: 0,
            violations: Vec::new(),
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.cfg.info
    }

    pub fn received_files(&self) -> &[ReceivedFile] {
        &self.received
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
                    Mac::BROADCAST,
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

    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let me = self.cfg.info.mac;
        if self.state == State::Terminated || frame.dst != me || frame.src == me {
            return;
        }
        if self.server.is_some_and(|server| frame.src != server) {
            // Another netinstall server on the segment: not ours.
            return;
        }
        if self.last_seq.is_some_and(|last| frame.seq < last) {
            // Stale or delayed duplicate of an older frame.
            return;
        }
        if frame.ack != frame.seq.wrapping_sub(1) {
            self.violation(format!(
                "frame seq {} carries ack {}, expected {}",
                frame.seq,
                frame.ack,
                frame.seq.wrapping_sub(1)
            ));
            self.nack(frame);
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
            self.nack(frame);
            return;
        }
        if self.server.is_none() {
            self.server = Some(frame.src);
        }
        self.last_seq = Some(frame.seq);
        let state = std::mem::replace(&mut self.state, State::Discovering);
        let replies = match self.process(state, &frame.payload, now) {
            Ok((next, replies)) => {
                self.state = next;
                replies
            }
            Err((prev, why)) => {
                self.state = prev;
                self.violation(format!("seq {}: {why}", frame.seq));
                vec![message::NACK]
            }
        };
        for payload in replies {
            self.reply(frame.seq, payload);
        }
        self.schedule(now);
    }

    fn process(&mut self, state: State, payload: &[u8], now: Instant) -> Step {
        use message::{RETR, STRT, WTRM, YACK};
        let what = describe(payload);
        match state {
            State::Discovering if payload == message::OFFR => Ok((State::Offered, vec![YACK])),
            State::Offered if payload.is_empty() => {
                if self.cfg.format_delay.is_zero() {
                    Ok((State::Formatted, vec![STRT]))
                } else {
                    let until = now + self.cfg.format_delay;
                    Ok((State::Formatting { until }, vec![]))
                }
            }
            State::Formatted if payload.is_empty() => Ok((State::Ready, vec![RETR])),
            State::Ready if payload.is_empty() && self.packages_received > 0 => {
                Ok((State::Closing, vec![RETR]))
            }
            State::Ready => match message::parse_file_header(payload) {
                Some((name, size)) => {
                    let script = name == message::CONFIG_SCRIPT_NAME;
                    if self.packages_received == 0 && script {
                        return Err((State::Ready, "script before any package".into()));
                    }
                    let data = Vec::new();
                    Ok((
                        State::Receiving {
                            name,
                            size,
                            data,
                            script,
                        },
                        vec![RETR],
                    ))
                }
                None => Err((State::Ready, format!("expected FILE header, got {what}"))),
            },
            State::Receiving {
                name,
                size,
                mut data,
                script,
            } => {
                let error = if payload.len() > MAX_CHUNK {
                    Some(format!(
                        "chunk of {} bytes exceeds {MAX_CHUNK}",
                        payload.len()
                    ))
                } else if payload.is_empty() && size > 0 {
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
                    let state = State::Receiving {
                        name,
                        size,
                        data,
                        script,
                    };
                    return Err((state, why));
                }
                data.extend_from_slice(payload);
                if (data.len() as u64) < size {
                    let state = State::Receiving {
                        name,
                        size,
                        data,
                        script,
                    };
                    return Ok((state, vec![RETR]));
                }
                self.received.push(ReceivedFile { name, data });
                let next = if script {
                    State::ScriptDone
                } else {
                    self.packages_received += 1;
                    State::PackageDone
                };
                // pyNetinstall reads twice after the last chunk.
                let replies = if self.cfg.double_final_retr {
                    vec![RETR, RETR]
                } else {
                    vec![RETR]
                };
                Ok((next, replies))
            }
            State::PackageDone if payload.is_empty() => Ok((State::Ready, vec![RETR])),
            State::ScriptDone if payload.is_empty() => Ok((State::Closing, vec![RETR])),
            State::Closing if payload == message::FILE_END => Ok((State::Finished, vec![WTRM])),
            State::Finished if payload == message::TERM => Ok((State::Terminated, vec![])),
            other => {
                let why = format!("unexpected {what} in state {}", state_name(&other));
                Err((other, why))
            }
        }
    }

    fn reply(&mut self, seq: u16, payload: &'static [u8]) {
        let Some(server) = self.server else { return };
        let reply = Frame::new(
            self.cfg.info.mac,
            server,
            seq,
            seq,
            Bytes::from_static(payload),
        );
        self.last_reply = Some(reply.clone());
        self.tx.push_back(reply);
    }

    /// Answer a frame that failed the header checks, without treating it as
    /// accepted (it does not become the reply that is re-sent).
    fn nack(&mut self, frame: &Frame) {
        let reply = Frame::new(
            self.cfg.info.mac,
            frame.src,
            frame.seq,
            frame.seq,
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
        State::PackageDone => "PackageDone",
        State::ScriptDone => "ScriptDone",
        State::Closing => "Closing",
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

    const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
    const DEVICE: Mac = Mac([0xaa; 6]);

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
    /// and return the payloads of all replies.
    fn feed(sim: &mut DeviceSim, seq: &mut u16, payloads: &[&[u8]], now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for p in payloads {
            *seq += 1;
            sim.handle_frame(&Frame::new(SERVER, DEVICE, *seq, *seq - 1, p.to_vec()), now);
            while let Some(r) = sim.poll_transmit() {
                assert_eq!((r.seq, r.ack), (*seq, *seq));
                out.push(r.payload.to_vec());
            }
        }
        out
    }

    fn header(name: &str, size: u64) -> Vec<u8> {
        message::file_header(name, size)
    }

    #[test]
    fn accepts_the_pynetinstall_sequence() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let h2 = header(message::CONFIG_SCRIPT_NAME, 2);
        let replies = feed(
            &mut sim,
            &mut seq,
            &[
                message::OFFR,
                b"",
                b"",
                &h1,
                b"abc",
                b"",
                &h2,
                b"xy",
                b"",
                message::FILE_END,
                message::TERM,
            ],
            now,
        );
        let r: Vec<&[u8]> = replies.iter().map(Vec::as_slice).collect();
        use message::{RETR, STRT, WTRM, YACK};
        assert_eq!(
            r,
            [YACK, STRT, RETR, RETR, RETR, RETR, RETR, RETR, RETR, RETR, RETR, WTRM]
        );
        assert!(sim.is_terminated());
        assert!(sim.violations().is_empty(), "{:?}", sim.violations());
        assert_eq!(sim.received_files().len(), 2);
    }

    #[test]
    fn missing_spacer_after_package_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let h2 = header("b.npk", 1);
        let r = feed(
            &mut sim,
            &mut seq,
            &[message::OFFR, b"", b"", &h1, b"abc", &h2],
            now,
        );
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn script_must_not_be_followed_by_a_spacer_then_file_end_directly() {
        // After the script the next frame is the final empty, then FILE\n.
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 1);
        let h2 = header(message::CONFIG_SCRIPT_NAME, 1);
        let r = feed(
            &mut sim,
            &mut seq,
            &[
                message::OFFR,
                b"",
                b"",
                &h1,
                b"a",
                b"",
                &h2,
                b"x",
                message::FILE_END,
            ],
            now,
        );
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn skipping_the_format_step_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 1);
        let r = feed(&mut sim, &mut seq, &[message::OFFR, b"", &h1], now);
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn wrong_ack_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        sim.handle_frame(&Frame::new(SERVER, DEVICE, 1, 1, message::OFFR), now);
        assert_eq!(sim.violations().len(), 1);
        assert_eq!(
            sim.poll_transmit().map(|f| f.payload.to_vec()),
            Some(message::NACK.to_vec())
        );
    }

    #[test]
    fn oversized_chunk_and_overshoot_are_violations() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 5000);
        let big = vec![0u8; MAX_CHUNK + 1];
        let r = feed(
            &mut sim,
            &mut seq,
            &[message::OFFR, b"", b"", &h1, &big],
            now,
        );
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);

        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let r = feed(
            &mut sim,
            &mut seq,
            &[message::OFFR, b"", b"", &h1, b"abcd"],
            now,
        );
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1);
    }

    #[test]
    fn stale_frames_are_ignored_silently() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        feed(&mut sim, &mut seq, &[message::OFFR, b"", b""], now);
        // A delayed duplicate of an older frame.
        sim.handle_frame(&Frame::new(SERVER, DEVICE, 1, 0, message::OFFR), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
    }

    #[test]
    fn frames_from_other_servers_are_ignored_once_offered() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        feed(&mut sim, &mut seq, &[message::OFFR], now);
        let other = Mac([0x02, 0, 0, 0, 0, 0x99]);
        sim.handle_frame(&Frame::new(other, DEVICE, 2, 1, &b""[..]), now);
        sim.handle_frame(&Frame::new(other, DEVICE, 7, 3, &b"junk"[..]), now);
        assert_eq!(sim.poll_transmit(), None);
        assert!(sim.violations().is_empty());
        // The real server continues normally.
        let r = feed(&mut sim, &mut seq, &[b""], now);
        assert_eq!(r, [message::STRT.to_vec()]);
    }

    #[test]
    fn short_non_final_chunk_is_a_violation() {
        let (mut sim, now) = sim_with(|_| {});
        let mut seq = 0;
        let h1 = header("a.npk", 2000);
        let r = feed(
            &mut sim,
            &mut seq,
            &[message::OFFR, b"", b"", &h1, &[1u8; 1000]],
            now,
        );
        assert_eq!(r.last().unwrap().as_slice(), message::NACK);
        assert_eq!(sim.violations().len(), 1, "{:?}", sim.violations());
    }

    #[test]
    fn single_final_retr_when_configured() {
        let (mut sim, now) = sim_with(|c| c.double_final_retr = false);
        let mut seq = 0;
        let h1 = header("a.npk", 3);
        let r = feed(
            &mut sim,
            &mut seq,
            &[message::OFFR, b"", b"", &h1, b"abc"],
            now,
        );
        // YACK, STRT, RETR (prepare), RETR (header), one RETR for the last chunk.
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn duplicate_frames_are_deduplicated_unless_disabled() {
        for dedup in [true, false] {
            let (mut sim, now) = sim_with(|c| c.dedup = dedup);
            let mut seq = 0;
            let h1 = header("a.npk", 2 * MAX_CHUNK as u64);
            let chunk = vec![7u8; MAX_CHUNK];
            feed(
                &mut sim,
                &mut seq,
                &[message::OFFR, b"", b"", &h1, &chunk],
                now,
            );
            // Re-send the chunk with the same seq.
            sim.handle_frame(
                &Frame::new(SERVER, DEVICE, seq, seq - 1, chunk.clone()),
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
        let mut seq = 0;
        let r = feed(&mut sim, &mut seq, &[message::OFFR, b""], now);
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
