//! System Control Space: the memory-mapped NVIC, SCB, SysTick, MPU and SAU registers, and the
//! rest of the internal Private Peripheral Bus (PPB, `0xE000_0000..=0xE003_FFFF`), which the CPU
//! handles itself before the `Bus`. The external PPB (EPPB, `0xE004_0000..=0xE00F_FFFF`) is
//! reached over the system bus, so privileged data accesses there go to the `Bus` like any other
//! address. Unprivileged data accesses to any part of the PPB, the EPPB included, BusFault and
//! never reach the `Bus`.
//!
//! Guest data accesses to internal PPB registers this core does not implement read as zero and
//! ignore writes; each one is recorded as a `PpbMiss` for the host to collect with
//! `Cpu::take_ppb_misses`. Host accesses (`ppb_read32`, `ppb_peek32`, `ppb_write32`) record
//! nothing.
//!
//! Every access goes through a security *view* (`Sec`): software sees the copy of the banked
//! registers for its own security state, and Secure software reaches the Non-secure view through
//! the Non-secure alias at `0xE002_0000..=0xE002_FFFF` (RAZ/WI from Non-secure state).

pub mod mpu;
pub mod nvic;
pub mod sau;
pub mod scb;
pub mod systick;

use serde::{Deserialize, Serialize};

use crate::bus::{Access, BusError};
use crate::cpu::Cpu;
use crate::exception::{BUSFAULT, HARDFAULT, NMI, PENDSV, SYSTICK as SYSTICK_EXC, is_banked};
use crate::instr::MemSize;
use crate::regs::{Banked, Sec};

pub use mpu::Mpu;
pub use nvic::Nvic;
pub use sau::Sau;
pub use scb::Scb;
pub use systick::SysTick;

/// Implemented priority bits (the top bits of each 8-bit priority field).
pub const PRIORITY_BITS: u32 = 3;
/// Mask of the implemented priority bits.
pub const PRIORITY_MASK: u8 = !(0xFF >> PRIORITY_BITS);

pub const PPB_START: u32 = 0xE000_0000;
/// Last address of the internal PPB; `0xE004_0000..` (the external PPB) goes to the `Bus`.
pub const INTERNAL_PPB_END: u32 = 0xE003_FFFF;
/// Last address of the external PPB, and of the whole PPB.
pub const EPPB_END: u32 = 0xE00F_FFFF;

/// A guest data access to an internal PPB register this core does not implement: the read
/// returned zero or the write was ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpbMiss {
    /// The address accessed (before any Non-secure alias translation).
    pub addr: u32,
    /// Access width in bytes.
    pub width: u8,
    /// The value written, or `None` for a read.
    pub write: Option<u32>,
    /// True when the access came from Secure state.
    pub secure: bool,
}

/// Non-secure alias of `0xE000_0000..=0xE000_FFFF`, accessible from Secure state only.
const NS_ALIAS_START: u32 = 0xE002_0000;
const NS_ALIAS_END: u32 = 0xE002_FFFF;
const NS_ALIAS_OFFSET: u32 = 0x0002_0000;

const SCS_START: u32 = 0xE000_E000;
const SCS_END: u32 = 0xE000_EFFF;
const ICTR: u32 = 0xE000_E004;
const ACTLR: u32 = 0xE000_E008;
const CPPWR: u32 = 0xE000_E00C;
const SYSTICK: u32 = 0xE000_E010;
const NVIC: u32 = 0xE000_E100;
const NVIC_END: u32 = 0xE000_E5DF;
const CPUID: u32 = 0xE000_ED00;
const ICSR: u32 = 0xE000_ED04;
const VTOR: u32 = 0xE000_ED08;
const AIRCR: u32 = 0xE000_ED0C;
const SCR: u32 = 0xE000_ED10;
const CCR: u32 = 0xE000_ED14;
const SHPR1: u32 = 0xE000_ED18;
const SHPR3: u32 = 0xE000_ED20;
const SHCSR: u32 = 0xE000_ED24;
const CFSR: u32 = 0xE000_ED28;
const HFSR: u32 = 0xE000_ED2C;
const MMFAR: u32 = 0xE000_ED34;
const BFAR: u32 = 0xE000_ED38;
const CPACR: u32 = 0xE000_ED88;
const NSACR: u32 = 0xE000_ED8C;
const MPU: u32 = 0xE000_ED90;
const MPU_END: u32 = 0xE000_EDC7;
const SAU: u32 = 0xE000_EDD0;
const SAU_END: u32 = 0xE000_EDE8;
const STIR: u32 = 0xE000_EF00;

