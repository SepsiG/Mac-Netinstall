//! HTTP access: small text files, ranged tails and streaming downloads.
//!
//! Redirects are followed (at most 10) only to `https` URLs, or to plain
//! `http` on the same host and port as the URL that redirected; an
//! `https` → `http` downgrade is refused (`redirect_refused`). A
//! user-configured plain-`http` mirror has no transport integrity: only the
//! published checksums (from 7.15 on) and the NPK checks protect the files.

use crate::error::AssetError;
use crate::events::{AssetEvent, PROGRESS_STEP};
use crate::npk::hex;
use crate::urls;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, Response, StatusCode, Url};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// `User-Agent` sent with every request.
pub const USER_AGENT: &str = concat!("mac-netinstall/", env!("CARGO_PKG_VERSION"));

/// Largest text response accepted (version and checksum files are < 100 bytes).
const MAX_TEXT: u64 = 64 * 1024;
/// Largest whole body accepted when a server ignores a `Range` request.
const MAX_IGNORED_RANGE: u64 = 4 << 20;
/// Most redirects followed for one request.
const MAX_REDIRECTS: usize = 10;

/// HTTP settings. Bases have no trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConfig {
    /// Where versions live: `<download_base>/<ver>/<file>`.
    pub download_base: String,
    /// Where `NEWESTa7.<channel>` lives.
    pub upgrade_base: String,
    pub connect_timeout: Duration,
    /// Longest silence while reading a response.
    pub read_timeout: Duration,
    /// Longest time one file download may take in total (default 30 min).
    pub download_timeout: Duration,
    /// Largest download accepted (the netinstall tarball is ~46 MB).
    pub max_download_bytes: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            download_base: urls::DOWNLOAD_BASE.into(),
            upgrade_base: urls::UPGRADE_BASE.into(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(60),
            download_timeout: Duration::from_secs(30 * 60),
            max_download_bytes: 512 << 20,
        }
    }
}

/// Result of a streamed download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub size: u64,
    /// SHA-256 of the bytes received from the network.
    pub sha256: String,
}

/// Whether a redirect from `from` to `to` may be followed: to any `https`
/// URL, or to `http` on the same host and port when `from` is `http`.
fn redirect_allowed(from: &Url, to: &Url) -> bool {
    match (from.scheme(), to.scheme()) {
        (_, "https") => true,
        ("http", "http") => {
            from.host_str() == to.host_str()
                && from.port_or_known_default() == to.port_or_known_default()
        }
        _ => false,
    }
}

fn redirect_policy(attempt: Attempt) -> reqwest::redirect::Action {
    if attempt.previous().len() > MAX_REDIRECTS {
        return attempt.error("too many redirects");
    }
    let allowed = attempt
        .previous()
        .last()
        .is_some_and(|from| redirect_allowed(from, attempt.url()));
    if allowed {
        attempt.follow()
    } else {
        attempt.error("redirect refused")
    }
}

/// Map a reqwest error to `redirect_refused`, `timeout` or `network`.
fn net_error(url: &str, source: reqwest::Error) -> AssetError {
    let url = url.to_string();
    if source.is_redirect() {
        AssetError::RedirectRefused { url }
    } else if source.is_timeout() {
        AssetError::Timeout { url }
    } else {
        AssetError::Network { url, source }
    }
}

