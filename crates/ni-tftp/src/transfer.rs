//! Sans-IO state machine for one read transfer (server side).
//!
//! Lock-step per RFC 1350: send block `n`, wait for ACK `n`, send `n + 1`.
//! The last block is shorter than `blksize`; if the file size is a
//! multiple of `blksize` (including 0), a final empty block is sent.
//! Block numbers wrap from 65535 to 0. Duplicate ACKs are ignored (no
//! "Sorcerer's Apprentice" resends); lost packets are recovered by
//! retransmitting on timeout.

use crate::packet::{encode_data, Packet};
use bytes::Bytes;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Why a transfer failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransferError {
    #[error("no ACK after {retransmits} retransmits")]
    Timeout { retransmits: u32 },
    #[error("client sent error {code}: {message}")]
    ClientError { code: u16, message: String },
    #[error("socket error: {0}")]
    Io(String),
}

/// Current state of a [`Transfer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed(TransferError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Send the current packet at `at`.
    Send {
        at: Instant,
    },
    /// Current packet sent; waiting for its ACK until `deadline`.
    Ack {
        deadline: Instant,
    },
    Finished,
}

/// One file being sent to one client.
#[derive(Debug)]
pub struct Transfer {
    file: String,
    data: Bytes,
    blksize: usize,
    timeout: Duration,
    max_retransmits: u32,
    /// OACK to send (and have acknowledged with ACK 0) before block 1.
    oack: Option<Vec<(String, String)>>,
    /// Block being sent: 0 = OACK, 1.. = data blocks (not wrapped).
    block: u64,
    last_block: u64,
    retries_left: u32,
    wait: Wait,
    status: Status,
}

impl Transfer {
    /// Start sending `data`. With `oack`, the OACK is sent first.
    pub fn new(
        file: String,
        data: Bytes,
        blksize: usize,
        timeout: Duration,
        max_retransmits: u32,
        oack: Option<Vec<(String, String)>>,
        now: Instant,
    ) -> Self {
        assert!(blksize > 0, "blksize must be positive");
        let last_block = (data.len() / blksize) as u64 + 1;
        let block = if oack.is_some() { 0 } else { 1 };
        Transfer {
            file,
            data,
            blksize,
            timeout,
            max_retransmits,
            oack,
            block,
            last_block,
            retries_left: max_retransmits,
            wait: Wait::Send { at: now },
            status: Status::Running,
        }
    }

    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn blksize(&self) -> usize {
        self.blksize
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn is_finished(&self) -> bool {
        self.status != Status::Running
    }

    /// Bytes the client has acknowledged so far.
    pub fn acked_bytes(&self) -> u64 {
        let acked_blocks = if self.status == Status::Done {
            self.last_block
        } else {
            self.block.saturating_sub(1)
        };
        (acked_blocks * self.blksize as u64).min(self.size())
    }

    /// Next datagram to send, if one is due at `now`.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Vec<u8>> {
        let Wait::Send { at } = self.wait else {
            return None;
        };
        if now < at {
            return None;
        }
        self.wait = Wait::Ack {
            deadline: now + self.timeout,
        };
        Some(self.current_packet())
    }

    /// Feed a datagram received from the client's address.
    pub fn handle_packet(&mut self, datagram: &[u8], now: Instant) {
        if self.is_finished() {
            return;
        }
        match Packet::parse(datagram) {
            Ok(Packet::Ack(n)) if n == wire_block(self.block) => {
                if !matches!(self.wait, Wait::Ack { .. }) {
                    return;
                }
                if self.block == self.last_block {
                    self.finish(Status::Done);
                } else {
                    self.block += 1;
                    self.retries_left = self.max_retransmits;
                    self.wait = Wait::Send { at: now };
                }
            }
            Ok(Packet::Error { code, message }) => {
                self.finish(Status::Failed(TransferError::ClientError {
                    code: code.to_u16(),
                    message,
                }));
            }
            _ => {}
        }
    }

    /// Call when `now >= poll_timeout()`.
    pub fn handle_timeout(&mut self, now: Instant) {
        let Wait::Ack { deadline } = self.wait else {
            return;
        };
        if now < deadline {
            return;
        }
        if self.retries_left > 0 {
            self.retries_left -= 1;
            self.wait = Wait::Send { at: now };
        } else {
            self.finish(Status::Failed(TransferError::Timeout {
                retransmits: self.max_retransmits,
            }));
        }
    }

    /// When the transfer next needs attention; `None` once finished.
    pub fn poll_timeout(&self) -> Option<Instant> {
        match self.wait {
            Wait::Send { at } => Some(at),
            Wait::Ack { deadline } => Some(deadline),
            Wait::Finished => None,
        }
    }

    /// Fail the transfer locally (e.g. the socket broke).
    pub fn abort(&mut self, error: TransferError) {
        if !self.is_finished() {
            self.finish(Status::Failed(error));
        }
    }

    fn finish(&mut self, status: Status) {
        self.status = status;
        self.wait = Wait::Finished;
    }

    fn current_packet(&self) -> Vec<u8> {
        if self.block == 0 {
            let opts = self.oack.clone().unwrap_or_default();
            return Packet::Oack(opts).encode();
        }
        let start = ((self.block - 1) as usize).saturating_mul(self.blksize);
        let start = start.min(self.data.len());
        let end = (start + self.blksize).min(self.data.len());
        encode_data(wire_block(self.block), &self.data[start..end])
    }
}

