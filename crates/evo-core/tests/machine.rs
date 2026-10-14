//! `Machine`: booting a bundle, running, stopping, sleeping and system reset.
//!
//! Every test boots a synthetic bundle whose secure image is a hand-assembled Thumb program,
//! placed by `Layout::EVO_7_1` like the real OS.

use std::ops::ControlFlow;

use armv8m::Sec;
use evo_core::{EventKind, Machine, StopReason};
use evo_image::testutil::secure_image_bundle;
use evo_image::{Bundle, ImageError, Layout};

/// `VTOR_S` for `Layout::EVO_7_1`: Secure alias of slot 0x000B_0000 plus the 0x400 header.
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the code, after room for 128 vectors.
const CODE: u32 = 0x200;
const SP_INIT: u32 = 0x3000_0800;

const NOP: u16 = 0xBF00;
const WFI: u16 = 0xBF30;
const UDF0: u16 = 0xDE00;
const BX_LR: u16 = 0x4770;
const B_SELF: u16 = 0xE7FE;
const ADDS_R0_1: u16 = 0x3001;
/// `adds r4, #1`: r4, unlike r0-r3, survives an exception return.
const ADDS_R4_1: u16 = 0x3401;

const SYSTICK: u32 = 15;
const IRQ0: u32 = 16;
const HARDFAULT: u32 = 3;
const NVIC_ISER0: u32 = 0xE000_E100;
const SYST_CSR: u32 = 0xE000_E010;
const SYST_RVR: u32 = 0xE000_E014;

/// Address of payload offset `off` (Secure alias).
fn addr(off: u32) -> u32 {
    VTOR + off
}

/// The secure image's payload: a vector table at offset 0, code from `CODE` on.
struct Payload(Vec<u8>);

impl Payload {
    fn new() -> Self {
        let mut p = Payload(vec![0; 0x400]);
        p.word(0, SP_INIT);
        p
    }

    fn word(&mut self, off: u32, v: u32) -> &mut Self {
        let o = off as usize;
        self.0[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }

    fn code(&mut self, off: u32, hws: &[u16]) -> &mut Self {
        for (i, hw) in hws.iter().enumerate() {
            let o = off as usize + 2 * i;
            self.0[o..o + 2].copy_from_slice(&hw.to_le_bytes());
        }
        self
    }

    /// Point vector `n` at the Thumb code at payload offset `off`.
    fn vector(&mut self, n: u32, off: u32) -> &mut Self {
        self.word(4 * n, addr(off) | 1)
    }

    fn bundle(&self) -> Bundle {
        Bundle::from_bytes(&secure_image_bundle(self.0.clone())).expect("synthetic bundle")
    }

    fn boot(&self) -> Machine {
        Machine::from_bundle(&self.bundle(), &Layout::EVO_7_1).expect("boot")
    }
}

/// Reset handler: a 6-instruction loop, 7 cycles per pass.
fn looping() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE).code(
        CODE,
        &[
            ADDS_R0_1, // adds r0, #1
            0x3102,    // adds r1, #2
            0x3A01,    // subs r2, #1
            NOP,       // nop
            NOP,       // nop
            0xE7F9,    // b CODE
        ],
    );
    p
}

/// Reset handler: `wfi ; adds r0, #1 ; b wfi`. IRQ 0 and SysTick run `adds r4, #1 ; bx lr`.
fn sleeper() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE)
        .vector(SYSTICK, 0x220)
        .vector(IRQ0, 0x220)
        .code(CODE, &[WFI, ADDS_R0_1, 0xE7FC])
        .code(0x220, &[ADDS_R4_1, BX_LR]);
    p
}