/// Parse `Content-Range: bytes <start>-<end>/<total>`.
fn parse_content_range(v: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = v.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

pub(crate) struct Http {
    client: Client,
    max: u64,
    download_timeout: Duration,
}

impl Http {
    pub fn new(cfg: &HttpConfig) -> Result<Self, AssetError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.read_timeout)
            .redirect(Policy::custom(redirect_policy))
            .build()
            .map_err(AssetError::Client)?;
        Ok(Self {
            client,
            max: cfg.max_download_bytes,
            download_timeout: cfg.download_timeout,
        })
    }

    async fn get(
        &self,
        url: &str,
        tail: Option<u64>,
        timeout: Option<Duration>,
    ) -> Result<Response, AssetError> {
        let mut req = self.client.get(url);
        if let Some(n) = tail {
            req = req.header(RANGE, format!("bytes=-{n}"));
        }
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(|e| net_error(url, e))?;
        match resp.status() {
            s if s.is_success() => Ok(resp),
            StatusCode::NOT_FOUND => Err(AssetError::NotFound { url: url.into() }),
            s => Err(AssetError::Status {
                url: url.into(),
                status: s.as_u16(),
            }),
        }
    }

    /// Read a whole body of at most `max` bytes.
    async fn body(url: &str, mut resp: Response, max: u64) -> Result<Vec<u8>, AssetError> {
        if resp.content_length().is_some_and(|n| n > max) {
            return Err(AssetError::TooLarge {
                url: url.into(),
                max,
            });
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| net_error(url, e))? {
            if out.len() as u64 + chunk.len() as u64 > max {
                return Err(AssetError::TooLarge {
                    url: url.into(),
                    max,
                });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// A small text file.
    pub async fn text(&self, url: &str) -> Result<String, AssetError> {
        let resp = self.get(url, None, None).await?;
        let body = Self::body(url, resp, MAX_TEXT).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// A small text file, or `None` on 404.
    pub async fn optional_text(&self, url: &str) -> Result<Option<String>, AssetError> {
        match self.text(url).await {
            Ok(t) => Ok(Some(t)),
            Err(AssetError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The last `n` bytes of a file and the file's total size, with a
    /// `Range` request. A 206 must carry a `Content-Range` that matches the
    /// body; a server that ignores the range (200) is accepted only for
    /// files up to 4 MiB.
    pub async fn tail(&self, url: &str, n: u64) -> Result<(Vec<u8>, u64), AssetError> {
        let resp = self.get(url, Some(n), None).await?;
        if resp.status() == StatusCode::PARTIAL_CONTENT {
            let bad = || AssetError::BadResponse {
                url: url.into(),
                reason: "missing or invalid Content-Range",
            };
            let (start, end, total) = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .ok_or_else(bad)?;
            let body = Self::body(url, resp, n).await?;
            let len = body.len() as u64;
            if len == 0 || len > total || start != total - len || end != total - 1 {
                return Err(bad());
            }
            Ok((body, total))
        } else {
            let body = match Self::body(url, resp, MAX_IGNORED_RANGE).await {
                Err(AssetError::TooLarge { .. }) => {
                    return Err(AssetError::BadResponse {
                        url: url.into(),
                        reason: "the server ignored the Range request and the file is over 4 MiB",
                    })
                }
                other => other?,
            };
            let total = body.len() as u64;
            let start = body.len().saturating_sub(n as usize);
            Ok((body[start..].to_vec(), total))
        }
    }

    /// Stream `url` into `dest`, which must not exist yet (it is created
    /// with `create_new`, so a file of another writer is never truncated),
    /// hashing on the way. `dest` is removed if the download fails.
    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        file: &str,
        emit: &(dyn Fn(AssetEvent) + Sync),
    ) -> Result<Downloaded, AssetError> {
        let out = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .await
            .map_err(AssetError::io("create", dest))?;
        let result = self.download_inner(url, out, dest, file, emit).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(dest).await;
        }
        result
    }

    async fn download_inner(
        &self,
        url: &str,
        mut out: tokio::fs::File,
        dest: &Path,
        file: &str,
        emit: &(dyn Fn(AssetEvent) + Sync),
    ) -> Result<Downloaded, AssetError> {
        let mut resp = self.get(url, None, Some(self.download_timeout)).await?;
        let total = resp.content_length();
        if total.is_some_and(|t| t > self.max) {
            return Err(AssetError::TooLarge {
                url: url.into(),
                max: self.max,
            });
        }
        emit(AssetEvent::DownloadStarted {
            file: file.into(),
            total,
        });
        let mut hasher = Sha256::new();
        let (mut done, mut reported) = (0u64, 0u64);
        while let Some(chunk) = resp.chunk().await.map_err(|e| net_error(url, e))? {
            done += chunk.len() as u64;
            if done > self.max {
                return Err(AssetError::TooLarge {
                    url: url.into(),
                    max: self.max,
                });
            }
            hasher.update(&chunk);
            out.write_all(&chunk)
                .await
                .map_err(AssetError::io("write", dest))?;
            if done - reported >= PROGRESS_STEP {
                reported = done;
                emit(AssetEvent::DownloadProgress {
                    file: file.into(),
                    done,
                    total,
                });
            }
        }
        if let Some(expected) = total.filter(|&t| t != done) {
            return Err(AssetError::Truncated {
                url: url.into(),
                expected,
                got: done,
            });
        }
        out.sync_all()
            .await
            .map_err(AssetError::io("write", dest))?;
        emit(AssetEvent::DownloadFinished {
            file: file.into(),
            size: done,
        });
        Ok(Downloaded {
            size: done,
            sha256: hex(&hasher.finalize()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{tempdir, Reply, TestServer};
    use std::sync::{Arc, Mutex};

    fn http(max: u64) -> Http {
        Http::new(&HttpConfig {
            read_timeout: Duration::from_millis(300),
            max_download_bytes: max,
            ..HttpConfig::default()
        })
        .unwrap()
    }

    fn collect() -> (Arc<Mutex<Vec<AssetEvent>>>, impl Fn(AssetEvent)) {
        let events: Arc<Mutex<Vec<AssetEvent>>> = Arc::default();
        let sink = events.clone();
        (events, move |e| sink.lock().unwrap().push(e))
    }

    #[tokio::test]
    async fn sends_the_fixed_user_agent() {
        let srv = TestServer::start().await;
        srv.route("/a", Reply::Body(b"hi".to_vec()));
        assert_eq!(
            http(1 << 20)
                .text(&format!("{}/a", srv.base))
                .await
                .unwrap(),
            "hi"
        );
        let head = srv.requests().remove(0).to_ascii_lowercase();
        assert!(
            head.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())),
            "{head}"
        );
        assert!(USER_AGENT.starts_with("mac-netinstall/0."));
    }

    #[tokio::test]
    async fn maps_404_and_other_statuses() {
        let srv = TestServer::start().await;
        srv.route("/boom", Reply::Status(503));
        let h = http(1 << 20);
        let url = format!("{}/missing", srv.base);
        assert!(matches!(h.text(&url).await, Err(AssetError::NotFound { url: u }) if u == url));
        assert_eq!(h.optional_text(&url).await.unwrap(), None);
        let err = h.text(&format!("{}/boom", srv.base)).await.unwrap_err();
        assert_eq!(srv.paths(), ["GET /missing", "GET /missing", "GET /boom"]);
        assert!(
            matches!(err, AssetError::Status { status: 503, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn download_streams_hashes_and_reports_progress() {
        let srv = TestServer::start().await;
        let body: Vec<u8> = (0..700_000u32).map(|i| i as u8).collect();
        srv.route("/big", Reply::Body(body.clone()));
        let dir = tempdir("dl");
        let dest = dir.join("big.part");
        let (events, emit) = collect();
        let got = http(1 << 20)
            .download(&format!("{}/big", srv.base), &dest, "big", &emit)
            .await
            .unwrap();
        assert_eq!(got.size, 700_000);
        assert_eq!(got.sha256, crate::npk::sha256_hex(&body));
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        let events = events.lock().unwrap().clone();
        assert_eq!(
            events.first(),
            Some(&AssetEvent::DownloadStarted {
                file: "big".into(),
                total: Some(700_000)
            })
        );
        assert_eq!(
            events.last(),
            Some(&AssetEvent::DownloadFinished {
                file: "big".into(),
                size: 700_000
            })
        );
        let progress: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                AssetEvent::DownloadProgress { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        assert!(!progress.is_empty() && progress.len() <= 2, "{progress:?}");
        assert!(progress.windows(2).all(|w| w[1] - w[0] >= PROGRESS_STEP));
    }

    #[tokio::test]
    async fn failed_downloads_leave_no_file() {
        let srv = TestServer::start().await;
        srv.route(
            "/short",
            Reply::Truncated {
                body: vec![1; 100],
                declared: 1000,
            },
        );
        srv.route("/huge", Reply::Body(vec![0; 2000]));
        srv.route("/stall", Reply::Stall);
        let h = http(1000);
        let dir = tempdir("fail");
        let dest = dir.join("x.part");
        let noop = |_| {};
        for (path, code) in [
            ("/short", "network"),
            ("/huge", "too_large"),
            ("/stall", "timeout"),
            ("/missing", "not_found"),
        ] {
            let err = h
                .download(&format!("{}{path}", srv.base), &dest, "x", &noop)
                .await
                .unwrap_err();
            assert_eq!(err.code(), code, "{path}: {err:?}");
            assert!(!dest.exists(), "{path}");
        }
    }

    #[tokio::test]
    async fn tail_uses_a_range_request() {
        let srv = TestServer::start().await;
        let body: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        srv.route("/z", Reply::Body(body.clone()));
        let (tail, total) = http(1 << 20)
            .tail(&format!("{}/z", srv.base), 100)
            .await
            .unwrap();
        assert_eq!((tail.as_slice(), total), (&body[900..], 1000));
        assert!(srv.requests()[0]
            .to_ascii_lowercase()
            .contains("range: bytes=-100"));
        // A file shorter than the tail comes back whole (200).
        let (tail, total) = http(1 << 20)
            .tail(&format!("{}/z", srv.base), 5000)
            .await
            .unwrap();
        assert_eq!((tail.len(), total), (1000, 1000));
    }

    #[tokio::test]
    async fn redirects_stay_on_the_same_host_or_go_to_https() {
        let srv = TestServer::start().await;
        let port = srv.base.rsplit(':').next().unwrap().to_string();
        srv.route("/a", Reply::Body(b"hi".to_vec()));
        srv.route("/same", Reply::Redirect("/a".into()));
        srv.route(
            "/other",
            Reply::Redirect(format!("http://localhost:{port}/a")),
        );
        let h = http(1 << 20);
        assert_eq!(h.text(&format!("{}/same", srv.base)).await.unwrap(), "hi");
        let err = h.text(&format!("{}/other", srv.base)).await.unwrap_err();
        assert_eq!(err.code(), "redirect_refused", "{err:?}");
        assert_eq!(srv.paths(), ["GET /same", "GET /a", "GET /other"]);
    }

    #[test]
    fn redirect_policy_refuses_downgrades_and_foreign_plain_http() {
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        let ok = |a: &str, b: &str| redirect_allowed(&u(a), &u(b));
        assert!(ok(
            "https://download.mikrotik.com/x",
            "https://cdn.mikrotik.com/x"
        ));
        assert!(ok("http://127.0.0.1:1/x", "http://127.0.0.1:1/y"));
        assert!(ok("http://mirror.lan/x", "https://mirror.lan/x"));
        assert!(!ok(
            "https://download.mikrotik.com/x",
            "http://download.mikrotik.com/x"
        ));
        assert!(!ok("http://127.0.0.1:1/x", "http://evil.example/x"));
        assert!(!ok("https://a/x", "ftp://a/x"));
    }

    #[tokio::test]
    async fn downloads_have_an_overall_time_limit() {
        let srv = TestServer::start().await;
        srv.route(
            "/slow",
            Reply::Slow {
                body: vec![7; 4000],
                chunk: 100,
                delay: Duration::from_millis(50),
            },
        );
        let h = Http::new(&HttpConfig {
            read_timeout: Duration::from_secs(5),
            download_timeout: Duration::from_millis(400),
            ..HttpConfig::default()
        })
        .unwrap();
        let dir = tempdir("dl-timeout");
        let dest = dir.join("slow.part");
        let err = h
            .download(&format!("{}/slow", srv.base), &dest, "slow", &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code(), "timeout", "{err:?}");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn download_never_overwrites_an_existing_file() {
        let srv = TestServer::start().await;
        srv.route("/a", Reply::Body(b"new".to_vec()));
        let dir = tempdir("dl-exists");
        let dest = dir.join("a.part");
        std::fs::write(&dest, b"someone else's").unwrap();
        let err = http(1 << 20)
            .download(&format!("{}/a", srv.base), &dest, "a", &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code(), "io", "{err:?}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"someone else's");
    }

    #[tokio::test]
    async fn tail_checks_content_range_and_caps_ignored_ranges() {
        let srv = TestServer::start().await;
        let raw = |range: &str, body: Vec<u8>| Reply::Raw {
            status: 206,
            headers: vec![format!("Content-Range: {range}")],
            body,
        };
        // Body is the last 100 bytes, but the range claims the first 100.
        srv.route("/wrong-start", raw("bytes 0-99/1000", vec![1; 100]));
        srv.route("/no-total", raw("bytes 900-999/*", vec![1; 100]));
        srv.route("/good", raw("bytes 900-999/1000", vec![1; 100]));
        srv.route(
            "/ignored",
            Reply::Raw {
                status: 200,
                headers: vec![],
                body: vec![0; (MAX_IGNORED_RANGE + 1) as usize],
            },
        );
        let h = http(512 << 20);
        for path in ["/wrong-start", "/no-total", "/ignored"] {
            let err = h
                .tail(&format!("{}{path}", srv.base), 100)
                .await
                .unwrap_err();
            assert_eq!(err.code(), "bad_response", "{path}: {err:?}");
        }
        let (tail, total) = h.tail(&format!("{}/good", srv.base), 100).await.unwrap();
        assert_eq!((tail.len(), total), (100, 1000));
    }
}
