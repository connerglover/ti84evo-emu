//! The emulated TI-84 Evo: CPU, memory map and scheduler, booted from an OS bundle.
//!
//! The memory map: 16 MiB of flash at `FLASH_NS` and `FLASH_S`, 1.5 MiB of SRAM at `SRAM_NS`
//! and `SRAM_S`, the SoC's devices ([`evo_hw::soc::attach`]), and everything else unmapped and
//! logged (see `docs/hardware/memory-map.md`). At power-on and on every system reset the
//! machine writes the BL2 shared-data header ([`BL2_SHARED_HEADER`] at [`BL2_SHARED_DATA`]),
//! as the boot ROM's BL2 stage does on the real calculator.
//!
//! # Devices
//!
//! The machine connects the devices to the CPU and the scheduler (`sync_bus`) after every CPU
//! step and after every fired event: IRQ line levels go to the NVIC (a line still high after a
//! system reset pends again), pulses pend once, event requests become
//! [`EventKind::Peripheral`] events, and the CPU's unimplemented PPB accesses go to the
//! unmapped log. Devices see `sched.now` as `ctx.now`. Host APIs reach a device only through
//! `Machine::with_dev`, which does the same around the call.
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
//!
//! # Recent trace
//!
//! The machine remembers the last [`RECENT_STEPS`] CPU steps (cycle count, security state and
//! PC; three stores per step) and formats them only when asked
//! ([`Machine::recent_trace`]), e.g. to show how the guest reached a lockup.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use armv8m::decode::is_32bit;
use armv8m::{Cpu, Sec, SleepKind, StepResult, decode16, decode32, disasm};
use evo_hw::dev::spif::Spif;
use evo_hw::nor::{NvState, SECREGS_LEN};
use evo_hw::soc::{self, FLASH_BUFFER, SRAM_BUFFER};
use evo_hw::{DevCtx, Device, MemoryMap, ResetKind, UnmappedAccess, UnmappedKind};
use evo_image::{Bundle, ImageError, Layout};
use serde::{Deserialize, Serialize};

use crate::{EventKind, Scheduler};

pub use evo_hw::CPU_HZ;

/// Size of the flash (erased to 0xFF). It holds both OS images of `Layout::EVO_7_1`.
pub const FLASH_SIZE: u32 = 16 << 20;
/// Non-secure alias of flash; flash offsets are addresses in this alias.
pub const FLASH_NS: u32 = 0x0000_0000;
/// Secure alias of flash.
pub const FLASH_S: u32 = 0x1000_0000;
/// Size of the SRAM: 1.5 MiB = 3 x 512 KiB.
pub const SRAM_SIZE: u32 = 0x0018_0000;
/// Non-secure alias of SRAM.
pub const SRAM_NS: u32 = 0x2000_0000;
/// Secure alias of SRAM.
pub const SRAM_S: u32 = 0x3000_0000;
/// Where BL2 leaves its shared data for the secure image (start of Secure SRAM).
pub const BL2_SHARED_DATA: u32 = 0x3000_0000;
/// The shared-data header the machine writes at [`BL2_SHARED_DATA`]: u16 magic 0x2016, u16
/// total length 4 (no entries).
pub const BL2_SHARED_HEADER: u32 = 0x0004_2016;
/// Erased NOR flash reads as all ones.
const ERASED: u8 = 0xFF;
/// CPU steps [`Machine::recent_trace`] remembers.
pub const RECENT_STEPS: usize = 64;

/// One remembered CPU step: what the trace line printed before it shows.
#[derive(Clone, Copy, Default)]
struct RecentStep {
    cycles: u64,
    pc: u32,
    secure: bool,
}

/// Ring buffer of the last [`RECENT_STEPS`] CPU steps.
#[derive(Default)]
struct RecentSteps {
    /// Up to `RECENT_STEPS` entries; once full, `next` is the oldest.
    steps: Vec<RecentStep>,
    /// Where the next step is written once `steps` is full.
    next: usize,
}

impl RecentSteps {
    fn push(&mut self, step: RecentStep) {
        if self.steps.len() < RECENT_STEPS {
            self.steps.push(step);
        } else {
            self.steps[self.next] = step;
            self.next = (self.next + 1) % RECENT_STEPS;
        }
    }

    /// Oldest first.
    fn iter(&self) -> impl Iterator<Item = &RecentStep> {
        let (newer, older) = self.steps.split_at(self.next);
        older.iter().chain(newer)
    }
}

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
    /// The device IRQ lines (`bus.irq_lines()`) last passed to the NVIC.
    #[serde(default)]
    applied_irqs: u64,
    /// Debugging aid, not machine state: not saved, and empty after a restore.
    #[serde(skip)]
    recent: RecentSteps,
}