#[test]
fn machine_boots_synthetic_bundle() {
    let mut m = looping().boot();
    assert_eq!(m.cpu.regs.pc, addr(CODE));
    assert!(m.cpu.regs.xpsr.t);
    assert_eq!(m.cpu.regs.sec, Sec::Secure);
    assert_eq!(m.cpu.regs.msp.s, SP_INIT);
    assert_eq!(m.cpu.scs.scb.vtor.s, VTOR);
    assert_eq!(m.sched.now, 0);
    assert_eq!(m.resets, 0);

    // Flash: 8 MiB at both aliases, erased outside the image.
    assert_eq!(m.bus.peek(0x000B_0400, 4), Some(SP_INIT));
    assert_eq!(m.bus.peek(0x100B_0400, 4), Some(SP_INIT));
    assert_eq!(
        m.bus.peek(0x000B_0000, 4),
        Some(0x96f3_b83d),
        "MCUboot header"
    );
    assert_eq!(m.bus.peek(0x0000_0000, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x107F_FFFC, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x0080_0000, 1), None);
    assert_eq!(m.bus.peek(0x1080_0000, 1), None);
    // SRAM: 2 MiB, one backing at both aliases.
    assert!(m.bus.poke(0x2000_0010, 4, 0x1234_5678));
    assert_eq!(m.bus.peek(0x3000_0010, 4), Some(0x1234_5678));
    assert_eq!(m.bus.peek(0x301F_FFFC, 4), Some(0));
    assert_eq!(m.bus.peek(0x2020_0000, 1), None);
    assert_eq!(m.bus.peek(0x3020_0000, 1), None);

    // Execution enters the loop and stays there.
    assert_eq!(m.run(100), StopReason::CycleLimit);
    assert!(
        (addr(CODE)..addr(CODE + 12)).contains(&m.cpu.regs.pc),
        "pc {:#x}",
        m.cpu.regs.pc
    );
    assert_eq!(
        m.cpu.regs.r[0], 15,
        "100 cycles = 14 passes of 7 + 2 into the 15th"
    );
    assert!(m.bus.log.is_empty(), "no unmapped accesses");
}

#[test]
fn run_stops_at_cycle_limit() {
    // Review Focus 3: a guest that never stops is stopped by the cycle budget.
    let mut m = looping().boot();
    assert_eq!(m.run(10_000), StopReason::CycleLimit);
    // Checked before every instruction (each costs at most 2 cycles here).
    assert!((10_000..10_002).contains(&m.sched.now), "{}", m.sched.now);
    assert!(!m.cpu.locked_up());
    let passes = m.cpu.regs.r[0];
    assert!((1428..=1429).contains(&passes), "{passes}");

    // The budget is per call.
    assert_eq!(m.run(5_000), StopReason::CycleLimit);
    assert!((15_000..15_002).contains(&m.sched.now), "{}", m.sched.now);

    // A zero budget does nothing.
    let (now, pc) = (m.sched.now, m.cpu.regs.pc);
    assert_eq!(m.run(0), StopReason::CycleLimit);
    assert_eq!((m.sched.now, m.cpu.regs.pc), (now, pc));
}

#[test]
fn lockup_reported() {
    // Reset handler `udf` -> UsageFault, disabled -> HardFault, whose handler is `udf` too:
    // a fault in HardFault locks the CPU up.
    let mut p = Payload::new();
    p.vector(1, CODE)
        .vector(HARDFAULT, 0x210)
        .code(CODE, &[UDF0])
        .code(0x210, &[UDF0]);
    let mut m = p.boot();
    assert_eq!(m.run(1_000_000), StopReason::Lockup { pc: addr(0x210) });
    assert!(m.cpu.locked_up());
    assert!(m.sched.now < 100, "stopped at once: {}", m.sched.now);

    // Running a locked-up machine again reports the lockup again instead of spinning.
    let now = m.sched.now;
    assert!(matches!(m.run(1_000_000), StopReason::Lockup { .. }));
    assert_eq!(m.sched.now, now);
}

#[test]
fn breakpoint_stops() {
    let mut m = looping().boot();
    let bp = addr(CODE + 4); // subs r2, #1
    m.breakpoints.insert(bp);
    assert_eq!(m.run(10_000), StopReason::Breakpoint(bp));
    assert_eq!(m.cpu.regs.pc, bp);
    assert_eq!(
        (m.cpu.regs.r[0], m.cpu.regs.r[2]),
        (1, 0),
        "stopped before it"
    );
    assert_eq!(m.sched.now, 2);

    // Resuming executes the breakpointed instruction and stops at it on the next pass.
    assert_eq!(m.run(10_000), StopReason::Breakpoint(bp));
    assert_eq!((m.cpu.regs.r[0], m.cpu.regs.r[2]), (2, u32::MAX));
    assert_eq!(m.sched.now, 9);

    // A breakpoint on the first instruction of a fresh machine stops before it runs.
    let mut m = looping().boot();
    m.breakpoints.insert(addr(CODE));
    assert_eq!(m.run(10_000), StopReason::Breakpoint(addr(CODE)));
    assert_eq!(m.sched.now, 0);
    m.breakpoints.clear();
    assert_eq!(m.run(100), StopReason::CycleLimit);
}

