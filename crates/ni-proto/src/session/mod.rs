//! Server-side install session for one device (sans-IO).
//!
//! The session is driven by the caller:
//! - [`ServerSession::poll_transmit`] returns frames to broadcast,
//! - [`ServerSession::handle_frame`] feeds frames received from the device,
//! - [`ServerSession::handle_timeout`] must be called once
//!   [`ServerSession::poll_timeout`] has passed,
//! - [`ServerSession::poll_event`] yields progress/phase events.
//!
//! Step sequence (mirrors pyNetinstall `Flasher.run`):
//! 1. `OFFR` → any reply (normally `YACK`)
//! 2. empty → any reply (`STRT`, device formatted; long timeout)
//! 3. empty → any reply (`RETR`)
//! 4. per package: `FILE` header (reply optional) → chunks (last chunk
//!    must be answered with `RETR`) → empty spacer
//! 5. optional config script as `autorun.scr` (same as a package, no spacer)
//! 6. empty → any reply
//! 7. `FILE\n` → any reply (`WTRM`)
//! 8. `TERM\n` → no reply, session done
//!
//! Sequence numbers: the k-th server frame has `seq = k, ack = k - 1`;
//! the device answers with `seq = k, ack = k`.

mod steps;

use crate::device_info::DeviceInfo;
use crate::frame::{Frame, Mac};
use crate::message;
use bytes::Bytes;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use steps::{build_steps, Expect, Step, TimeoutKind};
use thiserror::Error;

/// A file to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub name: String,
    pub data: Bytes,
}

/// What to install on one device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    /// RouterOS packages; the main `routeros` package must be first.
    pub packages: Vec<PlanFile>,
    /// Optional RouterOS script, uploaded as `autorun.scr`.
    pub config_script: Option<Bytes>,
}

impl InstallPlan {
    pub fn total_bytes(&self) -> u64 {
        let pkgs: u64 = self.packages.iter().map(|p| p.data.len() as u64).sum();
        pkgs + self.config_script.as_ref().map_or(0, |c| c.len() as u64)
    }
}

/// Timing parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Wait for an ordinary reply.
    pub reply_timeout: Duration,
    /// Wait for the device to finish formatting.
    pub format_timeout: Duration,
    /// Wait for replies some devices never send (`FILE` header).
    pub optional_timeout: Duration,
    /// Re-sends of an unanswered frame before failing.
    pub max_retransmits: u32,
    /// Pause between file chunks (pyNetinstall uses 5 ms to avoid state errors).
    pub chunk_delay: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(10),
            format_timeout: Duration::from_secs(120),
            optional_timeout: Duration::from_secs(3),
            max_retransmits: 3,
            chunk_delay: Duration::from_millis(5),
        }
    }
}

/// Coarse session phase, for UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Offering,
    Formatting,
    Preparing,
    Uploading { file: String },
    Finishing,
    Rebooting,
    Done,
    Failed,
}

impl Phase {
    /// Stable machine-readable name (used in error codes and i18n keys).
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Offering => "offering",
            Phase::Formatting => "formatting",
            Phase::Preparing => "preparing",
            Phase::Uploading { .. } => "uploading",
            Phase::Finishing => "finishing",
            Phase::Rebooting => "rebooting",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FailReason {
    #[error("device did not reply during {phase}")]
    Timeout { phase: &'static str },
    #[error("device rejected the request during {phase}")]
    Rejected { phase: &'static str },
}

impl FailReason {
    /// Stable code for UI translation.
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::Timeout { .. } => "timeout",
            FailReason::Rejected { .. } => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PhaseChanged(Phase),
    Progress {
        file: String,
        file_sent: u64,
        file_total: u64,
        bytes_sent: u64,
        bytes_total: u64,
    },
    Done,
    Failed(FailReason),
}

#[derive(Debug, Clone, Copy)]
enum Wait {
    /// Ready to send the current step (or re-send the last frame) at `at`.
    Send {
        at: Instant,
        resend: Option<u32>,
    },
    /// Waiting for the device to answer the current step.
    Reply {
        deadline: Instant,
        retries_left: u32,
    },
    Finished,
}

/// Install session for one device.
pub struct ServerSession {
    server_mac: Mac,
    device: DeviceInfo,
    cfg: SessionConfig,
    steps: Vec<Step>,
    idx: usize,
    seq: u16,
    wait: Wait,
    phase: Phase,
    last_frame: Option<Frame>,
    bytes_sent: u64,
    bytes_total: u64,
    events: VecDeque<Event>,
}

impl ServerSession {
    pub fn new(
        server_mac: Mac,
        device: DeviceInfo,
        plan: &InstallPlan,
        cfg: SessionConfig,
        now: Instant,
    ) -> Self {
        let mut events = VecDeque::new();
        events.push_back(Event::PhaseChanged(Phase::Offering));
        Self {
            server_mac,
            device,
            cfg,
            steps: build_steps(plan),
            idx: 0,
            seq: 0,
            wait: Wait::Send {
                at: now,
                resend: None,
            },
            phase: Phase::Offering,
            last_frame: None,
            bytes_sent: 0,
            bytes_total: plan.total_bytes(),
            events,
        }
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.wait, Wait::Finished)
    }

    /// Next frame to send, if any is due at `now`.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Frame> {
        let Wait::Send { at, resend } = self.wait else {
            return None;
        };
        if now < at {
            return None;
        }
        if let Some(retries_left) = resend {
            let frame = self.last_frame.clone()?;
            let timeout = self.timeout(self.steps[self.idx].timeout);
            self.wait = Wait::Reply {
                deadline: now + timeout,
                retries_left,
            };
            return Some(frame);
        }
        let step = self.steps.get(self.idx)?.clone();
        self.set_phase(step.phase.clone());
        self.seq = self.seq.wrapping_add(1);
        let frame = Frame::new(
            self.server_mac,
            self.device.mac,
            self.seq,
            self.seq.wrapping_sub(1),
            step.payload.clone(),
        );
        self.last_frame = Some(frame.clone());
        if step.expect == Expect::None {
            self.advance(now, false);
        } else {
            self.wait = Wait::Reply {
                deadline: now + self.timeout(step.timeout),
                retries_left: self.cfg.max_retransmits,
            };
        }
        Some(frame)
    }

    /// Feed a frame received on the wire. Unrelated frames are ignored.
    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        if !matches!(self.wait, Wait::Reply { .. }) {
            return;
        }
        if frame.src != self.device.mac || frame.seq != self.seq || frame.ack != self.seq {
            return;
        }
        if frame.payload.starts_with(message::NACK) {
            self.fail(FailReason::Rejected {
                phase: self.phase.name(),
            });
            return;
        }
        let step = &self.steps[self.idx];
        if let Expect::Payload(expected) = step.expect {
            if frame.payload.as_ref() != expected {
                return;
            }
        }
        let delay = step.delay_after;
        if let Some(chunk) = step.chunk.clone() {
            self.bytes_sent += step.payload.len() as u64;
            self.events.push_back(Event::Progress {
                file: chunk.file,
                file_sent: chunk.file_sent,
                file_total: chunk.file_total,
                bytes_sent: self.bytes_sent,
                bytes_total: self.bytes_total,
            });
        }
        self.advance(now, delay);
    }

