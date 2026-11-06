//! Boot kernels and RouterOS packages for mac-netinstall.
//!
//! Nothing from MikroTik is bundled: everything is downloaded at runtime
//! from `download.mikrotik.com` (see [`urls`]), verified and cached.
//!
//! - [`Assets::latest`] / [`Assets::resolve`]: channel → version.
//! - [`Assets::ensure_kernels`]: boot kernels per [`ni_bootp::BootArch`],
//!   carved out of the official `netinstall-cli` ([`extract_kernels`]).
//! - [`Assets::available_packages`] / [`Assets::ensure_packages`]: `.npk`
//!   files per [`PkgArch`], ready for [`ni_proto::InstallPlan`].
//! - [`Assets::register_local_file`]: user-supplied `.npk` / `.rsc`.
//!
//! Errors carry stable codes ([`AssetError::code`]) for UI translation.

pub mod arch;
pub mod archive;
pub mod assets;
pub mod cache;
pub mod error;
pub mod events;
pub mod http;
pub mod kernel;
mod lock;
pub mod npk;
pub mod urls;
pub mod version;

/// Test fixtures (synthetic kernels, NPKs, a fake MikroTik mirror over a
/// local HTTP server, self-cleaning temp dirs). Not a stable API: enabled
/// for this crate's tests and, through the `testing` feature, for the
/// engine's end-to-end tests.
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testutil;

pub use arch::{ArchError, PkgArch};
pub use archive::ArchiveError;
pub use assets::{
    default_cache_root, kernel_file_name, Assets, LocalFile, LocalKind, BUNDLE_ID, MAX_LOCAL_NAME,
    MAX_SCRIPT_SIZE, STALE_PART_AGE,
};
pub use cache::{Entry, Index};
pub use error::AssetError;
pub use events::{AssetEvent, EventSink, PROGRESS_STEP};
pub use http::{HttpConfig, USER_AGENT};
pub use kernel::{extract_kernels, KernelError, KernelSet};
pub use npk::NpkError;
pub use version::{Channel, Release, Version, VersionError, VersionSpec};
