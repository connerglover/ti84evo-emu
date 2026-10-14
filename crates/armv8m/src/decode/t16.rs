//! 16-bit Thumb decode.

use crate::bits::SRType;
use crate::instr::{AluOp, Instr, Shift};

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
                },
                _ => Instr::SubImm {
                    rd,
                    rn,
                    imm: field,
                    setflags: true,
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
                },
                1 => Instr::CmpImm { rn: r, imm },
                2 => Instr::AddImm {
                    rd: r,
                    rn: r,
                    imm,
                    setflags: true,
                },
                _ => Instr::SubImm {
                    rd: r,
                    rn: r,
                    imm,
                    setflags: true,
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
        },
        // Miscellaneous: only ADD/SUB SP, #imm7 and IT here; Task 3 adds the rest (hints included).
        0b10110 if hw & 0x0F00 == 0 => {
            let imm = (h & 0x7F) << 2;
            if hw & 0x80 == 0 {
                Instr::AddImm {
                    rd: 13,
                    rn: 13,
                    imm,
                    setflags: false,
                }
            } else {
                Instr::SubImm {
                    rd: 13,
                    rn: 13,
                    imm,
                    setflags: false,
                }
            }
        }
        0b10111 if hw & 0x0F00 == 0x0F00 && hw & 0xF != 0 => Instr::It {
            firstcond: ((hw >> 4) & 0xF) as u8,
            mask: (hw & 0xF) as u8,
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
