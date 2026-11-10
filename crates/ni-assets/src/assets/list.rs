//! The version list ([`crate::versions`]) and version existence checks.

use super::{blocking, Assets};
use crate::cache::{read_capped, unix_now, CacheDir};
use crate::error::AssetError;
use crate::urls;
use crate::version::Version;
use crate::versions::{
    merge, parse_changelogs, parse_stored, seed, VersionList, MAX_CHANGELOG_PAGE,
    MAX_VERSIONS_FILE, PAGES, VERSIONS_FILE,
};
use serde::Serialize;
use std::time::Duration;

/// Where a version list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListSource {
    /// Just fetched from mikrotik.com.
    Fresh,
    /// The stored list (fresh enough, or fetching failed).
    Cached,
    /// The built-in list (nothing stored, fetching failed).
    Seed,
}

/// What [`Assets::version_list`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionsAnswer {
    pub list: VersionList,
    pub source: ListSource,
    /// Why fetching failed (an [`AssetError::code`]), if it was tried and
    /// failed.
    pub error: Option<&'static str>,
}

impl Assets {
    /// The stored version list, if any.
    async fn stored_versions(&self) -> Option<VersionList> {
        let path = self.root.join(VERSIONS_FILE);
        blocking(move || read_capped(&path, MAX_VERSIONS_FILE).and_then(|d| parse_stored(&d)))
            .await
            .ok()
            .flatten()
    }

    /// Fetch and parse both changelog pages. An empty result is a bad
    /// answer (the page layout changed).
    async fn fetch_versions(&self) -> Result<VersionList, AssetError> {
        let mut pages = Vec::new();
        for (filter, tag) in PAGES {
            let url = urls::changelogs_page(&self.changelog_base, filter);
            let html = self.http.text_max(&url, MAX_CHANGELOG_PAGE).await?;
            let releases = parse_changelogs(&html);
            if releases.is_empty() {
                return Err(AssetError::BadResponse {
                    url,
                    reason: "no versions on the changelog page",
                });
            }
            pages.push((*tag, releases));
        }
        Ok(merge(&pages, unix_now()))
    }

    /// The versions a profile can pick from. The stored list
    /// ([`VERSIONS_FILE`]) is used while it is younger than `max_age`
    /// (unless `refresh`); otherwise both changelog pages are fetched and
    /// the result stored. If that fails, the last good list is kept (and
    /// its age reported through [`VersionList::fetched_unix`]), or the
    /// built-in seed when there is none. Never fails.
    pub async fn version_list(&self, max_age: Duration, refresh: bool) -> VersionsAnswer {
        let stored = self.stored_versions().await;
        if let Some(list) = &stored {
            let now = unix_now();
            // A time in the future (clock changed, edited file) is stale.
            let future = list.fetched_unix > now.saturating_add(60);
            let age = now.saturating_sub(list.fetched_unix);
            if !refresh && !future && age < max_age.as_secs() {
                return VersionsAnswer {
                    list: list.clone(),
                    source: ListSource::Cached,
                    error: None,
                };
            }
        }
        match self.fetch_versions().await {
            Ok(list) => {
                let dir = CacheDir::new(self.root.clone());
                let json = serde_json::to_vec_pretty(&list).expect("list serializes");
                // Best-effort: a list that cannot be stored is still good.
                let _ = blocking(move || dir.write_atomic(VERSIONS_FILE, &json)).await;
                VersionsAnswer {
                    list,
                    source: ListSource::Fresh,
                    error: None,
                }
            }
            Err(e) => match stored {
                Some(list) => VersionsAnswer {
                    list,
                    source: ListSource::Cached,
                    error: Some(e.code()),
                },
                None => VersionsAnswer {
                    list: seed(),
                    source: ListSource::Seed,
                    error: Some(e.code()),
                },
            },
        }
    }