#[test]
fn step_executes_one_instruction() {
    let mut m = looping().boot();
    m.breakpoints.insert(addr(CODE));
    assert_eq!(m.step(), StopReason::Stepped, "a step ignores breakpoints");
    assert_eq!((m.cpu.regs.pc, m.sched.now), (addr(CODE + 2), 1));
    assert_eq!(m.cpu.regs.r[0], 1);

    // While asleep, a step skips to the next event instead.
    let mut m = sleeper().boot();
    m.sched.schedule(500, EventKind::Peripheral(1));
    assert_eq!(m.step(), StopReason::Stepped);
    assert!(m.asleep());
    assert_eq!(m.sched.now, 1);
    assert_eq!(m.step(), StopReason::Stepped);
    assert_eq!(m.sched.now, 500);
    assert!(m.asleep());
    assert_eq!(m.cpu.regs.pc, addr(CODE + 2), "nothing executed");

    // With nothing left that could wake it, a step reports that instead of stepping.
    assert_eq!(m.step(), StopReason::Idle);
    assert_eq!((m.sched.now, m.cpu.regs.pc), (500, addr(CODE + 2)));
    assert!(m.asleep());
}

#[test]
fn guest_bkpt_reported() {
    let mut p = Payload::new();
    p.vector(1, CODE).code(CODE, &[NOP, 0xBEAB]);
    let mut m = p.boot();
    let stop = m.run(1_000);
    assert_eq!(
        stop,
        StopReason::Bkpt {
            pc: addr(CODE + 2),
            imm: 0xAB
        }
    );
    assert_eq!(m.cpu.regs.pc, addr(CODE + 2), "the PC stays on the bkpt");
}

#[test]
fn wfi_skips_to_next_event() {
    let mut m = sleeper().boot();
    m.sched.schedule(10_000, EventKind::Peripheral(7));
    // `wfi` (1 cycle), then the machine jumps to the event. The event wakes nobody (no
    // peripheral raises an IRQ yet), and with nothing else due the CPU would sleep forever,
    // which `run` reports as `Idle` long before the budget.
    assert_eq!(m.run(1_000_000), StopReason::Idle);
    assert_eq!(m.sched.now, 10_000);
    assert_eq!(m.sched.next_deadline(), None, "the event fired");
    assert!(m.asleep());
    assert_eq!(m.cpu.regs.r[0], 0, "still asleep");
    assert_eq!(m.cpu.regs.pc, addr(CODE + 2));

    // An event beyond the budget: time advances to the end of the budget. That is a
    // `CycleLimit`, not `Idle`: the CPU is asleep, but something will wake it.
    m.sched.schedule(50_000, EventKind::Peripheral(7));
    assert_eq!(m.run(1_000), StopReason::CycleLimit);
    assert_eq!(m.sched.now, 11_000);
    assert!(m.asleep());

    // A pending interrupt wakes it: handler, back to thread, `adds`, `wfi` again.
    m.cpu.ppb_write32(NVIC_ISER0, 1);
    m.cpu.pulse_irq(0);
    assert_eq!(m.run(1_000), StopReason::CycleLimit);
    assert_eq!((m.cpu.regs.r[0], m.cpu.regs.r[4]), (1, 1));
    assert!(m.asleep());
    assert_eq!(m.sched.now, 12_000);
}

