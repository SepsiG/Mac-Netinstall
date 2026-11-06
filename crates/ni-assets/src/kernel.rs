//! Boot kernel extraction from the official `netinstall-cli` binary.
//!
//! MikroTik's Linux `netinstall-cli` (an i386 ELF) carries the network-boot
//! kernels as plain ELF images appended to its data. Each image is carved
//! out by scanning for the ELF magic, and its exact size is computed from
//! its headers: the end of the furthest program segment or of the section
//! header table. Only executables (`e_type == 2`) count; the small shared
//! objects (`e_type == 3`) found inside kernels are vDSOs.
//!
//! Mapping to [`BootArch`] (verified on 7.25.1):
//!
//! | ELF machine | Class, byte order | Extra marker | Arch |
//! |---|---|---|---|
//! | 8 (MIPS) | 32-bit big-endian | | [`BootArch::Mips`] |
//! | 8 (MIPS) | 32-bit little-endian | | [`BootArch::MMips`] |
//! | 20 (PPC) | 32-bit big-endian | `Linux version 5.6.3 ` (no suffix) | [`BootArch::PowerPc`] |
//! | 20 (PPC) | 32-bit big-endian | `Linux version …-e500` | [`BootArch::E500`] |
//! | 20 (PPC) | 32-bit big-endian | `Linux version …-440` | [`BootArch::Ppc440`] |
//! | 40 (ARM) | 32-bit little-endian | | [`BootArch::Arm`] |
//! | 183 (AArch64) | 64-bit little-endian | | [`BootArch::Arm64`] |
//! | 191 (TILE-Gx) | 64-bit little-endian | | [`BootArch::Tile`] |

use bytes::Bytes;
use ni_bootp::BootArch;
use std::collections::HashMap;
use thiserror::Error;

/// One boot kernel per architecture.
pub type KernelSet = HashMap<BootArch, Bytes>;

/// Upper bound for one kernel image (real ones are 3.5–8 MiB).
pub const MAX_KERNEL_SIZE: u64 = 32 << 20;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ET_EXEC: u16 = 2;
const EM_MIPS: u16 = 8;
const EM_PPC: u16 = 20;
const EM_ARM: u16 = 40;
const EM_AARCH64: u16 = 183;
const EM_TILEGX: u16 = 191;
const LINUX_VERSION: &[u8] = b"Linux version ";

/// Why a kernel set could not be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KernelError {
    /// No image was found for these architectures (in [`BootArch::ALL`] order).
    #[error("no boot kernel found for: {}", names(.0))]
    Missing(Vec<BootArch>),
    /// Two images map to the same architecture; we cannot tell which to serve.
    #[error("more than one boot kernel found for {0}")]
    Duplicate(BootArch),
}

fn names(arches: &[BootArch]) -> String {
    let v: Vec<&str> = arches.iter().map(|a| a.as_str()).collect();
    v.join(", ")
}

/// An ELF executable found inside a larger blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElfImage {
    /// Offset of the ELF magic in the blob.
    pub offset: usize,
    /// Image size computed from its headers.
    pub len: usize,
    /// `e_machine`.
    pub machine: u16,
    /// `ELFCLASS64`.
    pub class64: bool,
    /// `ELFDATA2MSB`.
    pub big_endian: bool,
}

/// Every well-formed ELF executable (`e_type == 2`) in `data` whose computed
/// size fits in `data` and in [`MAX_KERNEL_SIZE`], in offset order. Images
/// nested in other images are listed too.
pub fn find_elf_images(data: &[u8]) -> Vec<ElfImage> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(pos) = find(&data[from..], ELF_MAGIC) {
        let offset = from + pos;
        if let Some(img) = parse_elf(data, offset) {
            found.push(img);
        }
        from = offset + 1;
    }
    found
}

