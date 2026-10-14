//! Synthetic bundle builders for tests. Nothing here resembles TI data: the
//! images are made-up bytes wrapped in genuine MCUboot framing.

use std::io::{Cursor, Write};

use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

/// MCUboot header magic.
pub const MAGIC: u32 = 0x96f3_b83d;

/// Everything a test might want to vary about a synthetic image.
#[derive(Clone)]
pub struct Img {
    pub hdr_size: u16,
    pub protect_tlv_size: u16,
    pub flags: u32,
    pub version: (u8, u8, u16, u32),
    pub body: Vec<u8>,
    /// Bytes placed in the TLV area after the body (as a 0x6907 TLV with one
    /// entry), or `None` for no TLV area at all.
    pub tlv_payload: Option<Vec<u8>>,
}

impl Img {
    pub fn new(hdr_size: u16, body: Vec<u8>) -> Self {
        Self {
            hdr_size,
            protect_tlv_size: 0,
            flags: 0,
            version: (1, 2, 3, 4),
            body,
            tlv_payload: Some(vec![0xA5; 32]),
        }
    }

    /// A body of `len` bytes that is deterministic and never contains the
    /// MCUboot magic by accident.
    pub fn filled(hdr_size: u16, len: usize, seed: u8) -> Self {
        let body = (0..len)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1)
            .collect();
        Self::new(hdr_size, body)
    }

    pub fn without_tlv(mut self) -> Self {
        self.tlv_payload = None;
        self
    }

    /// The header only (`hdr_size` bytes).
    pub fn header_bytes(&self) -> Vec<u8> {
        let mut h = vec![0u8; 32];
        h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        h[4..8].copy_from_slice(&0u32.to_le_bytes()); // load_addr
        h[8..10].copy_from_slice(&self.hdr_size.to_le_bytes());
        h[10..12].copy_from_slice(&self.protect_tlv_size.to_le_bytes());
        h[12..16].copy_from_slice(&(self.body.len() as u32).to_le_bytes());
        h[16..20].copy_from_slice(&self.flags.to_le_bytes());
        h[20] = self.version.0;
        h[21] = self.version.1;
        h[22..24].copy_from_slice(&self.version.2.to_le_bytes());
        h[24..28].copy_from_slice(&self.version.3.to_le_bytes());
        // A header whose hdr_size field is below 32 still occupies 32 bytes here
        // (tests use that to build deliberately implausible headers).
        h.resize(usize::from(self.hdr_size).max(32), 0);
        h
    }

    /// The TLV area following the body (empty when `tlv_payload` is `None`).
    pub fn tlv_bytes(&self) -> Vec<u8> {
        let Some(payload) = &self.tlv_payload else {
            return Vec::new();
        };
        // image_tlv_info {magic 0x6907, total len} + one TLV {type, len, data}.
        let total = 4 + 4 + payload.len();
        let mut t = Vec::with_capacity(total);
        t.extend_from_slice(&0x6907u16.to_le_bytes());
        t.extend_from_slice(&(total as u16).to_le_bytes());
        t.extend_from_slice(&0x10u16.to_le_bytes());
        t.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        t.extend_from_slice(payload);
        t
    }

    /// Header + body (the part `hdr_size + img_size` covers).
    pub fn image_bytes(&self) -> Vec<u8> {
        let mut v = self.header_bytes();
        v.extend_from_slice(&self.body);
        v
    }

    /// Header + body + TLV area: what a programmer would flash.
    pub fn full_bytes(&self) -> Vec<u8> {
        let mut v = self.image_bytes();
        v.extend_from_slice(&self.tlv_bytes());
        v
    }
}

/// Lay images out at the given package offsets in a buffer padded with 0xFF.
/// The buffer ends exactly where the last image (with its TLV area) ends.
pub fn package(images: &[(usize, &Img)]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (off, img) in images {
        let bytes = img.full_bytes();
        let end = off + bytes.len();
        if buf.len() < end {
            buf.resize(end, 0xFF);
        }
        buf[*off..end].copy_from_slice(&bytes);
    }
    buf
}

/// The standard two-image package: a secure-style image at 0 and a
/// non-secure-style image at 0x4000.
pub fn two_image_package() -> (Vec<u8>, Img, Img) {
    let a = Img::filled(0x400, 0x1000, 1);
    let b = Img::filled(0x200, 0x2345, 2);
    let pkg = package(&[(0, &a), (0x4000, &b)]);
    (pkg, a, b)
}

/// The METADATA text of a typical bundle, with LF line endings.
pub fn metadata_text() -> String {
    [
        "bundle_identifier:TI Bundle",
        "bundle_format_version:1",
        "bundle_build_version:9.8.7.6543",
        "bundle_target_device:84XX",
        "bundle_target_prodid:42",
        "bundle_target_type:RESTORE",
        "bundle_comments:synthetic: colons, in: values",
        "",
    ]
    .join("\n")
}

/// Build a `.84b2`-style ZIP from `(name, data)` entries; names ending in `/`
/// become directory entries.
pub fn zip_bytes(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        if name.ends_with('/') {
            w.add_directory(*name, opts).expect("add_directory");
        } else {
            w.start_file(*name, opts).expect("start_file");
            w.write_all(data).expect("write entry");
        }
    }
    w.finish().expect("finish zip").into_inner()
}

/// A complete synthetic bundle around `pkg`.
pub fn bundle_bytes(pkg: &[u8], method: CompressionMethod) -> Vec<u8> {
    let meta = metadata_text();
    zip_bytes(
        &[
            ("METADATA", meta.as_bytes()),
            ("Synthetic_Package.84pk2", pkg),
            ("Archive/", b""),
        ],
        method,
    )
}

/// A `.84b2` whose package holds one image with a 0x400-byte header (like the
/// real secure image) and `body` as its payload. Placed by
/// `Layout::EVO_7_1`, the payload starts at flash offset 0x000B_0400, which
/// is `VTOR_S` (0x100B_0400) through the Secure alias, so `body` starts with
/// the vector table.
pub fn secure_image_bundle(body: Vec<u8>) -> Vec<u8> {
    let img = Img::new(0x400, body);
    bundle_bytes(&package(&[(0, &img)]), CompressionMethod::Stored)
}