/// With `SCR.SEVONPEND`, pending even a disabled IRQ sets the event register. That wakes a WFE
/// (consuming the event) but not a WFI, which waits for an exception.
#[test]
fn wfe_wakes_on_event_wfi_does_not() {
    const SCR: u32 = 0xE000_ED10;
    const SEVONPEND: u32 = 1 << 4;
    const WFE: u16 = 0xBF20;

    // wfe ; adds r0, #1 ; b wfe
    let mut p = Payload::new();
    p.vector(1, CODE).code(CODE, &[WFE, ADDS_R0_1, 0xE7FC]);
    let mut m = p.boot();
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert!(m.asleep(), "no event yet: WFE sleeps");
    assert_eq!(m.cpu.regs.r[0], 0);

    m.cpu.ppb_write32(SCR, SEVONPEND);
    m.cpu.pulse_irq(5); // disabled: pending, but no exception will be taken
    assert!(m.cpu.event);
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert_eq!(m.cpu.regs.r[0], 1, "woke once, then slept in the next WFE");
    assert!(m.asleep());
    assert!(!m.cpu.event, "the wake-up consumed the event");

    // The same with WFI: the event does not wake it.
    let mut m = sleeper().boot();
    assert_eq!(m.run(1_000), StopReason::Idle);
    m.cpu.ppb_write32(SCR, SEVONPEND);
    m.cpu.pulse_irq(5);
    assert!(m.cpu.event);
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert_eq!(m.cpu.regs.r[0], 0, "WFI still asleep");
    assert!(m.asleep());
    assert!(m.cpu.event, "left for a later WFE");
}

