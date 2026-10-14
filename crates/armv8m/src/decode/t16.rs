//! 16-bit Thumb decode.

use crate::bits::SRType;
use crate::instr::{AluOp, HintOp, Instr, MemSize, RevKind, Shift};

fn reg(hw: u16, lsb: u32) -> u8 {
    ((hw >> lsb) & 7) as u8
}

/// Decode a 16-bit Thumb instruction.
pub fn decode16(hw: u16) -> Instr {
    let h = u32::from(hw);
    match hw >> 11 {
        // Shift by immediate: LSL, LSR, ASR
        0b00000..=0b00010 => {
            let ty = [SRType::Lsl, SRType::Lsr, SRType::Asr][(hw >> 11) as usize];
            let imm5 = (h >> 6) & 0x1F;
            let (rd, rm) = (reg(hw, 0), reg(hw, 3));
            match (ty, imm5) {
                (SRType::Lsl, 0) => Instr::MovReg {
                    rd,
                    rm,
                    setflags: true,
                },
                (_, 0) => Instr::ShiftImm {
                    rd,
                    rm,
                    ty,
                    amount: 32,
                    setflags: true,
                },
                (_, n) => Instr::ShiftImm {
                    rd,
                    rm,
                    ty,
                    amount: n,
                    setflags: true,
                },
            }
        }
        // Add/subtract register or 3-bit immediate
        0b00011 => {
            let (rd, rn) = (reg(hw, 0), reg(hw, 3));
            let field = (h >> 6) & 7;
            let rm = field as u8;
            match (hw >> 9) & 3 {
                0 => Instr::AddReg {
                    rd,
                    rn,
                    rm,
                    shift: Shift::NONE,
                    setflags: true,
                },
                1 => Instr::SubReg {
                    rd,
                    rn,
                    rm,
                    shift: Shift::NONE,
                    setflags: true,
                },
                2 => Instr::AddImm {
                    rd,
                    rn,
                    imm: field,
                    setflags: true,
                    two_op: false,
                    imm12: false,
                },
                _ => Instr::SubImm {
                    rd,
                    rn,
                    imm: field,
                    setflags: true,
                    two_op: false,
                    imm12: false,
                },
            }
        }
        // MOV/CMP/ADD/SUB with 8-bit immediate
        0b00100..=0b00111 => {
            let r = reg(hw, 8);
            let imm = h & 0xFF;
            match (hw >> 11) & 3 {
                0 => Instr::MovImm {
                    rd: r,
                    imm,
                    setflags: true,
                    carry: None,
                },
                1 => Instr::CmpImm { rn: r, imm },
                2 => Instr::AddImm {
                    rd: r,
                    rn: r,
                    imm,
                    setflags: true,
                    two_op: true,
                    imm12: false,
                },
                _ => Instr::SubImm {
                    rd: r,
                    rn: r,
                    imm,
                    setflags: true,
                    two_op: true,
                    imm12: false,
                },
            }
        }
        0b01000 if hw & 0x0400 == 0 => data_processing(hw),
        0b01000 => special_data(hw),
        // ADR / ADD (SP plus immediate)
        0b10100 => Instr::Adr {
            rd: reg(hw, 8),
            imm: (h & 0xFF) << 2,
        },
        0b10101 => Instr::AddImm {
            rd: reg(hw, 8),
            rn: 13,
            imm: (h & 0xFF) << 2,
            setflags: false,
            two_op: false,
            imm12: false,
        },
        // LDR (literal)
        0b01001 => Instr::LdrLit {
            rt: reg(hw, 8),
            imm: (h & 0xFF) << 2,
            add: true,
        },
        // Load/store register offset
        0b01010 | 0b01011 => load_store_reg(hw),
        // Load/store word/byte immediate
        0b01100..=0b01111 => {
            let (rt, rn) = (reg(hw, 0), reg(hw, 3));
            let imm5 = (h >> 6) & 0x1F;
            let byte = hw & 0x1000 != 0;
            let (size, imm) = if byte {
                (MemSize::Byte, imm5)
            } else {
                (MemSize::Word, imm5 << 2)
            };
            mem_imm(hw & 0x0800 != 0, rt, rn, imm, size)
        }
        // Load/store halfword immediate
        0b10000 | 0b10001 => mem_imm(
            hw & 0x0800 != 0,
            reg(hw, 0),
            reg(hw, 3),
            ((h >> 6) & 0x1F) << 1,
            MemSize::Half,
        ),
        // Load/store SP-relative
        0b10010 | 0b10011 => mem_imm(
            hw & 0x0800 != 0,
            reg(hw, 8),
            13,
            (h & 0xFF) << 2,
            MemSize::Word,
        ),
        0b10110 | 0b10111 => misc(hw),
        // STM / LDM
        0b11000 | 0b11001 => {
            let rn = reg(hw, 8);
            let regs = (h & 0xFF) as u16;
            if hw & 0x0800 == 0 {
                Instr::Stm {
                    rn,
                    regs,
                    wback: true,
                }
            } else {
                Instr::Ldm {
                    rn,
                    regs,
                    wback: regs & (1 << rn) == 0,
                }
            }
        }
        // Conditional branch, UDF, SVC
        0b11010 | 0b11011 => {
            let imm8 = h & 0xFF;
            match (hw >> 8) & 0xF {
                0xE => Instr::Undefined { raw: h },
                0xF => Instr::Svc { imm: imm8 as u8 },
                c => Instr::BCond {
                    cond: c as u8,
                    imm: i32::from(imm8 as u8 as i8) << 1,
                },
            }
        }
        // Unconditional branch
        0b11100 => Instr::B {
            imm: (((h & 0x7FF) << 21) as i32) >> 20,
        },
        _ => Instr::Undefined { raw: h },
    }
}

