//! 32-bit Thumb load/store decode: single, dual, multiple, exclusive, acquire/release,
//! table branch and preload hints.
//!
//! As elsewhere, encodings the ARM ARM marks UNPREDICTABLE (SP/PC operands, writeback with
//! `Rt == Rn`, short register lists, ...) decode to `Instr::Undefined`.

use super::t32::{field, gp};
use crate::instr::{Instr, MemSize};

/// `SG` (shares its space with the LDRD literal-with-writeback encoding, which does not exist).
const SG: u32 = 0xE97F;

/// Load/store multiple, dual, exclusive, acquire/release, table branch, SG and TT:
/// `1110 100x xxxx`.
pub(super) fn multiple_dual_exclusive(h1: u32, h2: u32) -> Option<Instr> {
    if h1 == SG && h2 == SG {
        return Some(Instr::Sg);
    }
    let p = h1 >> 8 & 1 != 0;
    let u = h1 >> 7 & 1 != 0;
    let w = h1 >> 5 & 1 != 0;
    let l = h1 >> 4 & 1 != 0;
    if h1 & 0x40 == 0 {
        // SRS / RFE (P == U) are not M-profile instructions.
        return if p == u {
            None
        } else {
            multiple(h1, h2, u, w, l)
        };
    }
    if p || w {
        dual(h1, h2, (p, u, w), l)
    } else if !u {
        exclusive_word(h1, h2, l)
    } else {
        byte_half_acquire(h1, h2, l)
    }
}

/// LDM / STM (IA when `ia`, otherwise DB); `PUSH.W` and `POP.W` are the SP-writeback forms.
fn multiple(h1: u32, h2: u32, ia: bool, wback: bool, load: bool) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let regs = h2 as u16;
    let bad_list = if load {
        regs & 0x2000 != 0 || regs & 0xC000 == 0xC000
    } else {
        regs & 0xA000 != 0
    };
    if rn == 15 || regs.count_ones() < 2 || bad_list || (wback && regs & (1 << rn) != 0) {
        return None;
    }
    Some(match (load, ia) {
        (true, true) if wback && rn == 13 => Instr::Pop { regs },
        (false, false) if wback && rn == 13 => Instr::Push { regs },
        (true, true) => Instr::Ldm { rn, regs, wback },
        (false, true) => Instr::Stm { rn, regs, wback },
        (true, false) => Instr::Ldmdb { rn, regs, wback },
        (false, false) => Instr::Stmdb { rn, regs, wback },
    })
}

/// LDRD / STRD (immediate and literal).
fn dual(h1: u32, h2: u32, (p, u, w): (bool, bool, bool), load: bool) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rt = field(h2, 12, 4) as u8;
    let rt2 = field(h2, 8, 4) as u8;
    let imm = field(h2, 0, 8) * 4;
    let ok = gp(rt)
        && gp(rt2)
        // Only LDRD requires two different registers; `strd r2, r2, [ip]` is valid.
        && !(load && rt == rt2)
        && !(w && (rn == rt || rn == rt2))
        // STRD has no literal form; LDRD literal cannot write back.
        && !(rn == 15 && (!load || w));
    ok.then_some(if load {
        Instr::Ldrd {
            rt,
            rt2,
            rn,
            imm,
            index: p,
            add: u,
            wback: w,
        }
    } else {
        Instr::Strd {
            rt,
            rt2,
            rn,
            imm,
            index: p,
            add: u,
            wback: w,
        }
    })
}

/// TT / TTT / TTA / TTAT: `1110 1000 0100 Rn | 1111 Rd A T 000000`.
fn test_target(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    (rn != 15 && gp(rd) && field(h2, 0, 6) == 0).then_some(Instr::Tt {
        rd,
        rn,
        alt: h2 & 0x80 != 0,
        unpriv: h2 & 0x40 != 0,
    })
}

/// LDREX / STREX (word), and TT (the STREX space with `Rt == 15`).
fn exclusive_word(h1: u32, h2: u32, load: bool) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rt = field(h2, 12, 4) as u8;
    let rd = field(h2, 8, 4) as u8;
    let imm = field(h2, 0, 8) * 4;
    if !load && rt == 15 {
        return test_target(h1, h2);
    }
    if rn == 15 || !gp(rt) {
        return None;
    }
    if load {
        return (rd == 15).then_some(Instr::Ldrex {
            rt,
            rn,
            imm,
            size: MemSize::Word,
            acquire: false,
        });
    }
    (gp(rd) && rd != rn && rd != rt).then_some(Instr::Strex {
        rd,
        rt,
        rn,
        imm,
        size: MemSize::Word,
        release: false,
    })
}

