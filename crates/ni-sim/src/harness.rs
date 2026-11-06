//! In-memory driver connecting a [`ServerSession`] and a [`DeviceSim`]
//! with a virtual clock. Frames pass through encode/decode on the way.

use crate::DeviceSim;
use ni_proto::{DeviceInfo, Frame, ServerSession};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ToDevice,
    ToServer,
}

#[derive(Debug, Clone, Copy)]
pub struct PumpResult {
    /// Virtual time when pumping stopped.
    pub now: Instant,
}

/// Fire the simulator's first hello and parse it.
pub fn discover(sim: &mut DeviceSim, now: Instant) -> DeviceInfo {
    sim.handle_timeout(now);
    let hello = sim.poll_transmit().expect("simulator emits hello");
    DeviceInfo::parse(&hello).expect("valid hello payload")
}

/// Run until the server session finishes or `limit` virtual time passes.
/// `deliver` decides whether each frame reaches the other side.
pub fn pump(
    server: &mut ServerSession,
    sim: &mut DeviceSim,
    mut now: Instant,
    limit: Duration,
    mut deliver: impl FnMut(Direction, &Frame) -> bool,
) -> PumpResult {
    let end = now + limit;
    loop {
        let mut progressed = false;
        while let Some(f) = server.poll_transmit(now) {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            if deliver(Direction::ToDevice, &wire) {
                sim.handle_frame(&wire, now);
            }
        }
        while let Some(f) = sim.poll_transmit() {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            if deliver(Direction::ToServer, &wire) {
                server.handle_frame(&wire, now);
            }
        }
        if server.is_finished() {
            return PumpResult { now };
        }
        if progressed {
            continue;
        }
        let next = [server.poll_timeout(), sim.poll_timeout()]
            .into_iter()
            .flatten()
            .min();
        match next {
            Some(t) if t <= end => {
                now = now.max(t);
                server.handle_timeout(now);
                sim.handle_timeout(now);
            }
            _ => return PumpResult { now },
        }
    }
}
