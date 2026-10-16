//! Completed permanent-update outcome for the existing BL2 bypass, not a bootloader.
//! Slot addresses come from the EVO 7.1 secure firmware flash-area table; trailer
//! fields and final permanent-swap state follow MCUboot's documented format.

use evo_hw::{MemoryMap, nor::NvState};
use evo_image::Layout;
use rsa::{Pss, RsaPublicKey, pkcs1::DecodeRsaPublicKey, traits::PublicKeyParts};
use sha2::{Digest, Sha256};

use crate::{Machine, NvBootError};

const PRIMARY: usize = 0x0b0000;
const SECONDARY: usize = 0x280000;
const SLOT: usize = 0x100000;
const TRAILER_SECTOR: usize = 0x1000;
const MAGIC: [u8; 16] = [
    0x77, 0xc2, 0x95, 0xf3, 0x60, 0xd2, 0xef, 0x7f, 0x35, 0x52, 0x50, 0x0f, 0x2c, 0xb6, 0x79, 0x80,
];

fn error(message: &'static str) -> NvBootError {
    NvBootError::Update(message)
}
fn u16le(bytes: &[u8]) -> usize {
    usize::from(u16::from_le_bytes(bytes.try_into().unwrap()))
}
fn u32le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

pub(crate) fn pending(bus: &MemoryMap) -> bool {
    let end = (SECONDARY + SLOT) as u32;
    bus.peek(end - 24, 1) == Some(1)
        && bus.peek(end - 32, 1) == Some(0xff)
        && MAGIC
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .all(|(i, word)| bus.peek(end - 16 + i as u32 * 4, 4) == Some(u32le(word)))
}

struct Image<'a> {
    body: &'a [u8],
    hashed: &'a [u8],
    hash: &'a [u8],
    key_hash: &'a [u8],
    signature: &'a [u8],
    counter: u32,
}

fn tlvs<'a>(
    bytes: &'a [u8],
    mut visit: impl FnMut(u16, &'a [u8]) -> Result<(), NvBootError>,
) -> Result<(), NvBootError> {
    let mut rest = bytes;
    while !rest.is_empty() {
        let header = rest.get(..4).ok_or(error("truncated TLV header"))?;
        let len = u16le(&header[2..4]);
        let value = rest.get(4..4 + len).ok_or(error("truncated TLV value"))?;
        visit(u16::from_le_bytes([header[0], header[1]]), value)?;
        rest = &rest[4 + len..];
    }
    Ok(())
}

