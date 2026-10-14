//! System Control Block state.
//!
//! Registers that the Security Extension banks are stored as `Banked`. For registers that are
//! only partially banked (SHPR1-3, SHCSR, CCR, the fault status registers) the non-banked fields
//! live in the Secure copy; the Non-secure view reaches them only where the architecture allows
//! (mostly when `AIRCR.BFHFNMINS` hands BusFault, HardFault and NMI to Non-secure state).

use serde::{Deserialize, Deserializer, Serialize};

use crate::regs::Banked;

/// `CPUID`: Arm Cortex-M33 r0p3.
pub const CPUID: u32 = 0x410F_D213;
/// `AIRCR.VECTKEY` value required on writes.
pub const VECTKEY: u32 = 0x05FA;
/// `AIRCR.VECTKEYSTAT` value returned on reads.
pub const VECTKEYSTAT: u32 = 0xFA05;
/// `CCR` reset value: STKALIGN and bit 0 are RES1 in ARMv8-M.
pub const CCR_RESET: u32 = 0x0000_0201;

// CCR fields.
pub const CCR_USERSETMPEND: u32 = 1 << 1;
pub const CCR_UNALIGN_TRP: u32 = 1 << 3;
pub const CCR_DIV_0_TRP: u32 = 1 << 4;
/// Stack limit violations are ignored at a negative execution priority (HardFault, NMI).
pub const CCR_STKOFHFNMIGN: u32 = 1 << 10;
/// Writable CCR bits: USERSETMPEND, UNALIGN_TRP, DIV_0_TRP, BFHFNMIGN, STKOFHFNMIGN, DC, IC, BP.
pub const CCR_WRITABLE: u32 = (1 << 1) | (1 << 3) | (1 << 4) | (1 << 8) | (1 << 10) | (7 << 16);

// SCR fields (SLEEPONEXIT, SLEEPDEEP, SLEEPDEEPS, SEVONPEND).
pub const SCR_SLEEPONEXIT: u32 = 1 << 1;
pub const SCR_SEVONPEND: u32 = 1 << 4;
pub const SCR_WRITABLE: u32 = 0b1_1110;

// HFSR fields.
pub const HFSR_VECTTBL: u32 = 1 << 1;
pub const HFSR_FORCED: u32 = 1 << 30;

// MMFSR / BFSR / UFSR fields.
pub const MMFSR_IACCVIOL: u8 = 1 << 0;
pub const BFSR_IBUSERR: u8 = 1 << 0;
pub const BFSR_PRECISERR: u8 = 1 << 1;
pub const BFSR_UNSTKERR: u8 = 1 << 3;
pub const BFSR_STKERR: u8 = 1 << 4;
pub const BFSR_BFARVALID: u8 = 1 << 7;
pub const UFSR_UNDEFINSTR: u16 = 1 << 0;
pub const UFSR_INVSTATE: u16 = 1 << 1;
pub const UFSR_INVPC: u16 = 1 << 2;
pub const UFSR_NOCP: u16 = 1 << 3;
pub const UFSR_STKOF: u16 = 1 << 4;
pub const UFSR_UNALIGNED: u16 = 1 << 8;
pub const UFSR_DIVBYZERO: u16 = 1 << 9;

// SHCSR enable bits.
pub const MEMFAULTENA: u32 = 1 << 16;
pub const BUSFAULTENA: u32 = 1 << 17;
pub const USGFAULTENA: u32 = 1 << 18;
pub const SECUREFAULTENA: u32 = 1 << 19;

