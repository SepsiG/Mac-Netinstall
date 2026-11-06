//! Simulated RouterBOARDs on a real UDP network (feature `lan`).
//!
//! Used by the engine's end-to-end tests over loopback, where nothing can
//! be broadcast: the engine is configured to send BOOTP replies to
//! [`SimLan::bootp_addr`] and Netinstall frames to
//! [`SimLan::netinstall_addr`] instead of `255.255.255.255`. Once the
//! engine runs, [`SimLan::connect`] tells the devices where it listens.
//!
//! - [`SimLan::boot`]: a RouterBOOT-like BOOTP request (retried every
//!   250 ms) until the server answers with a boot file;
//! - [`tftp_get`]: download that boot file;
//! - [`SimLan::add`]: from then on a [`DeviceSim`] for that MAC runs
//!   on the shared Netinstall socket. Every received frame is offered to
//!   every device, as on a broadcast segment.
//!
//! Fault injection: [`SimLan::set_mute`] drops everything a device sends
//! except hellos; [`SimLan::reset`] replaces a device with a fresh one
//! (as if it rebooted into Netinstall mode).

use crate::{DeviceSim, ReceivedFile};
use ni_proto::{AfterInstall, DeviceInfo, Frame, Mac, Offer};
use ni_tftp::{Packet, Request};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// What a BOOTP server told a simulated device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootReply {
    /// `yiaddr`: the address assigned to the device.
    pub ip: Ipv4Addr,
    /// `siaddr`: where to fetch the boot file (TFTP).
    pub server: Ipv4Addr,
    /// Boot file name.
    pub file: String,
}

/// A RouterBOOT-like plain BOOTP request (no DHCP message type).
pub fn bootp_request(mac: Mac, vendor_class: &str, xid: u32) -> Vec<u8> {
    let mut b = vec![0u8; 236];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&xid.to_be_bytes());
    b[28..34].copy_from_slice(&mac.0);
    b.extend_from_slice(&[99, 130, 83, 99, 60, vendor_class.len() as u8]);
    b.extend_from_slice(vendor_class.as_bytes());
    b.push(255);
    b.resize(300, 0);
    b
}

/// Parse a BOOTP reply addressed to `mac` (op 2, matching `chaddr`).
pub fn parse_boot_reply(buf: &[u8], mac: Mac) -> Option<BootReply> {
    if buf.len() < 236 || buf[0] != 2 || buf[28..34] != mac.0 {
        return None;
    }
    let ip = |at: usize| Ipv4Addr::new(buf[at], buf[at + 1], buf[at + 2], buf[at + 3]);
    let file = &buf[108..236];
    let end = file.iter().position(|&b| b == 0).unwrap_or(file.len());
    Some(BootReply {
        ip: ip(16),
        server: ip(20),
        file: String::from_utf8_lossy(&file[..end]).into_owned(),
    })
}

/// Download `file` from the TFTP server at `server` (512-byte blocks, no
/// options), as RouterBOOT does. Errors: an ERROR packet, or no packet
/// for 2 s.
pub async fn tftp_get(server: SocketAddr, file: &str) -> io::Result<Vec<u8>> {
    let sock = UdpSocket::bind("127.0.0.1:0").await?;
    let rrq = Packet::Rrq(Request {
        filename: file.into(),
        mode: "octet".into(),
        options: Vec::new(),
    });
    sock.send_to(&rrq.encode(), server).await?;
    let mut data = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TFTP timeout"))??;
        match Packet::parse(&buf[..n]) {
            Ok(Packet::Data { block, data: d }) => {
                data.extend_from_slice(&d);
                sock.send_to(&Packet::Ack(block).encode(), from).await?;
                if d.len() < 512 {
                    return Ok(data);
                }
            }
            Ok(Packet::Error { code, message }) => {
                return Err(io::Error::other(format!("TFTP error {code:?}: {message}")))
            }
            other => return Err(io::Error::other(format!("unexpected {other:?}"))),
        }
    }
}

/// A device's state, as seen by the test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceReport {
    pub received: Vec<ReceivedFile>,
    /// The offer it accepted.
    pub offer: Option<Offer>,
    /// What `TERM` asked for, once received.
    pub after_install: Option<AfterInstall>,
    pub terminated: bool,
    pub violations: Vec<String>,
    /// Frames addressed to this device since it was added or reset.
    pub frames_to_device: usize,
}

enum Cmd {
    Connect(SocketAddr),
    Add(Box<DeviceSim>),
    Reset(Mac),
    Mute(Mac, bool),
    Report(Mac, oneshot::Sender<Option<DeviceReport>>),
}

struct Device {
    sim: DeviceSim,
    frames: usize,
}

/// Simulated devices on two loopback sockets; see the module docs.
pub struct SimLan {
    bootp: UdpSocket,
    bootp_addr: SocketAddr,
    netinstall_addr: SocketAddr,
    cmds: mpsc::Sender<Cmd>,
    task: JoinHandle<()>,
}

impl Drop for SimLan {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SimLan {
    /// Bind the BOOTP client and Netinstall sockets on `127.0.0.1`.
    pub async fn bind() -> io::Result<SimLan> {
        let bootp = UdpSocket::bind("127.0.0.1:0").await?;
        let netinstall = UdpSocket::bind("127.0.0.1:0").await?;
        let (cmds, rx) = mpsc::channel(16);
        Ok(SimLan {
            bootp_addr: bootp.local_addr()?,
            netinstall_addr: netinstall.local_addr()?,
            bootp,
            cmds,
            task: tokio::spawn(run(netinstall, rx)),
        })
    }

