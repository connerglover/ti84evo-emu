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

/// Width of a load/store access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSize {
    Byte,
    Half,
    Word,
}

/// Byte-reverse flavours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevKind {
    /// `REV`: reverse bytes in a word.
    Rev,
    /// `REV16`: reverse bytes in each halfword.
    Rev16,
    /// `REVSH`: reverse the low halfword and sign-extend.
    Revsh,
}

/// Hint instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintOp {
    Nop,
    Yield,
    Wfe,
    Wfi,
    Sev,
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
        /// Spelled with two operands in UAL (16-bit imm8 and SP forms).
        two_op: bool,
    },
    SubImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
        /// Spelled with two operands in UAL (16-bit imm8 and SP forms).
        two_op: bool,
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
    /// `LDR{B,H,SB,SH} rt, [rn, #+/-imm]` with optional pre/post indexing.
    LoadImm {
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
        signed: bool,
        index: bool,
        add: bool,
        wback: bool,
    },
    /// `STR{B,H} rt, [rn, #+/-imm]` with optional pre/post indexing.
    StoreImm {
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
        index: bool,
        add: bool,
        wback: bool,
    },
    /// `LDR{B,H,SB,SH} rt, [rn, rm, LSL #shift]`
    LoadReg {
        rt: Reg,
        rn: Reg,
        rm: Reg,
        shift: u8,
        size: MemSize,
        signed: bool,
    },
    /// `STR{B,H} rt, [rn, rm, LSL #shift]`
    StoreReg {
        rt: Reg,
        rn: Reg,
        rm: Reg,
        shift: u8,
        size: MemSize,
    },
    /// `LDR rt, [pc, #+/-imm]`
    LdrLit {
        rt: Reg,
        imm: u32,
        add: bool,
    },
    Push {
        regs: u16,
    },
    Pop {
        regs: u16,
    },
    Ldm {
        rn: Reg,
        regs: u16,
        wback: bool,
    },
    Stm {
        rn: Reg,
        regs: u16,
        wback: bool,
    },
    /// Unconditional branch; `imm` is the signed offset from PC + 4.
    B {
        imm: i32,
    },
    /// Conditional branch; `imm` is the signed offset from PC + 4.
    BCond {
        cond: u8,
        imm: i32,
    },
    /// Compare and branch on (non-)zero; `imm` is the forward offset from PC + 4.
    Cbz {
        rn: Reg,
        imm: u32,
        nonzero: bool,
    },
    Svc {
        imm: u8,
    },
    Bkpt {
        imm: u8,
    },
    Cps {
        disable: bool,
        i: bool,
        f: bool,
    },
    Hint(HintOp),
    /// `SXTB/SXTH/UXTB/UXTH rd, rm{, ROR #rotate}`
    Extend {
        rd: Reg,
        rm: Reg,
        rotate: u8,
        signed: bool,
        half: bool,
    },
    Rev {
        rd: Reg,
        rm: Reg,
        kind: RevKind,
    },
    Undefined {
        raw: u32,
    },
}
