//! Server-side install session for one device (sans-IO).
//!
//! The session is driven by the caller:
//! - [`ServerSession::poll_transmit`] returns frames to broadcast,
//! - [`ServerSession::handle_frame`] feeds frames received from the device,
//! - [`ServerSession::handle_timeout`] must be called once
//!   [`ServerSession::poll_timeout`] has passed,
//! - [`ServerSession::poll_event`] yields progress/phase events.
//!
//! Step sequence (the official `netinstall-cli` 7.25.1, as captured):
//! 1. `OFFR\n<key>\n<keep>\n<brand>\n\0` → `YACK`. A `NACK` makes us offer
//!    again (same frame) after [`SessionConfig::offer_retry`], at most
//!    [`SessionConfig::max_retransmits`] times, then the session fails
//!    with [`FailReason::Rejected`].
//! 2. empty → any reply (`STRT`, device formatted; long timeout)
//! 3. empty → any reply (`RETR`)
//! 4. per file (packages, the system package first, then `autorun.scr` /
//!    `mode.scr`; see [`InstallPlan::files`]): `FILE\n<name>\n<size>\n\0`
//!    (reply optional) → one frame per 1024 bytes, one reply each (an
//!    empty file has none) → an empty poll. Before the last file the poll
//!    must be answered `RETR`; `WTRM` there means the device stopped early
//!    ([`FailReason::EndedEarly`]).
//! 5. after the last file: empty polls while the device answers `RETR`
//!    (every [`SessionConfig::poll_interval`] for up to
//!    [`SessionConfig::finish_timeout`], at most [`MAX_FINISH_POLLS`];
//!    each reply awaited [`SessionConfig::finish_reply_timeout`]) until it
//!    answers `WTRM`;
//! 6. fallback, only if it never did (the window passed, the polls went
//!    unanswered or were refused): pyNetinstall's `FILE\n`, whatever the
//!    answer (`WTRM`, anything else, `NACK` or none at all);
//! 7. `TERM\n<message>\0` → no reply, session done.
//!
//! Once every file is sent the session always ends with `TERM` and
//! [`Event::Done`]: a formatted device that has all its files is never left
//! without it. If the device never said `WTRM`, [`Event::FinishUnconfirmed`]
//! precedes [`Event::Done`].
//!
//! Frame header: `[0:6]` mirrors `[6:12]` of the last device frame
//! answered (for the offer: the hello's, [`DeviceInfo::hello_dst`], the
//! broadcast address in every capture) and `[6:12]` is the device's MAC: our own MAC
//! is never on the wire. The k-th server frame has `seq = k` and
//! `ack = k - 2` (0 for the first two); the device answers with
//! `seq = ack = k`.
//!
//! Retransmission: the official server never retransmits (a lost reply
//! stalls until the device re-sends its last answer). We re-send an
//! unanswered frame with the *same* `seq`, assuming the device recognises
//! the duplicate and only repeats its previous answer. This is unverified
//! on hardware: a device that processed a re-sent chunk again would store
//! it twice (see the simulator's `SimConfig::dedup` and the
//! `without_device_dedup_a_lost_reply_corrupts_the_file` test).

mod steps;

use crate::device_info::DeviceInfo;
use crate::frame::{Frame, Mac};
use crate::message::{self, AfterInstall, Offer};
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use steps::{build_steps, Expect, Step, TimeoutKind};
use thiserror::Error;

/// Most empty polls after the last file before the session falls back to
/// `FILE\n`, whatever [`SessionConfig::finish_timeout`] and
/// [`SessionConfig::poll_interval`] allow (the default 60 s window at 1 s
/// needs 61).
pub const MAX_FINISH_POLLS: u32 = 64;

/// A file to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub name: String,
    pub data: Bytes,
}

/// What happens to the device's configuration (the official tool's
/// `-r` / `-e` / `-s`; nothing = keep).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceConfig {
    /// Keep the existing configuration (the official tool's default).
    Keep,
    /// RouterOS applies its default configuration (`-r`).
    #[default]
    Default,
    /// No configuration (`-e`): an empty `autorun.scr` is uploaded.
    Empty,
    /// Run this script on first boot (`-s`), uploaded as `autorun.scr`.
    Script(Bytes),
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
    /// RouterOS packages; the system package (`routeros`) must be first.
    pub packages: Vec<PlanFile>,
    pub config: DeviceConfig,
    /// The official tool's mode script (`-sm`), uploaded as `mode.scr`.
    /// Not with [`DeviceConfig::Keep`].
    pub mode_script: Option<Bytes>,
    /// See [`Offer::remove_branding`].
    pub remove_branding: bool,
    /// Software license key for the offer, or empty. At most
    /// [`message::MAX_LICENSE_KEY`] bytes, no line breaks or NUL.
    pub license_key: String,
    pub after_install: AfterInstall,
}

impl InstallPlan {
    /// The offer's fields.
    pub fn offer(&self) -> Offer {
        Offer {
            license_key: self.license_key.clone(),
            keep_config: self.config == DeviceConfig::Keep,
            remove_branding: self.remove_branding,
        }
    }

