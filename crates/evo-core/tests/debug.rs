//! `Machine` debugging: watchpoints that stop the run, and run goals (step over, step out,
//! run to).
//!
//! Every test boots a synthetic bundle whose secure image is a hand-assembled Thumb program,
//! placed by `Layout::EVO_7_1` like the real OS. The helpers are copied from `tests/machine.rs`
//! (test files cannot share private helpers).

use evo_core::{
    AccessDir, CPU_HZ, Machine, PowerState, RAM_HEADER_LEN, RAM_MAGIC, RAM_VERSION, RamError,
    RegError, RunGoal, StopReason, UartPort, WatchError, WatchKinds, WatchRange,
};
use evo_image::testutil::secure_image_bundle;
use evo_image::{Bundle, Layout};

/// `VTOR_S` for `Layout::EVO_7_1`: Secure alias of slot 0x000B_0000 plus the 0x400 header.
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the code, after room for 128 vectors.
const CODE: u32 = 0x200;
const SP_INIT: u32 = 0x3000_0800;

#[allow(dead_code)]
const NOP: u16 = 0xBF00;
#[allow(dead_code)]
const BX_LR: u16 = 0x4770;
#[allow(dead_code)]
const B_SELF: u16 = 0xE7FE;

/// Address of payload offset `off` (Secure alias).
fn addr(off: u32) -> u32 {
    VTOR + off
}

/// The secure image's payload: a vector table at offset 0, code from `CODE` on.
struct Payload(Vec<u8>);

/// One item of a test program.
#[derive(Clone, Copy)]
#[allow(dead_code)]
enum I {
    /// A 16-bit instruction.
    H(u16),
    /// `ldr rd, [pc, #..]` of the literal at payload offset `lit`.
    Ld(u16, u32),
    /// `bne` to payload offset `to`.
    Bne(u32),
}

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

    fn bundle(&self) -> Bundle {
        Bundle::from_bytes(&secure_image_bundle(self.0.clone())).expect("synthetic bundle")
    }

    fn boot(&self) -> Machine {
        Machine::from_bundle(&self.bundle(), &Layout::EVO_7_1).expect("boot")
    }
}

/// Where `STORE` stores (Secure SRAM, below the stack).
const WORD: u32 = 0x3000_0200;
/// The `str r1, [r0]` of `STORE`.
const STORE_PC: u32 = 0x100B_0606;
/// The loop head (`adds r1, #1`) of `STORE`.
const LOOP_HEAD: u32 = 0x100B_0604;

/// `ldr r0, =WORD ; movs r1, #0 ; loop: adds r1, #1 ; str r1, [r0] ; b loop`.
fn store() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE).word(0x300, WORD).asm(
        CODE,
        &[
            I::Ld(0, 0x300), // ldr r0, =0x30000200
            I::H(0x2100),    // movs r1, #0
            I::H(0x3101),    // 0x204 adds r1, #1
            I::H(0x6001),    // 0x206 str r1, [r0]
            I::H(0xE7FC),    // 0x208 b 0x204
        ],
    );
    p
}

fn kinds(read: bool, write: bool, exec: bool) -> WatchKinds {
    WatchKinds { read, write, exec }
}

fn range(start: u32, len: u32, kinds: WatchKinds) -> WatchRange {
    WatchRange { start, len, kinds }
}

fn store_hit(value: u32) -> StopReason {
    StopReason::Watchpoint {
        addr: WORD,
        dir: AccessDir::Write,
        value,
        pc: STORE_PC,
    }
}

fn exec_hit() -> StopReason {
    StopReason::Watchpoint {
        addr: LOOP_HEAD,
        dir: AccessDir::Exec,
        value: 0,
        pc: LOOP_HEAD,
    }
}

#[test]
fn data_watch_stops_after_the_store() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    assert_eq!(m.run(1000), store_hit(1));
    assert_eq!(m.cpu.regs.pc, 0x100B_0608, "the store has completed");
    assert_eq!(m.bus.peek(WORD, 4), Some(1));
}

