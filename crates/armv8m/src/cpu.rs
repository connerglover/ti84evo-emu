//! CPU state and the fetch/decode/execute step.

use serde::{Deserialize, Serialize};

use crate::bits::condition_holds;
use crate::bus::{Access, AccessKind, Bus};
use crate::decode::{decode16, decode32, is_32bit};
use crate::exception::{ENTRY_CYCLES, Fault, ReturnOutcome, SVCALL};
use crate::exec::{self, Flow};
use crate::instr::Instr;
use crate::regs::{Registers, Sec};
use crate::scs::sau::{Attr, Idau, IdauHandle, SecAttr};
use crate::scs::scb::CCR_STKOFHFNMIGN;
use crate::scs::{PpbMiss, Scs};

/// Outcome of one `Cpu::step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepResult {
    Executed {
        cycles: u32,
    },
    /// The processor went to sleep; the kind says what wakes it.
    Sleep(SleepKind),
    Bkpt(u8),
    Lockup,
}

/// Why the processor went to sleep, which decides what wakes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SleepKind {
    /// WFI: woken by a pending exception.
    Wfi,
    /// WFE with the event register clear: woken by a pending exception or by the event
    /// register becoming set (SEV, `SCR.SEVONPEND`), which the wake-up consumes.
    Wfe,
    /// An exception return to Thread mode with `SCR.SLEEPONEXIT` set: woken like WFI.
    OnExit,
}

/// Processor state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
    pub regs: Registers,
    /// Event register set by SEV and consumed by WFE.
    pub event: bool,
    /// Local exclusive monitor: the address of the open LDREX reservation, if any. Cleared by
    /// CLREX, by every STREX and by exception entry and return.
    pub exclusive: Option<u32>,
    /// System Control Space (NVIC, SCB, SysTick, MPU).
    pub scs: Scs,
    /// While locked up: the priority the lockup is held at (see `locked_up`).
    pub(crate) lockup: Option<i32>,
    /// Set by `write_reg` when an instruction's SP write violated the stack limit; `step` turns
    /// it into a STKOF UsageFault. Only meaningful within one instruction.
    #[serde(skip)]
    pub(crate) sp_violation: bool,
    /// Guest data accesses to unimplemented internal PPB registers, drained by
    /// `take_ppb_misses` (diagnostics only: not serialized).
    #[serde(skip)]
    pub(crate) ppb_misses: Vec<PpbMiss>,
    /// The SoC's attribution unit (configuration: kept across reset, not serialized).
    #[serde(skip)]
    idau: IdauHandle,
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

/// Instruction fetches from these regions are Execute Never in the default memory map:
/// Peripheral (0x4000_0000..0x6000_0000), Device and System (0xA000_0000..).
fn execute_never(addr: u32) -> bool {
    (0x4000_0000..0x6000_0000).contains(&addr) || addr >= 0xA000_0000
}

/// UNPREDICTABLE instructions inside an IT block, which this implementation makes UNDEFINED:
/// conditional branches, CBZ/CBNZ, CPS, IT and SG anywhere in a block, and instructions that
/// write the PC anywhere but last.
fn unpredictable_in_it(instr: &Instr, last: bool) -> bool {
    match *instr {
        Instr::BCond { .. }
        | Instr::Cbz { .. }
        | Instr::Cps { .. }
        | Instr::It { .. }
        | Instr::Sg => true,
        _ => !last && writes_pc(instr),
    }
}

fn writes_pc(instr: &Instr) -> bool {
    match *instr {
        Instr::B { .. }
        | Instr::Bl { .. }
        | Instr::Bx { .. }
        | Instr::Blx { .. }
        | Instr::Bxns { .. }
        | Instr::Blxns { .. } => true,
        Instr::Tb { .. } => true,
        Instr::Pop { regs } | Instr::Ldm { regs, .. } | Instr::Ldmdb { regs, .. } => {
            regs & 0x8000 != 0
        }
        Instr::LoadImm { rt, .. } | Instr::LoadReg { rt, .. } | Instr::LdrLit { rt, .. } => {
            rt == 15
        }
        Instr::MovReg { rd, .. } | Instr::AddReg { rd, .. } => rd == 15,
        _ => false,
    }
}

/// Result of executing (or failing to execute) one instruction.
enum Exec {
    Done {
        cycles: u32,
        sleep: Option<SleepKind>,
    },
    Bkpt(u8),
    Lockup,
}

