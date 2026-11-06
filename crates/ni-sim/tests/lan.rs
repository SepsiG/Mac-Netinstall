//! The `lan` feature: BOOTP codec helpers and devices on a real socket.
//! Run with `cargo test -p ni-sim --features lan`.
#![cfg(feature = "lan")]

use ni_proto::{message, DeviceInfo, Frame, Mac};
use ni_sim::lan::{bootp_request, parse_boot_reply, BootReply, SimLan};
use ni_sim::{DeviceSim, SimConfig};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const MAC: Mac = Mac([0x48, 0xa9, 0x8a, 1, 2, 3]);
const SERVER: Mac = Mac([2, 0, 0, 0, 0, 1]);

#[test]
fn bootp_request_and_reply_layout() {
    let req = bootp_request(MAC, "ARM64__boot", 7);
    assert_eq!(req.len(), 300);
    assert_eq!((req[0], req[1], req[2]), (1, 1, 6));
    assert_eq!(&req[4..8], &7u32.to_be_bytes());
    assert_eq!(&req[28..34], &MAC.0);
    assert_eq!(&req[236..242], &[99, 130, 83, 99, 60, 11]);
    assert_eq!(&req[242..253], b"ARM64__boot");

    let mut reply = req.clone();
    reply[0] = 2;
    reply[16..20].copy_from_slice(&[192, 168, 88, 10]);
    reply[20..24].copy_from_slice(&[192, 168, 88, 1]);
    reply[108..124].copy_from_slice(b"kernel-arm64.elf");
    assert_eq!(
        parse_boot_reply(&reply, MAC),
        Some(BootReply {
            ip: Ipv4Addr::new(192, 168, 88, 10),
            server: Ipv4Addr::new(192, 168, 88, 1),
            file: "kernel-arm64.elf".into(),
        })
    );
    assert_eq!(parse_boot_reply(&reply, Mac([9; 6])), None, "other MAC");
    assert_eq!(parse_boot_reply(&req, MAC), None, "a request, not a reply");
}

async fn recv_frame(sock: &UdpSocket) -> Frame {
    let mut buf = vec![0u8; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("frame within 2 s")
        .unwrap();
    Frame::decode(&buf[..n]).unwrap()
}

#[tokio::test]
async fn devices_say_hello_and_mute_drops_replies() {
    let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let lan = SimLan::bind().await.unwrap();
    lan.connect(server.local_addr().unwrap()).await;
    let mut cfg = SimConfig::new(MAC, "hEX", "mmips");
    cfg.hello_interval = Duration::from_millis(50);
    lan.add(DeviceSim::new(cfg, Instant::now())).await;
    let hello = recv_frame(&server).await;
    assert!(DeviceInfo::is_hello(&hello));
    assert_eq!(DeviceInfo::parse(&hello).unwrap().model, "hEX");

    // An offer is answered with YACK...
    let offer = Frame::new(SERVER, MAC, 1, 0, message::OFFR.to_vec());
    server
        .send_to(&offer.encode(), lan.netinstall_addr())
        .await
        .unwrap();
    let reply = loop {
        let f = recv_frame(&server).await;
        if !DeviceInfo::is_hello(&f) {
            break f;
        }
    };
    assert_eq!((reply.seq, reply.ack), (1, 1));
    assert_eq!(reply.payload.as_ref(), message::YACK);
    let report = lan.report(MAC).await.unwrap();
    assert_eq!(report.frames_to_device, 1);

    // ...but nothing gets through while muted, and a reset starts over.
    lan.set_mute(MAC, true).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    while tokio::time::timeout(Duration::from_millis(50), recv_frame(&server))
        .await
        .is_ok()
    {}
    lan.reset(MAC).await;
    let f = recv_frame(&server).await;
    assert!(
        DeviceInfo::is_hello(&f),
        "a reset device says hello, even muted"
    );
    assert_eq!(lan.report(MAC).await.unwrap().frames_to_device, 0);
    assert_eq!(lan.report(Mac([9; 6])).await, None);
}