#[test]
fn continue_after_data_watch_moves_on() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    assert_eq!(m.run(1000), store_hit(1));
    assert_eq!(m.run(1000), store_hit(2));
    assert_eq!(m.run(1000), store_hit(3));
}

#[test]
fn exec_watch_resumes_like_a_breakpoint() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(LOOP_HEAD, 2, kinds(false, false, true))])
        .unwrap();
    assert_eq!(m.run(1000), exec_hit());
    assert_eq!(m.cpu.regs.pc, LOOP_HEAD, "the instruction has not run");
    assert_eq!(m.cpu.regs.r[1], 0);
    assert_eq!(m.run(1000), exec_hit());
    assert_eq!(m.cpu.regs.r[1], 1);
}

#[test]
fn read_watch_reports_the_value_read() {
    // ldr r0, =WORD ; ldr r1, [r0] ; b .
    let mut p = Payload::new();
    p.vector(1, CODE)
        .word(0x300, WORD)
        .asm(CODE, &[I::Ld(0, 0x300), I::H(0x6801), I::H(B_SELF)]);
    let mut m = p.boot();
    assert!(m.bus.poke(WORD, 4, 0x1234_5678));
    m.set_watchpoints(vec![range(WORD + 2, 1, kinds(true, false, false))])
        .unwrap();
    assert_eq!(
        m.run(1000),
        StopReason::Watchpoint {
            addr: WORD,
            dir: AccessDir::Read,
            value: 0x1234_5678,
            pc: addr(CODE + 2),
        }
    );
    assert_eq!(m.cpu.regs.r[1], 0x1234_5678);
}

#[test]
fn step_reports_a_data_hit() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    // ldr, movs, adds.
    for _ in 0..3 {
        assert_eq!(m.step(), StopReason::Stepped);
    }
    assert_eq!(m.cpu.regs.pc, STORE_PC);
    assert_eq!(m.step(), store_hit(1));
    assert_eq!(m.step(), StopReason::Stepped, "b loop");
}

#[test]
fn step_ignores_exec_watch() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(addr(CODE), 0x10, kinds(false, false, true))])
        .unwrap();
    for _ in 0..5 {
        assert_eq!(m.step(), StopReason::Stepped);
    }
}

#[test]
fn watchpoints_read_back_and_invalid_sets_change_nothing() {
    let mut m = store().boot();
    let set = vec![
        range(WORD, 4, kinds(false, true, false)),
        range(LOOP_HEAD, 2, kinds(false, false, true)),
    ];
    m.set_watchpoints(set.clone()).unwrap();
    assert_eq!(m.watchpoints(), set);
    assert_eq!(
        m.set_watchpoints(vec![
            range(0x100, 4, kinds(true, false, false)),
            range(0x200, 0, kinds(true, false, false)),
        ]),
        Err(WatchError::Empty)
    );
    assert_eq!(m.watchpoints(), set);
    // The exec range still stops the run.
    assert_eq!(m.run(1000), exec_hit());
}

#[test]
fn exec_watch_off_after_empty_set() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(LOOP_HEAD, 2, kinds(false, false, true))])
        .unwrap();
    assert_eq!(m.run(1000), exec_hit());
    m.set_watchpoints(vec![]).unwrap();
    assert!(m.watchpoints().is_empty());
    assert_eq!(m.run(1000), StopReason::CycleLimit);
}

#[test]
fn breakpoint_and_exec_watch_on_one_pc_step_over_once() {
    let mut m = store().boot();
    m.breakpoints.insert(LOOP_HEAD);
    m.set_watchpoints(vec![range(LOOP_HEAD, 2, kinds(false, false, true))])
        .unwrap();
    assert_eq!(m.run(1000), StopReason::Breakpoint(LOOP_HEAD));
    assert_eq!(m.cpu.regs.r[1], 0);
    // The resumed run executes the instruction; the next pass stops again.
    assert_eq!(m.run(1000), StopReason::Breakpoint(LOOP_HEAD));
    assert_eq!(m.cpu.regs.r[1], 1);
}

