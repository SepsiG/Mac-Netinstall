//! Translation of an [`InstallPlan`] into the list of frames to send.

use super::{InstallPlan, Phase};
use crate::frame::MAX_CHUNK;
use crate::message;
use bytes::Bytes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Expect {
    /// Do not wait for a reply.
    None,
    /// Any reply with matching sequence numbers.
    Any,
    /// Like `Any`, but once retransmits are exhausted the session moves on.
    Optional,
    /// Only a reply with exactly this payload.
    Payload(&'static [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeoutKind {
    Reply,
    Format,
    Optional,
}

#[derive(Debug, Clone)]
pub(super) struct ChunkInfo {
    pub(super) file: String,
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
    fn control(phase: Phase, payload: &'static [u8], expect: Expect, timeout: TimeoutKind) -> Self {
        Self {
            phase,
            payload: Bytes::from_static(payload),
            expect,
            timeout,
            chunk: None,
            delay_after: false,
        }
    }
}

/// Number of file chunks `push_file` emits for `len` bytes (at least one).
fn chunk_count(len: usize) -> usize {
    len.div_ceil(MAX_CHUNK).max(1)
}

/// Number of frames [`build_steps`] produces for `plan`, without building it.
pub(super) fn frame_count(plan: &InstallPlan) -> usize {
    // OFFR, format, prepare / final empty, FILE\n, TERM
    let fixed = 6;
    // header + chunks + spacer
    let packages: usize = plan
        .packages
        .iter()
        .map(|p| 2 + chunk_count(p.data.len()))
        .sum();
    // header + chunks
    let script = plan
        .config_script
        .as_ref()
        .map_or(0, |s| 1 + chunk_count(s.len()));
    fixed + packages + script
}

pub(super) fn build_steps(plan: &InstallPlan) -> Vec<Step> {
    use Expect::Any;
    use TimeoutKind::{Format, Reply};

    let mut steps = vec![
        Step::control(Phase::Offering, message::OFFR, Any, Reply),
        Step::control(Phase::Formatting, b"", Any, Format),
        Step::control(Phase::Preparing, b"", Any, Reply),
    ];
    for pkg in &plan.packages {
        push_file(&mut steps, &pkg.name, &pkg.data);
        let phase = Phase::Uploading {
            file: pkg.name.clone(),
        };
        steps.push(Step::control(phase, b"", Any, Reply));
    }
    if let Some(script) = &plan.config_script {
        push_file(&mut steps, message::CONFIG_SCRIPT_NAME, script);
    }
    steps.push(Step::control(Phase::Finishing, b"", Any, Reply));
    steps.push(Step::control(
        Phase::Finishing,
        message::FILE_END,
        Any,
        Reply,
    ));
    steps.push(Step::control(
        Phase::Rebooting,
        message::TERM,
        Expect::None,
        Reply,
    ));
    steps
}

fn push_file(steps: &mut Vec<Step>, name: &str, data: &Bytes) {
    let phase = Phase::Uploading {
        file: name.to_string(),
    };
    let total = data.len() as u64;
    steps.push(Step {
        phase: phase.clone(),
        payload: Bytes::from(message::file_header(name, total)),
        expect: Expect::Optional,
        timeout: TimeoutKind::Optional,
        chunk: None,
        delay_after: false,
    });
    let chunks: Vec<Bytes> = if data.is_empty() {
        vec![Bytes::new()]
    } else {
        (0..data.len())
            .step_by(MAX_CHUNK)
            .map(|o| data.slice(o..(o + MAX_CHUNK).min(data.len())))
            .collect()
    };
    let last = chunks.len() - 1;
    let mut sent = 0u64;
    for (i, chunk) in chunks.into_iter().enumerate() {
        sent += chunk.len() as u64;
        let is_last = i == last;
        steps.push(Step {
            phase: phase.clone(),
            payload: chunk,
            expect: if is_last {
                Expect::Payload(message::RETR)
            } else {
                Expect::Any
            },
            timeout: TimeoutKind::Reply,
            chunk: Some(ChunkInfo {
                file: name.to_string(),
                file_sent: sent,
                file_total: total,
            }),
            delay_after: !is_last,
        });
    }
}