/// `ICTR.INTLINESNUM`: (number of IRQs / 32) - 1.
const ICTR_VALUE: u32 = (nvic::NUM_IRQS / 32 - 1) as u32;

/// Exceptions 4..=15 that have a priority field in SHPR1-3.
const SHPR_IMPLEMENTED: [bool; 12] = [
    true, true, true, true, false, false, false, true, true, false, true, true,
];

/// `NSACR` bits: CP0-CP7, CP10, CP11.
const NSACR_MASK: u32 = 0x0CFF;

/// System Control Space state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scs {
    pub nvic: Nvic,
    pub scb: Scb,
    pub systick: Banked<SysTick>,
    pub mpu: Banked<Mpu>,
    pub sau: Sau,
    /// `ACTLR` (stored).
    pub actlr: u32,
    /// `CPPWR` (stored).
    pub cppwr: u32,
}

/// True for addresses the CPU handles itself instead of the `Bus`: the internal PPB only.
pub fn is_ppb(addr: u32) -> bool {
    (PPB_START..=INTERNAL_PPB_END).contains(&addr)
}

/// True for the external PPB (`0xE004_0000..=0xE00F_FFFF`). Data accesses there go to the `Bus`,
/// but, like the rest of the PPB, only privileged ones.
pub fn is_eppb(addr: u32) -> bool {
    (INTERNAL_PPB_END + 1..=EPPB_END).contains(&addr)
}

/// The register word and security view that an access from security state `from` to PPB word
/// `word` reaches; `None` for the Non-secure alias accessed from Non-secure state (RAZ/WI).
fn ppb_view(word: u32, from: Sec) -> Option<(u32, Sec)> {
    if (NS_ALIAS_START..=NS_ALIAS_END).contains(&word) {
        (from == Sec::Secure).then_some((word - NS_ALIAS_OFFSET, Sec::NonSecure))
    } else {
        Some((word, from))
    }
}

/// The register word and view a host (debugger/test) access from security state `sec` to
/// `addr` reaches: as for `ppb_view`, except that the Non-secure alias always shows the
/// Non-secure view.
fn host_view(addr: u32, sec: Sec) -> (u32, Sec) {
    let word = addr & !3;
    ppb_view(word, sec).unwrap_or((word - NS_ALIAS_OFFSET, Sec::NonSecure))
}

fn size_bytes(size: MemSize) -> u32 {
    match size {
        MemSize::Byte => 1,
        MemSize::Half => 2,
        MemSize::Word => 4,
    }
}

/// Registers that accept byte and halfword accesses.
fn byte_accessible(word: u32) -> bool {
    (NVIC..=NVIC_END).contains(&word) || (SHPR1..=SHPR3).contains(&word) || word == CFSR
}

impl Cpu {
    /// Data read from the PPB. Unprivileged and unaligned accesses are bus errors.
    pub(crate) fn ppb_read(
        &mut self,
        addr: u32,
        size: MemSize,
        acc: Access,
    ) -> Result<u32, BusError> {
        let n = size_bytes(size);
        if addr & (n - 1) != 0 || !acc.privileged {
            return Err(BusError::Decode);
        }
        let Some((word, view)) = ppb_view(addr & !3, self.regs.sec) else {
            return Ok(0);
        };
        let shift = 8 * (addr & 3);
        let Some(v) = self.scs_read(word, view) else {
            self.ppb_miss(addr, n, None);
            return Ok(0);
        };
        Ok(if n == 4 {
            v
        } else {
            (v >> shift) & ((1 << (8 * n)) - 1)
        })
    }

