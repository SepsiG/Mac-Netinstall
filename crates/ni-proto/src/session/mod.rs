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
//! 4. per package: `FILE` header (reply optional) → chunks (one reply each;
//!    the last chunk needs a second reply, which must be `RETR`) → empty spacer
//! 5. optional config script as `autorun.scr` (same as a package, no spacer)
//! 6. empty → any reply
//! 7. `FILE\n` → any reply (`WTRM`)
//! 8. `TERM\n` → no reply, session done
//!
//! Sequence numbers: the k-th server frame has `seq = k, ack = k - 1`;
//! the device answers with `seq = k, ack = k`.
//!
//! Retransmission: pyNetinstall never retransmits; it just waits (60 s) for
//! the device. We re-send an unanswered frame with the *same* `seq`, assuming
//! the device recognises the duplicate and only repeats its previous answer.
//! This is unverified on hardware: a device that processed a re-sent chunk
//! again would store it twice (see the simulator's `SimConfig::dedup` and
//! the `without_device_dedup_a_lost_reply_corrupts_the_file` test).

mod steps;

use crate::device_info::DeviceInfo;
use crate::frame::{Frame, Mac};
use crate::message;
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
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
///
/// [`ServerSession::new`] validates the plan (see [`PlanError`]). In
/// particular, the whole session must fit in [`u16::MAX`] frames, so the
/// 16-bit sequence number never wraps (roughly 64 MiB of total payload).
/// Wraparound has never been exercised on hardware: pyNetinstall cannot even
/// produce it (`struct.pack("<H")` raises past 65535), so such plans are
/// rejected for now.
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

/// Reasons an [`InstallPlan`] is rejected before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the plan contains no packages")]
    NoPackages,
    #[error("{name} does not look like a RouterOS package (missing NPK magic)")]
    NotAnNpk { name: String },
    /// A package name is empty, contains `\n` or `/`, or is the reserved
    /// configuration script name ([`message::CONFIG_SCRIPT_NAME`]).
    #[error("invalid file name {name:?}")]
    InvalidFileName { name: String },
    #[error("the plan needs {frames} frames, at most {max} are supported")]
    TooManyFrames { frames: usize, max: usize },
}

impl InstallPlan {
    /// Check the plan can be sent safely; see [`PlanError`].
    pub fn validate(&self) -> Result<(), PlanError> {
        let first = self.packages.first().ok_or(PlanError::NoPackages)?;
        if !first.data.starts_with(&message::NPK_MAGIC) {
            return Err(PlanError::NotAnNpk {
                name: first.name.clone(),
            });
        }
        if let Some(bad) = self.packages.iter().find(|p| !valid_package_name(&p.name)) {
            return Err(PlanError::InvalidFileName {
                name: bad.name.clone(),
            });
        }
        let frames = steps::frame_count(self);
        let max = usize::from(u16::MAX);
        if frames > max {
            return Err(PlanError::TooManyFrames { frames, max });
        }
        Ok(())
    }
}

/// Package names travel in the newline-separated `FILE` header and become
/// file names on the device; `autorun.scr` is reserved for the script.
fn valid_package_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\n', '/']) && name != message::CONFIG_SCRIPT_NAME
}

/// Timing parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// Wait for an ordinary reply.
    pub reply_timeout: Duration,
    /// Wait for the device to finish formatting.
    pub format_timeout: Duration,
    /// Wait for replies some devices never send (`FILE` header). With the
    /// default retransmits this waits about 40 s in total, close to
    /// pyNetinstall's 60 s socket timeout.
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
            optional_timeout: Duration::from_secs(10),
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
    Uploading { file: Arc<str> },
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
    #[error("installation cancelled")]
    Cancelled,
}

impl FailReason {
    /// Stable code for UI translation.
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::Timeout { .. } => "timeout",
            FailReason::Rejected { .. } => "rejected",
            FailReason::Cancelled => "cancelled",
        }
    }
}

