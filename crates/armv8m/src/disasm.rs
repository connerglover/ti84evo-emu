//! Disassembly in GNU/UAL syntax (lowercase, `sp`/`lr`/`pc` names).

use crate::bits::SRType;
use crate::instr::{AluOp, HintOp, Instr, LongMulKind, MemSize, Reg, RevKind, Shift};

const COND: [&str; 16] = [
    "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al", "nv",
];

fn r(n: Reg) -> &'static str {
    const NAMES: [&str; 16] = [
        "r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "sp",
        "lr", "pc",
    ];
    NAMES[usize::from(n & 15)]
}

/// `.w` suffix for 32-bit encodings of mnemonics that also have a 16-bit form.
fn w(len: u8) -> &'static str {
    if len == 4 { ".w" } else { "" }
}

/// Mnemonic (with `s`) of a logical / carry ALU operation.
fn alu_name(op: AluOp, setflags: bool) -> String {
    let name = match op {
        AluOp::And => "and",
        AluOp::Eor => "eor",
        AluOp::Orr => "orr",
        AluOp::Orn => "orn",
        AluOp::Bic => "bic",
        AluOp::Adc => "adc",
        AluOp::Sbc => "sbc",
    };
    format!("{name}{}", s(setflags))
}

/// `ORN` has no 16-bit form, so no `.w`.
fn alu_w(op: AluOp, len: u8) -> &'static str {
    if op == AluOp::Orn { "" } else { w(len) }
}

fn s(setflags: bool) -> &'static str {
    if setflags { "s" } else { "" }
}

fn imm(v: u32) -> String {
    if v < 256 {
        format!("#{v}")
    } else {
        format!("#{v:#x}")
    }
}

fn ty_name(t: SRType) -> &'static str {
    match t {
        SRType::Lsl => "lsl",
        SRType::Lsr => "lsr",
        SRType::Asr => "asr",
        SRType::Ror => "ror",
        SRType::Rrx => "rrx",
    }
}

fn shift_suffix(sh: Shift) -> String {
    match (sh.ty, sh.amount) {
        (SRType::Rrx, _) => ", rrx".to_string(),
        (SRType::Lsl, 0) => String::new(),
        (t, n) => format!(", {} #{n}", ty_name(t)),
    }
}

/// Three-operand form, printed with two operands only where UAL spells the encoding that way.
fn dnm(mnem: &str, rd: Reg, rn: Reg, last: String, two_op: bool) -> String {
    if two_op && rd == rn {
        format!("{mnem} {}, {last}", r(rd))
    } else {
        format!("{mnem} {}, {}, {last}", r(rd), r(rn))
    }
}

fn size_suffix(size: MemSize, signed: bool) -> &'static str {
    match (size, signed) {
        (MemSize::Byte, false) => "b",
        (MemSize::Byte, true) => "sb",
        (MemSize::Half, false) => "h",
        (MemSize::Half, true) => "sh",
        (MemSize::Word, _) => "",
    }
}

fn bang(wback: bool) -> &'static str {
    if wback { "!" } else { "" }
}

/// `{r0, r1, lr}`
fn reg_list(regs: u16) -> String {
    let names: Vec<&str> = (0..16u8).filter(|&i| regs & (1 << i) != 0).map(r).collect();
    format!("{{{}}}", names.join(", "))
}

fn mem_imm_text(
    name: &str,
    rt: Reg,
    rn: Reg,
    v: u32,
    index: bool,
    add: bool,
    wback: bool,
) -> String {
    let sign = if add { "" } else { "-" };
    match (index, wback) {
        (true, false) if v == 0 && add => format!("{name} {}, [{}]", r(rt), r(rn)),
        (true, false) => format!("{name} {}, [{}, #{sign}{v}]", r(rt), r(rn)),
        (true, true) => format!("{name} {}, [{}, #{sign}{v}]!", r(rt), r(rn)),
        (false, _) => format!("{name} {}, [{}], #{sign}{v}", r(rt), r(rn)),
    }
}

fn mem_reg_text(name: &str, rt: Reg, rn: Reg, rm: Reg, shift: u8) -> String {
    if shift == 0 {
        format!("{name} {}, [{}, {}]", r(rt), r(rn), r(rm))
    } else {
        format!("{name} {}, [{}, {}, lsl #{shift}]", r(rt), r(rn), r(rm))
    }
}

/// Branch target for an offset relative to `PC + 4`.
fn branch_target(addr: u32, off: i32) -> u32 {
    addr.wrapping_add(4).wrapping_add(off as u32)
}