    /// Data write to the PPB. Unprivileged and unaligned accesses are bus errors, except
    /// unprivileged STIR writes when `CCR.USERSETMPEND` is set.
    pub(crate) fn ppb_write(
        &mut self,
        addr: u32,
        v: u32,
        size: MemSize,
        acc: Access,
    ) -> Result<(), BusError> {
        let n = size_bytes(size);
        let view = ppb_view(addr & !3, self.regs.sec);
        let user_stir = view.is_some_and(|(word, sec)| {
            word == STIR && n == 4 && self.scs.scb.ccr.get(sec) & scb::CCR_USERSETMPEND != 0
        });
        if addr & (n - 1) != 0 || !(acc.privileged || user_stir) {
            return Err(BusError::Decode);
        }
        let Some((word, view)) = view else {
            return Ok(());
        };
        let done = if n == 4 {
            self.scs_write(word, v, u32::MAX, view)
        } else if byte_accessible(word) {
            let shift = 8 * (addr & 3);
            let lanes = ((1u32 << (8 * n)) - 1) << shift;
            self.scs_write(word, (v << shift) & lanes, lanes, view)
        } else {
            false
        };
        if !done {
            self.ppb_miss(addr, n, Some(v));
        }
        Ok(())
    }

    /// Record a guest data access to an unimplemented internal PPB register (or a sub-word
    /// write to a word-only one).
    fn ppb_miss(&mut self, addr: u32, n: u32, write: Option<u32>) {
        let secure = self.regs.sec == Sec::Secure;
        log::debug!(
            "PPB: {n}-byte {} {addr:#010x} not implemented ({}, pc {:#010x})",
            write.map_or("read of".to_string(), |v| format!("write of {v:#x} to")),
            if secure { "Secure" } else { "Non-secure" },
            self.regs.pc
        );
        self.ppb_misses.push(PpbMiss {
            addr,
            width: n as u8,
            write,
            secure,
        });
    }

    /// Drain the guest data accesses to unimplemented internal PPB registers recorded since
    /// the last call, oldest first. Host accesses are never recorded.
    pub fn take_ppb_misses(&mut self) -> Vec<PpbMiss> {
        std::mem::take(&mut self.ppb_misses)
    }

    /// Host (debugger/test) read of a PPB word: privileged, no fault, in the current security
    /// state's view; the Non-secure alias (`0xE002_xxxx`) always reads the Non-secure view.
    pub fn ppb_read32(&mut self, addr: u32) -> u32 {
        let (word, view) = host_view(addr, self.regs.sec);
        self.scs_read(word, view).unwrap_or_else(|| {
            log::debug!("PPB: host read of unimplemented register {word:#010x} ({view:?} view)");
            0
        })
    }

    /// Side-effect-free host read for a debugger: what `ppb_read32` would return for the word
    /// containing `addr` if the CPU were in security state `sec`, except that reading
    /// `SYST_CSR` leaves COUNTFLAG set. `None` outside the internal PPB and for registers that
    /// are not implemented (which `ppb_read32` reads as zero; this logs nothing).
    pub fn ppb_peek32(&self, addr: u32, sec: Sec) -> Option<u32> {
        if !is_ppb(addr) {
            return None;
        }
        let (word, view) = host_view(addr, sec);
        if (SCS_START..=SCS_END).contains(&word) {
            self.scs_reg_peek(word, view)
        } else {
            None
        }
    }

    /// Host (debugger/test) write of a PPB word, with the views of `ppb_read32`.
    pub fn ppb_write32(&mut self, addr: u32, v: u32) {
        let (word, view) = host_view(addr, self.regs.sec);
        if !self.scs_write(word, v, u32::MAX, view) {
            log::debug!(
                "PPB: host write of {v:#x} to unimplemented register {word:#010x} ({view:?} view)"
            );
        }
    }

