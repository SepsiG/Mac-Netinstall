//! Simulated MikroTik device in Netinstall mode (sans-IO).
//!
//! Behaviour mirrors what pyNetinstall expects from real hardware:
//! broadcasts hello frames until offered, answers each server frame with
//! `seq = ack = <server seq>`, and periodically re-sends its last reply
//! (real devices do this too).

pub mod harness;

use bytes::Bytes;
use ni_proto::{message, DeviceInfo, Frame, Mac};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub info: DeviceInfo,
    pub hello_interval: Duration,
    pub resend_interval: Duration,
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
    Discovering,
    Offered,
    Formatted,
    Ready,
    Receiving {
        name: String,
        size: u64,
        data: Vec<u8>,
    },
    Finished,
    Terminated,
}

pub struct DeviceSim {
    cfg: SimConfig,
    state: State,
    last_seq: u16,
    last_reply: Option<Frame>,
    tx: VecDeque<Frame>,
    next_timer: Option<Instant>,
    received: Vec<ReceivedFile>,
}

impl DeviceSim {
    pub fn new(cfg: SimConfig, now: Instant) -> Self {
        Self {
            cfg,
            state: State::Discovering,
            last_seq: 0,
            last_reply: None,
            tx: VecDeque::new(),
            next_timer: Some(now),
            received: Vec::new(),
        }
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.cfg.info
    }

    pub fn received_files(&self) -> &[ReceivedFile] {
        &self.received
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
            _ => {
                if let Some(reply) = &self.last_reply {
                    self.tx.push_back(reply.clone());
                }
                self.next_timer = Some(now + self.cfg.resend_interval);
            }
        }
    }

    pub fn handle_frame(&mut self, frame: &Frame, now: Instant) {
        let me = self.cfg.info.mac;
        if self.state == State::Terminated || frame.dst != me || frame.src == me {
            return;
        }
        if self.last_seq != 0 && frame.seq == self.last_seq {
            // Server re-sent its last frame: repeat our answer.
            if let Some(reply) = &self.last_reply {
                self.tx.push_back(reply.clone());
            }
            return;
        }
        if frame.seq != self.last_seq.wrapping_add(1) {
            return;
        }
        self.last_seq = frame.seq;
        for payload in self.process(&frame.payload) {
            let reply = Frame::new(
                me,
                frame.src,
                frame.seq,
                frame.seq,
                Bytes::from_static(payload),
            );
            self.last_reply = Some(reply.clone());
            self.tx.push_back(reply);
        }
        self.next_timer = if self.state == State::Terminated {
            None
        } else {
            Some(now + self.cfg.resend_interval)
        };
    }

    fn process(&mut self, payload: &[u8]) -> Vec<&'static [u8]> {
        use message::{NACK, RETR, STRT, WTRM, YACK};
        let state = std::mem::replace(&mut self.state, State::Discovering);
        let (next, replies) = match state {
            State::Discovering if payload.starts_with(b"OFFR") => (State::Offered, vec![YACK]),
            State::Offered if payload.is_empty() => (State::Formatted, vec![STRT]),
            State::Formatted if payload.is_empty() => (State::Ready, vec![RETR]),
            State::Ready if payload == message::FILE_END => (State::Finished, vec![WTRM]),
            State::Ready if payload.is_empty() => (State::Ready, vec![RETR]),
            State::Ready => match message::parse_file_header(payload) {
                Some((name, size)) => (
                    State::Receiving {
                        name,
                        size,
                        data: Vec::new(),
                    },
                    vec![RETR],
                ),
                None => (State::Ready, vec![NACK]),
            },
            State::Receiving {
                name,
                size,
                mut data,
            } => {
                data.extend_from_slice(payload);
                if data.len() as u64 >= size {
                    self.received.push(ReceivedFile { name, data });
                    // pyNetinstall reads twice after the last chunk.
                    (State::Ready, vec![RETR, RETR])
                } else {
                    (State::Receiving { name, size, data }, vec![RETR])
                }
            }
            State::Finished if payload.starts_with(b"TERM") => (State::Terminated, vec![]),
            other => (other, vec![NACK]),
        };
        self.state = next;
        replies
    }
}