/// Data-processing (register), `010000 opcode Rm Rdn`.
fn data_processing(hw: u16) -> Instr {
    let rdn = reg(hw, 0);
    let rm = reg(hw, 3);
    let alu = |op| Instr::AluReg {
        op,
        rd: rdn,
        rn: rdn,
        rm,
        shift: Shift::NONE,
        setflags: true,
    };
    let sh = |ty| Instr::ShiftReg {
        rd: rdn,
        rn: rdn,
        rm,
        ty,
        setflags: true,
    };
    match (hw >> 6) & 0xF {
        0x0 => alu(AluOp::And),
        0x1 => alu(AluOp::Eor),
        0x2 => sh(SRType::Lsl),
        0x3 => sh(SRType::Lsr),
        0x4 => sh(SRType::Asr),
        0x5 => alu(AluOp::Adc),
        0x6 => alu(AluOp::Sbc),
        0x7 => sh(SRType::Ror),
        0x8 => Instr::TstReg {
            rn: rdn,
            rm,
            shift: Shift::NONE,
        },
        0x9 => Instr::RsbImm {
            rd: rdn,
            rn: rm,
            imm: 0,
            setflags: true,
        },
        0xA => Instr::CmpReg {
            rn: rdn,
            rm,
            shift: Shift::NONE,
        },
        0xB => Instr::CmnReg {
            rn: rdn,
            rm,
            shift: Shift::NONE,
        },
        0xC => alu(AluOp::Orr),
        0xD => Instr::Mul {
            rd: rdn,
            rn: rm,
            rm: rdn,
            setflags: true,
        },
        0xE => alu(AluOp::Bic),
        _ => Instr::Mvn {
            rd: rdn,
            rm,
            shift: Shift::NONE,
            setflags: true,
        },
    }
}

/// Special data instructions and branch exchange, `010001 op ...`.
fn special_data(hw: u16) -> Instr {
    let rdn = (((hw >> 4) & 8) | (hw & 7)) as u8;
    let rm = ((hw >> 3) & 0xF) as u8;
    match (hw >> 8) & 3 {
        0 => Instr::AddReg {
            rd: rdn,
            rn: rdn,
            rm,
            shift: Shift::NONE,
            setflags: false,
        },
        1 => Instr::CmpReg {
            rn: rdn,
            rm,
            shift: Shift::NONE,
        },
        2 => Instr::MovReg {
            rd: rdn,
            rm,
            setflags: false,
        },
        _ if hw & 7 != 0 => Instr::Undefined { raw: u32::from(hw) },
        _ if hw & 0x80 == 0 => Instr::Bx { rm },
        _ => Instr::Blx { rm },
    }
}