    /// Consume a latched `AIRCR.SYSRESETREQ` (the machine resets the system when it sees one).
    pub fn take_sysreset_request(&mut self) -> bool {
        std::mem::take(&mut self.scs.scb.sysresetreq)
    }

    /// Set `VTOR` for `sec`, also used as that state's vector table at the next reset.
    pub fn set_vtor(&mut self, sec: Sec, v: u32) {
        let v = v & !0x7F;
        *self.scs.scb.vtor.get_mut(sec) = v;
        *self.scs.scb.init_vtor.get_mut(sec) = v;
    }

    /// Read the PPB word at `word` in view `view`; `None` for an unimplemented register.
    fn scs_read(&mut self, word: u32, view: Sec) -> Option<u32> {
        // The one read with a side effect: reading SYST_CSR clears COUNTFLAG.
        if word == SYSTICK {
            return Some(self.scs.systick.get_mut(view).read(0));
        }
        if (SCS_START..=SCS_END).contains(&word) {
            self.scs_reg_peek(word, view)
        } else {
            None
        }
    }

    /// Write the PPB word at `word` in view `view`; `v` is already masked to the byte `lanes`
    /// being written. False when the register is not implemented (the write is ignored).
    fn scs_write(&mut self, word: u32, v: u32, lanes: u32, view: Sec) -> bool {
        (SCS_START..=SCS_END).contains(&word) && self.scs_reg_write(word, v, lanes, view)
    }

    /// True when view `view` reaches the non-banked BusFault/NMI state: always from Secure,
    /// from Non-secure only while `AIRCR.BFHFNMINS` hands those exceptions to Non-secure state.
    fn sees_bf_nmi(&self, view: Sec) -> bool {
        view == Sec::Secure || self.scs.scb.bfhfnmins
    }

    /// Which copy of the SHPR byte for exception `4 + k` view `view` reaches, if any.
    fn shpr_copy(&self, k: usize, view: Sec) -> Option<Sec> {
        let num = 4 + k as u16;
        if !SHPR_IMPLEMENTED[k] {
            None
        } else if is_banked(num) {
            Some(view)
        } else if view == Sec::Secure || (num == BUSFAULT && self.scs.scb.bfhfnmins) {
            Some(Sec::Secure)
        } else {
            // SecureFault and DebugMonitor priorities are RAZ/WI from Non-secure.
            None
        }
    }

    /// The value of the SCS register at `word` in view `view`, without read side effects (see
    /// `scs_read`); `None` if it is not implemented.
    fn scs_reg_peek(&self, word: u32, view: Sec) -> Option<u32> {
        let secure = view == Sec::Secure;
        let bf = self.sees_bf_nmi(view);
        let scb = &self.scs.scb;
        Some(match word {
            ICTR => ICTR_VALUE,
            ACTLR => self.scs.actlr,
            CPPWR => self.scs.cppwr,
            SYSTICK..=0xE000_E01C => self.scs.systick.get(view).peek(word - SYSTICK),
            NVIC..=NVIC_END => return self.scs.nvic.read(word - NVIC, !secure),
            CPUID => scb::CPUID,
            ICSR => self.icsr(view),
            VTOR => *scb.vtor.get(view),
            AIRCR => {
                // PRIS and SYSRESETREQS are RAZ from Non-secure; BFHFNMINS is readable.
                let mut v = (scb::VECTKEYSTAT << 16)
                    | (u32::from(scb.bfhfnmins) << 13)
                    | (u32::from(*scb.prigroup.get(view)) << 8);
                if secure {
                    v |= (u32::from(scb.pris) << 14) | (u32::from(scb.sysresetreqs) << 3);
                }
                v
            }
            SCR => *scb.scr.get(view),
            CCR => *scb.ccr.get(view),
            SHPR1..=SHPR3 => {
                let base = (word - SHPR1) as usize;
                u32::from_le_bytes(std::array::from_fn(|k| {
                    self.shpr_copy(base + k, view)
                        .map_or(0, |copy| scb.shpr.get(copy)[base + k])
                }))
            }
            SHCSR => self.shcsr(view),
            CFSR => {
                let bfsr = if bf { scb.bfsr } else { 0 };
                u32::from(*scb.mmfsr.get(view))
                    | (u32::from(bfsr) << 8)
                    | (u32::from(*scb.ufsr.get(view)) << 16)
            }
            HFSR => scb.hfsr,
            MMFAR => *scb.mmfar.get(view),
            BFAR if bf => scb.bfar,
            BFAR => 0,
            CPACR => *scb.cpacr.get(view),
            NSACR => scb.nsacr,
            MPU..=MPU_END => return self.scs.mpu.get(view).read(word - MPU),
            // The SAU and SFSR/SFAR are RAZ/WI from Non-secure.
            SAU..=SAU_END if secure => return self.scs.sau.read(word - SAU),
            SAU..=SAU_END => 0,
            STIR => 0,
            _ => return None,
        })
    }

