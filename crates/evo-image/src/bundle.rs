//! The `.84b2` bundle: a ZIP holding `METADATA` and a `.84pk2` package.

use std::io::{Cursor, Read};
use std::path::Path;

use zip::ZipArchive;
use zip::result::ZipError;

use crate::{ImageError, Package};

/// The fields of the bundle's `METADATA` file that the emulator uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// `bundle_build_version`, e.g. `7.1.0.4421`.
    pub build_version: String,
    /// `bundle_target_device`.
    pub target_device: String,
    /// `bundle_target_prodid`.
    pub prodid: u32,
    /// `bundle_target_type` (e.g. `RESTORE`).
    pub kind: String,
}

/// A parsed `.84b2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub metadata: Metadata,
    pub package: Package,
}

/// Real bundles are a few MiB; anything far beyond that is not one. The limits
/// bound memory use on a hostile or corrupt archive (a decompression bomb).
const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 1024 * 1024;
/// Never reserve more than this up front on the strength of a ZIP header's
/// (possibly lying) declared size.
const MAX_PREALLOC: usize = 16 * 1024 * 1024;

const PACKAGE_EXTENSION: &[u8] = b".84pk2";

/// Read an entry fully, failing if it holds more than `limit` bytes.
fn read_entry(
    entry: &mut impl Read,
    declared_size: u64,
    name: &'static str,
    limit: usize,
) -> Result<Vec<u8>, ImageError> {
    let prealloc = usize::try_from(declared_size)
        .unwrap_or(usize::MAX)
        .min(limit)
        .min(MAX_PREALLOC);
    let mut buf = Vec::with_capacity(prealloc);
    entry.take(limit as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > limit {
        return Err(ImageError::EntryTooLarge { name, limit });
    }
    Ok(buf)
}

fn is_package_name(name: &[u8]) -> bool {
    name.len() >= PACKAGE_EXTENSION.len()
        && name[name.len() - PACKAGE_EXTENSION.len()..].eq_ignore_ascii_case(PACKAGE_EXTENSION)
}

impl Metadata {
    /// Parse the `key:value` lines of a `METADATA` file. Values may themselves
    /// contain colons; only the first one separates key from value. All four
    /// keys are required and must have a non-empty value.
    fn parse(text: &str) -> Result<Self, ImageError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let get = |key: &'static str| {
            text.lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim())
                .filter(|v| !v.is_empty())
                .ok_or(ImageError::BadMetadata { key })
        };
        let prodid = get("bundle_target_prodid")?
            .parse()
            .map_err(|_| ImageError::BadMetadata {
                key: "bundle_target_prodid",
            })?;
        Ok(Self {
            build_version: get("bundle_build_version")?.to_owned(),
            target_device: get("bundle_target_device")?.to_owned(),
            prodid,
            kind: get("bundle_target_type")?.to_owned(),
        })
    }
}

impl Bundle {
    /// Read and parse a `.84b2` file.
    pub fn open(path: &Path) -> Result<Self, ImageError> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Parse the bytes of a `.84b2`.
    ///
    /// The package is the first file entry whose name ends in `.84pk2`
    /// (case-insensitive, any directory); a `METADATA` entry must also exist.
    pub fn from_bytes(b: &[u8]) -> Result<Self, ImageError> {
        let mut archive = ZipArchive::new(Cursor::new(b))?;

        let mut package_index = None;
        for i in 0..archive.len() {
            let entry = archive.by_index_data(i)?;
            if entry.is_file() && is_package_name(entry.name_raw()) {
                package_index = Some(i);
                break;
            }
        }
        let package_index = package_index.ok_or(ImageError::MissingPackage)?;

        let metadata = match archive.by_name("METADATA") {
            Ok(mut entry) => {
                let size = entry.size();
                let raw = read_entry(&mut entry, size, "METADATA", MAX_METADATA_BYTES)?;
                Metadata::parse(&String::from_utf8_lossy(&raw))?
            }
            Err(ZipError::FileNotFound) => return Err(ImageError::MissingMetadata),
            Err(e) => return Err(e.into()),
        };

        let mut entry = archive.by_index(package_index)?;
        let size = entry.size();
        let bytes = read_entry(&mut entry, size, "package", MAX_PACKAGE_BYTES)?;
        let package = Package::parse(bytes)?;
        Ok(Self { metadata, package })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        Img, bundle_bytes, metadata_text, package, two_image_package, zip_bytes,
    };
    use zip::CompressionMethod;

    fn expected_metadata() -> Metadata {
        Metadata {
            build_version: "9.8.7.6543".into(),
            target_device: "84XX".into(),
            prodid: 42,
            kind: "RESTORE".into(),
        }
    }

