//! Where the images live in the emulated flash.

use crate::{ImageError, Package};

/// Flash addresses of the slots that images 0 and 1 are programmed into
/// (the address of each image's header, i.e. of the slot start).
///
/// In real layouts such as [`Layout::EVO_7_1`] these are flash offsets,
/// which are also the Non-secure alias addresses (flash is mapped at 0, so
/// [`place`](Layout::place) them with `flash_base` 0). The Secure alias of
/// offset `o` is `o | 0x1000_0000`. The secure image runs from the Secure
/// alias, so boot sets
/// `VTOR_S = 0x1000_0000 | (secure_slot_base + header size of image 0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub secure_slot_base: u32,
    pub ns_slot_base: u32,
}

impl Layout {
    /// OS 7.1 (bundle 7.1.0.4421), found by `evo-cli inspect --score-bases`.
    /// Evidence: `docs/hardware/memory-map.md`. Image 0's header is 0x400
    /// bytes, so `VTOR_S` = 0x100B_0400.
    pub const EVO_7_1: Self = Self {
        secure_slot_base: 0x000B_0000,
        ns_slot_base: 0x0038_0000,
    };

    /// Program the package's images into `flash`, a slice that is mapped at
    /// address `flash_base`: image 0 at `secure_slot_base`, image 1 at
    /// `ns_slot_base`, and each later image at `ns_slot_base` plus its package
    /// distance from image 1 (the OS image's protected TLVs 0xB1..0xB8 give
    /// those addresses; `docs/hardware/boot-flow.md` §3.4). Each is copied
    /// whole (header, payload and TLV areas). A package with a single image
    /// places just that one.
    ///
    /// An image that does not lie entirely inside the slice yields
    /// [`ImageError::DoesNotFit`]. Every image is checked before any is
    /// copied, so `flash` is unchanged on error.
    pub fn place(
        &self,
        pkg: &Package,
        flash: &mut [u8],
        flash_base: u32,
    ) -> Result<(), ImageError> {
        // The slot address of image `index`; `None` when it cannot be expressed.
        let image1_offset = pkg.images.get(1).map(|i| i.pkg_offset);
        let slot = |index: usize, pkg_offset: usize| -> Option<u32> {
            match index {
                0 => Some(self.secure_slot_base),
                1 => Some(self.ns_slot_base),
                _ => {
                    let delta = pkg_offset.checked_sub(image1_offset?)?;
                    self.ns_slot_base.checked_add(u32::try_from(delta).ok()?)
                }
            }
        };

        // (offset into `flash`, package bytes) for each image to place.
        let mut jobs = Vec::with_capacity(pkg.images.len());
        for (index, image) in pkg.images.iter().enumerate() {
            // `Package` has public fields, so tolerate one whose images do not
            // match its bytes instead of indexing out of range.
            let end = image.end_with_tlvs(&pkg.bytes);
            let src = pkg
                .bytes
                .get(image.pkg_offset..end)
                .ok_or(ImageError::Truncated {
                    offset: image.pkg_offset,
                    need: end.saturating_sub(image.pkg_offset),
                    have: pkg.bytes.len().saturating_sub(image.pkg_offset),
                })?;
            let len = src.len();
            let addr = slot(index, image.pkg_offset);
            let does_not_fit = || ImageError::DoesNotFit {
                image: index,
                addr: addr.unwrap_or(u32::MAX),
                len,
            };
            let start = addr
                .and_then(|a| a.checked_sub(flash_base))
                .and_then(|o| usize::try_from(o).ok())
                .ok_or_else(does_not_fit)?;
            let end = start.checked_add(len).ok_or_else(does_not_fit)?;
            if end > flash.len() {
                return Err(does_not_fit());
            }
            jobs.push((start, src));
        }

        for (start, src) in jobs {
            flash[start..start + src.len()].copy_from_slice(src);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Img, package, two_image_package};

    const FLASH_BASE: u32 = 0x1000_0000;
    const FILL: u8 = 0xEE;

    fn layout() -> Layout {
        Layout {
            secure_slot_base: FLASH_BASE + 0x1000,
            ns_slot_base: FLASH_BASE + 0x8000,
        }
    }

    #[test]
    fn place_copies_each_image_to_its_slot() {
        let (bytes, a, b) = two_image_package();
        let pkg = Package::parse(bytes).expect("parse");
        let mut flash = vec![FILL; 0x20000];
        layout().place(&pkg, &mut flash, FLASH_BASE).expect("place");

        let (a_full, b_full) = (a.full_bytes(), b.full_bytes());
        assert_eq!(&flash[0x1000..0x1000 + a_full.len()], &a_full[..]);
        assert_eq!(&flash[0x8000..0x8000 + b_full.len()], &b_full[..]);
        // The vector table sits right after the header, at slot base + hdr_size.
        assert_eq!(flash[0x1000 + 0x400], a.body[0]);

        // Nothing outside the two slots was touched.
        let touched = |i: usize| {
            (0x1000..0x1000 + a_full.len()).contains(&i)
                || (0x8000..0x8000 + b_full.len()).contains(&i)
        };
        assert!(
            flash
                .iter()
                .enumerate()
                .all(|(i, &byte)| touched(i) || byte == FILL)
        );
    }

    #[test]
    fn place_puts_later_images_after_image_1_as_in_the_package() {
        // Images 2.. keep their package distance from image 1 (`boot-flow.md` §3.4: the OS
        // image's protected TLVs 0xB1..0xB8 name their addresses that way).
        let (a, b, c) = (
            Img::filled(0x400, 0x300, 1),
            Img::filled(0x200, 0x500, 2),
            Img::filled(0x40, 0x100, 3),
        );
        let bytes = package(&[(0, &a), (0x2000, &b), (0x3100, &c)]);
        let pkg = Package::parse(bytes).expect("parse");
        let mut flash = vec![FILL; 0x20000];
        layout().place(&pkg, &mut flash, FLASH_BASE).expect("place");

        let c_full = c.full_bytes();
        let c_at = 0x8000 + (0x3100 - 0x2000);
        assert_eq!(&flash[c_at..c_at + c_full.len()], &c_full[..]);
        let b_full = b.full_bytes();
        assert_eq!(&flash[0x8000..0x8000 + b_full.len()], &b_full[..]);
    }

    #[test]
    fn place_later_image_past_the_flash_does_not_fit() {
        let (a, b, c) = (
            Img::filled(0x400, 0x300, 1),
            Img::filled(0x200, 0x500, 2),
            Img::filled(0x40, 0x100, 3),
        );
        let bytes = package(&[(0, &a), (0x2000, &b), (0x3100, &c)]);
        let pkg = Package::parse(bytes).expect("parse");
        // Room for images 0 and 1 but not for image 2.
        let mut flash = vec![FILL; 0x8000 + 0x1100 + 0x10];
        assert!(matches!(
            layout().place(&pkg, &mut flash, FLASH_BASE),
            Err(ImageError::DoesNotFit { image: 2, .. })
        ));
        assert!(flash.iter().all(|&x| x == FILL));
    }

    #[test]
    fn place_does_not_fit_errors() {
        // Review Focus 2 / ruling R16: never panic when an image outruns the
        // flash slice, and leave the slice untouched.
        let (bytes, a, b) = two_image_package();
        let pkg = Package::parse(bytes).expect("parse");
        let b_len = b.full_bytes().len();

        // Image 1 would end one byte past the slice.
        let mut flash = vec![FILL; 0x8000 + b_len - 1];
        match layout().place(&pkg, &mut flash, FLASH_BASE) {
            Err(ImageError::DoesNotFit { image, addr, len }) => {
                assert_eq!(image, 1);
                assert_eq!(addr, FLASH_BASE + 0x8000);
                assert_eq!(len, b_len);
            }
            other => panic!("expected DoesNotFit, got {other:?}"),
        }
        // All-or-nothing: image 0 fit, but must not have been written.
        assert!(flash.iter().all(|&x| x == FILL));

        // Exactly enough room is fine.
        let mut flash = vec![FILL; 0x8000 + b_len];
        layout()
            .place(&pkg, &mut flash, FLASH_BASE)
            .expect("exact fit");

        // Image 0 too big for an empty slice.
        match layout().place(&pkg, &mut [], FLASH_BASE) {
            Err(ImageError::DoesNotFit { image, len, .. }) => {
                assert_eq!(image, 0);
                assert_eq!(len, a.full_bytes().len());
            }
            other => panic!("expected DoesNotFit, got {other:?}"),
        }
    }

    #[test]
    fn place_slot_below_flash_base_does_not_fit() {
        let (bytes, _, _) = two_image_package();
        let pkg = Package::parse(bytes).expect("parse");
        let layout = Layout {
            secure_slot_base: FLASH_BASE - 0x100,
            ns_slot_base: FLASH_BASE + 0x8000,
        };
        let mut flash = vec![FILL; 0x20000];
        assert!(matches!(
            layout.place(&pkg, &mut flash, FLASH_BASE),
            Err(ImageError::DoesNotFit { image: 0, .. })
        ));
        assert!(flash.iter().all(|&x| x == FILL));
    }

    #[test]
    fn place_slot_address_at_top_of_address_space_does_not_overflow() {
        let (bytes, _, _) = two_image_package();
        let pkg = Package::parse(bytes).expect("parse");
        let layout = Layout {
            secure_slot_base: u32::MAX - 8,
            ns_slot_base: u32::MAX,
        };
        let mut flash = vec![FILL; 0x20000];
        assert!(matches!(
            layout.place(&pkg, &mut flash, 0),
            Err(ImageError::DoesNotFit { image: 0, .. })
        ));
    }

    #[test]
    fn place_inconsistent_package_errors_instead_of_panicking() {
        let (bytes, _, _) = two_image_package();
        let mut pkg = Package::parse(bytes).expect("parse");
        pkg.bytes.truncate(0x100); // images now point past the end of the bytes
        let mut flash = vec![FILL; 0x20000];
        assert!(matches!(
            layout().place(&pkg, &mut flash, FLASH_BASE),
            Err(ImageError::Truncated { .. })
        ));
        assert!(flash.iter().all(|&x| x == FILL));
    }

    #[test]
    fn place_single_image_package_places_only_the_secure_slot() {
        let a = Img::filled(0x400, 0x300, 1);
        let pkg = Package::parse(package(&[(0, &a)])).expect("parse");
        let mut flash = vec![FILL; 0x20000];
        layout().place(&pkg, &mut flash, FLASH_BASE).expect("place");
        let a_full = a.full_bytes();
        assert_eq!(&flash[0x1000..0x1000 + a_full.len()], &a_full[..]);
        assert!(flash[0x8000..].iter().all(|&x| x == FILL));
    }

    /// Reads the user's own OS bundle from `EVO_OS_IMAGE`; skipped when unset.
    #[test]
    fn real_bundle_fits_8_mib_flash_with_evo_7_1() {
        let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
            eprintln!(
                "skipping real_bundle_fits_8_mib_flash_with_evo_7_1: EVO_OS_IMAGE is not set"
            );
            return;
        };
        let bundle = crate::Bundle::open(std::path::Path::new(&path)).expect("open real bundle");
        let pkg = &bundle.package;
        let mut flash = vec![FILL; 8 << 20];
        Layout::EVO_7_1
            .place(pkg, &mut flash, 0)
            .expect("both images fit an 8 MiB flash at offset 0");

        let word = |at: usize| u32::from_le_bytes(flash[at..at + 4].try_into().expect("4 bytes"));
        let layout = Layout::EVO_7_1;
        for (image, base) in pkg
            .images
            .iter()
            .zip([layout.secure_slot_base, layout.ns_slot_base])
        {
            let base = base as usize;
            assert_eq!(
                word(base),
                0x96f3_b83d,
                "slot {base:#x} starts with its header"
            );
            let len = image.end_with_tlvs(&pkg.bytes) - image.pkg_offset;
            assert!(flash[base..base + len] == pkg.bytes[image.pkg_offset..][..len]);
        }
        // Image 0's reset vector is a Secure-alias Thumb address in its slot.
        let secure = &pkg.images[0];
        let vtor = layout.secure_slot_base as usize + usize::from(secure.header_size);
        let reset = word(vtor + 4);
        assert_eq!(reset & 0x1000_0001, 0x1000_0001, "{reset:#x}");
        let len = (secure.body.end - secure.pkg_offset) as u32;
        let slot = layout.secure_slot_base..layout.secure_slot_base + len;
        assert!(slot.contains(&(reset & !0x1000_0001)), "{reset:#x}");
    }
}
