//! HTTP access: small text files, ranged tails and streaming downloads.

use crate::error::AssetError;
use crate::events::{AssetEvent, PROGRESS_STEP};
use crate::npk::hex;
use crate::urls;
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{Client, Response, StatusCode};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// `User-Agent` sent with every request.
pub const USER_AGENT: &str = concat!("mac-netinstall/", env!("CARGO_PKG_VERSION"));

/// Largest text response accepted (version and checksum files are < 100 bytes).
const MAX_TEXT: u64 = 64 * 1024;

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
            max_download_bytes: 512 << 20,
        }
    }
}

/// Result of a streamed download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub size: u64,
    pub sha256: String,
}

pub(crate) struct Http {
    client: Client,
    max: u64,
}

impl Http {
    pub fn new(cfg: &HttpConfig) -> Result<Self, AssetError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.read_timeout)
            .build()
            .map_err(AssetError::Client)?;
        Ok(Self {
            client,
            max: cfg.max_download_bytes,
        })
    }

    async fn get(&self, url: &str, tail: Option<u64>) -> Result<Response, AssetError> {
        let mut req = self.client.get(url);
        if let Some(n) = tail {
            req = req.header(RANGE, format!("bytes=-{n}"));
        }
        let resp = req.send().await.map_err(|source| AssetError::Network {
            url: url.into(),
            source,
        })?;
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
        while let Some(chunk) = resp.chunk().await.map_err(|source| AssetError::Network {
            url: url.into(),
            source,
        })? {
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
        let resp = self.get(url, None).await?;
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

    /// The last `n` bytes of a file and the file's total size. Uses a
    /// `Range` request; a server that ignores it sends the whole file,
    /// which is accepted up to the download limit.
    pub async fn tail(&self, url: &str, n: u64) -> Result<(Vec<u8>, u64), AssetError> {
        let resp = self.get(url, Some(n)).await?;
        if resp.status() == StatusCode::PARTIAL_CONTENT {
            let bad = || AssetError::BadResponse {
                url: url.into(),
                reason: "missing or invalid Content-Range",
            };
            let total = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit_once('/'))
                .and_then(|(_, t)| t.parse::<u64>().ok())
                .ok_or_else(bad)?;
            let body = Self::body(url, resp, n).await?;
            if body.len() as u64 > total {
                return Err(bad());
            }
            Ok((body, total))
        } else {
            let body = Self::body(url, resp, self.max).await?;
            let total = body.len() as u64;
            let start = body.len().saturating_sub(n as usize);
            Ok((body[start..].to_vec(), total))
        }
    }

    /// Stream `url` into `dest` (created or truncated), hashing on the way.
    /// `dest` is removed if the download fails.
    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        file: &str,
        emit: &dyn Fn(AssetEvent),
    ) -> Result<Downloaded, AssetError> {
        let result = self.download_inner(url, dest, file, emit).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(dest).await;
        }
        result
    }

    async fn download_inner(
        &self,
        url: &str,
        dest: &Path,
        file: &str,
        emit: &dyn Fn(AssetEvent),
    ) -> Result<Downloaded, AssetError> {
        let mut resp = self.get(url, None).await?;
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
        let mut out = tokio::fs::File::create(dest)
            .await
            .map_err(AssetError::io("create", dest))?;
        let mut hasher = Sha256::new();
        let (mut done, mut reported) = (0u64, 0u64);
        while let Some(chunk) = resp.chunk().await.map_err(|source| AssetError::Network {
            url: url.into(),
            source,
        })? {
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
            ("/stall", "network"),
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
}
