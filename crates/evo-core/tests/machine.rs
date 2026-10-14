//! `Machine`: booting a bundle, running, stopping, sleeping and system reset.
//!
//! Every test boots a synthetic bundle whose secure image is a hand-assembled Thumb program,
//! placed by `Layout::EVO_7_1` like the real OS.

use std::ops::ControlFlow;

use armv8m::Sec;
use evo_core::{EventKind, Key, Machine, StopReason, UartPort};
use evo_hw::dev::keypad::{Keypad, SCAN_CYCLES};
use evo_hw::{DevCtx, Device, ResetKind, StateError, UnmappedKind};
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

    // Flash: 16 MiB at both aliases, erased outside the image.
    assert_eq!(m.bus.peek(0x000B_0400, 4), Some(SP_INIT));
    assert_eq!(m.bus.peek(0x100B_0400, 4), Some(SP_INIT));
    assert_eq!(
        m.bus.peek(0x000B_0000, 4),
        Some(0x96f3_b83d),
        "MCUboot header"
    );
    assert_eq!(m.bus.peek(0x0000_0000, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x107F_FFFC, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x0100_0000, 1), None);
    assert_eq!(m.bus.peek(0x1100_0000, 1), None);
    // SRAM: 1.5 MiB, one backing at both aliases.
    assert!(m.bus.poke(0x2000_0010, 4, 0x1234_5678));
    assert_eq!(m.bus.peek(0x3000_0010, 4), Some(0x1234_5678));
    assert_eq!(m.bus.peek(0x3017_FFFC, 4), Some(0));
    assert_eq!(m.bus.peek(0x2018_0000, 1), None);
    assert_eq!(m.bus.peek(0x3018_0000, 1), None);

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
            0x4806, // ldr r0, [pc, #24]  ; =0x3000_0100
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
    p.word(CODE + 0x1C, 0x3000_0100)
        .word(CODE + 0x20, 0x4000_0000)
        .word(CODE + 0x24, 0xE000_ED0C)
        .word(CODE + 0x28, 0x05FA_0004);
    let mut m = p.boot();
    m.sched.schedule(1_000_000, EventKind::Peripheral(3));

    assert_eq!(m.run(1_000), StopReason::CycleLimit);
    assert_eq!(m.resets, 2);
    assert_eq!(m.bus.peek(0x3000_0100, 4), Some(3), "SRAM survives a reset");
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
    // 16 MiB flash is an `ImageError`, not a panic.
    let b = looping().bundle();
    let layout = Layout {
        secure_slot_base: 0x00FF_FF00,
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

// ---- devices in the machine ----

/// Window of the test device.
const PROBE: u32 = 0x6000_0000;
const AIRCR: u32 = 0xE000_ED0C;
const SYSRESETREQ_KEY: u32 = 0x05FA_0004;
const IRQ5: u32 = IRQ0 + 5;
const IRQ6: u32 = IRQ0 + 6;
/// IRQ 5 handler: `probe[0] = 0` (drop the line), `adds r4, #1`, `bx lr`.
const H5: u32 = 0x280;
/// IRQ 6 handler: `adds r5, #1`, `bx lr`.
const H6: u32 = 0x2A0;
/// Literal pool: one word per constant a test program loads.
const LIT: u32 = 0x300;
const L_PROBE: u32 = LIT;
const L_ISER0: u32 = LIT + 4;
const L_AIRCR: u32 = LIT + 8;
const L_KEY: u32 = LIT + 0xC;
const L_1000: u32 = LIT + 0x10;
const L_DWT: u32 = LIT + 0x14;
const L_EPPB: u32 = LIT + 0x18;
const L_FLAG: u32 = LIT + 0x1C;
/// SRAM word a program uses to tell its first boot from the next.
const FLAG: u32 = 0x3000_0100;

/// Test device at [`PROBE`]: +0 drives IRQ line 5 (`v != 0`), +4 asks for an event at
/// `now + v` that asserts line 5, +8 pulses IRQ 6. Counts `event` and `reset` calls; with
/// `reset_event` set, `reset(System)` asks for an event that many cycles later.
#[derive(Default)]
struct Probe {
    events: u32,
    resets: u32,
    reset_event: Option<u64>,
}

impl Device for Probe {
    fn read(&mut self, _off: u32, _width: u8, _ctx: &mut DevCtx<'_>) -> u32 {
        0
    }

    fn write(&mut self, off: u32, _width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        match off {
            0 => ctx.set_irq(5, v != 0),
            4 => ctx.schedule_at(ctx.now + u64::from(v)),
            8 => ctx.pulse_irq(6),
            _ => {}
        }
    }

    fn event(&mut self, ctx: &mut DevCtx<'_>) {
        self.events += 1;
        ctx.set_irq(5, true);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        self.resets += 1;
        if let (ResetKind::System, Some(d)) = (kind, self.reset_event) {
            ctx.schedule_at(ctx.now + d);
        }
    }

    fn save(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load(&mut self, _state: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

/// One item of a test program.
#[derive(Clone, Copy)]
enum I {
    /// A 16-bit instruction.
    H(u16),
    /// `ldr rd, [pc, #..]` of the literal at payload offset `lit`.
    Ld(u16, u32),
    /// `bne` to payload offset `to`.
    Bne(u32),
}

impl Payload {
    /// A payload with the literal pool, both IRQ handlers and `prog` as reset handler.
    fn device_test(prog: &[I]) -> Self {
        let mut p = Payload::new();
        p.word(L_PROBE, PROBE)
            .word(L_ISER0, NVIC_ISER0)
            .word(L_AIRCR, AIRCR)
            .word(L_KEY, SYSRESETREQ_KEY)
            .word(L_1000, 1000)
            .word(L_DWT, 0xE000_1000)
            .word(L_EPPB, 0xE004_3018)
            .word(L_FLAG, FLAG);
        p.vector(1, CODE).vector(IRQ5, H5).vector(IRQ6, H6);
        p.asm(
            H5,
            &[
                I::Ld(0, L_PROBE),
                I::H(0x2100), // movs r1, #0
                I::H(0x6001), // str r1, [r0]
                I::H(ADDS_R4_1),
                I::H(BX_LR),
            ],
        );
        p.asm(H6, &[I::H(0x3501), I::H(BX_LR)]); // adds r5, #1 ; bx lr
        p.asm(CODE, prog);
        p
    }

    /// Assemble `prog` at payload offset `off`.
    fn asm(&mut self, off: u32, prog: &[I]) -> &mut Self {
        for (at, ins) in (off..).step_by(2).zip(prog) {
            let hw = match *ins {
                I::H(hw) => hw,
                I::Ld(rd, lit) => {
                    let base = (at + 4) & !3;
                    0x4800 | (rd << 8) | ((lit - base) / 4) as u16
                }
                I::Bne(to) => {
                    let delta = (i64::from(to) - i64::from(at + 4)) / 2;
                    0xD100 | (delta as u16 & 0xFF)
                }
            };
            self.code(at, &[hw]);
        }
        self
    }
}

/// `str r1, [r0, #off]`.
fn str_r1_r0(off: u16) -> I {
    I::H(0x6001 | ((off / 4) << 6))
}

/// Request a system reset: `ldr r0, =AIRCR ; ldr r1, =KEY ; str r1, [r0] ; b .`.
const RESET_PROG: [I; 4] = [
    I::Ld(0, L_AIRCR),
    I::Ld(1, L_KEY),
    I::H(0x6001),
    I::H(B_SELF),
];

/// Boot `p` and add `probe` at [`PROBE`].
fn boot_with_probe(p: &Payload, probe: Probe) -> Machine {
    let mut m = p.boot();
    m.bus.add_device("probe", PROBE, 0x100, Box::new(probe));
    m
}

fn probe(m: &Machine) -> &Probe {
    m.bus.device::<Probe>("probe").expect("probe")
}

/// Step until the guest is about to execute the instruction at payload offset `off`; returns
/// the cycle count then.
fn run_to(m: &mut Machine, off: u32) -> u64 {
    for _ in 0..1_000 {
        if !m.asleep() && m.cpu.regs.pc == addr(off) {
            return m.sched.now;
        }
        m.step();
    }
    panic!("never reached {off:#x}");
}

/// Step until the first system reset (at most 1000 cycles).
fn step_to_reset(m: &mut Machine) {
    while m.resets == 0 && m.sched.now < 1_000 {
        m.step();
    }
    assert_eq!(m.resets, 1);
}

#[test]
fn flash_is_16_mib_erased() {
    let m = looping().boot();
    assert_eq!(m.bus.peek(0x00FF_FFFC, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x1000_0000 | 0x00FF_FFFC, 4), Some(0xFFFF_FFFF));
    assert_eq!(m.bus.peek(0x0100_0000, 1), None);
}

#[test]
fn sram_is_1_5_mib() {
    let m = looping().boot();
    assert!(m.bus.peek(0x2017_FFFC, 4).is_some());
    assert!(m.bus.peek(0x3017_FFFC, 4).is_some());
    assert_eq!(m.bus.peek(0x2018_0000, 1), None);
}

#[test]
fn bl2_header_at_boot_and_after_system_reset() {
    let mut m = Payload::device_test(&RESET_PROG).boot();
    assert_eq!(m.bus.peek(0x3000_0000, 4), Some(0x0004_2016));
    assert!(m.bus.poke(0x3000_0000, 4, 0));
    step_to_reset(&mut m);
    assert_eq!(m.bus.peek(0x3000_0000, 4), Some(0x0004_2016));
}

#[test]
fn device_irq_reaches_nvic() {
    // ldr r0, =PROBE ; movs r1, #1 ; str r1, [r0] ; b .
    let p = Payload::device_test(&[I::Ld(0, L_PROBE), I::H(0x2101), str_r1_r0(0), I::H(B_SELF)]);
    let mut m = boot_with_probe(&p, Probe::default());
    m.cpu.ppb_write32(NVIC_ISER0, 1 << 5);
    run_to(&mut m, CODE + 4);
    assert!(!m.cpu.scs.nvic.level(5));
    m.step();
    assert!(m.cpu.scs.nvic.level(5), "the store raised the line");
    assert_eq!(m.cpu.regs.r[4], 0);
    m.run(200);
    assert_eq!(m.cpu.regs.r[4], 1, "the handler ran once");
    assert!(!m.cpu.scs.nvic.level(5), "the handler dropped the line");
}

#[test]
fn asserted_line_repends_after_system_reset() {
    // Review Focus 1. First boot: set FLAG, raise line 5 with IRQ 5 disabled, reset. Second
    // boot: enable IRQ 5. `Cpu::reset` cleared the NVIC, but the line is still high, so the
    // handler must run.
    const SECOND: u32 = CODE + 0x1C;
    let p = Payload::device_test(&[
        I::Ld(2, L_FLAG), // 0x00 ldr r2, =FLAG
        I::H(0x6811),     // 0x02 ldr r1, [r2]
        I::H(0x2900),     // 0x04 cmp r1, #0
        I::Bne(SECOND),   // 0x06 bne second
        I::H(0x2101),     // 0x08 movs r1, #1
        I::H(0x6011),     // 0x0A str r1, [r2]
        I::Ld(0, L_PROBE),
        I::H(0x6001), // 0x0E str r1, [r0]: line 5 high
        I::Ld(0, L_AIRCR),
        I::Ld(1, L_KEY),
        I::H(0x6001), // 0x14 SYSRESETREQ
        I::H(B_SELF),
        I::H(NOP),
        I::H(NOP),
        I::Ld(0, L_ISER0), // 0x1C second:
        I::H(0x2120),      // movs r1, #32
        I::H(0x6001),      // str r1, [r0]: enable IRQ 5
        I::H(B_SELF),
    ]);
    let mut m = boot_with_probe(&p, Probe::default());
    m.run(1_000);
    assert_eq!(m.resets, 1);
    assert_eq!(m.cpu.regs.r[4], 1, "the handler ran after the reset");
}

#[test]
fn device_event_fires_at_deadline() {
    // ldr r0, =PROBE ; movs r1, #100 ; str r1, [r0, #4] ; b .
    let p = Payload::device_test(&[I::Ld(0, L_PROBE), I::H(0x2164), str_r1_r0(4), I::H(B_SELF)]);
    let mut m = boot_with_probe(&p, Probe::default());
    let t = run_to(&mut m, CODE + 4);
    while m.sched.now < t + 150 {
        m.step();
        let due = m.sched.now >= t + 100;
        assert_eq!(probe(&m).events, u32::from(due), "at {}", m.sched.now);
    }
}

#[test]
fn wfi_wakes_on_device_event() {
    // ldr r0, =PROBE ; ldr r1, =1000 ; str r1, [r0, #4] ; wfi ; b .
    let p = Payload::device_test(&[
        I::Ld(0, L_PROBE),
        I::Ld(1, L_1000),
        str_r1_r0(4),
        I::H(WFI),
        I::H(B_SELF),
    ]);
    let mut m = boot_with_probe(&p, Probe::default());
    m.cpu.ppb_write32(NVIC_ISER0, 1 << 5);
    let mut t = None;
    let mut wake = None;
    let mut entry = None;
    m.run_with(10_000, |m| {
        let pc = m.cpu.regs.pc;
        let now = Some(m.sched.now);
        if pc == addr(CODE + 4) {
            t = now;
        }
        // The first step after the `wfi` is the exception entry.
        if pc == addr(CODE + 8) && wake.is_none() {
            wake = now;
        }
        if pc == addr(H5) && entry.is_none() {
            entry = now;
        }
        ControlFlow::Continue(())
    });
    assert_eq!(m.cpu.regs.r[4], 1, "the handler ran");
    let t = t.expect("store ran");
    let (wake, entry) = (wake.expect("woke"), entry.expect("handler ran"));
    assert!((t + 1000..=t + 1003).contains(&wake), "t {t}, wake {wake}");
    // Exception entry takes 12 cycles (`armv8m` ENTRY_CYCLES).
    assert_eq!(entry, wake + 12, "handler's first instruction");

    // Without the store nothing can wake the CPU.
    let p = Payload::device_test(&[I::H(WFI), I::H(B_SELF)]);
    let mut m = boot_with_probe(&p, Probe::default());
    m.cpu.ppb_write32(NVIC_ISER0, 1 << 5);
    assert_eq!(m.run(10_000), StopReason::Idle);
}

#[test]
fn event_rescheduled_in_reset_survives_system_reset() {
    let p = Payload::device_test(&RESET_PROG);
    let probe_dev = Probe {
        reset_event: Some(500),
        ..Probe::default()
    };
    let mut m = boot_with_probe(&p, probe_dev);
    let t = run_to(&mut m, CODE + 4);
    step_to_reset(&mut m);
    // The program requests a reset on every boot: spin in its `b .` instead.
    m.cpu.regs.pc = addr(CODE + 6);
    while m.sched.now < t + 510 {
        if m.sched.now < t + 500 {
            assert_eq!(probe(&m).events, 0, "early at {}", m.sched.now);
        }
        m.step();
    }
    assert_eq!(m.resets, 1);
    assert_eq!(probe(&m).events, 1, "the event reached the new scheduler");
}

#[test]
fn pulse_pends_once() {
    // ldr r0, =PROBE ; movs r1, #1 ; str r1, [r0, #8] ; b .
    let p = Payload::device_test(&[I::Ld(0, L_PROBE), I::H(0x2101), str_r1_r0(8), I::H(B_SELF)]);
    let mut m = boot_with_probe(&p, Probe::default());
    m.cpu.ppb_write32(NVIC_ISER0, 1 << 6);
    m.run(1_000);
    assert_eq!(m.cpu.regs.r[5], 1);
}

#[test]
fn ppb_misses_and_eppb_land_in_unmapped_log() {
    // ldr r0, =0xE000_1000 ; ldr r1, [r0] ; ldr r0, =0xE004_3018 ; ldr r1, [r0] ; b .
    let mut m = Payload::device_test(&[
        I::Ld(0, L_DWT),
        I::H(0x6801),
        I::Ld(0, L_EPPB),
        I::H(0x6801),
        I::H(B_SELF),
    ])
    .boot();
    m.run(100);
    let log: Vec<_> = m
        .bus
        .log
        .entries()
        .map(|(a, _)| (a.addr, a.kind, a.pc))
        .collect();
    assert!(
        log.contains(&(0xE000_1000, UnmappedKind::Read, addr(CODE + 2))),
        "{log:x?}"
    );
    assert!(
        log.contains(&(0xE004_3018, UnmappedKind::Read, addr(CODE + 6))),
        "{log:x?}"
    );
}

#[test]
fn devices_see_system_reset() {
    let mut m = boot_with_probe(&Payload::device_test(&RESET_PROG), Probe::default());
    step_to_reset(&mut m);
    assert_eq!(probe(&m).resets, 1);
}

#[test]
fn uart_host_api() {
    const L_UART0: u32 = LIT + 0x20;
    // ldr r0, =UART0 ; movs r1, #'A' ; str r1, [r0] (THR) ; ldr r2, [r0, #0x14] (LSR) ;
    // ldr r3, [r0] (RBR) ; b .
    let mut p = Payload::device_test(&[
        I::Ld(0, L_UART0),
        I::H(0x2141),
        I::H(0x6001),
        I::H(0x6942),
        I::H(0x6803),
        I::H(B_SELF),
    ]);
    p.word(L_UART0, 0x4010_6000);
    let mut m = p.boot();
    m.inject_uart_rx(UartPort::Uart0, b"z");
    m.run(100);
    assert_eq!(m.take_uart_tx(UartPort::Uart0), b"A");
    assert_eq!(m.take_uart_tx(UartPort::Uart0), b"");
    assert_eq!(m.take_uart_tx(UartPort::Uart2), b"");
    assert_eq!(m.cpu.regs.r[2], 0x61);
    assert_eq!(m.cpu.regs.r[3], u32::from(b'z'));
}

#[test]
fn usb_cc_host_api() {
    const L_USBTC: u32 = LIT + 0x20;
    // ldr r0, =USBTC ; ldr r2, [r0, #0x1C] (CC) ; b .
    let mut p = Payload::device_test(&[I::Ld(0, L_USBTC), I::H(0x69C2), I::H(B_SELF)]);
    p.word(L_USBTC, 0x4010_C000);
    let mut m = p.boot();
    m.set_usb_cc(1, 2);
    m.run(100);
    assert_eq!(m.cpu.regs.r[2], 0x900);
    let tc = m.bus.device::<evo_hw::dev::usb::UsbTypeC>("usbtc");
    assert_eq!(tc.map(|t| t.cc()), Some((1, 2)));
}

/// An erased part with 0xA5 at flash offset 0x9_0000 (the ITS/PS area, outside both image
/// slots) and 0x12 in security-register byte 0.
fn nv_with_marks(fill: u8) -> evo_core::NvState {
    let mut flash = vec![fill; evo_hw::nor::NOR_SIZE];
    flash[0x9_0000] = 0xA5;
    let mut secregs = vec![0xFF; evo_hw::nor::SECREGS_LEN];
    secregs[0] = 0x12;
    evo_core::NvState::new(flash, secregs).expect("sizes")
}

#[test]
fn nv_roundtrip_keeps_data_outside_slots() {
    let b = looping().bundle();
    let m = Machine::from_bundle_nv(&b, &Layout::EVO_7_1, Some(nv_with_marks(0xFF))).expect("boot");
    assert_eq!(m.bus.peek(0x9_0000, 1), Some(0xA5));
    assert_eq!(m.bus.peek(0x1009_0000, 1), Some(0xA5));
    let nv = m.nv_state();
    assert_eq!(nv.flash()[0x9_0000], 0xA5);
    assert_eq!(nv.secregs()[0], 0x12);
    assert_eq!(nv.secregs()[1], 0xFF);
    // The bundle's images are in the saved flash too.
    assert_eq!(
        nv.flash()[0x000B_0000..0x000B_0004],
        0x96f3_b83du32.to_le_bytes()
    );

    // `from_bundle` starts from an erased part.
    let m = Machine::from_bundle(&b, &Layout::EVO_7_1).expect("boot");
    assert_eq!(m.bus.peek(0x9_0000, 1), Some(0xFF));
    assert!(m.nv_state().secregs().iter().all(|&b| b == 0xFF));
}

#[test]
fn bundle_images_win_over_nv() {
    let b = looping().bundle();
    let m = Machine::from_bundle_nv(&b, &Layout::EVO_7_1, Some(nv_with_marks(0x00))).expect("boot");
    assert_eq!(
        m.bus.peek(0x000B_0000, 4),
        Some(0x96f3_b83d),
        "MCUboot magic"
    );
    assert_eq!(m.bus.peek(0x100B_0400, 4), Some(SP_INIT));
    // Outside the images the saved bytes stay.
    assert_eq!(m.bus.peek(0x0000_0000, 4), Some(0));
    assert_eq!(m.bus.peek(0x9_0000, 1), Some(0xA5));
}

#[test]
fn gpio_input_irq_wakes_sleeping_cpu() {
    const GPIO: u32 = 0x4010_5000;
    const L_GPIO: u32 = LIT + 0x20;
    const L_PIN9: u32 = LIT + 0x24;
    const L_IRQ24: u32 = LIT + 0x28;
    /// IRQ 24 handler: `ldr r0, =GPIO ; ldr r1, =1 << 9 ; str r1, [r0, #0x30] (INT_CLR) ;
    /// adds r4, #1 ; bx lr`.
    const H24: u32 = 0x2B0;
    // ldr r0, =GPIO ; ldr r1, =1 << 9 ; str r1, [r0, #0x38] (INT_BOTH) ;
    // str r1, [r0, #0x20] (INT_EN) ; ldr r0, =ISER0 ; ldr r1, =1 << 24 ; str r1, [r0] ;
    // wfi ; b .
    let mut p = Payload::device_test(&[
        I::Ld(0, L_GPIO),
        I::Ld(1, L_PIN9),
        str_r1_r0(0x38),
        str_r1_r0(0x20),
        I::Ld(0, L_ISER0),
        I::Ld(1, L_IRQ24),
        I::H(0x6001),
        I::H(WFI),
        I::H(B_SELF),
    ]);
    p.word(L_GPIO, GPIO)
        .word(L_PIN9, 1 << 9)
        .word(L_IRQ24, 1 << 24)
        .vector(IRQ0 + 24, H24);
    p.asm(
        H24,
        &[
            I::Ld(0, L_GPIO),
            I::Ld(1, L_PIN9),
            str_r1_r0(0x30),
            I::H(ADDS_R4_1),
            I::H(BX_LR),
        ],
    );
    let mut m = p.boot();
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert_eq!(m.cpu.regs.r[4], 0);
    m.set_gpio_input(9, Some(true));
    m.run(1_000);
    assert_eq!(m.cpu.regs.r[4], 1, "the handler ran once");
    assert!(!m.cpu.scs.nvic.level(24), "the handler cleared the latch");
    // The falling edge interrupts again.
    m.set_gpio_input(9, Some(false));
    m.run(1_000);
    assert_eq!(m.cpu.regs.r[4], 2);
    assert!(!m.take_power_off());
}

#[test]
fn gpio_power_off_host_api() {
    const L_GPIO: u32 = LIT + 0x20;
    // ldr r0, =GPIO ; movs r1, #2 ; str r1, [r0, #8] (DIR) ; str r1, [r0, #0x10] (SET) ;
    // str r1, [r0, #0x14] (CLR) ; b .
    let mut p = Payload::device_test(&[
        I::Ld(0, L_GPIO),
        I::H(0x2102),
        str_r1_r0(0x08),
        str_r1_r0(0x10),
        str_r1_r0(0x14),
        I::H(B_SELF),
    ]);
    p.word(L_GPIO, 0x4010_5000);
    let mut m = p.boot();
    m.run(100);
    assert!(m.take_power_off());
    assert!(!m.take_power_off());
}

#[test]
fn lcdc_vcomp_irq_and_screen() {
    const LCDC: u32 = 0x4000_4000;
    const FB: u32 = 0x2004_8000;
    const L_LCDC: u32 = LIT + 0x20;
    const L_TIM0: u32 = LIT + 0x24;
    const L_TIM1: u32 = LIT + 0x28;
    const L_FB: u32 = LIT + 0x2C;
    const L_CTRL: u32 = LIT + 0x30;
    const L_IRQ14: u32 = LIT + 0x34;
    /// IRQ 14 handler: `ldr r0, =LCDC ; movs r1, #8 ; str r1, [r0, #0x28] (ICR) ;
    /// adds r4, #1 ; bx lr`.
    const H14: u32 = 0x2C0;
    // Program the firmware's 240x320 RGB565 BGR setup, IMSC = VComp, enable IRQ 14, wfi.
    let mut p = Payload::device_test(&[
        I::Ld(0, L_LCDC),
        I::Ld(1, L_TIM0),
        str_r1_r0(0x00),
        I::Ld(1, L_TIM1),
        str_r1_r0(0x04),
        I::Ld(1, L_FB),
        str_r1_r0(0x10),
        I::H(0x2108), // movs r1, #8
        str_r1_r0(0x1C),
        I::Ld(1, L_CTRL),
        str_r1_r0(0x18),
        I::Ld(0, L_ISER0),
        I::Ld(1, L_IRQ14),
        I::H(0x6001),
        I::H(WFI),
        I::H(B_SELF),
    ]);
    p.word(L_LCDC, LCDC)
        .word(L_TIM0, 0x0A0A_0A38)
        .word(L_TIM1, 0x042D_093F)
        .word(L_FB, FB)
        .word(L_CTRL, 0x312D | 0x800)
        .word(L_IRQ14, 1 << 14)
        .vector(IRQ0 + 14, H14);
    p.asm(
        H14,
        &[
            I::Ld(0, L_LCDC),
            I::H(0x2108),
            str_r1_r0(0x28),
            I::H(ADDS_R4_1),
            I::H(BX_LR),
        ],
    );
    let mut m = p.boot();
    assert_eq!(m.screen(), None, "LCD off at power-on");
    assert!(m.bus.poke(FB, 2, 0xF800));
    m.run(2_000_000);
    assert_eq!(m.cpu.regs.r[4], 0, "no VComp before the first frame");
    let f: evo_core::Frame = m.screen().expect("LCD on");
    assert_eq!((f.width, f.height), (320, 240));
    assert_eq!(&f.rgb[..3], &[0, 0, 255]);
    m.run(1_000_000);
    assert_eq!(
        m.cpu.regs.r[4], 1,
        "VComp at 2_600_000 ran the handler once"
    );
    assert!(
        !m.cpu.scs.nvic.level(14),
        "the handler cleared the interrupt"
    );
}

#[test]
fn set_key_reaches_keypad() {
    let mut m = sleeper().boot();
    m.cpu.ppb_write32(NVIC_ISER0, 1); // IRQ 0: the ON key
    assert_eq!(m.run(1_000), StopReason::Idle);
    assert_eq!(m.cpu.regs.r[4], 0);
    m.set_key(Key::On, true);
    m.run(1_000);
    assert_eq!(m.cpu.regs.r[4], 1, "the ON-key handler ran once");
    m.set_key(Key::On, false);
    m.run(1_000);
    assert_eq!(m.cpu.regs.r[4], 1, "release raises nothing");
    m.set_key(Key::Matrix { row: 2, col: 5 }, true);
    let rows = m.bus.device::<Keypad>("keypad").expect("keypad");
    assert!(rows.pressed(Key::Matrix { row: 2, col: 5 }));
}

#[test]
fn set_key_after_sleep_uses_current_time() {
    let mut m = sleeper().boot();
    m.cpu.ppb_write32(SYST_RVR, 999_999);
    m.cpu.ppb_write32(SYST_CSR, 0b111); // ENABLE, TICKINT, CLKSOURCE
    assert_eq!(m.run(500_000), StopReason::CycleLimit);
    assert!(m.asleep());
    assert_eq!(m.sched.now, 500_000);
    assert_eq!(m.cpu.regs.r[4], 0, "no SysTick yet");
    m.set_key(Key::Matrix { row: 0, col: 0 }, true);
    assert_eq!(m.sched.next_deadline(), Some(500_000 + SCAN_CYCLES));
}