impl Cpu {
    /// Reset state: Secure, Thread mode, privileged, Thumb.
    pub fn new() -> Self {
        let mut regs = Registers {
            sec: Sec::Secure,
            ..Default::default()
        };
        regs.xpsr.t = true;
        Self {
            regs,
            event: false,
            exclusive: None,
            scs: Scs::default(),
            lockup: None,
            sp_violation: false,
            ppb_misses: Vec::new(),
            idau: IdauHandle::default(),
        }
    }

    /// Reset: VTOR takes its reset value (see `set_vtor`, default 0), then MSP_S and PC are
    /// loaded from the Secure vector table. The installed IDAU is kept; the SAU resets disabled.
    pub fn reset(&mut self, bus: &mut impl Bus) {
        let init_vtor = self.scs.scb.init_vtor;
        let idau = self.idau.clone();
        *self = Self::new();
        self.idau = idau;
        self.scs.scb.init_vtor = init_vtor;
        self.scs.scb.vtor = init_vtor;
        let acc = self.access(AccessKind::Read);
        let vtor = init_vtor.s;
        if let (Ok(sp), Ok(pc)) = (bus.read32(vtor, acc), bus.read32(vtor + 4, acc)) {
            self.regs.msp.s = sp & !3;
            self.regs.pc = pc & !1;
            self.regs.xpsr.t = pc & 1 != 0;
        }
    }

    /// Replace all processor state with `saved` (e.g. a `Cpu` decoded from a save state, which
    /// carries the default IDAU), keeping the IDAU installed in `self`: the IDAU is the SoC's
    /// configuration, not state.
    pub fn restore_from(&mut self, saved: Cpu) {
        let idau = std::mem::take(&mut self.idau);
        *self = saved;
        self.idau = idau;
    }

    /// Install the SoC's IDAU (default: `DefaultIdau`, address bit 28 set is Secure).
    pub fn set_idau(&mut self, idau: Box<dyn Idau>) {
        self.idau = IdauHandle::new(idau);
    }

    /// Security attribution of `addr`: the SAU combined with the IDAU (most secure wins); the
    /// system space from `0xE000_0000` is `Exempt`.
    pub fn security_of(&self, addr: u32) -> Attr {
        self.attribution(addr).attr
    }

    /// `SecurityCheck()` for `addr`, with the region numbers `TT` reports.
    pub(crate) fn attribution(&self, addr: u32) -> SecAttr {
        self.scs.sau.check(addr, self.idau.get())
    }

    /// The attributes of a data access to `addr` by security state `from` (`acc.secure` is
    /// replaced): Non-secure memory is accessed Non-secure even from Secure state, exempt memory
    /// in state `from`. `None` when Non-secure state reaches Secure or Non-secure callable
    /// memory, which is a SecureFault AUVIOL.
    pub(crate) fn attribute(&self, addr: u32, acc: Access, from: Sec) -> Option<Access> {
        let secure = match self.security_of(addr) {
            Attr::Exempt => from == Sec::Secure,
            Attr::NonSecure => false,
            Attr::Secure | Attr::NonSecureCallable => {
                if from == Sec::NonSecure {
                    return None;
                }
                true
            }
        };
        Some(Access { secure, ..acc })
    }

    /// Handler mode, or Thread mode with CONTROL.nPRIV clear.
    pub(crate) fn is_privileged(&self) -> bool {
        let nonpriv = self.regs.control.get(self.regs.sec) & 1 != 0;
        self.regs.handler_mode() || !nonpriv
    }

    pub(crate) fn access(&self, kind: AccessKind) -> Access {
        Access {
            kind,
            secure: self.regs.sec == Sec::Secure,
            privileged: self.is_privileged(),
        }
    }

    /// Fetch and decode the instruction at `addr` for execution. The fetch's security is the
    /// attribution of `addr`, which must match the current security state: Non-secure code may
    /// enter Secure memory only at an `SG` in a Non-secure callable region (else INVEP), and
    /// Secure code reaches Non-secure memory only through BXNS/BLXNS (else INVTRAN).
    fn fetch(&self, bus: &mut impl Bus, addr: u32) -> Result<(Instr, u8), Fault> {
        let attr = self.security_of(addr);
        let sec = self.regs.sec;
        match (sec, attr) {
            (Sec::NonSecure, Attr::Secure) => return Err(Fault::InvEp),
            (Sec::Secure, Attr::NonSecure) => return Err(Fault::InvTran),
            _ => {}
        }
        if execute_never(addr) {
            return Err(Fault::IAccViol);
        }
        let secure = match attr {
            Attr::Exempt => sec == Sec::Secure,
            Attr::NonSecure => false,
            Attr::Secure | Attr::NonSecureCallable => true,
        };
        let decoded = self.read_instr(bus, addr, secure)?;
        if sec == Sec::NonSecure && attr == Attr::NonSecureCallable && decoded.0 != Instr::Sg {
            return Err(Fault::InvEp);
        }
        Ok(decoded)
    }