    /// Every file to upload, in order: the packages, then (as the official
    /// tool orders them) `autorun.scr` and `mode.scr` for a script,
    /// `mode.scr` and an empty `autorun.scr` for an empty configuration,
    /// `mode.scr` alone for the default one.
    pub fn files(&self) -> Vec<(String, Bytes)> {
        let mut out: Vec<(String, Bytes)> = self
            .packages
            .iter()
            .map(|p| (p.name.clone(), p.data.clone()))
            .collect();
        let autorun = |data: Bytes| (message::CONFIG_SCRIPT_NAME.to_string(), data);
        let mode = self
            .mode_script
            .clone()
            .map(|data| (message::MODE_SCRIPT_NAME.to_string(), data));
        match &self.config {
            DeviceConfig::Keep => {}
            DeviceConfig::Default => out.extend(mode),
            DeviceConfig::Empty => {
                out.extend(mode);
                out.push(autorun(Bytes::new()));
            }
            DeviceConfig::Script(script) => {
                out.push(autorun(script.clone()));
                out.extend(mode);
            }
        }
        out
    }

    pub fn total_bytes(&self) -> u64 {
        self.files().iter().map(|(_, d)| d.len() as u64).sum()
    }

    /// Move the system package (`routeros.npk` or
    /// `routeros-<version>-<arch>.npk`) to the front, keeping the order of
    /// the others: the official tool sends it first whatever the order on
    /// its command line.
    pub fn system_package_first(&mut self) {
        let system = |p: &PlanFile| p.name == "routeros.npk" || p.name.starts_with("routeros-");
        if let Some(i) = self.packages.iter().position(system) {
            let p = self.packages.remove(i);
            self.packages.insert(0, p);
        }
    }
}

/// Reasons an [`InstallPlan`] is rejected before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    #[error("the plan contains no packages")]
    NoPackages,
    #[error("{name} does not look like a RouterOS package (missing NPK magic)")]
    NotAnNpk { name: String },
    /// A package name is empty, contains `\n` or `/`, or is one of the
    /// reserved script names ([`message::CONFIG_SCRIPT_NAME`],
    /// [`message::MODE_SCRIPT_NAME`]).
    #[error("invalid file name {name:?}")]
    InvalidFileName { name: String },
    #[error("the plan needs {frames} frames, at most {max} are supported")]
    TooManyFrames { frames: usize, max: usize },
    /// The official tool refuses this too ("Cannot apply mode script and
    /// keep old config at the same time").
    #[error("a mode script cannot be combined with keeping the configuration")]
    ModeScriptWithKeep,
    #[error("the license key is too long or contains a line break or NUL")]
    InvalidLicenseKey,
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
        if self.mode_script.is_some() && self.config == DeviceConfig::Keep {
            return Err(PlanError::ModeScriptWithKeep);
        }
        if self.license_key.len() > message::MAX_LICENSE_KEY
            || self.license_key.contains(['\n', '\r', '\0'])
        {
            return Err(PlanError::InvalidLicenseKey);
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
/// file names on the device; the script names are reserved.
fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\n', '/', '\0'])
        && name != message::CONFIG_SCRIPT_NAME
        && name != message::MODE_SCRIPT_NAME
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
    /// Re-sends of an unanswered frame before failing; also the number of
    /// offers made again after a `NACK`.
    pub max_retransmits: u32,
    /// Pause between file chunks (pyNetinstall uses 5 ms to avoid state errors).
    pub chunk_delay: Duration,
    /// Pause before offering again after a `NACK` (the official server
    /// waits 1 s).
    pub offer_retry: Duration,
    /// How long to keep polling after the last file for `WTRM`, counted
    /// from the first poll, before falling back to `FILE\n` (at most
    /// [`MAX_FINISH_POLLS`] polls are sent whatever the timing).
    pub finish_timeout: Duration,
    /// Pause between those polls (the first follows the last file at once).
    pub poll_interval: Duration,
    /// Wait for the answer to a poll after the last file (the device may be
    /// busy writing its flash). Unanswered polls are re-sent while
    /// [`SessionConfig::finish_timeout`] lasts.
    pub finish_reply_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reply_timeout: Duration::from_secs(10),
            format_timeout: Duration::from_secs(120),
            optional_timeout: Duration::from_secs(10),
            max_retransmits: 3,
            chunk_delay: Duration::from_millis(5),
            offer_retry: Duration::from_secs(1),
            finish_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_secs(1),
            finish_reply_timeout: Duration::from_secs(20),
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
    /// The device answered a poll with `WTRM` while `remaining` files were
    /// still to be sent (the official server would skip them silently).
    #[error("device stopped receiving with {remaining} file(s) left")]
    EndedEarly { remaining: usize },
    #[error("installation cancelled")]
    Cancelled,
}