    /// Where the engine must send BOOTP replies.
    pub fn bootp_addr(&self) -> SocketAddr {
        self.bootp_addr
    }

    /// Where the engine must send Netinstall frames.
    pub fn netinstall_addr(&self) -> SocketAddr {
        self.netinstall_addr
    }

    /// Send device frames to the engine's Netinstall socket at `server`
    /// (until then they are dropped).
    pub async fn connect(&self, server: SocketAddr) {
        let _ = self.cmds.send(Cmd::Connect(server)).await;
    }

    /// Send BOOTP requests for `mac` to `server` every 250 ms until a reply
    /// arrives or `timeout` passes (`None`).
    pub async fn boot(
        &self,
        mac: Mac,
        vendor_class: &str,
        server: SocketAddr,
        timeout: Duration,
    ) -> Option<BootReply> {
        let end = tokio::time::Instant::now() + timeout;
        let mut buf = vec![0u8; 1500];
        let mut xid = u32::from_be_bytes([mac.0[2], mac.0[3], mac.0[4], mac.0[5]]);
        loop {
            xid = xid.wrapping_add(1);
            let _ = self
                .bootp
                .send_to(&bootp_request(mac, vendor_class, xid), server)
                .await;
            let wait = (tokio::time::Instant::now() + Duration::from_millis(250)).min(end);
            while let Ok(Ok((n, _))) =
                tokio::time::timeout_at(wait, self.bootp.recv_from(&mut buf)).await
            {
                if let Some(reply) = parse_boot_reply(&buf[..n], mac) {
                    return Some(reply);
                }
            }
            if tokio::time::Instant::now() >= end {
                return None;
            }
        }
    }

    /// Start running `sim` on the Netinstall socket (it says hello at once).
    pub async fn add(&self, sim: DeviceSim) {
        let _ = self.cmds.send(Cmd::Add(Box::new(sim))).await;
    }

    /// Replace the device with a fresh simulator (same configuration), as
    /// if it rebooted into Netinstall mode.
    pub async fn reset(&self, mac: Mac) {
        let _ = self.cmds.send(Cmd::Reset(mac)).await;
    }

    /// While muted, a device's frames are dropped, except hellos. May be
    /// set before the device is added.
    pub async fn set_mute(&self, mac: Mac, muted: bool) {
        let _ = self.cmds.send(Cmd::Mute(mac, muted)).await;
    }

    pub async fn report(&self, mac: Mac) -> Option<DeviceReport> {
        let (tx, rx) = oneshot::channel();
        self.cmds.send(Cmd::Report(mac, tx)).await.ok()?;
        rx.await.ok().flatten()
    }

    /// Wait until the device has been told to reboot (`TERM`).
    pub async fn wait_terminated(&self, mac: Mac, timeout: Duration) -> Option<DeviceReport> {
        let end = tokio::time::Instant::now() + timeout;
        loop {
            let report = self.report(mac).await?;
            if report.terminated {
                return Some(report);
            }
            if tokio::time::Instant::now() >= end {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn run(sock: UdpSocket, mut cmds: mpsc::Receiver<Cmd>) {
    let mut server: Option<SocketAddr> = None;
    let mut devices: BTreeMap<Mac, Device> = BTreeMap::new();
    let mut muted: BTreeSet<Mac> = BTreeSet::new();
    let mut buf = vec![0u8; 2048];
    loop {
        let now = Instant::now();
        for (mac, dev) in devices.iter_mut() {
            dev.sim.handle_timeout(now);
            while let Some(frame) = dev.sim.poll_transmit() {
                if muted.contains(mac) && !DeviceInfo::is_hello(&frame) {
                    continue;
                }
                if let Some(server) = server {
                    let _ = sock.send_to(&frame.encode(), server).await;
                }
            }
        }
        let next = devices
            .values()
            .filter_map(|d| d.sim.poll_timeout())
            .min()
            .unwrap_or(now + Duration::from_secs(1));
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                None => return,
                Some(Cmd::Connect(addr)) => server = Some(addr),
                Some(Cmd::Add(sim)) => {
                    let mac = sim.info().mac;
                    devices.insert(mac, Device { sim: *sim, frames: 0 });
                }
                Some(Cmd::Reset(mac)) => {
                    if let Some(dev) = devices.get_mut(&mac) {
                        dev.sim = DeviceSim::new(dev.sim.config().clone(), Instant::now());
                        dev.frames = 0;
                    }
                }
                Some(Cmd::Mute(mac, on)) => {
                    if on {
                        muted.insert(mac);
                    } else {
                        muted.remove(&mac);
                    }
                }
                Some(Cmd::Report(mac, reply)) => {
                    let report = devices.get(&mac).map(|d| DeviceReport {
                        received: d.sim.received_files().to_vec(),
                        offer: d.sim.offer().cloned(),
                        after_install: d.sim.after_install(),
                        terminated: d.sim.is_terminated(),
                        violations: d.sim.violations().to_vec(),
                        frames_to_device: d.frames,
                    });
                    let _ = reply.send(report);
                }
            },
            received = sock.recv_from(&mut buf) => {
                let Ok((n, _)) = received else { continue };
                let Ok(frame) = Frame::decode(&buf[..n]) else { continue };
                let now = Instant::now();
                for (mac, dev) in devices.iter_mut() {
                    if frame.dst == *mac {
                        dev.frames += 1;
                    }
                    dev.sim.handle_frame(&frame, now);
                }
            }
            _ = tokio::time::sleep_until(next.into()) => {}
        }
    }
}
