//! The emulated TI-84 Evo: CPU, memory map and scheduler, booted from an OS bundle.
//!
//! The memory map is provisional (refined in Plan 2): 8 MiB of flash at `FLASH_NS` and
//! `FLASH_S`, 2 MiB of SRAM at `SRAM_NS` and `SRAM_S`, and everything else unmapped and logged
//! (see `docs/hardware/memory-map.md`).
//!
//! # Time
//!
//! `sched.now` counts CPU cycles since power-on; a system reset does not restart it. Each
//! executed step advances it by the cycles the step reports and then fires the events that
//! fell due.
//!
//! # Sleep
//!
//! After WFI/WFE (or a return with `SCR.SLEEPONEXIT`) the machine is asleep unless it would
//! wake at once. While asleep the CPU is not stepped: time jumps to the next wake-up, the
//! earlier of the next scheduler deadline and the next SysTick interrupt. The CPU wakes once
//! an exception is pending, or, after WFE only, once the event register is set (e.g. by an
//! interrupt pended with `SCR.SEVONPEND`, even a disabled one); that wake-up consumes the
//! event. If no wake-up time exists the CPU would sleep forever, and `run` returns
//! [`StopReason::Idle`] at once instead of waiting out its budget. A debugger halt also ends
//! the sleep ([`Machine::wake`]).

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use armv8m::decode::is_32bit;
use armv8m::{Cpu, Sec, SleepKind, StepResult, decode16, decode32, disasm};
use evo_hw::MemoryMap;
use evo_image::{Bundle, ImageError, Layout};
use serde::{Deserialize, Serialize};

use crate::{EventKind, Scheduler};

/// Size of the flash. It holds both OS images of `Layout::EVO_7_1`.
pub const FLASH_SIZE: u32 = 8 << 20;
/// Non-secure alias of flash; flash offsets are addresses in this alias.
pub const FLASH_NS: u32 = 0x0000_0000;
/// Secure alias of flash.
pub const FLASH_S: u32 = 0x1000_0000;
/// Size of the SRAM.
pub const SRAM_SIZE: u32 = 2 << 20;
/// Non-secure alias of SRAM.
pub const SRAM_NS: u32 = 0x2000_0000;
/// Secure alias of SRAM.
pub const SRAM_S: u32 = 0x3000_0000;
/// Erased NOR flash reads as all ones.
const ERASED: u8 = 0xFF;

/// Why [`Machine::run`] or [`Machine::step`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    /// `run`: the cycle budget is used up. The CPU may be asleep, waiting for a wake-up that
    /// lies beyond the budget.
    CycleLimit,
    /// `step`: the step is done. It executed an instruction or entered an exception, or, while
    /// asleep, advanced time to the next wake-up.
    Stepped,
    /// The CPU is asleep (WFI/WFE) and nothing can wake it: no exception (or, for WFE, event)
    /// is pending, no scheduler event is scheduled and SysTick will not fire. Time stands
    /// still; only a host action (an interrupt line, a register write, [`Machine::wake`]) can
    /// change that.
    Idle,
    /// The `run_with` callback returned `ControlFlow::Break` before the instruction at the PC,
    /// which has not run.
    Halted,
    /// The next instruction is at this address in [`Machine::breakpoints`]; it has not run.
    Breakpoint(u32),
    /// The CPU locked up; `pc` is the instruction (or exception entry) it locked up on. If the
    /// CPU was already locked up, `pc` is the lockup address 0xEFFF_FFFE.
    Lockup { pc: u32 },
    /// The guest executed `BKPT #imm` at `pc` (the PC stays on it).
    Bkpt { pc: u32, imm: u8 },
}

/// The whole emulated calculator.
#[derive(Serialize, Deserialize)]
pub struct Machine {
    pub cpu: Cpu,
    pub bus: MemoryMap,
    pub sched: Scheduler,
    /// Instruction addresses (Thumb bit clear) that stop `run` before they execute.
    pub breakpoints: BTreeSet<u32>,
    /// System resets performed (AIRCR.SYSRESETREQ) since boot.
    pub resets: u64,
    /// `VTOR_S` at boot, reloaded by every system reset.
    boot_vtor: u32,
    /// Set while the CPU sleeps, with what put it to sleep.
    sleep: Option<SleepKind>,
    /// The breakpoint the last `run` stopped at, which the next `run` steps over.
    stopped_at: Option<u32>,
}

