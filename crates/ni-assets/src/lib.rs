//! Boot kernels and RouterOS packages for mac-netinstall.
//!
//! Nothing from MikroTik is bundled: everything is downloaded at runtime
//! from `download.mikrotik.com`, verified and cached.

pub mod arch;
pub mod archive;
pub mod assets;
pub mod cache;
pub mod error;
pub mod events;
pub mod http;
pub mod kernel;
pub mod npk;
pub mod urls;
pub mod version;

#[cfg(test)]
mod testutil;

pub use arch::{ArchError, PkgArch};
pub use archive::ArchiveError;
pub use assets::{default_cache_root, kernel_file_name, Assets, BUNDLE_ID};
pub use cache::{Entry, Index};
pub use error::AssetError;
pub use events::{AssetEvent, EventSink, PROGRESS_STEP};
pub use http::{HttpConfig, USER_AGENT};
pub use kernel::{extract_kernels, KernelError, KernelSet};
pub use npk::NpkError;
pub use version::{Channel, Release, Version, VersionError, VersionSpec};