#[test]
fn batching_disabled_while_watching() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(0x3000_0400, 4, kinds(false, true, false))])
        .unwrap();
    let before = m.batched_runs();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert_eq!(m.batched_runs(), before, "watching: checked path");

    m.set_watchpoints(vec![]).unwrap();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    let after = m.batched_runs();
    assert!(after > before, "nothing watched: batches again");

    m.set_watchpoints(vec![range(0x100, 2, kinds(false, false, true))])
        .unwrap();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert_eq!(m.batched_runs(), after, "exec range: checked path");
    m.set_watchpoints(vec![]).unwrap();

    m.bus.set_bus_monitor(Some((0x3000_0000, 0x1000)));
    let before = m.batched_runs();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert_eq!(m.batched_runs(), before, "bus monitor: checked path");
    assert!(!m.bus.bus_records().is_empty());

    m.bus.set_bus_monitor(None);
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert!(m.batched_runs() > before);
}

#[test]
fn empty_watch_set_keeps_results_identical() {
    let run = |watch: bool| {
        let mut m = store().boot();
        if watch {
            m.set_watchpoints(vec![]).unwrap();
        }
        assert_eq!(m.run(1_000_000), StopReason::CycleLimit);
        (
            m.sched.now,
            m.cpu.regs.r[0],
            m.cpu.regs.r[1],
            m.bus.peek(WORD, 4),
            m.take_uart_tx(UartPort::Uart0),
        )
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn watched_run_matches_unwatched_run() {
    // A watchpoint that never matches takes the checked path, with the same outcome.
    let run = |watch: bool| {
        let mut m = store().boot();
        if watch {
            m.set_watchpoints(vec![range(0x3000_0400, 4, kinds(true, true, true))])
                .unwrap();
        }
        assert_eq!(m.run(100_000), StopReason::CycleLimit);
        (m.sched.now, m.cpu.regs.r[1], m.bus.peek(WORD, 4))
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn power_cycle_clears_watchpoints_like_breakpoints() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    assert_eq!(m.run(1000), store_hit(1));
    m.power_cycle();
    assert!(m.watchpoints().is_empty());
    assert_eq!(m.run(1000), StopReason::CycleLimit);
}

#[test]
fn watchpoints_survive_load_state() {
    let mut m = store().boot();
    let snap = m.save_state();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    assert_eq!(m.run(1000), store_hit(1));
    m.load_state(&snap).unwrap();
    assert_eq!(m.run(1000), store_hit(1), "restored to reset; same stop");
}

// ---- Goals: step over, step out, run to ----

/// `bl 0x220 ; nop ; b .` with `0x220: movs r2, #100 ; loop: subs r2, #1 ; bne loop ; bx lr`.
fn call() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE)
        .asm(
            CODE,
            &[
                I::H(0xF000),
                I::H(0xF80E), // bl 0x220
                I::H(NOP),    // 0x204
                I::H(B_SELF), // 0x206
            ],
        )
        .asm(
            0x220,
            &[
                I::H(0x2264),  // movs r2, #100
                I::H(0x3A01),  // 0x222 subs r2, #1
                I::Bne(0x222), // 0x224
                I::H(BX_LR),   // 0x226
            ],
        );
    p
}

/// `movs r2, #3 ; bl f ; b .` with `f: push {lr} ; subs r2, #1 ; beq out ; bl f ; out: pop {pc}`.
fn recurse() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE)
        .asm(
            CODE,
            &[
                I::H(0x2203), // movs r2, #3
                I::H(0xF000),
                I::H(0xF81D), // 0x202 bl 0x240
                I::H(B_SELF), // 0x206
            ],
        )
        .asm(
            0x240,
            &[
                I::H(0xB500), // push {lr}
                I::H(0x3A01), // 0x242 subs r2, #1
                I::H(0xD001), // 0x244 beq 0x24A
                I::H(0xF7FF),
                I::H(0xFFFB), // 0x246 bl 0x240
                I::H(0xBD00), // 0x24A pop {pc}
            ],
        );
    p
}

