//! `start`: bind the three listeners and spawn the runners (the engine
//! loop starts the kernel download).
//!
//! # Starting without link
//!
//! With a configured server IP the engine starts even while the interface
//! has no carrier (and so, on macOS, no manual IPv4 address). That works
//! without rebinding later because every listener binds the wildcard
//! address with `IP_BOUND_IF` (an interface *index*), which needs no
//! address on the interface: binding succeeds, sends fail with
//! `ENETUNREACH` until the link (and with it the address) is back, and
//! then the same sockets work. Verified on macOS 27 with an Ethernet port
//! without IPv4: wildcard bind + `IP_BOUND_IF` succeeds, `sendto` (unicast
//! or broadcast) fails with `ENETUNREACH`, binding the missing address
//! itself fails with `EADDRNOTAVAIL`. So only a listener address other
//! than `0.0.0.0` (`Ports::bind_ip`, used by tests) needs the address at
//! start, and TFTP transfer sockets (bound to `server_ip:0`) cannot be
//! made while it is missing — but no device can reach us then anyway.
//! If the interface comes back under a different index (a re-attached
//! adapter normally keeps its index), the sockets are useless and the
//! watchdog stops the engine with `interface_gone`.

use super::core::{Internal, Io, Link, Running};
use crate::config::{link_status, resolve_network, EngineConfig, LinkStatus};
use crate::event::{Failure, KernelState, StartInfo};
use crate::gate::Gate;
use crate::preflight::{bind_failure, not_ethernet};
use ni_bootp::{serve_shared, BootpConfig, BootpHandle, BootpResponder};
use ni_net::{bind_udp, BindOptions};
use ni_tftp::{serve_watched, FileMap, TftpConfig};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Capacity of each runner's event channel (events beyond are dropped).
pub(crate) const RUNNER_EVENTS: usize = 256;
/// TFTP progress granularity.
pub(crate) const TFTP_PROGRESS_STEP: u64 = 64 * 1024;

/// Run a server future; report its end (it only ends on a fatal error).
fn spawn_runner(
    service: &'static str,
    server: impl Future<Output = io::Result<()>> + Send + 'static,
    internal: mpsc::Sender<Internal>,
    epoch: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let detail = match server.await {
            Ok(()) => "stopped".to_string(),
            Err(e) => e.to_string(),
        };
        let _ = internal
            .send(Internal::RunnerExited {
                epoch,
                service,
                detail,
            })
            .await;
    })
}

pub(crate) fn start(
    cfg: EngineConfig,
    internal: mpsc::Sender<Internal>,
    epoch: u64,
) -> Result<(Running, Io), Failure> {
    cfg.validate()?;
    let ifaces = cfg
        .interfaces
        .list()
        .map_err(|e| Failure::new("interface_list_failed").with("detail", e))?;
    // Defense in depth: the UI lists only capable interfaces.
    if let Some(f) = ifaces
        .iter()
        .find(|i| i.name == cfg.interface)
        .and_then(not_ethernet)
    {
        return Err(f);
    }
    let net = resolve_network(&ifaces, &cfg)?;
    let link = match link_status(&ifaces, &cfg.interface, net.server_ip) {
        LinkStatus::Up => Link::Up,
        LinkStatus::Down(reason) => Link::Down(reason),
        LinkStatus::Missing => Link::Missing(Instant::now()),
    };
    let link_index = ifaces
        .iter()
        .find(|i| i.name == cfg.interface)
        .map_or(0, |i| i.index);
    let responder = BootpResponder::new(BootpConfig {
        server_ip: net.server_ip,
        netmask: net.netmask,
        pool_start: net.pool_start,
        pool_end: net.pool_end,
        boot_files: HashMap::new(),
        lease_secs: cfg.lease_secs,
    })
    .map_err(|e| Failure::new("invalid_network").with("detail", e))?;
    let bind = |service: &str, port: u16, broadcast: bool| {
        let opts = BindOptions::new(port)
            .addr(cfg.ports.bind_ip)
            .interface(cfg.interface.clone())
            .broadcast(broadcast);
        bind_udp(&opts).map_err(|e| bind_failure(service, &e))
    };
    let bootp_sock = bind("bootp", cfg.ports.bootp, true)?;
    let tftp_sock = bind("tftp", cfg.ports.tftp, false)?;
    let socket = bind("netinstall", cfg.ports.netinstall, true)?;
    let public = |service: &str, s: &UdpSocket| -> Result<SocketAddr, Failure> {
        let a = s.local_addr().map_err(|e| {
            Failure::new("bind_failed")
                .with("service", service)
                .with("detail", e)
        })?;
        Ok(if a.ip().is_unspecified() {
            SocketAddr::from((net.server_ip, a.port()))
        } else {
            a
        })
    };
    let info = StartInfo {
        interface: cfg.interface.clone(),
        server_ip: net.server_ip,
        bootp: public("bootp", &bootp_sock)?,
        tftp: public("tftp", &tftp_sock)?,
        netinstall: public("netinstall", &socket)?,
    };

    let gate = Gate::default();
    gate.set_only(cfg.only_mac);
    let bootp = BootpHandle::new(responder);
    bootp.set_policy(Some(gate.policy()));
    let (bootp_tx, bootp_rx) = mpsc::channel(RUNNER_EVENTS);
    let (tftp_tx, tftp_rx) = mpsc::channel(RUNNER_EVENTS);
    let (files_tx, files_rx) = watch::channel(Arc::new(FileMap::new()));
    let tftp_cfg = TftpConfig {
        transfer: cfg.tftp.clone(),
        server_ip: net.server_ip,
        interface: Some(cfg.interface.clone()),
    };
    let tasks = vec![
        spawn_runner(
            "bootp",
            serve_shared(
                bootp_sock,
                bootp.clone(),
                cfg.ports.bootp_reply_to,
                bootp_tx,
            ),
            internal.clone(),
            epoch,
        ),
        spawn_runner(
            "tftp",
            serve_watched(tftp_sock, files_rx, tftp_cfg, TFTP_PROGRESS_STEP, tftp_tx),
            internal.clone(),
            epoch,
        ),
    ];
    let next_link_check = cfg.link_check.and_then(|d| Instant::now().checked_add(d));
    let running = Running {
        cfg,
        net,
        info,
        bootp,
        tftp_files: files_tx,
        tasks,
        gate,
        devices: BTreeMap::new(),
        kernel_version: None,
        kernel_spec: None,
        kernel_gen: 0,
        kernel_resolved: None,
        kernels: KernelState::Idle,
        queue_seq: 0,
        ignored: HashMap::new(),
        next_link_check,
        link,
        link_index,
        foreign_ip_checks: 0,
        send_errors: Default::default(),
        next_prune: Instant::now(),
    };
    let io = Io {
        socket,
        bootp_rx,
        tftp_rx,
        recv_errors: Default::default(),
    };
    Ok((running, io))
}