/// Session events.
///
/// [`Event::PhaseChanged`] is only emitted for active phases; the end of a
/// session is reported by [`Event::Done`] or [`Event::Failed`] alone (while
/// [`ServerSession::phase`] then reports [`Phase::Done`] / [`Phase::Failed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PhaseChanged(Phase),
    Progress {
        file: Arc<str>,
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
///
/// Plans that would need more than [`u16::MAX`] frames are rejected by
/// [`ServerSession::new`], so sequence numbers never wrap (wraparound is
/// unverified on hardware; see [`InstallPlan`]).
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
    /// State-matched replies received for the current step (only counted
    /// for [`Expect::SecondIs`]). Kept across retransmits, so a device that
    /// answers once and only repeats when we re-send still completes.
    step_replies: u8,
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
    ) -> Result<Self, PlanError> {
        plan.validate()?;
        let mut events = VecDeque::new();
        events.push_back(Event::PhaseChanged(Phase::Offering));
        Ok(Self {
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
            step_replies: 0,
            bytes_sent: 0,
            bytes_total: plan.total_bytes(),
            events,
        })
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
        let step = self.steps.get(self.idx)?;
        let (payload, expect, timeout) = (step.payload.clone(), step.expect, step.timeout);
        if self.phase != step.phase {
            self.phase = step.phase.clone();
            self.events
                .push_back(Event::PhaseChanged(self.phase.clone()));
        }
        // Cannot overflow: plans needing more than u16::MAX frames are rejected.
        debug_assert!(self.seq < u16::MAX, "sequence number would wrap");
        self.seq += 1;
        let frame = Frame::new(
            self.server_mac,
            self.device.mac,
            self.seq,
            self.seq - 1,
            payload,
        );
        self.last_frame = Some(frame.clone());
        self.step_replies = 0;
        if expect == Expect::None {
            self.advance(now, false);
        } else {
            self.wait = Wait::Reply {
                deadline: now + self.timeout(timeout),
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
        if let Expect::SecondIs(expected) = step.expect {
            if self.step_replies == 0 {
                self.step_replies = 1;
                return;
            }
            if frame.payload.as_ref() != expected {
                return;
            }
        }
        let delay = step.delay_after;
        if let Some(chunk) = &step.chunk {
            self.bytes_sent += step.payload.len() as u64;
            self.events.push_back(Event::Progress {
                file: Arc::clone(&chunk.file),
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
            ..
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

    /// Abort the session. If it has not finished yet, it fails with
    /// [`FailReason::Cancelled`]; nothing more is sent.
    pub fn cancel(&mut self) {
        if !self.is_finished() {
            self.fail(FailReason::Cancelled);
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

    fn advance(&mut self, now: Instant, delay: bool) {
        self.idx += 1;
        if self.idx >= self.steps.len() {
            self.wait = Wait::Finished;
            self.phase = Phase::Done;
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
        self.phase = Phase::Failed;
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
            extra: Vec::new(),
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: npk(10),
            }],
            config_script: None,
        }
    }

    /// `len` bytes starting with the NPK magic.
    fn npk(len: usize) -> Bytes {
        let mut v = message::NPK_MAGIC.to_vec();
        v.resize(len.max(4), 7);
        Bytes::from(v)
    }

    fn session(plan: &InstallPlan, now: Instant) -> ServerSession {
        ServerSession::new(SERVER, device(), plan, SessionConfig::default(), now).unwrap()
    }

    fn new_err(plan: &InstallPlan) -> PlanError {
        match ServerSession::new(
            SERVER,
            device(),
            plan,
            SessionConfig::default(),
            Instant::now(),
        ) {
            Ok(_) => panic!("plan must be rejected"),
            Err(e) => e,
        }
    }

    fn file(name: &str, data: Bytes) -> PlanFile {
        PlanFile {
            name: name.into(),
            data,
        }
    }

    #[test]
    fn rejects_plan_without_packages() {
        assert_eq!(new_err(&InstallPlan::default()), PlanError::NoPackages);
    }

    #[test]
    fn rejects_first_package_without_npk_magic() {
        let p = InstallPlan {
            packages: vec![file("routeros.npk", Bytes::from_static(b"PK\x03\x04rest"))],
            config_script: None,
        };
        assert_eq!(
            new_err(&p),
            PlanError::NotAnNpk {
                name: "routeros.npk".into()
            }
        );
        let short = InstallPlan {
            packages: vec![file("tiny.npk", Bytes::from_static(&[0x1E, 0xF1]))],
            config_script: None,
        };
        assert_eq!(
            new_err(&short),
            PlanError::NotAnNpk {
                name: "tiny.npk".into()
            }
        );
    }

    #[test]
    fn rejects_invalid_file_names() {
        for bad in [
            "",
            "evil\nname.npk",
            "dir/name.npk",
            message::CONFIG_SCRIPT_NAME,
        ] {
            let p = InstallPlan {
                packages: vec![file("routeros.npk", npk(10)), file(bad, npk(10))],
                config_script: None,
            };
            assert_eq!(new_err(&p), PlanError::InvalidFileName { name: bad.into() });
        }
    }

    #[test]
    fn rejects_plans_needing_more_than_u16_max_frames() {
        // Each package costs 3 frames (header, one chunk, spacer), plus
        // 6 fixed frames (OFFR, 2 empties, final empty, FILE\n, TERM).
        let plan_with = |n: usize| InstallPlan {
            packages: (0..n)
                .map(|i| {
                    file(
                        &format!("p{i}.npk"),
                        if i == 0 { npk(4) } else { Bytes::new() },
                    )
                })
                .collect(),
            config_script: None,
        };
        let max = (usize::from(u16::MAX) - 6) / 3;
        assert!(ServerSession::new(
            SERVER,
            device(),
            &plan_with(max),
            SessionConfig::default(),
            Instant::now()
        )
        .is_ok());
        assert_eq!(
            new_err(&plan_with(max + 1)),
            PlanError::TooManyFrames {
                frames: 6 + 3 * (max + 1),
                max: usize::from(u16::MAX),
            }
        );
    }

    #[test]
    fn frame_count_matches_built_steps() {
        let p = InstallPlan {
            packages: vec![file("a.npk", npk(2048)), file("b.npk", npk(1025))],
            config_script: Some(Bytes::from_static(b"x")),
        };
        assert_eq!(steps::frame_count(&p), build_steps(&p).len());
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, SERVER, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (SERVER, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], message::OFFR);
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
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
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg.clone(), now).unwrap();
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
        let mut s = ServerSession::new(SERVER, device(), &plan(), cfg, now).unwrap();
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
        let mut s = session(&plan(), now);
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

    /// Drive `s` (built from [`plan`]) until the single chunk has been sent.
    fn send_until_last_chunk(s: &mut ServerSession, now: Instant) {
        for (seq, answer) in [
            (1, message::YACK),
            (2, message::STRT),
            (3, message::RETR),
            (4, message::RETR),
        ] {
            let f = s.poll_transmit(now).unwrap();
            assert_eq!(f.seq, seq);
            s.handle_frame(&reply(seq, answer), now);
        }
        let chunk = s.poll_transmit(now).unwrap();
        assert_eq!((chunk.seq, chunk.payload.len()), (5, 10));
    }

    #[test]
    fn last_chunk_needs_a_second_reply_that_is_retr() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        send_until_last_chunk(&mut s, now);

        // First state-matched reply: any payload.
        s.handle_frame(&reply(5, b"WHAT"), now);
        assert_eq!(s.poll_transmit(now), None, "one reply is not enough");
        // Later non-RETR replies are ignored.
        s.handle_frame(&reply(5, b"STRT"), now);
        assert_eq!(s.poll_transmit(now), None);
        // The second reply must be exactly RETR.
        s.handle_frame(&reply(5, message::RETR), now);
        let spacer = s.poll_transmit(now).expect("spacer after the file");
        assert_eq!(spacer.seq, 6);
        assert!(spacer.payload.is_empty());
    }

    #[test]
    fn final_reply_count_survives_retransmit() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        send_until_last_chunk(&mut s, now);
        s.handle_frame(&reply(5, message::RETR), now);
        // No second reply: retransmit the last chunk.
        let t = s.poll_timeout().unwrap();
        s.handle_timeout(t);
        let again = s.poll_transmit(t).unwrap();
        assert_eq!(again.seq, 5);
        // The device repeats its answer once; that is the second reply.
        s.handle_frame(&reply(5, message::RETR), t);
        assert_eq!(s.poll_transmit(t).map(|f| f.seq), Some(6));
    }

    #[test]
    fn last_chunk_completes_with_two_retr() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        send_until_last_chunk(&mut s, now);
        s.handle_frame(&reply(5, message::RETR), now);
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(5, message::RETR), now);
        assert_eq!(s.poll_transmit(now).map(|f| f.seq), Some(6));
    }

    #[test]
    fn failure_emits_failed_event_but_no_phase_change() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK"), now);
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::Failed(FailReason::Rejected { phase: "offering" })
            ]
        );
    }

    #[test]
    fn cancel_fails_an_unfinished_session_once() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.cancel();
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.poll_timeout(), None);
        s.cancel();
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::Failed(FailReason::Cancelled)
            ]
        );
        assert_eq!(FailReason::Cancelled.code(), "cancelled");
    }

    #[test]
    fn default_optional_timeout_is_ten_seconds() {
        assert_eq!(
            SessionConfig::default().optional_timeout,
            Duration::from_secs(10)
        );
    }

    #[test]
    fn upload_phase_and_progress_name_the_file() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        send_until_last_chunk(&mut s, now);
        assert_eq!(
            s.phase(),
            &Phase::Uploading {
                file: Arc::from("routeros.npk")
            }
        );
        s.handle_frame(&reply(5, message::RETR), now);
        s.handle_frame(&reply(5, message::RETR), now);
        let progress = std::iter::from_fn(|| s.poll_event()).find_map(|e| match e {
            Event::Progress { file, .. } => Some(file),
            _ => None,
        });
        assert_eq!(progress.as_deref(), Some("routeros.npk"));
    }

    #[test]
    fn total_bytes_includes_config_script() {
        let mut p = plan();
        p.config_script = Some(Bytes::from_static(b"/system identity set name=x\n"));
        assert_eq!(p.total_bytes(), 10 + 28);
    }
}
