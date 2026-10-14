//! Disassembly in GNU/UAL syntax (lowercase, `sp`/`lr`/`pc` names).

use crate::bits::SRType;
use crate::instr::{AluOp, HintOp, Instr, MemSize, Reg, RevKind, Shift};

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
            len == 2,
        ),
        Instr::AddImm {
            rd,
            rn,
            imm: v,
            setflags,
            two_op,
        } => dnm(&format!("add{}", s(setflags)), rd, rn, imm(v), two_op),
        Instr::SubImm {
            rd,
            rn,
            imm: v,
            setflags,
            two_op,
        } => dnm(&format!("sub{}", s(setflags)), rd, rn, imm(v), two_op),
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
            len == 2 && !setflags,
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
            false,
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
                len == 2,
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
            if rotate == 0 {
                format!("{name} {}, {}", r(rd), r(rm))
            } else {
                format!("{name} {}, {}, ror #{rotate}", r(rd), r(rm))
            }
        }
        Instr::Rev { rd, rm, kind } => {
            let name = match kind {
                RevKind::Rev => "rev",
                RevKind::Rev16 => "rev16",
                RevKind::Revsh => "revsh",
            };
            format!("{name} {}, {}", r(rd), r(rm))
        }
        Instr::Undefined { raw } => match len {
            2 if raw & 0xFF00 == 0xDE00 => format!("udf #{}", raw & 0xFF),
            2 => format!(".short {raw:#06x}"),
            _ => format!(".inst.w {raw:#010x}"),
        },
    }
}