fn image(slot: &[u8]) -> Result<Image<'_>, NvBootError> {
    let data = &slot[..SLOT - TRAILER_SECTOR];
    if u32le(&data[..4]) != 0x96f3_b83d {
        return Err(error("missing MCUboot image header"));
    }
    let header = u16le(&data[8..10]);
    let protected = u16le(&data[10..12]);
    let size = u32le(&data[12..16]) as usize;
    if header < 32 || size < 8 || u32le(&data[16..20]) != 0 {
        return Err(error("unsupported MCUboot header, body, or image flags"));
    }
    let body_end = header
        .checked_add(size)
        .ok_or(error("image length overflow"))?;
    let hash_end = body_end
        .checked_add(protected)
        .ok_or(error("protected length overflow"))?;
    let hashed = data
        .get(..hash_end)
        .ok_or(error("image overlaps trailer or exceeds slot"))?;
    let body = hashed
        .get(header..body_end)
        .ok_or(error("invalid image body"))?;
    let prot = hashed
        .get(body_end..)
        .ok_or(error("invalid protected TLVs"))?;
    if prot.len() < 4 || u16le(&prot[..2]) != 0x6908 || u16le(&prot[2..4]) != protected {
        return Err(error("missing or inconsistent protected TLV area"));
    }
    let mut counter = None;
    tlvs(&prot[4..], |kind, value| {
        if kind == 0x50 {
            if value.len() != 4 || counter.is_some() {
                return Err(error("invalid security counter TLV"));
            }
            counter = Some(u32le(value));
        }
        Ok(())
    })?;
    let info = data
        .get(hash_end..hash_end + 4)
        .ok_or(error("missing signature TLVs"))?;
    let total = u16le(&info[2..4]);
    if u16le(&info[..2]) != 0x6907 || total < 4 {
        return Err(error("invalid signature TLV area"));
    }
    let tail = data
        .get(hash_end + 4..hash_end + total)
        .ok_or(error("signature TLVs exceed slot"))?;
    let (mut hash, mut key_hash, mut signature) = (None, None, None);
    tlvs(tail, |kind, value| {
        let target = match kind {
            0x10 => &mut hash,
            1 => &mut key_hash,
            0x23 => &mut signature,
            _ => return Ok(()),
        };
        if target.is_some() {
            return Err(error("duplicate signature or hash TLV"));
        }
        *target = Some(value);
        Ok(())
    })?;
    let hash = hash
        .filter(|v| v.len() == 32)
        .ok_or(error("missing SHA-256 TLV"))?;
    let key_hash = key_hash
        .filter(|v| v.len() == 32)
        .ok_or(error("missing public-key hash TLV"))?;
    let signature = signature
        .filter(|v| v.len() == 384)
        .ok_or(error("missing RSA-3072 signature TLV"))?;
    Ok(Image {
        body,
        hashed,
        hash,
        key_hash,
        signature,
        counter: counter.ok_or(error("missing protected security counter"))?,
    })
}

fn trusted_key(active: &Image<'_>, key_hash: &[u8]) -> Result<RsaPublicKey, NvBootError> {
    // TI stores PKCS#1 DER RSA keys inside the active secure body. Trust only that
    // existing image, never keys supplied by the candidate or elsewhere in flash.
    for (offset, prefix) in active.body.windows(4).enumerate() {
        if prefix[..2] != [0x30, 0x82] {
            continue;
        }
        let len = usize::from(u16::from_be_bytes([prefix[2], prefix[3]])) + 4;
        if !(384..=512).contains(&len) {
            continue;
        }
        let Some(der) = active.body.get(offset..offset + len) else {
            continue;
        };
        if Sha256::digest(der).as_slice() != key_hash {
            continue;
        }
        if let Ok(key) = RsaPublicKey::from_pkcs1_der(der)
            && key.n().bits() == 3072
        {
            return Ok(key);
        }
    }
    Err(error(
        "candidate key is absent from the active secure image",
    ))
}