impl FailReason {
    /// Stable code for UI translation.
    pub fn code(&self) -> &'static str {
        match self {
            FailReason::Timeout { .. } => "timeout",
            FailReason::Rejected { .. } => "rejected",
            FailReason::EndedEarly { .. } => "ended_early",
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
    /// The device never answered `WTRM` after the last file (the polls ran
    /// out, went unanswered or were refused); `TERM` was sent anyway and
    /// [`Event::Done`] follows. The device got every file, but did not
    /// confirm it is ready.
    FinishUnconfirmed,
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
    device: DeviceInfo,
    cfg: SessionConfig,
    steps: Vec<Step>,
    idx: usize,
    seq: u16,
    wait: Wait,
    phase: Phase,
    last_frame: Option<Frame>,
    /// `[0:6]` of our next frame: `[6:12]` of the device's last reply.
    echo: Mac,
    /// Polls sent after the last file.
    finish_polls: u32,
    /// Until when polls after the last file are sent (set by the first).
    finish_deadline: Option<Instant>,
    /// The device said `WTRM` after the last file.
    finish_confirmed: bool,
    bytes_sent: u64,
    bytes_total: u64,
    events: VecDeque<Event>,
}

impl ServerSession {
    pub fn new(
        device: DeviceInfo,
        plan: &InstallPlan,
        cfg: SessionConfig,
        now: Instant,
    ) -> Result<Self, PlanError> {
        plan.validate()?;
        let mut events = VecDeque::new();
        events.push_back(Event::PhaseChanged(Phase::Offering));
        let echo = device.hello_dst;
        Ok(Self {
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
            echo,
            finish_polls: 0,
            finish_deadline: None,
            finish_confirmed: false,
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
            self.echo,
            self.device.mac,
            self.seq,
            self.seq.saturating_sub(2),
            payload,
        );
        self.last_frame = Some(frame.clone());
        if expect == (Expect::Poll { remaining: 0 }) && self.finish_deadline.is_none() {
            self.finish_deadline = Some(now + self.cfg.finish_timeout);
        }
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
        let Wait::Reply { retries_left, .. } = self.wait else {
            return;
        };
        if frame.src != self.device.mac || frame.seq != self.seq || frame.ack != self.seq {
            return;
        }
        let step = &self.steps[self.idx];
        let nack = frame.payload.starts_with(message::NACK);
        let wtrm = frame.payload.as_ref() == message::WTRM;
        match step.expect {
            Expect::Poll { remaining: 0 } => {
                self.echo = frame.dst;
                if wtrm {
                    // Done: skip the `FILE\n` fallback, straight to `TERM`.
                    self.finish_confirmed = true;
                    self.idx = self.steps.len() - 2;
                    self.advance(now, false);
                } else if nack {
                    self.advance(now, false); // to the fallback
                } else {
                    self.finish_polls += 1;
                    let at = now + self.cfg.poll_interval;
                    let open = self.finish_deadline.is_some_and(|d| at <= d);
                    if open && self.finish_polls < MAX_FINISH_POLLS {
                        // Poll again (a new frame, not a re-send).
                        self.wait = Wait::Send { at, resend: None };
                    } else {
                        self.advance(now, false);
                    }
                }
                return;
            }
            Expect::Fallback => {
                // Whatever the answer, `TERM` follows.
                self.echo = frame.dst;
                self.finish_confirmed |= wtrm;
                self.advance(now, false);
                return;
            }
            Expect::Offer if nack => {
                if retries_left == 0 {
                    self.fail(FailReason::Rejected {
                        phase: self.phase.name(),
                    });
                } else {
                    self.wait = Wait::Send {
                        at: now + self.cfg.offer_retry,
                        resend: Some(retries_left - 1),
                    };
                }
                return;
            }
            _ if nack => {
                self.fail(FailReason::Rejected {
                    phase: self.phase.name(),
                });
                return;
            }
            Expect::Poll { remaining } if remaining > 0 && wtrm => {
                self.fail(FailReason::EndedEarly { remaining });
                return;
            }
            _ => {}
        }
        self.echo = frame.dst;
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
        let expect = self.steps[self.idx].expect;
        let finishing = expect == (Expect::Poll { remaining: 0 });
        let window_open = self.finish_deadline.is_some_and(|d| now < d);
        if retries_left > 0 && (!finishing || window_open) {
            self.wait = Wait::Send {
                at: now,
                resend: Some(retries_left - 1),
            };
        } else if matches!(expect, Expect::Optional | Expect::Fallback) || finishing {
            // Some devices never answer this step; move on. After the last
            // file this leads to `FILE\n` and then `TERM`.
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
            TimeoutKind::Finish => self.cfg.finish_reply_timeout,
        }
    }

    fn advance(&mut self, now: Instant, delay: bool) {
        self.idx += 1;
        if self.idx >= self.steps.len() {
            self.wait = Wait::Finished;
            self.phase = Phase::Done;
            if !self.finish_confirmed {
                self.events.push_back(Event::FinishUnconfirmed);
            }
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
            hello_dst: Mac::BROADCAST,
        }
    }

    fn plan() -> InstallPlan {
        InstallPlan {
            packages: vec![PlanFile {
                name: "routeros.npk".into(),
                data: npk(10),
            }],
            ..InstallPlan::default()
        }
    }

    /// `len` bytes starting with the NPK magic.
    fn npk(len: usize) -> Bytes {
        let mut v = message::NPK_MAGIC.to_vec();
        v.resize(len.max(4), 7);
        Bytes::from(v)
    }

    fn session(plan: &InstallPlan, now: Instant) -> ServerSession {
        ServerSession::new(device(), plan, SessionConfig::default(), now).unwrap()
    }

    fn new_err(plan: &InstallPlan) -> PlanError {
        match ServerSession::new(device(), plan, SessionConfig::default(), Instant::now()) {
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

    fn names(p: &InstallPlan) -> Vec<(String, usize)> {
        p.files().into_iter().map(|(n, d)| (n, d.len())).collect()
    }

    #[test]
    fn rejects_plan_without_packages() {
        assert_eq!(new_err(&InstallPlan::default()), PlanError::NoPackages);
    }

    #[test]
    fn rejects_first_package_without_npk_magic() {
        let p = InstallPlan {
            packages: vec![file("routeros.npk", Bytes::from_static(b"PK\x03\x04rest"))],
            ..InstallPlan::default()
        };
        assert_eq!(
            new_err(&p),
            PlanError::NotAnNpk {
                name: "routeros.npk".into()
            }
        );
        let short = InstallPlan {
            packages: vec![file("tiny.npk", Bytes::from_static(&[0x1E, 0xF1]))],
            ..InstallPlan::default()
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
            "nul\0.npk",
            message::CONFIG_SCRIPT_NAME,
            message::MODE_SCRIPT_NAME,
        ] {
            let p = InstallPlan {
                packages: vec![file("routeros.npk", npk(10)), file(bad, npk(10))],
                ..InstallPlan::default()
            };
            assert_eq!(new_err(&p), PlanError::InvalidFileName { name: bad.into() });
        }
    }

    #[test]
    fn rejects_a_mode_script_with_keep_and_bad_license_keys() {
        let mut p = plan();
        p.config = DeviceConfig::Keep;
        p.mode_script = Some(Bytes::from_static(b"x"));
        assert_eq!(new_err(&p), PlanError::ModeScriptWithKeep);
        for bad in [
            "a\nb",
            "a\rb",
            "a\0b",
            &"k".repeat(message::MAX_LICENSE_KEY + 1),
        ] {
            let mut p = plan();
            p.license_key = bad.into();
            assert_eq!(new_err(&p), PlanError::InvalidLicenseKey, "{bad:?}");
        }
        let mut p = plan();
        p.license_key = "k".repeat(message::MAX_LICENSE_KEY);
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn rejects_plans_needing_more_than_u16_max_frames() {
        // Each package costs 2 frames (header, poll) plus one per chunk,
        // plus 5 fixed frames (OFFR, 2 empties, FILE\n, TERM) and up to
        // MAX_FINISH_POLLS - 1 extra polls.
        let plan_with = |n: usize| InstallPlan {
            packages: (0..n)
                .map(|i| {
                    file(
                        &format!("p{i}.npk"),
                        if i == 0 { npk(4) } else { Bytes::new() },
                    )
                })
                .collect(),
            ..InstallPlan::default()
        };
        let fixed = 5 + MAX_FINISH_POLLS as usize - 1;
        // The first package has one chunk.
        let max = (usize::from(u16::MAX) - fixed - 1) / 2;
        assert!(plan_with(max).validate().is_ok());
        assert_eq!(
            new_err(&plan_with(max + 1)),
            PlanError::TooManyFrames {
                frames: fixed + 1 + 2 * (max + 1),
                max: usize::from(u16::MAX),
            }
        );
    }

    #[test]
    fn files_follow_the_official_order() {
        let script = Bytes::from_static(b"/system identity set name=x\n");
        let mode = Bytes::from_static(b":log info mode\n");
        let mut p = plan();
        p.config = DeviceConfig::Keep;
        assert_eq!(names(&p), [("routeros.npk".into(), 10)]);
        p.config = DeviceConfig::Default;
        assert_eq!(names(&p), [("routeros.npk".into(), 10)]);
        p.mode_script = Some(mode.clone());
        assert_eq!(
            names(&p),
            [("routeros.npk".into(), 10), ("mode.scr".into(), 15)]
        );
        p.config = DeviceConfig::Empty;
        assert_eq!(
            names(&p),
            [
                ("routeros.npk".into(), 10),
                ("mode.scr".into(), 15),
                ("autorun.scr".into(), 0)
            ]
        );
        p.config = DeviceConfig::Script(script);
        assert_eq!(
            names(&p),
            [
                ("routeros.npk".into(), 10),
                ("autorun.scr".into(), 28),
                ("mode.scr".into(), 15)
            ]
        );
        assert_eq!(p.total_bytes(), 10 + 28 + 15);
        p.mode_script = None;
        assert_eq!(
            names(&p),
            [("routeros.npk".into(), 10), ("autorun.scr".into(), 28)]
        );
    }

    #[test]
    fn the_system_package_goes_first() {
        let mut p = InstallPlan {
            packages: vec![
                file("container-7.25.1-arm64.npk", npk(4)),
                file("wifi-qcom-7.25.1-arm64.npk", npk(4)),
                file("routeros-7.25.1-arm64.npk", npk(4)),
            ],
            ..InstallPlan::default()
        };
        p.system_package_first();
        let order: Vec<&str> = p.packages.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            order,
            [
                "routeros-7.25.1-arm64.npk",
                "container-7.25.1-arm64.npk",
                "wifi-qcom-7.25.1-arm64.npk"
            ]
        );
        let mut q = plan();
        q.packages.insert(0, file("a.npk", npk(4)));
        q.system_package_first();
        assert_eq!(q.packages[0].name, "routeros.npk");
    }

    #[test]
    fn the_offer_carries_keep_key_and_branding() {
        let mut p = plan();
        assert_eq!(p.offer(), Offer::default());
        p.config = DeviceConfig::Keep;
        p.license_key = "KEY".into();
        p.remove_branding = true;
        assert_eq!(
            p.offer(),
            Offer {
                license_key: "KEY".into(),
                keep_config: true,
                remove_branding: true
            }
        );
        let now = Instant::now();
        let mut s = session(&p, now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!(&f.payload[..], b"OFFR\nKEY\nkeep\n\n\0");
    }

    fn reply(seq: u16, payload: &'static [u8]) -> Frame {
        Frame::new(DEVICE, Mac::ZERO, seq, seq, payload)
    }

    #[test]
    fn first_frame_is_offer_with_seq_1_ack_0_to_the_broadcast_slot() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (Mac::BROADCAST, DEVICE, 1, 0));
        assert_eq!(&f.payload[..], b"OFFR\n\n\nremoveBranding\n\0");
        assert_eq!(s.poll_transmit(now), None, "must wait for reply");
    }

    #[test]
    fn the_offer_echoes_the_hellos_dst() {
        let now = Instant::now();
        let mut info = device();
        info.hello_dst = Mac([0x02, 0, 0, 0, 0, 0x42]);
        let mut s = ServerSession::new(info, &plan(), SessionConfig::default(), now).unwrap();
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst), (Mac([0x02, 0, 0, 0, 0, 0x42]), DEVICE));
    }