/// Enable and pend IRQ0, then `b .`; the handler counts r3 down from 5 and returns.
fn irq() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE)
        .vector(16, 0x280)
        .word(0x300, 0xE000_E100) // ISER0
        .word(0x304, 0xE000_E200) // ISPR0
        .asm(
            CODE,
            &[
                I::Ld(0, 0x300),
                I::H(0x2101), // movs r1, #1
                I::H(0x6001), // str: enable IRQ0
                I::Ld(0, 0x304),
                I::H(0x6001), // 0x208 str: pend IRQ0
                I::H(B_SELF), // 0x20A
            ],
        )
        .asm(
            0x280,
            &[
                I::H(0x2305),  // movs r3, #5
                I::H(0x3B01),  // 0x282 subs r3, #1
                I::Bne(0x282), // 0x284
                I::H(BX_LR),   // 0x286
            ],
        );
    p
}

const BUDGET: u64 = 1_000_000;

#[test]
fn step_over_a_call_stops_after_it() {
    let mut m = call().boot();
    assert_eq!(m.cpu.regs.pc, addr(CODE));
    assert_eq!(m.arm_step_over(), None);
    assert!(matches!(
        m.goal(),
        Some(RunGoal::Return {
            pc: 0x100B_0604,
            ..
        })
    ));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0604 });
    assert_eq!(m.cpu.regs.r[2], 0, "the call ran to its end");
    assert_eq!(m.goal(), None, "reaching the goal disarms it");
    assert_eq!(m.run(1000), StopReason::CycleLimit);
}

#[test]
fn step_over_a_plain_instruction_steps_once() {
    let mut m = call().boot();
    m.arm_run_to(addr(0x220));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0620 });
    assert_eq!(m.arm_step_over(), Some(StopReason::Stepped));
    assert_eq!(m.cpu.regs.pc, 0x100B_0622);
    assert_eq!(m.cpu.regs.r[2], 100);
    assert_eq!(m.goal(), None);
}

#[test]
fn step_over_recursion_waits_for_the_outer_return() {
    let mut m = recurse().boot();
    m.arm_run_to(addr(0x246));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0646 });
    assert_eq!(m.cpu.regs.r[2], 2, "depth 1");
    let sp = m.cpu.regs.sp();
    assert_eq!(m.arm_step_over(), None);
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_064A });
    assert_eq!(m.cpu.regs.sp(), sp, "the outer return, not a deeper one");
    assert_eq!(m.cpu.regs.r[2], 0);
}

#[test]
fn step_out_returns_to_lr() {
    let mut m = call().boot();
    m.arm_run_to(addr(0x222));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0622 });
    m.arm_step_out();
    assert!(matches!(
        m.goal(),
        Some(RunGoal::Return {
            pc: 0x100B_0604,
            ..
        })
    ));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0604 });
    assert_eq!(m.cpu.regs.r[2], 0);
}

#[test]
fn step_out_of_a_handler_waits_for_exception_return() {
    let mut m = irq().boot();
    m.arm_run_to(addr(0x282));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0682 });
    assert_eq!(m.cpu.active_exceptions(), 1);
    m.arm_step_out();
    assert_eq!(m.goal(), Some(RunGoal::ExceptionReturn { depth: 1 }));
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_060A });
    assert_eq!(m.cpu.active_exceptions(), 0);
    assert_eq!(m.cpu.regs.r[3], 0);
}

#[test]
fn run_to_stops_before_the_address() {
    let mut m = store().boot();
    m.arm_run_to(0x100B_0607);
    assert_eq!(
        m.goal(),
        Some(RunGoal::Address(STORE_PC)),
        "Thumb bit cleared"
    );
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: STORE_PC });
    assert_eq!(m.cpu.regs.pc, STORE_PC);
    assert_eq!(m.bus.peek(WORD, 4), Some(0), "the store has not run");
}

#[test]
fn run_to_the_resumed_address_comes_back_round() {
    let mut m = store().boot();
    m.arm_run_to(STORE_PC);
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: STORE_PC });
    m.arm_run_to(STORE_PC);
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: STORE_PC });
    assert_eq!(m.bus.peek(WORD, 4), Some(1), "one pass of the loop");
}

