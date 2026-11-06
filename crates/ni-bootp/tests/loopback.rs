//! BOOTP server over real UDP sockets on 127.0.0.1.

use ni_bootp::{serve, BootArch, BootpConfig, BootpEvent, BootpResponder, IgnoreReason, ReplyKind};
use ni_proto::Mac;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 0x11, 0x22, 0x33]);

/// A RouterBOOT-like request: DHCP if `msg_type` is set, else plain BOOTP.
fn request(msg_type: Option<u8>, vendor_class: &str) -> Vec<u8> {
    let mut b = vec![0u8; 236];
    b[0] = 1;
    b[1] = 1;
    b[2] = 6;
    b[4..8].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
    b[28..34].copy_from_slice(&MAC.0);
    b.extend_from_slice(&[99, 130, 83, 99]);
    if let Some(t) = msg_type {
        b.extend_from_slice(&[53, 1, t]);
    }
    b.push(60);
    b.push(vendor_class.len() as u8);
    b.extend_from_slice(vendor_class.as_bytes());
    b.push(255);
    b.resize(300, 0);
    b
}

struct Fixture {
    client: UdpSocket,
    server_addr: SocketAddr,
    events: mpsc::UnboundedReceiver<BootpEvent>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start() -> Fixture {
    start_with(UdpSocket::bind("127.0.0.1:0").await.unwrap()).await
}

async fn start_with(server: UdpSocket) -> Fixture {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let server_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let responder = BootpResponder::new(BootpConfig {
        server_ip: Ipv4Addr::new(192, 168, 88, 1),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        pool_start: Ipv4Addr::new(192, 168, 88, 100),
        pool_end: Ipv4Addr::new(192, 168, 88, 199),
        boot_files: HashMap::from([(BootArch::Arm64, "arm64-boot".to_string())]),
        lease_secs: 300,
    })
    .unwrap();
    let (tx, events) = mpsc::unbounded_channel();
    let reply_to = client.local_addr().unwrap();
    let task = tokio::spawn(serve(server, responder, reply_to, tx));
    Fixture {
        client,
        server_addr,
        events,
        task,
    }
}

async fn recv(sock: &UdpSocket) -> Vec<u8> {
    let mut buf = vec![0u8; 1500];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("reply within 2 s")
        .unwrap();
    buf.truncate(n);
    buf
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<BootpEvent>) -> BootpEvent {
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("event within 2 s")
        .expect("server alive")
}

#[tokio::test]
async fn discover_then_request_over_udp() {
    let mut f = start().await;

    f.client
        .send_to(&request(Some(1), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let offer = recv(&f.client).await;
    assert_eq!(offer.len(), 300);
    assert_eq!(offer[0], 2);
    assert_eq!(&offer[4..8], &0x1234_5678_u32.to_be_bytes());
    assert_eq!(&offer[16..20], &[192, 168, 88, 100]);
    assert_eq!(&offer[20..24], &[192, 168, 88, 1]);
    assert_eq!(&offer[108..119], b"arm64-boot\0");
    assert_eq!(&offer[240..243], &[53, 1, 2]);
    assert_eq!(
        next_event(&mut f.events).await,
        BootpEvent::Replied {
            mac: MAC,
            arch: BootArch::Arm64,
            ip: Ipv4Addr::new(192, 168, 88, 100),
            kind: ReplyKind::Offer,
        }
    );

    f.client
        .send_to(&request(Some(3), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let ack = recv(&f.client).await;
    assert_eq!(&ack[16..20], &[192, 168, 88, 100]);
    assert_eq!(&ack[240..243], &[53, 1, 5]);
    f.task.abort();
}

#[tokio::test]
async fn plain_bootp_over_udp() {
    let mut f = start().await;
    f.client
        .send_to(&request(None, "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let reply = recv(&f.client).await;
    assert_eq!(
        &reply[236..247],
        &[99, 130, 83, 99, 1, 4, 255, 255, 255, 0, 255]
    );
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Replied {
            kind: ReplyKind::Bootp,
            ..
        }
    ));
    f.task.abort();
}

#[tokio::test]
async fn ignored_and_malformed_requests_produce_events_only() {
    let mut f = start().await;
    f.client
        .send_to(&request(Some(1), "X86_64__boot"), f.server_addr)
        .await
        .unwrap();
    assert_eq!(
        next_event(&mut f.events).await,
        BootpEvent::Ignored {
            mac: MAC,
            reason: IgnoreReason::UnsupportedDevice("X86_64__boot".into()),
        }
    );
    f.client.send_to(b"junk", f.server_addr).await.unwrap();
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Malformed { .. }
    ));
    let mut buf = [0u8; 1500];
    let nothing =
        tokio::time::timeout(Duration::from_millis(200), f.client.recv_from(&mut buf)).await;
    assert!(nothing.is_err(), "no reply expected");
    f.task.abort();
}

/// Production socket shape: `0.0.0.0`, `SO_BROADCAST`, `IP_BOUND_IF` (here lo0).
#[tokio::test]
async fn works_on_interface_bound_wildcard_socket() {
    let opts = ni_net::BindOptions::new(0).interface("lo0").broadcast(true);
    let server = ni_net::bind_udp(&opts).unwrap();
    let mut f = start_with(server).await;
    f.client
        .send_to(&request(Some(1), "ARM64__boot"), f.server_addr)
        .await
        .unwrap();
    let offer = recv(&f.client).await;
    assert_eq!(&offer[240..243], &[53, 1, 2]);
    assert!(matches!(
        next_event(&mut f.events).await,
        BootpEvent::Replied { .. }
    ));
    f.task.abort();
}