pub(crate) fn activate_pending(
    nv: &NvState,
    layout: &Layout,
) -> Result<Option<NvState>, NvBootError> {
    if *layout != Layout::EVO_7_1 {
        return Ok(None);
    }
    let bytes = nv.flash();
    let end = SECONDARY + SLOT;
    if bytes[end - 16..end] != MAGIC || bytes[end - 24] != 1 || bytes[end - 32] != 0xff {
        return Ok(None);
    }
    let active = image(&bytes[PRIMARY..PRIMARY + SLOT])?;
    let candidate = image(&bytes[SECONDARY..end])?;
    if candidate.counter < active.counter {
        return Err(error("candidate security counter decreases"));
    }
    if Sha256::digest(candidate.hashed).as_slice() != candidate.hash {
        return Err(error("candidate image hash mismatch"));
    }
    trusted_key(&active, candidate.key_hash)?
        .verify(
            Pss::new_with_salt::<Sha256>(32),
            candidate.hash,
            candidate.signature,
        )
        .map_err(|_| error("candidate RSA-PSS signature is invalid"))?;

    // Model the atomic completed outcome only. No user image bytes are patched;
    // the candidate was signed for its primary address. No test/revert support.
    let mut flash = bytes.to_vec();
    for i in 0..SLOT {
        flash.swap(PRIMARY + i, SECONDARY + i);
    }
    flash[PRIMARY + SLOT - 32] = 1; // copy_done; image_ok and magic came from secondary
    flash[SECONDARY + SLOT - TRAILER_SECTOR..end].fill(0xff);
    let activated = NvState::new(flash, nv.secregs().to_vec()).expect("unchanged NV sizes");
    Machine::nv_boot_metadata(&activated, layout)?;
    Ok(Some(activated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{FLASH_S, SRAM_S};
    use evo_hw::{
        nor::{NOR_SIZE, SECREGS_LEN},
        soc::FLASH_BUFFER,
    };
    use evo_image::testutil::Img;
    use rand::{SeedableRng, rngs::StdRng};
    use rsa::{
        RsaPrivateKey,
        pkcs1::{DecodeRsaPrivateKey, EncodeRsaPublicKey},
    };
    use std::sync::OnceLock;

    fn key() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        // Locally generated synthetic key, deliberately public test data. Never TI's key.
        KEY.get_or_init(|| {
            RsaPrivateKey::from_pkcs1_der(include_bytes!("../tests/fixtures/update-test-key.der"))
                .unwrap()
        })
    }

    fn signed_image(value: u8, counter: u32, valid_vector: bool) -> Vec<u8> {
        let public = RsaPublicKey::from(key()).to_pkcs1_der().unwrap();
        let mut body = vec![0; 0x400];
        body[..4].copy_from_slice(&(SRAM_S + 0x1000).to_le_bytes());
        let reset = if valid_vector {
            FLASH_S + PRIMARY as u32 + 0x501
        } else {
            0xdead_beef
        };
        body[4..8].copy_from_slice(&reset.to_le_bytes());
        body[0x100..0x102].copy_from_slice(&(0x2000 | u16::from(value)).to_le_bytes());
        body[0x102..0x104].copy_from_slice(&0xe7feu16.to_le_bytes());
        body[0x200..0x200 + public.as_bytes().len()].copy_from_slice(public.as_bytes());
        let mut img = Img::new(0x400, body).without_tlv();
        img.protect_tlv_size = 12;
        let mut bytes = img.image_bytes();
        bytes.extend_from_slice(&[8, 0x69, 12, 0, 0x50, 0, 4, 0]);
        bytes.extend_from_slice(&counter.to_le_bytes());
        let hash = Sha256::digest(&bytes);
        let signature = key()
            .sign_with_rng(
                &mut StdRng::seed_from_u64(42),
                Pss::new_with_salt::<Sha256>(32),
                &hash,
            )
            .unwrap();
        bytes.extend_from_slice(&[7, 0x69, 0xd0, 1]);
        for (kind, value) in [
            (0x10u16, hash.as_slice()),
            (1, Sha256::digest(public.as_bytes()).as_slice()),
            (0x23, signature.as_slice()),
        ] {
            bytes.extend_from_slice(&kind.to_le_bytes());
            bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
            bytes.extend_from_slice(value);
        }
        bytes
    }

    fn fixture() -> NvState {
        static NV: OnceLock<NvState> = OnceLock::new();
        NV.get_or_init(|| {
            let active = signed_image(7, 1, true);
            let candidate = signed_image(42, 2, true);
            let mut flash = vec![0xff; NOR_SIZE];
            flash[PRIMARY..PRIMARY + active.len()].copy_from_slice(&active);
            flash[SECONDARY..SECONDARY + candidate.len()].copy_from_slice(&candidate);
            flash[SECONDARY + SLOT - 16..SECONDARY + SLOT].copy_from_slice(&MAGIC);
            flash[SECONDARY + SLOT - 24] = 1;
            flash[0x90000] = 0xa5;
            NvState::new(flash, vec![0x12; SECREGS_LEN]).unwrap()
        })
        .clone()
    }

    #[test]
    fn signed_permanent_update_activates_once_and_runs_new_code() {
        let nv = fixture();
        let mut m = Machine::from_nv(nv.clone(), &Layout::EVO_7_1).unwrap();
        m.run(10);
        assert_eq!(m.cpu.regs.r[0], 42);
        let saved = m.nv_state();
        assert_eq!(saved.flash()[PRIMARY + SLOT - 32], 1);
        assert_eq!(saved.flash()[PRIMARY + SLOT - 24], 1);
        assert_eq!(
            &saved.flash()[SECONDARY..SECONDARY + 0x800],
            &nv.flash()[PRIMARY..PRIMARY + 0x800]
        );
        assert!(
            saved.flash()[SECONDARY + SLOT - TRAILER_SECTOR..SECONDARY + SLOT]
                .iter()
                .all(|&b| b == 0xff)
        );
        assert_eq!(saved.flash()[0x90000], 0xa5);
        assert_eq!(saved.secregs(), nv.secregs());
        assert!(
            activate_pending(&saved, &Layout::EVO_7_1)
                .unwrap()
                .is_none()
        );
        let mut restarted = Machine::from_nv(saved, &Layout::EVO_7_1).unwrap();
        restarted.run(10);
        assert_eq!(restarted.cpu.regs.r[0], 42);
    }

    #[test]
    fn invalid_hash_signature_counter_and_vectors_leave_original_untouched() {
        let original = fixture();
        for case in 0..4 {
            let mut flash = original.flash().to_vec();
            match case {
                0 => flash[SECONDARY + 0x600] ^= 1,
                1 => flash[SECONDARY + 0x800 + 12 + 0x1d0 - 1] ^= 1,
                _ => {
                    let image = signed_image(42, if case == 2 { 0 } else { 2 }, case != 3);
                    flash[SECONDARY..SECONDARY + image.len()].copy_from_slice(&image);
                }
            }
            let before = flash.clone();
            let nv = NvState::new(flash, original.secregs().to_vec()).unwrap();
            assert!(
                activate_pending(&nv, &Layout::EVO_7_1).is_err(),
                "case {case}"
            );
            assert_eq!(nv.flash(), before);
            assert_eq!(nv.secregs(), original.secregs());
        }
    }

    #[test]
    fn partial_or_unconfirmed_trailer_does_not_activate() {
        for offset in [SLOT - 1, SLOT - 24, SLOT - 32] {
            let original = fixture();
            let mut flash = original.flash().to_vec();
            flash[SECONDARY + offset] ^= 1;
            let nv = NvState::new(flash, original.secregs().to_vec()).unwrap();
            assert!(activate_pending(&nv, &Layout::EVO_7_1).unwrap().is_none());
            let mut m = Machine::from_nv(nv.clone(), &Layout::EVO_7_1).unwrap();
            m.run(10);
            assert_eq!(m.cpu.regs.r[0], 7);
            assert_eq!(m.nv_state().flash(), nv.flash());
        }
    }

    #[test]
    fn unknown_key_and_malformed_tlv_bounds_leave_original_untouched() {
        let original = fixture();
        for case in 0..5 {
            let mut flash = original.flash().to_vec();
            match case {
                // The candidate remains correctly signed, but its matching key is absent
                // from the trusted active body. A candidate's own embedded key is ignored.
                0 => flash[PRIMARY + 0x600] = 0,
                1 => flash[SECONDARY + 10..SECONDARY + 12].copy_from_slice(&u16::MAX.to_le_bytes()),
                2 => flash[SECONDARY + 0x800 + 6..SECONDARY + 0x800 + 8]
                    .copy_from_slice(&u16::MAX.to_le_bytes()),
                3 => flash[SECONDARY + 0x800 + 12 + 2..SECONDARY + 0x800 + 12 + 4]
                    .copy_from_slice(&3u16.to_le_bytes()),
                _ => flash[SECONDARY + 0x800 + 12 + 6..SECONDARY + 0x800 + 12 + 8]
                    .copy_from_slice(&u16::MAX.to_le_bytes()),
            }
            let nv = NvState::new(flash.clone(), original.secregs().to_vec()).unwrap();
            assert!(
                activate_pending(&nv, &Layout::EVO_7_1).is_err(),
                "case {case}"
            );
            assert_eq!(nv.flash(), flash);
            assert_eq!(nv.secregs(), original.secregs());
        }
    }

    #[test]
    fn power_cycle_and_system_reset_apply_completed_pending_update() {
        for system_reset in [false, true] {
            let staged = fixture();
            let mut original = staged.flash().to_vec();
            original[SECONDARY..SECONDARY + SLOT].fill(0xff);
            let mut m = Machine::from_nv(
                NvState::new(original, staged.secregs().to_vec()).unwrap(),
                &Layout::EVO_7_1,
            )
            .unwrap();
            let previous_identity = m.bundle_id;
            m.bus
                .buffer_mut(FLASH_BUFFER)
                .unwrap()
                .copy_from_slice(staged.flash());
            if system_reset {
                m.cpu.ppb_write32(0xe000_ed0c, 0x05fa_0004);
                m.step();
                assert_eq!(m.resets, 1);
            } else {
                m.power_cycle();
            }
            m.run(10);
            assert_eq!(m.cpu.regs.r[0], 42);
            assert_eq!(m.nv_state().flash()[PRIMARY + SLOT - 32], 1);
            assert_ne!(m.bundle_id, previous_identity);
            let restarted = Machine::from_nv(m.nv_state(), &Layout::EVO_7_1).unwrap();
            assert_eq!(m.bundle_id, restarted.bundle_id);
        }
    }

    #[test]
    fn reset_with_invalid_pending_update_keeps_active_image_and_nv() {
        let staged = fixture();
        let mut flash = staged.flash().to_vec();
        flash[SECONDARY..SECONDARY + SLOT].fill(0xff);
        for system_reset in [false, true] {
            let mut m = Machine::from_nv(
                NvState::new(flash.clone(), staged.secregs().to_vec()).unwrap(),
                &Layout::EVO_7_1,
            )
            .unwrap();
            let mut invalid = staged.flash().to_vec();
            invalid[SECONDARY + 0x500] ^= 1;
            m.bus
                .buffer_mut(FLASH_BUFFER)
                .unwrap()
                .copy_from_slice(&invalid);
            let identity = m.bundle_id;
            if system_reset {
                m.cpu.ppb_write32(0xe000_ed0c, 0x05fa_0004);
                m.step();
            } else {
                m.power_cycle();
            }
            m.run(10);
            assert_eq!(m.cpu.regs.r[0], 7);
            assert_eq!(m.nv_state().flash(), invalid);
            assert_eq!(m.nv_state().secregs(), staged.secregs());
            assert_eq!(m.bundle_id, identity);
        }
    }

    #[test]
    fn real_staged_update_when_supplied() {
        let Some(path) = std::env::var_os("EVO_STAGED_UPDATE") else {
            return;
        };
        let original = NvState::read_file(std::path::Path::new(&path)).unwrap();
        let activated = activate_pending(&original, &Layout::EVO_7_1)
            .unwrap()
            .expect("pending permanent update");
        assert_eq!(
            &activated.flash()[PRIMARY..PRIMARY + SLOT - TRAILER_SECTOR],
            &original.flash()[SECONDARY..SECONDARY + SLOT - TRAILER_SECTOR]
        );
        assert_eq!(activated.secregs(), original.secregs());
        let mut m = Machine::from_nv(activated, &Layout::EVO_7_1).unwrap();
        let stop = m.run(crate::CPU_HZ * 5);
        assert!(
            matches!(
                stop,
                crate::StopReason::CycleLimit | crate::StopReason::Idle
            ),
            "{stop:?}"
        );
    }
}
