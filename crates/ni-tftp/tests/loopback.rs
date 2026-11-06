//! TFTP server over real UDP sockets on lo0, with a minimal client.

use bytes::Bytes;
use ni_net::BindOptions;
use ni_tftp::{
    serve, ErrorCode, FileMap, Packet, Request, TftpConfig, TftpEvent, TransferConfig,
    TransferError,
};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

fn content(len: usize) -> Bytes {
    Bytes::from((0..len).map(|i| (i * 7 % 256) as u8).collect::<Vec<u8>>())
}

fn lo0() -> BindOptions {
    BindOptions::new(0)
        .addr(Ipv4Addr::LOCALHOST)
        .interface("lo0")
}

struct Server {
    addr: SocketAddr,
    events: mpsc::UnboundedReceiver<TftpEvent>,
    task: JoinHandle<std::io::Result<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start(transfer: TransferConfig) -> Server {
    start_on(lo0(), Ipv4Addr::LOCALHOST, Some("lo0"), transfer).await
}

async fn start_on(
    listen: BindOptions,
    server_ip: Ipv4Addr,
    interface: Option<&str>,
    transfer: TransferConfig,
) -> Server {
    let files = FileMap::from([
        ("kernel".to_string(), content(1600)),
        ("exact".to_string(), content(1024)),
        ("empty".to_string(), Bytes::new()),
    ]);
    let listener = ni_net::bind_udp(&listen).unwrap();
    let addr = SocketAddr::from((server_ip, listener.local_addr().unwrap().port()));
    let (tx, events) = mpsc::unbounded_channel();
    let cfg = TftpConfig {
        transfer,
        server_ip,
        interface: interface.map(str::to_string),
    };
    let task = tokio::spawn(serve(listener, Arc::new(files), cfg, tx));
    Server { addr, events, task }
}

fn fast() -> TransferConfig {
    TransferConfig {
        timeout: Duration::from_millis(100),
        max_retransmits: 2,
        ..Default::default()
    }
}

fn rrq(name: &str, options: &[(&str, &str)]) -> Vec<u8> {
    Packet::Rrq(Request {
        filename: name.into(),
        mode: "octet".into(),
        options: options
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
    .encode()
}

async fn recv(sock: &UdpSocket) -> (Packet, SocketAddr) {
    let mut buf = vec![0u8; 70_000];
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("packet within 2 s")
        .unwrap();
    (Packet::parse(&buf[..n]).unwrap(), from)
}

/// What a download saw.
#[derive(Debug)]
struct Download {
    data: Vec<u8>,
    blocks: Vec<(u16, usize)>,
    oack: Option<Vec<(String, String)>>,
    tid: SocketAddr,
}

/// Minimal TFTP client: RRQ, optional OACK -> ACK 0, then ACK each block.
async fn download(
    server: SocketAddr,
    name: &str,
    options: &[(&str, &str)],
) -> Result<Download, (ErrorCode, String)> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq(name, options), server).await.unwrap();
    let mut dl = Download {
        data: Vec::new(),
        blocks: Vec::new(),
        oack: None,
        tid: server,
    };
    loop {
        let (pkt, from) = recv(&sock).await;
        dl.tid = from;
        match pkt {
            Packet::Oack(opts) => {
                dl.oack = Some(opts);
                sock.send_to(&Packet::Ack(0).encode(), from).await.unwrap();
            }
            Packet::Data { block, data } => {
                let last = data.len() < 512;
                dl.blocks.push((block, data.len()));
                dl.data.extend_from_slice(&data);
                sock.send_to(&Packet::Ack(block).encode(), from)
                    .await
                    .unwrap();
                if last {
                    return Ok(dl);
                }
            }
            Packet::Error { code, message } => return Err((code, message)),
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn next_event(s: &mut Server) -> TftpEvent {
    tokio::time::timeout(Duration::from_secs(2), s.events.recv())
        .await
        .expect("event within 2 s")
        .unwrap()
}

#[tokio::test]
async fn downloads_multi_block_file_from_new_tid() {
    let mut s = start(fast()).await;
    let dl = download(s.addr, "kernel", &[]).await.unwrap();
    assert_eq!(dl.data, content(1600).to_vec());
    assert_eq!(dl.blocks, vec![(1, 512), (2, 512), (3, 512), (4, 64)]);
    assert_eq!(dl.oack, None);
    assert_ne!(dl.tid, s.addr, "data comes from a per-transfer port");
    assert_eq!(dl.tid.ip(), s.addr.ip(), "and from the server IP");
    let peer = match next_event(&mut s).await {
        TftpEvent::Started { peer, file, size } => {
            assert_eq!((file.as_str(), size), ("kernel", 1600));
            peer
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(
        next_event(&mut s).await,
        TftpEvent::Completed {
            peer,
            file: "kernel".into(),
            size: 1600
        }
    );
}

#[tokio::test]
async fn exact_multiple_of_512_ends_with_empty_block() {
    let s = start(fast()).await;
    let dl = download(s.addr, "exact", &[]).await.unwrap();
    assert_eq!(dl.blocks, vec![(1, 512), (2, 512), (3, 0)]);
    assert_eq!(dl.data, content(1024).to_vec());
}

#[tokio::test]
async fn empty_file_is_one_empty_block() {
    let s = start(fast()).await;
    let dl = download(s.addr, "/empty", &[]).await.unwrap();
    assert_eq!(dl.blocks, vec![(1, 0)]);
}

#[tokio::test]
async fn missing_file_is_error_1() {
    let mut s = start(fast()).await;
    let err = download(s.addr, "nope", &[]).await.unwrap_err();
    assert_eq!(err, (ErrorCode::FileNotFound, "file not found".into()));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Rejected {
            code: ErrorCode::FileNotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn traversal_is_error_2() {
    let s = start(fast()).await;
    let err = download(s.addr, "../kernel", &[]).await.unwrap_err();
    assert_eq!(err.0, ErrorCode::AccessViolation);
}

#[tokio::test]
async fn write_request_is_error_4() {
    let s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let wrq = Packet::Wrq(Request {
        filename: "kernel".into(),
        mode: "octet".into(),
        options: vec![],
    });
    sock.send_to(&wrq.encode(), s.addr).await.unwrap();
    let (pkt, _) = recv(&sock).await;
    assert_eq!(
        pkt,
        Packet::Error {
            code: ErrorCode::IllegalOperation,
            message: "illegal operation".into()
        }
    );
}

#[tokio::test]
async fn tsize_gets_oack_and_blksize_is_ignored() {
    let s = start(fast()).await;
    let dl = download(s.addr, "kernel", &[("tsize", "0"), ("blksize", "1468")])
        .await
        .unwrap();
    assert_eq!(dl.oack, Some(vec![("tsize".into(), "1600".into())]));
    assert_eq!(dl.blocks.len(), 4, "still 512-byte blocks");
    assert_eq!(dl.data, content(1600).to_vec());
}

#[tokio::test]
async fn lost_ack_is_recovered_by_retransmit() {
    let s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (first, tid) = recv(&sock).await;
    // Drop our ACK for block 1: the server must resend block 1.
    let (again, _) = recv(&sock).await;
    assert_eq!(first, again);
    let mut data = Vec::new();
    let mut pkt = again;
    loop {
        let Packet::Data { block, data: chunk } = pkt else {
            panic!("{pkt:?}")
        };
        data.extend_from_slice(&chunk);
        sock.send_to(&Packet::Ack(block).encode(), tid)
            .await
            .unwrap();
        if chunk.len() < 512 {
            break;
        }
        pkt = recv(&sock).await.0;
    }
    assert_eq!(data, content(1600).to_vec());
}

#[tokio::test]
async fn silent_client_fails_with_timeout() {
    let mut s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { .. }
    ));
    match next_event(&mut s).await {
        TftpEvent::Failed { error, .. } => {
            assert_eq!(error, TransferError::Timeout { retransmits: 2 })
        }
        other => panic!("{other:?}"),
    }
    // 1 original + 2 retransmits reached the client.
    for _ in 0..3 {
        assert!(matches!(recv(&sock).await.0, Packet::Data { block: 1, .. }));
    }
}

#[tokio::test]
async fn repeated_rrq_does_not_start_a_second_transfer() {
    let mut s = start(fast()).await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (_, tid) = recv(&sock).await;
    sock.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    // Complete the one transfer.
    let mut block = 1u16;
    loop {
        sock.send_to(&Packet::Ack(block).encode(), tid)
            .await
            .unwrap();
        let (pkt, from) = recv(&sock).await;
        assert_eq!(from, tid, "everything comes from the first transfer");
        let Packet::Data { block: b, data } = pkt else {
            panic!()
        };
        if b == block {
            continue; // retransmit raced with our ACK
        }
        block = b;
        if data.len() < 512 {
            sock.send_to(&Packet::Ack(block).encode(), tid)
                .await
                .unwrap();
            break;
        }
    }
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Completed { .. }
    ));
}

#[tokio::test]
async fn foreign_packet_on_transfer_port_gets_error_5() {
    let s = start(fast()).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (_, tid) = recv(&client).await;
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    stranger
        .send_to(&Packet::Ack(1).encode(), tid)
        .await
        .unwrap();
    let (pkt, _) = recv(&stranger).await;
    assert!(matches!(
        pkt,
        Packet::Error {
            code: ErrorCode::UnknownTransferId,
            ..
        }
    ));
}

#[tokio::test]
async fn concurrent_downloads() {
    let s = start(fast()).await;
    let (a, b, c) = tokio::join!(
        download(s.addr, "kernel", &[]),
        download(s.addr, "exact", &[]),
        download(s.addr, "kernel", &[("tsize", "0")]),
    );
    assert_eq!(a.unwrap().data, content(1600).to_vec());
    assert_eq!(b.unwrap().data, content(1024).to_vec());
    assert_eq!(c.unwrap().data, content(1600).to_vec());
}

#[tokio::test]
async fn foreign_error_packet_on_transfer_port_gets_no_reply() {
    let mut s = start(fast()).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (_, tid) = recv(&client).await;
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let error = Packet::Error {
        code: ErrorCode::NotDefined,
        message: "go away".into(),
    };
    stranger.send_to(&error.encode(), tid).await.unwrap();
    let mut buf = [0u8; 1500];
    let nothing =
        tokio::time::timeout(Duration::from_millis(300), stranger.recv_from(&mut buf)).await;
    assert!(nothing.is_err(), "never answer an ERROR with an ERROR");
    // The stranger's ERROR does not disturb the real transfer.
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { .. }
    ));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Failed {
            error: TransferError::Timeout { .. },
            ..
        }
    ));
}

/// DATA must come from the IP the client sent its RRQ to. On loopback
/// everything is 127.0.0.1, so this uses the Mac's own non-loopback IPv4
/// address (traffic to it still stays inside the host): a transfer socket
/// bound to `0.0.0.0` would answer a 127.0.0.1 client from 127.0.0.1.
#[tokio::test]
async fn data_source_ip_is_the_server_ip() {
    let Some(server_ip) = ni_net::list_interfaces()
        .unwrap()
        .into_iter()
        .filter(|i| i.is_up && !i.is_loopback)
        .flat_map(|i| i.ipv4)
        .map(|(ip, _)| ip)
        .find(|ip| !ip.is_link_local())
    else {
        eprintln!("skipped: no non-loopback IPv4 address on this machine");
        return;
    };
    let s = start_on(BindOptions::new(0).addr(server_ip), server_ip, None, fast()).await;
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.send_to(&rrq("kernel", &[]), s.addr).await.unwrap();
    let (pkt, tid) = recv(&client).await;
    assert!(matches!(pkt, Packet::Data { block: 1, .. }), "{pkt:?}");
    assert_eq!(tid.ip(), std::net::IpAddr::V4(server_ip));
    assert_ne!(tid.port(), s.addr.port());
}
