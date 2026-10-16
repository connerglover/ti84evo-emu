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
//!
//! # Watchpoints
//!
//! [`Machine::set_watchpoints`] passes read/write ranges to the memory map, which observes
//! every guest data access; the access that matches stops `run` (and `step`) after the
//! instruction that made it, with [`StopReason::Watchpoint`]. Exec ranges are checked by the
//! machine before each step, like breakpoints: the run stops before the instruction and the
//! next run executes it first. While anything is watched (or the bus monitor records),
//! `run` takes the checked path instead of batches ([`Machine::batched_runs`]).
//!
//! # Goals
//!
//! Step over, step out and run to arm a [`RunGoal`] ([`crate::debug`]) that `run` checks
//! before each step, ahead of breakpoints; reaching it stops the run with
//! [`StopReason::Goal`]. While a goal is armed, `run` takes the checked path too.
//!
//! # Save states
//!
//! [`Machine::save_state`] / [`Machine::load_state`] (in [`crate::state`]): CPU, scheduler,
//! SRAM, devices and the unmapped log, tied to the bundle the machine booted and to the flash
//! it had (its fingerprint, [`Machine::nv_fingerprint`]). Flash and the NOR security registers
//! are not in a save state; they persist in the flash file.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use armv8m::decode::is_32bit;
use armv8m::{Cpu, Sec, SleepKind, StepResult, decode16, decode32, disasm};
use evo_hw::dev::spif::Spif;
use evo_hw::nor::{NvState, SECREGS_LEN};
use evo_hw::soc::{self, FLASH_BUFFER, SRAM_BUFFER};
use evo_hw::{
    AccessDir, DevCtx, Device, MemoryMap, ResetKind, UnmappedAccess, UnmappedKind, WatchError,
    WatchHit, WatchRange,
};
use evo_image::{Bundle, ImageError, Layout};
use serde::{Deserialize, Serialize};

use crate::debug::RunGoal;
use crate::{EventKind, Scheduler, state};

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
pub(crate) const ERASED: u8 = 0xFF;
/// CPU steps [`Machine::recent_trace`] remembers.
pub const RECENT_STEPS: usize = 64;

/// One remembered CPU step: what the trace line printed before it shows.
#[derive(Clone, Copy, Default)]
struct RecentStep {
    cycles: u64,
    pc: u32,
    secure: bool,
}

/// Ring buffer of the last [`RECENT_STEPS`] CPU steps (a power of two).
pub(crate) struct RecentSteps {
    steps: [RecentStep; RECENT_STEPS],
    /// Steps pushed so far; the next one goes to `pushed % RECENT_STEPS`.
    pushed: u64,
}

impl Default for RecentSteps {
    fn default() -> Self {
        Self {
            steps: [RecentStep::default(); RECENT_STEPS],
            pushed: 0,
        }
    }
}

impl RecentSteps {
    #[inline]
    fn push(&mut self, step: RecentStep) {
        self.steps[self.pushed as usize % RECENT_STEPS] = step;
        self.pushed += 1;
    }

