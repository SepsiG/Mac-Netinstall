//! The list of RouterOS versions a profile can pick from.
//!
//! MikroTik publishes no machine-readable release history. The changelog
//! page (`https://mikrotik.com/download/changelogs?channelFilter=stable`,
//! `...=longTerm`) is HTML with one element per release carrying
//! `data-changelog-version="X"`, newest first, and the release date
//! (`2026-11-02`) as text in its header. The parameter is undocumented, so
//! this is a scrape that may break: the last good list is kept on disk
//! ([`VERSIONS_FILE`]) and a built-in [`SEED`] covers the first run
//! offline. See `docs/superpowers/research/2026-11-10-routeros-v6-and-version-list.md`.

use crate::version::Version;
use serde::{Deserialize, Serialize};

/// `<cache root>/versions.json`: the last good version list.
pub const VERSIONS_FILE: &str = "versions.json";
/// Largest changelog page accepted (the stable page was 1.2 MB on
/// 2026-11-10).
pub const MAX_CHANGELOG_PAGE: u64 = 2 << 20;
/// Largest `versions.json` read back.
pub const MAX_VERSIONS_FILE: u64 = 1 << 20;
/// Most versions kept in a list (the stable page lists ~260).
pub const MAX_VERSIONS: usize = 2000;
/// How far after a version's marker its date is looked for (bytes).
const DATE_WINDOW: usize = 6000;

/// The channel pages scraped, with the tag each gives its versions.
pub const PAGES: &[(&str, &str)] = &[("stable", "stable"), ("longTerm", "long-term")];

/// One version of the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedVersion {
    pub version: Version,
    /// Channels it was released on: `stable`, `long-term` (both for some).
    pub channels: Vec<String>,
    /// Release date, `YYYY-MM-DD`, if known.
    pub date: Option<String>,
}

/// A version list, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionList {
    /// When it was fetched (Unix seconds); 0 for the built-in seed.
    pub fetched_unix: u64,
    pub versions: Vec<ListedVersion>,
}

/// Versions known when this app was built, for a first run without the
/// network: the newest stable and long-term of v7 and v6.
pub const SEED: &[(&str, &[&str])] = &[
    ("7.25.1", &["stable"]),
    ("7.24.6", &["long-term"]),
    ("6.49.23", &["stable", "long-term"]),
    ("6.48.7", &["long-term"]),
];

/// The built-in list ([`SEED`]).
pub fn seed() -> VersionList {
    VersionList {
        fetched_unix: 0,
        versions: SEED
            .iter()
            .map(|(v, channels)| ListedVersion {
                version: Version::parse(v).expect("seed versions are valid"),
                channels: channels.iter().map(|c| c.to_string()).collect(),
                date: None,
            })
            .collect(),
    }
}

/// A release version as the list shows it: `X.Y` or `X.Y.Z`, digits only
/// (no betas or rcs), at most 16 characters.
fn release_shape(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    s.len() <= 16
        && (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| (1..=4).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The first `YYYY-MM-DD` in `text`.
fn first_date(text: &str) -> Option<String> {
    let b = text.as_bytes();
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let w = &b[i..i + 10];
        let digits = |r: std::ops::Range<usize>| w[r].iter().all(u8::is_ascii_digit);
        let shape = digits(0..4) && w[4] == b'-' && digits(5..7) && w[7] == b'-' && digits(8..10);
        let boundary = (i == 0 || !b[i - 1].is_ascii_digit())
            && b.get(i + 10).is_none_or(|c| !c.is_ascii_digit());
        (shape && boundary).then(|| String::from_utf8_lossy(w).into_owned())
    })
}

/// Releases of one page: version and date, newest first.
pub type Releases = Vec<(Version, Option<String>)>;

