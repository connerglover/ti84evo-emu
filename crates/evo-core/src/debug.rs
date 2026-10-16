//! Debugger controls on [`Machine`]: run goals (step over, step out, run to),
//! side-effect-free peripheral inspection ([`Machine::peripherals`]), register writes
//! ([`Machine::write_reg`]), setting the RTC and reading the power state.
//!
//! A goal is armed by one of the `arm_*` methods and checked by [`Machine::run`] before every
//! step, ahead of breakpoints and exec watchpoints, except on the instruction the run resumes
//! from. Reaching it disarms it and stops the run with [`StopReason::Goal`] before the
//! instruction at its PC. A run that ends any other way (budget, breakpoint, watchpoint, ...)
//! keeps it armed, so the next run goes on towards it. A goal is a debugger setting, not
//! machine state: it is not saved, and [`Machine::load_state`] and [`Machine::power_cycle`]
//! clear it.
//!
//! [`StopReason::Goal`]: crate::StopReason::Goal

use armv8m::scs::nvic::NUM_IRQS;
use armv8m::{Banked, Instr, Sec, Xpsr, decode16, decode32};
use evo_hw::dev::gpio::Gpio;
use evo_hw::dev::rtc::Rtc;
use evo_hw::dev::sysctl::Spctrl;
use evo_hw::{DeviceInspect, RegView};

use crate::{Machine, StopReason};

/// LR values from here up are `EXC_RETURN` values: in a handler, `bx lr` returns from the
/// exception rather than to an address.
const EXC_RETURN_MIN: u32 = 0xFF00_0000;

/// Where a debugger run should stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunGoal {
    /// The next instruction is at `pc` with the stack pointer at `sp` or above: the return
    /// of the call made at `sp`. A deeper (recursive) call returning to the same `pc` has a
    /// smaller stack pointer, so it does not count.
    Return { pc: u32, sp: u32 },
    /// Fewer than `depth` exceptions are active: the handler that was running returned.
    ExceptionReturn { depth: u32 },
    /// The next instruction is at this address (Thumb bit clear).
    Address(u32),
}

impl Machine {
    /// Step over the instruction at the PC. A call (`BL`, `BLX <reg>`, `BLXNS`) arms a
    /// [`RunGoal::Return`] to the instruction after it at the current stack pointer and returns
    /// `None`; the call runs with the next [`run`](Self::run). Anything else (and an
    /// instruction that cannot be read) is executed now with one [`step`](Self::step), and
    /// this returns `Some` with that step's stop reason. Either way the old goal is replaced.
    pub fn arm_step_over(&mut self) -> Option<StopReason> {
        let pc = self.cpu.regs.pc;
        if let Some(len) = self.call_len(pc) {
            self.goal = Some(RunGoal::Return {
                pc: pc.wrapping_add(len),
                sp: self.cpu.regs.sp(),
            });
            return None;
        }
        self.goal = None;
        Some(self.step())
    }

    /// Run until the current function or handler returns. In a handler with an `EXC_RETURN`
    /// value in LR: until fewer exceptions are active than now ([`RunGoal::ExceptionReturn`]).
    /// Otherwise: until the next instruction is at LR (Thumb bit clear) with the stack pointer
    /// at or above the current one ([`RunGoal::Return`]). LR must still hold the return
    /// address; a function that has called something since it was entered has overwritten it.
    pub fn arm_step_out(&mut self) {
        let lr = self.cpu.regs.lr;
        self.goal = Some(if lr >= EXC_RETURN_MIN && self.cpu.regs.handler_mode() {
            RunGoal::ExceptionReturn {
                depth: self.cpu.active_exceptions(),
            }
        } else {
            RunGoal::Return {
                pc: lr & !1,
                sp: self.cpu.regs.sp(),
            }
        });
    }

    /// Run until the next instruction is at `addr` (the Thumb bit is ignored). Armed at the
    /// instruction a run resumes from, the run goes round until it comes back.
    pub fn arm_run_to(&mut self, addr: u32) {
        self.goal = Some(RunGoal::Address(addr & !1));
    }