impl Machine {
    /// Build the memory map, program `b`'s images into flash per `layout` and reset the CPU
    /// into the secure image: `VTOR_S` = Secure alias of image 0's slot plus its header size.
    ///
    /// Images are placed before the map is built, so an image that does not fit in flash is an
    /// [`ImageError::DoesNotFit`], never a panic.
    pub fn from_bundle(b: &Bundle, layout: &Layout) -> Result<Self, ImageError> {
        let pkg = &b.package;
        // `Package::parse` never yields an empty package, but its fields are public.
        let secure = pkg
            .images
            .first()
            .ok_or(ImageError::BadMagic { offset: 0 })?;
        let mut flash = vec![ERASED; FLASH_SIZE as usize];
        layout.place(pkg, &mut flash, FLASH_NS)?;
        // `place` succeeded, so the slot and its header lie inside the 8 MiB flash.
        let vtor = FLASH_S | (layout.secure_slot_base + u32::from(secure.header_size));
        if vtor & 0x7F != 0 {
            log::warn!("vector table {vtor:#010x} is not 128-byte aligned; VTOR ignores bits 6:0");
        }

        let mut bus = MemoryMap::new();
        bus.add_rom("flash", flash, &[FLASH_NS, FLASH_S]);
        bus.add_ram("sram", SRAM_SIZE, &[SRAM_NS, SRAM_S]);
        let mut cpu = Cpu::new();
        cpu.set_vtor(Sec::Secure, vtor);
        cpu.reset(&mut bus);
        Ok(Self {
            cpu,
            bus,
            sched: Scheduler::new(),
            breakpoints: BTreeSet::new(),
            resets: 0,
            boot_vtor: vtor,
            sleep: None,
            stopped_at: None,
        })
    }

    /// True while the CPU sleeps in WFI/WFE, waiting for an exception (or, after WFE, an
    /// event).
    pub fn asleep(&self) -> bool {
        self.sleep.is_some()
    }

    /// End the sleep as a debugger halt does: the next step executes the instruction after
    /// the WFI/WFE, whether or not anything is pending. Does nothing while awake.
    pub fn wake(&mut self) {
        self.sleep = None;
    }

    /// Run for up to `max_cycles` cycles from now.
    ///
    /// Stops before an instruction at a breakpoint (a run that starts on the breakpoint the
    /// previous run stopped at executes it first), on lockup, on a guest `BKPT`, when the
    /// budget is used up, or when the CPU is asleep with nothing left to wake it
    /// ([`StopReason::Idle`]).
    pub fn run(&mut self, max_cycles: u64) -> StopReason {
        self.run_with(max_cycles, |_| ControlFlow::Continue(()))
    }

    /// [`run`](Self::run), calling `before_step` with the machine before each CPU step (not
    /// while asleep): e.g. to trace with [`trace_line`](Self::trace_line). When it returns
    /// `ControlFlow::Break`, the run stops with [`StopReason::Halted`] before that step.
    pub fn run_with(
        &mut self,
        max_cycles: u64,
        mut before_step: impl FnMut(&mut Self) -> ControlFlow<()>,
    ) -> StopReason {
        let end = self.sched.now.saturating_add(max_cycles);
        loop {
            if self.sched.now >= end {
                return StopReason::CycleLimit;
            }
            if self.asleep() {
                if let Some(stop) = self.idle(end) {
                    return stop;
                }
                continue;
            }
            let pc = self.cpu.regs.pc;
            let resumed = self.stopped_at.take() == Some(pc);
            if self.breakpoints.contains(&pc) && !resumed {
                self.stopped_at = Some(pc);
                return StopReason::Breakpoint(pc);
            }
            if before_step(self).is_break() {
                // Still on the breakpoint this run resumed from: the next run steps over it.
                if resumed {
                    self.stopped_at = Some(pc);
                }
                return StopReason::Halted;
            }
            if let Some(stop) = self.step_cpu() {
                return stop;
            }
        }
    }

    /// Execute one CPU step (one instruction, or the entry to a pending exception), ignoring
    /// breakpoints. While asleep, wake if a wake-up is due (and then step), else jump to the
    /// next wake-up time without stepping. Returns `Stepped` unless the step locked up or hit
    /// a guest `BKPT`, or the CPU is asleep with nothing left to wake it (`Idle`: nothing
    /// changes).
    pub fn step(&mut self) -> StopReason {
        if self.asleep() {
            if let Some(stop) = self.idle(u64::MAX) {
                return stop;
            }
            if self.asleep() {
                return StopReason::Stepped;
            }
        }
        self.step_cpu().unwrap_or(StopReason::Stepped)
    }

