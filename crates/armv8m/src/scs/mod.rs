//! System Control Space: the memory-mapped NVIC, SCB, SysTick and MPU registers, and the rest
//! of the Private Peripheral Bus (PPB, `0xE000_0000..=0xE00F_FFFF`), which the CPU handles
//! itself before the `Bus`.

pub mod mpu;
pub mod nvic;
pub mod scb;
pub mod systick;

use serde::{Deserialize, Serialize};

use crate::bus::{Access, BusError};
use crate::cpu::Cpu;
use crate::instr::MemSize;
use crate::regs::{Banked, Sec};

pub use mpu::Mpu;
pub use nvic::Nvic;
pub use scb::Scb;
pub use systick::SysTick;

/// Implemented priority bits (the top bits of each 8-bit priority field).
pub const PRIORITY_BITS: u32 = 3;
/// Mask of the implemented priority bits.
pub const PRIORITY_MASK: u8 = !(0xFF >> PRIORITY_BITS);

pub const PPB_START: u32 = 0xE000_0000;
pub const PPB_END: u32 = 0xE00F_FFFF;

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
const MPU: u32 = 0xE000_ED90;
const MPU_END: u32 = 0xE000_EDC7;
const STIR: u32 = 0xE000_EF00;

/// `ICTR.INTLINESNUM`: (number of IRQs / 32) - 1.
const ICTR_VALUE: u32 = (nvic::NUM_IRQS / 32 - 1) as u32;

/// Exceptions 4..=15 that have a priority field in SHPR1-3.
const SHPR_IMPLEMENTED: [bool; 12] = [
    true, true, true, true, false, false, false, true, true, false, true, true,
];

/// System Control Space state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scs {
    pub nvic: Nvic,
    pub scb: Scb,
    pub systick: Banked<SysTick>,
    pub mpu: Banked<Mpu>,
    /// `ACTLR` (stored).
    pub actlr: u32,
    /// `CPPWR` (stored).
    pub cppwr: u32,
}