    #[test]
    fn mac_fields_mirror_the_device_and_ack_is_seq_minus_two() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        let echo = Mac([0x12; 6]);
        s.handle_frame(&Frame::new(DEVICE, echo, 1, 1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (echo, DEVICE, 2, 0));
        s.handle_frame(&reply(2, message::STRT), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.src, f.dst, f.seq, f.ack), (Mac::ZERO, DEVICE, 3, 1));
    }

    #[test]
    fn ignores_replies_with_wrong_seq_or_source() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, Mac::ZERO, 1, 0, &b"YACK\n"[..]), now);
        s.handle_frame(
            &Frame::new(Mac([0xbb; 6]), Mac::ZERO, 1, 1, &b"YACK\n"[..]),
            now,
        );
        assert_eq!(s.poll_transmit(now), None);
        s.handle_frame(&reply(1, message::YACK), now);
        let f = s.poll_transmit(now).unwrap();
        assert_eq!((f.seq, f.ack), (2, 0));
        assert!(f.payload.is_empty());
    }

    #[test]
    fn our_own_frames_are_never_taken_for_replies() {
        // A device that puts its own MAC in [6:12] makes us echo it in
        // [0:6]; our looped-back broadcast then has `src` = the device,
        // but `ack` is `seq - 2`, never `seq`.
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&Frame::new(DEVICE, DEVICE, 1, 1, message::YACK), now);
        let ours = s.poll_transmit(now).unwrap();
        assert_eq!(ours.src, DEVICE);
        s.handle_frame(&ours, now);
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.phase(), &Phase::Formatting);
    }

    #[test]
    fn formatting_step_uses_format_timeout() {
        let now = Instant::now();
        let cfg = SessionConfig::default();
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
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
        let mut s = ServerSession::new(device(), &plan(), cfg, now).unwrap();
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
    fn nack_to_the_offer_offers_again_then_fails_with_rejected() {
        let now = Instant::now();
        let cfg = SessionConfig {
            max_retransmits: 2,
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        let offer = s.poll_transmit(now).unwrap();
        let mut t = now;
        for _ in 0..2 {
            s.handle_frame(&reply(1, b"NACK\n"), t);
            assert_eq!(s.poll_transmit(t), None, "not at once");
            t = s.poll_timeout().unwrap();
            assert_eq!(s.poll_transmit(t), Some(offer.clone()), "same frame again");
        }
        s.handle_frame(&reply(1, b"NACK\n"), t);
        assert!(s.is_finished());
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::Failed(FailReason::Rejected { phase: "offering" })
            ]
        );
        // A device that accepts the second offer goes on normally.
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, b"NACK\n"), now);
        let t = now + cfg.offer_retry;
        assert_eq!(s.poll_timeout(), Some(t));
        assert_eq!(s.poll_transmit(t).map(|f| f.seq), Some(1));
        s.handle_frame(&reply(1, message::YACK), t);
        assert_eq!(s.poll_transmit(t).map(|f| f.seq), Some(2));
    }

    #[test]
    fn nack_after_the_offer_fails_with_rejected() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(2, b"NACK\nno space\n"), now);
        assert!(s.is_finished());
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::Rejected {
                phase: "formatting"
            }))
        );
    }

    /// Answer every frame of `s` with what `answer` returns for its payload
    /// until the session ends; returns the payloads sent (data frames as
    /// `<len>`) with their seq.
    fn run(
        s: &mut ServerSession,
        now: Instant,
        mut answer: impl FnMut(&[u8]) -> &'static [u8],
    ) -> Vec<(u16, String)> {
        let mut out = Vec::new();
        let mut t = now;
        while !s.is_finished() {
            match s.poll_transmit(t) {
                Some(f) => {
                    let p = &f.payload[..];
                    let shown = if p.is_empty() || p.ends_with(b"\0") || p == message::FILE_END {
                        String::from_utf8_lossy(p).into_owned()
                    } else {
                        format!("<{}>", p.len())
                    };
                    out.push((f.seq, shown));
                    if !p.starts_with(b"TERM") {
                        s.handle_frame(&reply(f.seq, answer(p)), t);
                    }
                }
                None => t = s.poll_timeout().unwrap(),
            }
        }
        out
    }

    /// The device sim.py emulates: `STRT` to the format frame, `RETR` to
    /// the prepare frame, then `RETR` to the first poll after a file and
    /// `WTRM` to a poll right after another poll.
    fn official_device() -> impl FnMut(&[u8]) -> &'static [u8] {
        let (mut empties, mut polled) = (0, false);
        move |p: &[u8]| {
            if p.starts_with(b"OFFR") {
                return message::YACK;
            }
            if !p.is_empty() {
                polled = false;
                return message::RETR;
            }
            empties += 1;
            let answer = match empties {
                1 => message::STRT,
                2 => message::RETR,
                _ if polled => message::WTRM,
                _ => message::RETR,
            };
            polled = empties > 2;
            answer
        }
    }

    #[test]
    fn official_flow_with_a_script_and_a_mode_script() {
        let now = Instant::now();
        let mut p = plan();
        p.packages[0].data = npk(1500);
        p.config = DeviceConfig::Script(Bytes::from_static(b"/x\n"));
        p.mode_script = Some(Bytes::from_static(b":y\n"));
        let mut s = session(&p, now);
        let frames = run(&mut s, now, official_device());
        let want: Vec<(u16, &str)> = vec![
            (1, "OFFR\n\n\nremoveBranding\n\0"),
            (2, ""),
            (3, ""),
            (4, "FILE\nrouteros.npk\n1500\n\0"),
            (5, "<1024>"),
            (6, "<476>"),
            (7, ""),
            (8, "FILE\nautorun.scr\n3\n\0"),
            (9, "<3>"),
            (10, ""),
            (11, "FILE\nmode.scr\n3\n\0"),
            (12, "<3>"),
            (13, ""),
            (14, ""),
            (15, "TERM\n\0"),
        ];
        let got: Vec<(u16, &str)> = frames.iter().map(|(q, p)| (*q, p.as_str())).collect();
        assert_eq!(got, want);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done));
    }

    #[test]
    fn empty_config_sends_a_header_only_autorun() {
        let now = Instant::now();
        let mut p = plan();
        p.config = DeviceConfig::Empty;
        let mut s = session(&p, now);
        let frames = run(&mut s, now, official_device());
        let tail: Vec<&str> = frames[3..].iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(
            tail,
            [
                "FILE\nrouteros.npk\n10\n\0",
                "<10>",
                "",
                "FILE\nautorun.scr\n0\n\0",
                "",
                "",
                "TERM\n\0"
            ]
        );
    }

    #[test]
    fn wtrm_before_the_last_file_fails_with_ended_early() {
        let now = Instant::now();
        let mut p = plan();
        p.packages.push(file("extra.npk", npk(5)));
        p.config = DeviceConfig::Empty;
        let mut s = session(&p, now);
        let frames = run(&mut s, now, |payload| {
            if payload.starts_with(b"OFFR") {
                message::YACK
            } else if payload.is_empty() {
                message::WTRM
            } else {
                message::RETR
            }
        });
        // OFFR, format, prepare: WTRM to the format/prepare polls is just
        // "any reply"; the poll after routeros.npk is where it counts.
        assert_eq!(frames.last().map(|(_, p)| p.as_str()), Some(""));
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events.last(),
            Some(&Event::Failed(FailReason::EndedEarly { remaining: 2 }))
        );
        assert_eq!(
            FailReason::EndedEarly { remaining: 2 }.code(),
            "ended_early"
        );
    }

    #[test]
    fn a_device_that_never_says_wtrm_gets_file_end_after_the_polls() {
        let now = Instant::now();
        let cfg = SessionConfig {
            finish_timeout: Duration::from_secs(2),
            ..SessionConfig::default()
        };
        let mut s = ServerSession::new(device(), &plan(), cfg.clone(), now).unwrap();
        let mut times = Vec::new();
        let mut t = now;
        let mut payloads = Vec::new();
        while !s.is_finished() {
            match s.poll_transmit(t) {
                Some(f) => {
                    payloads.push(f.payload.to_vec());
                    times.push(t);
                    let answer: &[u8] = if f.payload.starts_with(b"OFFR") {
                        message::YACK
                    } else {
                        message::RETR
                    };
                    s.handle_frame(&reply(f.seq, answer), t);
                }
                None => t = s.poll_timeout().unwrap(),
            }
        }
        let tail: Vec<&[u8]> = payloads[5..].iter().map(Vec::as_slice).collect();
        assert_eq!(
            tail,
            [&b""[..], b"", b"", message::FILE_END, b"TERM\n\0"],
            "3 polls, FILE\\n, TERM"
        );
        // The first poll follows the file at once, the others are paced.
        assert_eq!(times[5] - times[4], Duration::ZERO);
        assert_eq!(times[6] - times[5], cfg.poll_interval);
        assert_eq!(times[7] - times[6], cfg.poll_interval);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(events.last(), Some(&Event::Done));
    }

    /// A device that answers every frame `RETR` except the polls after the
    /// last file, which it answers with `finish(t)` (`None`: silence), and
    /// `FILE\n`, which it answers with `file_end` (`None`: silence).
    /// Returns the payloads sent with their times and the events.
    fn run_finish(
        cfg: SessionConfig,
        mut finish: impl FnMut(Duration) -> Option<&'static [u8]>,
        file_end: Option<&'static [u8]>,
    ) -> (Vec<(Duration, Vec<u8>)>, Vec<Event>) {
        let now = Instant::now();
        let mut s = ServerSession::new(device(), &plan(), cfg, now).unwrap();
        let mut t = now;
        let mut sent = Vec::new();
        let mut last_file_at = None;
        while !s.is_finished() {
            let Some(f) = s.poll_transmit(t) else {
                t = s.poll_timeout().unwrap();
                s.handle_timeout(t);
                continue;
            };
            sent.push((t - now, f.payload.to_vec()));
            let p = &f.payload[..];
            let answer = if p.starts_with(b"OFFR") {
                Some(message::YACK)
            } else if p == message::FILE_END {
                file_end
            } else if p.starts_with(b"TERM") {
                None
            } else if p.is_empty() && sent.len() > 5 {
                let since = t - *last_file_at.get_or_insert(t);
                finish(since)
            } else {
                Some(message::RETR)
            };
            if let Some(a) = answer {
                s.handle_frame(&reply(f.seq, a), t);
            }
        }
        (sent, std::iter::from_fn(|| s.poll_event()).collect())
    }

    fn payloads(sent: &[(Duration, Vec<u8>)]) -> Vec<&[u8]> {
        sent.iter().map(|(_, p)| p.as_slice()).collect()
    }

    #[test]
    fn a_slow_device_is_polled_every_second_until_it_says_wtrm() {
        // RETR to the polls for 30 s (far past the old 5 s bound), then WTRM.
        let (sent, events) = run_finish(
            SessionConfig::default(),
            |since| {
                Some(if since >= Duration::from_secs(30) {
                    message::WTRM
                } else {
                    message::RETR
                })
            },
            Some(message::WTRM),
        );
        let p = payloads(&sent);
        assert_eq!(p.last(), Some(&&b"TERM\n\0"[..]));
        assert!(!p.contains(&message::FILE_END), "no fallback needed");
        assert_eq!(p[5..p.len() - 1].len(), 31, "polls at 0, 1, ..., 30 s");
        let gaps: Vec<Duration> = sent[5..sent.len() - 1]
            .windows(2)
            .map(|w| w[1].0 - w[0].0)
            .collect();
        assert!(
            gaps.iter().all(|g| *g == Duration::from_secs(1)),
            "{gaps:?}"
        );
        assert_eq!(events.last(), Some(&Event::Done));
        assert!(!events.contains(&Event::FinishUnconfirmed));
    }

    #[test]
    fn polling_stops_after_the_finish_timeout_then_file_end_and_term() {
        let cfg = SessionConfig::default();
        let (sent, events) = run_finish(cfg.clone(), |_| Some(message::RETR), Some(message::RETR));
        let p = payloads(&sent);
        assert_eq!(&p[p.len() - 2..], [message::FILE_END, b"TERM\n\0"]);
        let polls = &sent[5..sent.len() - 2];
        assert_eq!(polls.len(), 61, "polls at 0, 1, ..., 60 s");
        assert_eq!(polls.last().unwrap().0 - polls[0].0, cfg.finish_timeout);
        assert_eq!(
            events[events.len() - 2..],
            [Event::FinishUnconfirmed, Event::Done]
        );
    }

    #[test]
    fn a_nack_to_file_end_still_gets_term() {
        let (sent, events) = run_finish(
            SessionConfig::default(),
            |_| Some(message::RETR),
            Some(b"NACK\n"),
        );
        assert_eq!(
            sent.last().map(|(_, p)| p.as_slice()),
            Some(&b"TERM\n\0"[..])
        );
        assert_eq!(
            events[events.len() - 2..],
            [Event::FinishUnconfirmed, Event::Done]
        );
    }

    #[test]
    fn a_nack_to_a_finish_poll_still_gets_term() {
        let (sent, events) = run_finish(
            SessionConfig::default(),
            |_| Some(b"NACK\n"),
            Some(message::WTRM),
        );
        let p = payloads(&sent);
        assert_eq!(&p[p.len() - 2..], [message::FILE_END, b"TERM\n\0"]);
        // WTRM to the fallback confirms the end.
        assert_eq!(events.last(), Some(&Event::Done));
        assert!(!events.contains(&Event::FinishUnconfirmed));
    }

    #[test]
    fn a_device_silent_after_the_last_file_still_gets_term() {
        let cfg = SessionConfig::default();
        let (sent, events) = run_finish(cfg.clone(), |_| None, None);
        let p = payloads(&sent);
        assert_eq!(p.last(), Some(&&b"TERM\n\0"[..]));
        assert!(p.contains(&message::FILE_END));
        // The poll is re-sent while the finish window lasts, then FILE\n
        // (re-sent, unanswered), then TERM: bounded.
        let end = sent.last().unwrap().0 - sent[5].0;
        assert!(
            end <= cfg.finish_timeout
                + cfg.finish_reply_timeout
                + cfg.optional_timeout * (cfg.max_retransmits + 1),
            "{end:?}"
        );
        assert_eq!(
            events[events.len() - 2..],
            [Event::FinishUnconfirmed, Event::Done]
        );
    }

    #[test]
    fn finish_polls_are_capped() {
        let now = Instant::now();
        // A poll every 1 ms for `polls` ms: polls + 1 of them, at most MAX_FINISH_POLLS.
        for (polls, sent) in [
            (0, 1),
            (3, 4),
            (MAX_FINISH_POLLS + 5, MAX_FINISH_POLLS as usize),
        ] {
            let cfg = SessionConfig {
                finish_timeout: Duration::from_millis(polls.into()),
                poll_interval: Duration::from_millis(1),
                ..SessionConfig::default()
            };
            let mut s = ServerSession::new(device(), &plan(), cfg, now).unwrap();
            let frames = run(&mut s, now, |p| {
                if p.starts_with(b"OFFR") {
                    message::YACK
                } else {
                    message::RETR
                }
            });
            let empties_after_data = frames[5..].iter().filter(|(_, p)| p.is_empty()).count();
            assert_eq!(empties_after_data, sent, "finish window {polls} ms");
        }
    }

    #[test]
    fn failure_emits_failed_event_but_no_phase_change() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(1, message::YACK), now);
        s.poll_transmit(now).unwrap();
        s.handle_frame(&reply(2, b"NACK"), now);
        assert_eq!(s.phase(), &Phase::Failed);
        let events: Vec<Event> = std::iter::from_fn(|| s.poll_event()).collect();
        assert_eq!(
            events,
            [
                Event::PhaseChanged(Phase::Offering),
                Event::PhaseChanged(Phase::Formatting),
                Event::Failed(FailReason::Rejected {
                    phase: "formatting"
                })
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
    fn default_timeouts() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.optional_timeout, Duration::from_secs(10));
        assert_eq!(cfg.offer_retry, Duration::from_secs(1));
        assert_eq!(cfg.poll_interval, Duration::from_secs(1));
        assert_eq!(cfg.finish_timeout, Duration::from_secs(60));
        assert!(cfg.finish_reply_timeout > cfg.reply_timeout);
        // The whole window fits in MAX_FINISH_POLLS polls.
        assert!(cfg.finish_timeout.as_secs() < u64::from(MAX_FINISH_POLLS));
    }

    #[test]
    fn upload_phase_and_progress_name_the_file() {
        let now = Instant::now();
        let mut s = session(&plan(), now);
        for (seq, answer) in [(1, message::YACK), (2, message::STRT), (3, message::RETR)] {
            s.poll_transmit(now).unwrap();
            s.handle_frame(&reply(seq, answer), now);
        }
        s.poll_transmit(now).unwrap(); // header
        assert_eq!(
            s.phase(),
            &Phase::Uploading {
                file: Arc::from("routeros.npk")
            }
        );
        s.handle_frame(&reply(4, message::RETR), now);
        let chunk = s.poll_transmit(now).unwrap();
        assert_eq!((chunk.seq, chunk.payload.len()), (5, 10));
        s.handle_frame(&reply(5, message::RETR), now);
        let progress = std::iter::from_fn(|| s.poll_event()).find_map(|e| match e {
            Event::Progress {
                file,
                file_sent,
                bytes_sent,
                bytes_total,
                ..
            } => Some((file, file_sent, bytes_sent, bytes_total)),
            _ => None,
        });
        assert_eq!(progress, Some((Arc::from("routeros.npk"), 10, 10, 10)));
        let poll = s.poll_transmit(now).expect("poll right after the file");
        assert!(poll.payload.is_empty());
        assert_eq!(s.phase(), &Phase::Finishing);
    }
}
