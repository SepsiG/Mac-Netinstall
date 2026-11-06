//! RouterOS package architectures.
//!
//! Packages are published per RouterOS architecture (`routeros-<ver>-<arch>.npk`).
//! The device names its architecture in the Netinstall hello
//! ([`ni_proto::DeviceInfo::arch`]); the boot loader only names a
//! [`BootArch`], which is not always enough (mipsbe and smips boards both
//! boot the `Mips` kernel).

use ni_bootp::BootArch;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Architectures RouterOS packages are built for (x86 is out of scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PkgArch {
    Arm,
    Arm64,
    Mipsbe,
    Mmips,
    Smips,
    Ppc,
    Tile,
}

/// A device architecture we cannot install packages for.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArchError {
    /// Not a RouterOS architecture name we know.
    #[error("unknown device architecture {0:?}")]
    Unknown(String),
    /// A RouterOS architecture outside the MVP (x86).
    #[error("device architecture {0:?} is not supported")]
    Unsupported(String),
}

impl PkgArch {
    pub const ALL: [PkgArch; 7] = [
        PkgArch::Arm,
        PkgArch::Arm64,
        PkgArch::Mipsbe,
        PkgArch::Mmips,
        PkgArch::Smips,
        PkgArch::Ppc,
        PkgArch::Tile,
    ];

    /// The architecture as it appears in package file names.
    pub fn as_str(self) -> &'static str {
        match self {
            PkgArch::Arm => "arm",
            PkgArch::Arm64 => "arm64",
            PkgArch::Mipsbe => "mipsbe",
            PkgArch::Mmips => "mmips",
            PkgArch::Smips => "smips",
            PkgArch::Ppc => "ppc",
            PkgArch::Tile => "tile",
        }
    }

    /// Map the architecture a device reports in its Netinstall hello.
    ///
    /// Matching ignores case and surrounding whitespace. `powerpc` is
    /// accepted for `ppc`. Which exact strings real devices send is a
    /// hardware check (see the roadmap).
    pub fn from_device_arch(s: &str) -> Result<Self, ArchError> {
        let lower = s.trim().to_ascii_lowercase();
        let arch = match lower.as_str() {
            "arm" => PkgArch::Arm,
            "arm64" => PkgArch::Arm64,
            "mipsbe" => PkgArch::Mipsbe,
            "mmips" => PkgArch::Mmips,
            "smips" => PkgArch::Smips,
            "ppc" | "powerpc" => PkgArch::Ppc,
            "tile" => PkgArch::Tile,
            "x86" | "x86_64" | "i386" => return Err(ArchError::Unsupported(s.trim().into())),
            _ => return Err(ArchError::Unknown(s.trim().into())),
        };
        Ok(arch)
    }

    /// Package architectures whose devices boot the `boot` kernel.
    pub fn for_boot_arch(boot: BootArch) -> &'static [PkgArch] {
        match boot {
            BootArch::Mips => &[PkgArch::Mipsbe, PkgArch::Smips],
            BootArch::MMips => &[PkgArch::Mmips],
            BootArch::PowerPc | BootArch::E500 | BootArch::Ppc440 => &[PkgArch::Ppc],
            BootArch::Tile => &[PkgArch::Tile],
            BootArch::Arm => &[PkgArch::Arm],
            BootArch::Arm64 => &[PkgArch::Arm64],
        }
    }
}

impl fmt::Display for PkgArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PkgArch {
    type Err = ArchError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_device_arch(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_arch_strings_map_to_package_arches() {
        let cases = [
            ("arm", PkgArch::Arm),
            ("arm64", PkgArch::Arm64),
            ("mipsbe", PkgArch::Mipsbe),
            ("mmips", PkgArch::Mmips),
            ("smips", PkgArch::Smips),
            ("ppc", PkgArch::Ppc),
            ("powerpc", PkgArch::Ppc),
            ("tile", PkgArch::Tile),
            (" ARM64\n", PkgArch::Arm64),
        ];
        for (s, arch) in cases {
            assert_eq!(PkgArch::from_device_arch(s), Ok(arch), "{s:?}");
        }
    }

    #[test]
    fn x86_is_unsupported_and_others_unknown() {
        assert_eq!(
            PkgArch::from_device_arch("x86"),
            Err(ArchError::Unsupported("x86".into()))
        );
        for s in ["", "mips", "aarch64", "risc-v"] {
            assert_eq!(
                PkgArch::from_device_arch(s),
                Err(ArchError::Unknown(s.into())),
                "{s:?}"
            );
        }
    }

    #[test]
    fn names_round_trip() {
        for arch in PkgArch::ALL {
            assert_eq!(arch.as_str().parse::<PkgArch>(), Ok(arch));
            assert_eq!(arch.to_string(), arch.as_str());
        }
        assert_eq!(
            serde_json::to_string(&PkgArch::Mipsbe).unwrap(),
            r#""mipsbe""#
        );
    }

    #[test]
    fn every_boot_arch_has_package_arches() {
        for boot in BootArch::ALL {
            assert!(!PkgArch::for_boot_arch(boot).is_empty(), "{boot}");
        }
        assert_eq!(
            PkgArch::for_boot_arch(BootArch::Mips),
            &[PkgArch::Mipsbe, PkgArch::Smips]
        );
        assert_eq!(PkgArch::for_boot_arch(BootArch::E500), &[PkgArch::Ppc]);
    }
}