    /// Cancel the goal, if any.
    pub fn disarm(&mut self) {
        self.goal = None;
    }

    /// The armed goal.
    pub fn goal(&self) -> Option<RunGoal> {
        self.goal
    }

    /// Whether the armed goal is reached with the next instruction at `pc`. Called before
    /// every checked step while a goal is armed; batched runs never call it.
    pub(crate) fn goal_reached(&self, pc: u32) -> bool {
        match self.goal {
            None => false,
            Some(RunGoal::Return { pc: at, sp }) => pc == at && self.cpu.regs.sp() >= sp,
            Some(RunGoal::ExceptionReturn { depth }) => self.cpu.active_exceptions() < depth,
            Some(RunGoal::Address(at)) => pc == at,
        }
    }

    /// The length of the call instruction at `pc`, if it is one. Read without touching
    /// devices or the unmapped log.
    fn call_len(&self, pc: u32) -> Option<u32> {
        let hw1 = self.bus.peek(pc, 2)? as u16;
        if armv8m::decode::is_32bit(hw1) {
            let hw2 = self.bus.peek(pc.wrapping_add(2), 2)? as u16;
            matches!(decode32(hw1, hw2), Instr::Bl { .. }).then_some(4)
        } else {
            matches!(decode16(hw1), Instr::Blx { .. } | Instr::Blxns { .. }).then_some(2)
        }
    }
}

/// Base of the Secure SysTick registers (`SYST_CSR`); the Non-secure view is at the same
/// address through the Non-secure alias, listed under its own name.
const SYSTICK_BASE: u32 = 0xE000_E010;
/// Base of the NVIC registers (`NVIC_ISER0`).
const NVIC_BASE: u32 = 0xE000_E100;

impl Machine {
    /// Every peripheral's registers as a debugger shows them, without side effects: the
    /// memory-mapped devices with something to show, in the order they are attached
    /// ([`evo_hw::MemoryMap::inspect_devices`] at `sched.now`), then `systick_s` and
    /// `systick_ns` (rows `csr`, `rvr`, `cvr`) and `nvic`: one row `irq{n}` (offset `n`) per
    /// IRQ that is enabled, pending, active or has its line asserted, with value
    /// `enabled | pending << 1 | active << 2 | level << 3` and those words as the note.
    pub fn peripherals(&self) -> Vec<DeviceInspect> {
        let mut devs = self.bus.inspect_devices(self.sched.now);
        for (name, sec) in [("systick_s", Sec::Secure), ("systick_ns", Sec::NonSecure)] {
            let st = self.cpu.scs.systick.get(sec);
            devs.push(DeviceInspect {
                name,
                base: SYSTICK_BASE,
                regs: [("csr", 0x0), ("rvr", 0x4), ("cvr", 0x8)]
                    .into_iter()
                    .map(|(n, off)| RegView::new(n, off, st.peek(off)))
                    .collect(),
            });
        }
        let nvic = &self.cpu.scs.nvic;
        let regs = (0..NUM_IRQS)
            .filter_map(|irq| {
                let bits = [
                    ("enabled", nvic.is_enabled(irq)),
                    ("pending", nvic.is_pending(irq)),
                    ("active", nvic.is_active(irq)),
                    ("level", nvic.level(irq)),
                ];
                let value = bits
                    .iter()
                    .enumerate()
                    .fold(0, |v, (i, &(_, set))| v | u32::from(set) << i);
                (value != 0).then(|| {
                    let words: Vec<_> = bits.iter().filter(|b| b.1).map(|b| b.0).collect();
                    // `irq < NUM_IRQS` fits in u32.
                    RegView::new(format!("irq{irq}"), irq as u32, value).with_note(words.join(" "))
                })
            })
            .collect();
        devs.push(DeviceInspect {
            name: "nvic",
            base: NVIC_BASE,
            regs,
        });
        devs
    }
}

