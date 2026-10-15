//! System Control Space, exception entry/return, priorities and faults (single security state).

mod common;

use armv8m::scs::PpbMiss;
use armv8m::{Access, Bus, BusError, Cpu, Sec, SleepKind, StepResult};
use common::{FlatBus, RAM_BASE, cpu_with_code};

// SCS register addresses.
const ICTR: u32 = 0xE000_E004;
const SYST_CSR: u32 = 0xE000_E010;
const SYST_RVR: u32 = 0xE000_E014;
const SYST_CVR: u32 = 0xE000_E018;
const NVIC_ISER0: u32 = 0xE000_E100;
const NVIC_ISPR0: u32 = 0xE000_E200;
const NVIC_IABR0: u32 = 0xE000_E300;
const NVIC_IPR0: u32 = 0xE000_E400;
const CPUID: u32 = 0xE000_ED00;
const ICSR: u32 = 0xE000_ED04;
const VTOR: u32 = 0xE000_ED08;
const AIRCR: u32 = 0xE000_ED0C;
const CCR: u32 = 0xE000_ED14;
const SHPR1: u32 = 0xE000_ED18;
const SHPR2: u32 = 0xE000_ED1C;
const SHCSR: u32 = 0xE000_ED24;
const CFSR: u32 = 0xE000_ED28;
const HFSR: u32 = 0xE000_ED2C;
const BFAR: u32 = 0xE000_ED38;
const STIR: u32 = 0xE000_EF00;

// CFSR bits.
const IACCVIOL: u32 = 1 << 0;
const IBUSERR: u32 = 1 << 8;
const PRECISERR: u32 = 1 << 9;
const UNSTKERR: u32 = 1 << 11;
const STKERR: u32 = 1 << 12;
const BFARVALID: u32 = 1 << 15;
const UNDEFINSTR: u32 = 1 << 16;
const INVSTATE: u32 = 1 << 17;
const INVPC: u32 = 1 << 18;
const NOCP: u32 = 1 << 19;
const STKOF: u32 = 1 << 20;
const UNALIGNED: u32 = 1 << 24;
const DIVBYZERO: u32 = 1 << 25;
const FORCED: u32 = 1 << 30;

// SHCSR enables.
const MEMFAULTENA: u32 = 1 << 16;
const BUSFAULTENA: u32 = 1 << 17;
const USGFAULTENA: u32 = 1 << 18;

const SP0: u32 = 0x2000_1000;

fn code(bus: &mut FlatBus, addr: u32, hws: &[u16]) {
    for (i, hw) in hws.iter().enumerate() {
        let a = addr as usize + 2 * i;
        bus.code[a..a + 2].copy_from_slice(&hw.to_le_bytes());
    }
}

/// Point vector `n` (in the table at 0) at a Thumb handler at `addr` containing `hws`.
fn handler(bus: &mut FlatBus, n: u32, addr: u32, hws: &[u16]) {
    let v = (4 * n) as usize;
    bus.code[v..v + 4].copy_from_slice(&(addr | 1).to_le_bytes());
    code(bus, addr, hws);
}

fn ram(bus: &FlatBus, addr: u32) -> u32 {
    let o = (addr - RAM_BASE) as usize;
    u32::from_le_bytes(bus.ram[o..o + 4].try_into().unwrap())
}

/// The 8-word basic frame at the current SP: r0, r1, r2, r3, r12, lr, return address, xPSR.
fn frame(cpu: &Cpu, bus: &FlatBus) -> [u32; 8] {
    let sp = cpu.regs.sp();
    std::array::from_fn(|i| ram(bus, sp + 4 * i as u32))
}

fn ipsr(cpu: &Cpu) -> u16 {
    cpu.regs.xpsr.exception
}

fn executed(r: StepResult) {
    assert!(matches!(r, StepResult::Executed { .. }), "{r:?}");
}

const BX_LR: u16 = 0x4770;
const B_SELF: u16 = 0xE7FE;
const NOP: u16 = 0xBF00;
const SVC0: u16 = 0xDF00;
const UDF0: u16 = 0xDE00;

// ---- Exception entry and return ----

#[test]
fn svc_enters_handler_and_returns() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    for i in 0..4 {
        cpu.regs.r[i] = 0x1111_1111 * (i as u32 + 1);
    }
    cpu.regs.r[12] = 0xCCCC_CCCC;
    cpu.regs.lr = 0x0000_0E01;
    cpu.regs.xpsr.n = true;
    cpu.regs.xpsr.c = true;
    cpu.regs.xpsr.ge = 0b1010;
    let before = cpu.regs.clone();

    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 11);
    assert_eq!(cpu.regs.pc, 0x200);
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF9);
    assert_eq!(cpu.regs.sp(), SP0 - 0x20);
    let xpsr = before.xpsr.to_u32();
    assert_eq!(
        frame(&cpu, &bus),
        [
            0x1111_1111,
            0x2222_2222,
            0x3333_3333,
            0x4444_4444,
            0xCCCC_CCCC,
            0x0000_0E01,
            0x102,
            xpsr
        ]
    );
    assert_eq!(cpu.ppb_read32(SHCSR) & (1 << 7), 1 << 7, "SVCALLACT");

    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);
    assert!(!cpu.regs.handler_mode());
    assert_eq!(cpu.regs.pc, 0x102);
    assert_eq!(cpu.regs.sp(), SP0);
    assert_eq!(cpu.regs.r[..4], before.r[..4]);
    assert_eq!(cpu.regs.r[12], before.r[12]);
    assert_eq!(cpu.regs.lr, before.lr);
    assert_eq!(cpu.regs.xpsr, before.xpsr);
    assert_eq!(cpu.ppb_read32(SHCSR) & (1 << 7), 0);
}

#[test]
fn stack_alignment_bit9() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    cpu.regs.msp.s = SP0 - 4;
    executed(cpu.step(&mut bus));
    // (0x2000_0FFC - 0x20) & !7
    assert_eq!(cpu.regs.sp(), 0x2000_0FD8);
    assert_ne!(frame(&cpu, &bus)[7] & (1 << 9), 0);
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.regs.sp(), SP0 - 4);

    // An 8-byte aligned SP leaves bit 9 clear.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    executed(cpu.step(&mut bus));
    assert_eq!(frame(&cpu, &bus)[7] & (1 << 9), 0);
}

#[test]
fn psp_thread_entry_uses_psp_and_returns_to_it() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    cpu.regs.control.s = 0b10;
    cpu.regs.psp.s = 0x2000_2000;
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFFD);
    assert_eq!(cpu.regs.psp.s, 0x2000_1FE0);
    assert_eq!(cpu.regs.msp.s, SP0);
    assert_eq!(cpu.regs.sp(), SP0, "handler runs on MSP");
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.regs.control.s & 0b10, 0b10);
    assert_eq!(cpu.regs.psp.s, 0x2000_2000);
    assert_eq!(cpu.regs.pc, 0x102);
}