    /// Returns false for unimplemented registers.
    fn scs_reg_write(&mut self, word: u32, v: u32, lanes: u32, view: Sec) -> bool {
        let secure = view == Sec::Secure;
        let bf = self.sees_bf_nmi(view);
        match word {
            ICTR | CPUID => {}
            ACTLR => self.scs.actlr = v,
            CPPWR => self.scs.cppwr = v,
            SYSTICK..=0xE000_E01C => self.scs.systick.get_mut(view).write(word - SYSTICK, v),
            NVIC..=NVIC_END => {
                let nvic = &mut self.scs.nvic;
                let Some(newly) = nvic.write(word - NVIC, v, lanes, PRIORITY_MASK, !secure) else {
                    return false;
                };
                if newly != 0 {
                    self.on_irq_pended();
                }
            }
            ICSR => self.write_icsr(v, view),
            VTOR => *self.scs.scb.vtor.get_mut(view) = v & !0x7F,
            AIRCR => self.write_aircr(v, view),
            SCR => {
                // SLEEPDEEPS is Secure-only.
                let mask = if secure {
                    scb::SCR_WRITABLE
                } else {
                    scb::SCR_WRITABLE & !(1 << 3)
                };
                *self.scs.scb.scr.get_mut(view) = v & mask;
            }
            CCR => {
                *self.scs.scb.ccr.get_mut(view) = (v & scb::CCR_WRITABLE) | scb::CCR_RESET;
            }
            SHPR1..=SHPR3 => {
                let base = (word - SHPR1) as usize;
                for k in 0..4 {
                    if lanes >> (8 * k) & 0xFF == 0 {
                        continue;
                    }
                    if let Some(copy) = self.shpr_copy(base + k, view) {
                        self.scs.scb.shpr.get_mut(copy)[base + k] =
                            (v >> (8 * k)) as u8 & PRIORITY_MASK;
                    }
                }
            }
            SHCSR => self.write_shcsr(v, view),
            CFSR => {
                let scb = &mut self.scs.scb;
                *scb.mmfsr.get_mut(view) &= !(v as u8);
                if bf {
                    scb.bfsr &= !((v >> 8) as u8);
                }
                *scb.ufsr.get_mut(view) &= !((v >> 16) as u16);
            }
            HFSR => self.scs.scb.hfsr &= !v,
            MMFAR => *self.scs.scb.mmfar.get_mut(view) = v,
            BFAR => {
                if bf {
                    self.scs.scb.bfar = v;
                }
            }
            CPACR => *self.scs.scb.cpacr.get_mut(view) = v & 0x00F0_FFFF,
            NSACR => {
                if secure {
                    self.scs.scb.nsacr = v & NSACR_MASK;
                }
            }
            MPU..=MPU_END => return self.scs.mpu.get_mut(view).write(word - MPU, v),
            SAU..=SAU_END if secure => return self.scs.sau.write(word - SAU, v),
            SAU..=SAU_END => {}
            STIR => {
                // Non-secure software can only pend interrupts that target Non-secure state.
                let irq = (v & 0x1FF) as usize;
                if irq < nvic::NUM_IRQS && (secure || !self.scs.nvic.targets_secure(irq)) {
                    self.pend_irq(irq);
                }
            }
            _ => return false,
        }
        true
    }