    /// Read and decode the instruction at `addr` with a fetch of security `secure`.
    fn read_instr(
        &self,
        bus: &mut impl Bus,
        addr: u32,
        secure: bool,
    ) -> Result<(Instr, u8), Fault> {
        let acc = Access {
            kind: AccessKind::Fetch,
            secure,
            privileged: self.is_privileged(),
        };
        let hw1 = bus.read16(addr, acc).map_err(|_| Fault::IBusErr)?;
        if is_32bit(hw1) {
            let hw2 = bus
                .read16(addr.wrapping_add(2), acc)
                .map_err(|_| Fault::IBusErr)?;
            Ok((decode32(hw1, hw2), 4))
        } else {
            Ok((decode16(hw1), 2))
        }
    }

    /// Fetch and decode the instruction at `addr`; returns it with its length (2 or 4). Unlike
    /// execution this ignores the security state (a debugger view), but not Execute Never.
    pub fn decode_at(&self, bus: &mut impl Bus, addr: u32) -> Option<(Instr, u8)> {
        if execute_never(addr) {
            return None;
        }
        let secure = self.security_of(addr) != Attr::NonSecure;
        self.read_instr(bus, addr, secure).ok()
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

    /// `ViolatesSPLim(LookUpSP(), value)`: `value` is below the limit of the stack pointer in
    /// use (MSPLIM in Handler mode or with CONTROL.SPSEL = 0, else PSPLIM, of the current
    /// security state). With `CCR.STKOFHFNMIGN` set, a negative execution priority (HardFault,
    /// NMI, FAULTMASK) ignores the limit.
    pub(crate) fn violates_sp_lim(&self, value: u32) -> bool {
        let r = &self.regs;
        let sec = r.sec;
        let limit = if !r.handler_mode() && r.control.get(sec) & 0b10 != 0 {
            *r.psplim.get(sec)
        } else {
            *r.msplim.get(sec)
        };
        value < limit
            && !(self.scs.scb.ccr.get(sec) & CCR_STKOFHFNMIGN != 0 && self.execution_priority() < 0)
    }

    /// A register write by an instruction. A write to SP is checked against the stack limit
    /// (`RSPCheck[]` / the SP setter): on a violation SP keeps its value and `sp_violation`
    /// makes `step` raise a STKOF UsageFault for the instruction. Writes from outside the
    /// instruction stream (a debugger, MSR MSP/PSP) use `set_reg` and are not checked.
    pub(crate) fn write_reg(&mut self, n: u8, v: u32) {
        if n == 13 && self.violates_sp_lim(v & !3) {
            self.sp_violation = true;
        } else {
            self.set_reg(n, v);
        }
    }

    /// `BranchWritePC`: no interworking.
    pub fn branch_write_pc(&mut self, addr: u32) {
        self.regs.pc = addr & !1;
    }

    /// `BLXWritePC`: bit 0 selects the instruction set (T bit); never an exception return.
    pub fn blx_write_pc(&mut self, addr: u32) {
        self.regs.xpsr.t = addr & 1 != 0;
        self.regs.pc = addr & !1;
    }

    /// `BXWritePC` / `LoadWritePC`: in Handler mode an EXC_RETURN value (0xFF00_0000 and up)
    /// requests an exception return, and in either mode an FNC_RETURN value (0xFE00_0000 and
    /// up) a function return from a BLXNS call; `step` performs both. Otherwise an
    /// interworking branch. With `allow_ns` (BXNS) a target with bit 0 clear continues in
    /// Non-secure state (in Thumb state: bit 0 selects the security state instead).
    pub(crate) fn bx_write_pc(&mut self, addr: u32, allow_ns: bool) -> Flow {
        if self.regs.handler_mode() && addr >> 24 == 0xFF {
            return Flow::ExcReturn(addr);
        }
        if addr >> 24 == 0xFE {
            return Flow::FncReturn;
        }
        if allow_ns && addr & 1 == 0 {
            self.regs.sec = Sec::NonSecure;
            self.regs.xpsr.t = true;
            self.regs.pc = addr;
        } else {
            self.blx_write_pc(addr);
        }
        Flow::Branched
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

    /// Take a pending exception that preempts the current execution priority, else execute one
    /// instruction (and take any exception it caused or that became pending meanwhile).
    pub fn step(&mut self, bus: &mut impl Bus) -> StepResult {
        if self.stay_locked_up() {
            return StepResult::Lockup;
        }
        // Asynchronous exceptions that became pending between steps.
        if self.take_pending(bus) {
            return self.entry_result(ENTRY_CYCLES);
        }
        let (mut cycles, sleep) = match self.execute_one(bus) {
            Exec::Done { cycles, sleep } => (cycles, sleep),
            Exec::Bkpt(imm) => return StepResult::Bkpt(imm),
            Exec::Lockup => return StepResult::Lockup,
        };
        self.tick_systick(cycles);
        if self.take_pending(bus) {
            cycles += ENTRY_CYCLES;
            return self.entry_result(cycles);
        }
        match sleep {
            Some(kind) => StepResult::Sleep(kind),
            None => StepResult::Executed { cycles },
        }
    }

    fn entry_result(&self, cycles: u32) -> StepResult {
        if self.lockup.is_some() {
            StepResult::Lockup
        } else {
            StepResult::Executed { cycles }
        }
    }

    /// Raise `fault` for the instruction at PC (which stays on it).
    fn fault(&mut self, fault: Fault) -> Exec {
        if self.raise_fault(fault) {
            Exec::Done {
                cycles: 1,
                sleep: None,
            }
        } else {
            self.enter_lockup(None);
            Exec::Lockup
        }
    }

    fn execute_one(&mut self, bus: &mut impl Bus) -> Exec {
        let pc = self.regs.pc;
        if !self.regs.xpsr.t {
            return self.fault(Fault::InvState);
        }
        let (instr, len) = match self.fetch(bus, pc) {
            Ok(x) => x,
            Err(f) => return self.fault(f),
        };
        let it = self.regs.xpsr.it;
        let in_it = it & 0xF != 0;
        // BKPT executes unconditionally, even when its IT condition fails.
        if let Instr::Bkpt { imm } = instr {
            return Exec::Bkpt(imm);
        }
        // An undefined instruction inside an IT block faults even when its condition fails
        // (CONSTRAINED UNPREDICTABLE: faulting is one of the permitted behaviours).
        if let Instr::Undefined { raw } = instr {
            log::warn!("undefined instruction {raw:#x} at {pc:#010x}");
            return self.fault(Fault::UndefInstr);
        }
        if in_it && unpredictable_in_it(&instr, it & 0xF == 0x8) {
            log::warn!("UNPREDICTABLE {instr:?} in IT block at {pc:#010x}");
            return self.fault(Fault::UndefInstr);
        }

        let executes = !in_it || condition_holds(it >> 4, &self.regs.xpsr);
        // 16-bit "S" forms do not set flags inside an IT block.
        self.sp_violation = false;
        let flow = if executes {
            exec::execute(self, bus, &instr, in_it && len == 2)
        } else {
            Flow::Next
        };
        // An SP write below the stack limit (`write_reg`) faults the instruction.
        let flow = if std::mem::take(&mut self.sp_violation) {
            Flow::Fault(Fault::StkOf)
        } else {
            flow
        };
        // A branch to FNC_RETURN completes only if the function return does.
        let flow = match flow {
            Flow::FncReturn => match self.function_return(bus) {
                Ok(()) => Flow::Branched,
                Err(f) => Flow::Fault(f),
            },
            flow => flow,
        };
        match flow {
            Flow::Fault(f) => return self.fault(f),
            // Intercepted above; kept for completeness.
            Flow::Bkpt(imm) => return Exec::Bkpt(imm),
            Flow::Next | Flow::Sleep(_) | Flow::Svc(_) => {
                self.regs.pc = pc.wrapping_add(u32::from(len));
            }
            // FncReturn was turned into Branched or Fault above.
            Flow::Branched | Flow::ExcReturn(_) | Flow::FncReturn => {}
        }
        if in_it {
            self.it_advance();
        }
        let branched = matches!(flow, Flow::Branched | Flow::ExcReturn(_));
        let extra = if executes {
            exec::extra_cycles(&instr)
        } else {
            0
        };
        let cycles = 1 + u32::from(branched) + extra;
        let mut sleep = match flow {
            Flow::Sleep(kind) => Some(kind),
            _ => None,
        };
        match flow {
            Flow::Svc(_) => {
                if !self.raise_sync(SVCALL, self.regs.sec) {
                    self.enter_lockup(None);
                    return Exec::Lockup;
                }
            }
            Flow::ExcReturn(v) => match self.exception_return(bus, v) {
                ReturnOutcome::Done => {}
                ReturnOutcome::Sleep => sleep = Some(SleepKind::OnExit),
                ReturnOutcome::Lockup => return Exec::Lockup,
            },
            _ => {}
        }
        Exec::Done { cycles, sleep }
    }
}
