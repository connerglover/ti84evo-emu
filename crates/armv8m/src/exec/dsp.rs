//! DSP extension: parallel add/subtract, saturating arithmetic, DSP multiplies, `USAD8`,
//! `SSAT16`/`USAT16` and `SXTB16`/`UXTB16`.

use super::Flow;
use crate::cpu::Cpu;
use crate::instr::{Instr, ParKind, ParOp};

/// Clamp `v` to `min..=max`, setting the sticky Q flag when it was clamped.
fn sat(cpu: &mut Cpu, v: i64, min: i64, max: i64) -> i64 {
    let r = v.clamp(min, max);
    if r != v {
        cpu.regs.xpsr.q = true;
    }
    r
}

/// Signed 32-bit saturation (`SignedSatQ(v, 32)`).
fn sat32(cpu: &mut Cpu, v: i64) -> u32 {
    sat(cpu, v, i64::from(i32::MIN), i64::from(i32::MAX)) as u32
}

/// Set Q when `v` does not fit in a signed 32-bit register (accumulate overflow, no clamping).
fn q_on_overflow(cpu: &mut Cpu, v: i64) -> u32 {
    if v != i64::from(v as i32) {
        cpu.regs.xpsr.q = true;
    }
    v as u32
}

/// Signed halfword of `v`: the top one when `top`, else the bottom one.
fn half(v: u32, top: bool) -> i64 {
    i64::from(if top { (v >> 16) as i16 } else { v as i16 })
}

/// The two signed 16 x 16 products of `DualMul`: `(bottom, top)`, with `rm` swapped when `x`.
fn dual_products(n: u32, m: u32, x: bool) -> (i64, i64) {
    let m = if x { m.rotate_right(16) } else { m };
    (
        half(n, false) * half(m, false),
        half(n, true) * half(m, true),
    )
}

/// Parallel add/subtract on 16- or 8-bit lanes; returns `(result, ge)` (`ge` only for S/U).
fn parallel(kind: ParKind, op: ParOp, n: u32, m: u32) -> (u32, Option<u8>) {
    let (width, lanes) = match op {
        ParOp::Add8 | ParOp::Sub8 => (8u32, 4u32),
        _ => (16, 2),
    };
    let mask = (1u32 << width) - 1;
    let signed = matches!(kind, ParKind::S | ParKind::Q | ParKind::Sh);
    let extract = |v: u32, lane: u32| {
        let raw = (v >> (width * lane)) & mask;
        if signed {
            // Sign-extend from `width` bits.
            ((raw << (32 - width)) as i32) >> (32 - width)
        } else {
            raw as i32
        }
    };
    let (mut res, mut ge) = (0u32, 0u8);
    for i in 0..lanes {
        let (j, sub) = match op {
            ParOp::Add16 | ParOp::Add8 => (i, false),
            ParOp::Sub16 | ParOp::Sub8 => (i, true),
            ParOp::Asx => (1 - i, i == 0),
            ParOp::Sax => (1 - i, i != 0),
        };
        let (a, b) = (extract(n, i), extract(m, j));
        let r = if sub { a - b } else { a + b };
        let out = match kind {
            ParKind::S | ParKind::U => r,
            ParKind::Q => r.clamp(-(1 << (width - 1)), (1 << (width - 1)) - 1),
            ParKind::Uq => r.clamp(0, mask as i32),
            ParKind::Sh | ParKind::Uh => r >> 1,
        };
        res |= (out as u32 & mask) << (width * i);
        // GE: signed results and all differences test `>= 0`; unsigned sums test for carry.
        let flag = if signed || sub {
            r >= 0
        } else {
            r > mask as i32
        };
        if flag {
            ge |= (if width == 16 { 0b11 } else { 1 }) << (if width == 16 { 2 * i } else { i });
        }
    }
    (res, matches!(kind, ParKind::S | ParKind::U).then_some(ge))
}