    /// Oldest first.
    fn iter(&self) -> impl Iterator<Item = &RecentStep> {
        let filled = self.pushed.min(RECENT_STEPS as u64) as usize;
        let next = self.pushed as usize % RECENT_STEPS;
        let (newer, older) = self.steps[..filled.max(next)].split_at(next);
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
    /// A watchpoint ([`Machine::set_watchpoints`]) matched. Read/write: the instruction at
    /// `pc` accessed `addr` (`value` read or written) and has completed. Exec: the next
    /// instruction is at `addr` = `pc` in an exec range and has not run (`value` 0); the next
    /// run executes it first, as for a breakpoint.
    Watchpoint {
        addr: u32,
        dir: AccessDir,
        value: u32,
        pc: u32,
    },
    /// A run goal was reached; the next instruction is at `pc` and has not run.
    Goal { pc: u32 },
}

/// GPIO inputs the board drives high at power-on (`docs/hardware/gpio.md`, "Pin roles"):
/// pin 9, the charger status line (high = no charger or USB power; low makes the BSP show its
/// charging animation instead of launching the OS), and pin 11, the battery comparator (high
/// = the battery is above the threshold the BSP sets on pins 5/7/12; low reads as a critical
/// battery). `Machine::set_gpio_input` changes them.
pub const BOARD_INPUTS_HIGH: [u8; 2] = [9, 11];

/// A persisted flash cannot boot directly into its stored secure image.
#[derive(Debug, thiserror::Error)]
pub enum NvBootError {
    #[error("invalid persisted boot image: {0}")]
    Image(&'static str),
    #[error("invalid persisted boot vectors: {0}")]
    Vectors(&'static str),
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
    pub(crate) boot_vtor: u32,
    /// Set while the CPU sleeps, with what put it to sleep.
    pub(crate) sleep: Option<SleepKind>,
    /// The breakpoint the last `run` stopped at, which the next `run` steps over.
    pub(crate) stopped_at: Option<u32>,
    /// The device IRQ lines (`bus.irq_lines()`) last passed to the NVIC.
    #[serde(default)]
    pub(crate) applied_irqs: u64,
    /// Debugging aid, not machine state: not saved, and empty after a restore.
    #[serde(skip)]
    pub(crate) recent: RecentSteps,
    /// Fingerprint of the OS this machine booted (`state::bundle_id`); a save state carries
    /// it and loads only on a machine with the same one.
    #[serde(default)]
    pub(crate) bundle_id: [u8; 32],
    /// Cycles of CPU steps (instructions and exception entries) since power-on: `sched.now`
    /// without the time skipped while asleep. Statistics only: not saved.
    #[serde(skip)]
    pub(crate) awake_cycles: u64,
    /// Set by [`set_batching(false)`](Self::set_batching): `run` checks breakpoints, sleep and
    /// due events before every step instead of in batches. Not machine state: not saved, and
    /// a restore keeps it.
    #[serde(skip)]
    pub(crate) unbatched: bool,
    /// The exec ranges of [`set_watchpoints`](Self::set_watchpoints); the bus holds all the
    /// ranges and checks the read/write ones. Debugger setting: not saved; a restore keeps it.
    #[serde(skip)]
    pub(crate) exec_watch: Vec<WatchRange>,
    /// `run_batch` calls since power-on. Statistics only: not saved.
    #[serde(skip)]
    pub(crate) batched: u64,
    /// The run goal ([`crate::debug`]). Debugger setting: not saved; a restore clears it.
    #[serde(skip)]
    pub(crate) goal: Option<RunGoal>,
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
        let bundle_id = state::bundle_id(&pkg.bytes, layout.secure_slot_base, layout.ns_slot_base);
        Ok(Self::power_on(
            flash,
            nv.as_ref().map(NvState::secregs),
            vtor,
            bundle_id,
        ))
    }

    /// Boot the images already in `nv`, preserving every flash byte and NOR security
    /// register. Unlike [`from_bundle_nv`](Self::from_bundle_nv), this never installs a
    /// bundle, so an OS update received over USB survives the next process start.
    ///
    /// The secure slot must contain an MCUboot header, an aligned vector table, a mapped
    /// SRAM stack pointer and a Thumb reset vector within its image body. This checks boot
    /// bounds, not firmware signatures or BL1/BL2 recovery/swap state. An invalid image is
    /// an error; callers must not silently replace it with an older bundle.
    ///
    /// Save-state identity comes from the stored secure header/payload and layout, rather
    /// than a bundle package. The separate NV fingerprint still checks *all* flash and
    /// security-register bytes. States from bundle boot and persisted boot need not share
    /// an identity; ordinary data writes do not change the persisted boot identity.
    pub fn from_nv(nv: NvState, layout: &Layout) -> Result<Self, NvBootError> {
        let flash = nv.flash();
        let start = layout.secure_slot_base as usize;
        if layout.secure_slot_base >= FLASH_SIZE
            || layout.ns_slot_base >= FLASH_SIZE
            || layout.secure_slot_base == layout.ns_slot_base
        {
            return Err(NvBootError::Image(
                "slot addresses must be distinct flash offsets",
            ));
        }
        let header = flash
            .get(start..start + 32)
            .ok_or(NvBootError::Image("secure header lies outside flash"))?;
        let word = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().unwrap());
        if word(&header[..4]) != 0x96f3_b83d {
            return Err(NvBootError::Image("secure slot has no MCUboot header"));
        }
        let header_size = usize::from(u16::from_le_bytes([header[8], header[9]]));
        let image_size = word(&header[12..16]) as usize;
        if header_size < 32 || image_size < 8 {
            return Err(NvBootError::Image("header or vector table is too short"));
        }
        let body = start + header_size;
        let end = body
            .checked_add(image_size)
            .filter(|&end| end <= flash.len())
            .ok_or(NvBootError::Image("secure image extends outside flash"))?;
        if start < layout.ns_slot_base as usize && end > layout.ns_slot_base as usize {
            return Err(NvBootError::Image(
                "secure image overlaps the non-secure slot",
            ));
        }
        if body & 0x7f != 0 {
            return Err(NvBootError::Vectors("vector table is not 128-byte aligned"));
        }
        let sp = word(&flash[body..body + 4]);
        let reset = word(&flash[body + 4..body + 8]);
        let mapped_stack = [SRAM_S, SRAM_NS]
            .into_iter()
            .any(|base| sp > base && sp <= base + SRAM_SIZE);
        if sp & 3 != 0 || !mapped_stack {
            return Err(NvBootError::Vectors(
                "initial MSP is outside mapped SRAM or unaligned",
            ));
        }
        let target = reset & !1;
        let vtor = FLASH_S | body as u32;
        if reset & 1 == 0
            || target < vtor
            || u64::from(target) + 2 > u64::from(FLASH_S) + end as u64
        {
            return Err(NvBootError::Vectors(
                "reset vector is not Thumb code within the secure image",
            ));
        }
        let identity = state::bundle_id(
            &flash[start..end],
            layout.secure_slot_base,
            layout.ns_slot_base,
        );
        Ok(Self::power_on(
            flash.to_vec(),
            Some(nv.secregs()),
            vtor,
            identity,
        ))
    }