#[test]
fn exception_entry_clears_it_state_and_exclusive_monitor() {
    // ldrex r0, [r1] ; svc 0
    let (mut cpu, mut bus) = cpu_with_code(&[0xE851, 0x0F00, SVC0]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    cpu.regs.r[1] = RAM_BASE;
    executed(cpu.step(&mut bus));
    assert!(cpu.exclusive.is_some());
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 11);
    assert_eq!(cpu.exclusive, None);
}

#[test]
fn pop_pc_and_ldr_pc_perform_exception_return() {
    // Handler: push {lr} ; pop {pc}
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[0xB500, 0xBD00]);
    for _ in 0..3 {
        executed(cpu.step(&mut bus));
    }
    assert_eq!((ipsr(&cpu), cpu.regs.pc, cpu.regs.sp()), (0, 0x102, SP0));

    // Handler: mov r1, lr ; str r1, [r0] ; ldr.w pc, [r0]
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[0x4671, 0x6001, 0xF8D0, 0xF000]);
    cpu.regs.r[0] = RAM_BASE + 0x800;
    for _ in 0..4 {
        executed(cpu.step(&mut bus));
    }
    assert_eq!((ipsr(&cpu), cpu.regs.pc, cpu.regs.sp()), (0, 0x102, SP0));
}

// ---- Interrupts, priorities and masking ----

fn enable_irqs(cpu: &mut Cpu, mask: u32) {
    cpu.ppb_write32(NVIC_ISER0, mask);
}

/// Set an IRQ's priority byte through a word write to its IPR register.
fn irq_priority(cpu: &mut Cpu, irq: u32, pri: u8) {
    let addr = NVIC_IPR0 + (irq & !3);
    let shift = 8 * (irq & 3);
    let v = cpu.ppb_read32(addr) & !(0xFF << shift);
    cpu.ppb_write32(addr, v | (u32::from(pri) << shift));
}

fn run2(cpu: &mut Cpu, bus: &mut FlatBus) {
    executed(cpu.step(bus));
    executed(cpu.step(bus));
}

#[test]
fn irq_preempts_by_priority() {
    let (mut cpu, mut bus) = cpu_with_code(&[B_SELF]);
    handler(&mut bus, 16 + 3, 0x200, &[B_SELF]);
    handler(&mut bus, 16 + 5, 0x300, &[BX_LR]);
    handler(&mut bus, 16 + 4, 0x400, &[BX_LR]);
    irq_priority(&mut cpu, 3, 0x80);
    irq_priority(&mut cpu, 4, 0x80);
    irq_priority(&mut cpu, 5, 0x40);
    enable_irqs(&mut cpu, 0b11_1000);

    cpu.pulse_irq(3);
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (19, 0x200));
    assert_eq!(cpu.ppb_read32(ICSR) & 0x1FF, 19, "VECTACTIVE");
    assert_ne!(cpu.ppb_read32(ICSR) & (1 << 11), 0, "RETTOBASE");
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 19);

    // Same priority: stays pending.
    cpu.pulse_irq(4);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 19);
    assert_eq!((cpu.ppb_read32(ICSR) >> 12) & 0x1FF, 20, "VECTPENDING");
    assert_ne!(cpu.ppb_read32(ICSR) & (1 << 22), 0, "ISRPENDING");

    // Higher priority preempts.
    cpu.pulse_irq(5);
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (21, 0x300));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF1);
    assert_eq!(cpu.ppb_read32(NVIC_IABR0), (1 << 3) | (1 << 5));
    assert_eq!(
        cpu.ppb_read32(ICSR) & (1 << 11),
        0,
        "RETTOBASE with a nested activation"
    );

    // Returning from IRQ 5 resumes IRQ 3 (IRQ 4 still cannot preempt it).
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (19, 0x200));
    assert_eq!(cpu.ppb_read32(NVIC_IABR0), 1 << 3);
    assert_eq!(cpu.ppb_read32(NVIC_ISPR0), 1 << 4);
}

/// Delegates to a `FlatBus`, counting data writes and RAM word reads.
struct CountingBus {
    inner: FlatBus,
    writes: usize,
    reads: usize,
}

impl Bus for CountingBus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        self.reads += 1;
        self.inner.read8(addr, a)
    }
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        self.inner.read16(addr, a)
    }
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        if addr >= RAM_BASE {
            self.reads += 1;
        }
        self.inner.read32(addr, a)
    }
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        self.writes += 1;
        self.inner.write8(addr, v, a)
    }
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        self.writes += 1;
        self.inner.write16(addr, v, a)
    }
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        self.writes += 1;
        self.inner.write32(addr, v, a)
    }
}

#[test]
fn tail_chain() {
    let (mut cpu, mut inner) = cpu_with_code(&[B_SELF]);
    handler(&mut inner, 16 + 3, 0x200, &[BX_LR]);
    handler(&mut inner, 16 + 5, 0x300, &[BX_LR]);
    let mut bus = CountingBus {
        inner,
        writes: 0,
        reads: 0,
    };
    enable_irqs(&mut cpu, (1 << 3) | (1 << 5));
    cpu.pulse_irq(3);
    cpu.pulse_irq(5);

    // Equal priority: the lower number goes first.
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 19);
    assert_eq!(bus.writes, 8);
    // Return from IRQ 3 tail-chains into IRQ 5: no unstack, no restack.
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (21, 0x300));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF9);
    assert_eq!(cpu.regs.sp(), SP0 - 0x20);
    assert_eq!(bus.writes, 8);
    assert_eq!(bus.reads, 0, "no frame was popped");
    // Final return unstacks once.
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc, cpu.regs.sp()), (0, 0x100, SP0));
    assert_eq!(bus.writes, 8, "one frame written instead of two");
    assert_eq!(bus.reads, 8);
}

#[test]
fn primask_masks_irq() {
    // cpsid i ; nop ; cpsie i ; b .
    let (mut cpu, mut bus) = cpu_with_code(&[0xB672, NOP, 0xB662, B_SELF]);
    handler(&mut bus, 16, 0x200, &[B_SELF]);
    enable_irqs(&mut cpu, 1);
    executed(cpu.step(&mut bus));
    cpu.pulse_irq(0);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);
    assert_eq!(cpu.regs.pc, 0x104);
    assert_ne!(cpu.ppb_read32(ICSR) & (1 << 22), 0, "ISRPENDING");
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 16);
    assert_eq!(frame(&cpu, &bus)[6], 0x106, "return address after cpsie");
}

#[test]
fn disabled_irq_stays_pending() {
    let (mut cpu, mut bus) = cpu_with_code(&[NOP, NOP, B_SELF]);
    handler(&mut bus, 16 + 7, 0x200, &[B_SELF]);
    cpu.pulse_irq(7);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);
    assert_eq!(cpu.ppb_read32(NVIC_ISPR0), 1 << 7);
    enable_irqs(&mut cpu, 1 << 7);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 23);
}

#[test]
fn basepri_masks_equal_and_lower_priority() {
    let (mut cpu, mut bus) = cpu_with_code(&[NOP, NOP, NOP, B_SELF]);
    handler(&mut bus, 16 + 1, 0x200, &[B_SELF]);
    handler(&mut bus, 16 + 2, 0x300, &[B_SELF]);
    irq_priority(&mut cpu, 1, 0x40);
    irq_priority(&mut cpu, 2, 0x20);
    enable_irqs(&mut cpu, 0b110);
    cpu.regs.basepri.s = 0x40;
    cpu.pulse_irq(1);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);
    cpu.pulse_irq(2);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 18);
}

