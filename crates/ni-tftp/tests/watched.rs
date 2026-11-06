//! `serve_watched`: replaceable file map, progress events, bounded channel.

use bytes::Bytes;
use ni_net::BindOptions;
use ni_tftp::{
    serve_watched, ErrorCode, FileMap, Packet, Request, TftpConfig, TftpEvent, TransferConfig,
};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

fn content(len: usize) -> Bytes {
    Bytes::from((0..len).map(|i| (i * 7 % 256) as u8).collect::<Vec<u8>>())
}

struct Server {
    addr: SocketAddr,
    files: watch::Sender<Arc<FileMap>>,
    events: mpsc::Receiver<TftpEvent>,
    task: JoinHandle<std::io::Result<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn start(files: FileMap, progress_step: u64, capacity: usize) -> Server {
    let opts = BindOptions::new(0)
        .addr(Ipv4Addr::LOCALHOST)
        .interface("lo0");
    let listener = ni_net::bind_udp(&opts).unwrap();
    let addr = listener.local_addr().unwrap();
    let (files, rx) = watch::channel(Arc::new(files));
    let (tx, events) = mpsc::channel(capacity);
    let cfg = TftpConfig {
        transfer: TransferConfig {
            timeout: Duration::from_millis(200),
            ..Default::default()
        },
        server_ip: Ipv4Addr::LOCALHOST,
        interface: Some("lo0".into()),
    };
    let task = tokio::spawn(serve_watched(listener, rx, cfg, progress_step, tx));
    Server {
        addr,
        files,
        events,
        task,
    }
}

/// Download `name` (512-byte blocks); `Err` carries a TFTP error code.
async fn download(server: SocketAddr, name: &str) -> Result<Vec<u8>, ErrorCode> {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let rrq = Packet::Rrq(Request {
        filename: name.into(),
        mode: "octet".into(),
        options: Vec::new(),
    });
    sock.send_to(&rrq.encode(), server).await.unwrap();
    let mut data = Vec::new();
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
            .await
            .expect("packet within 2 s")
            .unwrap();
        match Packet::parse(&buf[..n]).unwrap() {
            Packet::Data { block, data: d } => {
                data.extend_from_slice(&d);
                sock.send_to(&Packet::Ack(block).encode(), from)
                    .await
                    .unwrap();
                if d.len() < 512 {
                    return Ok(data);
                }
            }
            Packet::Error { code, .. } => return Err(code),
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
async fn files_published_later_are_served() {
    let mut s = start(FileMap::new(), 0, 16);
    assert_eq!(
        download(s.addr, "kernel-arm64.elf").await,
        Err(ErrorCode::FileNotFound)
    );
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Rejected {
            code: ErrorCode::FileNotFound,
            ..
        }
    ));
    s.files.send_replace(Arc::new(FileMap::from([(
        "kernel-arm64.elf".to_string(),
        content(700),
    )])));
    assert_eq!(
        download(s.addr, "kernel-arm64.elf").await.unwrap(),
        content(700).to_vec()
    );
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Started { size: 700, .. }
    ));
    assert!(matches!(
        next_event(&mut s).await,
        TftpEvent::Completed { size: 700, .. }
    ));
}

#[tokio::test]
async fn progress_is_reported_every_step_but_not_for_the_last_block() {
    let mut s = start(
        FileMap::from([("k".to_string(), content(5 * 512 + 100))]),
        1024,
        64,
    );
    download(s.addr, "k").await.unwrap();
    let mut seen = Vec::new();
    loop {
        match next_event(&mut s).await {
            TftpEvent::Started { .. } => {}
            TftpEvent::Progress { acked, size, .. } => {
                assert_eq!(size, 2660);
                seen.push(acked);
            }
            TftpEvent::Completed { size, .. } => {
                assert_eq!(size, 2660);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, vec![1024, 2048]);
}

#[tokio::test]
async fn a_full_event_channel_does_not_stall_transfers() {
    let s = start(FileMap::from([("k".to_string(), content(4000))]), 512, 1);
    for _ in 0..3 {
        assert_eq!(download(s.addr, "k").await.unwrap(), content(4000).to_vec());
    }
}