    /// Whether `version` is published: its `CHANGELOG` exists (a `HEAD`
    /// request; 404 is `false`).
    pub async fn version_exists(&self, version: &Version) -> Result<bool, AssetError> {
        let url = urls::changelog(&self.download_base, version);
        self.http.exists(&url).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn page(versions: &[(&str, &str)]) -> Reply {
        let html: String = versions
            .iter()
            .map(|(v, d)| {
                format!(
                    "<div data-changelog-version=\"{v}\"><span>{v}</span><span> {d} </span></div>"
                )
            })
            .collect();
        Reply::Body(html.into_bytes())
    }

    fn serve_pages(srv: &TestServer) {
        srv.route(
            "/cl?channelFilter=stable",
            page(&[("7.25.1", "2026-11-02"), ("6.49.23", "2026-03-15")]),
        );
        srv.route(
            "/cl?channelFilter=longTerm",
            page(&[("7.24.6", "2026-11-02"), ("6.49.23", "2026-03-15")]),
        );
    }

    fn names(a: &VersionsAnswer) -> Vec<&str> {
        a.list.versions.iter().map(|l| l.version.as_str()).collect()
    }

    #[tokio::test]
    async fn the_list_is_fetched_stored_and_reused() {
        let srv = TestServer::start().await;
        serve_pages(&srv);
        let root = tempdir("versions-list");
        let a = assets(&srv, &root);
        let day = Duration::from_secs(24 * 3600);
        let got = a.version_list(day, false).await;
        assert_eq!((got.source, got.error), (ListSource::Fresh, None));
        assert_eq!(names(&got), ["7.25.1", "7.24.6", "6.49.23"]);
        assert!(root.join(VERSIONS_FILE).exists());
        let requests = srv.paths().len();
        assert_eq!(requests, 2);
        // Young enough: from disk.
        let again = a.version_list(day, false).await;
        assert_eq!(again.source, ListSource::Cached);
        assert_eq!(again.list, got.list);
        assert_eq!(srv.paths().len(), requests);
        // Refresh asks again; an old list is fetched again too.
        a.version_list(day, true).await;
        a.version_list(Duration::ZERO, false).await;
        assert_eq!(srv.paths().len(), requests + 4);
    }

    /// A list "fetched" in the future (clock changed, hand-edited) is
    /// stale, not fresh forever.
    #[tokio::test]
    async fn a_list_from_the_future_is_stale() {
        let srv = TestServer::start().await;
        serve_pages(&srv);
        let root = tempdir("versions-future");
        std::fs::create_dir_all(&*root).unwrap();
        let mut future = crate::versions::seed();
        future.fetched_unix = unix_now() + 365 * 24 * 3600;
        std::fs::write(
            root.join(VERSIONS_FILE),
            serde_json::to_vec(&future).unwrap(),
        )
        .unwrap();
        let a = assets(&srv, &root);
        let got = a.version_list(Duration::from_secs(24 * 3600), false).await;
        assert_eq!(got.source, ListSource::Fresh);
        assert_eq!(srv.paths().len(), 2);
    }

    #[tokio::test]
    async fn a_failed_fetch_keeps_the_last_good_list_or_the_seed() {
        let srv = TestServer::start().await;
        let root = tempdir("versions-fail");
        let a = assets(&srv, &root);
        let day = Duration::from_secs(24 * 3600);
        // Nothing stored, nothing served: the seed.
        let got = a.version_list(day, false).await;
        assert_eq!(
            (got.source, got.error),
            (ListSource::Seed, Some("not_found"))
        );
        assert_eq!(names(&got), ["7.25.1", "7.24.6", "6.49.23", "6.48.7"]);
        // A good fetch, then the page changes layout: the stored list stays.
        serve_pages(&srv);
        a.version_list(day, true).await;
        srv.route(
            "/cl?channelFilter=longTerm",
            Reply::Body(b"<html>new</html>".to_vec()),
        );
        let got = a.version_list(day, true).await;
        assert_eq!(
            (got.source, got.error),
            (ListSource::Cached, Some("bad_response"))
        );
        assert_eq!(names(&got), ["7.25.1", "7.24.6", "6.49.23"]);
        // A huge page is refused, not read.
        srv.route(
            "/cl?channelFilter=stable",
            Reply::Body(vec![b' '; MAX_CHANGELOG_PAGE as usize + 1]),
        );
        let got = a.version_list(day, true).await;
        assert_eq!(got.error, Some("too_large"));
    }

    #[tokio::test]
    async fn existence_is_checked_with_the_changelog() {
        let srv = TestServer::start().await;
        mirror_v6(&srv, V6, "arm", "arm", true);
        let root = tempdir("versions-exists");
        let a = assets(&srv, &root);
        assert!(a
            .version_exists(&Version::parse(V6).unwrap())
            .await
            .unwrap());
        assert!(!a
            .version_exists(&Version::parse("6.49.99").unwrap())
            .await
            .unwrap());
        assert!(srv.paths().contains(&format!("HEAD /dl/{V6}/CHANGELOG")));
    }
}
