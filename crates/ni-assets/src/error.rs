//! Error type with stable codes for UI translation.

use crate::arch::{ArchError, PkgArch};
use crate::archive::ArchiveError;
use crate::kernel::KernelError;
use crate::npk::NpkError;
use crate::version::{Version, VersionError};
use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Everything that can go wrong while fetching or caching assets.
#[derive(Debug, Error)]
pub enum AssetError {
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error(transparent)]
    Arch(#[from] ArchError),
    #[error("invalid package name {0:?}")]
    InvalidPackageName(String),
    #[error("package {name} is not available for {arch}")]
    UnknownPackage { name: String, arch: PkgArch },
    /// The main package of `version` is not published for `arch` (404):
    /// old v6 releases lack newer architectures (6.30.4 has no arm or
    /// mmips; arm64 starts around 6.45).
    #[error("RouterOS {version} is not published for {arch} ({file})")]
    PackageNotPublished {
        file: String,
        arch: PkgArch,
        version: Version,
    },
    #[error("cannot set up the HTTP client")]
    Client(#[source] reqwest::Error),
    #[error("{url} was not found")]
    NotFound { url: String },
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("cannot download {url}")]
    Network {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} timed out")]
    Timeout { url: String },
    #[error("{url} redirected to a URL that is not allowed (https downgrade or another host)")]
    RedirectRefused { url: String },
    #[error("{url} ended after {got} of {expected} bytes")]
    Truncated {
        url: String,
        expected: u64,
        got: u64,
    },
    #[error("{url} is larger than the {max} byte limit")]
    TooLarge { url: String, max: u64 },
    #[error("unexpected response from {url}: {reason}")]
    BadResponse { url: String, reason: &'static str },
    #[error("{file} has SHA-256 {actual}, MikroTik publishes {expected}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// The bytes read back from disk differ from the bytes received.
    #[error("{file} changed on disk while it was being verified")]
    DiskMismatch { file: String },
    /// [`crate::Assets::delete_cached`]: not an item of this cache.
    #[error("{id} is not a cached item")]
    InvalidItem { id: String },
    /// [`crate::Assets::delete_cached`]: it is being downloaded right now.
    #[error("{id} is being written right now")]
    Busy { id: String },
    /// The runtime shut down while file work was pending.
    #[error("the operation was cancelled")]
    Cancelled,
    /// MikroTik publishes checksums from 7.15 on, but this one is missing.
    #[error("{url} is missing; MikroTik publishes checksums for this version")]
    ChecksumMissing { url: String },
    #[error(transparent)]
    Npk(#[from] NpkError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Kernel(#[from] KernelError),
    #[error("cannot {op} {}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{name:?} is not a supported local file (.npk or .rsc)")]
    UnsupportedLocalFile { name: String },
    #[error("{name:?} is larger than the {max} byte limit")]
    LocalFileTooLarge { name: String, max: u64 },
    #[error("{name:?} is not a regular file")]
    LocalNotAFile { name: String },
    #[error("local file {name:?} is not registered")]
    LocalMissing { name: String },
    #[error("local file {name:?} changed in the cache; register it again")]
    LocalCorrupt { name: String },
}

impl AssetError {
    /// Every code [`AssetError::code`] can return, so a UI can check that
    /// each one has a translation.
    pub const CODES: &'static [&'static str] = &[
        "invalid_version",
        "version_too_old",
        "unknown_arch",
        "unsupported_arch",
        "invalid_package_name",
        "unknown_package",
        "package_not_published",
        "http_client",
        "not_found",
        "http_status",
        "network",
        "timeout",
        "redirect_refused",
        "truncated",
        "too_large",
        "bad_response",
        "checksum_mismatch",
        "checksum_missing",
        "disk_mismatch",
        "cancelled",
        "not_npk",
        "npk_length",
        "npk_malformed",
        "archive_missing_cli",
        "archive",
        "kernel_missing",
        "kernel_duplicate",
        "io",
        "unsupported_local_file",
        "local_too_large",
        "local_not_file",
        "local_missing",
        "local_corrupt",
        "invalid_item",
        "busy",
    ];

    /// Stable machine-readable code (i18n key).
    pub fn code(&self) -> &'static str {
        match self {
            AssetError::Version(VersionError::Invalid(_)) => "invalid_version",
            AssetError::Version(VersionError::TooOld(_)) => "version_too_old",
            AssetError::Arch(ArchError::Unknown(_)) => "unknown_arch",
            AssetError::Arch(ArchError::Unsupported(_)) => "unsupported_arch",
            AssetError::InvalidPackageName(_) => "invalid_package_name",
            AssetError::UnknownPackage { .. } => "unknown_package",
            AssetError::PackageNotPublished { .. } => "package_not_published",
            AssetError::Client(_) => "http_client",
            AssetError::NotFound { .. } => "not_found",
            AssetError::Status { .. } => "http_status",
            AssetError::Network { .. } => "network",
            AssetError::Timeout { .. } => "timeout",
            AssetError::RedirectRefused { .. } => "redirect_refused",
            AssetError::Truncated { .. } => "truncated",
            AssetError::TooLarge { .. } => "too_large",
            AssetError::BadResponse { .. } => "bad_response",
            AssetError::ChecksumMismatch { .. } => "checksum_mismatch",
            AssetError::ChecksumMissing { .. } => "checksum_missing",
            AssetError::DiskMismatch { .. } => "disk_mismatch",
            AssetError::Cancelled => "cancelled",
            AssetError::Npk(NpkError::NotAnNpk { .. }) => "not_npk",
            AssetError::Npk(NpkError::LengthMismatch { .. }) => "npk_length",
            AssetError::Npk(NpkError::Malformed { .. }) => "npk_malformed",
            AssetError::Archive(ArchiveError::MissingCli) => "archive_missing_cli",
            AssetError::Archive(_) => "archive",
            AssetError::Kernel(KernelError::Missing(_)) => "kernel_missing",
            AssetError::Kernel(KernelError::Duplicate(_)) => "kernel_duplicate",
            AssetError::Io { .. } => "io",
            AssetError::UnsupportedLocalFile { .. } => "unsupported_local_file",
            AssetError::LocalFileTooLarge { .. } => "local_too_large",
            AssetError::LocalNotAFile { .. } => "local_not_file",
            AssetError::LocalMissing { .. } => "local_missing",
            AssetError::LocalCorrupt { .. } => "local_corrupt",
            AssetError::InvalidItem { .. } => "invalid_item",
            AssetError::Busy { .. } => "busy",
        }
    }

    pub(crate) fn io(op: &'static str, path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| AssetError::Io { op, path, source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_bootp::BootArch;

    #[test]
    fn codes_are_stable_and_unique() {
        let errors = [
            AssetError::Version(VersionError::Invalid("x".into())),
            AssetError::Version(VersionError::TooOld("6.49".into())),
            AssetError::Arch(ArchError::Unknown("x".into())),
            AssetError::Arch(ArchError::Unsupported("x86".into())),
            AssetError::InvalidPackageName("A".into()),
            AssetError::UnknownPackage {
                name: "x".into(),
                arch: PkgArch::Arm,
            },
            AssetError::PackageNotPublished {
                file: "f".into(),
                arch: PkgArch::Arm,
                version: Version::parse("6.30.4").unwrap(),
            },
            AssetError::NotFound { url: "u".into() },
            AssetError::Status {
                url: "u".into(),
                status: 500,
            },
            AssetError::Timeout { url: "u".into() },
            AssetError::RedirectRefused { url: "u".into() },
            AssetError::Truncated {
                url: "u".into(),
                expected: 2,
                got: 1,
            },
            AssetError::TooLarge {
                url: "u".into(),
                max: 1,
            },
            AssetError::BadResponse {
                url: "u".into(),
                reason: "r",
            },
            AssetError::ChecksumMismatch {
                file: "f".into(),
                expected: "a".into(),
                actual: "b".into(),
            },
            AssetError::ChecksumMissing { url: "u".into() },
            AssetError::DiskMismatch { file: "f".into() },
            AssetError::Cancelled,
            AssetError::Npk(NpkError::NotAnNpk { name: "n".into() }),
            AssetError::Npk(NpkError::LengthMismatch {
                name: "n".into(),
                expected: 1,
                actual: 2,
            }),
            AssetError::Npk(NpkError::Malformed { reason: "r" }),
            AssetError::Archive(ArchiveError::MissingCli),
            AssetError::Archive(ArchiveError::BadZip("x")),
            AssetError::Kernel(KernelError::Missing(vec![BootArch::Tile])),
            AssetError::Kernel(KernelError::Duplicate(BootArch::Arm)),
            AssetError::Io {
                op: "read",
                path: "/x".into(),
                source: io::ErrorKind::NotFound.into(),
            },
            AssetError::UnsupportedLocalFile {
                name: "a.txt".into(),
            },
            AssetError::LocalFileTooLarge {
                name: "a.rsc".into(),
                max: 1,
            },
            AssetError::LocalNotAFile { name: "a".into() },
            AssetError::LocalMissing { name: "a".into() },
            AssetError::LocalCorrupt { name: "a".into() },
            AssetError::InvalidItem { id: "x".into() },
            AssetError::Busy { id: "x".into() },
        ];
        let codes: Vec<&str> = errors.iter().map(AssetError::code).collect();
        assert_eq!(
            codes,
            [
                "invalid_version",
                "version_too_old",
                "unknown_arch",
                "unsupported_arch",
                "invalid_package_name",
                "unknown_package",
                "package_not_published",
                "not_found",
                "http_status",
                "timeout",
                "redirect_refused",
                "truncated",
                "too_large",
                "bad_response",
                "checksum_mismatch",
                "checksum_missing",
                "disk_mismatch",
                "cancelled",
                "not_npk",
                "npk_length",
                "npk_malformed",
                "archive_missing_cli",
                "archive",
                "kernel_missing",
                "kernel_duplicate",
                "io",
                "unsupported_local_file",
                "local_too_large",
                "local_not_file",
                "local_missing",
                "local_corrupt",
                "invalid_item",
                "busy",
            ]
        );
        let mut unique = codes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
        for code in &codes {
            assert!(
                AssetError::CODES.contains(code),
                "{code} missing from CODES"
            );
        }
    }

    #[test]
    fn codes_list_is_unique_and_complete() {
        let mut all = AssetError::CODES.to_vec();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), AssetError::CODES.len());
        // The two codes the test above cannot construct (they wrap
        // `reqwest::Error`), plus the 33 it checks.
        assert!(AssetError::CODES.contains(&"http_client"));
        assert!(AssetError::CODES.contains(&"network"));
        assert_eq!(AssetError::CODES.len(), 35);
    }

    #[test]
    fn messages_name_the_problem() {
        let e = AssetError::Io {
            op: "write",
            path: "/cache/x".into(),
            source: io::ErrorKind::PermissionDenied.into(),
        };
        assert_eq!(e.to_string(), "cannot write /cache/x");
        let e = AssetError::Kernel(KernelError::Missing(vec![BootArch::Tile]));
        assert_eq!(e.to_string(), "no boot kernel found for: tile");
    }
}
