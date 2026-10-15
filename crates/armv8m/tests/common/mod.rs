#![allow(dead_code)]

use armv8m::{Access, Bus, BusError, Cpu, StepResult};

pub const CODE_BASE: u32 = 0x0000_0000;
pub const RAM_BASE: u32 = 0x2000_0000;
const SIZE: u32 = 0x1_0000;

/// Test bus: 64 KiB of code at 0x0 and 64 KiB of RAM at 0x2000_0000.
pub struct FlatBus {
    pub code: Vec<u8>,
    pub ram: Vec<u8>,
    /// Let the core cache decoded code (`Bus::cache_code`): every bus write starts a new code
    /// epoch. Direct changes to `code` are not seen, so only tests that never patch code
    /// between steps turn this on.
    pub cache: bool,
    epoch: u64,
}

impl FlatBus {
    pub fn new() -> Self {
        Self {
            code: vec![0; SIZE as usize],
            ram: vec![0; SIZE as usize],
            cache: false,
            epoch: 1,
        }
    }

    fn slot(&mut self, addr: u32) -> Result<&mut u8, BusError> {
        let (mem, base) = match addr {
            a if a < SIZE => (&mut self.code, CODE_BASE),
            a if (RAM_BASE..RAM_BASE + SIZE).contains(&a) => (&mut self.ram, RAM_BASE),
            _ => return Err(BusError::Decode),
        };
        Ok(&mut mem[(addr - base) as usize])
    }

    fn read(&mut self, addr: u32, n: u32) -> Result<u32, BusError> {
        let mut v = 0;
        for i in 0..n {
            v |= u32::from(*self.slot(addr.wrapping_add(i))?) << (8 * i);
        }
        Ok(v)
    }

    fn write(&mut self, addr: u32, v: u32, n: u32) -> Result<(), BusError> {
        self.epoch += 1;
        for i in 0..n {
            *self.slot(addr.wrapping_add(i))? = (v >> (8 * i)) as u8;
        }
        Ok(())
    }
}

impl Default for FlatBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for FlatBus {
    fn read8(&mut self, addr: u32, _a: Access) -> Result<u8, BusError> {
        self.read(addr, 1).map(|v| v as u8)
    }
    fn read16(&mut self, addr: u32, _a: Access) -> Result<u16, BusError> {
        self.read(addr, 2).map(|v| v as u16)
    }
    fn read32(&mut self, addr: u32, _a: Access) -> Result<u32, BusError> {
        self.read(addr, 4)
    }
    fn write8(&mut self, addr: u32, v: u8, _a: Access) -> Result<(), BusError> {
        self.write(addr, u32::from(v), 1)
    }
    fn write16(&mut self, addr: u32, v: u16, _a: Access) -> Result<(), BusError> {
        self.write(addr, u32::from(v), 2)
    }
    fn write32(&mut self, addr: u32, v: u32, _a: Access) -> Result<(), BusError> {
        self.write(addr, v, 4)
    }
    fn code_epoch(&self) -> u64 {
        self.epoch
    }
    fn cache_code(&mut self, addr: u32, len: u32) -> bool {
        self.cache && addr.checked_add(len).is_some_and(|end| end <= SIZE)
    }
}

/// A CPU (Secure, Thumb) at PC=0x100 with SP=0x2000_1000 and `code` placed there.
pub fn cpu_with_code(code: &[u16]) -> (Cpu, FlatBus) {
    let mut bus = FlatBus::new();
    for (i, hw) in code.iter().enumerate() {
        let a = 0x100 + 2 * i;
        bus.code[a..a + 2].copy_from_slice(&hw.to_le_bytes());
    }
    let mut cpu = Cpu::new();
    cpu.regs.pc = 0x100;
    cpu.regs.msp.s = 0x2000_1000;
    (cpu, bus)
}

pub fn run(cpu: &mut Cpu, bus: &mut FlatBus, n: usize) {
    for _ in 0..n {
        cpu.step(bus);
    }
}

/// Step once and require that the instruction at PC faulted into HardFault (the configurable
/// fault handlers are disabled at reset, so every fault escalates). Returns CFSR.
pub fn step_hardfault(cpu: &mut Cpu, bus: &mut FlatBus) -> u32 {
    let pc = cpu.regs.pc;
    let r = cpu.step(bus);
    assert!(matches!(r, StepResult::Executed { .. }), "{r:?}");
    assert_eq!(cpu.regs.xpsr.exception, 3, "expected HardFault");
    let sp = (cpu.regs.sp() - RAM_BASE) as usize + 0x18;
    let stacked_pc = u32::from_le_bytes(bus.ram[sp..sp + 4].try_into().unwrap());
    assert_eq!(stacked_pc, pc, "stacked PC is the faulting instruction");
    cpu.ppb_read32(0xE000_ED28)
}
