//! `start`: bind the three listeners, spawn the runners and the kernel
//! download.

use super::core::{Internal, Io, Running};
use crate::config::{resolve_network, EngineConfig};
use crate::event::{Failure, StartInfo};
use crate::gate::Gate;
use crate::preflight::bind_failure;
use crate::prepare::kernels;
use ni_assets::{Assets, Channel, VersionSpec};
use ni_bootp::{serve_shared, BootpConfig, BootpHandle, BootpResponder};
use ni_net::{bind_udp, list_interfaces, BindOptions};
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
    assets: Arc<Assets>,
    internal: mpsc::Sender<Internal>,
    epoch: u64,
) -> Result<(Running, Io), Failure> {
    if let Some(p) = &cfg.profile {
        p.validate()?;
    }
    let ifaces =
        list_interfaces().map_err(|e| Failure::new("interface_list_failed").with("detail", e))?;
    let net = resolve_network(&ifaces, &cfg)?;
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
    let public = |s: &UdpSocket| -> Result<SocketAddr, Failure> {
        let a = s
            .local_addr()
            .map_err(|e| Failure::new("bind_failed").with("detail", e))?;
        Ok(if a.ip().is_unspecified() {
            SocketAddr::from((net.server_ip, a.port()))
        } else {
            a
        })
    };
    let info = StartInfo {
        interface: cfg.interface.clone(),
        server_ip: net.server_ip,
        bootp: public(&bootp_sock)?,
        tftp: public(&tftp_sock)?,
        netinstall: public(&socket)?,
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
    let mut tasks = vec![
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
    let spec = cfg
        .profile
        .as_ref()
        .map(|p| p.version.clone())
        .unwrap_or(VersionSpec::Channel(Channel::Stable));
    let kernel_assets = assets.clone();
    let kernel_tx = internal.clone();
    tasks.push(tokio::spawn(async move {
        let result = kernels(&kernel_assets, &spec).await;
        let _ = kernel_tx.send(Internal::Kernels { epoch, result }).await;
    }));
    let next_link_check = cfg.link_check.map(|d| Instant::now() + d);
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
        queue_seq: 0,
        ignored: HashMap::new(),
        next_link_check,
    };
    let io = Io {
        socket,
        bootp_rx,
        tftp_rx,
        recv_errors: Default::default(),
    };
    Ok((running, io))
}
