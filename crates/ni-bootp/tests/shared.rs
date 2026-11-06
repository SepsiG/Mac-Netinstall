//! `serve_shared`: boot files, MAC policy and leases changed while running.

use ni_bootp::{
    serve_shared, BootArch, BootpConfig, BootpEvent, BootpHandle, BootpResponder, IgnoreReason,
    ReplyKind,
};
use ni_proto::Mac;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0x11, 0x22, 0x33]);

/// A plain BOOTP request with vendor class `ARM64__boot`.
fn request() -> Vec<u8> {
    let mut b = vec![0u8; 236];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
    b[28..34].copy_from_slice(&MAC.0);
    b.extend_from_slice(&[99, 130, 83, 99, 60, 11]);
    b.extend_from_slice(b"ARM64__boot");
    b.push(255);
    b.resize(300, 0);
    b
}

async fn next_event(events: &mut mpsc::Receiver<BootpEvent>) -> BootpEvent {
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event within 2 s")
        .expect("server alive")
}

#[tokio::test]
async fn boot_files_policy_and_leases_change_while_running() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr().unwrap().port()));
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let handle = BootpHandle::new(
        BootpResponder::new(BootpConfig {
            server_ip: Ipv4Addr::new(192, 168, 88, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            pool_start: Ipv4Addr::new(192, 168, 88, 100),
            pool_end: Ipv4Addr::new(192, 168, 88, 199),
            boot_files: HashMap::new(),
            lease_secs: 300,
        })
        .unwrap(),
    );
    let (tx, mut events) = mpsc::channel(16);
    let task = tokio::spawn(serve_shared(
        server,
        handle.clone(),
        client.local_addr().unwrap(),
        tx,
    ));

    // No kernels yet: ignored.
    client.send_to(&request(), server_addr).await.unwrap();
    assert_eq!(
        next_event(&mut events).await,
        BootpEvent::Ignored {
            mac: MAC,
            reason: IgnoreReason::NoBootFile(BootArch::Arm64)
        }
    );

    // A policy that refuses everyone.
    let open = Arc::new(AtomicBool::new(false));
    let gate = open.clone();
    handle.set_policy(Some(Arc::new(move |_| gate.load(Ordering::SeqCst))));
    handle
        .set_boot_files(HashMap::from([(
            BootArch::Arm64,
            "kernel-arm64.elf".to_string(),
        )]))
        .unwrap();
    client.send_to(&request(), server_addr).await.unwrap();
    assert_eq!(
        next_event(&mut events).await,
        BootpEvent::Ignored {
            mac: MAC,
            reason: IgnoreReason::Denied(BootArch::Arm64)
        }
    );
    assert_eq!(handle.lease_of(&MAC), None);

    // Open the gate: served, and the lease maps back to the MAC.
    open.store(true, Ordering::SeqCst);
    client.send_to(&request(), server_addr).await.unwrap();
    let ip = Ipv4Addr::new(192, 168, 88, 100);
    assert_eq!(
        next_event(&mut events).await,
        BootpEvent::Replied {
            mac: MAC,
            arch: BootArch::Arm64,
            ip,
            kind: ReplyKind::Bootp
        }
    );
    let mut buf = [0u8; 1500];
    let (n, _) = client.recv_from(&mut buf).await.unwrap();
    assert!(n >= 300);
    assert_eq!(&buf[108..124], b"kernel-arm64.elf");
    assert_eq!(handle.lease_of(&MAC), Some(ip));
    assert_eq!(handle.mac_of(ip), Some(MAC));
    task.abort();
}

#[tokio::test]
async fn a_full_event_channel_drops_events_instead_of_blocking() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, server.local_addr().unwrap().port()));
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let handle = BootpHandle::new(
        BootpResponder::new(BootpConfig {
            server_ip: Ipv4Addr::new(192, 168, 88, 1),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            pool_start: Ipv4Addr::new(192, 168, 88, 100),
            pool_end: Ipv4Addr::new(192, 168, 88, 199),
            boot_files: HashMap::from([(BootArch::Arm64, "k".to_string())]),
            lease_secs: 300,
        })
        .unwrap(),
    );
    let (tx, mut events) = mpsc::channel(1);
    let task = tokio::spawn(serve_shared(
        server,
        handle.clone(),
        client.local_addr().unwrap(),
        tx,
    ));
    // Nobody reads events: the server must keep answering anyway.
    let mut buf = [0u8; 1500];
    for _ in 0..5 {
        client.send_to(&request(), server_addr).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("reply although the event channel is full")
            .unwrap();
    }
    assert!(matches!(events.try_recv(), Ok(BootpEvent::Replied { .. })));
    assert!(events.try_recv().is_err(), "the other events were dropped");
    task.abort();
}