    /// One trace line for the instruction at the PC, which is about to execute:
    /// `{cycles:>12} {S|N} {pc:08x}: {disasm}`. The instruction is read without touching
    /// devices or the unmapped log; code that cannot be read that way shows as `<unreadable>`.
    pub fn trace_line(&mut self) -> String {
        let pc = self.cpu.regs.pc;
        let sec = match self.cpu.regs.sec {
            Sec::Secure => 'S',
            Sec::NonSecure => 'N',
        };
        let text = self.disasm_at(pc);
        let text = text.as_deref().unwrap_or("<unreadable>");
        format!("{:>12} {sec} {pc:08x}: {text}", self.sched.now)
    }

    fn disasm_at(&mut self, pc: u32) -> Option<String> {
        let hw1 = self.bus.peek(pc, 2)? as u16;
        let (instr, len) = if is_32bit(hw1) {
            let hw2 = self.bus.peek(pc.wrapping_add(2), 2)? as u16;
            (decode32(hw1, hw2), 4)
        } else {
            (decode16(hw1), 2)
        };
        Some(disasm(&instr, pc, len))
    }

    /// Step the CPU once and account for it; `Some` if the machine must stop.
    fn step_cpu(&mut self) -> Option<StopReason> {
        self.stopped_at = None;
        let pc = self.cpu.regs.pc;
        self.bus.pc_hint = pc;
        let stop = match self.cpu.step(&mut self.bus) {
            StepResult::Executed { cycles } => {
                self.advance(u64::from(cycles));
                None
            }
            StepResult::Sleep(kind) => {
                // The step does not report its cycles; WFI/WFE cost 1.
                self.advance(1);
                self.sleep = Some(kind);
                self.wake_if_due();
                None
            }
            StepResult::Bkpt(imm) => Some(StopReason::Bkpt { pc, imm }),
            StepResult::Lockup => Some(StopReason::Lockup { pc }),
        };
        if self.cpu.take_sysreset_request() {
            self.system_reset();
        }
        stop
    }

    /// While asleep: wake if a wake-up is due, else advance time to the next wake-up time (at
    /// most to `end`). `Some(Idle)` if nothing will ever wake the CPU.
    fn idle(&mut self, end: u64) -> Option<StopReason> {
        if self.wake_if_due() {
            return None;
        }
        let now = self.sched.now;
        let systick = self
            .cpu
            .cycles_to_systick()
            .map(|c| now.saturating_add(u64::from(c)));
        let Some(wake) = [self.sched.next_deadline(), systick]
            .into_iter()
            .flatten()
            .min()
        else {
            return Some(StopReason::Idle);
        };
        let target = wake.clamp(now, end.max(now));
        let mut left = target - now;
        // SysTick counts while the CPU sleeps; the CPU only ticks it as it steps.
        while left > 0 {
            let chunk = u32::try_from(left).unwrap_or(u32::MAX);
            self.cpu.tick_systick(chunk);
            left -= u64::from(chunk);
        }
        self.advance(target - now);
        None
    }

    /// Wake the sleeping CPU if an exception is pending or, for WFE, the event register is set
    /// (consuming it). Returns whether it woke.
    fn wake_if_due(&mut self) -> bool {
        let Some(kind) = self.sleep else {
            return false;
        };
        if kind == SleepKind::Wfe && self.cpu.event {
            self.cpu.event = false;
        } else if !self.cpu.exception_pending() {
            return false;
        }
        self.sleep = None;
        true
    }

    /// Advance time by `cycles` and fire every event that is due.
    fn advance(&mut self, cycles: u64) {
        self.sched.now = self.sched.now.saturating_add(cycles);
        while let Some(kind) = self.sched.pop_due() {
            self.fire(kind);
        }
    }

    /// Deliver an event. No peripheral schedules events yet (Plan 2), so there is nothing to
    /// deliver it to.
    fn fire(&mut self, kind: EventKind) {
        log::debug!("event {kind:?} at cycle {} has no handler", self.sched.now);
    }

    /// AIRCR.SYSRESETREQ: reset the CPU into the boot vector table again. Flash, SRAM and the
    /// unmapped log are kept, pending events are dropped, and time keeps counting.
    fn system_reset(&mut self) {
        self.resets += 1;
        log::info!(
            "system reset #{} at cycle {} (pc {:#010x})",
            self.resets,
            self.sched.now,
            self.bus.pc_hint
        );
        // `reset` returns the CPU to its `Cpu::new` state but keeps its configuration (the
        // IDAU and the reset vector table, set again here in case anything changed it).
        self.cpu.set_vtor(Sec::Secure, self.boot_vtor);
        self.cpu.reset(&mut self.bus);
        let now = self.sched.now;
        self.sched = Scheduler::new();
        self.sched.now = now;
        self.sleep = None;
        self.stopped_at = None;
    }
}
