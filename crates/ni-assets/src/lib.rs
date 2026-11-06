//! Boot kernels and RouterOS packages for mac-netinstall.
//!
//! Nothing from MikroTik is bundled: everything is downloaded at runtime
//! from `download.mikrotik.com`, verified and cached.

pub mod arch;
pub mod version;

pub use arch::{ArchError, PkgArch};
pub use version::{Channel, Release, Version, VersionError, VersionSpec};