#[test]
fn prigroup_subpriority_does_not_preempt() {
    let (mut cpu, mut bus) = cpu_with_code(&[B_SELF]);
    handler(&mut bus, 16 + 1, 0x200, &[B_SELF]);
    handler(&mut bus, 16 + 2, 0x300, &[B_SELF]);
    // PRIGROUP = 6: the group priority is bit 7 only.
    cpu.ppb_write32(AIRCR, 0x05FA_0600);
    irq_priority(&mut cpu, 1, 0x60);
    irq_priority(&mut cpu, 2, 0x20);
    enable_irqs(&mut cpu, 0b110);
    cpu.pulse_irq(1);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 17);
    cpu.pulse_irq(2);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 17, "same group priority");
}

#[test]
fn nmi_ignores_primask_and_pendsv_set_by_icsr() {
    let (mut cpu, mut bus) = cpu_with_code(&[NOP, NOP, B_SELF]);
    handler(&mut bus, 2, 0x200, &[BX_LR]);
    handler(&mut bus, 14, 0x300, &[BX_LR]);
    cpu.regs.primask.s = true;
    cpu.ppb_write32(ICSR, 1 << 31);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 2);
    assert_ne!(cpu.ppb_read32(SHCSR) & (1 << 5), 0, "NMIACT");
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);

    cpu.regs.primask.s = false;
    cpu.ppb_write32(ICSR, 1 << 28);
    assert_ne!(cpu.ppb_read32(ICSR) & (1 << 28), 0, "PENDSVSET reads back");
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 14);
    assert_eq!(cpu.ppb_read32(ICSR) & (1 << 28), 0);
}

#[test]
fn level_irq_repends_while_asserted() {
    let (mut cpu, mut bus) = cpu_with_code(&[B_SELF]);
    handler(&mut bus, 16 + 2, 0x200, &[BX_LR]);
    enable_irqs(&mut cpu, 1 << 2);
    cpu.set_irq_level(2, true);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 18);
    assert_eq!(cpu.ppb_read32(NVIC_ISPR0), 0, "activation clears pending");
    // Still asserted at return: pends again and tail-chains.
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (18, 0x200));
    cpu.set_irq_level(2, false);
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (0, 0x100));
}

#[test]
fn stir_and_out_of_range_irqs() {
    let (mut cpu, mut bus) = cpu_with_code(&[B_SELF]);
    handler(&mut bus, 16 + 40, 0x200, &[B_SELF]);
    cpu.pulse_irq(480);
    cpu.set_irq_level(1000, true);
    cpu.ppb_write32(NVIC_ISER0 + 4, 1 << 8);
    cpu.ppb_write32(STIR, 40);
    assert_eq!(cpu.ppb_read32(NVIC_ISPR0 + 4), 1 << 8);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 56);
}

// ---- SysTick ----

#[test]
fn systick_fires_after_reload() {
    let (mut cpu, mut bus) = cpu_with_code(&[B_SELF]);
    handler(&mut bus, 15, 0x200, &[B_SELF]);
    cpu.ppb_write32(SYST_RVR, 99);
    cpu.ppb_write32(SYST_CVR, 1234);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 0, "any write clears the counter");
    cpu.ppb_write32(SYST_CSR, 0b111);
    // `b .` costs 2 cycles: one cycle reloads 99, 99 more reach zero.
    for _ in 0..49 {
        executed(cpu.step(&mut bus));
        assert_eq!(ipsr(&cpu), 0);
    }
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 15);
    assert_eq!(cpu.regs.pc, 0x200);
    let csr = cpu.ppb_read32(SYST_CSR);
    assert_ne!(csr & (1 << 16), 0, "COUNTFLAG");
    assert_eq!(
        cpu.ppb_read32(SYST_CSR) & (1 << 16),
        0,
        "COUNTFLAG clears on read"
    );
}

#[test]
fn systick_tick_counts_and_reloads() {
    let (mut cpu, _bus) = cpu_with_code(&[B_SELF]);
    cpu.ppb_write32(SYST_RVR, 99);
    // TICKINT clear: COUNTFLAG only.
    cpu.ppb_write32(SYST_CSR, 0b101);
    cpu.tick_systick(1);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 99);
    cpu.tick_systick(98);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 1);
    cpu.tick_systick(1);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 0);
    assert_eq!(
        cpu.ppb_read32(ICSR) & (1 << 26),
        0,
        "no TICKINT, not pended"
    );
    assert_ne!(cpu.ppb_read32(SYST_CSR) & (1 << 16), 0);
    cpu.ppb_write32(SYST_CSR, 0b111);
    // 0 -> reload (1) -> 99 to 0 -> reload (1) -> 99 to 0 -> reload (1) -> 49 more
    cpu.tick_systick(250);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 50);
    assert_ne!(cpu.ppb_read32(ICSR) & (1 << 26), 0, "PENDSTSET");
    // Disabled: no counting.
    cpu.ppb_write32(SYST_CSR, 0);
    cpu.tick_systick(10);
    assert_eq!(cpu.ppb_read32(SYST_CVR), 50);
}

/// A debugger reads the PPB with `ppb_peek32`, which must not disturb the guest: reading
/// `SYST_CSR` that way leaves COUNTFLAG set, and nothing is logged for unimplemented space.
#[test]
fn ppb_peek32_has_no_side_effects() {
    const COUNTFLAG: u32 = 1 << 16;
    let (mut cpu, _bus) = cpu_with_code(&[B_SELF]);
    cpu.ppb_write32(SYST_RVR, 9);
    cpu.ppb_write32(SYST_CSR, 0b101);
    cpu.tick_systick(11); // reload, 9 to zero (COUNTFLAG), reload
    assert_eq!(
        cpu.ppb_peek32(SYST_CSR, Sec::Secure),
        Some(0b101 | COUNTFLAG)
    );
    assert_eq!(
        cpu.ppb_peek32(SYST_CSR, Sec::Secure),
        Some(0b101 | COUNTFLAG),
        "a peek leaves COUNTFLAG set"
    );
    assert_eq!(cpu.ppb_read32(SYST_CSR), 0b101 | COUNTFLAG);
    assert_eq!(
        cpu.ppb_peek32(SYST_CSR, Sec::Secure),
        Some(0b101),
        "a read clears it"
    );

    // Otherwise a peek sees what a read sees, in the given security state's view.
    assert_eq!(
        cpu.ppb_peek32(CPUID, Sec::Secure),
        Some(cpu.ppb_read32(CPUID))
    );
    assert_eq!(cpu.ppb_peek32(SYST_RVR, Sec::Secure), Some(9));
    assert_eq!(
        cpu.ppb_peek32(SYST_RVR + 3, Sec::Secure),
        Some(9),
        "containing word"
    );
    assert_eq!(
        cpu.ppb_peek32(SYST_RVR, Sec::NonSecure),
        Some(0),
        "the NS SysTick"
    );
    assert_eq!(
        cpu.ppb_peek32(SYST_RVR + 0x2_0000, Sec::Secure),
        Some(0),
        "the Non-secure alias shows the NS view"
    );
    cpu.ppb_write32(SYST_RVR + 0x2_0000, 7);
    assert_eq!(cpu.ppb_peek32(SYST_RVR, Sec::NonSecure), Some(7));

    // No register there, or outside the PPB.
    assert_eq!(cpu.ppb_peek32(0xE000_1000, Sec::Secure), None);
    assert_eq!(cpu.ppb_peek32(0xE00F_FFFC, Sec::Secure), None);
    assert_eq!(cpu.ppb_peek32(0x2000_0000, Sec::Secure), None);
    assert_eq!(cpu.ppb_peek32(0xE010_0000, Sec::Secure), None);
}