#[test]
fn breakpoint_before_goal_keeps_goal_armed() {
    let mut m = call().boot();
    // `bx lr`, which runs once (the brief's 0x100B_0622 is the loop's `subs`, hit 100 times).
    m.breakpoints.insert(0x100B_0626);
    assert_eq!(m.arm_step_over(), None);
    assert_eq!(m.run(BUDGET), StopReason::Breakpoint(0x100B_0626));
    assert!(m.goal().is_some());
    assert_eq!(m.run(BUDGET), StopReason::Goal { pc: 0x100B_0604 });
    assert_eq!(m.goal(), None);
}

#[test]
fn watchpoint_before_goal_keeps_goal_armed() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    m.arm_run_to(addr(0x300));
    assert_eq!(m.run(1000), store_hit(1));
    assert!(m.goal().is_some());
}

#[test]
fn goal_is_disarmed_by_disarm() {
    let mut m = store().boot();
    m.arm_run_to(STORE_PC);
    m.disarm();
    assert_eq!(m.goal(), None);
    assert_eq!(m.run(1000), StopReason::CycleLimit);
}

#[test]
fn unreached_goal_returns_at_budget_and_stays_armed() {
    let mut m = store().boot();
    m.arm_run_to(addr(0x300));
    let start = m.sched.now;
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    let ran = m.sched.now - start;
    assert!((1000..1010).contains(&ran), "ran {ran} cycles");
    assert_eq!(m.goal(), Some(RunGoal::Address(addr(0x300))));
}

#[test]
fn goal_turns_batching_off() {
    let mut m = store().boot();
    m.arm_run_to(addr(0x300));
    let before = m.batched_runs();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert_eq!(m.batched_runs(), before);
    m.disarm();
    assert_eq!(m.run(1000), StopReason::CycleLimit);
    assert!(m.batched_runs() > before);
}

#[test]
fn load_state_and_power_cycle_clear_the_goal() {
    let mut m = store().boot();
    let snap = m.save_state();
    m.arm_run_to(STORE_PC);
    m.load_state(&snap).unwrap();
    assert_eq!(m.goal(), None);
    m.arm_run_to(STORE_PC);
    m.power_cycle();
    assert_eq!(m.goal(), None);
    assert_eq!(m.run(1000), StopReason::CycleLimit);
}

// ---- device and SCS inspection ----

#[test]
fn peripherals_lists_devices_in_order() {
    let m = store().boot();
    let devs = m.peripherals();
    let names: Vec<_> = devs.iter().map(|d| d.name).collect();
    assert_eq!(
        names,
        [
            "mpc_sram0",
            "mpc_sram1",
            "mpc_sram2",
            "mpc_flash",
            "cru",
            "spctrl",
            "timer",
            "uart0",
            "uart2",
            "spif",
            "usb",
            "gpio",
            "lcdc",
            "pwm",
            "keypad",
            "rtc",
            "systick_s",
            "systick_ns",
            "nvic"
        ]
    );
    let gpio = devs.iter().find(|d| d.name == "gpio").unwrap();
    assert_eq!(gpio.base, 0x4010_5000);
    let data_in = gpio.regs.iter().find(|r| r.name == "data_in").unwrap();
    assert_eq!(data_in.value & (1 << 9 | 1 << 11), 1 << 9 | 1 << 11);
    let systick = devs.iter().find(|d| d.name == "systick_s").unwrap();
    assert_eq!(systick.base, 0xE000_E010);
    let rows: Vec<_> = systick
        .regs
        .iter()
        .map(|r| (r.name.as_str(), r.offset))
        .collect();
    assert_eq!(rows, [("csr", 0x0), ("rvr", 0x4), ("cvr", 0x8)]);
}

