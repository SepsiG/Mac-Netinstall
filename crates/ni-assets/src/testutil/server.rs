//! A tiny HTTP/1.1 server for download tests, and temp directories.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How the test server answers one path.
#[derive(Debug, Clone)]
pub enum Reply {
    /// 200 with this body (206 for `Range: bytes=-N` requests).
    Body(Vec<u8>),
    /// Any status with an empty body.
    Status(u16),
    /// Declares `declared` bytes but sends only `body`, then closes.
    Truncated { body: Vec<u8>, declared: usize },
    /// Sends the response head, then nothing.
    Stall,
}

type Routes = Arc<Mutex<HashMap<String, Reply>>>;

/// A minimal HTTP/1.1 server on 127.0.0.1 (one request per connection).
/// Unknown paths get 404. Every request head is recorded.
pub struct TestServer {
    pub base: String,
    routes: Routes,
    log: Arc<Mutex<Vec<String>>>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (r, l) = (routes.clone(), log.clone());
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(handle(stream, r.clone(), l.clone()));
            }
        });
        Self { base, routes, log }
    }

    pub fn route(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.to_string(), reply);
    }

    /// Request heads received so far.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Request lines (`GET /path`) received so far.
    pub fn paths(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| {
                r.lines()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(" HTTP/1.1")
                    .to_string()
            })
            .collect()
    }
}

async fn handle(mut stream: TcpStream, routes: Routes, log: Arc<Mutex<Vec<String>>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > 16 * 1024 {
            return;
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    log.lock().unwrap().push(head.clone());
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    let range = head.lines().find_map(|l| {
        let l = l.to_ascii_lowercase();
        let v = l.strip_prefix("range: bytes=-")?;
        v.trim().parse::<usize>().ok()
    });
    let reply = routes.lock().unwrap().get(&path).cloned();
    let (status, extra, body, declared) = match reply {
        None => (404, String::new(), Vec::new(), 0),
        Some(Reply::Status(code)) => (code, String::new(), Vec::new(), 0),
        Some(Reply::Body(body)) => match range {
            Some(n) if n < body.len() => {
                let start = body.len() - n;
                let extra = format!(
                    "Content-Range: bytes {start}-{}/{}\r\n",
                    body.len() - 1,
                    body.len()
                );
                let part = body[start..].to_vec();
                let len = part.len();
                (206, extra, part, len)
            }
            _ => {
                let len = body.len();
                (200, String::new(), body, len)
            }
        },
        Some(Reply::Truncated { body, declared }) => (200, String::new(), body, declared),
        Some(Reply::Stall) => {
            let head = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n";
            let _ = stream.write_all(head.as_bytes()).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
    };
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {declared}\r\n{extra}Connection: close\r\n\r\n"
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