/// Carve the boot kernels for every [`BootArch`] out of `data` (the
/// `netinstall-cli` binary).
///
/// Images starting inside an already accepted kernel are ignored, and
/// executables for other machines (the i386 host program itself) are
/// skipped. Fails if an architecture is missing or found twice.
pub fn extract_kernels(data: &[u8]) -> Result<KernelSet, KernelError> {
    let mut set = KernelSet::new();
    let mut covered = 0;
    for img in find_elf_images(data) {
        if img.offset < covered {
            continue;
        }
        let bytes = &data[img.offset..img.offset + img.len];
        let Some(arch) = classify(&img, bytes) else {
            continue;
        };
        if set.insert(arch, Bytes::copy_from_slice(bytes)).is_some() {
            return Err(KernelError::Duplicate(arch));
        }
        covered = img.offset + img.len;
    }
    let missing: Vec<BootArch> = BootArch::ALL
        .into_iter()
        .filter(|a| !set.contains_key(a))
        .collect();
    if missing.is_empty() {
        Ok(set)
    } else {
        Err(KernelError::Missing(missing))
    }
}

/// Map an image to its boot architecture; `None` if it is not a kernel we
/// know.
pub fn classify(img: &ElfImage, bytes: &[u8]) -> Option<BootArch> {
    match (img.machine, img.class64, img.big_endian) {
        (EM_MIPS, false, true) => Some(BootArch::Mips),
        (EM_MIPS, false, false) => Some(BootArch::MMips),
        (EM_PPC, false, true) => {
            let version = linux_version(bytes)?;
            if version.ends_with("-e500") {
                Some(BootArch::E500)
            } else if version.ends_with("-440") {
                Some(BootArch::Ppc440)
            } else if !version.contains('-') {
                Some(BootArch::PowerPc)
            } else {
                None
            }
        }
        (EM_ARM, false, false) => Some(BootArch::Arm),
        (EM_AARCH64, true, false) => Some(BootArch::Arm64),
        (EM_TILEGX, true, false) => Some(BootArch::Tile),
        _ => None,
    }
}

/// The kernel release from the first `Linux version <release> ` banner whose
/// release starts with a digit (e.g. `5.6.3-e500`).
pub fn linux_version(bytes: &[u8]) -> Option<&str> {
    let mut from = 0;
    while let Some(pos) = find(&bytes[from..], LINUX_VERSION) {
        let start = from + pos + LINUX_VERSION.len();
        let rest = &bytes[start..];
        let len = rest
            .iter()
            .take(64)
            .take_while(|b| b.is_ascii_graphic())
            .count();
        let release = &rest[..len];
        if release.first().is_some_and(u8::is_ascii_digit) {
            return std::str::from_utf8(release).ok();
        }
        from = start;
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = needle[0];
    let mut from = 0;
    while let Some(pos) = haystack[from..].iter().position(|&b| b == first) {
        let at = from + pos;
        if haystack[at..].starts_with(needle) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// Endian-aware reads from a slice; `None` when out of bounds.
struct Reader<'a> {
    d: &'a [u8],
    big: bool,
}

impl Reader<'_> {
    fn bytes<const N: usize>(&self, at: u64) -> Option<[u8; N]> {
        let at = usize::try_from(at).ok()?;
        self.d.get(at..at.checked_add(N)?)?.try_into().ok()
    }

    fn u16(&self, at: u64) -> Option<u64> {
        let b = self.bytes::<2>(at)?;
        Some(u64::from(if self.big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }))
    }

    fn u32(&self, at: u64) -> Option<u64> {
        let b = self.bytes::<4>(at)?;
        Some(u64::from(if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }))
    }

    fn u64(&self, at: u64) -> Option<u64> {
        let b = self.bytes::<8>(at)?;
        Some(if self.big {
            u64::from_be_bytes(b)
        } else {
            u64::from_le_bytes(b)
        })
    }
}