    /// Switch the calculator off and on again: the machine becomes what
    /// [`from_bundle_nv`](Self::from_bundle_nv) builds from [`nv_state`](Self::nv_state) (the
    /// bundle's images are already in that flash). Flash and the NOR security registers are
    /// kept; the CPU, SRAM, devices, time, `resets`, breakpoints, watchpoints (and any pending
    /// hit), the bus monitor, the run goal, held keys and host inputs start over as at
    /// power-on.
    pub fn power_cycle(&mut self) {
        let nv = self.nv_state();
        let unbatched = self.unbatched;
        *self = Self::power_on(
            nv.flash().to_vec(),
            Some(nv.secregs()),
            self.boot_vtor,
            self.bundle_id,
        );
        self.unbatched = unbatched;
    }

    /// The machine at power-on with `flash` (images already placed) and, if given, these NOR
    /// security registers: map built, BL2 header written, CPU reset into `vtor`, board inputs
    /// driven.
    fn power_on(flash: Vec<u8>, secregs: Option<&[u8]>, vtor: u32, bundle_id: [u8; 32]) -> Self {
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
            bundle_id,
            awake_cycles: 0,
            unbatched: false,
            exec_watch: Vec::new(),
            batched: 0,
            goal: None,
        };
        if let Some(secregs) = secregs {
            m.with_dev("spif", |s: &mut Spif, _| s.nor_mut().set_secregs(secregs));
        }
        for pin in BOARD_INPUTS_HIGH {
            m.set_gpio_input(pin, Some(true));
        }
        m
    }