    /// Call when `now >= poll_timeout()`.
    pub fn handle_timeout(&mut self, now: Instant) {
        let Wait::Reply {
            deadline,
            retries_left,
        } = self.wait
        else {
            return;
        };
        if now < deadline {
            return;
        }
        if retries_left > 0 {
            self.wait = Wait::Send {
                at: now,
                resend: Some(retries_left - 1),
            };
        } else if self.steps[self.idx].expect == Expect::Optional {
            // Some devices never answer this step; move on.
            self.advance(now, false);
        } else {
            self.fail(FailReason::Timeout {
                phase: self.phase.name(),
            });
        }
    }

    /// When the session next needs attention (send or timeout).
    pub fn poll_timeout(&self) -> Option<Instant> {
        match self.wait {
            Wait::Send { at, .. } => Some(at),
            Wait::Reply { deadline, .. } => Some(deadline),
            Wait::Finished => None,
        }
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    fn timeout(&self, kind: TimeoutKind) -> Duration {
        match kind {
            TimeoutKind::Reply => self.cfg.reply_timeout,
            TimeoutKind::Format => self.cfg.format_timeout,
            TimeoutKind::Optional => self.cfg.optional_timeout,
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase.clone();
            self.events.push_back(Event::PhaseChanged(phase));
        }
    }

    fn advance(&mut self, now: Instant, delay: bool) {
        self.idx += 1;
        if self.idx >= self.steps.len() {
            self.wait = Wait::Finished;
            self.set_phase(Phase::Done);
            self.events.push_back(Event::Done);
        } else {
            let at = if delay {
                now + self.cfg.chunk_delay
            } else {
                now
            };
            self.wait = Wait::Send { at, resend: None };
        }
    }

    fn fail(&mut self, reason: FailReason) {
        self.wait = Wait::Finished;
        self.set_phase(Phase::Failed);
        self.events.push_back(Event::Failed(reason));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: Mac = Mac([0x02, 0, 0, 0, 0, 0x01]);
    const DEVICE: Mac = Mac([0xaa; 6]);

    fn device() -> DeviceInfo {
        DeviceInfo {
            mac: DEVICE,
            tag: "DSCV".into(),
            license_id: String::new(),
            license_key: String::new(),
            model: "hEX".into(),
            arch: "mmips".into(),
            min_os: "7.0".into(),
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: Bytes::from(vec![7u8; 10]),
            }],
            config_script: None,
        }
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, SERVER, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (SERVER, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], message::OFFR);
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, SERVER, 1, 0, &b"YACK\n"[..]), now);
        s.handle_frame(
            &Frame::new(Mac([0xbb; 6]), SERVER, 1, 1, &b"YACK\n"[..]),
            now,
        );
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.seq, f.ack), (2, 1));
        assert!(f.payload.is_empty());
    }

    #[test]
    fn formatting_step_uses_format_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig::default();
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg.clone(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        assert_eq!(s.phase(), &Phase::Formatting);
        assert_eq!(s.poll_timeout(), Some(now + cfg.format_timeout));
    }

    #[test]
    fn retransmits_then_fails_with_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig {
            max_retransmits: 2,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg, now);
        let first = s.poll_transmit(now).unwrap();
        for _ in 0..2 {
            let t = s.poll_timeout().unwrap();
            s.handle_timeout(t);
            assert_eq!(
                s.poll_transmit(t),
                Some(first.clone()),
                "same frame re-sent"
            );
        }
        let t = s.poll_timeout().unwrap();
        s.handle_timeout(t);
        assert!(s.is_finished());
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Timeout { phase: "offering" }))
        );
    }

    #[test]
    fn nack_fails_with_rejected() {
        let now = Instant::now();
        let mut s = ServerSession::new(SERVER, device(), &plan(), SessionConfig::default(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK\nno space\n"), now);
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Rejected { phase: "offering" }))
        );
    }

    #[test]
    fn total_bytes_includes_config_script() {
        let mut p = plan();
        p.config_script = Some(Bytes::from_static(b"/system identity set name=x\n"));
        assert_eq!(p.total_bytes(), 10 + 28);
    }
}
