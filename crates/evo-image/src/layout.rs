//! Where the images live in the emulated flash.

use crate::{ImageError, Package};

/// Flash addresses of the slots that images 0 and 1 are programmed into
/// (the address of each image's header, i.e. of the slot start).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub secure_slot_base: u32,
    pub ns_slot_base: u32,
}

impl Layout {
    /// Program the package's images into `flash`, a slice that is mapped at
    /// address `flash_base`: image 0 at `secure_slot_base` and image 1 at
    /// `ns_slot_base`. Each is copied whole (header, payload and TLV areas).
    /// A package with a single image places just that one. Images beyond the
    /// first two are not placed (their role is not yet known).
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
        let slots = [self.secure_slot_base, self.ns_slot_base];

        // (offset into `flash`, package bytes) for each image to place.
        let mut jobs = Vec::with_capacity(slots.len());
        for (index, (image, &addr)) in pkg.images.iter().zip(&slots).enumerate() {
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
            let does_not_fit = || ImageError::DoesNotFit {
                image: index,
                addr,
                len,
            };
            let start = addr
                .checked_sub(flash_base)
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

    #[test]
    fn place_ignores_images_beyond_the_first_two() {
        let a = Img::filled(0x400, 0x300, 1);
        let b = Img::filled(0x200, 0x300, 2);
        let c = Img::filled(0x40, 0x100, 3);
        let pkg = Package::parse(package(&[(0, &a), (0x1000, &b), (0x2000, &c)])).expect("parse");
        assert_eq!(pkg.images.len(), 3);
        let mut flash = vec![FILL; 0x20000];
        layout().place(&pkg, &mut flash, FLASH_BASE).expect("place");
        let c_bytes = c.image_bytes();
        assert!(
            !flash.windows(c_bytes.len()).any(|w| w == &c_bytes[..]),
            "third image must not be placed"
        );
    }
}