const PENDSTSET: u32 = 1 << 26;

#[test]
fn cycles_to_systick_is_the_tick_that_pends() {
    let (mut cpu, _bus) = cpu_with_code(&[B_SELF]);
    assert_eq!(cpu.cycles_to_systick(), None, "disabled");
    cpu.ppb_write32(SYST_RVR, 99);
    cpu.ppb_write32(SYST_CSR, 0b101);
    assert_eq!(cpu.cycles_to_systick(), None, "counting, but TICKINT clear");

    cpu.ppb_write32(SYST_CSR, 0b111);
    // At zero: one reload cycle, then 99 down to zero.
    assert_eq!(cpu.cycles_to_systick(), Some(100));
    cpu.tick_systick(99);
    assert_eq!(cpu.cycles_to_systick(), Some(1));
    assert_eq!(cpu.ppb_read32(ICSR) & PENDSTSET, 0);
    cpu.tick_systick(1);
    assert_ne!(cpu.ppb_read32(ICSR) & PENDSTSET, 0);
    assert_eq!(cpu.cycles_to_systick(), Some(100), "at zero again");
    cpu.tick_systick(30);
    assert_eq!(cpu.cycles_to_systick(), Some(70));

    // The Non-secure counter (through the NS alias) is due first.
    cpu.ppb_write32(0xE002_E014, 9);
    cpu.ppb_write32(0xE002_E010, 0b111);
    assert_eq!(cpu.cycles_to_systick(), Some(10));

    // RVR = 0 stops a counter at zero: no further interrupt from it.
    cpu.ppb_write32(0xE002_E010, 0);
    cpu.ppb_write32(SYST_RVR, 0);
    cpu.tick_systick(70);
    assert_eq!(cpu.cycles_to_systick(), None);
}

#[test]
fn exception_pending_ignores_priority_but_not_enable() {
    // cpsid i ; b .
    let (mut cpu, mut bus) = cpu_with_code(&[0xB672, B_SELF]);
    handler(&mut bus, 16 + 3, 0x200, &[B_SELF]);
    assert!(!cpu.exception_pending());
    cpu.pulse_irq(3);
    assert!(!cpu.exception_pending(), "a disabled IRQ is not counted");
    executed(cpu.step(&mut bus));
    enable_irqs(&mut cpu, 1 << 3);
    assert!(
        cpu.exception_pending(),
        "counted while PRIMASK keeps it from being taken"
    );
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0, "still masked");
    // Taking it clears the pending state.
    cpu.regs.primask.s = false;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 19);
    assert!(!cpu.exception_pending());
    // System exceptions count too.
    cpu.ppb_write32(ICSR, 1 << 28); // PENDSVSET
    assert!(cpu.exception_pending());
}

#[test]
fn sleep_on_exit_reports_its_kind() {
    const SCR: u32 = 0xE000_ED10;
    const SLEEPONEXIT: u32 = 1 << 1;
    // svc #0 ; b .   with an SVCall handler that just returns.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, B_SELF]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    cpu.ppb_write32(SCR, SLEEPONEXIT);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 11);
    assert_eq!(cpu.step(&mut bus), StepResult::Sleep(SleepKind::OnExit));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (0, 0x102), "back in Thread mode");
}

// ---- Faults ----

#[test]
fn undefined_escalates_to_hardfault() {
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 3, 0x200, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
    assert_eq!(cpu.ppb_read32(CFSR), UNDEFINSTR);
    assert_eq!(
        frame(&cpu, &bus)[6],
        0x100,
        "stacked PC is the faulting instruction"
    );
    // HFSR and CFSR are write-one-to-clear.
    cpu.ppb_write32(HFSR, FORCED);
    cpu.ppb_write32(CFSR, UNDEFINSTR);
    assert_eq!((cpu.ppb_read32(HFSR), cpu.ppb_read32(CFSR)), (0, 0));

    // With UsageFault enabled it is taken directly.
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 6, 0x200, &[B_SELF]);
    cpu.ppb_write32(SHCSR, USGFAULTENA);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 6);
    assert_eq!(cpu.ppb_read32(HFSR), 0);
    assert_eq!(cpu.ppb_read32(CFSR), UNDEFINSTR);
}

#[test]
fn usage_fault_of_insufficient_priority_escalates() {
    // UsageFault enabled but at priority 0x80 while executing an SVCall handler at 0.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0]);
    handler(&mut bus, 11, 0x200, &[UDF0]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, USGFAULTENA);
    cpu.ppb_write32(SHPR1, 0x80 << 16);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 11);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
}

#[test]
fn svc_with_insufficient_priority_escalates() {
    // cpsid i ; svc 0
    let (mut cpu, mut bus) = cpu_with_code(&[0xB672, SVC0]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHPR2, 0x40 << 24);
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
    assert_eq!(frame(&cpu, &bus)[6], 0x104);
}

#[test]
fn branch_to_even_address_invstate() {
    // bx r0 to an even address clears T; the next fetch faults.
    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    code(&mut bus, 0x200, &[NOP]);
    handler(&mut bus, 6, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, USGFAULTENA);
    cpu.regs.r[0] = 0x200;
    executed(cpu.step(&mut bus));
    assert_eq!(
        (ipsr(&cpu), cpu.regs.pc, cpu.regs.xpsr.t),
        (0, 0x200, false)
    );
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 6);
    assert_eq!(cpu.ppb_read32(CFSR), INVSTATE);
    let f = frame(&cpu, &bus);
    assert_eq!(f[6], 0x200);
    assert_eq!(f[7] & (1 << 24), 0, "stacked T bit is clear");
    assert!(cpu.regs.xpsr.t, "handler runs in Thumb state");

    // pop {pc} with an even value: same.
    let (mut cpu, mut bus) = cpu_with_code(&[0xBD00]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.msp.s = SP0 - 4;
    bus.ram[0xFFC..0x1000].copy_from_slice(&0x200u32.to_le_bytes());
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), INVSTATE);
}

#[test]
fn fetch_from_unmapped_ibuserr() {
    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.r[0] = 0x0002_0001;
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(
        cpu.ppb_read32(CFSR),
        IBUSERR,
        "BFAR is not valid for IBUSERR"
    );
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
    assert_eq!(frame(&cpu, &bus)[6], 0x0002_0000);

    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    handler(&mut bus, 5, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, BUSFAULTENA);
    cpu.regs.r[0] = 0x0002_0001;
    run2(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (5, 0x300));

    // A 32-bit instruction whose second halfword is unmapped also faults at its start.
    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    code(&mut bus, 0xFFFE, &[0xF8D0]);
    cpu.regs.r[0] = 0xFFFF;
    run2(&mut cpu, &mut bus);
    assert_eq!(cpu.ppb_read32(CFSR), IBUSERR);
    assert_eq!(frame(&cpu, &bus)[6], 0xFFFE);
}

