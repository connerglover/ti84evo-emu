//! Load and store instructions.

use super::Flow;
use crate::bus::{AccessKind, Bus, BusError};
use crate::cpu::Cpu;
use crate::instr::{Instr, MemSize};

/// Why a memory instruction failed.
#[derive(Clone, Copy, Debug)]
enum MemFault {
    // Task 7: the BusError selects the fault status bits; unread until then.
    Bus(#[allow(dead_code)] BusError),
    /// LDM/STM/PUSH/POP with a base address that is not word aligned.
    Unaligned,
}

/// The single place where a memory failure is turned into a fault.
fn fault(cause: MemFault) -> Flow {
    // Task 7: choose the fault here: Bus(_) -> BusFault PRECISERR (or MemManage/SecureFault
    // by cause), Unaligned -> UsageFault UNALIGNED.
    match cause {
        MemFault::Bus(_) | MemFault::Unaligned => Flow::Fault,
    }
}

fn read(cpu: &Cpu, bus: &mut impl Bus, addr: u32, size: MemSize) -> Result<u32, MemFault> {
    let acc = cpu.access(AccessKind::Read);
    let r = match size {
        MemSize::Byte => bus.read8(addr, acc).map(u32::from),
        MemSize::Half if addr & 1 == 0 => bus.read16(addr, acc).map(u32::from),
        MemSize::Word if addr & 3 == 0 => bus.read32(addr, acc),
        // Unaligned accesses are permitted (UNALIGN_TRP = 0); do them bytewise.
        _ => {
            let n = if size == MemSize::Half { 2 } else { 4 };
            let mut v = 0;
            for i in 0..n {
                let b = bus
                    .read8(addr.wrapping_add(i), acc)
                    .map_err(MemFault::Bus)?;
                v |= u32::from(b) << (8 * i);
            }
            Ok(v)
        }
    };
    r.map_err(MemFault::Bus)
}

fn write(cpu: &Cpu, bus: &mut impl Bus, addr: u32, v: u32, size: MemSize) -> Result<(), MemFault> {
    let acc = cpu.access(AccessKind::Write);
    let r = match size {
        MemSize::Byte => bus.write8(addr, v as u8, acc),
        MemSize::Half if addr & 1 == 0 => bus.write16(addr, v as u16, acc),
        MemSize::Word if addr & 3 == 0 => bus.write32(addr, v, acc),
        _ => {
            let n = if size == MemSize::Half { 2 } else { 4 };
            for i in 0..n {
                bus.write8(addr.wrapping_add(i), (v >> (8 * i)) as u8, acc)
                    .map_err(MemFault::Bus)?;
            }
            Ok(())
        }
    };
    r.map_err(MemFault::Bus)
}

fn extend(v: u32, size: MemSize, signed: bool) -> u32 {
    match (size, signed) {
        (MemSize::Byte, true) => v as u8 as i8 as i32 as u32,
        (MemSize::Half, true) => v as u16 as i16 as i32 as u32,
        _ => v,
    }
}

/// Write a loaded value; a PC destination is a `LoadWritePC` interworking branch.
fn write_loaded(cpu: &mut Cpu, rt: u8, v: u32) -> Flow {
    if rt == 15 {
        cpu.bx_write_pc(v);
        Flow::Branched
    } else {
        cpu.set_reg(rt, v);
        Flow::Next
    }
}

fn offset_addr(base: u32, imm: u32, add: bool) -> u32 {
    if add {
        base.wrapping_add(imm)
    } else {
        base.wrapping_sub(imm)
    }
}

/// Register list as ascending register numbers.
fn list(regs: u16) -> impl Iterator<Item = u8> {
    (0..16u8).filter(move |&i| regs & (1 << i) != 0)
}

/// Read `regs` from consecutive words at `base` without changing any register.
fn load_multiple(
    cpu: &Cpu,
    bus: &mut impl Bus,
    base: u32,
    regs: u16,
) -> Result<Vec<(u8, u32)>, Flow> {
    if base & 3 != 0 {
        return Err(fault(MemFault::Unaligned));
    }
    let mut out = Vec::with_capacity(regs.count_ones() as usize);
    let mut addr = base;
    for r in list(regs) {
        out.push((r, read(cpu, bus, addr, MemSize::Word).map_err(fault)?));
        addr = addr.wrapping_add(4);
    }
    Ok(out)
}

/// Write `regs` to consecutive words from `base`; returns the address after the last word.
fn store_multiple(cpu: &Cpu, bus: &mut impl Bus, base: u32, regs: u16) -> Result<u32, Flow> {
    if base & 3 != 0 {
        return Err(fault(MemFault::Unaligned));
    }
    let mut addr = base;
    for r in list(regs) {
        write(cpu, bus, addr, cpu.reg(r), MemSize::Word).map_err(fault)?;
        addr = addr.wrapping_add(4);
    }
    Ok(addr)
}

/// Commit loaded registers; the PC entry (if any) is a `LoadWritePC` branch.
fn commit_loads(cpu: &mut Cpu, vals: Vec<(u8, u32)>) -> Flow {
    let mut flow = Flow::Next;
    for (r, v) in vals {
        if r == 15 {
            flow = write_loaded(cpu, 15, v);
        } else {
            cpu.set_reg(r, v);
        }
    }
    flow
}

/// Execute a load/store; `None` if `instr` is not one.
pub(super) fn execute(cpu: &mut Cpu, bus: &mut impl Bus, instr: &Instr) -> Option<Flow> {
    let flow = match *instr {
        Instr::LoadImm {
            rt,
            rn,
            imm,
            size,
            signed,
            index,
            add,
            wback,
        } => {
            let base = cpu.reg(rn);
            let off = offset_addr(base, imm, add);
            let addr = if index { off } else { base };
            match read(cpu, bus, addr, size) {
                Ok(v) => {
                    if wback {
                        cpu.set_reg(rn, off);
                    }
                    write_loaded(cpu, rt, extend(v, size, signed))
                }
                Err(e) => fault(e),
            }
        }
        Instr::StoreImm {
            rt,
            rn,
            imm,
            size,
            index,
            add,
            wback,
        } => {
            let base = cpu.reg(rn);
            let off = offset_addr(base, imm, add);
            let addr = if index { off } else { base };
            match write(cpu, bus, addr, cpu.reg(rt), size) {
                Ok(()) => {
                    if wback {
                        cpu.set_reg(rn, off);
                    }
                    Flow::Next
                }
                Err(e) => fault(e),
            }
        }
        Instr::LoadReg {
            rt,
            rn,
            rm,
            shift,
            size,
            signed,
        } => {
            let addr = cpu.reg(rn).wrapping_add(cpu.reg(rm) << shift);
            match read(cpu, bus, addr, size) {
                Ok(v) => write_loaded(cpu, rt, extend(v, size, signed)),
                Err(e) => fault(e),
            }
        }
        Instr::StoreReg {
            rt,
            rn,
            rm,
            shift,
            size,
        } => {
            let addr = cpu.reg(rn).wrapping_add(cpu.reg(rm) << shift);
            match write(cpu, bus, addr, cpu.reg(rt), size) {
                Ok(()) => Flow::Next,
                Err(e) => fault(e),
            }
        }
        Instr::LdrLit { rt, imm, add } => {
            let addr = offset_addr(cpu.reg(15) & !3, imm, add);
            match read(cpu, bus, addr, MemSize::Word) {
                Ok(v) => write_loaded(cpu, rt, v),
                Err(e) => fault(e),
            }
        }
        Instr::Push { regs } => {
            let n = regs.count_ones();
            let start = cpu.reg(13).wrapping_sub(4 * n);
            match store_multiple(cpu, bus, start, regs) {
                Ok(_) => {
                    cpu.set_reg(13, start);
                    cpu.extra_cycles = n;
                    Flow::Next
                }
                Err(f) => f,
            }
        }
        Instr::Pop { regs } => {
            let n = regs.count_ones();
            let sp = cpu.reg(13);
            match load_multiple(cpu, bus, sp, regs) {
                Ok(vals) => {
                    cpu.set_reg(13, sp.wrapping_add(4 * n));
                    // The taken-branch cycle of POP {.., pc} is part of its N-register cost.
                    cpu.extra_cycles = if regs & 0x8000 != 0 { n - 1 } else { n };
                    commit_loads(cpu, vals)
                }
                Err(f) => f,
            }
        }
        Instr::Ldm { rn, regs, wback } => {
            let base = cpu.reg(rn);
            match load_multiple(cpu, bus, base, regs) {
                Ok(vals) => {
                    if wback {
                        cpu.set_reg(rn, base.wrapping_add(4 * regs.count_ones()));
                    }
                    cpu.extra_cycles = regs.count_ones();
                    commit_loads(cpu, vals)
                }
                Err(f) => f,
            }
        }
        Instr::Stm { rn, regs, wback } => {
            let base = cpu.reg(rn);
            match store_multiple(cpu, bus, base, regs) {
                Ok(end) => {
                    if wback {
                        cpu.set_reg(rn, end);
                    }
                    cpu.extra_cycles = regs.count_ones();
                    Flow::Next
                }
                Err(f) => f,
            }
        }
        _ => return None,
    };
    Some(flow)
}