/// Block number as sent on the wire (wraps 65535 -> 0).
fn wire_block(block: u64) -> u16 {
    (block & 0xffff) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(1);

    fn data(len: usize) -> Bytes {
        Bytes::from((0..len).map(|i| (i % 251) as u8).collect::<Vec<u8>>())
    }

    fn ack(n: u16) -> Vec<u8> {
        Packet::Ack(n).encode()
    }

    /// Drive a transfer to completion with a perfect client; returns the
    /// DATA packets (block, payload length) and the reassembled file.
    fn run(mut t: Transfer, now: Instant) -> (Vec<(u16, usize)>, Vec<u8>) {
        let mut seen = Vec::new();
        let mut file = Vec::new();
        while let Some(pkt) = t.poll_transmit(now) {
            match Packet::parse(&pkt).unwrap() {
                Packet::Data { block, data } => {
                    seen.push((block, data.len()));
                    file.extend_from_slice(&data);
                    t.handle_packet(&ack(block), now);
                }
                Packet::Oack(_) => t.handle_packet(&ack(0), now),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(t.status(), &Status::Done);
        assert_eq!(t.poll_timeout(), None);
        assert_eq!(t.acked_bytes(), t.size());
        (seen, file)
    }

    #[test]
    fn three_and_a_bit_blocks() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), data(1600), 512, T, 3, None, now);
        let (seen, file) = run(t, now);
        assert_eq!(seen, vec![(1, 512), (2, 512), (3, 512), (4, 64)]);
        assert_eq!(file, data(1600).to_vec());
    }

    #[test]
    fn exact_multiple_ends_with_empty_block() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), data(1024), 512, T, 3, None, now);
        let (seen, _) = run(t, now);
        assert_eq!(seen, vec![(1, 512), (2, 512), (3, 0)]);
    }

    #[test]
    fn empty_file_is_one_empty_block() {
        let now = Instant::now();
        let t = Transfer::new("f".into(), Bytes::new(), 512, T, 3, None, now);
        let (seen, _) = run(t, now);
        assert_eq!(seen, vec![(1, 0)]);
    }

    #[test]
    fn oack_first_then_data_after_ack_0() {
        let now = Instant::now();
        let opts = vec![("tsize".to_string(), "10".to_string())];
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, Some(opts), now);
        let first = t.poll_transmit(now).unwrap();
        assert_eq!(first, b"\x00\x06tsize\x0010\x00");
        assert_eq!(t.poll_transmit(now), None, "waits for ACK 0");
        t.handle_packet(&ack(1), now);
        assert_eq!(t.poll_transmit(now), None, "wrong ACK ignored");
        t.handle_packet(&ack(0), now);
        let second = t.poll_transmit(now).unwrap();
        assert_eq!(&second[..4], b"\x00\x03\x00\x01");
        assert_eq!(second.len(), 4 + 10);
    }

    #[test]
    fn block_numbers_wrap_after_65535() {
        let now = Instant::now();
        let len = 8 * 65_537 + 3;
        let t = Transfer::new("f".into(), data(len), 8, T, 3, None, now);
        let (seen, file) = run(t, now);
        assert_eq!(seen.len(), 65_538);
        assert_eq!(seen[65_534], (65_535, 8));
        assert_eq!(seen[65_535], (0, 8));
        assert_eq!(seen[65_536], (1, 8));
        assert_eq!(seen[65_537], (2, 3));
        assert_eq!(file.len(), len);
    }

    #[test]
    fn retransmits_on_timeout_then_fails() {
        let t0 = Instant::now();
        let mut t = Transfer::new("f".into(), data(100), 512, T, 2, None, t0);
        let first = t.poll_transmit(t0).unwrap();
        assert_eq!(t.poll_timeout(), Some(t0 + T));
        t.handle_timeout(t0 + T / 2);
        assert_eq!(t.poll_transmit(t0 + T / 2), None, "not yet");
        let mut now = t0;
        for _ in 0..2 {
            now += T;
            t.handle_timeout(now);
            assert_eq!(t.poll_transmit(now).as_deref(), Some(first.as_slice()));
        }
        now += T;
        t.handle_timeout(now);
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::Timeout { retransmits: 2 })
        );
        assert_eq!(t.poll_transmit(now), None);
        assert_eq!(t.poll_timeout(), None);
    }

    #[test]
    fn retry_budget_resets_per_block() {
        let t0 = Instant::now();
        let mut t = Transfer::new("f".into(), data(600), 512, T, 1, None, t0);
        t.poll_transmit(t0).unwrap();
        t.handle_timeout(t0 + T);
        t.poll_transmit(t0 + T).unwrap();
        t.handle_packet(&ack(1), t0 + T);
        t.poll_transmit(t0 + T).unwrap();
        t.handle_timeout(t0 + 2 * T);
        assert!(
            t.poll_transmit(t0 + 2 * T).is_some(),
            "block 2 may retry once"
        );
        assert_eq!(t.status(), &Status::Running);
    }

    #[test]
    fn duplicate_ack_does_not_trigger_resend() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(1600), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(&ack(1), now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(&ack(1), now);
        assert_eq!(t.poll_transmit(now), None);
        assert_eq!(t.acked_bytes(), 512);
    }

    #[test]
    fn client_error_fails_transfer() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(b"\x00\x05\x00\x08bad option\x00", now);
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::ClientError {
                code: 8,
                message: "bad option".into()
            })
        );
    }

    #[test]
    fn garbage_is_ignored_and_abort_fails() {
        let now = Instant::now();
        let mut t = Transfer::new("f".into(), data(10), 512, T, 3, None, now);
        t.poll_transmit(now).unwrap();
        t.handle_packet(b"\xff", now);
        assert_eq!(t.status(), &Status::Running);
        t.abort(TransferError::Io("gone".into()));
        assert_eq!(
            t.status(),
            &Status::Failed(TransferError::Io("gone".into()))
        );
    }
}