/// Byte/halfword exclusives, load-acquire/store-release, and TBB/TBH.
fn byte_half_acquire(h1: u32, h2: u32, load: bool) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rt = field(h2, 12, 4) as u8;
    let op3 = field(h2, 4, 4);
    let rm = field(h2, 0, 4) as u8;
    if load && op3 < 2 {
        // TBB / TBH
        return (h2 >> 8 == 0xF0 && rn != 13 && gp(rm)).then_some(Instr::Tb {
            rn,
            rm,
            half: op3 == 1,
        });
    }
    // Remaining forms have 1111 in bits 11:8.
    if rn == 15 || !gp(rt) || field(h2, 8, 4) != 0xF {
        return None;
    }
    let size = match op3 & 3 {
        0 => MemSize::Byte,
        1 => MemSize::Half,
        2 => MemSize::Word,
        _ => return None,
    };
    // op3<3:2>: 01 exclusive, 10 acquire/release, 11 acquire/release exclusive.
    let kind = op3 >> 2;
    let exclusive_store = |release| {
        (gp(rm) && rm != rn && rm != rt).then_some(Instr::Strex {
            rd: rm,
            rt,
            rn,
            imm: 0,
            size,
            release,
        })
    };
    match (load, kind, size) {
        // Word-sized exclusive forms exist only as LDAEX/STLEX.
        (_, 1, MemSize::Word) => None,
        (true, 1, _) | (true, 3, _) if rm == 15 => Some(Instr::Ldrex {
            rt,
            rn,
            imm: 0,
            size,
            acquire: kind == 3,
        }),
        (true, 2, _) if rm == 15 => Some(Instr::Lda { rt, rn, size }),
        (false, 1, _) => exclusive_store(false),
        (false, 3, _) => exclusive_store(true),
        (false, 2, _) if rm == 15 => Some(Instr::Stl { rt, rn, size }),
        _ => None,
    }
}

/// Load/store single data item: `1111 100S x size L Rn | ...`.
pub(super) fn load_store_single(h1: u32, h2: u32) -> Option<Instr> {
    let rn = field(h1, 0, 4) as u8;
    let rt = field(h2, 12, 4) as u8;
    let load = h1 & 0x10 != 0;
    let signed = h1 & 0x100 != 0;
    let imm12_form = h1 & 0x80 != 0;
    let size = match field(h1, 5, 2) {
        0 => MemSize::Byte,
        1 => MemSize::Half,
        2 => MemSize::Word,
        _ => return None,
    };
    if signed && (!load || size == MemSize::Word) {
        return None;
    }
    if load && rt == 15 && size != MemSize::Word {
        return preload(h1, h2, rn, (size, signed));
    }
    // Rt: PC is only valid for a word load; SP is valid for word accesses (not stores of PC).
    let rt_ok = match (load, size) {
        (_, MemSize::Word) => load || rt != 15,
        _ => gp(rt),
    };
    if !rt_ok {
        return None;
    }
    if load && rn == 15 {
        // Literal: the U bit is hw1[7] and the immediate is the whole of hw2[11:0].
        let (add, imm) = (imm12_form, field(h2, 0, 12));
        return Some(if size == MemSize::Word {
            Instr::LdrLit { rt, imm, add }
        } else {
            Instr::LoadImm {
                rt,
                rn,
                imm,
                size,
                signed,
                index: true,
                add,
                wback: false,
            }
        });
    }
    if rn == 15 {
        return None;
    }
    let make = |imm, index, add, wback| {
        if wback && rn == rt {
            return None;
        }
        Some(if load {
            Instr::LoadImm {
                rt,
                rn,
                imm,
                size,
                signed,
                index,
                add,
                wback,
            }
        } else {
            Instr::StoreImm {
                rt,
                rn,
                imm,
                size,
                index,
                add,
                wback,
            }
        })
    };
    if imm12_form {
        return make(field(h2, 0, 12), true, true, false);
    }
    if h2 & 0x800 != 0 {
        let (p, u, w) = (h2 & 0x400 != 0, h2 & 0x200 != 0, h2 & 0x100 != 0);
        let imm = field(h2, 0, 8);
        return match (p, u, w) {
            // LDRT / STRT
            (true, true, false) => (gp(rt)).then_some(if load {
                Instr::LoadUnpriv {
                    rt,
                    rn,
                    imm,
                    size,
                    signed,
                }
            } else {
                Instr::StoreUnpriv { rt, rn, imm, size }
            }),
            (false, _, false) => None,
            _ => make(imm, p, u, w),
        };
    }
    if h2 & 0xFC0 != 0 {
        return None;
    }
    let rm = field(h2, 0, 4) as u8;
    if !gp(rm) {
        return None;
    }
    let shift = field(h2, 4, 2) as u8;
    Some(if load {
        Instr::LoadReg {
            rt,
            rn,
            rm,
            shift,
            size,
            signed,
        }
    } else {
        Instr::StoreReg {
            rt,
            rn,
            rm,
            shift,
            size,
        }
    })
}

/// Loads with `Rt == 15` that are not word sized: PLD / PLI, or no-op hints.
fn preload(h1: u32, h2: u32, rn: u8, (size, signed): (MemSize, bool)) -> Option<Instr> {
    if size != MemSize::Byte {
        // PLDW and the unallocated halfword hints are NOPs on a core without the MP extension.
        return Some(Instr::Hint(crate::instr::HintOp::Nop));
    }
    let make = |rm, shift, imm, add, rn| {
        Some(Instr::Preload {
            instr: signed,
            rn,
            rm,
            shift,
            imm,
            add,
        })
    };
    let u = h1 & 0x80 != 0;
    if rn == 15 {
        return make(None, 0, field(h2, 0, 12), u, rn);
    }
    if u {
        return make(None, 0, field(h2, 0, 12), true, rn);
    }
    if h2 & 0xFC0 == 0 {
        let rm = field(h2, 0, 4) as u8;
        if !gp(rm) {
            return None;
        }
        return make(Some(rm), field(h2, 4, 2) as u8, 0, true, rn);
    }
    if field(h2, 8, 4) == 0xC {
        return make(None, 0, field(h2, 0, 8), false, rn);
    }
    None
}