    fn icsr(&self, view: Sec) -> u32 {
        let mut v = u32::from(self.regs.xpsr.exception) & 0x1FF;
        if self.active_count() <= 1 {
            v |= 1 << 11; // RETTOBASE
        }
        if let Some((num, _)) = self.highest_pending() {
            v |= u32::from(num) << 12; // VECTPENDING
        }
        if self.scs.nvic.pending.iter().any(|&w| w != 0) {
            v |= 1 << 22; // ISRPENDING
        }
        if self.exc_pending(SYSTICK_EXC, view) {
            v |= 1 << 26; // PENDSTSET
        }
        if self.exc_pending(PENDSV, view) {
            v |= 1 << 28; // PENDSVSET
        }
        if self.sees_bf_nmi(view) && self.exc_pending(NMI, view) {
            v |= 1 << 31; // PENDNMISET
        }
        v
    }

    fn write_icsr(&mut self, v: u32, view: Sec) {
        // PENDNMISET/CLR are WI from Non-secure unless NMI targets Non-secure state.
        if self.sees_bf_nmi(view) {
            if v & (1 << 31) != 0 {
                self.set_exc_pending(NMI, view, true);
            } else if v & (1 << 30) != 0 {
                self.set_exc_pending(NMI, view, false);
            }
        }
        if v & (1 << 28) != 0 {
            self.set_exc_pending(PENDSV, view, true);
        } else if v & (1 << 27) != 0 {
            self.set_exc_pending(PENDSV, view, false);
        }
        if v & (1 << 26) != 0 {
            self.set_exc_pending(SYSTICK_EXC, view, true);
        } else if v & (1 << 25) != 0 {
            self.set_exc_pending(SYSTICK_EXC, view, false);
        }
    }

    fn write_aircr(&mut self, v: u32, view: Sec) {
        if v >> 16 != scb::VECTKEY {
            return;
        }
        let scb = &mut self.scs.scb;
        *scb.prigroup.get_mut(view) = (v >> 8 & 7) as u8;
        if view == Sec::Secure {
            scb.pris = v & (1 << 14) != 0;
            scb.bfhfnmins = v & (1 << 13) != 0;
            scb.sysresetreqs = v & (1 << 3) != 0;
        }
        if v & (1 << 2) != 0 && (view == Sec::Secure || !scb.sysresetreqs) {
            log::info!("AIRCR.SYSRESETREQ written (pc {:#010x})", self.regs.pc);
            scb.sysresetreq = true;
        }
    }

    /// Whether view `view` sees the SHCSR state bits of exception `num`: the Secure view sees
    /// all of them; the Non-secure view sees the banked exceptions, and BusFault and NMI only
    /// with `AIRCR.BFHFNMINS` (never SecureFault or DebugMonitor).
    fn shcsr_visible(&self, num: u16, view: Sec) -> bool {
        view == Sec::Secure
            || is_banked(num)
            || (matches!(num, BUSFAULT | NMI) && self.sees_bf_nmi(view))
    }

    fn shcsr(&self, view: Sec) -> u32 {
        let scb = &self.scs.scb;
        let mut v = scb.shcsr_ena.get(view) & (scb::MEMFAULTENA | scb::USGFAULTENA);
        if view == Sec::Secure {
            v |= scb.shcsr_ena.s & (scb::BUSFAULTENA | scb::SECUREFAULTENA);
        } else if scb.bfhfnmins {
            v |= scb.shcsr_ena.s & scb::BUSFAULTENA;
        }
        for (bit, num, pending) in scb::SHCSR_STATE_BITS {
            if !self.shcsr_visible(num, view) {
                continue;
            }
            let set = if pending {
                self.exc_pending(num, view)
            } else {
                self.exc_active(num, view)
            };
            if set {
                v |= 1 << bit;
            }
        }
        if self.exc_pending(HARDFAULT, view) {
            v |= scb::SHCSR_HARDFAULTPENDED;
        }
        v
    }