#[test]
fn fetch_from_execute_never_region_is_memmanage() {
    // The system region (0xE000_0000..) is Execute Never in the default memory map.
    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    handler(&mut bus, 4, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, MEMFAULTENA);
    cpu.regs.r[0] = 0xE000_0001;
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 4);
    assert_eq!(cpu.ppb_read32(CFSR), IACCVIOL);
    assert_eq!(frame(&cpu, &bus)[6], 0xE000_0000);
}

#[test]
fn vldr_raises_nocp() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xED90, 0x0A00]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), NOCP);
    assert_eq!(frame(&cpu, &bus)[6], 0x100);
}

#[test]
fn fault_in_hardfault_locks_up() {
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 3, 0x200, &[UDF0]);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert!(!cpu.locked_up());
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    assert!(cpu.locked_up());
    assert_eq!(cpu.regs.pc, 0xEFFF_FFFE);
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    // Reset leaves lockup.
    cpu.reset(&mut bus);
    assert!(!cpu.locked_up());
}

#[test]
fn nmi_exits_lockup_at_priority_minus_one() {
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 3, 0x200, &[UDF0]);
    handler(&mut bus, 2, 0x300, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    cpu.ppb_write32(ICSR, 1 << 31);
    executed(cpu.step(&mut bus));
    assert!(!cpu.locked_up());
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (2, 0x300));
}

#[test]
fn fault_with_faultmask_locks_up() {
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    cpu.regs.faultmask.s = true;
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
}

#[test]
fn precise_busfault_sets_bfar() {
    // str r1, [r0]
    let (mut cpu, mut bus) = cpu_with_code(&[0x6001]);
    handler(&mut bus, 5, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, BUSFAULTENA);
    cpu.regs.r[0] = 0x4000_0000;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 5);
    assert_eq!(cpu.ppb_read32(CFSR), PRECISERR | BFARVALID);
    assert_eq!(cpu.ppb_read32(BFAR), 0x4000_0000);
    assert_eq!(frame(&cpu, &bus)[6], 0x100);
}

#[test]
fn faulting_ldm_and_pop_are_precise() {
    // ldm.w r0!, {r1, r2} with the second word unmapped
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8B0, 0x0006]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.r[0] = RAM_BASE + 0xFFFC;
    cpu.regs.r[1] = 0x1111;
    cpu.regs.r[2] = 0x2222;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), PRECISERR | BFARVALID);
    assert_eq!(cpu.ppb_read32(BFAR), RAM_BASE + 0x1_0000);
    assert_eq!(
        (cpu.regs.r[0], cpu.regs.r[1], cpu.regs.r[2]),
        (RAM_BASE + 0xFFFC, 0x1111, 0x2222)
    );
    let f = frame(&cpu, &bus);
    assert_eq!(
        (f[0], f[1], f[2], f[6]),
        (RAM_BASE + 0xFFFC, 0x1111, 0x2222, 0x100)
    );

    // pop {r0, r1} with SP at the last RAM word.
    let (mut cpu, mut bus) = cpu_with_code(&[0xBC03]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.msp.s = RAM_BASE + 0xFFFC;
    cpu.regs.r[0] = 0xAAAA;
    cpu.regs.r[1] = 0xBBBB;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    let f = frame(&cpu, &bus);
    assert_eq!((f[0], f[1], f[6]), (0xAAAA, 0xBBBB, 0x100));
    // The frame sits below the unchanged SP (realigned to 8 bytes).
    assert_eq!(cpu.regs.sp(), (RAM_BASE + 0xFFFC - 0x20) & !7);
    assert_ne!(f[7] & (1 << 9), 0);
}

#[test]
fn unaligned_traps() {
    // ldr r1, [r0] unaligned with CCR.UNALIGN_TRP
    let (mut cpu, mut bus) = cpu_with_code(&[0x6801]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    let ccr = cpu.ppb_read32(CCR);
    cpu.ppb_write32(CCR, ccr | (1 << 3));
    cpu.regs.r[0] = RAM_BASE + 1;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), UNALIGNED);

    // Without the trap it succeeds.
    let (mut cpu, mut bus) = cpu_with_code(&[0x6801]);
    cpu.regs.r[0] = RAM_BASE + 1;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);

    // ldm.w r0, {r1, r2}: always traps when unaligned.
    let (mut cpu, mut bus) = cpu_with_code(&[0xE890, 0x0006]);
    cpu.regs.r[0] = RAM_BASE + 2;
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.ppb_read32(CFSR), UNALIGNED);
}

#[test]
fn divide_by_zero_traps_when_enabled() {
    // sdiv r0, r1, r2
    let (mut cpu, mut bus) = cpu_with_code(&[0xFB91, 0xF0F2]);
    cpu.regs.r[0] = 7;
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.r[0]), (0, 0));

    let (mut cpu, mut bus) = cpu_with_code(&[0xFB91, 0xF0F2]);
    handler(&mut bus, 6, 0x300, &[B_SELF]);
    cpu.ppb_write32(SHCSR, USGFAULTENA);
    let ccr = cpu.ppb_read32(CCR);
    cpu.ppb_write32(CCR, ccr | (1 << 4));
    cpu.regs.r[0] = 7;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 6);
    assert_eq!(cpu.ppb_read32(CFSR), DIVBYZERO);
    assert_eq!(frame(&cpu, &bus)[0], 7, "r0 unchanged");
}

#[test]
fn stacking_error_sets_stkerr() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    handler(&mut bus, 11, 0x200, &[B_SELF]);
    cpu.regs.msp.s = 0x3000_0000;
    executed(cpu.step(&mut bus));
    // The derived (escalated) BusFault outranks SVCall and is taken instead; SVCall stays pending.
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), STKERR);
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
    assert_ne!(cpu.ppb_read32(SHCSR) & (1 << 15), 0, "SVCALLPENDED");
}

#[test]
fn unstacking_error_sets_unstkerr() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    executed(cpu.step(&mut bus));
    cpu.regs.msp.s = 0x3000_0000;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), UNSTKERR);
    assert_eq!(
        cpu.ppb_read32(SHCSR) & (1 << 7),
        0,
        "SVCall was deactivated"
    );
}

#[test]
fn stack_limit_violation_on_entry_is_stkof() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    handler(&mut bus, 11, 0x200, &[B_SELF]);
    cpu.regs.msplim.s = SP0 - 0x10;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), STKOF);
    assert_eq!(cpu.regs.msp.s, SP0 - 0x10, "SP is set to the limit");
    assert_eq!(bus.ram[0xFE0..0x1000], [0; 32], "no frame below the limit");
}

