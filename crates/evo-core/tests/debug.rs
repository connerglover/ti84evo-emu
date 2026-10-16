//! `Machine` debugging: watchpoints that stop the run, and run goals (step over, step out,
//! run to).
//!
//! Every test boots a synthetic bundle whose secure image is a hand-assembled Thumb program,
//! placed by `Layout::EVO_7_1` like the real OS. The helpers are copied from `tests/machine.rs`
//! (test files cannot share private helpers).

use evo_core::{
    AccessDir, Machine, RunGoal, StopReason, UartPort, WatchError, WatchKinds, WatchRange,
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
    assert!(m.arm_step_over());
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
    assert!(!m.arm_step_over());
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
    assert!(m.arm_step_over());
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
    assert!(m.arm_step_over());
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
