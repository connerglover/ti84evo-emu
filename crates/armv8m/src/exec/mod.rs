//! Instruction execution.

mod alu;
mod branch;
mod dsp;
mod mem;
mod sys;

pub(crate) use mem::{simple_read, simple_write};

use crate::bus::Bus;
use crate::cpu::{Cpu, SleepKind};
use crate::exception::Fault;
use crate::instr::Instr;

/// What an instruction did to control flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Fall through to the next instruction.
    Next,
    /// PC was written by the instruction.
    Branched,
    /// WFI/WFE: advance PC and report sleep.
    Sleep(SleepKind),
    /// BKPT: PC stays on the instruction.
    Bkpt(u8),
    /// SVC: PC advances, then the exception is requested.
    Svc(u8),
    /// The instruction faulted; PC stays on it.
    Fault(Fault),
    /// PC was written with an EXC_RETURN value in Handler mode: `Cpu::step` performs the
    /// exception return.
    ExcReturn(u32),
    /// PC was written with an FNC_RETURN value: `Cpu::step` performs the function return (PC
    /// is unchanged until it does).
    FncReturn,
}

/// Execute `instr`. `suppress_flags` is true for 16-bit encodings inside an IT block.
pub(crate) fn execute(
    cpu: &mut Cpu,
    bus: &mut impl Bus,
    instr: &Instr,
    suppress_flags: bool,
) -> Flow {
    let flow = match *instr {
        Instr::MovImm { .. }
        | Instr::MovReg { .. }
        | Instr::Mvn { .. }
        | Instr::ShiftImm { .. }
        | Instr::ShiftReg { .. }
        | Instr::AddImm { .. }
        | Instr::SubImm { .. }
        | Instr::RsbImm { .. }
        | Instr::AddReg { .. }
        | Instr::SubReg { .. }
        | Instr::AluReg { .. }
        | Instr::Mul { .. }
        | Instr::CmpImm { .. }
        | Instr::CmpReg { .. }
        | Instr::CmnReg { .. }
        | Instr::TstReg { .. }
        | Instr::Adr { .. }
        | Instr::Extend { .. }
        | Instr::Rev { .. }
        | Instr::Movw { .. }
        | Instr::Movt { .. }
        | Instr::MvnImm { .. }
        | Instr::RsbReg { .. }
        | Instr::AluImm { .. }
        | Instr::CmnImm { .. }
        | Instr::TstImm { .. }
        | Instr::TeqImm { .. }
        | Instr::TeqReg { .. }
        | Instr::ExtendAdd { .. }
        | Instr::Clz { .. }
        | Instr::Rbit { .. }
        | Instr::Sel { .. }
        | Instr::Pkh { .. }
        | Instr::Ssat { .. }
        | Instr::Usat { .. }
        | Instr::Bfx { .. }
        | Instr::Bfi { .. }
        | Instr::Bfc { .. }
        | Instr::MulAcc { .. }
        | Instr::LongMul { .. }
        | Instr::Div { .. } => alu::execute(cpu, instr, suppress_flags),
        Instr::Parallel { .. }
        | Instr::SatArith { .. }
        | Instr::MulHalf { .. }
        | Instr::MulWordHalf { .. }
        | Instr::MulLongHalf { .. }
        | Instr::DualMul { .. }
        | Instr::DualMulLong { .. }
        | Instr::MulMsw { .. }
        | Instr::Usad8 { .. }
        | Instr::Ssat16 { .. }
        | Instr::Usat16 { .. }
        | Instr::Extend16 { .. } => dsp::execute(cpu, instr),
        Instr::LoadImm { .. }
        | Instr::StoreImm { .. }
        | Instr::LoadReg { .. }
        | Instr::StoreReg { .. }
        | Instr::LdrLit { .. }
        | Instr::Push { .. }
        | Instr::Pop { .. }
        | Instr::Ldm { .. }
        | Instr::Stm { .. }
        | Instr::LoadUnpriv { .. }
        | Instr::StoreUnpriv { .. }
        | Instr::Ldrd { .. }
        | Instr::Strd { .. }
        | Instr::Ldmdb { .. }
        | Instr::Stmdb { .. }
        | Instr::Ldrex { .. }
        | Instr::Strex { .. }
        | Instr::Lda { .. }
        | Instr::Stl { .. }
        | Instr::Clrex
        | Instr::Tb { .. } => mem::execute(cpu, bus, instr),
        Instr::B { .. }
        | Instr::BCond { .. }
        | Instr::Bl { .. }
        | Instr::Cbz { .. }
        | Instr::Bx { .. }
        | Instr::Blx { .. }
        | Instr::Bxns { .. }
        | Instr::Blxns { .. } => branch::execute(cpu, bus, instr),
        Instr::It { .. }
        | Instr::Svc { .. }
        | Instr::Bkpt { .. }
        | Instr::Cps { .. }
        | Instr::Hint(_)
        | Instr::Preload { .. }
        | Instr::Msr { .. }
        | Instr::Mrs { .. }
        | Instr::Barrier { .. }
        | Instr::Coprocessor { .. }
        | Instr::Sg
        | Instr::Tt { .. } => sys::execute(cpu, instr),
        // Intercepted in `Cpu::step` before execution.
        Instr::Undefined { .. } => None,
    };
    // A group executor that does not recognise its own variant is a decoder/executor
    // mismatch: treat it as an undefined instruction rather than silently skipping it.
    flow.unwrap_or(Flow::Fault(Fault::UndefInstr))
}

/// Cycles an executed instruction costs beyond the base cycle and the taken-branch cycle.
pub(crate) fn extra_cycles(instr: &Instr) -> u32 {
    match *instr {
        Instr::Push { regs }
        | Instr::Ldm { regs, .. }
        | Instr::Stm { regs, .. }
        | Instr::Ldmdb { regs, .. }
        | Instr::Stmdb { regs, .. } => regs.count_ones(),
        // The taken-branch cycle of POP {.., pc} is part of its N-register cost.
        Instr::Pop { regs } => regs.count_ones() - u32::from(regs & 0x8000 != 0),
        Instr::Ldrd { .. } | Instr::Strd { .. } => 1,
        // The return address and partial xPSR it may push.
        Instr::Blxns { .. } => 2,
        // 4 cycles in total.
        Instr::Div { .. } => 3,
        _ => 0,
    }
}