#[test]
fn invalid_exc_return_is_invpc() {
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    executed(cpu.step(&mut bus));
    // Return to Handler mode with no other active exception.
    cpu.regs.lr = 0xFFFF_FFF1;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), INVPC);
    assert_ne!(cpu.ppb_read32(SHCSR) & (1 << 7), 0, "SVCall still active");
}

#[test]
fn exc_return_value_in_thread_mode_is_a_plain_branch() {
    // bx r0 with r0 = 0xFFFF_FFF9 in Thread mode: execute-never fetch fault, not a return.
    let (mut cpu, mut bus) = cpu_with_code(&[0x4700]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.r[0] = 0xFFFF_FFF9;
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), IACCVIOL);
    assert_eq!(frame(&cpu, &bus)[6], 0xFFFF_FFF8);
}

// ---- Items carried from earlier reviews ----

#[test]
fn bkpt_executes_when_it_condition_fails() {
    // it eq ; bkpt #1 with Z clear
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, 0xBE01]);
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.step(&mut bus), StepResult::Bkpt(1));
}

#[test]
fn cpsid_f_and_msr_faultmask_ignored_at_priority_minus_one() {
    // HardFault handler: cpsid f ; msr FAULTMASK, r0
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 3, 0x200, &[0xB671, 0xF380, 0x8813]);
    cpu.regs.r[0] = 1;
    for _ in 0..3 {
        executed(cpu.step(&mut bus));
    }
    assert_eq!(ipsr(&cpu), 3);
    assert!(!cpu.regs.faultmask.s);
}

#[test]
fn faultmask_cleared_on_exception_return() {
    // SVC handler: cpsid f ; bx lr
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[0xB671, BX_LR]);
    run2(&mut cpu, &mut bus);
    assert!(cpu.regs.faultmask.s);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 0);
    assert!(!cpu.regs.faultmask.s);
}

#[test]
fn unpredictable_in_it_block_is_undefinstr() {
    // it eq ; cbz r0, ... (CBZ is never allowed in an IT block), condition false
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, 0xB100]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), UNDEFINSTR);
    assert_eq!(frame(&cpu, &bus)[6], 0x102);

    // itt eq ; bx lr ; nop  (a branch that is not last in the IT block), condition true
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF04, BX_LR, NOP]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.xpsr.z = true;
    cpu.regs.lr = 0x201;
    run2(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), UNDEFINSTR);
    // The stacked xPSR keeps the IT state of the faulting instruction.
    assert_ne!(frame(&cpu, &bus)[7] & 0x0600_FC00, 0);

    // The same branch as the last instruction of the block is fine.
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, BX_LR]);
    cpu.regs.xpsr.z = true;
    cpu.regs.lr = 0x201;
    run2(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (0, 0x200));
}

// ---- SCS registers ----

#[test]
fn scs_register_basics() {
    let (mut cpu, _bus) = cpu_with_code(&[NOP]);
    assert_eq!(cpu.ppb_read32(CPUID), 0x410F_D213);
    assert_eq!(cpu.ppb_read32(ICTR), 14, "480 IRQs = 15 groups of 32");
    assert_eq!(cpu.ppb_read32(CCR), 0x0000_0201);
    cpu.ppb_write32(VTOR, 0x0001_2345);
    assert_eq!(cpu.ppb_read32(VTOR), 0x0001_2300);
    assert_eq!(cpu.scs.scb.vtor.s, 0x0001_2300);
    // AIRCR: VECTKEYSTAT reads 0xFA05; writes need VECTKEY 0x05FA.
    assert_eq!(cpu.ppb_read32(AIRCR) >> 16, 0xFA05);
    cpu.ppb_write32(AIRCR, 0x0000_0500);
    assert_eq!(cpu.ppb_read32(AIRCR) & 0x700, 0);
    cpu.ppb_write32(AIRCR, 0x05FA_0500);
    assert_eq!(cpu.ppb_read32(AIRCR), 0xFA05_0500);
    assert!(!cpu.take_sysreset_request());
    cpu.ppb_write32(AIRCR, 0x05FA_0504);
    assert!(cpu.take_sysreset_request());
    assert!(!cpu.take_sysreset_request());
    // Priority bytes keep the implemented (top three) bits.
    cpu.ppb_write32(NVIC_IPR0, 0xFFFF_FFFF);
    assert_eq!(cpu.ppb_read32(NVIC_IPR0), 0xE0E0_E0E0);
    // Unimplemented PPB registers read as zero and ignore writes.
    cpu.ppb_write32(0xE000_EDF0, 0xFFFF_FFFF);
    assert_eq!(cpu.ppb_read32(0xE000_EDF0), 0);
    // CPACR and the MPU registers are stored.
    cpu.ppb_write32(0xE000_ED88, 0x00F0_0000);
    assert_eq!(cpu.ppb_read32(0xE000_ED88), 0x00F0_0000);
    assert_eq!(cpu.ppb_read32(0xE000_ED90), 8 << 8, "MPU_TYPE: 8 regions");
    cpu.ppb_write32(0xE000_ED98, 3);
    cpu.ppb_write32(0xE000_ED9C, 0x2000_0000);
    cpu.ppb_write32(0xE000_ED98, 0);
    assert_eq!(cpu.ppb_read32(0xE000_ED9C), 0);
    cpu.ppb_write32(0xE000_ED98, 3);
    assert_eq!(cpu.ppb_read32(0xE000_ED9C), 0x2000_0000);
}

#[test]
fn guest_byte_and_word_scs_access() {
    // strb r1, [r0] ; ldr r2, [r3]
    let (mut cpu, mut bus) = cpu_with_code(&[0x7001, 0x681A]);
    cpu.regs.r[0] = NVIC_IPR0 + 5;
    cpu.regs.r[1] = 0x40;
    cpu.regs.r[3] = NVIC_IPR0 + 4;
    run2(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[2], 0x40 << 8);
}

#[test]
fn unprivileged_scs_access_busfaults() {
    // ldr r1, [r0]
    let (mut cpu, mut bus) = cpu_with_code(&[0x6801]);
    handler(&mut bus, 3, 0x300, &[B_SELF]);
    cpu.regs.control.s = 1;
    cpu.regs.r[0] = CPUID;
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), PRECISERR | BFARVALID);
    assert_eq!(cpu.ppb_read32(BFAR), CPUID);
}

#[test]
fn reset_uses_vtor_set_by_host() {
    let (mut cpu, mut bus) = cpu_with_code(&[NOP]);
    bus.code[0x1000..0x1004].copy_from_slice(&0x2000_8000u32.to_le_bytes());
    bus.code[0x1004..0x1008].copy_from_slice(&0x0000_0401u32.to_le_bytes());
    cpu.set_vtor(Sec::Secure, 0x1000);
    cpu.reset(&mut bus);
    assert_eq!(cpu.regs.msp.s, 0x2000_8000);
    assert_eq!(cpu.regs.pc, 0x400);
    assert_eq!(cpu.ppb_read32(VTOR), 0x1000);
}

// ---- Review fix round 1 ----

