//! Synthetic ELF images shaped like the embedded boot kernels.

use ni_bootp::BootArch;

/// Shape of a synthetic ELF file.
#[derive(Debug, Clone, Copy)]
pub struct ElfSpec {
    pub class64: bool,
    pub big_endian: bool,
    pub machine: u16,
    pub e_type: u16,
    /// Append a one-entry section header table after the payload.
    pub section_headers: bool,
}

impl ElfSpec {
    pub fn exec(class64: bool, big_endian: bool, machine: u16) -> Self {
        Self {
            class64,
            big_endian,
            machine,
            e_type: 2,
            section_headers: true,
        }
    }
}

struct Out {
    buf: Vec<u8>,
    big: bool,
}

impl Out {
    fn u16(&mut self, v: u16) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }
    fn u32(&mut self, v: u32) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }
    fn u64(&mut self, v: u64) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }
    /// Address-sized field.
    fn addr(&mut self, class64: bool, v: u64) {
        if class64 {
            self.u64(v)
        } else {
            self.u32(v as u32)
        }
    }
}

/// A minimal valid ELF: header, one `PT_LOAD` program header covering
/// `payload`, the payload, then (optionally) a zeroed section header table.
pub fn elf(spec: ElfSpec, payload: &[u8]) -> Vec<u8> {
    let c64 = spec.class64;
    let (ehsize, phentsize, shentsize) = if c64 { (64, 56, 64) } else { (52, 32, 40) };
    let data_off = u64::from(ehsize + phentsize);
    let shoff = data_off + payload.len() as u64;
    let mut o = Out {
        buf: Vec::new(),
        big: spec.big_endian,
    };
    o.buf.extend_from_slice(b"\x7fELF");
    o.buf.push(if c64 { 2 } else { 1 });
    o.buf.push(if spec.big_endian { 2 } else { 1 });
    o.buf.push(1);
    o.buf.resize(16, 0);
    o.u16(spec.e_type);
    o.u16(spec.machine);
    o.u32(1); // e_version
    o.addr(c64, 0x8000_0000); // e_entry
    o.addr(c64, u64::from(ehsize)); // e_phoff
    o.addr(c64, if spec.section_headers { shoff } else { 0 }); // e_shoff
    o.u32(0); // e_flags
    o.u16(ehsize);
    o.u16(phentsize);
    o.u16(1); // e_phnum
    o.u16(shentsize);
    o.u16(u16::from(spec.section_headers)); // e_shnum
    o.u16(0); // e_shstrndx
    assert_eq!(o.buf.len(), usize::from(ehsize));
    // Program header: PT_LOAD.
    o.u32(1);
    if c64 {
        o.u32(5); // p_flags
        o.u64(data_off);
        o.u64(0x8000_0000);
        o.u64(0x8000_0000);
        o.u64(payload.len() as u64);
        o.u64(payload.len() as u64);
        o.u64(0x1000);
    } else {
        o.u32(data_off as u32);
        o.u32(0x8000_0000);
        o.u32(0x8000_0000);
        o.u32(payload.len() as u32);
        o.u32(payload.len() as u32);
        o.u32(5);
        o.u32(0x1000);
    }
    o.buf.extend_from_slice(payload);
    if spec.section_headers {
        o.buf.resize(o.buf.len() + usize::from(shentsize), 0);
    }
    o.buf
}

/// One synthetic kernel per [`BootArch`], in [`BootArch::ALL`] order. The
/// MIPS kernel contains a vDSO (`e_type == 3`) like real kernels do.
pub fn synthetic_kernels() -> Vec<(BootArch, Vec<u8>)> {
    let banner = |release: &str, filler: u8| {
        let mut p = vec![filler; 300];
        p.extend_from_slice(format!("Linux version {release} (b@h) (gcc) #1\n").as_bytes());
        p.extend_from_slice(&[filler; 200]);
        p
    };
    let vdso = elf(
        ElfSpec {
            e_type: 3,
            ..ElfSpec::exec(false, true, 8)
        },
        b"vdso",
    );
    let mut mips = banner("5.6.3", 0x11);
    mips.extend_from_slice(&vdso);
    vec![
        (BootArch::Mips, elf(ElfSpec::exec(false, true, 8), &mips)),
        (
            BootArch::MMips,
            elf(ElfSpec::exec(false, false, 8), &banner("5.6.3", 0x22)),
        ),
        (
            BootArch::PowerPc,
            elf(ElfSpec::exec(false, true, 20), &banner("5.6.3", 0x33)),
        ),
        (
            BootArch::E500,
            elf(ElfSpec::exec(false, true, 20), &banner("5.6.3-e500", 0x44)),
        ),
        (
            BootArch::Ppc440,
            elf(ElfSpec::exec(false, true, 20), &banner("5.6.3-440", 0x55)),
        ),
        (
            BootArch::Tile,
            elf(ElfSpec::exec(true, false, 191), &[0x66; 700]),
        ),
        (
            BootArch::Arm,
            elf(ElfSpec::exec(false, false, 40), &[0x77; 600]),
        ),
        (
            BootArch::Arm64,
            elf(ElfSpec::exec(true, false, 183), &[0x88; 800]),
        ),
    ]
}

/// A stand-in for `netinstall-cli`: an i386 executable whose payload holds
/// some junk followed by the kernels, back to back.
pub fn kernel_container(kernels: &[(BootArch, Vec<u8>)]) -> Vec<u8> {
    let mut payload = b"\x7fELF but not really, some host code".to_vec();
    for (_, k) in kernels {
        payload.extend_from_slice(k);
    }
    payload.extend_from_slice(b"trailing host data");
    elf(ElfSpec::exec(false, false, 3), &payload)
}