/// Parse the ELF headers at `offset`; `None` unless it is a well-formed
/// executable that fits in `data`.
fn parse_elf(data: &[u8], offset: usize) -> Option<ElfImage> {
    let d = &data[offset..];
    if !d.starts_with(ELF_MAGIC) {
        return None;
    }
    let class64 = match *d.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let big = match *d.get(5)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    if *d.get(6)? != 1 {
        return None;
    }
    let r = Reader { d, big };
    if r.u16(16)? != u64::from(ET_EXEC) {
        return None;
    }
    let machine = u16::try_from(r.u16(18)?).ok()?;
    let (phoff, shoff, ehsize, phentsize, phnum, shentsize, shnum) = if class64 {
        let h = |at| r.u16(at);
        (
            r.u64(32)?,
            r.u64(40)?,
            h(52)?,
            h(54)?,
            h(56)?,
            h(58)?,
            h(60)?,
        )
    } else {
        let h = |at| r.u16(at);
        (
            r.u32(28)?,
            r.u32(32)?,
            h(40)?,
            h(42)?,
            h(44)?,
            h(46)?,
            h(48)?,
        )
    };
    let (want_ph, want_sh) = if class64 { (56, 64) } else { (32, 40) };
    if phnum == 0 || phentsize != want_ph || (shnum != 0 && shentsize != want_sh) {
        return None;
    }
    let mut end = ehsize
        .max(phoff.checked_add(phnum * phentsize)?)
        .max(shoff.checked_add(shnum * shentsize)?);
    for k in 0..phnum {
        let ph = phoff + k * phentsize;
        let (p_offset, p_filesz) = if class64 {
            (r.u64(ph + 8)?, r.u64(ph + 32)?)
        } else {
            (r.u32(ph + 4)?, r.u32(ph + 16)?)
        };
        end = end.max(p_offset.checked_add(p_filesz)?);
    }
    if end > MAX_KERNEL_SIZE || end > d.len() as u64 {
        return None;
    }
    Some(ElfImage {
        offset,
        len: end as usize,
        machine,
        class64,
        big_endian: big,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{elf, kernel_container, synthetic_kernels, ElfSpec};
    use proptest::prelude::*;

    #[test]
    fn extracts_all_eight_kernels_from_a_synthetic_container() {
        let kernels = synthetic_kernels();
        let blob = kernel_container(&kernels);
        let set = extract_kernels(&blob).unwrap();
        assert_eq!(set.len(), 8);
        for (arch, image) in &kernels {
            assert_eq!(set[arch].as_ref(), image.as_slice(), "{arch}");
        }
    }

    #[test]
    fn powerpc_flavour_comes_from_the_linux_version_suffix() {
        let ppc = |banner: &str| {
            let payload = format!("junk\0Linux version {banner} (builder@host) #1\0more");
            let image = elf(ElfSpec::exec(false, true, EM_PPC), payload.as_bytes());
            let img = find_elf_images(&image)[0];
            classify(&img, &image)
        };
        assert_eq!(ppc("5.6.3"), Some(BootArch::PowerPc));
        assert_eq!(ppc("5.6.3-e500"), Some(BootArch::E500));
        assert_eq!(ppc("5.6.3-440"), Some(BootArch::Ppc440));
        assert_eq!(ppc("5.6.3-e6500"), None, "unknown flavour stays unknown");
        let bare = elf(ElfSpec::exec(false, true, EM_PPC), b"no banner here");
        assert_eq!(classify(&find_elf_images(&bare)[0], &bare), None);
    }

    #[test]
    fn linux_version_skips_banners_without_a_release_number() {
        let bytes = b"Linux version %s\0...Linux version 5.6.3-440 (x)";
        assert_eq!(linux_version(bytes), Some("5.6.3-440"));
        assert_eq!(linux_version(b"Linux version "), None);
    }

    #[test]
    fn size_covers_segments_and_the_section_header_table() {
        // Section headers after the payload: they end the image.
        let with_sh = elf(ElfSpec::exec(false, false, EM_ARM), &[1; 100]);
        let img = find_elf_images(&with_sh)[0];
        assert_eq!(img.len, with_sh.len());
        // No section headers: the segment ends the image.
        let spec = ElfSpec {
            section_headers: false,
            ..ElfSpec::exec(true, false, EM_AARCH64)
        };
        let no_sh = elf(spec, &[2; 100]);
        let mut blob = no_sh.clone();
        blob.extend_from_slice(&[0xee; 50]);
        let img = find_elf_images(&blob)[0];
        assert_eq!(img.len, no_sh.len());
        assert_eq!(
            (img.machine, img.class64, img.big_endian),
            (183, true, false)
        );
    }

    #[test]
    fn shared_objects_and_foreign_machines_are_not_kernels() {
        let vdso = elf(
            ElfSpec {
                e_type: 3,
                ..ElfSpec::exec(false, true, EM_MIPS)
            },
            b"vdso",
        );
        assert!(find_elf_images(&vdso).is_empty());
        let host = elf(ElfSpec::exec(false, false, 3), b"i386 program");
        let img = find_elf_images(&host)[0];
        assert_eq!(classify(&img, &host), None);
        let mips64 = elf(ElfSpec::exec(true, true, EM_MIPS), b"64-bit mips");
        assert_eq!(classify(&find_elf_images(&mips64)[0], &mips64), None);
    }

    #[test]
    fn executables_nested_in_a_kernel_are_ignored() {
        let mut kernels = synthetic_kernels();
        let inner = elf(ElfSpec::exec(false, false, EM_ARM), b"nested arm program");
        let arm64 = kernels
            .iter_mut()
            .find(|(a, _)| *a == BootArch::Arm64)
            .unwrap();
        let mut payload = b"kernel text".to_vec();
        payload.extend_from_slice(&inner);
        arm64.1 = elf(ElfSpec::exec(true, false, EM_AARCH64), &payload);
        let set = extract_kernels(&kernel_container(&kernels)).unwrap();
        assert_eq!(set[&BootArch::Arm64].as_ref(), kernels[7].1.as_slice());
        assert_eq!(set[&BootArch::Arm].as_ref(), kernels[6].1.as_slice());
    }

    #[test]
    fn missing_architectures_are_listed_in_order() {
        let kernels: Vec<_> = synthetic_kernels()
            .into_iter()
            .filter(|(a, _)| !matches!(a, BootArch::Tile | BootArch::E500))
            .collect();
        let err = extract_kernels(&kernel_container(&kernels)).unwrap_err();
        assert_eq!(
            err,
            KernelError::Missing(vec![BootArch::E500, BootArch::Tile])
        );
        assert_eq!(err.to_string(), "no boot kernel found for: e500, tile");
    }

    #[test]
    fn a_second_kernel_for_the_same_arch_is_an_error() {
        let mut kernels = synthetic_kernels();
        let extra = elf(ElfSpec::exec(false, false, EM_ARM), b"another arm kernel");
        kernels.push((BootArch::Arm, extra));
        assert_eq!(
            extract_kernels(&kernel_container(&kernels)),
            Err(KernelError::Duplicate(BootArch::Arm))
        );
    }

    #[test]
    fn truncated_images_are_skipped() {
        let image = elf(ElfSpec::exec(false, false, EM_ARM), &[0; 64]);
        assert!(find_elf_images(&image[..image.len() - 1]).is_empty());
        assert!(find_elf_images(&image[..30]).is_empty());
    }

    proptest! {
        #[test]
        fn never_panics_on_arbitrary_bytes(
            mut data in proptest::collection::vec(any::<u8>(), 0..512),
            at in 0usize..512,
        ) {
            // Plant a magic so header parsing is exercised too.
            if at + 4 <= data.len() {
                data[at..at + 4].copy_from_slice(ELF_MAGIC);
            }
            let _ = extract_kernels(&data);
        }

        #[test]
        fn never_panics_on_corrupted_headers(idx in 0usize..200, byte in any::<u8>()) {
            let mut image = elf(ElfSpec::exec(true, false, EM_AARCH64), &[7; 64]);
            let i = idx % image.len();
            image[i] = byte;
            let _ = find_elf_images(&image);
        }
    }
}
