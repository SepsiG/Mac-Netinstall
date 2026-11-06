//! Translation of an [`InstallPlan`] into the list of frames to send.

use super::{InstallPlan, Phase, MAX_FINISH_POLLS};
use crate::frame::MAX_CHUNK;
use crate::message;
use bytes::Bytes;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expect {
    /// Do not wait for a reply.
    None,
    /// The offer: `YACK` (any reply but `NACK`) moves on; `NACK` makes the
    /// session offer again.
    Offer,
    /// Any reply with matching sequence numbers.
    Any,
    /// Like `Any`, but once retransmits are exhausted the session moves on.
    Optional,
    /// The empty poll after a file. `remaining` files are still to be
    /// sent: then the device must not answer `WTRM`. After the last file
    /// (`remaining == 0`) `WTRM` ends the upload and anything else is
    /// polled again.
    Poll { remaining: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeoutKind {
    Reply,
    Format,
    Optional,
}

#[derive(Debug, Clone)]
pub(super) struct ChunkInfo {
    pub(super) file: Arc<str>,
    pub(super) file_sent: u64,
    pub(super) file_total: u64,
}

#[derive(Debug, Clone)]
pub(super) struct Step {
    pub(super) phase: Phase,
    pub(super) payload: Bytes,
    pub(super) expect: Expect,
    pub(super) timeout: TimeoutKind,
    pub(super) chunk: Option<ChunkInfo>,
    pub(super) delay_after: bool,
}

impl Step {
    fn control(phase: Phase, payload: Bytes, expect: Expect, timeout: TimeoutKind) -> Self {
        Self {
            phase,
            payload,
            expect,
            timeout,
            chunk: None,
            delay_after: false,
        }
    }
}

/// Number of data frames for a file of `len` bytes (none for an empty one).
fn chunk_count(len: usize) -> usize {
    len.div_ceil(MAX_CHUNK)
}

/// Most frames (new sequence numbers) [`build_steps`] can lead to for
/// `plan`: every step once, plus up to [`MAX_FINISH_POLLS`] polls after
/// the last file.
pub(super) fn frame_count(plan: &InstallPlan) -> usize {
    // OFFR, format, prepare, FILE\n, TERM
    let fixed = 5;
    // header + chunks + poll
    let files: usize = plan
        .files()
        .iter()
        .map(|(_, data)| 2 + chunk_count(data.len()))
        .sum();
    fixed + files + (MAX_FINISH_POLLS as usize - 1)
}

/// The steps for `plan`, in the official tool's order:
///
/// ```text
/// OFFR → "" (format) → "" (prepare)
///   → per file: FILE header → chunks → "" (poll)
///   → [FILE\n, only if the device never answers a poll with WTRM] → TERM
/// ```
///
/// The last step is always `TERM`, the one before it `FILE\n`.
pub(super) fn build_steps(plan: &InstallPlan) -> Vec<Step> {
    use Expect::Any;
    use TimeoutKind::{Format, Reply};

    let offer = Bytes::from(message::offer(&plan.offer()));
    let mut steps = vec![
        Step::control(Phase::Offering, offer, Expect::Offer, Reply),
        Step::control(Phase::Formatting, Bytes::new(), Any, Format),
        Step::control(Phase::Preparing, Bytes::new(), Any, Reply),
    ];
    let files = plan.files();
    for (i, (name, data)) in files.iter().enumerate() {
        let name: Arc<str> = Arc::from(name.as_str());
        push_file(&mut steps, Arc::clone(&name), data);
        let remaining = files.len() - 1 - i;
        let phase = if remaining == 0 {
            Phase::Finishing
        } else {
            Phase::Uploading { file: name }
        };
        steps.push(Step::control(
            phase,
            Bytes::new(),
            Expect::Poll { remaining },
            Reply,
        ));
    }
    steps.push(Step::control(
        Phase::Finishing,
        Bytes::from_static(message::FILE_END),
        Any,
        Reply,
    ));
    steps.push(Step::control(
        Phase::Rebooting,
        Bytes::from_static(message::term(plan.after_install)),
        Expect::None,
        Reply,
    ));
    steps
}

/// The header, then one frame per [`MAX_CHUNK`] bytes (an empty file has
/// no data frame).
fn push_file(steps: &mut Vec<Step>, name: Arc<str>, data: &Bytes) {
    let header = Bytes::from(message::file_header(&name, data.len() as u64));
    let phase = Phase::Uploading {
        file: Arc::clone(&name),
    };
    let total = data.len() as u64;
    steps.push(Step {
        phase: phase.clone(),
        payload: header,
        expect: Expect::Optional,
        timeout: TimeoutKind::Optional,
        chunk: None,
        delay_after: false,
    });
    let count = chunk_count(data.len());
    for i in 0..count {
        let start = i * MAX_CHUNK;
        let chunk = data.slice(start..(start + MAX_CHUNK).min(data.len()));
        let is_last = i + 1 == count;
        steps.push(Step {
            phase: phase.clone(),
            payload: chunk,
            expect: Expect::Any,
            timeout: TimeoutKind::Reply,
            chunk: Some(ChunkInfo {
                file: Arc::clone(&name),
                file_sent: ((start + MAX_CHUNK).min(data.len())) as u64,
                file_total: total,
            }),
            delay_after: !is_last,
        });
    }
}