impl Machine {
    /// Build the memory map, program `b`'s images into an erased flash per `layout` and reset
    /// the CPU into the secure image: `VTOR_S` = Secure alias of image 0's slot plus its header
    /// size. Same as [`from_bundle_nv`](Self::from_bundle_nv) with no saved state.
    ///
    /// Images are placed before the map is built, so an image that does not fit in flash is an
    /// [`ImageError::DoesNotFit`], never a panic.
    pub fn from_bundle(b: &Bundle, layout: &Layout) -> Result<Self, ImageError> {
        Self::from_bundle_nv(b, layout, None)
    }

    /// [`from_bundle`](Self::from_bundle), starting from `nv` (flash bytes and NOR security
    /// registers, e.g. read from a flash file) instead of an erased part. The bundle's images
    /// are placed over that flash, so in their slots they always win over the saved bytes;
    /// everything outside the slots (e.g. ITS/PS at 0x0009_0000) is kept.
    pub fn from_bundle_nv(
        b: &Bundle,
        layout: &Layout,
        nv: Option<NvState>,
    ) -> Result<Self, ImageError> {
        let pkg = &b.package;
        // `Package::parse` never yields an empty package, but its fields are public.
        let secure = pkg
            .images
            .first()
            .ok_or(ImageError::BadMagic { offset: 0 })?;
        // An `NvState` always holds `NOR_SIZE` (= `FLASH_SIZE`) flash bytes.
        let mut flash = nv.as_ref().map_or_else(
            || vec![ERASED; FLASH_SIZE as usize],
            |nv| nv.flash().to_vec(),
        );
        layout.place(pkg, &mut flash, FLASH_NS)?;
        // `place` succeeded, so the slot and its header lie inside the 16 MiB flash.
        let vtor = FLASH_S | (layout.secure_slot_base + u32::from(secure.header_size));
        if vtor & 0x7F != 0 {
            log::warn!("vector table {vtor:#010x} is not 128-byte aligned; VTOR ignores bits 6:0");
        }

        let mut bus = MemoryMap::new();
        bus.add_rom(FLASH_BUFFER, flash, &[FLASH_NS, FLASH_S]);
        bus.add_ram(SRAM_BUFFER, SRAM_SIZE, &[SRAM_NS, SRAM_S]);
        soc::attach(&mut bus);
        write_bl2_header(&mut bus);
        let mut cpu = Cpu::new();
        cpu.set_vtor(Sec::Secure, vtor);
        cpu.reset(&mut bus);
        let mut m = Self {
            cpu,
            bus,
            sched: Scheduler::new(),
            breakpoints: BTreeSet::new(),
            resets: 0,
            boot_vtor: vtor,
            sleep: None,
            stopped_at: None,
            applied_irqs: 0,
            recent: RecentSteps::default(),
        };
        if let Some(nv) = &nv {
            m.with_dev("spif", |s: &mut Spif, _| {
                s.nor_mut().set_secregs(nv.secregs())
            });
        }
        Ok(m)
    }

    /// The non-volatile state now, as a flash file stores it: the flash bytes and the SPIF NOR
    /// part's security registers. Security registers the part does not hold (it has none
    /// after a save-state restore until they are set again) read as erased.
    pub fn nv_state(&self) -> NvState {
        let mut flash = self
            .bus
            .region_bytes(FLASH_BUFFER)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        // The map always holds `FLASH_SIZE` bytes of flash; this only keeps `new` infallible.
        flash.resize(FLASH_SIZE as usize, ERASED);
        let secregs = self
            .bus
            .device::<Spif>("spif")
            .map(|s| s.nor().secregs())
            .filter(|r| r.len() == SECREGS_LEN)
            .map_or_else(|| vec![ERASED; SECREGS_LEN], <[u8]>::to_vec);
        NvState::new(flash, secregs).unwrap_or_else(|e| {
            // Unreachable: both lengths are fixed above.
            log::error!("nv_state: {e}");
            NvState::erased()
        })
    }