/// The releases on one changelog page, in page order (newest first), with
/// their dates: every `data-changelog-version="X"` whose `X` is a release
/// (see `release_shape`) this app accepts (v6 from 6.30, v7+). Never panics;
/// duplicates and anything past [`MAX_VERSIONS`] are dropped.
pub fn parse_changelogs(html: &str) -> Releases {
    const MARK: &str = "data-changelog-version=\"";
    let starts: Vec<usize> = html.match_indices(MARK).map(|(i, _)| i).collect();
    let mut out: Vec<(Version, Option<String>)> = Vec::new();
    for (n, &at) in starts.iter().enumerate() {
        if out.len() >= MAX_VERSIONS {
            break;
        }
        let rest = &html[at + MARK.len()..];
        // Only the next 33 bytes are looked at: a page full of markers
        // without a closing quote stays linear.
        let head = &rest.as_bytes()[..rest.len().min(33)];
        let Some(end) = head.iter().position(|&b| b == b'"') else {
            continue;
        };
        let text = &rest[..end];
        if !release_shape(text) {
            continue;
        }
        let Ok(version) = Version::parse(text) else {
            continue;
        };
        if out.iter().any(|(v, _)| *v == version) {
            continue;
        }
        let next = starts.get(n + 1).copied().unwrap_or(html.len());
        let mut stop = (at + DATE_WINDOW).min(next).min(html.len());
        while !html.is_char_boundary(stop) {
            stop -= 1;
        }
        let date = first_date(&html[at..stop]);
        out.push((version, date));
    }
    out
}

/// Merge the pages' releases (`(tag, releases)`) into one list, newest
/// first, each version once with all its channels.
pub fn merge(pages: &[(&str, Releases)], fetched_unix: u64) -> VersionList {
    let mut versions: Vec<ListedVersion> = Vec::new();
    for (tag, releases) in pages {
        for (version, date) in releases {
            match versions.iter_mut().find(|l| l.version == *version) {
                Some(l) => {
                    if !l.channels.iter().any(|c| c == tag) {
                        l.channels.push(tag.to_string());
                    }
                    if l.date.is_none() {
                        l.date.clone_from(date);
                    }
                }
                None => versions.push(ListedVersion {
                    version: version.clone(),
                    channels: vec![tag.to_string()],
                    date: date.clone(),
                }),
            }
        }
    }
    versions.sort_by(|a, b| b.version.cmp(&a.version));
    versions.truncate(MAX_VERSIONS);
    VersionList {
        fetched_unix,
        versions,
    }
}

