//! Disassembly in GNU/UAL syntax (lowercase, `sp`/`lr`/`pc` names).

use crate::bits::SRType;
use crate::instr::{AluOp, Instr, Reg, Shift};

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

/// Three-operand form, collapsed to two operands for 16-bit `rd == rn`.
fn dnm(mnem: &str, rd: Reg, rn: Reg, last: String, len: u8) -> String {
    if len == 2 && rd == rn {
        format!("{mnem} {}, {last}", r(rd))
    } else {
        format!("{mnem} {}, {}, {last}", r(rd), r(rn))
    }
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
        } => format!("mov{} {}, {}", s(setflags), r(rd), imm(v)),
        Instr::MovReg { rd, rm, setflags } => format!("mov{} {}, {}", s(setflags), r(rd), r(rm)),
        Instr::Mvn {
            rd,
            rm,
            shift,
            setflags,
        } => {
            format!(
                "mvn{} {}, {}{}",
                s(setflags),
                r(rd),
                r(rm),
                shift_suffix(shift)
            )
        }
        Instr::ShiftImm {
            rd,
            rm,
            ty,
            amount,
            setflags,
        } => {
            format!(
                "{}{} {}, {}, #{amount}",
                ty_name(ty),
                s(setflags),
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
            &format!("{}{}", ty_name(ty), s(setflags)),
            rd,
            rn,
            r(rm).to_string(),
            len,
        ),
        Instr::AddImm {
            rd,
            rn,
            imm: v,
            setflags,
        } => dnm(&format!("add{}", s(setflags)), rd, rn, imm(v), len),
        Instr::SubImm {
            rd,
            rn,
            imm: v,
            setflags,
        } => dnm(&format!("sub{}", s(setflags)), rd, rn, imm(v), len),
        Instr::RsbImm {
            rd,
            rn,
            imm: v,
            setflags,
        } => {
            format!("rsb{} {}, {}, {}", s(setflags), r(rd), r(rn), imm(v))
        }
        Instr::AddReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => dnm(
            &format!("add{}", s(setflags)),
            rd,
            rn,
            format!("{}{}", r(rm), shift_suffix(shift)),
            len,
        ),
        Instr::SubReg {
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => dnm(
            &format!("sub{}", s(setflags)),
            rd,
            rn,
            format!("{}{}", r(rm), shift_suffix(shift)),
            len,
        ),
        Instr::AluReg {
            op,
            rd,
            rn,
            rm,
            shift,
            setflags,
        } => {
            let name = match op {
                AluOp::And => "and",
                AluOp::Eor => "eor",
                AluOp::Orr => "orr",
                AluOp::Bic => "bic",
                AluOp::Adc => "adc",
                AluOp::Sbc => "sbc",
            };
            dnm(
                &format!("{name}{}", s(setflags)),
                rd,
                rn,
                format!("{}{}", r(rm), shift_suffix(shift)),
                len,
            )
        }
        Instr::Mul {
            rd,
            rn,
            rm,
            setflags,
        } => {
            format!("mul{} {}, {}, {}", s(setflags), r(rd), r(rn), r(rm))
        }
        Instr::CmpImm { rn, imm: v } => format!("cmp {}, {}", r(rn), imm(v)),
        Instr::CmpReg { rn, rm, shift } => {
            format!("cmp {}, {}{}", r(rn), r(rm), shift_suffix(shift))
        }
        Instr::CmnReg { rn, rm, shift } => {
            format!("cmn {}, {}{}", r(rn), r(rm), shift_suffix(shift))
        }
        Instr::TstReg { rn, rm, shift } => {
            format!("tst {}, {}{}", r(rn), r(rm), shift_suffix(shift))
        }
        Instr::Adr { rd, imm: v } => {
            let target = (addr.wrapping_add(4) & !3).wrapping_add(v);
            format!("adr {}, {target:#x}", r(rd))
        }
        Instr::Bx { rm } => format!("bx {}", r(rm)),
        Instr::Blx { rm } => format!("blx {}", r(rm)),
        Instr::It { firstcond, mask } => it_text(firstcond, mask),
        Instr::Undefined { raw } => match len {
            2 if raw & 0xFF00 == 0xDE00 => format!("udf #{}", raw & 0xFF),
            2 => format!(".short {raw:#06x}"),
            _ => format!(".inst.w {raw:#010x}"),
        },
    }
}
