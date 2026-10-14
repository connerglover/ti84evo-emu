//! MCUboot image headers and the `.84pk2` package (a concatenation of images).

use std::ops::Range;

use crate::ImageError;

/// One MCUboot image found in a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McubootImage {
    /// Offset of the header within [`Package::bytes`].
    pub pkg_offset: usize,
    /// `ih_hdr_size`: header length; the payload starts this far after `pkg_offset`.
    pub header_size: u16,
    /// `ih_img_size`: payload length, excluding header and TLV areas.
    pub image_size: u32,
    /// `ih_protect_tlv_size`.
    pub protected_tlv_size: u16,
    /// `ih_flags`.
    pub flags: u32,
    /// `ih_ver`: (major, minor, revision, build number).
    pub version: (u8, u8, u16, u32),
    /// The payload's byte range within [`Package::bytes`].
    pub body: Range<usize>,
}

/// A parsed `.84pk2`: the raw bytes plus the images located in them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub bytes: Vec<u8>,
    pub images: Vec<McubootImage>,
}

/// `ih_magic`, little-endian at offset 0 of every image header.
const MAGIC: u32 = 0x96f3_b83d;
/// Size of `struct image_header`; no valid `ih_hdr_size` is smaller.
const HEADER_LEN: usize = 32;
/// `it_magic` of the protected / unprotected TLV area info headers.
const TLV_PROTECTED_MAGIC: u16 = 0x6908;
const TLV_UNPROTECTED_MAGIC: u16 = 0x6907;
/// Size of `struct image_tlv_info` (`it_magic`, `it_tlv_tot`).
const TLV_INFO_LEN: usize = 4;

fn le_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// If a TLV area with `magic` starts at `pos` and lies inside `bytes`, return
/// the offset just past it.
fn tlv_area_end(bytes: &[u8], pos: usize, magic: u16) -> Option<usize> {
    let info = bytes.get(pos..pos.checked_add(TLV_INFO_LEN)?)?;
    if u16::from_le_bytes([info[0], info[1]]) != magic {
        return None;
    }
    let total = usize::from(u16::from_le_bytes([info[2], info[3]]));
    if total < TLV_INFO_LEN {
        return None;
    }
    let end = pos.checked_add(total)?;
    (end <= bytes.len()).then_some(end)
}

/// Offset just past the TLV areas (protected, then unprotected) that follow an
/// image whose payload ends at `img_end`. Images without a recognisable TLV
/// area (synthetic ones, or a damaged tail) yield `img_end` unchanged.
fn skip_tlvs(bytes: &[u8], img_end: usize) -> usize {
    let mut pos = img_end;
    for magic in [TLV_PROTECTED_MAGIC, TLV_UNPROTECTED_MAGIC] {
        if let Some(end) = tlv_area_end(bytes, pos, magic) {
            pos = end;
        }
    }
    pos
}

impl McubootImage {
    /// Offset just past this image's header, payload and TLV areas.
    pub(crate) fn end_with_tlvs(&self, pkg_bytes: &[u8]) -> usize {
        skip_tlvs(pkg_bytes, self.body.end)
    }
}

impl Package {
    /// Locate every MCUboot image in `bytes`. Also the entry point for a bare
    /// `.84pk2` that is not wrapped in a bundle.
    ///
    /// The package is scanned at 4-byte alignment for the header magic. Each
    /// image found is skipped over (header, payload, TLV areas) before
    /// scanning resumes, so magic-like bytes inside an image are data. A magic
    /// with `ih_hdr_size < 32` is too small to be a header and is treated as
    /// data; a magic whose header or payload runs past the end of `bytes` is
    /// reported as [`ImageError::Truncated`]. A package with no header at all
    /// is [`ImageError::BadMagic`].
    pub fn parse(bytes: Vec<u8>) -> Result<Self, ImageError> {
        let mut images = Vec::new();
        let mut off = 0usize;
        while off + 4 <= bytes.len() {
            if le_u32(&bytes, off) != MAGIC {
                off += 4;
                continue;
            }
            let have = bytes.len() - off;
            if have < HEADER_LEN {
                return Err(ImageError::Truncated {
                    offset: off,
                    need: HEADER_LEN,
                    have,
                });
            }
            let header_size = le_u16(&bytes, off + 8);
            if usize::from(header_size) < HEADER_LEN {
                off += 4;
                continue;
            }
            let image_size = le_u32(&bytes, off + 12);
            let need = usize::try_from(u64::from(header_size) + u64::from(image_size))
                .unwrap_or(usize::MAX);
            if need > have {
                return Err(ImageError::Truncated {
                    offset: off,
                    need,
                    have,
                });
            }
            let body_start = off + usize::from(header_size);
            let body = body_start..off + need;
            let next = skip_tlvs(&bytes, body.end);
            images.push(McubootImage {
                pkg_offset: off,
                header_size,
                image_size,
                protected_tlv_size: le_u16(&bytes, off + 10),
                flags: le_u32(&bytes, off + 16),
                version: (
                    bytes[off + 20],
                    bytes[off + 21],
                    le_u16(&bytes, off + 22),
                    le_u32(&bytes, off + 24),
                ),
                body,
            });
            off = next.next_multiple_of(4);
        }
        if images.is_empty() {
            return Err(ImageError::BadMagic { offset: 0 });
        }
        Ok(Self { bytes, images })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Img, package, two_image_package};