/// True for addresses the CPU handles itself instead of the `Bus`.
pub fn is_ppb(addr: u32) -> bool {
    (PPB_START..=PPB_END).contains(&addr)
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
        let word = addr & !3;
        let shift = 8 * (addr & 3);
        let v = self.scs_read(word);
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
        let sec = self.regs.sec;
        let user_stir =
            addr == STIR && n == 4 && self.scs.scb.ccr.get(sec) & scb::CCR_USERSETMPEND != 0;
        if addr & (n - 1) != 0 || !(acc.privileged || user_stir) {
            return Err(BusError::Decode);
        }
        let word = addr & !3;
        if n == 4 {
            self.scs_write(word, v, u32::MAX);
        } else if byte_accessible(word) {
            let shift = 8 * (addr & 3);
            let lanes = ((1u32 << (8 * n)) - 1) << shift;
            self.scs_write(word, (v << shift) & lanes, lanes);
        } else {
            log::warn!("PPB: {n}-byte write of {v:#x} to {addr:#010x} ignored (word access only)");
        }
        Ok(())
    }

    /// Host (debugger/test) read of a PPB word: privileged, current security state, no fault.
    pub fn ppb_read32(&mut self, addr: u32) -> u32 {
        self.scs_read(addr & !3)
    }

    /// Host (debugger/test) write of a PPB word: privileged, current security state, no fault.
    pub fn ppb_write32(&mut self, addr: u32, v: u32) {
        self.scs_write(addr & !3, v, u32::MAX);
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

    /// Read the PPB word at `word`; unimplemented registers read as zero with a warning.
    fn scs_read(&mut self, word: u32) -> u32 {
        let v = if (SCS_START..=SCS_END).contains(&word) {
            self.scs_reg_read(word)
        } else {
            None
        };
        v.unwrap_or_else(|| {
            log::warn!(
                "PPB: read of unimplemented register {word:#010x} (pc {:#010x})",
                self.regs.pc
            );
            0
        })
    }

    /// Write the PPB word at `word`; `v` is already masked to the byte `lanes` being written.
    fn scs_write(&mut self, word: u32, v: u32, lanes: u32) {
        let done = (SCS_START..=SCS_END).contains(&word) && self.scs_reg_write(word, v, lanes);
        if !done {
            log::warn!(
                "PPB: write of {v:#x} to unimplemented register {word:#010x} (pc {:#010x})",
                self.regs.pc
            );
        }
    }

    fn scs_reg_read(&mut self, word: u32) -> Option<u32> {
        let sec = self.regs.sec;
        let scb = &self.scs.scb;
        Some(match word {
            ICTR => ICTR_VALUE,
            ACTLR => self.scs.actlr,
            CPPWR => self.scs.cppwr,
            SYSTICK..=0xE000_E01C => self.scs.systick.get_mut(sec).read(word - SYSTICK),
            NVIC..=NVIC_END => return self.scs.nvic.read(word - NVIC),
            CPUID => scb::CPUID,
            ICSR => self.icsr(),
            VTOR => *scb.vtor.get(sec),
            AIRCR => {
                (scb::VECTKEYSTAT << 16)
                    | (u32::from(scb.pris) << 14)
                    | (u32::from(scb.bfhfnmins) << 13)
                    | (u32::from(*scb.prigroup.get(sec)) << 8)
                    | (u32::from(scb.sysresetreqs) << 3)
            }
            SCR => *scb.scr.get(sec),
            CCR => *scb.ccr.get(sec),
            SHPR1..=SHPR3 => {
                let base = (word - SHPR1) as usize;
                u32::from_le_bytes(std::array::from_fn(|k| scb.shpr.get(sec)[base + k]))
            }
            SHCSR => self.shcsr(),
            CFSR => {
                u32::from(*scb.mmfsr.get(sec))
                    | (u32::from(scb.bfsr) << 8)
                    | (u32::from(*scb.ufsr.get(sec)) << 16)
            }
            HFSR => scb.hfsr,
            MMFAR => *scb.mmfar.get(sec),
            BFAR => scb.bfar,
            CPACR => *scb.cpacr.get(sec),
            MPU..=MPU_END => return self.scs.mpu.get(sec).read(word - MPU),
            STIR => 0,
            _ => return None,
        })
    }

    /// Returns false for unimplemented registers.
    fn scs_reg_write(&mut self, word: u32, v: u32, lanes: u32) -> bool {
        let sec = self.regs.sec;
        match word {
            ICTR | CPUID => {}
            ACTLR => self.scs.actlr = v,
            CPPWR => self.scs.cppwr = v,
            SYSTICK..=0xE000_E01C => self.scs.systick.get_mut(sec).write(word - SYSTICK, v),
            NVIC..=NVIC_END => {
                let Some(newly) = self.scs.nvic.write(word - NVIC, v, lanes, PRIORITY_MASK) else {
                    return false;
                };
                if newly != 0 {
                    self.on_irq_pended();
                }
            }
            ICSR => self.write_icsr(v),
            VTOR => *self.scs.scb.vtor.get_mut(sec) = v & !0x7F,
            AIRCR => self.write_aircr(v),
            SCR => {
                let mask = if sec == Sec::Secure {
                    scb::SCR_WRITABLE
                } else {
                    scb::SCR_WRITABLE & !(1 << 3)
                };
                *self.scs.scb.scr.get_mut(sec) = v & mask;
            }
            CCR => {
                *self.scs.scb.ccr.get_mut(sec) = (v & scb::CCR_WRITABLE) | scb::CCR_RESET;
            }
            SHPR1..=SHPR3 => {
                let base = (word - SHPR1) as usize;
                let shpr = self.scs.scb.shpr.get_mut(sec);
                for k in 0..4 {
                    if lanes >> (8 * k) & 0xFF != 0 && SHPR_IMPLEMENTED[base + k] {
                        shpr[base + k] = (v >> (8 * k)) as u8 & PRIORITY_MASK;
                    }
                }
            }
            SHCSR => self.write_shcsr(v),
            CFSR => {
                let scb = &mut self.scs.scb;
                *scb.mmfsr.get_mut(sec) &= !(v as u8);
                scb.bfsr &= !((v >> 8) as u8);
                *scb.ufsr.get_mut(sec) &= !((v >> 16) as u16);
            }
            HFSR => self.scs.scb.hfsr &= !v,
            MMFAR => *self.scs.scb.mmfar.get_mut(sec) = v,
            BFAR => self.scs.scb.bfar = v,
            CPACR => *self.scs.scb.cpacr.get_mut(sec) = v & 0x00F0_FFFF,
            MPU..=MPU_END => return self.scs.mpu.get_mut(sec).write(word - MPU, v),
            STIR => {
                let irq = (v & 0x1FF) as usize;
                if irq < nvic::NUM_IRQS {
                    self.pend_irq(irq);
                }
            }
            _ => return false,
        }
        true
    }

    fn icsr(&self) -> u32 {
        let sec = self.regs.sec;
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
        if self.exc_pending(15, sec) {
            v |= 1 << 26; // PENDSTSET
        }
        if self.exc_pending(14, sec) {
            v |= 1 << 28; // PENDSVSET
        }
        if self.exc_pending(2, sec) {
            v |= 1 << 31; // PENDNMISET
        }
        v
    }

    fn write_icsr(&mut self, v: u32) {
        let sec = self.regs.sec;
        // Task 8: PENDNMISET/CLR from Non-secure only when AIRCR.BFHFNMINS is set.
        if v & (1 << 31) != 0 {
            self.set_exc_pending(2, sec, true);
        } else if v & (1 << 30) != 0 {
            self.set_exc_pending(2, sec, false);
        }
        if v & (1 << 28) != 0 {
            self.set_exc_pending(14, sec, true);
        } else if v & (1 << 27) != 0 {
            self.set_exc_pending(14, sec, false);
        }
        if v & (1 << 26) != 0 {
            self.set_exc_pending(15, sec, true);
        } else if v & (1 << 25) != 0 {
            self.set_exc_pending(15, sec, false);
        }
    }

    fn write_aircr(&mut self, v: u32) {
        if v >> 16 != scb::VECTKEY {
            return;
        }
        let sec = self.regs.sec;
        let scb = &mut self.scs.scb;
        *scb.prigroup.get_mut(sec) = (v >> 8 & 7) as u8;
        if sec == Sec::Secure {
            scb.pris = v & (1 << 14) != 0;
            scb.bfhfnmins = v & (1 << 13) != 0;
            scb.sysresetreqs = v & (1 << 3) != 0;
        }
        if v & (1 << 2) != 0 && (sec == Sec::Secure || !scb.sysresetreqs) {
            log::info!("AIRCR.SYSRESETREQ written (pc {:#010x})", self.regs.pc);
            scb.sysresetreq = true;
        }
    }

    fn shcsr(&self) -> u32 {
        let sec = self.regs.sec;
        let scb = &self.scs.scb;
        let mut v = scb.shcsr_ena.get(sec) & (scb::MEMFAULTENA | scb::USGFAULTENA)
            | scb.shcsr_ena.s & (scb::BUSFAULTENA | scb::SECUREFAULTENA);
        for (bit, num, pending) in scb::SHCSR_STATE_BITS {
            let set = if pending {
                self.exc_pending(num, sec)
            } else {
                self.exc_active(num, sec)
            };
            if set {
                v |= 1 << bit;
            }
        }
        if self.exc_pending(3, sec) {
            v |= scb::SHCSR_HARDFAULTPENDED;
        }
        v
    }

    fn write_shcsr(&mut self, v: u32) {
        let sec = self.regs.sec;
        let banked = scb::MEMFAULTENA | scb::USGFAULTENA;
        let scb = &mut self.scs.scb;
        *scb.shcsr_ena.get_mut(sec) = v & banked;
        if sec == Sec::Secure {
            scb.shcsr_ena.s |= v & (scb::BUSFAULTENA | scb::SECUREFAULTENA);
        }
        // The active and pending bits are writable (used by context-switching software), except
        // NMIACT and HARDFAULTACT.
        for (bit, num, pending) in scb::SHCSR_STATE_BITS {
            if matches!(num, 2 | 3) {
                continue;
            }
            let set = v & (1 << bit) != 0;
            if pending {
                self.set_exc_pending(num, sec, set);
            } else {
                self.set_exc_active(num, sec, set);
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
                self.set_exc_pending(15, sec, true);
            }
        }
    }
}