fn it_text(firstcond: u8, mask: u8) -> String {
    let fc0 = firstcond & 1;
    let count = 4 - mask.trailing_zeros() as usize;
    let mut name = String::from("it");
    for i in 1..count {
        name.push(if (mask >> (4 - i)) & 1 == fc0 {
            't'
        } else {
            'e'
        });
    }
    format!("{name} {}", COND[usize::from(firstcond & 15)])
}

/// Disassemble `instr`, located at `addr` and `len` bytes long.
pub fn disasm(instr: &Instr, addr: u32, len: u8) -> String {
    match *instr {
        Instr::MovImm {
            rd,
            imm: v,
            setflags,
            ..
        } => format!("mov{}{} {}, {}", s(setflags), w(len), r(rd), imm(v)),
        Instr::Movw { rd, imm: v } => format!("movw {}, {}", r(rd), imm(u32::from(v))),
        Instr::Movt { rd, imm: v } => format!("movt {}, {}", r(rd), imm(u32::from(v))),
        Instr::MvnImm {
            rd,
            imm: v,
            setflags,
            ..
        } => format!("mvn{}{} {}, {}", s(setflags), w(len), r(rd), imm(v)),
        Instr::MovReg { rd, rm, setflags } => {
            format!("mov{}{} {}, {}", s(setflags), w(len), r(rd), r(rm))
        }
        Instr::Mvn {
            rd,
            rm,
            shift,
            setflags,
        } => {
            format!(
                "mvn{}{} {}, {}{}",
                s(setflags),
                w(len),
                r(rd),
                r(rm),
                shift_suffix(shift)
            )
        }
        Instr::ShiftImm {
            rd,
            rm,
            ty: SRType::Rrx,
            setflags,
            ..
        } => format!("rrx{} {}, {}", s(setflags), r(rd), r(rm)),
        Instr::ShiftImm {
            rd,
            rm,
            ty,
            amount,
            setflags,
        } => {
            format!(
                "{}{}{} {}, {}, #{amount}",
                ty_name(ty),
                s(setflags),
                w(len),
                r(rd),
                r(rm)
            )
        }
        Instr::ShiftReg {
            rd,
            rn,
            rm,
            ty,
            setflags,
        } => dnm(
            &format!("{}{}{}", ty_name(ty), s(setflags), w(len)),
            rd,
            rn,
            r(rm).to_string(),
            len == 2,
        ),
        Instr::AddImm {
            rd,
            rn,
            imm: v,
            setflags,
            two_op,
            imm12,
        } => {
            let name = if imm12 {
                "addw".to_string()
            } else {
                format!("add{}{}", s(setflags), w(len))
            };
            dnm(&name, rd, rn, imm(v), two_op)
        }
        Instr::SubImm {
            rd,
            rn,
            imm: v,
            setflags,
            two_op,
            imm12,
        } => {
            let name = if imm12 {
                "subw".to_string()
            } else {
                format!("sub{}{}", s(setflags), w(len))
            };
            dnm(&name, rd, rn, imm(v), two_op)
        }
        Instr::RsbImm {
            rd,
            rn,
            imm: v,
            setflags,
        } => {
            format!("rsb{} {}, {}, {}", s(setflags), r(rd), r(rn), imm(v))
        }
        Instr::RsbReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => format!(
            "rsb{} {}, {}, {}{}",
            s(setflags),
            r(rd),
            r(rn),
            r(rm),
            shift_suffix(shift)
        ),
        Instr::AddReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => dnm(
            &format!("add{}{}", s(setflags), w(len)),
            rd,
            rn,
            format!("{}{}", r(rm), shift_suffix(shift)),
            len == 2 && !setflags,
        ),
        Instr::SubReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => dnm(
            &format!("sub{}{}", s(setflags), w(len)),
            rd,
            rn,
            format!("{}{}", r(rm), shift_suffix(shift)),
            false,
        ),
        Instr::AluReg {
            op,
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => dnm(
            &format!("{}{}", alu_name(op, setflags), alu_w(op, len)),
            rd,
            rn,
            format!("{}{}", r(rm), shift_suffix(shift)),
            len == 2,
        ),
        Instr::AluImm {
            op,
            rd,
            rn,
            imm: v,
            setflags,
            ..
        } => dnm(
            &format!("{}{}", alu_name(op, setflags), alu_w(op, len)),
            rd,
            rn,
            imm(v),
            false,
        ),
        Instr::Mul {
            rd,
            rn,
            rm,
            setflags,
        } => {
            format!(
                "mul{}{} {}, {}, {}",
                s(setflags),
                w(len),
                r(rd),
                r(rn),
                r(rm)
            )
        }
        Instr::MulAcc {
            rd,
            rn,
            rm,
            ra,
            sub,
        } => format!(
            "{} {}, {}, {}, {}",
            if sub { "mls" } else { "mla" },
            r(rd),
            r(rn),
            r(rm),
            r(ra)
        ),
        Instr::LongMul {
            kind,
            rdlo,
            rdhi,
            rn,
            rm,
        } => {
            let name = match kind {
                LongMulKind::Smull => "smull",
                LongMulKind::Umull => "umull",
                LongMulKind::Smlal => "smlal",
                LongMulKind::Umlal => "umlal",
                LongMulKind::Umaal => "umaal",
            };
            format!("{name} {}, {}, {}, {}", r(rdlo), r(rdhi), r(rn), r(rm))
        }
        Instr::Div { rd, rn, rm, signed } => format!(
            "{} {}, {}, {}",
            if signed { "sdiv" } else { "udiv" },
            r(rd),
            r(rn),
            r(rm)
        ),
        Instr::CmpImm { rn, imm: v } => format!("cmp{} {}, {}", w(len), r(rn), imm(v)),
        Instr::CmnImm { rn, imm: v } => format!("cmn{} {}, {}", w(len), r(rn), imm(v)),
        Instr::TstImm { rn, imm: v, .. } => format!("tst{} {}, {}", w(len), r(rn), imm(v)),
        Instr::TeqImm { rn, imm: v, .. } => format!("teq {}, {}", r(rn), imm(v)),
        Instr::CmpReg { rn, rm, shift } => {
            format!("cmp{} {}, {}{}", w(len), r(rn), r(rm), shift_suffix(shift))
        }
        Instr::CmnReg { rn, rm, shift } => {
            format!("cmn{} {}, {}{}", w(len), r(rn), r(rm), shift_suffix(shift))
        }
        Instr::TstReg { rn, rm, shift } => {
            format!("tst{} {}, {}{}", w(len), r(rn), r(rm), shift_suffix(shift))
        }
        Instr::TeqReg { rn, rm, shift } => {
            format!("teq {}, {}{}", r(rn), r(rm), shift_suffix(shift))
        }
        Instr::Adr { rd, imm: v } => {
            let target = (addr.wrapping_add(4) & !3).wrapping_add(v);
            format!("adr{} {}, {target:#x}", w(len), r(rd))
        }
        Instr::Bx { rm } => format!("bx {}", r(rm)),
        Instr::Blx { rm } => format!("blx {}", r(rm)),
        Instr::It { firstcond, mask } => it_text(firstcond, mask),
        Instr::LoadImm {
            rt,
            rn,
            imm: v,
            size,
            signed,
            index,
            add,
            wback,
        } => {
            let name = format!("ldr{}", size_suffix(size, signed));
            mem_imm_text(&name, rt, rn, v, index, add, wback)
        }
        Instr::StoreImm {
            rt,
            rn,
            imm: v,
            size,
            index,
            add,
            wback,
        } => {
            let name = format!("str{}", size_suffix(size, false));
            mem_imm_text(&name, rt, rn, v, index, add, wback)
        }
        Instr::LoadReg {
            rt,
            rn,
            rm,
            shift,
            size,
            signed,
        } => {
            let name = format!("ldr{}", size_suffix(size, signed));
            mem_reg_text(&name, rt, rn, rm, shift)
        }
        Instr::StoreReg {
            rt,
            rn,
            rm,
            shift,
            size,
        } => {
            let name = format!("str{}", size_suffix(size, false));
            mem_reg_text(&name, rt, rn, rm, shift)
        }
        Instr::LdrLit { rt, imm: v, add } => {
            let sign = if add { "" } else { "-" };
            if v == 0 && add {
                format!("ldr {}, [pc]", r(rt))
            } else {
                format!("ldr {}, [pc, #{sign}{v}]", r(rt))
            }
        }
        Instr::Push { regs } => format!("push {}", reg_list(regs)),
        Instr::Pop { regs } => format!("pop {}", reg_list(regs)),
        Instr::Ldm { rn, regs, wback } => {
            format!("ldmia {}{}, {}", r(rn), bang(wback), reg_list(regs))
        }
        Instr::Stm { rn, regs, wback } => {
            format!("stmia {}{}, {}", r(rn), bang(wback), reg_list(regs))
        }
        Instr::B { imm: off } => format!("b {:#x}", branch_target(addr, off)),
        Instr::BCond { cond, imm: off } => format!(
            "b{} {:#x}",
            COND[usize::from(cond & 15)],
            branch_target(addr, off)
        ),
        Instr::Cbz {
            rn,
            imm: off,
            nonzero,
        } => format!(
            "cb{}z {}, {:#x}",
            if nonzero { "n" } else { "" },
            r(rn),
            branch_target(addr, off as i32)
        ),
        Instr::Svc { imm: v } => format!("svc {}", imm(u32::from(v))),
        Instr::Bkpt { imm: v } => format!("bkpt #{v:#x}"),
        Instr::Cps { disable, i, f } => format!(
            "cps{} {}{}",
            if disable { "id" } else { "ie" },
            if i { "i" } else { "" },
            if f { "f" } else { "" }
        ),
        Instr::Hint(op) => match op {
            HintOp::Nop => "nop",
            HintOp::Yield => "yield",
            HintOp::Wfe => "wfe",
            HintOp::Wfi => "wfi",
            HintOp::Sev => "sev",
        }
        .to_string(),
        Instr::Extend {
            rd,
            rm,
            rotate,
            signed,
            half,
        } => {
            let name = format!(
                "{}xt{}",
                if signed { "s" } else { "u" },
                if half { "h" } else { "b" }
            );
            let name = format!("{name}{}", w(len));
            if rotate == 0 {
                format!("{name} {}, {}", r(rd), r(rm))
            } else {
                format!("{name} {}, {}, ror #{rotate}", r(rd), r(rm))
            }
        }
        Instr::ExtendAdd {
            rd,
            rn,
            rm,
            rotate,
            signed,
            half,
        } => {
            let name = format!(
                "{}xta{}",
                if signed { "s" } else { "u" },
                if half { "h" } else { "b" }
            );
            if rotate == 0 {
                format!("{name} {}, {}, {}", r(rd), r(rn), r(rm))
            } else {
                format!("{name} {}, {}, {}, ror #{rotate}", r(rd), r(rn), r(rm))
            }
        }
        Instr::Rev { rd, rm, kind } => {
            let name = match kind {
                RevKind::Rev => "rev",
                RevKind::Rev16 => "rev16",
                RevKind::Revsh => "revsh",
            };
            format!("{name}{} {}, {}", w(len), r(rd), r(rm))
        }
        Instr::Clz { rd, rm } => format!("clz {}, {}", r(rd), r(rm)),
        Instr::Rbit { rd, rm } => format!("rbit {}, {}", r(rd), r(rm)),
        Instr::Sel { rd, rn, rm } => format!("sel {}, {}, {}", r(rd), r(rn), r(rm)),
        Instr::Pkh {
            rd,
            rn,
            rm,
            tb,
            amount,
        } => {
            if tb {
                format!("pkhtb {}, {}, {}, asr #{amount}", r(rd), r(rn), r(rm))
            } else if amount == 0 {
                format!("pkhbt {}, {}, {}", r(rd), r(rn), r(rm))
            } else {
                format!("pkhbt {}, {}, {}, lsl #{amount}", r(rd), r(rn), r(rm))
            }
        }
        Instr::Ssat {
            rd,
            rn,
            bits,
            shift,
        } => format!("ssat {}, #{bits}, {}{}", r(rd), r(rn), shift_suffix(shift)),
        Instr::Usat {
            rd,
            rn,
            bits,
            shift,
        } => format!("usat {}, #{bits}, {}{}", r(rd), r(rn), shift_suffix(shift)),
        Instr::Bfx {
            rd,
            rn,
            lsb,
            width,
            signed,
        } => format!(
            "{}bfx {}, {}, #{lsb}, #{width}",
            if signed { "s" } else { "u" },
            r(rd),
            r(rn)
        ),
        Instr::Bfi { rd, rn, lsb, width } => {
            format!("bfi {}, {}, #{lsb}, #{width}", r(rd), r(rn))
        }
        Instr::Bfc { rd, lsb, width } => format!("bfc {}, #{lsb}, #{width}", r(rd)),
        Instr::Undefined { raw } => match len {
            2 if raw & 0xFF00 == 0xDE00 => format!("udf #{}", raw & 0xFF),
            2 => format!(".short {raw:#06x}"),
            _ => format!(".inst.w {raw:#010x}"),
        },
    }
}