/// A stored list read back, or `None` if it is damaged or empty. Dates
/// that are not `YYYY-MM-DD` and unknown channels are dropped.
pub fn parse_stored(data: &[u8]) -> Option<VersionList> {
    let mut list: VersionList = serde_json::from_slice(data).ok()?;
    if list.versions.is_empty() {
        return None;
    }
    list.versions.truncate(MAX_VERSIONS);
    for l in &mut list.versions {
        l.channels.retain(|c| PAGES.iter().any(|(_, tag)| tag == c));
        if l.date
            .as_deref()
            .is_some_and(|d| first_date(d).as_deref() != Some(d))
        {
            l.date = None;
        }
    }
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(v: &str, date: &str) -> String {
        format!(
            "<div x-on:header-click=\"toggleChangelog('{v}')\" class=\"changelog\" \
             wire:key=\"routeros-{v}\" data-changelog-version=\"{v}\" wire:ignore=\"\">\n\
             <span class=\"mtk-text-sm font-bold min-w-20\">\n {v}\n </span>\n\
             <div class=\"rounded-[4px] bg-green p-1\"> Long-term </div>\n\
             <span class=\"mtk-text-xs\">\n {date}\n </span>\n\
             <pre x-text=\"versionChangelog('{v}')\"></pre></div>\n"
        )
    }

    #[test]
    fn reads_versions_and_dates_from_the_changelog_page() {
        let html = [
            entry("7.24.6", "2026-11-02"),
            entry("7.23.8", "2026-10-01"),
            entry("7.26beta5", "2026-10-30"),
            entry("6.49.23", "2026-03-15"),
            entry("6.30.1", "2015-07-14"),
            entry("6.29", "2015-06-01"),
            entry("5.26", "2013-09-01"),
            entry("7.24.6", "2026-11-02"),
        ]
        .concat();
        let got = parse_changelogs(&html);
        let got: Vec<(String, Option<String>)> =
            got.into_iter().map(|(v, d)| (v.to_string(), d)).collect();
        let want: Vec<(String, Option<String>)> = [
            ("7.24.6", "2026-11-02"),
            ("7.23.8", "2026-10-01"),
            ("6.49.23", "2026-03-15"),
            ("6.30.1", "2015-07-14"),
        ]
        .iter()
        .map(|(v, d)| (v.to_string(), Some(d.to_string())))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_missing_date_is_none_and_never_taken_from_the_next_entry() {
        let html = format!(
            "<div data-changelog-version=\"7.25.1\"><span>7.25.1</span></div>{}",
            entry("7.25", "2026-09-09")
        );
        let got = parse_changelogs(&html);
        assert_eq!(got[0], (Version::parse("7.25.1").unwrap(), None));
        assert_eq!(got[1].1.as_deref(), Some("2026-09-09"));
    }

    #[test]
    fn hostile_markers_are_skipped() {
        for html in [
            "data-changelog-version=\"",
            "data-changelog-version=\"7.25.1",
            "data-changelog-version=\"../../etc\"",
            "data-changelog-version=\"7.25.1.1.1\"",
            "data-changelog-version=\"7.25.1 \"",
            "data-changelog-version=\"77777.1\"",
            "data-changelog-version=\"7.\"",
        ] {
            assert!(parse_changelogs(html).is_empty(), "{html:?}");
        }
        assert_eq!(first_date("x 2026-11-022 y 12026-11-02 2026-1-02"), None);
        assert_eq!(first_date("é2026-11-02é").as_deref(), Some("2026-11-02"));
    }

    /// The real pages from 2026-11-10, if present (not committed).
    #[test]
    #[ignore = "reads saved changelog pages from SCRAPE_DIR, if set"]
    fn real_pages_parse() {
        let Some(dir) = std::env::var_os("SCRAPE_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let stable =
            std::fs::read_to_string(dir.join("changelogs-stable.html")).unwrap_or_default();
        let lt = std::fs::read_to_string(dir.join("changelogs-longTerm.html")).unwrap_or_default();
        let list = merge(
            &[
                ("stable", parse_changelogs(&stable)),
                ("long-term", parse_changelogs(&lt)),
            ],
            1,
        );
        let first = &list.versions[0];
        assert_eq!(first.version.as_str(), "7.25.1");
        let v6 = list
            .versions
            .iter()
            .find(|l| l.version.as_str() == "6.49.23")
            .unwrap();
        assert_eq!(v6.channels, ["stable", "long-term"]);
        assert!(list.versions.iter().all(|l| l.date.is_some()));
        assert_eq!(list.versions.last().unwrap().version.as_str(), "6.30");
    }

    #[test]
    fn pages_merge_newest_first_with_all_tags() {
        let p = |v: &str| Version::parse(v).unwrap();
        let list = merge(
            &[
                (
                    "stable",
                    vec![
                        (p("7.25.1"), Some("2026-11-02".into())),
                        (p("6.49.23"), None),
                    ],
                ),
                (
                    "long-term",
                    vec![
                        (p("7.24.6"), None),
                        (p("6.49.23"), Some("2026-03-15".into())),
                        (p("6.48.7"), None),
                    ],
                ),
            ],
            42,
        );
        let got: Vec<(&str, Vec<&str>, Option<&str>)> = list
            .versions
            .iter()
            .map(|l| {
                (
                    l.version.as_str(),
                    l.channels.iter().map(String::as_str).collect(),
                    l.date.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("7.25.1", vec!["stable"], Some("2026-11-02")),
                ("7.24.6", vec!["long-term"], None),
                ("6.49.23", vec!["stable", "long-term"], Some("2026-03-15")),
                ("6.48.7", vec!["long-term"], None),
            ]
        );
        assert_eq!(list.fetched_unix, 42);
    }

    #[test]
    fn stored_lists_round_trip_and_damaged_ones_are_ignored() {
        let list = seed();
        assert_eq!(list.versions[0].version.as_str(), "7.25.1");
        let json = serde_json::to_vec(&list).unwrap();
        assert_eq!(parse_stored(&json), Some(list));
        assert_eq!(parse_stored(b"{oops"), None);
        assert_eq!(parse_stored(br#"{"fetched_unix":1,"versions":[]}"#), None);
        assert_eq!(
            parse_stored(
                br#"{"fetched_unix":1,"versions":[{"version":"../x","channels":[],"date":null}]}"#
            ),
            None
        );
        let odd = br#"{"fetched_unix":1,"versions":[{"version":"7.1","channels":["stable","evil"],"date":"<b>"}]}"#;
        let got = parse_stored(odd).unwrap();
        assert_eq!(got.versions[0].channels, ["stable"]);
        assert_eq!(got.versions[0].date, None);
    }

    proptest! {
        #[test]
        fn the_parser_never_panics(
            junk in "\\PC{0,400}",
            v in "[0-9.a-z\"]{0,20}",
            date in "[0-9-]{0,12}",
        ) {
            let html = format!("{junk}data-changelog-version=\"{v}\"{junk}{date}data-changelog-version=\"{junk}");
            let _ = parse_changelogs(&html);
            let _ = first_date(&html);
            let _ = parse_stored(html.as_bytes());
        }
    }
}