#[test]
fn run_with_callback_can_halt_the_run() {
    let mut m = looping().boot();
    let mut calls = 0;
    let stop = m.run_with(10_000, |_| {
        calls += 1;
        if calls == 4 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    assert_eq!(stop, StopReason::Halted);
    assert_eq!(
        m.cpu.regs.pc,
        addr(CODE + 6),
        "the fourth instruction did not run"
    );
    assert_eq!((m.sched.now, m.cpu.regs.r[0]), (3, 1));
    // The next run goes on from there.
    assert_eq!(m.run(1), StopReason::CycleLimit);
    assert_eq!(m.cpu.regs.pc, addr(CODE + 8));

    // A run resumed from a breakpoint and halted before its first step still steps over the
    // breakpoint when it is resumed again.
    let mut m = looping().boot();
    let bp = addr(CODE + 4);
    m.breakpoints.insert(bp);
    assert_eq!(m.run(10_000), StopReason::Breakpoint(bp));
    assert_eq!(
        m.run_with(10_000, |_| ControlFlow::Break(())),
        StopReason::Halted
    );
    assert_eq!((m.cpu.regs.pc, m.sched.now), (bp, 2));
    assert_eq!(m.run(10_000), StopReason::Breakpoint(bp));
    assert_eq!(
        m.cpu.regs.r[2],
        u32::MAX,
        "the breakpointed instruction ran once"
    );
    assert_eq!(m.sched.now, 9);
}

#[test]
fn wake_ends_sleep_like_a_debug_halt() {
    let mut m = sleeper().boot();
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert!(m.asleep());
    m.wake();
    assert!(!m.asleep());
    // Execution continues after the WFI, although no interrupt is pending.
    assert_eq!(m.step(), StopReason::Stepped);
    assert_eq!(m.cpu.regs.r[0], 1);
    m.wake();
    assert!(!m.asleep(), "waking an awake CPU does nothing");
    assert_eq!(m.run(1_000), StopReason::Idle, "the next WFI sleeps again");
    assert!(m.asleep());
}

#[test]
fn wfi_wakes_on_systick() {
    let mut m = sleeper().boot();
    m.cpu.ppb_write32(SYST_RVR, 999);
    m.cpu.ppb_write32(SYST_CSR, 0b111); // ENABLE, TICKINT, CLKSOURCE
    assert_eq!(m.run(10_000), StopReason::CycleLimit);
    // One SysTick every 1000 cycles, each one handler pass and one thread pass.
    let ticks = m.cpu.regs.r[4];
    assert!((9..=10).contains(&ticks), "{ticks} SysTicks");
    assert_eq!(m.cpu.regs.r[0], ticks);
    assert!(m.asleep());
    assert_eq!(m.sched.now, 10_000);
}

#[test]
fn system_reset_request_reboots() {
    // Each boot increments a counter in SRAM and reads an unmapped address; the first two
    // boots then request a system reset through AIRCR.
    let mut p = Payload::new();
    p.vector(1, CODE).code(
        CODE,
        &[
            0x4806, // ldr r0, [pc, #24]  ; =0x3000_0000
            0x6801, // ldr r1, [r0]
            0x3101, // adds r1, #1
            0x6001, // str r1, [r0]
            0x4C05, // ldr r4, [pc, #20]  ; =0x4000_0000
            0x6824, // ldr r4, [r4]       ; unmapped
            0x2903, // cmp r1, #3
            0xD003, // beq stay
            0x4A04, // ldr r2, [pc, #16]  ; =AIRCR
            0x4B05, // ldr r3, [pc, #20]  ; =VECTKEY | SYSRESETREQ
            0x6013, // str r3, [r2]
            B_SELF, // b .                ; until the reset
            B_SELF, // stay: b .
            NOP,
        ],
    );
    p.word(CODE + 0x1C, 0x3000_0000)
        .word(CODE + 0x20, 0x4000_0000)
        .word(CODE + 0x24, 0xE000_ED0C)
        .word(CODE + 0x28, 0x05FA_0004);
    let mut m = p.boot();
    m.sched.schedule(1_000_000, EventKind::Peripheral(3));

    assert_eq!(m.run(1_000), StopReason::CycleLimit);
    assert_eq!(m.resets, 2);
    assert_eq!(m.bus.peek(0x3000_0000, 4), Some(3), "SRAM survives a reset");
    assert_eq!(m.cpu.regs.pc, addr(CODE + 0x18), "third boot stays");
    assert_eq!(m.cpu.regs.msp.s, SP_INIT);
    assert_eq!(m.sched.next_deadline(), None, "pending events are dropped");
    assert!(m.sched.now >= 1_000, "time keeps counting: {}", m.sched.now);
    let unmapped: Vec<_> = m.bus.log.entries().map(|(a, n)| (a.addr, n)).collect();
    assert_eq!(unmapped, [(0x4000_0000, 3)], "the log survives a reset");
}

#[test]
fn trace_line_shows_the_next_instruction() {
    let mut m = looping().boot();
    assert_eq!(m.trace_line(), "           0 S 100b0600: adds r0, #1");
    m.step();
    assert_eq!(m.trace_line(), "           1 S 100b0602: adds r1, #2");
    // Unreadable code is reported, not fetched (nothing is logged).
    m.cpu.regs.pc = 0x5000_0000;
    assert_eq!(m.trace_line(), "           1 S 50000000: <unreadable>");
    assert!(m.bus.log.is_empty());
}

#[test]
fn recent_trace_holds_the_last_64_steps() {
    let mut m = looping().boot();
    assert!(m.recent_trace().is_empty(), "nothing has run yet");

    // Each step adds the line `--trace` prints before it.
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.push(m.trace_line());
        m.step();
    }
    assert_eq!(m.recent_trace(), expected);

    // A longer run keeps only the last 64 steps, oldest first.
    let mut traced = Vec::new();
    let stop = m.run_with(1_000, |m| {
        traced.push(m.trace_line());
        ControlFlow::Continue(())
    });
    assert_eq!(stop, StopReason::CycleLimit);
    assert!(traced.len() > 64);
    assert_eq!(m.recent_trace(), traced[traced.len() - 64..]);
}

#[test]
fn recent_trace_ends_at_the_lockup() {
    // As in `lockup_reported`: `udf` in the reset handler, and again in HardFault.
    let mut p = Payload::new();
    p.vector(1, CODE)
        .vector(HARDFAULT, 0x210)
        .code(CODE, &[UDF0])
        .code(0x210, &[UDF0]);
    let mut m = p.boot();
    assert_eq!(m.run(1_000_000), StopReason::Lockup { pc: addr(0x210) });
    let recent = m.recent_trace();
    assert!(
        recent[0].ends_with(" S 100b0600: udf #0"),
        "first step: {recent:?}"
    );
    assert!(
        recent
            .last()
            .is_some_and(|l| l.ends_with(" S 100b0610: udf #0")),
        "last step is the one that locked up: {recent:?}"
    );
}

#[test]
fn image_outside_flash_is_an_error() {
    // Ruling R16: placement is checked before the memory map is built, so a slot past the
    // 8 MiB flash is an `ImageError`, not a panic.
    let b = looping().bundle();
    let layout = Layout {
        secure_slot_base: 0x007F_FF00,
        ns_slot_base: 0x0038_0000,
    };
    assert!(matches!(
        Machine::from_bundle(&b, &layout).err(),
        Some(ImageError::DoesNotFit { image: 0, .. })
    ));
}

#[test]
fn machine_state_is_serializable() {
    fn serde<T: serde::Serialize + serde::de::DeserializeOwned>() {}
    serde::<Machine>();
    serde::<StopReason>();
}