/// Immediate-offset load or store (`load` selects LDR over STR).
fn mem_imm(load: bool, rt: u8, rn: u8, imm: u32, size: MemSize) -> Instr {
    if load {
        Instr::LoadImm {
            rt,
            rn,
            imm,
            size,
            signed: false,
            index: true,
            add: true,
            wback: false,
        }
    } else {
        Instr::StoreImm {
            rt,
            rn,
            imm,
            size,
            index: true,
            add: true,
            wback: false,
        }
    }
}

/// Load/store register offset, `0101 opc Rm Rn Rt`.
fn load_store_reg(hw: u16) -> Instr {
    let (rt, rn, rm) = (reg(hw, 0), reg(hw, 3), reg(hw, 6));
    let store = |size| Instr::StoreReg {
        rt,
        rn,
        rm,
        shift: 0,
        size,
    };
    let load = |size, signed| Instr::LoadReg {
        rt,
        rn,
        rm,
        shift: 0,
        size,
        signed,
    };
    match (hw >> 9) & 7 {
        0 => store(MemSize::Word),
        1 => store(MemSize::Half),
        2 => store(MemSize::Byte),
        3 => load(MemSize::Byte, true),
        4 => load(MemSize::Word, false),
        5 => load(MemSize::Half, false),
        6 => load(MemSize::Byte, false),
        _ => load(MemSize::Half, true),
    }
}

/// Miscellaneous 16-bit instructions, `1011 xxxx ...`.
fn misc(hw: u16) -> Instr {
    let h = u32::from(hw);
    let (rd, rm) = (reg(hw, 0), reg(hw, 3));
    match (hw >> 8) & 0xF {
        0x0 => {
            let imm = (h & 0x7F) << 2;
            if hw & 0x80 == 0 {
                Instr::AddImm {
                    rd: 13,
                    rn: 13,
                    imm,
                    setflags: false,
                    two_op: true,
                    imm12: false,
                }
            } else {
                Instr::SubImm {
                    rd: 13,
                    rn: 13,
                    imm,
                    setflags: false,
                    two_op: true,
                    imm12: false,
                }
            }
        }
        // CBZ / CBNZ: 1011 op 0 i 1 imm5 Rn
        0x1 | 0x3 | 0x9 | 0xB => Instr::Cbz {
            rn: rd,
            imm: (((h >> 9) & 1) << 6) | (((h >> 3) & 0x1F) << 1),
            nonzero: hw & 0x0800 != 0,
        },
        0x2 => Instr::Extend {
            rd,
            rm,
            rotate: 0,
            signed: hw & 0x80 == 0,
            half: hw & 0x40 == 0,
        },
        0x6 if hw & 0xFFE8 == 0xB660 => Instr::Cps {
            disable: hw & 0x10 != 0,
            i: hw & 2 != 0,
            f: hw & 1 != 0,
        },
        0x4 | 0x5 => Instr::Push {
            regs: (hw & 0xFF) | ((hw & 0x100) << 6),
        },
        0xA => match (hw >> 6) & 3 {
            0 => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Rev,
            },
            1 => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Rev16,
            },
            3 => Instr::Rev {
                rd,
                rm,
                kind: RevKind::Revsh,
            },
            _ => Instr::Undefined { raw: h },
        },
        0xC | 0xD => Instr::Pop {
            regs: (hw & 0xFF) | ((hw & 0x100) << 7),
        },
        0xE => Instr::Bkpt { imm: h as u8 },
        // Hints and IT
        0xF if hw & 0xF != 0 => Instr::It {
            firstcond: ((hw >> 4) & 0xF) as u8,
            mask: (hw & 0xF) as u8,
        },
        0xF => Instr::Hint(match (hw >> 4) & 0xF {
            1 => HintOp::Yield,
            2 => HintOp::Wfe,
            3 => HintOp::Wfi,
            4 => HintOp::Sev,
            // NOP and the hints without a visible effect here (SEVL, reserved).
            _ => HintOp::Nop,
        }),
        _ => Instr::Undefined { raw: h },
    }
}