    #[test]
    fn metadata_fields() {
        let (pkg, _, _) = two_image_package();
        let bundle = Bundle::from_bytes(&bundle_bytes(&pkg, CompressionMethod::Deflated))
            .expect("parse bundle");
        assert_eq!(bundle.metadata, expected_metadata());
        assert_eq!(bundle.package.images.len(), 2);
        assert_eq!(bundle.package.bytes, pkg);
    }

    #[test]
    fn metadata_tolerates_crlf_and_stored_entries() {
        let (pkg, _, _) = two_image_package();
        let meta = metadata_text().replace('\n', "\r\n");
        let zip = zip_bytes(
            &[("METADATA", meta.as_bytes()), ("P.84pk2", &pkg)],
            CompressionMethod::Stored,
        );
        let bundle = Bundle::from_bytes(&zip).expect("parse bundle");
        assert_eq!(bundle.metadata, expected_metadata());
    }

    #[test]
    fn package_is_found_by_extension_not_name() {
        let (pkg, _, _) = two_image_package();
        let meta = metadata_text();
        let zip = zip_bytes(
            &[
                ("Archive/", b""),
                ("notes.txt", b"not the package"),
                ("METADATA", meta.as_bytes()),
                ("sub/dir/Whatever.84PK2", &pkg),
            ],
            CompressionMethod::Deflated,
        );
        let bundle = Bundle::from_bytes(&zip).expect("parse bundle");
        assert_eq!(bundle.package.bytes, pkg);
    }

    #[test]
    fn bare_pk2_accepted() {
        let (pkg, _, _) = two_image_package();
        let bare = Package::parse(pkg.clone()).expect("bare .84pk2");
        let wrapped = Bundle::from_bytes(&bundle_bytes(&pkg, CompressionMethod::Deflated))
            .expect("bundle")
            .package;
        assert_eq!(bare, wrapped);
        // ...but a bare package is not a bundle.
        assert!(matches!(Bundle::from_bytes(&pkg), Err(ImageError::Zip(_))));
    }

    #[test]
    fn bad_zip_errors() {
        let cases: [&[u8]; 3] = [&[], b"definitely not a zip file", &[0u8; 1024]];
        for junk in cases {
            assert!(
                matches!(Bundle::from_bytes(junk), Err(ImageError::Zip(_))),
                "len {}",
                junk.len()
            );
        }
        // A real bundle with its tail (the central directory) cut off.
        let (pkg, _, _) = two_image_package();
        let mut zip = bundle_bytes(&pkg, CompressionMethod::Deflated);
        zip.truncate(zip.len() / 2);
        assert!(matches!(Bundle::from_bytes(&zip), Err(ImageError::Zip(_))));
    }

    #[test]
    fn missing_package_errors() {
        let meta = metadata_text();
        let zip = zip_bytes(
            &[
                ("METADATA", meta.as_bytes()),
                ("Archive/", b""),
                ("README", b"hi"),
            ],
            CompressionMethod::Deflated,
        );
        assert!(matches!(
            Bundle::from_bytes(&zip),
            Err(ImageError::MissingPackage)
        ));
        // A directory that merely has the right extension is not a package.
        let zip = zip_bytes(
            &[("METADATA", meta.as_bytes()), ("fake.84pk2/", b"")],
            CompressionMethod::Deflated,
        );
        assert!(matches!(
            Bundle::from_bytes(&zip),
            Err(ImageError::MissingPackage)
        ));
    }

    #[test]
    fn missing_metadata_errors() {
        let (pkg, _, _) = two_image_package();
        let zip = zip_bytes(&[("P.84pk2", &pkg)], CompressionMethod::Deflated);
        assert!(matches!(
            Bundle::from_bytes(&zip),
            Err(ImageError::MissingMetadata)
        ));
    }

    #[test]
    fn malformed_metadata_errors_name_the_key() {
        let (pkg, _, _) = two_image_package();
        let bad_prodids = [
            "bundle_target_prodid:forty-two",
            "bundle_target_prodid:",
            "bundle_target_prodid:-1",
            "bundle_target_prodid:99999999999",
        ];
        for line in bad_prodids {
            let meta = metadata_text().replace("bundle_target_prodid:42", line);
            let zip = zip_bytes(
                &[("METADATA", meta.as_bytes()), ("P.84pk2", &pkg)],
                CompressionMethod::Deflated,
            );
            match Bundle::from_bytes(&zip) {
                Err(ImageError::BadMetadata { key }) => assert_eq!(key, "bundle_target_prodid"),
                other => panic!("{line}: expected BadMetadata, got {other:?}"),
            }
        }
        // Missing keys are named too.
        for key in [
            "bundle_build_version",
            "bundle_target_device",
            "bundle_target_prodid",
            "bundle_target_type",
        ] {
            let meta: String = metadata_text()
                .lines()
                .filter(|l| !l.starts_with(key))
                .map(|l| format!("{l}\n"))
                .collect();
            let zip = zip_bytes(
                &[("METADATA", meta.as_bytes()), ("P.84pk2", &pkg)],
                CompressionMethod::Deflated,
            );
            match Bundle::from_bytes(&zip) {
                Err(ImageError::BadMetadata { key: k }) => assert_eq!(k, key),
                other => panic!("without {key}: expected BadMetadata, got {other:?}"),
            }
        }
    }