/// SHCSR active/pending bits as (bit, exception number, pending?).
pub const SHCSR_STATE_BITS: [(u32, u16, bool); 15] = [
    (0, 4, false),   // MEMFAULTACT
    (1, 5, false),   // BUSFAULTACT
    (2, 3, false),   // HARDFAULTACT
    (3, 6, false),   // USGFAULTACT
    (4, 7, false),   // SECUREFAULTACT
    (5, 2, false),   // NMIACT
    (7, 11, false),  // SVCALLACT
    (8, 12, false),  // MONITORACT
    (10, 14, false), // PENDSVACT
    (11, 15, false), // SYSTICKACT
    (12, 6, true),   // USGFAULTPENDED
    (13, 4, true),   // MEMFAULTPENDED
    (14, 5, true),   // BUSFAULTPENDED
    (15, 11, true),  // SVCALLPENDED
    (20, 7, true),   // SECUREFAULTPENDED
];
/// HARDFAULTPENDED (read-only here).
pub const SHCSR_HARDFAULTPENDED: u32 = 1 << 21;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scb {
    /// `VTOR` per security state.
    pub vtor: Banked<u32>,
    /// Vector table base loaded into `VTOR` at reset (the INITSVTOR/INITNSVTOR inputs).
    pub init_vtor: Banked<u32>,
    /// `AIRCR.PRIGROUP` per security state: 0..=7. The priority grouping shifts by it, so
    /// decoding a larger value fails.
    #[serde(deserialize_with = "prigroups")]
    pub prigroup: Banked<u8>,
    /// `AIRCR.PRIS` (Secure only): Non-secure priorities are de-prioritised.
    pub pris: bool,
    /// `AIRCR.BFHFNMINS` (Secure only): BusFault, HardFault and NMI target Non-secure.
    pub bfhfnmins: bool,
    /// `AIRCR.SYSRESETREQS` (Secure only).
    pub sysresetreqs: bool,
    /// Latched `AIRCR.SYSRESETREQ`, consumed by the machine.
    pub sysresetreq: bool,
    pub scr: Banked<u32>,
    pub ccr: Banked<u32>,
    /// System handler priority bytes for exceptions 4..=15 (`SHPR1-3`).
    pub shpr: Banked<[u8; 12]>,
    /// `SHCSR` enable bits (bits 16..19).
    pub shcsr_ena: Banked<u32>,
    /// Pending state of exceptions 1..=15 (bit n = exception n).
    pub pending: Banked<u16>,
    /// Active state of exceptions 1..=15 (bit n = exception n).
    pub active: Banked<u16>,
    pub mmfsr: Banked<u8>,
    pub bfsr: u8,
    pub ufsr: Banked<u16>,
    pub hfsr: u32,
    pub mmfar: Banked<u32>,
    pub bfar: u32,
    /// `CPACR` (stored; no coprocessors are implemented).
    pub cpacr: Banked<u32>,
    /// `NSACR` (stored; Secure-writable, Non-secure read-only).
    pub nsacr: u32,
}

/// Decodes `Scb::prigroup`, rejecting values above 7 (save states are untrusted).
fn prigroups<'de, D: Deserializer<'de>>(d: D) -> Result<Banked<u8>, D::Error> {
    let v = Banked::<u8>::deserialize(d)?;
    for p in [v.s, v.ns] {
        if p > 7 {
            return Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Unsigned(u64::from(p)),
                &"AIRCR.PRIGROUP 0..=7",
            ));
        }
    }
    Ok(v)
}

impl Default for Scb {
    fn default() -> Self {
        Self {
            vtor: Banked::default(),
            init_vtor: Banked::default(),
            prigroup: Banked::default(),
            pris: false,
            bfhfnmins: false,
            sysresetreqs: false,
            sysresetreq: false,
            scr: Banked::default(),
            ccr: Banked {
                s: CCR_RESET,
                ns: CCR_RESET,
            },
            shpr: Banked::default(),
            shcsr_ena: Banked::default(),
            pending: Banked::default(),
            active: Banked::default(),
            mmfsr: Banked::default(),
            bfsr: 0,
            ufsr: Banked::default(),
            hfsr: 0,
            mmfar: Banked::default(),
            bfar: 0,
            cpacr: Banked::default(),
            nsacr: 0,
        }
    }
}