    fn write_shcsr(&mut self, v: u32, view: Sec) {
        let banked = scb::MEMFAULTENA | scb::USGFAULTENA;
        let bfhfnmins = self.scs.scb.bfhfnmins;
        let ena = &mut self.scs.scb.shcsr_ena;
        *ena.get_mut(view) = v & banked;
        if view == Sec::Secure {
            ena.s |= v & (scb::BUSFAULTENA | scb::SECUREFAULTENA);
        } else if bfhfnmins {
            ena.s = (ena.s & !scb::BUSFAULTENA) | (v & scb::BUSFAULTENA);
        }
        // The active and pending bits are writable (used by context-switching software), except
        // NMIACT and HARDFAULTACT.
        for (bit, num, pending) in scb::SHCSR_STATE_BITS {
            if matches!(num, NMI | HARDFAULT) || !self.shcsr_visible(num, view) {
                continue;
            }
            let set = v & (1 << bit) != 0;
            if pending {
                self.set_exc_pending(num, view, set);
            } else {
                self.set_exc_active(num, view, set);
            }
        }
    }

    /// Mark external interrupt `irq` pending.
    pub(crate) fn pend_irq(&mut self, irq: usize) {
        if !self.scs.nvic.is_pending(irq) {
            self.scs.nvic.set_pending(irq, true);
            self.on_irq_pended();
        }
    }

    /// `SCR.SEVONPEND`: an interrupt becoming pending sets the event register.
    fn on_irq_pended(&mut self) {
        if self.scs.scb.scr.get(self.regs.sec) & scb::SCR_SEVONPEND != 0 {
            self.event = true;
        }
    }

    /// Level-sensitive interrupt line from a peripheral. Asserting it pends the IRQ; while it
    /// stays asserted the IRQ pends again when its handler returns.
    pub fn set_irq_level(&mut self, irq: u16, asserted: bool) {
        let irq = usize::from(irq);
        if irq >= nvic::NUM_IRQS {
            log::warn!("IRQ {irq} out of range; ignored");
            return;
        }
        let was = self.scs.nvic.level(irq);
        self.scs.nvic.set_level(irq, asserted);
        if asserted && !was {
            self.pend_irq(irq);
        }
    }

    /// Edge-triggered interrupt from a peripheral: pends the IRQ.
    pub fn pulse_irq(&mut self, irq: u16) {
        let irq = usize::from(irq);
        if irq >= nvic::NUM_IRQS {
            log::warn!("IRQ {irq} out of range; ignored");
            return;
        }
        self.pend_irq(irq);
    }

    /// Advance both SysTick timers by `cycles`, pending SysTick in each state that wrapped with
    /// TICKINT set. `Cpu::step` calls this with each instruction's cycles.
    pub fn tick_systick(&mut self, cycles: u32) {
        for sec in [Sec::Secure, Sec::NonSecure] {
            if self.scs.systick.get_mut(sec).tick(cycles) {
                self.set_exc_pending(SYSTICK_EXC, sec, true);
            }
        }
    }

    /// The fewest cycles of `tick_systick` after which a SysTick exception (either security
    /// state) is pended, or `None` if neither counter will pend one. Lets a sleeping machine
    /// skip ahead to the next SysTick instead of stepping through the wait.
    pub fn cycles_to_systick(&self) -> Option<u32> {
        [Sec::Secure, Sec::NonSecure]
            .into_iter()
            .filter_map(|sec| self.scs.systick.get(sec).cycles_to_interrupt())
            .min()
    }
}