    /// Run `f` on the device called `name`, if it is a `T`: the only way host APIs reach a
    /// device. The device sees the current time, and whatever it changes (IRQ lines, pulses,
    /// events) reaches the CPU and the scheduler before this returns.
    pub(crate) fn with_dev<T: Device, R>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut T, &mut DevCtx<'_>) -> R,
    ) -> Option<R> {
        self.bus.now = self.sched.now;
        let r = self.bus.with_device(name, f);
        self.sync_bus();
        r
    }

    /// Pass what the devices and the CPU queued since the last call on: (1) IRQ line changes
    /// to the NVIC, (2) pulses, (3) event requests to the scheduler, (4) the CPU's
    /// unimplemented PPB accesses to the unmapped log.
    fn sync_bus(&mut self) {
        let lines = self.bus.irq_lines();
        let mut changed = lines ^ self.applied_irqs;
        while changed != 0 {
            let bit = changed.trailing_zeros();
            changed &= changed - 1;
            // `bit < 64`, so it fits.
            self.cpu.set_irq_level(bit as u16, lines & (1 << bit) != 0);
        }
        self.applied_irqs = lines;
        for irq in self.bus.take_pulses() {
            self.cpu.pulse_irq(irq);
        }
        for (id, at) in self.bus.take_event_requests() {
            let kind = EventKind::Peripheral(id);
            match at {
                Some(at) => self.sched.schedule(at, kind),
                None => self.sched.cancel(kind),
            }
        }
        for miss in self.cpu.take_ppb_misses() {
            let kind = if miss.write.is_some() {
                UnmappedKind::Write
            } else {
                UnmappedKind::Read
            };
            self.bus.log.record(UnmappedAccess {
                addr: miss.addr,
                width: miss.width,
                write: miss.write,
                kind,
                pc: self.bus.pc_hint,
                secure: miss.secure,
            });
        }
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
    pub fn trace_line(&self) -> String {
        let step = RecentStep {
            cycles: self.sched.now,
            pc: self.cpu.regs.pc,
            secure: self.cpu.regs.sec == Sec::Secure,
        };
        self.format_step(&step)
    }

    /// The trace lines ([`trace_line`](Self::trace_line)) of the last [`RECENT_STEPS`] CPU
    /// steps, oldest first; fewer if fewer have run. The last line is the most recent step:
    /// after a lockup, the instruction (or exception entry) that locked up.
    ///
    /// Only the cycle count, security state and PC are recorded as the machine runs; the
    /// instruction is disassembled from memory now, so code that was overwritten since shows
    /// its new contents.
    pub fn recent_trace(&self) -> Vec<String> {
        self.recent.iter().map(|s| self.format_step(s)).collect()
    }

    fn format_step(&self, step: &RecentStep) -> String {
        let sec = if step.secure { 'S' } else { 'N' };
        let text = self.disasm_at(step.pc);
        let text = text.as_deref().unwrap_or("<unreadable>");
        format!("{:>12} {sec} {:08x}: {text}", step.cycles, step.pc)
    }

    fn disasm_at(&self, pc: u32) -> Option<String> {
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
        self.recent.push(RecentStep {
            cycles: self.sched.now,
            pc,
            secure: self.cpu.regs.sec == Sec::Secure,
        });
        self.bus.pc_hint = pc;
        self.bus.now = self.sched.now;
        let result = self.cpu.step(&mut self.bus);
        self.sync_bus();
        let stop = match result {
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

    /// Advance time by `cycles` and fire every event that is due, passing what each one
    /// changed on before the next ([`sync_bus`](Self::sync_bus)).
    fn advance(&mut self, cycles: u64) {
        self.sched.now = self.sched.now.saturating_add(cycles);
        while let Some(kind) = self.sched.pop_due() {
            self.bus.now = self.sched.now;
            self.fire(kind);
            self.sync_bus();
        }
    }

    /// Deliver an event to its device.
    fn fire(&mut self, kind: EventKind) {
        match kind {
            EventKind::Peripheral(id) => self.bus.device_event(id),
        }
    }

    /// AIRCR.SYSRESETREQ: reset the CPU into the boot vector table again and every device
    /// with [`ResetKind::System`]. Flash, SRAM (but for the BL2 header, written again) and the
    /// unmapped log are kept, pending events are dropped (a device that keeps running asks for
    /// its event again in `reset`), and time keeps counting. Device IRQ lines still high after
    /// the reset pend again.
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
        // `Cpu::reset` cleared the NVIC's levels: every line still high has to be applied
        // again.
        self.applied_irqs = 0;
        self.bus.now = self.sched.now;
        self.bus.reset_devices(ResetKind::System);
        write_bl2_header(&mut self.bus);
        self.sync_bus();
    }
}

/// Write the BL2 shared-data header into SRAM.
fn write_bl2_header(bus: &mut MemoryMap) {
    let written = bus.poke(BL2_SHARED_DATA, 4, BL2_SHARED_HEADER);
    debug_assert!(written, "SRAM holds the BL2 shared data");
}
