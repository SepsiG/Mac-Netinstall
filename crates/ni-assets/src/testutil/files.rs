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
