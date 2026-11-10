//! Update check and verified download of new Mac Netinstall releases.
//!
//! - [`Updater::latest`]: one unauthenticated `GET` of GitHub's
//!   `releases/latest` for this project (no user data is sent: only the
//!   fixed `User-Agent` and `Accept` headers). Drafts and prereleases are
//!   ignored.
//! - [`is_newer`]: plain `X.Y.Z` comparison with the running version.
//! - [`Updater::download`]: the release's DMG into a folder, checked
//!   against the published `.sha256` file, never overwriting a file
//!   (`name (1).dmg`, …).
//!
//! Asset URLs must lie under the project's release downloads
//! ([`DOWNLOAD_PREFIX`]); redirects are followed only over `https` to
//! GitHub's download hosts ([`DOWNLOAD_HOSTS`]) or within the same origin.
//! Errors carry stable codes ([`UpdateError::code`], [`PROBLEMS`]).

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT};
use reqwest::redirect::{Attempt, Policy};
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// GitHub's latest-release endpoint for this project.
pub const API_LATEST: &str = "https://api.github.com/repos/SepsiG/Mac-Netinstall/releases/latest";
/// Every release page starts with this.
pub const RELEASES_PAGE: &str = "https://github.com/SepsiG/Mac-Netinstall/releases/";
/// Every release download starts with this.
pub const DOWNLOAD_PREFIX: &str = "https://github.com/SepsiG/Mac-Netinstall/releases/download/";
/// Hosts a download may be redirected to (over `https` only).
pub const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
/// `User-Agent` of every request.
pub const USER_AGENT: &str = concat!("mac-netinstall/", env!("CARGO_PKG_VERSION"));
/// How often the automatic check may ask GitHub.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest release notes kept (characters; longer ones end in `…`).
pub const MAX_NOTES_CHARS: usize = 20_000;
/// Largest API answer read.
const MAX_API_BYTES: u64 = 1 << 20;
/// Largest checksum file read.
const MAX_SUM_BYTES: u64 = 4096;
/// Most redirects followed for one request.
const MAX_REDIRECTS: usize = 10;
/// Progress is reported at least this many bytes apart.
const PROGRESS_STEP: u64 = 256 * 1024;
/// Most `name (n).dmg` tried before giving up.
const MAX_NAME_SUFFIX: u32 = 999;

/// Codes of [`UpdateError`] (the UI's translation check reads them).
pub const PROBLEMS: &[&str] = &[
    "rate_limited",
    "no_release",
    "http_status",
    "http_client",
    "network",
    "timeout",
    "redirect_refused",
    "bad_response",
    "missing_asset",
    "url_refused",
    "too_large",
    "truncated",
    "checksum_invalid",
    "checksum_mismatch",
    "name_taken",
    "io",
];