#[test]
fn nvic_rows_show_pending_irq() {
    let mut m = store().boot();
    m.cpu.set_irq_level(13, true);
    let devs = m.peripherals();
    let nvic = devs.iter().find(|d| d.name == "nvic").unwrap();
    assert_eq!(nvic.base, 0xE000_E100);
    let row = nvic.regs.iter().find(|r| r.name == "irq13").unwrap();
    assert_eq!(row.offset, 13);
    assert_eq!(row.value, 1 << 1 | 1 << 3);
    assert_eq!(row.note.as_deref(), Some("pending level"));
    // Only IRQs with something set are listed.
    assert!(nvic.regs.iter().all(|r| r.value != 0));
}

#[test]
fn peripherals_changes_nothing() {
    let mut m = store().boot();
    m.run(10_000);
    let before = m.save_state();
    let first = m.peripherals();
    assert_eq!(m.save_state(), before);
    assert_eq!(m.peripherals(), first);
}

// ---- RAM images, register writes, RTC set, power state ----

const SRAM_LEN: usize = 0x18_0000;
const SRAM_LAST: u32 = 0x3000_0000 + SRAM_LEN as u32 - 4;

/// A fresh STORE machine with recognisable bytes at the start and end of the SRAM.
fn poked() -> Machine {
    let mut m = store().boot();
    assert!(m.bus.poke(0x3000_0000, 4, 0x1122_3344));
    assert!(m.bus.poke(SRAM_LAST, 4, 0x5566_7788));
    m
}

fn sram_words(m: &Machine) -> [Option<u32>; 3] {
    [
        m.bus.peek(0x3000_0000, 4),
        m.bus.peek(0x3000_1000, 4),
        m.bus.peek(SRAM_LAST, 4),
    ]
}

#[test]
fn ram_image_roundtrip() {
    let mut m = poked();
    let img = m.export_ram();
    assert_eq!(img.len(), RAM_HEADER_LEN + SRAM_LEN);
    assert_eq!(img.len(), 12 + 0x18_0000);
    assert_eq!(&img[..6], RAM_MAGIC);
    assert_eq!(u16::from_le_bytes([img[6], img[7]]), RAM_VERSION);
    assert_eq!(
        u32::from_le_bytes(img[8..12].try_into().unwrap()) as usize,
        SRAM_LEN
    );
    assert_eq!(&img[12..16], &0x1122_3344u32.to_le_bytes());
    m.bus.poke(0x3000_0000, 4, 0xAAAA_AAAA);
    m.bus.poke(0x3000_1000, 4, 0xBBBB_BBBB);
    m.bus.poke(SRAM_LAST, 4, 0);
    m.import_ram(&img).unwrap();
    assert_eq!(
        sram_words(&m),
        [Some(0x1122_3344), Some(0), Some(0x5566_7788)]
    );
    assert_eq!(m.export_ram(), img);
}

#[test]
fn import_ram_is_seen_by_the_guest() {
    let mut m = store().boot();
    m.run(1000);
    let mut img = m.export_ram();
    let off = RAM_HEADER_LEN + (WORD - 0x3000_0000) as usize;
    img[off..off + 4].copy_from_slice(&0xCAFEu32.to_le_bytes());
    m.import_ram(&img).unwrap();
    assert_eq!(m.bus.peek(WORD, 4), Some(0xCAFE));
    // The Non-secure alias shows the same bytes.
    assert_eq!(m.bus.peek(WORD - 0x1000_0000, 4), Some(0xCAFE));
}

#[test]
fn import_ram_rejects_bad_magic() {
    let mut m = poked();
    let mut img = m.export_ram();
    img[0] = b'X';
    m.bus.poke(0x3000_0000, 4, 0xAAAA_AAAA);
    let before = sram_words(&m);
    assert_eq!(m.import_ram(&img), Err(RamError::BadMagic));
    assert_eq!(m.import_ram(&[]), Err(RamError::BadMagic));
    assert_eq!(m.import_ram(&img[..5]), Err(RamError::BadMagic));
    // A truncated header is not a RAM image either.
    assert_eq!(m.import_ram(b"EVORAM\x01\x00"), Err(RamError::BadMagic));
    assert_eq!(sram_words(&m), before);
    assert_eq!(RamError::BadMagic.to_string(), "not a RAM image");
}