    #[test]
    fn bundle_with_truncated_image_errors() {
        // Review Focus 2 end to end: the ZIP is fine, the package inside is cut.
        let img = Img::filled(0x200, 0x1000, 1);
        let mut pkg = package(&[(0, &img)]);
        pkg.truncate(0x200 + 0x10);
        let zip = bundle_bytes(&pkg, CompressionMethod::Deflated);
        assert!(matches!(
            Bundle::from_bytes(&zip),
            Err(ImageError::Truncated { offset: 0, .. })
        ));
    }

    #[test]
    fn corrupt_zip_entry_data_errors() {
        // Flip a byte inside a stored (uncompressed) package: the CRC check
        // must reject it as an error, not hand back wrong data or panic.
        let (pkg, _, _) = two_image_package();
        let mut zip = bundle_bytes(&pkg, CompressionMethod::Stored);
        let marker = &pkg[0x500..0x520];
        let at = zip
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("stored package bytes appear verbatim in the zip");
        zip[at + 4] ^= 0xFF;
        assert!(Bundle::from_bytes(&zip).is_err());
    }

    #[test]
    fn open_reads_a_file_and_reports_io_errors() {
        let (pkg, _, _) = two_image_package();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("evo-image-test-{}.84b2", std::process::id()));
        std::fs::write(&path, bundle_bytes(&pkg, CompressionMethod::Deflated)).expect("write");
        let opened = Bundle::open(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(opened.expect("open").metadata, expected_metadata());

        let missing = dir.join("evo-image-test-this-file-does-not-exist.84b2");
        assert!(matches!(Bundle::open(&missing), Err(ImageError::Io(_))));
    }

    /// Reads the user's own OS bundle from `EVO_OS_IMAGE`; skipped when unset.
    #[test]
    fn real_bundle_matches_spec() {
        let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
            eprintln!("skipping real_bundle_matches_spec: EVO_OS_IMAGE is not set");
            return;
        };
        let bundle = Bundle::open(Path::new(&path)).expect("open real bundle");

        assert_eq!(bundle.metadata.build_version, "7.1.0.4421");
        assert_eq!(bundle.metadata.target_device, "84US");
        assert_eq!(bundle.metadata.prodid, 23);
        assert_eq!(bundle.metadata.kind, "RESTORE");

        let imgs = &bundle.package.images;
        assert_eq!(imgs.len(), 10, "images: {imgs:#x?}");

        let secure = &imgs[0];
        assert_eq!(secure.pkg_offset, 0);
        assert_eq!(secure.header_size, 0x400);
        assert_eq!(secure.image_size, 0xDAEB8);
        assert_eq!(secure.version, (7, 1, 0, 4413));

        let ns = &imgs[1];
        assert_eq!(ns.pkg_offset, 0x10_0000);
        assert_eq!(ns.header_size, 0x200);
        assert_eq!(ns.image_size, 0x29_3678);
        assert_eq!(ns.version, (7, 1, 0, 4421));

        let small = [
            0x39A6D8, 0x3A1000, 0x3A78A0, 0x3BF7C8, 0x3C260C, 0x3C7E2C, 0x3F2BB4, 0x3F4350,
        ];
        for (img, want) in imgs[2..].iter().zip(small) {
            assert_eq!(img.pkg_offset, want);
            assert_eq!(img.header_size, 0x40);
            assert_eq!(img.version, (7, 1, 0, 4421));
        }

        // Structural invariants: ordered, in bounds, non-overlapping, 4-aligned.
        let mut prev_end = 0;
        for img in imgs {
            let start = img.pkg_offset + usize::from(img.header_size);
            assert_eq!(img.pkg_offset % 4, 0);
            assert!(
                img.pkg_offset >= prev_end,
                "overlap at {:#x}",
                img.pkg_offset
            );
            assert_eq!(img.body, start..start + img.image_size as usize);
            assert!(img.body.end <= bundle.package.bytes.len());
            prev_end = img.body.end;
        }
    }
}
