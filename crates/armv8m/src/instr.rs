//! Encoding-independent instruction representation.

use crate::bits::SRType;

/// Register number 0..=15 (13 = SP, 14 = LR, 15 = PC).
pub type Reg = u8;

/// Register shift applied to the second operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shift {
    pub ty: SRType,
    pub amount: u32,
}

impl Shift {
    pub const NONE: Shift = Shift {
        ty: SRType::Lsl,
        amount: 0,
    };
}

/// Logical / carry operations sharing the `AluReg` form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AluOp {
    And,
    Eor,
    Orr,
    Bic,
    Adc,
    Sbc,
}

/// A decoded instruction. Immediates are already expanded. For 16-bit forms
/// that set flags only outside an IT block, `setflags` is `true` and the
/// executor suppresses it inside an IT block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instr {
    MovImm {
        rd: Reg,
        imm: u32,
        setflags: bool,
    },
    MovReg {
        rd: Reg,
        rm: Reg,
        setflags: bool,
    },
    Mvn {
        rd: Reg,
        rm: Reg,
        shift: Shift,
        setflags: bool,
    },
    ShiftImm {
        rd: Reg,
        rm: Reg,
        ty: SRType,
        amount: u32,
        setflags: bool,
    },
    ShiftReg {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ty: SRType,
        setflags: bool,
    },
    AddImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
    },
    SubImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
    },
    RsbImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
    },
    AddReg {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        setflags: bool,
    },
    SubReg {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        setflags: bool,
    },
    AluReg {
        op: AluOp,
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        setflags: bool,
    },
    Mul {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        setflags: bool,
    },
    CmpImm {
        rn: Reg,
        imm: u32,
    },
    CmpReg {
        rn: Reg,
        rm: Reg,
        shift: Shift,
    },
    CmnReg {
        rn: Reg,
        rm: Reg,
        shift: Shift,
    },
    TstReg {
        rn: Reg,
        rm: Reg,
        shift: Shift,
    },
    /// `rd = Align(PC, 4) + imm`
    Adr {
        rd: Reg,
        imm: u32,
    },
    Bx {
        rm: Reg,
    },
    Blx {
        rm: Reg,
    },
    It {
        firstcond: u8,
        mask: u8,
    },
    Undefined {
        raw: u32,
    },
}