/// Execute a DSP instruction; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, instr: &Instr) -> Option<Flow> {
    match *instr {
        Instr::Parallel {
            kind,
            op,
            rd,
            rn,
            rm,
        } => {
            let (res, ge) = parallel(kind, op, cpu.reg(rn), cpu.reg(rm));
            if let Some(ge) = ge {
                cpu.regs.xpsr.ge = ge;
            }
            cpu.write_reg(rd, res);
        }
        Instr::SatArith {
            rd,
            rm,
            rn,
            sub,
            double,
        } => {
            let mut b = i64::from(cpu.reg(rn) as i32);
            if double {
                b = i64::from(sat32(cpu, 2 * b) as i32);
            }
            let a = i64::from(cpu.reg(rm) as i32);
            let r = sat32(cpu, if sub { a - b } else { a + b });
            cpu.write_reg(rd, r);
        }
        Instr::MulHalf {
            rd,
            rn,
            rm,
            ra,
            n_top,
            m_top,
        } => {
            let acc = ra.map_or(0, |r| i64::from(cpu.reg(r) as i32));
            let p = half(cpu.reg(rn), n_top) * half(cpu.reg(rm), m_top) + acc;
            let r = q_on_overflow(cpu, p);
            cpu.write_reg(rd, r);
        }
        Instr::MulWordHalf {
            rd,
            rn,
            rm,
            ra,
            top,
        } => {
            let acc = ra.map_or(0, |r| i64::from(cpu.reg(r) as i32) << 16);
            let p = i64::from(cpu.reg(rn) as i32) * half(cpu.reg(rm), top) + acc;
            let r = q_on_overflow(cpu, p >> 16);
            cpu.write_reg(rd, r);
        }
        Instr::MulLongHalf {
            rdlo,
            rdhi,
            rn,
            rm,
            n_top,
            m_top,
        } => {
            let p = half(cpu.reg(rn), n_top) * half(cpu.reg(rm), m_top);
            let acc = (u64::from(cpu.reg(rdhi)) << 32) | u64::from(cpu.reg(rdlo));
            set_long(cpu, rdlo, rdhi, acc.wrapping_add(p as u64));
        }
        Instr::DualMul {
            rd,
            rn,
            rm,
            ra,
            sub,
            x,
        } => {
            let (p1, p2) = dual_products(cpu.reg(rn), cpu.reg(rm), x);
            let acc = ra.map_or(0, |r| i64::from(cpu.reg(r) as i32));
            let r = q_on_overflow(cpu, if sub { p1 - p2 } else { p1 + p2 } + acc);
            cpu.write_reg(rd, r);
        }
        Instr::DualMulLong {
            rdlo,
            rdhi,
            rn,
            rm,
            sub,
            x,
        } => {
            let (p1, p2) = dual_products(cpu.reg(rn), cpu.reg(rm), x);
            let acc = (u64::from(cpu.reg(rdhi)) << 32) | u64::from(cpu.reg(rdlo));
            let p = if sub { p1 - p2 } else { p1 + p2 };
            set_long(cpu, rdlo, rdhi, acc.wrapping_add(p as u64));
        }
        Instr::MulMsw {
            rd,
            rn,
            rm,
            ra,
            sub,
            round,
        } => {
            let p = i64::from(cpu.reg(rn) as i32) * i64::from(cpu.reg(rm) as i32);
            let mut v = match ra {
                None => p,
                Some(r) => {
                    let a = i64::from(cpu.reg(r) as i32) << 32;
                    if sub {
                        a.wrapping_sub(p)
                    } else {
                        a.wrapping_add(p)
                    }
                }
            };
            if round {
                v = v.wrapping_add(0x8000_0000);
            }
            cpu.write_reg(rd, (v >> 32) as u32);
        }
        Instr::Usad8 { rd, rn, rm, ra } => {
            let (n, m) = (cpu.reg(rn).to_le_bytes(), cpu.reg(rm).to_le_bytes());
            let sum: u32 = n
                .iter()
                .zip(m)
                .map(|(&a, b)| u32::from(a.abs_diff(b)))
                .sum();
            let acc = ra.map_or(0, |r| cpu.reg(r));
            cpu.write_reg(rd, sum.wrapping_add(acc));
        }
        Instr::Ssat16 { rd, rn, bits } => {
            let v = cpu.reg(rn);
            let (min, max) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
            let lo = sat(cpu, half(v, false), min, max) as u32 & 0xFFFF;
            let hi = sat(cpu, half(v, true), min, max) as u32 & 0xFFFF;
            cpu.write_reg(rd, (hi << 16) | lo);
        }
        Instr::Usat16 { rd, rn, bits } => {
            let v = cpu.reg(rn);
            let max = (1i64 << bits) - 1;
            let lo = sat(cpu, half(v, false), 0, max) as u32;
            let hi = sat(cpu, half(v, true), 0, max) as u32;
            cpu.write_reg(rd, (hi << 16) | lo);
        }
        Instr::Extend16 {
            rd,
            rn,
            rm,
            rotate,
            signed,
        } => {
            let v = cpu.reg(rm).rotate_right(u32::from(rotate));
            let ext = |b: u32| -> u16 {
                if signed {
                    b as u8 as i8 as i16 as u16
                } else {
                    u16::from(b as u8)
                }
            };
            let (mut lo, mut hi) = (ext(v), ext(v >> 16));
            if let Some(rn) = rn {
                let n = cpu.reg(rn);
                lo = lo.wrapping_add(n as u16);
                hi = hi.wrapping_add((n >> 16) as u16);
            }
            cpu.write_reg(rd, (u32::from(hi) << 16) | u32::from(lo));
        }
        _ => return None,
    }
    Some(Flow::Next)
}

fn set_long(cpu: &mut Cpu, rdlo: u8, rdhi: u8, v: u64) {
    cpu.write_reg(rdlo, v as u32);
    cpu.write_reg(rdhi, (v >> 32) as u32);
}
