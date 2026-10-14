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
    /// `ORN` (32-bit only): `Rn OR NOT(operand)`.
    Orn,
}

/// 64-bit multiply flavours (`RdLo`/`RdHi` destinations).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LongMulKind {
    Smull,
    Umull,
    Smlal,
    Umlal,
    Umaal,
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

/// Parallel add/subtract flavour (the prefix of `SADD16`, `QADD8`, `UHSAX`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParKind {
    /// Signed, modulo, sets APSR.GE.
    S,
    /// Signed saturating.
    Q,
    /// Signed halving.
    Sh,
    /// Unsigned, modulo, sets APSR.GE.
    U,
    /// Unsigned saturating.
    Uq,
    /// Unsigned halving.
    Uh,
}

/// Parallel add/subtract lane arrangement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParOp {
    Add16,
    /// `ASX`: low = `Rn.lo - Rm.hi`, high = `Rn.hi + Rm.lo`.
    Asx,
    /// `SAX`: low = `Rn.lo + Rm.hi`, high = `Rn.hi - Rm.lo`.
    Sax,
    Sub16,
    Add8,
    Sub8,
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

/// Memory barrier flavours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierKind {
    Dsb,
    Dmb,
    Isb,
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
        /// Carry-out of `ThumbExpandImm_C`: `Some(c)` sets APSR.C to `c` when flags are set,
        /// `None` leaves it unchanged.
        carry: Option<bool>,
    },
    /// `MOVW rd, #imm16`
    Movw {
        rd: Reg,
        imm: u16,
    },
    /// `MOVT rd, #imm16`: replace the top halfword.
    Movt {
        rd: Reg,
        imm: u16,
    },
    /// `MVN rd, #imm` (modified immediate).
    MvnImm {
        rd: Reg,
        imm: u32,
        setflags: bool,
        carry: Option<bool>,
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
        /// Plain 12-bit immediate encoding (`ADDW`/`SUBW`) rather than a modified immediate.
        imm12: bool,
    },
    SubImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
        /// Spelled with two operands in UAL (16-bit imm8 and SP forms).
        two_op: bool,
        /// Plain 12-bit immediate encoding (`ADDW`/`SUBW`) rather than a modified immediate.
        imm12: bool,
    },
    RsbImm {
        rd: Reg,
        rn: Reg,
        imm: u32,
        setflags: bool,
    },
    RsbReg {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        shift: Shift,
        setflags: bool,
    },
    /// Logical / carry operation with a modified immediate (32-bit encodings).
    AluImm {
        op: AluOp,
        rd: Reg,
        rn: Reg,
        imm: u32,
        carry: Option<bool>,
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
    CmnImm {
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
    TstImm {
        rn: Reg,
        imm: u32,
        carry: Option<bool>,
    },
    TeqImm {
        rn: Reg,
        imm: u32,
        carry: Option<bool>,
    },
    TeqReg {
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
    /// `SXTAB/SXTAH/UXTAB/UXTAH rd, rn, rm{, ROR #rotate}`
    ExtendAdd {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        rotate: u8,
        signed: bool,
        half: bool,
    },
    Clz {
        rd: Reg,
        rm: Reg,
    },
    Rbit {
        rd: Reg,
        rm: Reg,
    },
    /// Select bytes from `rn`/`rm` according to APSR.GE.
    Sel {
        rd: Reg,
        rn: Reg,
        rm: Reg,
    },
    /// `PKHBT` (`tb == false`, LSL) / `PKHTB` (`tb == true`, ASR); `amount` is the shift.
    Pkh {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        tb: bool,
        amount: u32,
    },
    /// `SSAT rd, #bits, rn{, shift}` (`bits` is 1..=32).
    Ssat {
        rd: Reg,
        rn: Reg,
        bits: u8,
        shift: Shift,
    },
    /// `USAT rd, #bits, rn{, shift}` (`bits` is 0..=31).
    Usat {
        rd: Reg,
        rn: Reg,
        bits: u8,
        shift: Shift,
    },
    /// `SBFX` / `UBFX`
    Bfx {
        rd: Reg,
        rn: Reg,
        lsb: u8,
        width: u8,
        signed: bool,
    },
    Bfi {
        rd: Reg,
        rn: Reg,
        lsb: u8,
        width: u8,
    },
    Bfc {
        rd: Reg,
        lsb: u8,
        width: u8,
    },
    /// `MLA` (`sub == false`) / `MLS`: `rd = ra +/- rn * rm`.
    MulAcc {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Reg,
        sub: bool,
    },
    LongMul {
        kind: LongMulKind,
        rdlo: Reg,
        rdhi: Reg,
        rn: Reg,
        rm: Reg,
    },
    /// `SDIV` / `UDIV`
    Div {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        signed: bool,
    },
    /// `LDR{B,H,SB,SH}T rt, [rn, #imm]`: load with unprivileged permissions.
    LoadUnpriv {
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
        signed: bool,
    },
    /// `STR{B,H}T rt, [rn, #imm]`: store with unprivileged permissions.
    StoreUnpriv {
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
    },
    /// `LDRD rt, rt2, [rn, #+/-imm]` (`rn == 15` is the literal form, base `Align(PC, 4)`).
    Ldrd {
        rt: Reg,
        rt2: Reg,
        rn: Reg,
        imm: u32,
        index: bool,
        add: bool,
        wback: bool,
    },
    /// `STRD rt, rt2, [rn, #+/-imm]`
    Strd {
        rt: Reg,
        rt2: Reg,
        rn: Reg,
        imm: u32,
        index: bool,
        add: bool,
        wback: bool,
    },
    /// `LDMDB rn{!}, regs`
    Ldmdb {
        rn: Reg,
        regs: u16,
        wback: bool,
    },
    /// `STMDB rn{!}, regs`
    Stmdb {
        rn: Reg,
        regs: u16,
        wback: bool,
    },
    /// `LDREX{B,H}` / `LDAEX{B,H}`: load and open the exclusive monitor.
    Ldrex {
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
        acquire: bool,
    },
    /// `STREX{B,H}` / `STLEX{B,H}`: `rd` receives 0 on success, 1 on failure.
    Strex {
        rd: Reg,
        rt: Reg,
        rn: Reg,
        imm: u32,
        size: MemSize,
        release: bool,
    },
    /// `LDA{B,H}`: load-acquire.
    Lda {
        rt: Reg,
        rn: Reg,
        size: MemSize,
    },
    /// `STL{B,H}`: store-release.
    Stl {
        rt: Reg,
        rn: Reg,
        size: MemSize,
    },
    Clrex,
    /// `TBB` / `TBH`: table branch.
    Tb {
        rn: Reg,
        rm: Reg,
        half: bool,
    },
    /// `PLD` / `PLI` (a no-op here): `[rn, #+/-imm]` or `[rn, rm, LSL #shift]` when `rm` is set.
    Preload {
        instr: bool,
        rn: Reg,
        rm: Option<Reg>,
        shift: u8,
        imm: u32,
        add: bool,
    },
    /// `BL`; `imm` is the signed offset from PC + 4.
    Bl {
        imm: i32,
    },
    /// `MSR sysm, rn`; `mask` is the APSR field mask (bit 1 = NZCVQ, bit 0 = GE).
    Msr {
        sysm: u8,
        rn: Reg,
        mask: u8,
    },
    Mrs {
        rd: Reg,
        sysm: u8,
    },
    Barrier {
        kind: BarrierKind,
        option: u8,
    },
    /// Coprocessor space (including the FP load/stores); raises NOCP.
    Coprocessor {
        raw: u32,
    },
    /// Parallel add/subtract (DSP): `{S,Q,SH,U,UQ,UH}{ADD16,ASX,SAX,SUB16,ADD8,SUB8}`.
    Parallel {
        kind: ParKind,
        op: ParOp,
        rd: Reg,
        rn: Reg,
        rm: Reg,
    },
    /// `QADD`/`QSUB`/`QDADD`/`QDSUB rd, rm, rn`: `rm +/- (double ? sat(2 * rn) : rn)`, saturating.
    SatArith {
        rd: Reg,
        rm: Reg,
        rn: Reg,
        sub: bool,
        double: bool,
    },
    /// `SMULxy` (`ra == None`) / `SMLAxy`: 16 x 16 multiply; `*_top` selects the top halfword.
    MulHalf {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
        n_top: bool,
        m_top: bool,
    },
    /// `SMULWy` / `SMLAWy`: `(rn * half(rm)) >> 16`, optionally accumulating `ra`.
    MulWordHalf {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
        top: bool,
    },
    /// `SMLALxy`: 64-bit accumulate of a 16 x 16 product.
    MulLongHalf {
        rdlo: Reg,
        rdhi: Reg,
        rn: Reg,
        rm: Reg,
        n_top: bool,
        m_top: bool,
    },
    /// `SMUAD`/`SMLAD`/`SMUSD`/`SMLSD` (`x` swaps the halves of `rm`).
    DualMul {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
        sub: bool,
        x: bool,
    },
    /// `SMLALD`/`SMLSLD` (`x` swaps the halves of `rm`).
    DualMulLong {
        rdlo: Reg,
        rdhi: Reg,
        rn: Reg,
        rm: Reg,
        sub: bool,
        x: bool,
    },
    /// `SMMUL`/`SMMLA`/`SMMLS` (+`R` rounding): most-significant-word multiply.
    MulMsw {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
        sub: bool,
        round: bool,
    },
    /// `USAD8` (`ra == None`) / `USADA8`.
    Usad8 {
        rd: Reg,
        rn: Reg,
        rm: Reg,
        ra: Option<Reg>,
    },
    /// `SSAT16 rd, #bits, rn` (`bits` is 1..=16).
    Ssat16 {
        rd: Reg,
        rn: Reg,
        bits: u8,
    },
    /// `USAT16 rd, #bits, rn` (`bits` is 0..=15).
    Usat16 {
        rd: Reg,
        rn: Reg,
        bits: u8,
    },
    /// `SXTB16`/`UXTB16` (`rn == None`) and `SXTAB16`/`UXTAB16`.
    Extend16 {
        rd: Reg,
        rn: Option<Reg>,
        rm: Reg,
        rotate: u8,
        signed: bool,
    },
    Undefined {
        raw: u32,
    },
}
