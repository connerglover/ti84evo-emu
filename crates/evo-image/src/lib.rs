//! TI-84 Evo OS images: the `.84b2` bundle (a ZIP), the `.84pk2` package inside
//! it, the MCUboot images the package is made of, and their placement in flash.
//!
//! Nothing here ships TI data. Callers supply their own bundle; every parser
//! returns an [`ImageError`] for malformed input instead of panicking.

mod bundle;
mod layout;
mod mcuboot;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use bundle::{Bundle, Metadata};
pub use layout::Layout;
pub use mcuboot::{McubootImage, Package};

/// Everything that can go wrong reading a bundle, a package or placing images.
#[derive(thiserror::Error, Debug)]
pub enum ImageError {
    /// The `.84b2` is not a readable ZIP archive.
    #[error("not a valid ZIP archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// The ZIP has no `.84pk2` package entry.
    #[error("bundle contains no .84pk2 package entry")]
    MissingPackage,
    /// The ZIP has no `METADATA` entry.
    #[error("bundle contains no METADATA entry")]
    MissingMetadata,
    /// A required `METADATA` key is absent or its value does not parse.
    #[error("METADATA key `{key}` is missing or invalid")]
    BadMetadata { key: &'static str },
    /// A ZIP entry decompresses to more than the sanity limit (a corrupt or
    /// hostile archive, not a real bundle).
    #[error("{name} is larger than the {limit} byte limit")]
    EntryTooLarge { name: &'static str, limit: usize },
    /// No MCUboot image header was found in the package.
    #[error(
        "no MCUboot image header found in package (expected magic 0x96f3b83d, first looked at offset {offset:#x})"
    )]
    BadMagic { offset: usize },
    /// An MCUboot header at `offset` needs `need` bytes but only `have` remain.
    #[error(
        "truncated MCUboot image at package offset {offset:#x}: need {need:#x} bytes, have {have:#x}"
    )]
    Truncated {
        offset: usize,
        need: usize,
        have: usize,
    },
    /// Image `image` (index into the package) would occupy `len` bytes at flash
    /// address `addr`, which is outside the flash it is being placed in.
    #[error("image {image} ({len:#x} bytes at {addr:#010x}) does not fit in flash")]
    DoesNotFit { image: usize, addr: u32, len: usize },
    /// Reading the bundle file or a ZIP entry failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