    /// The non-volatile state now, as a flash file stores it: the flash bytes and the SPIF NOR
    /// part's security registers. A save-state restore keeps the registers the part holds
    /// (they are not in a save state; `Spif::load` keeps the current ones), so after
    /// [`load_state`](Self::load_state) this is the same as before it. (Were the part to hold no
    /// registers, they would read as erased; the SoC's part always holds them.)
    pub fn nv_state(&self) -> NvState {
        let mut flash = Vec::with_capacity(FLASH_SIZE as usize);
        for chunk in self.nv_flash_chunks() {
            flash.extend_from_slice(chunk);
        }
        let secregs = self.nv_secregs().to_vec();
        NvState::new(flash, secregs).unwrap_or_else(|e| {
            // Unreachable: both lengths are fixed above.
            log::error!("nv_state: {e}");
            NvState::erased()
        })
    }

    /// Canonical flash bytes for both persistence and its fingerprint, without allocating.
    /// Short or absent buffers read as erased; oversized buffers are cut at the chip size.
    pub(crate) fn nv_flash_chunks(&self) -> impl Iterator<Item = &[u8]> {
        const ERASED_PAGE: [u8; 4096] = [ERASED; 4096];
        let flash = self.bus.region_bytes(FLASH_BUFFER).unwrap_or_default();
        let size = FLASH_SIZE as usize;
        let pad = size.saturating_sub(flash.len());
        std::iter::once(&flash[..flash.len().min(size)])
            .chain(std::iter::repeat_n(
                ERASED_PAGE.as_slice(),
                pad / ERASED_PAGE.len(),
            ))
            .chain(std::iter::once(&ERASED_PAGE[..pad % ERASED_PAGE.len()]))
    }

