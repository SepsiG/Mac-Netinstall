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
    now: Instant,
    limit: Duration,
    mut deliver: impl FnMut(Direction, &Frame) -> bool,
) -> PumpResult {
    pump_with(server, sim, now, limit, |dir, f| {
        if deliver(dir, &f) {
            vec![f]
        } else {
            Vec::new()
        }
    })
}

/// Like [`pump`], but `route` returns the frames actually delivered to the
/// receiving side in place of each frame: an empty list drops it, and the
/// list may hold modified or extra (injected) frames.
pub fn pump_with(
    server: &mut ServerSession,
    sim: &mut DeviceSim,
    mut now: Instant,
    limit: Duration,
    mut route: impl FnMut(Direction, Frame) -> Vec<Frame>,
) -> PumpResult {
    let end = now + limit;
    loop {
        let mut progressed = false;
        while let Some(f) = server.poll_transmit(now) {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            for f in route(Direction::ToDevice, wire) {
                sim.handle_frame(&f, now);
            }
        }
        while let Some(f) = sim.poll_transmit() {
            progressed = true;
            let wire = Frame::decode(&f.encode()).expect("decodable");
            for f in route(Direction::ToServer, wire) {
                server.handle_frame(&f, now);
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