#[test]
fn lockup_on_vector_read_failure_is_stable() {
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    cpu.ppb_write32(VTOR, 0x3000_0000);
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    assert_ne!(cpu.ppb_read32(HFSR) & (1 << 1), 0, "VECTTBL");
    let sp = cpu.regs.sp();
    let ram = bus.ram.clone();
    for _ in 0..4 {
        assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
        assert_eq!(cpu.regs.sp(), sp, "no stacking while locked up");
        assert!(bus.ram == ram, "memory unchanged while locked up");
    }
}

#[test]
fn lockup_on_nmi_stacking_failure_in_hardfault_is_stable() {
    // A stacking fault during NMI entry from HardFault locks up at NMI priority (-2), so the
    // still-pending NMI cannot leave lockup.
    let (mut cpu, mut bus) = cpu_with_code(&[UDF0]);
    handler(&mut bus, 3, 0x200, &[B_SELF]);
    handler(&mut bus, 2, 0x300, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    cpu.regs.msp.s = 0x3000_0000;
    cpu.ppb_write32(ICSR, 1 << 31);
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    let ram = bus.ram.clone();
    for _ in 0..4 {
        assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
        assert_eq!(cpu.regs.msp.s, 0x3000_0000 - 0x20);
        assert!(bus.ram == ram);
    }
}

#[test]
fn exception_return_sets_event_register() {
    // svc 0 ; wfe ; b .   with the SVC handler returning immediately.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, 0xBF20, B_SELF]);
    handler(&mut bus, 11, 0x200, &[BX_LR]);
    executed(cpu.step(&mut bus));
    cpu.event = false;
    executed(cpu.step(&mut bus));
    assert!(cpu.event, "exception return sets the event register");
    // WFE consumes the event and falls through.
    executed(cpu.step(&mut bus));
    assert_eq!(cpu.regs.pc, 0x104);
    assert!(!cpu.event);

    // Exception entry sets it too.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0]);
    handler(&mut bus, 11, 0x200, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert!(cpu.event);
}

#[test]
fn stacking_below_address_zero_does_not_panic() {
    // msr MSP, r0 (r0 = 0x10) ; svc 0: the frame wraps around the top of the address space.
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8808, SVC0]);
    cpu.regs.r[0] = 0x10;
    executed(cpu.step(&mut bus));
    let r = cpu.step(&mut bus);
    assert!(
        matches!(r, StepResult::Executed { .. } | StepResult::Lockup),
        "{r:?}"
    );
    assert_ne!(cpu.ppb_read32(CFSR) & STKERR, 0);
}

#[test]
fn corrupt_stacked_ipsr_is_invpc_not_a_panic() {
    // SVC handler: nop ; bx lr. NMI preempts it; its frame's stacked IPSR is corrupted to 0x1FF,
    // which the NMI return restores (Handler mode, nonzero). The SVC handler's own return then
    // finds exception 511 not active: INVPC.
    let (mut cpu, mut bus) = cpu_with_code(&[SVC0, NOP]);
    handler(&mut bus, 11, 0x200, &[NOP, BX_LR]);
    handler(&mut bus, 2, 0x300, &[BX_LR]);
    handler(&mut bus, 3, 0x400, &[B_SELF]);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 11);
    cpu.ppb_write32(ICSR, 1 << 31);
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 2);
    let psr = cpu.regs.sp() + 0x1C;
    let o = (psr - RAM_BASE) as usize;
    let v = ram(&bus, psr) | 0x1FF;
    bus.ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.pc), (0x1FF, 0x200));
    executed(cpu.step(&mut bus));
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR), INVPC);
}

// ---- Stack limit checks on instruction SP writes (ViolatesSPLim) ----

/// MSPLIM sits one basic frame below SP0, so the STKOF UsageFault itself can still be stacked.
const LIMIT: u32 = SP0 - 0x20;

/// A Secure Thread CPU on MSP = SP0 with MSPLIM = `LIMIT`, UsageFault enabled and handled at
/// 0x400, running `code` at 0x100.
fn stklim_cpu(code: &[u16]) -> (Cpu, FlatBus) {
    let (mut cpu, mut bus) = cpu_with_code(code);
    handler(&mut bus, 6, 0x400, &[B_SELF]);
    cpu.ppb_write32(SHCSR, USGFAULTENA);
    cpu.regs.msplim.s = LIMIT;
    (cpu, bus)
}

/// Step the instruction at 0x100 and require a precise STKOF UsageFault: the frame (with the
/// faulting instruction's address) is the only thing written, and SP was not moved by the
/// instruction.
fn expect_stkof(cpu: &mut Cpu, bus: &mut FlatBus) {
    executed(cpu.step(bus));
    assert_eq!(ipsr(cpu), 6, "UsageFault");
    assert_eq!(cpu.ppb_read32(CFSR), STKOF);
    assert_eq!(
        cpu.regs.msp.s,
        SP0 - 0x20,
        "only the exception frame moved MSP"
    );
    assert_eq!(
        frame(cpu, bus)[6],
        0x100,
        "stacked PC is the faulting instruction"
    );
    for a in (LIMIT - 0x40..LIMIT).step_by(4) {
        assert_eq!(ram(bus, a), 0, "nothing stored below the limit at {a:#x}");
    }
}

#[test]
fn push_below_msplim_is_stkof() {
    // push {r0-r7, lr}: 36 bytes, 4 below the limit.
    let (mut cpu, mut bus) = stklim_cpu(&[0xB5FF]);
    for i in 0..8 {
        cpu.regs.r[i] = 0xA0 + i as u32;
    }
    expect_stkof(&mut cpu, &mut bus);

    // stmdb sp!, {r0-r8} (PUSH.W) likewise.
    let (mut cpu, mut bus) = stklim_cpu(&[0xE92D, 0x01FF]);
    cpu.regs.r[0] = 0xA0;
    expect_stkof(&mut cpu, &mut bus);

    // push {r0-r7}: exactly down to the limit is allowed.
    let (mut cpu, mut bus) = stklim_cpu(&[0xB4FF]);
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.sp()), (0, LIMIT));
    assert_eq!(cpu.ppb_read32(CFSR), 0);
}

#[test]
fn sub_sp_below_limit_is_stkof() {
    // sub sp, #64
    let (mut cpu, mut bus) = stklim_cpu(&[0xB090]);
    expect_stkof(&mut cpu, &mut bus);

    // subs sp, sp, #64: the faulting instruction does not set the flags either (C would be 1).
    let (mut cpu, mut bus) = stklim_cpu(&[0xF1BD, 0x0D40]);
    expect_stkof(&mut cpu, &mut bus);
    assert_eq!(
        frame(&cpu, &bus)[7] & 0xF000_0000,
        0,
        "stacked NZCV unchanged"
    );

    // In Thread mode with CONTROL.SPSEL = 1 the limit is PSPLIM (MSPLIM does not apply).
    let (mut cpu, mut bus) = stklim_cpu(&[0xB090, 0xB090]);
    const PSP: u32 = SP0 + 0x800;
    cpu.regs.control.s = 0b10;
    cpu.regs.psp.s = PSP;
    cpu.regs.psplim.s = PSP - 0x60;
    cpu.regs.msplim.s = PSP;
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.psp.s), (0, PSP - 0x40));
    executed(cpu.step(&mut bus));
    assert_eq!(ipsr(&cpu), 6);
    assert_eq!(cpu.ppb_read32(CFSR), STKOF);
    assert_eq!(cpu.regs.psp.s, PSP - 0x60, "frame on PSP; SP not moved");
}