/// What can go wrong; [`UpdateError::code`] is stable.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// GitHub refused for now (403 / 429): skip quietly.
    #[error("GitHub rate limit (HTTP {status})")]
    RateLimited { status: u16 },
    #[error("no published release")]
    NoRelease,
    #[error("HTTP {status}")]
    Status { status: u16 },
    #[error("cannot set up the HTTP client: {0}")]
    Client(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("timed out")]
    Timeout,
    #[error("redirect to a host that is not allowed")]
    RedirectRefused,
    #[error("unexpected answer: {0}")]
    BadResponse(&'static str),
    #[error("the release has no {0}")]
    MissingAsset(&'static str),
    #[error("download URL outside the project's releases")]
    UrlRefused,
    #[error("larger than {max} bytes")]
    TooLarge { max: u64 },
    #[error("ended after {got} of {expected} bytes")]
    Truncated { expected: u64, got: u64 },
    #[error("the checksum file is not a SHA-256 of the DMG")]
    ChecksumInvalid,
    #[error("SHA-256 mismatch")]
    ChecksumMismatch,
    #[error("no free file name")]
    NameTaken,
    #[error("file error: {0}")]
    Io(String),
}

impl UpdateError {
    /// One of [`PROBLEMS`].
    pub fn code(&self) -> &'static str {
        match self {
            UpdateError::RateLimited { .. } => "rate_limited",
            UpdateError::NoRelease => "no_release",
            UpdateError::Status { .. } => "http_status",
            UpdateError::Client(_) => "http_client",
            UpdateError::Network(_) => "network",
            UpdateError::Timeout => "timeout",
            UpdateError::RedirectRefused => "redirect_refused",
            UpdateError::BadResponse(_) => "bad_response",
            UpdateError::MissingAsset(_) => "missing_asset",
            UpdateError::UrlRefused => "url_refused",
            UpdateError::TooLarge { .. } => "too_large",
            UpdateError::Truncated { .. } => "truncated",
            UpdateError::ChecksumInvalid => "checksum_invalid",
            UpdateError::ChecksumMismatch => "checksum_mismatch",
            UpdateError::NameTaken => "name_taken",
            UpdateError::Io(_) => "io",
        }
    }

    fn io(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

/// A release version: exactly `X.Y.Z` (an optional leading `v`), no
/// prerelease or build suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemVer(pub u64, pub u64, pub u64);

impl SemVer {
    pub fn parse(s: &str) -> Option<SemVer> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut parts = s.split('.');
        let mut num = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = SemVer(num()?, num()?, num()?);
        parts.next().is_none().then_some(v)
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Whether `latest` is a newer `X.Y.Z` than `current` (anything that is
/// not a plain version, e.g. a prerelease, is never newer).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (SemVer::parse(latest), SemVer::parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Whether an automatic check is due: never checked, a day passed since
/// `last` (Unix seconds), or the clock went back.
pub fn check_due(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        Some(last) => now < last || now - last >= CHECK_INTERVAL.as_secs(),
    }
}

/// `url` parses to exactly itself (no `..`, spaces or other text that a
/// parser would rewrite) and starts with `prefix`.
fn url_under(url: &str, prefix: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.as_str() == url && url.starts_with(prefix))
}

/// Whether `url` is one of the project's release pages (what the app may
/// open in the browser).
pub fn release_page_allowed(url: &str) -> bool {
    url_under(url, RELEASES_PAGE)
        && Url::parse(url)
            .is_ok_and(|u| u.query().is_none() && u.fragment().is_none() && u.port().is_none())
}

/// Whether a redirect from `from` to `to` may be followed: over `https` to
/// one of [`DOWNLOAD_HOSTS`], or within the same origin.
pub fn redirect_allowed(from: &Url, to: &Url) -> bool {
    let github = to.scheme() == "https"
        && to.port().is_none()
        && to.host_str().is_some_and(|h| DOWNLOAD_HOSTS.contains(&h));
    let same_origin = from.scheme() == to.scheme()
        && from.host_str() == to.host_str()
        && from.port_or_known_default() == to.port_or_known_default();
    github || same_origin
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

fn net_error(e: reqwest::Error) -> UpdateError {
    if e.is_redirect() {
        UpdateError::RedirectRefused
    } else if e.is_timeout() {
        UpdateError::Timeout
    } else {
        UpdateError::Network(e.to_string())
    }
}

/// A release file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    /// Under the configured download prefix.
    pub url: String,
    pub size: u64,
}

/// The latest published release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// `X.Y.Z`.
    pub version: String,
    /// The tag (`vX.Y.Z`).
    pub tag: String,
    /// The release page (always under [`RELEASES_PAGE`]).
    pub page_url: String,
    /// The release notes (Markdown source, at most [`MAX_NOTES_CHARS`]).
    pub notes: String,
    /// When it was published (RFC 3339, as GitHub sends it).
    pub published_at: Option<String>,
    /// `Mac-Netinstall_<version>_aarch64.dmg`.
    pub dmg: Asset,
    /// The DMG's `.sha256` file.
    pub checksum: Asset,
}

impl Release {
    /// The DMG's file name for `version`.
    pub fn dmg_name(version: &str) -> String {
        format!("Mac-Netinstall_{version}_aarch64.dmg")
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn cap_notes(s: &str) -> String {
    match s.char_indices().nth(MAX_NOTES_CHARS) {
        None => s.to_string(),
        Some((i, _)) => format!("{}…", &s[..i]),
    }
}

/// A `releases/latest` answer; `None` for a draft or prerelease.
fn parse_latest(json: &[u8], download_prefix: &str) -> Result<Option<Release>, UpdateError> {
    let gh: GhRelease =
        serde_json::from_slice(json).map_err(|_| UpdateError::BadResponse("not a release"))?;
    if gh.draft || gh.prerelease {
        return Ok(None);
    }
    let Some(version) = SemVer::parse(&gh.tag_name) else {
        let bare = gh.tag_name.strip_prefix('v').unwrap_or(&gh.tag_name);
        let prerelease = bare
            .split_once('-')
            .is_some_and(|(base, _)| SemVer::parse(base).is_some());
        return if prerelease {
            Ok(None)
        } else {
            Err(UpdateError::BadResponse("tag is not a version"))
        };
    };
    let version = version.to_string();
    let dmg_name = Release::dmg_name(&version);
    let sum_name = format!("{dmg_name}.sha256");
    let asset = |name: &str, what: &'static str| -> Result<Asset, UpdateError> {
        let a = gh
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or(UpdateError::MissingAsset(what))?;
        if !url_under(&a.browser_download_url, download_prefix) {
            return Err(UpdateError::UrlRefused);
        }
        Ok(Asset {
            name: a.name.clone(),
            url: a.browser_download_url.clone(),
            size: a.size,
        })
    };
    let dmg = asset(&dmg_name, "dmg")?;
    let checksum = asset(&sum_name, "checksum")?;
    let page_url = gh
        .html_url
        .filter(|u| release_page_allowed(u))
        .unwrap_or_else(|| format!("{RELEASES_PAGE}tag/{}", gh.tag_name));
    let page_url = if release_page_allowed(&page_url) {
        page_url
    } else {
        format!("{RELEASES_PAGE}latest")
    };
    Ok(Some(Release {
        tag: gh.tag_name,
        version,
        page_url,
        notes: cap_notes(gh.body.as_deref().unwrap_or("")),
        published_at: gh.published_at.filter(|p| p.len() <= 64),
        dmg,
        checksum,
    }))
}

/// The SHA-256 in a `.sha256` file (`<hex>  <name>` or `<hex> *<name>`,
/// as `shasum -a 256` writes), if it is for `name`.
fn parse_checksum(text: &str, name: &str) -> Option<String> {
    let line = text.lines().next()?.trim_end();
    let (hex, rest) = line.split_once(' ')?;
    let file = rest.trim_start_matches(' ');
    let file = file.strip_prefix('*').unwrap_or(file);
    let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) && file == name;
    ok.then(|| hex.to_ascii_lowercase())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `name`, or `stem (n).ext` for the first `n` free in `dir`.
fn free_path(dir: &Path, name: &str) -> Result<PathBuf, UpdateError> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    (0..=MAX_NAME_SUFFIX)
        .map(|n| match n {
            0 => dir.join(name),
            n => dir.join(format!("{stem} ({n}){ext}")),
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .ok_or(UpdateError::NameTaken)
}

/// Where to ask and what to accept. Bases have no trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateConfig {
    /// The latest-release endpoint ([`API_LATEST`]).
    pub api_url: String,
    /// Asset URLs must start with this ([`DOWNLOAD_PREFIX`]).
    pub download_prefix: String,
    pub connect_timeout: Duration,
    /// Longest silence while reading a response.
    pub read_timeout: Duration,
    /// Longest time the DMG download may take in total.
    pub download_timeout: Duration,
    /// Largest DMG accepted (200 MB).
    pub max_download_bytes: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig {
            api_url: API_LATEST.into(),
            download_prefix: DOWNLOAD_PREFIX.into(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            download_timeout: Duration::from_secs(30 * 60),
            max_download_bytes: 200 << 20,
        }
    }
}

/// Asks GitHub and downloads releases.
#[derive(Debug, Clone)]
pub struct Updater {
    client: Client,
    cfg: UpdateConfig,
}

/// Distinguishes the partial files of concurrent downloads.
static PART_COUNTER: AtomicU32 = AtomicU32::new(0);

impl Updater {
    pub fn new(cfg: UpdateConfig) -> Result<Updater, UpdateError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(cfg.connect_timeout)
            .read_timeout(cfg.read_timeout)
            .redirect(Policy::custom(redirect_policy))
            .build()
            .map_err(|e| UpdateError::Client(e.to_string()))?;
        Ok(Updater { client, cfg })
    }

    pub fn config(&self) -> &UpdateConfig {
        &self.cfg
    }

    async fn get(
        &self,
        url: &str,
        headers: HeaderMap,
        timeout: Duration,
    ) -> Result<Response, UpdateError> {
        let resp = self
            .client
            .get(url)
            .headers(headers)
            .timeout(timeout)
            .send()
            .await
            .map_err(net_error)?;
        match resp.status() {
            s if s.is_success() => Ok(resp),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                Err(UpdateError::RateLimited {
                    status: resp.status().as_u16(),
                })
            }
            StatusCode::NOT_FOUND => Err(UpdateError::NoRelease),
            s => Err(UpdateError::Status { status: s.as_u16() }),
        }
    }

    async fn body(mut resp: Response, max: u64) -> Result<Vec<u8>, UpdateError> {
        if resp.content_length().is_some_and(|n| n > max) {
            return Err(UpdateError::TooLarge { max });
        }
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
            if out.len() as u64 + chunk.len() as u64 > max {
                return Err(UpdateError::TooLarge { max });
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// The latest release, or `None` if it is a draft or prerelease.
    /// `rate_limited` (403 / 429) is meant to be skipped quietly.
    pub async fn latest(&self) -> Result<Option<Release>, UpdateError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        let resp = self
            .get(&self.cfg.api_url, headers, self.cfg.read_timeout * 2)
            .await?;
        let body = Self::body(resp, MAX_API_BYTES).await?;
        parse_latest(&body, &self.cfg.download_prefix)
    }

    /// Download `release`'s DMG into `dir` (never overwriting: `name (1).dmg`
    /// if taken) after fetching its `.sha256`, and check it. `progress`
    /// gets bytes done and the total (if known). Nothing is left in `dir`
    /// on failure. Returns the DMG's path.
    pub async fn download(
        &self,
        release: &Release,
        dir: &Path,
        progress: &(dyn Fn(u64, Option<u64>) + Sync),
    ) -> Result<PathBuf, UpdateError> {
        let prefix = &self.cfg.download_prefix;
        if !url_under(&release.dmg.url, prefix) || !url_under(&release.checksum.url, prefix) {
            return Err(UpdateError::UrlRefused);
        }
        let max = self.cfg.max_download_bytes;
        if release.dmg.size > max {
            return Err(UpdateError::TooLarge { max });
        }
        let sum = self
            .get(
                &release.checksum.url,
                HeaderMap::new(),
                self.cfg.read_timeout * 2,
            )
            .await?;
        let sum = Self::body(sum, MAX_SUM_BYTES).await?;
        let expected = parse_checksum(&String::from_utf8_lossy(&sum), &release.dmg.name)
            .ok_or(UpdateError::ChecksumInvalid)?;
        let part = dir.join(format!(
            ".{}.{}-{}.part",
            release.dmg.name,
            std::process::id(),
            PART_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let out = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
            .map_err(UpdateError::io)?;
        let result = self
            .stream(&release.dmg.url, out, &expected, progress)
            .await
            .and_then(|()| Self::place(&part, dir, &release.dmg.name));
        let _ = std::fs::remove_file(&part);
        result
    }

    async fn stream(
        &self,
        url: &str,
        mut out: tokio::fs::File,
        expected: &str,
        progress: &(dyn Fn(u64, Option<u64>) + Sync),
    ) -> Result<(), UpdateError> {
        let max = self.cfg.max_download_bytes;
        let mut resp = self
            .get(url, HeaderMap::new(), self.cfg.download_timeout)
            .await?;
        let total = resp.content_length();
        if total.is_some_and(|t| t > max) {
            return Err(UpdateError::TooLarge { max });
        }
        progress(0, total);
        let mut hasher = Sha256::new();
        let (mut done, mut reported) = (0u64, 0u64);
        while let Some(chunk) = resp.chunk().await.map_err(net_error)? {
            done += chunk.len() as u64;
            if done > max {
                return Err(UpdateError::TooLarge { max });
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await.map_err(UpdateError::io)?;
            if done - reported >= PROGRESS_STEP {
                reported = done;
                progress(done, total);
            }
        }
        if let Some(expected) = total.filter(|&t| t != done) {
            return Err(UpdateError::Truncated {
                expected,
                got: done,
            });
        }
        out.sync_all().await.map_err(UpdateError::io)?;
        if reported != done {
            progress(done, total);
        }
        if hex(&hasher.finalize()) != expected {
            return Err(UpdateError::ChecksumMismatch);
        }
        Ok(())
    }

    /// Give the verified `part` its final name: a hard link fails instead
    /// of replacing a file that appeared meanwhile.
    fn place(part: &Path, dir: &Path, name: &str) -> Result<PathBuf, UpdateError> {
        for _ in 0..8 {
            let dest = free_path(dir, name)?;
            match std::fs::hard_link(part, &dest) {
                Ok(()) => return Ok(dest),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(UpdateError::io(e)),
            }
        }
        Err(UpdateError::NameTaken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_lines() {
        let h = "a".repeat(64);
        assert_eq!(
            parse_checksum(&format!("{h}  x.dmg\n"), "x.dmg"),
            Some(h.clone())
        );
        assert_eq!(
            parse_checksum(&format!("{h} *x.dmg"), "x.dmg"),
            Some(h.clone())
        );
        assert_eq!(
            parse_checksum(&format!("{}  x.dmg", "A".repeat(64)), "x.dmg"),
            Some(h.clone())
        );
        assert_eq!(parse_checksum(&format!("{h}  y.dmg"), "x.dmg"), None);
        assert_eq!(parse_checksum(&format!("{h}x  x.dmg"), "x.dmg"), None);
        assert_eq!(parse_checksum("", "x.dmg"), None);
    }

    #[test]
    fn free_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("ni-update-free-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_path(&dir, "a.dmg").unwrap(), dir.join("a.dmg"));
        std::fs::write(dir.join("a.dmg"), b"").unwrap();
        std::fs::write(dir.join("a (1).dmg"), b"").unwrap();
        assert_eq!(free_path(&dir, "a.dmg").unwrap(), dir.join("a (2).dmg"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