/// Why [`Machine::write_reg`] refused a write.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegError {
    #[error("no register {0}")]
    Unknown(String),
}

/// What the unit's power logic shows: whether the guest holds its own power on, and whether
/// it armed deep sleep (the OS switched the unit "off").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerState {
    /// GPIO pin 1 is an output driven high (`Gpio::power_hold`).
    pub hold: bool,
    /// SPCTRL +0xD4 bit 4 is set (`Spctrl::deep_sleep_armed`).
    pub deep_sleep: bool,
}

/// One of a register's two security banks, from a `_s` / `_ns` name suffix.
fn bank<T>(b: &mut Banked<T>, ns: bool) -> &mut T {
    if ns { &mut b.ns } else { &mut b.s }
}

impl Machine {
    /// Write a CPU register for the debugger. Names (lower case): `r0`-`r12`, `sp` (the
    /// stack pointer in use now), `lr`, `pc`, `xpsr`, and the banked `msp psp msplim psplim
    /// control primask basepri faultmask`, each with `_s` or `_ns`. `pc` drops the Thumb bit,
    /// ends a WFI/WFE sleep and forgets the breakpoint the last run stopped at (a breakpoint at
    /// the new PC stops the next run before it executes). The mask registers take bit 0
    /// (`primask`, `faultmask`) or the low byte (`basepri`). An unknown name changes nothing.
    pub fn write_reg(&mut self, name: &str, v: u32) -> Result<(), RegError> {
        let regs = &mut self.cpu.regs;
        if let Some(n) = name.strip_prefix('r')
            && let Ok(i) = n.parse::<usize>()
            && i < regs.r.len()
            && n == i.to_string()
        {
            regs.r[i] = v;
            return Ok(());
        }
        match name {
            "sp" => regs.set_sp(v),
            "lr" => regs.lr = v,
            "pc" => {
                regs.pc = v & !1;
                self.stopped_at = None;
                self.wake();
            }
            "xpsr" => regs.xpsr = Xpsr::from_u32(v),
            _ => {
                let unknown = || RegError::Unknown(name.to_owned());
                let (base, ns) = match name.rsplit_once('_') {
                    Some((base, "s")) => (base, false),
                    Some((base, "ns")) => (base, true),
                    _ => return Err(unknown()),
                };
                match base {
                    "msp" => *bank(&mut regs.msp, ns) = v,
                    "psp" => *bank(&mut regs.psp, ns) = v,
                    "msplim" => *bank(&mut regs.msplim, ns) = v,
                    "psplim" => *bank(&mut regs.psplim, ns) = v,
                    "control" => *bank(&mut regs.control, ns) = v,
                    "primask" => *bank(&mut regs.primask, ns) = v & 1 != 0,
                    "faultmask" => *bank(&mut regs.faultmask, ns) = v & 1 != 0,
                    "basepri" => *bank(&mut regs.basepri, ns) = v as u8,
                    _ => return Err(unknown()),
                }
            }
        }
        Ok(())
    }

    /// Set the RTC to `seconds_of_day % 86_400` now; it counts on from there.
    pub fn set_rtc(&mut self, seconds_of_day: u32) {
        self.with_dev("rtc", |r: &mut Rtc, ctx| {
            r.set_seconds_of_day(seconds_of_day, ctx.now)
        });
    }

    /// The RTC's seconds of day now (0..86_400).
    pub fn rtc_seconds(&self) -> u32 {
        self.bus
            .device::<Rtc>("rtc")
            .map_or(0, |r| r.seconds_of_day(self.sched.now))
    }

    /// The power hold and deep-sleep flags, read without side effects.
    pub fn power_state(&self) -> PowerState {
        PowerState {
            hold: self
                .bus
                .device::<Gpio>("gpio")
                .is_some_and(Gpio::power_hold),
            deep_sleep: self
                .bus
                .device::<Spctrl>("spctrl")
                .is_some_and(Spctrl::deep_sleep_armed),
        }
    }
}