#[test]
fn str_with_sp_writeback_below_limit_is_stkof() {
    // str r0, [sp, #-40]!
    let (mut cpu, mut bus) = stklim_cpu(&[0xF84D, 0x0D28]);
    cpu.regs.r[0] = 0xDEAD_BEEF;
    expect_stkof(&mut cpu, &mut bus);

    // ldr r0, [sp, #-40]! : the load does not complete either.
    let (mut cpu, mut bus) = stklim_cpu(&[0xF85D, 0x0D28]);
    cpu.regs.r[0] = 0x1234;
    expect_stkof(&mut cpu, &mut bus);
    assert_eq!(frame(&cpu, &bus)[0], 0x1234, "r0 not loaded");
}

#[test]
fn msr_msp_is_exempt_from_the_stack_limit() {
    // msr msp, r0 ; nop
    let (mut cpu, mut bus) = stklim_cpu(&[0xF380, 0x8808, NOP]);
    cpu.regs.r[0] = LIMIT - 0x100;
    executed(cpu.step(&mut bus));
    assert_eq!((ipsr(&cpu), cpu.regs.msp.s), (0, LIMIT - 0x100));
    assert_eq!(cpu.ppb_read32(CFSR), 0);
}

// ---- PPB accesses the CPU does not implement ----

/// A `FlatBus` that answers `0x55` bytes for reads at or above `0xE004_0000` (the external PPB),
/// records word writes there, and logs every access there as `(addr, privileged)`.
struct EppbBus {
    inner: FlatBus,
    writes: Vec<(u32, u32)>,
    accesses: Vec<(u32, bool)>,
}

const EPPB_START: u32 = 0xE004_0000;

impl EppbBus {
    fn new(inner: FlatBus) -> Self {
        Self {
            inner,
            writes: Vec::new(),
            accesses: Vec::new(),
        }
    }

    /// Records `addr` and returns true when it is in the external PPB.
    fn eppb(&mut self, addr: u32, a: Access) -> bool {
        let hit = addr >= EPPB_START;
        if hit {
            self.accesses.push((addr, a.privileged));
        }
        hit
    }
}

impl Bus for EppbBus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        if self.eppb(addr, a) {
            return Ok(0x55);
        }
        self.inner.read8(addr, a)
    }
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        if self.eppb(addr, a) {
            return Ok(0x5555);
        }
        self.inner.read16(addr, a)
    }
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        if self.eppb(addr, a) {
            return Ok(0x5555_5555);
        }
        self.inner.read32(addr, a)
    }
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        if self.eppb(addr, a) {
            return Ok(());
        }
        self.inner.write8(addr, v, a)
    }
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        if self.eppb(addr, a) {
            return Ok(());
        }
        self.inner.write16(addr, v, a)
    }
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        if self.eppb(addr, a) {
            self.writes.push((addr, v));
            return Ok(());
        }
        self.inner.write32(addr, v, a)
    }
}

#[test]
fn eppb_access_reaches_the_bus() {
    // ldr r0,=0xE004_3018; ldr r1,[r0]; orr r1,#2; str r1,[r0]; b .; literal
    let (mut cpu, inner) = cpu_with_code(&[
        0x4802, 0x6801, 0xF041, 0x0102, 0x6001, B_SELF, 0x3018, 0xE004,
    ]);
    let mut bus = EppbBus::new(inner);
    for _ in 0..4 {
        executed(cpu.step(&mut bus));
    }
    assert_eq!(ipsr(&cpu), 0, "no fault");
    assert_eq!(cpu.regs.r[1], 0x5555_5557);
    assert_eq!(bus.writes, [(0xE004_3018, 0x5555_5557)]);
    assert_eq!(bus.accesses, [(0xE004_3018, true), (0xE004_3018, true)]);
    assert_eq!(cpu.take_ppb_misses(), []);
}

#[test]
fn unprivileged_eppb_access_is_busfault() {
    // Unprivileged Thread mode: ldr r1,[r0] / str r1,[r0] at an aligned EPPB word, and an
    // unaligned ldr there (the bytewise path). The whole PPB is privileged-only, so each one
    // BusFaults (escalated to HardFault) without reaching the bus.
    for (hw, addr) in [
        (0x6801, 0xE004_3018), // ldr r1,[r0]
        (0x6001, 0xE004_3018), // str r1,[r0]
        (0x6801, 0xE004_3019), // ldr r1,[r0], unaligned
        (0x6801, 0xE00F_FFFC), // ldr r1,[r0], last EPPB word
    ] {
        let (mut cpu, mut inner) = cpu_with_code(&[hw]);
        handler(&mut inner, 3, 0x300, &[B_SELF]);
        let mut bus = EppbBus::new(inner);
        cpu.regs.control.s = 1;
        cpu.regs.r[0] = addr;
        cpu.regs.r[1] = 0x1234_5678;
        executed(cpu.step(&mut bus));
        assert_eq!(ipsr(&cpu), 3, "{hw:#06x} at {addr:#010x}");
        assert_eq!(cpu.ppb_read32(CFSR), PRECISERR | BFARVALID);
        assert_eq!(cpu.ppb_read32(BFAR), addr);
        assert_eq!(bus.accesses, [], "{hw:#06x} at {addr:#010x}");
        assert_eq!(bus.writes, []);
        assert_eq!(cpu.take_ppb_misses(), []);
    }
}

#[test]
fn unimplemented_internal_ppb_is_recorded() {
    // ldr r0,=DWT_CTRL; ldr r1,[r0]; ldr r0,=ITM_TCR; str r2,[r0]; b .; nop; literals
    let (mut cpu, mut bus) = cpu_with_code(&[
        0x4802, 0x6801, 0x4802, 0x6002, B_SELF, NOP, 0x1000, 0xE000, 0x0E80, 0xE000,
    ]);
    cpu.regs.r[1] = 0xFFFF_FFFF;
    cpu.regs.r[2] = 1;
    for _ in 0..4 {
        executed(cpu.step(&mut bus));
    }
    assert_eq!(ipsr(&cpu), 0, "no fault");
    assert_eq!(cpu.regs.r[1], 0);
    assert_eq!(
        cpu.take_ppb_misses(),
        [
            PpbMiss {
                addr: 0xE000_1000,
                width: 4,
                write: None,
                secure: true
            },
            PpbMiss {
                addr: 0xE000_0E80,
                width: 4,
                write: Some(1),
                secure: true
            },
        ]
    );
    assert_eq!(cpu.take_ppb_misses(), [], "drained");
}

#[test]
fn host_ppb_reads_record_nothing() {
    let (mut cpu, _bus) = cpu_with_code(&[B_SELF]);
    assert_eq!(cpu.ppb_read32(0xE000_1000), 0);
    cpu.ppb_write32(0xE000_0E80, 1);
    assert_eq!(cpu.ppb_peek32(0xE000_1000, Sec::Secure), None);
    assert_eq!(cpu.take_ppb_misses(), []);
}
