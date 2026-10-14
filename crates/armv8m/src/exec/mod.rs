//! Instruction execution.

mod alu;
mod branch;
mod mem;
mod sys;

use crate::bus::Bus;
use crate::cpu::Cpu;
use crate::instr::Instr;

/// What an instruction did to control flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Fall through to the next instruction.
    Next,
    /// PC was written by the instruction.
    Branched,
    /// WFI/WFE: advance PC and report sleep.
    Sleep,
    /// BKPT: PC stays on the instruction.
    Bkpt(u8),
    /// SVC: PC advances, then the exception is requested.
    Svc(u8),
    /// The instruction faulted (undefined, or a memory access failed); PC stays.
    Fault,
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
        Instr::LoadImm { .. }
        | Instr::StoreImm { .. }
        | Instr::LoadReg { .. }
        | Instr::StoreReg { .. }
        | Instr::LdrLit { .. }
        | Instr::Push { .. }
        | Instr::Pop { .. }
        | Instr::Ldm { .. }
        | Instr::Stm { .. } => mem::execute(cpu, bus, instr),
        Instr::B { .. }
        | Instr::BCond { .. }
        | Instr::Cbz { .. }
        | Instr::Bx { .. }
        | Instr::Blx { .. } => branch::execute(cpu, instr),
        Instr::It { .. }
        | Instr::Svc { .. }
        | Instr::Bkpt { .. }
        | Instr::Cps { .. }
        | Instr::Hint(_) => sys::execute(cpu, instr),
        // Intercepted in `Cpu::step` before execution.
        Instr::Undefined { .. } => None,
    };
    // A group executor that does not recognise its own variant is a decoder/executor
    // mismatch: treat it as an undefined instruction rather than silently skipping it.
    flow.unwrap_or(Flow::Fault)
}
