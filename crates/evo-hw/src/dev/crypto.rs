//! The two polled crypto blocks of the secure image (`docs/hardware/crypto.md`): the CC312-style
//! TRNG at 0x5010_A000 and the SHA-256 engine at 0x5000_2000.
//!
//! Neither block drives an IRQ (26 `TRNG_Handler` and 23 `SHA_Handler` are unexpected-IRQ stubs
//! in the firmware). Only aligned 32-bit accesses reach registers; any other width, and any
//! offset not listed below, reads 0, ignores writes and is logged as unhandled.
//!
//! **Across a System reset** the TRNG keeps its generator running (the registers return to 0,
//! the random stream continues where it was); everything else returns to its power-on state.

use serde::{Deserialize, Serialize};
use sha2::digest::generic_array::GenericArray;

use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// The fixed seed of the SoC's TRNG: the machine is deterministic (no host RNG).
pub const TRNG_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

const RNG_IMR: u32 = 0x100;
const RNG_ISR: u32 = 0x104;
const RNG_ICR: u32 = 0x108;
const TRNG_CONFIG: u32 = 0x10C;
const EHR_DATA_FIRST: u32 = 0x114;
const EHR_DATA_LAST: u32 = 0x128;
const RND_SOURCE_ENABLE: u32 = 0x12C;
const SAMPLE_CNT1: u32 = 0x130;
const TRNG_DEBUG_CONTROL: u32 = 0x138;

/// xorshift64* multiplier (Vigna).
const XORSHIFT64_STAR: u64 = 0x2545_F491_4F6C_DD1D;

/// CC312-style TRNG. Plain storage for IMR, CONFIG, SOURCE_ENABLE, SAMPLE_CNT1 and
/// DEBUG_CONTROL; ISR reads `EHR_VALID = RND_SOURCE_ENABLE & 1` with the error bits 1–3
/// always 0; ICR is write-1-to-clear on bits that never set; each EHR_DATA read draws one
/// 32-bit word from a xorshift64* generator.
///
/// A System reset clears the registers but keeps the generator state; a PowerOn reset restarts
/// the generator from its seed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trng {
    seed: u64,
    /// Generator state, never 0.
    x: u64,
    imr: u32,
    config: u32,
    enable: u32,
    sample_cnt1: u32,
    debug_control: u32,
}

/// xorshift64* has the all-zero state as a fixed point; a 0 seed uses [`TRNG_SEED`].
fn nonzero_seed(seed: u64) -> u64 {
    if seed == 0 { TRNG_SEED } else { seed }
}

impl Trng {
    /// A TRNG in its power-on state whose stream starts from `seed` (0 means [`TRNG_SEED`]).
    pub fn new(seed: u64) -> Self {
        let seed = nonzero_seed(seed);
        Trng {
            seed,
            x: seed,
            imr: 0,
            config: 0,
            enable: 0,
            sample_cnt1: 0,
            debug_control: 0,
        }
    }

    /// The next 32-bit word of the xorshift64* stream (high half of the scrambled state).
    fn draw(&mut self) -> u32 {
        let mut x = self.x;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.x = x;
        (x.wrapping_mul(XORSHIFT64_STAR) >> 32) as u32
    }
}

impl Device for Trng {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if width != 4 || !off.is_multiple_of(4) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        match off {
            RNG_IMR => self.imr,
            RNG_ISR => self.enable & 1,
            // Write-only.
            RNG_ICR => 0,
            TRNG_CONFIG => self.config,
            EHR_DATA_FIRST..=EHR_DATA_LAST => self.draw(),
            RND_SOURCE_ENABLE => self.enable,
            SAMPLE_CNT1 => self.sample_cnt1,
            TRNG_DEBUG_CONTROL => self.debug_control,
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if width != 4 || !off.is_multiple_of(4) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            RNG_IMR => self.imr = v,
            // ISR and EHR_DATA are read-only; ICR clears error bits that never set.
            RNG_ISR | RNG_ICR | EHR_DATA_FIRST..=EHR_DATA_LAST => {}
            TRNG_CONFIG => self.config = v,
            RND_SOURCE_ENABLE => self.enable = v,
            SAMPLE_CNT1 => self.sample_cnt1 = v,
            TRNG_DEBUG_CONTROL => self.debug_control = v,
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        let x = self.x;
        *self = Trng::new(self.seed);
        // The generator keeps running across SYSRESETREQ.
        if kind == ResetKind::System {
            self.x = x;
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.x == 0 || s.seed == 0 {
            return Err(StateError::Mismatch(
                "TRNG state with a zero generator state or seed".into(),
            ));
        }
        *self = s;
        Ok(())
    }
}

const SHA_CTRL: u32 = 0x00;
const SHA_W_FIRST: u32 = 0x10;
const SHA_W_LAST: u32 = 0x4C;
const SHA_H_FIRST: u32 = 0x60;
const SHA_H_LAST: u32 = 0x7C;

/// CTRL bit 1: start one block.
const CTRL_START: u32 = 1 << 1;
/// CTRL bit 2: load the SHA-256 initial hash value before the block.
const CTRL_LOAD_IV: u32 = 1 << 2;
/// CTRL bit 11: BUSY. The model hashes synchronously, so it always reads 0.
const CTRL_BUSY: u32 = 1 << 11;

/// The SHA-256 initial hash value (FIPS 180-4 §5.3.3).
const SHA256_IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// SHA-256 block engine. W[0..15] (+0x10..+0x4C, read/write) hold the message words as
/// standard big-endian SHA words; H[0..7] (+0x60..+0x7C, read-only) the running hash. A CTRL
/// write with bit 1 set loads the IV into H if bit 2 is set, then compresses W into H at once.
/// CTRL reads back what was written with BUSY (bit 11) clear. Every reset clears everything.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sha256Engine {
    ctrl: u32,
    w: [u32; 16],
    h: [u32; 8],
}

impl Sha256Engine {
    /// An engine in its power-on state (all registers 0).
    pub fn new() -> Self {
        Self::default()
    }

    /// The running hash H[0..7].
    pub fn state(&self) -> [u32; 8] {
        self.h
    }

    /// One SHA-256 compression of W into H.
    fn process(&mut self) {
        let mut block = [0u8; 64];
        for (dst, w) in block.as_chunks_mut::<4>().0.iter_mut().zip(self.w) {
            *dst = w.to_be_bytes();
        }
        sha2::compress256(&mut self.h, &[GenericArray::from(block)]);
    }
}

impl Device for Sha256Engine {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if width != 4 || !off.is_multiple_of(4) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        match off {
            SHA_CTRL => self.ctrl & !CTRL_BUSY,
            SHA_W_FIRST..=SHA_W_LAST => self.w[((off - SHA_W_FIRST) / 4) as usize],
            SHA_H_FIRST..=SHA_H_LAST => self.h[((off - SHA_H_FIRST) / 4) as usize],
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if width != 4 || !off.is_multiple_of(4) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            SHA_CTRL => {
                self.ctrl = v;
                if v & CTRL_START != 0 {
                    if v & CTRL_LOAD_IV != 0 {
                        self.h = SHA256_IV;
                    }
                    self.process();
                }
            }
            SHA_W_FIRST..=SHA_W_LAST => self.w[((off - SHA_W_FIRST) / 4) as usize] = v,
            // The digest is read-only.
            SHA_H_FIRST..=SHA_H_LAST => {}
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        *self = Sha256Engine::new();
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        *self = s;
        Ok(())
    }
}