#[test]
fn import_ram_rejects_version_2() {
    let mut m = poked();
    let mut img = m.export_ram();
    img[6..8].copy_from_slice(&2u16.to_le_bytes());
    m.bus.poke(0x3000_0000, 4, 0xAAAA_AAAA);
    let before = sram_words(&m);
    assert_eq!(m.import_ram(&img), Err(RamError::Version(2)));
    assert_eq!(sram_words(&m), before);
    assert_eq!(
        RamError::Version(2).to_string(),
        "RAM image version 2 is not supported"
    );
}

#[test]
fn import_ram_rejects_short_and_long() {
    let mut m = poked();
    let img = m.export_ram();
    m.bus.poke(0x3000_0000, 4, 0xAAAA_AAAA);
    let before = sram_words(&m);
    // Body shorter or longer than the header says.
    assert_eq!(
        m.import_ram(&img[..img.len() - 1]),
        Err(RamError::Size {
            expected: SRAM_LEN,
            found: SRAM_LEN - 1
        })
    );
    let mut long = img.clone();
    long.push(0);
    assert_eq!(
        m.import_ram(&long),
        Err(RamError::Size {
            expected: SRAM_LEN,
            found: SRAM_LEN + 1
        })
    );
    // A header (and body) for another SRAM size.
    let mut small = img[..RAM_HEADER_LEN + 0x1000].to_vec();
    small[8..12].copy_from_slice(&0x1000u32.to_le_bytes());
    assert_eq!(
        m.import_ram(&small),
        Err(RamError::Size {
            expected: SRAM_LEN,
            found: 0x1000
        })
    );
    // Header only.
    assert_eq!(
        m.import_ram(&img[..RAM_HEADER_LEN]),
        Err(RamError::Size {
            expected: SRAM_LEN,
            found: 0
        })
    );
    assert_eq!(sram_words(&m), before);
    assert_eq!(
        RamError::Size {
            expected: 2,
            found: 1
        }
        .to_string(),
        "RAM image holds 1 bytes, this machine has 2"
    );
}

#[test]
fn write_reg_sets_core_and_banked() {
    let mut m = store().boot();
    m.write_reg("pc", 0x100B_0603).unwrap();
    assert_eq!(m.cpu.regs.pc, 0x100B_0602);
    m.write_reg("r0", 7).unwrap();
    m.write_reg("r12", 12).unwrap();
    assert_eq!(m.cpu.regs.r[0], 7);
    assert_eq!(m.cpu.regs.r[12], 12);
    m.write_reg("lr", 0xFFFF_FFF9).unwrap();
    assert_eq!(m.cpu.regs.lr, 0xFFFF_FFF9);
    m.write_reg("sp", 0x3000_0700).unwrap();
    assert_eq!(m.cpu.regs.sp(), 0x3000_0700);
    assert_eq!(m.cpu.regs.msp.s, 0x3000_0700);
    m.write_reg("xpsr", 0x6100_0000).unwrap();
    assert_eq!(m.cpu.regs.xpsr.to_u32(), 0x6100_0000);
    m.write_reg("control_ns", 0b10).unwrap();
    assert_eq!(m.cpu.regs.control.ns, 0b10);
    assert_eq!(m.cpu.regs.control.s, 0);
    m.write_reg("primask_s", 1).unwrap();
    assert!(m.cpu.regs.primask.s);
    assert!(!m.cpu.regs.primask.ns);
    m.write_reg("faultmask_ns", 3).unwrap();
    assert!(m.cpu.regs.faultmask.ns);
    m.write_reg("faultmask_ns", 2).unwrap();
    assert!(!m.cpu.regs.faultmask.ns, "bools come from bit 0");
    m.write_reg("basepri_s", 0x1E0).unwrap();
    assert_eq!(m.cpu.regs.basepri.s, 0xE0);
    for (name, v) in [
        ("msp_s", 0x10),
        ("msp_ns", 0x20),
        ("psp_s", 0x30),
        ("psp_ns", 0x40),
        ("msplim_s", 0x50),
        ("msplim_ns", 0x60),
        ("psplim_s", 0x70),
        ("psplim_ns", 0x80),
        ("control_s", 0x1),
        ("basepri_ns", 0x40),
        ("primask_ns", 1),
        ("faultmask_s", 1),
    ] {
        m.write_reg(name, v).unwrap();
    }
    let r = &m.cpu.regs;
    assert_eq!(
        [
            r.msp.s,
            r.msp.ns,
            r.psp.s,
            r.psp.ns,
            r.msplim.s,
            r.msplim.ns,
            r.psplim.s,
            r.psplim.ns,
            r.control.s,
            u32::from(r.basepri.ns),
            u32::from(r.primask.ns),
            u32::from(r.faultmask.s),
        ],
        [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 1, 0x40, 1, 1
        ]
    );
}

