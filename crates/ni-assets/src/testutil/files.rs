//! Synthetic archives and packages.

use flate2::write::GzEncoder;
use flate2::Compression;
use ni_proto::message::NPK_MAGIC;

/// A `.tar.gz` holding `LICENSE.txt` and the given files.
pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut all = vec![("LICENSE.txt", &b"license text"[..])];
    all.extend_from_slice(files);
    for (name, data) in all {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// A stored (uncompressed) ZIP archive.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    fn u16le(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes())
    }
    fn u32le(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes())
    }
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        u32le(&mut out, 0x0403_4b50);
        for x in [20, 0, 0, 0, 0] {
            u16le(&mut out, x); // version, flags, method, time, date
        }
        u32le(&mut out, 0); // crc (not checked by our reader)
        u32le(&mut out, data.len() as u32);
        u32le(&mut out, data.len() as u32);
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        u32le(&mut central, 0x0201_4b50);
        for x in [20, 20, 0, 0, 0, 0] {
            u16le(&mut central, x);
        }
        u32le(&mut central, 0);
        u32le(&mut central, data.len() as u32);
        u32le(&mut central, data.len() as u32);
        u16le(&mut central, name.len() as u16);
        for x in [0, 0, 0, 0] {
            u16le(&mut central, x); // extra, comment, disk, internal attrs
        }
        u32le(&mut central, 0); // external attrs
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    u16le(&mut out, 0);
    u16le(&mut out, 0);
    u16le(&mut out, files.len() as u16);
    u16le(&mut out, files.len() as u16);
    u32le(&mut out, central.len() as u32);
    u32le(&mut out, cd_offset);
    u16le(&mut out, 0);
    out
}

/// A fake NPK: magic, payload length, then `fill` bytes (`len` in total).
pub fn npk(len: usize, fill: u8) -> Vec<u8> {
    assert!(len >= 8);
    let mut v = NPK_MAGIC.to_vec();
    v.extend_from_slice(&((len - 8) as u32).to_le_bytes());
    v.resize(len, fill);
    v
}

/// A synthetic NPK with the part layout of a real RouterOS package
/// (magic, payload length, then `u16 type, u32 len, data` parts): an empty
/// type-25 part, the type-1 header (`name`, NUL-padded to 16 bytes, then
/// 20 bytes of version data), the type-16 architecture `arch`, a type-21
/// filler of `fill` bytes and the trailing 1-byte type-16 part real
/// packages end with. `len` bytes in total.
pub fn npk_package(name: &str, arch: &str, len: usize, fill: u8) -> Vec<u8> {
    fn part(v: &mut Vec<u8>, kind: u16, data: &[u8]) {
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
    }
    assert!(name.len() <= 16);
    let mut header = name.as_bytes().to_vec();
    header.resize(16, 0);
    header.extend_from_slice(&[0x07, 0x19, 0x66, 0x01, 0x00, 0x6a, 0, 0, 0, 0]);
    header.resize(36, 0);
    let mut v = NPK_MAGIC.to_vec();
    v.extend_from_slice(&[0; 4]);
    part(&mut v, 25, &[]);
    part(&mut v, 1, &header);
    part(&mut v, 16, arch.as_bytes());
    let fixed = v.len() + 6 + 7;
    assert!(len >= fixed, "an NPK of {name}/{arch} needs {fixed} bytes");
    part(&mut v, 21, &vec![fill; len - fixed]);
    part(&mut v, 16, b"I");
    let size = (v.len() - 8) as u32;
    v[4..8].copy_from_slice(&size.to_le_bytes());
    v
}