    pub(crate) fn nv_secregs(&self) -> &[u8] {
        self.bus
            .device::<Spif>("spif")
            .map(|s| s.nor().secregs())
            .filter(|r| r.len() == SECREGS_LEN)
            .unwrap_or(&[ERASED; SECREGS_LEN])
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
    #[inline]
    fn sync_bus(&mut self) {
        if self.bus.take_device_activity() {
            self.sync_devices();
        }
        if self.cpu.has_ppb_misses() {
            self.drain_ppb_misses();
        }
    }

    /// Step (4) of [`sync_bus`](Self::sync_bus).
    #[cold]
    #[inline(never)]
    fn drain_ppb_misses(&mut self) {
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

    /// Steps (1) to (3) of [`sync_bus`](Self::sync_bus), needed only after a device ran.
    #[inline(never)]
    fn sync_devices(&mut self) {
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

    /// Cycles this machine spent executing (instructions and exception entries) since power-on:
    /// time it advanced minus the time skipped while asleep. Statistics only: not in a save
    /// state, and a restore keeps the count going.
    pub fn awake_cycles(&self) -> u64 {
        self.awake_cycles
    }

    /// Run for up to `max_cycles` cycles from now.
    ///
    /// Stops before an instruction where the run goal is reached, at a breakpoint or in an exec
    /// watchpoint (a run that starts where the previous run stopped this way executes it
    /// first), after an instruction that
    /// matched a read/write watchpoint, on lockup, on a guest `BKPT`, when the budget is used
    /// up, or when the CPU is asleep with nothing left to wake it ([`StopReason::Idle`]).
    ///
    /// While no breakpoint, watchpoint or goal is set and the bus monitor is off, the CPU steps in
    /// batches that end at the next due event, when
    /// a device runs, or when the CPU sleeps; each step does exactly what it does one at a
    /// time, so the result is the same as [`set_batching(false)`](Self::set_batching).
    pub fn run(&mut self, max_cycles: u64) -> StopReason {
        if self.unbatched {
            return self.run_with(max_cycles, |_| ControlFlow::Continue(()));
        }
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
            let stop = if !self.needs_checks() {
                self.run_batch(end)
            } else {
                self.checked_step(&mut |_| ControlFlow::Continue(()))
            };
            if let Some(stop) = stop {
                return stop;
            }
        }
    }

    /// Whether `run` must check every step: a breakpoint, watchpoint or goal is set, the bus
    /// monitor records, or the last run stopped on an instruction it has to step over.
    #[inline]
    fn needs_checks(&self) -> bool {
        !self.breakpoints.is_empty()
            || self.stopped_at.is_some()
            || !self.exec_watch.is_empty()
            || self.goal.is_some()
            || self.bus.watching()
    }

    /// Replace the watchpoints (read, write and exec ranges). Every range is validated first;
    /// on `Err` nothing changes. An empty set turns watching off.
    pub fn set_watchpoints(&mut self, ranges: Vec<WatchRange>) -> Result<(), WatchError> {
        let exec = ranges.iter().filter(|r| r.kinds.exec).copied().collect();
        self.bus.set_watchpoints(ranges)?;
        self.exec_watch = exec;
        Ok(())
    }

    /// The watchpoints as last set.
    pub fn watchpoints(&self) -> Vec<WatchRange> {
        self.bus.watchpoints()
    }

    /// How many batches `run` stepped since power-on (statistics, e.g. to see that watching
    /// turns batching off). Not saved; a restore keeps the count going.
    pub fn batched_runs(&self) -> u64 {
        self.batched
    }

    /// The exec watchpoint stop for an instruction at `pc`, if an exec range holds `pc`.
    #[cold]
    #[inline(never)]
    fn exec_stop(&self, pc: u32) -> Option<StopReason> {
        self.exec_watch
            .iter()
            .any(|r| r.overlaps(pc, 1))
            .then_some(StopReason::Watchpoint {
                addr: pc,
                dir: AccessDir::Exec,
                value: 0,
                pc,
            })
    }

    /// The outcome of the step at `pc` that returned `stop`: a lockup or `BKPT` wins, else a
    /// read/write watchpoint the step matched. The pending hit is taken either way.
    #[inline(always)]
    fn with_data_hit(&mut self, pc: u32, stop: Option<StopReason>) -> Option<StopReason> {
        let hit = self.bus.take_watch_hit();
        stop.or_else(|| {
            hit.map(
                |WatchHit {
                     addr, dir, value, ..
                 }| StopReason::Watchpoint {
                    addr,
                    dir,
                    value,
                    pc,
                },
            )
        })
    }

    /// Whether [`run`](Self::run) steps in batches (the default). Off, it checks breakpoints,
    /// sleep and due events before every step, like [`run_with`](Self::run_with); the outcome
    /// is the same either way (this exists to test that). Not saved in a save state.
    pub fn set_batching(&mut self, on: bool) {
        self.unbatched = !on;
    }

    /// Step the awake CPU, with no breakpoint set and nothing to step over, in one
    /// [`Cpu::run_steps`] run. The run ends when time reaches `end` or the next due event,
    /// after a step that ran a device (its event requests may move the next deadline), queued
    /// PPB misses or requested a system reset, or when a step sleeps, hits `BKPT` or locks up.
    /// At least one step runs.
    ///
    /// Each step sees what it sees under `run_with`: its own start time and PC on the bus, and
    /// the recent trace records it. Only bookkeeping that has nothing to do before the last
    /// step moves to the end of the run: until then no device ran and nothing was queued, so
    /// syncing the bus was a no-op, and no event was due, so advancing time fired nothing.
    fn run_batch(&mut self, end: u64) -> Option<StopReason> {
        self.stopped_at = None;
        self.batched += 1;
        let start = self.sched.now;
        let limit = self.sched.next_deadline().map_or(end, |d| d.min(end));
        let recent = &mut self.recent;
        let run = self.cpu.run_steps(
            &mut self.bus,
            limit.saturating_sub(start),
            |bus, pc, secure, elapsed| {
                let now = start + elapsed;
                recent.push(RecentStep {
                    cycles: now,
                    pc,
                    secure,
                });
                bus.pc_hint = pc;
                bus.now = now;
            },
            MemoryMap::device_activity,
        );
        self.sync_bus();
        let (cycles, sleep, stop) = match run.last {
            StepResult::Executed { .. } => (run.cycles, None, None),
            // The step does not report its cycles; WFI/WFE cost 1.
            StepResult::Sleep(kind) => (run.cycles + 1, Some(kind), None),
            StepResult::Bkpt(imm) => (run.cycles, None, Some(StopReason::Bkpt { pc: run.pc, imm })),
            StepResult::Lockup => (run.cycles, None, Some(StopReason::Lockup { pc: run.pc })),
        };
        self.awake_cycles += cycles;
        self.advance(cycles);
        if let Some(kind) = sleep {
            self.sleep = Some(kind);
            self.wake_if_due();
        }
        if self.cpu.take_sysreset_request() {
            self.system_reset();
        }
        stop
    }

    /// One step of `run_with` on the awake CPU: stop where the goal is reached, at a breakpoint
    /// or at an exec watchpoint (unless the last run stopped there), call `before_step`, step,
    /// then stop if the step matched a read/write watchpoint.
    #[inline(always)]
    fn checked_step(
        &mut self,
        before_step: &mut impl FnMut(&mut Self) -> ControlFlow<()>,
    ) -> Option<StopReason> {
        let pc = self.cpu.regs.pc;
        let resumed = self.stopped_at.take() == Some(pc);
        if !resumed && self.goal.is_some() && self.goal_reached(pc) {
            self.goal = None;
            self.stopped_at = Some(pc);
            return Some(StopReason::Goal { pc });
        }
        if !resumed && !self.breakpoints.is_empty() && self.breakpoints.contains(&pc) {
            self.stopped_at = Some(pc);
            return Some(StopReason::Breakpoint(pc));
        }
        if !resumed
            && !self.exec_watch.is_empty()
            && let Some(stop) = self.exec_stop(pc)
        {
            self.stopped_at = Some(pc);
            return Some(stop);
        }
        if before_step(self).is_break() {
            // Still on the breakpoint this run resumed from: the next run steps over it.
            if resumed {
                self.stopped_at = Some(pc);
            }
            return Some(StopReason::Halted);
        }
        let stop = self.step_cpu();
        self.with_data_hit(pc, stop)
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
            if let Some(stop) = self.checked_step(&mut before_step) {
                return stop;
            }
        }
    }

    /// Execute one CPU step (one instruction, or the entry to a pending exception), ignoring
    /// breakpoints and exec watchpoints. While asleep, wake if a wake-up is due (and then
    /// step), else jump to the next wake-up time without stepping. Returns `Stepped` unless the
    /// step locked up, hit a guest `BKPT` or matched a read/write watchpoint
    /// ([`StopReason::Watchpoint`]), or the CPU is asleep with nothing left to wake it (`Idle`:
    /// nothing changes).
    pub fn step(&mut self) -> StopReason {
        if self.asleep() {
            if let Some(stop) = self.idle(u64::MAX) {
                return stop;
            }
            if self.asleep() {
                return StopReason::Stepped;
            }
        }
        let pc = self.cpu.regs.pc;
        let stop = self.step_cpu();
        self.with_data_hit(pc, stop).unwrap_or(StopReason::Stepped)
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
                self.awake_cycles += u64::from(cycles);
                self.advance(u64::from(cycles));
                None
            }
            StepResult::Sleep(kind) => {
                // The step does not report its cycles; WFI/WFE cost 1.
                self.awake_cycles += 1;
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
    #[inline(never)]
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
    #[inline(never)]
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
    #[cold]
    #[inline(never)]
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
