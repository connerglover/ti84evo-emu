//! CPU state and the fetch/decode/execute step.

use serde::{Deserialize, Serialize};

use crate::bits::condition_holds;
use crate::bus::{Access, AccessKind, Bus};
use crate::decode::{decode16, decode32, is_32bit};
use crate::exec::{self, Flow};
use crate::instr::Instr;
use crate::regs::{Registers, Sec};

/// Outcome of one `Cpu::step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepResult {
    Executed { cycles: u32 },
    Sleep,
    Bkpt(u8),
    Lockup,
}

/// Processor state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
    pub regs: Registers,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Reset state: Secure, Thread mode, privileged, Thumb.
    pub fn new() -> Self {
        let mut regs = Registers {
            sec: Sec::Secure,
            ..Default::default()
        };
        regs.xpsr.t = true;
        Self { regs }
    }

    /// Load MSP_S and PC from the vector table at VTOR_S (default 0).
    pub fn reset(&mut self, bus: &mut impl Bus) {
        *self = Self::new();
        let acc = self.access(AccessKind::Read);
        let vtor = 0;
        if let (Ok(sp), Ok(pc)) = (bus.read32(vtor, acc), bus.read32(vtor + 4, acc)) {
            self.regs.msp.s = sp & !3;
            self.regs.pc = pc & !1;
            self.regs.xpsr.t = pc & 1 != 0;
        }
    }

    fn access(&self, kind: AccessKind) -> Access {
        let nonpriv = self.regs.control.get(self.regs.sec) & 1 != 0;
        Access {
            kind,
            secure: self.regs.sec == Sec::Secure,
            privileged: self.regs.handler_mode() || !nonpriv,
        }
    }

    /// Fetch and decode the instruction at `addr`; returns it with its length (2 or 4).
    pub fn decode_at(&self, bus: &mut impl Bus, addr: u32) -> Option<(Instr, u8)> {
        let acc = self.access(AccessKind::Fetch);
        let hw1 = bus.read16(addr, acc).ok()?;
        if is_32bit(hw1) {
            let hw2 = bus.read16(addr.wrapping_add(2), acc).ok()?;
            Some((decode32(hw1, hw2), 4))
        } else {
            Some((decode16(hw1), 2))
        }
    }

    /// Value of register `n` as read by an instruction (PC reads as address + 4).
    pub fn reg(&self, n: u8) -> u32 {
        match n {
            0..=12 => self.regs.r[usize::from(n)],
            13 => self.regs.sp(),
            14 => self.regs.lr,
            _ => self.regs.pc.wrapping_add(4),
        }
    }

    /// Write a general register (not PC; use the branch helpers for that).
    pub fn set_reg(&mut self, n: u8, v: u32) {
        match n {
            0..=12 => self.regs.r[usize::from(n)] = v,
            13 => self.regs.set_sp(v & !3),
            14 => self.regs.lr = v,
            _ => unreachable!("PC writes go through branch_write_pc / bx_write_pc"),
        }
    }

    /// `BranchWritePC`: no interworking.
    pub fn branch_write_pc(&mut self, addr: u32) {
        self.regs.pc = addr & !1;
    }

    /// `BXWritePC`: bit 0 selects the instruction set (T bit).
    pub fn bx_write_pc(&mut self, addr: u32) {
        // Task 7: EXC_RETURN values (0xFF00_0000..) start an exception return here.
        self.regs.xpsr.t = addr & 1 != 0;
        self.regs.pc = addr & !1;
    }

    /// `ITAdvance()`.
    fn it_advance(&mut self) {
        let it = self.regs.xpsr.it;
        self.regs.xpsr.it = if it & 7 == 0 {
            0
        } else {
            (it & 0xE0) | ((it << 1) & 0x1F)
        };
    }

    /// Execute one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) -> StepResult {
        let pc = self.regs.pc;
        let Some((instr, len)) = self.decode_at(bus, pc) else {
            return self.raise_fault();
        };
        if matches!(instr, Instr::Undefined { .. }) {
            return self.raise_fault();
        }

        let in_it = self.regs.xpsr.it & 0xF != 0;
        let mut flow = Flow::Next;
        if !in_it || condition_holds(self.regs.xpsr.it >> 4, &self.regs.xpsr) {
            // 16-bit "S" forms do not set flags inside an IT block.
            flow = exec::execute(self, bus, &instr, in_it && len == 2);
        }
        if flow == Flow::Next {
            self.regs.pc = pc.wrapping_add(u32::from(len));
        }
        if in_it {
            self.it_advance();
        }
        StepResult::Executed {
            cycles: 1 + u32::from(flow == Flow::Branched),
        }
    }

    /// Placeholder for faults raised by undefined or unfetchable instructions.
    fn raise_fault(&mut self) -> StepResult {
        // Task 7: raise UsageFault UNDEFINSTR / BusFault IBUSERR here instead of locking up.
        StepResult::Lockup
    }
}