#[test]
fn write_pc_stops_at_a_breakpoint_there() {
    // Stopped at a breakpoint, a PC write moves on: the next run does not step over the
    // breakpoint it stopped at, it stops there again.
    let mut m = store().boot();
    m.breakpoints.insert(LOOP_HEAD);
    assert_eq!(m.run(1000), StopReason::Breakpoint(LOOP_HEAD));
    m.write_reg("pc", LOOP_HEAD).unwrap();
    assert_eq!(m.run(1000), StopReason::Breakpoint(LOOP_HEAD));
    assert_eq!(m.cpu.regs.r[1], 0, "nothing ran");
}

#[test]
fn write_reg_unknown_is_err() {
    let mut m = store().boot();
    let before = m.cpu.regs.clone();
    for name in ["r13", "foo", "", "msp", "control", "PC"] {
        assert_eq!(
            m.write_reg(name, 1),
            Err(RegError::Unknown(name.to_owned()))
        );
    }
    assert_eq!(m.cpu.regs, before);
    assert_eq!(RegError::Unknown("x".into()).to_string(), "no register x");
}

/// `wfi ; b wfi`, woken by a SysTick every 2^24 cycles (handler `bx lr`): time passes in
/// big sleep jumps, so seconds of guest time cost few steps.
fn sleeper() -> Payload {
    let mut p = Payload::new();
    p.vector(1, CODE)
        .vector(15, 0x220)
        .code(CODE, &[0xBF30 /* wfi */, 0xE7FD /* b wfi */])
        .code(0x220, &[BX_LR]);
    p
}

#[test]
fn set_rtc_reads_back() {
    let mut m = sleeper().boot();
    m.cpu.ppb_write32(0xE000_E014, 0x00FF_FFFF); // SYST_RVR
    m.cpu.ppb_write32(0xE000_E010, 0b111); // SYST_CSR: ENABLE, TICKINT, CLKSOURCE
    m.run(1000);
    m.set_rtc(3600);
    assert_eq!(m.rtc_seconds(), 3600);
    let start = m.sched.now;
    while m.sched.now - start < 2 * CPU_HZ {
        m.run(2 * CPU_HZ - (m.sched.now - start));
    }
    assert_eq!(m.rtc_seconds(), 3602);
    m.set_rtc(86_401);
    assert_eq!(m.rtc_seconds(), 1);
}

#[test]
fn power_state_at_power_on() {
    let m = store().boot();
    assert_eq!(
        m.power_state(),
        PowerState {
            hold: false,
            deep_sleep: false
        }
    );
}

#[test]
fn step_over_preserves_data_hit() {
    let mut m = store().boot();
    m.set_watchpoints(vec![range(WORD, 4, kinds(false, true, false))])
        .unwrap();
    for _ in 0..3 {
        assert_eq!(m.step(), StopReason::Stepped);
    }
    assert_eq!(m.cpu.regs.pc, STORE_PC);
    assert_eq!(
        m.arm_step_over(),
        Some(StopReason::Watchpoint {
            addr: WORD,
            dir: AccessDir::Write,
            value: 1,
            pc: STORE_PC,
        })
    );
    assert_eq!(m.bus.peek(WORD, 4), Some(1), "store ran");
    assert_eq!(
        m.step(),
        StopReason::Stepped,
        "the hit is delivered exactly once"
    );
}