    #[test]
    fn parses_synthetic_two_image_package() {
        let (bytes, a, b) = two_image_package();
        let total = bytes.len();
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.bytes.len(), total);
        assert_eq!(pkg.images.len(), 2);

        let i0 = &pkg.images[0];
        assert_eq!(i0.pkg_offset, 0);
        assert_eq!(i0.header_size, 0x400);
        assert_eq!(i0.image_size, 0x1000);
        assert_eq!(i0.body, 0x400..0x1400);
        assert_eq!(&pkg.bytes[i0.body.clone()], &a.body[..]);

        let i1 = &pkg.images[1];
        assert_eq!(i1.pkg_offset, 0x4000);
        assert_eq!(i1.header_size, 0x200);
        assert_eq!(i1.image_size, 0x2345);
        assert_eq!(i1.body, 0x4200..0x4200 + 0x2345);
        assert_eq!(&pkg.bytes[i1.body.clone()], &b.body[..]);
    }

    #[test]
    fn header_fields_are_decoded_little_endian() {
        let mut img = Img::filled(0x40, 0x100, 9);
        img.flags = 0x0102_0304;
        img.protect_tlv_size = 0x0050;
        img.version = (7, 1, 0x0203, 0x0000_1135);
        let bytes = package(&[(0, &img)]);
        let pkg = Package::parse(bytes).expect("parse");
        let i = &pkg.images[0];
        assert_eq!(i.flags, 0x0102_0304);
        assert_eq!(i.protected_tlv_size, 0x0050);
        assert_eq!(i.version, (7, 1, 0x0203, 0x1135));
    }

    #[test]
    fn magic_inside_an_image_body_is_data_not_a_header() {
        // A body that embeds a perfectly valid-looking second header.
        let fake = Img::filled(0x40, 0x20, 5).image_bytes();
        let mut real = Img::filled(0x200, 0x400, 3);
        real.body[0x100..0x100 + fake.len()].copy_from_slice(&fake);
        let bytes = package(&[(0, &real)]);
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.images.len(), 1, "embedded header must not be reported");
    }

    #[test]
    fn magic_inside_a_tlv_area_is_data_not_a_header() {
        let fake = Img::filled(0x40, 0x20, 5).image_bytes();
        let mut a = Img::filled(0x200, 0x400, 3);
        a.tlv_payload = Some(fake); // aligned, valid-looking header inside the TLV
        let b = Img::filled(0x40, 0x80, 4);
        let bytes = package(&[(0, &a), (0x1000, &b)]);
        let pkg = Package::parse(bytes).expect("parse");
        let offsets: Vec<usize> = pkg.images.iter().map(|i| i.pkg_offset).collect();
        assert_eq!(offsets, vec![0, 0x1000]);
    }

    #[test]
    fn protected_and_unprotected_tlv_areas_are_skipped() {
        let fake = Img::filled(0x40, 0x20, 5).image_bytes();
        let a = Img::filled(0x200, 0x400, 3);
        let mut bytes = a.image_bytes();
        // Protected TLV area (magic 0x6908) holding a fake header, then the
        // unprotected one (0x6907) holding another.
        let total = (4 + fake.len()) as u16;
        for magic in [0x6908u16, 0x6907] {
            bytes.extend_from_slice(&magic.to_le_bytes());
            bytes.extend_from_slice(&total.to_le_bytes());
            bytes.extend_from_slice(&fake);
        }
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.images.len(), 1);
    }

    #[test]
    fn images_without_a_tlv_area_are_still_found() {
        let a = Img::filled(0x40, 0x100, 1).without_tlv();
        let b = Img::filled(0x40, 0x100, 2).without_tlv();
        let bytes = package(&[(0, &a), (0x140, &b)]);
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.images.len(), 2);
        assert_eq!(pkg.images[1].pkg_offset, 0x140);
    }

    #[test]
    fn unaligned_magic_is_ignored() {
        let a = Img::filled(0x40, 0x100, 1);
        let b = Img::filled(0x40, 0x100, 2);
        let mut bytes = package(&[(0, &a)]);
        // A complete, valid image at offset 0x2002: not 4-byte aligned.
        bytes.resize(0x2002, 0xFF);
        bytes.extend_from_slice(&b.full_bytes());
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.images.len(), 1);
    }

    #[test]
    fn implausible_header_size_is_not_a_header() {
        let mut bogus = Img::filled(0x40, 0x10, 1);
        bogus.hdr_size = 16; // below the 32-byte minimum
        let good = Img::filled(0x40, 0x100, 2);
        let bytes = package(&[(0, &bogus), (0x100, &good)]);
        let pkg = Package::parse(bytes).expect("parse");
        let offsets: Vec<usize> = pkg.images.iter().map(|i| i.pkg_offset).collect();
        assert_eq!(offsets, vec![0x100]);
    }

    #[test]
    fn truncated_image_errors() {
        // Review Focus 2: header says more payload than the file holds.
        let img = Img::filled(0x200, 0x1000, 1);
        let mut bytes = img.image_bytes();
        bytes.truncate(0x200 + 0x800); // cut the payload in half
        let have = bytes.len();
        match Package::parse(bytes) {
            Err(ImageError::Truncated {
                offset,
                need,
                have: h,
            }) => {
                assert_eq!(offset, 0);
                assert_eq!(need, 0x200 + 0x1000);
                assert_eq!(h, have);
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn payload_ending_exactly_at_eof_is_accepted_one_byte_short_is_not() {
        let img = Img::filled(0x40, 0x100, 1).without_tlv();
        let exact = img.image_bytes();
        let pkg = Package::parse(exact.clone()).expect("exact fit");
        assert_eq!(pkg.images[0].body.end, exact.len());

        let mut short = exact;
        short.pop();
        assert!(matches!(
            Package::parse(short),
            Err(ImageError::Truncated { offset: 0, .. })
        ));
    }

    #[test]
    fn truncated_second_image_reports_its_own_offset() {
        let a = Img::filled(0x40, 0x100, 1);
        let b = Img::filled(0x40, 0x100, 2);
        let mut bytes = package(&[(0, &a), (0x400, &b)]);
        bytes.truncate(0x400 + 0x40 + 0x10);
        match Package::parse(bytes) {
            Err(ImageError::Truncated { offset, .. }) => assert_eq!(offset, 0x400),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn header_cut_short_errors() {
        let mut bytes = MAGIC.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0u8; 8]); // 12 of the 32 header bytes
        match Package::parse(bytes) {
            Err(ImageError::Truncated { offset, need, have }) => {
                assert_eq!((offset, need, have), (0, 32, 12));
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn huge_image_size_does_not_overflow() {
        let img = Img::filled(0x40, 0x10, 1);
        let mut bytes = img.header_bytes();
        bytes[8..10].copy_from_slice(&u16::MAX.to_le_bytes()); // hdr_size
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // img_size
        assert!(matches!(
            Package::parse(bytes),
            Err(ImageError::Truncated { .. })
        ));
    }

    #[test]
    fn no_images_is_bad_magic() {
        let cases: [&[u8]; 4] = [&[], &[0u8; 3], &[0xFF; 4096], b"PK\x03\x04 not a package"];
        for junk in cases {
            assert!(
                matches!(
                    Package::parse(junk.to_vec()),
                    Err(ImageError::BadMagic { offset: 0 })
                ),
                "junk of len {} should be BadMagic",
                junk.len()
            );
        }
    }

    #[test]
    fn leading_padding_before_the_first_image_is_tolerated() {
        let a = Img::filled(0x40, 0x100, 1);
        let bytes = package(&[(0x100, &a)]);
        let pkg = Package::parse(bytes).expect("parse");
        assert_eq!(pkg.images[0].pkg_offset, 0x100);
    }
}
