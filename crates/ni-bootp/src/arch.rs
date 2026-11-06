//! RouterBOOT architectures, identified by the DHCP vendor class (option 60).
//!
//! [`BootArch`] is the shared architecture identity for later crates
//! (`ni-assets` picks the boot kernel by it).

use std::fmt;

/// Architectures whose RouterBOOT can network-boot a Netinstall kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BootArch {
    /// `Mips_boot`: MIPS big-endian (mipsbe).
    Mips,
    /// `MMipsBoot`: MIPS little-endian, MT7621 (mmips).
    MMips,
    /// `Powerboot`: generic PowerPC (e300 / mpc83xx).
    PowerPc,
    /// `e500_boot`, `e500sboot`: PowerPC e500.
    E500,
    /// `440__boot`: PowerPC 440.
    Ppc440,
    /// `tile_boot`: Tilera.
    Tile,
    /// `ARM__boot`: 32-bit ARM.
    Arm,
    /// `ARM64__boot`: AArch64.
    Arm64,
}

impl BootArch {
    pub const ALL: [BootArch; 8] = [
        BootArch::Mips,
        BootArch::MMips,
        BootArch::PowerPc,
        BootArch::E500,
        BootArch::Ppc440,
        BootArch::Tile,
        BootArch::Arm,
        BootArch::Arm64,
    ];

    /// Stable lowercase identifier for logs, events and file names.
    pub fn as_str(self) -> &'static str {
        match self {
            BootArch::Mips => "mips",
            BootArch::MMips => "mmips",
            BootArch::PowerPc => "powerpc",
            BootArch::E500 => "e500",
            BootArch::Ppc440 => "ppc440",
            BootArch::Tile => "tile",
            BootArch::Arm => "arm",
            BootArch::Arm64 => "arm64",
        }
    }

    /// Classify a DHCP vendor class string (exact, case-sensitive match).
    pub fn from_vendor_class(class: &str) -> VendorClass {
        let arch = match class {
            "Mips_boot" => BootArch::Mips,
            "MMipsBoot" => BootArch::MMips,
            "Powerboot" => BootArch::PowerPc,
            "e500_boot" | "e500sboot" => BootArch::E500,
            "440__boot" => BootArch::Ppc440,
            "tile_boot" => BootArch::Tile,
            "ARM__boot" => BootArch::Arm,
            "ARM64__boot" => BootArch::Arm64,
            "X86_64__boot" | "ARM64UEFI__boot" => {
                return VendorClass::Unsupported(class.to_string())
            }
            c if c.starts_with("PXEClient") => return VendorClass::Unsupported(class.to_string()),
            _ => return VendorClass::Unknown(class.to_string()),
        };
        VendorClass::Supported(arch)
    }
}

impl fmt::Display for BootArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of classifying a vendor class string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendorClass {
    /// A RouterBOOT architecture we can boot.
    Supported(BootArch),
    /// A MikroTik boot loader outside the MVP (x86 PXE, ARM64 UEFI).
    Unsupported(String),
    /// Not a known MikroTik vendor class (another DHCP client on the wire).
    Unknown(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_routerboot_vendor_class() {
        let cases = [
            ("Mips_boot", BootArch::Mips),
            ("MMipsBoot", BootArch::MMips),
            ("Powerboot", BootArch::PowerPc),
            ("e500_boot", BootArch::E500),
            ("e500sboot", BootArch::E500),
            ("440__boot", BootArch::Ppc440),
            ("tile_boot", BootArch::Tile),
            ("ARM__boot", BootArch::Arm),
            ("ARM64__boot", BootArch::Arm64),
        ];
        for (class, arch) in cases {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Supported(arch),
                "{class}"
            );
        }
    }

    #[test]
    fn x86_and_uefi_are_unsupported() {
        for class in [
            "X86_64__boot",
            "ARM64UEFI__boot",
            "PXEClient:Arch:00000:UNDI:002001",
        ] {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Unsupported(class.to_string())
            );
        }
    }

    #[test]
    fn other_classes_are_unknown_and_matching_is_case_sensitive() {
        for class in ["MSFT 5.0", "android-dhcp-14", "mips_boot", ""] {
            assert_eq!(
                BootArch::from_vendor_class(class),
                VendorClass::Unknown(class.to_string())
            );
        }
    }

    #[test]
    fn identifiers_are_unique() {
        let mut names: Vec<&str> = BootArch::ALL.iter().map(|a| a.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BootArch::ALL.len());
        assert_eq!(BootArch::Arm64.to_string(), "arm64");
    }
}
