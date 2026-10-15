//! Factory provisioning: make a fresh unit boot straight to the OS (`docs/hardware/boot-flow.md`
//! §3.5).
//!
//! A fresh flash has no ITS UID 0x1F61E867_8CE70001 ("boot to OS"), so the BSP starts its
//! diagnostic app (the Self-Test menu) on every power-on. The factory sets that flag with the
//! BSP's own Engineering menu; [`provision`] does the same with the keypad, as a person would:
//!
//! 1. power on with ALPHA + SIN + ENTER held (boot combo 9, `keypad.md` "Boot key combos");
//! 2. press ENTER on the unit info screen: the diagnostic app opens the Engineering menu;
//! 3. press 2 ("Boot Select: Self-Test") once: the BSP writes the flag and the line reads
//!    "Boot Select: Calc OS". 2 toggles the flag, so it is never pressed a second time: the
//!    label is polled until it changes, for up to `TOGGLE_POLLS` × `SETTLE_MS` (3 s) of guest
//!    time;
//! 4. press 0 ("Exit this menu"): the BSP launches the OS;
//! 5. power-cycle and check that the next boot, with no key held, enters OS image 1.
//!
//! The flag is written through the secure ITS service into flash, so it lives in
//! [`Machine::nv_state`] (a flash file). Nothing here writes ITS or flash directly.
//!
//! The waits are tied to BSP 7.1.0.4413 (OS bundle 7.1.0.4421) code addresses, used as
//! breakpoints, and to the Engineering menu object in its RAM. Another BSP version needs these
//! constants checked; the menu title and line labels are verified before any key is pressed.

use crate::{CPU_HZ, Key, Machine, StopReason};

/// Boot combo 9, the Engineering menu (`keypad.md` "Boot key combos").
pub const ENGINEERING_COMBO: [&str; 3] = ["alpha", "sin", "enter"];

/// BSP main thread, just after `r4 = get_combo()` (0x0015_D482: `bl`, `mov r4, r0`).
const AFTER_GET_COMBO: u32 = 0x0015_D488;
/// `get_combo()` result for the Engineering combo.
const COMBO_ENGINEERING: u32 = 9;
/// The diagnostic app's entry: it shows the unit info screen and waits for ENTER.
const DIAG_APP_ENTRY: u32 = 0x0011_7AFC;
/// In the diagnostic app's menu chooser (0x0011_7BE0), just after it read ITS UID ..0004 into
/// r0: 2 opens the Engineering menu, anything else the Self-Test menu.
const MENU_CHOSEN: u32 = 0x0011_7BE6;
/// UID ..0004 value that opens the Engineering menu.
const DIAG_MODE_ENGINEERING: u32 = 2;
/// The Engineering menu object in BSP RAM: a title pointer at +0x0C, then items of 0x14
/// bytes from +0x24, each starting with its label pointer.
const ENGINEERING_MENU: u32 = 0x2001_0EF4;
/// Offset of the title pointer in a menu object.
const MENU_TITLE: u32 = 0x0C;
/// Offset of the label pointer of item 2, Boot Select (rewritten by NS 0x0016_8808).
const BOOT_SELECT_LABEL: u32 = 0x38;
const TITLE: &str = "Engineering";
const BOOT_SELF_TEST: &str = "Boot Select: Self-Test";
const BOOT_CALC_OS: &str = "Boot Select: Calc OS";
/// Image 1's reset handler, which the BSP's launch code `blx`es to (`boot-flow.md` §3.4).
pub const IMAGE1_ENTRY: u32 = 0x0044_BC90;

/// Cycles per millisecond of guest time.
const MS: u64 = CPU_HZ / 1000;
/// How long a key press is held, and the gap after it.
const TAP_MS: u64 = 50;
/// Guest time for the BSP to redraw after a key.
const SETTLE_MS: u64 = 500;
/// ENTER on the info screen is retried this often, this many times.
const ENTER_EVERY_MS: u64 = 500;
const ENTER_TRIES: u32 = 20;
/// After the one press of 2, Boot Select is read this many times, [`SETTLE_MS`] apart; it must
/// read Calc OS by the last read. 2 is not pressed again: a second press toggles the flag back.
const TOGGLE_POLLS: u32 = 6;
/// Budget from power-on to `get_combo` (measured about 92 M cycles).
const COMBO_BUDGET: u64 = 1_000_000_000;
/// Budget from `get_combo` to the diagnostic app (measured about 18 M cycles).
const APP_BUDGET: u64 = 1_000_000_000;
/// Budget from Exit to image 1 (measured about 160 M cycles: it validates image 1).
const LAUNCH_BUDGET: u64 = 1_500_000_000;
/// Budget from power-on to image 1 on the provisioned unit (measured about 300 M cycles).
const VERIFY_BUDGET: u64 = 1_500_000_000;

/// What [`provision`] did. Cycles count from the power-on of their boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProvisionReport {
    /// Provisioning boot: when the BSP matched the Engineering combo.
    pub combo_cycle: u64,
    /// Provisioning boot: when the diagnostic app chose the Engineering menu.
    pub menu_cycle: u64,
    /// Whether Boot Select had to be switched; false when it already read Calc OS (the unit
    /// was provisioned before).
    pub toggled: bool,
    /// Provisioning boot: when "Exit this menu" launched image 1.
    pub launch_cycle: u64,
    /// Verification boot (no key held): when image 1 was entered.
    pub boot_cycle: u64,
}

/// Why [`provision`] failed. The machine is left where it stopped.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// The machine stopped on its own (lockup, guest BKPT, idle with nothing to wake it).
    #[error("the machine stopped ({stop:?}) at cycle {cycle} while {stage}")]
    Stopped {
        stage: &'static str,
        stop: StopReason,
        cycle: u64,
    },
    /// A step did not happen within its budget.
    #[error("timed out at cycle {cycle} while {stage}")]
    Timeout { stage: &'static str, cycle: u64 },
    /// The BSP's boot-combo matcher did not see the Engineering combo.
    #[error("the BSP matched boot combo {combo}, not {COMBO_ENGINEERING} (Engineering)")]
    ComboNotMatched { combo: u32 },
    /// The diagnostic app chose the Self-Test menu (ITS UID ..0004 is not 2).
    #[error("the diagnostic app chose the Self-Test menu (diagnostic mode {mode})")]
    NotEngineering { mode: u32 },
    /// The menu object does not hold the expected text (another BSP version?).
    #[error("unexpected Engineering menu text: {text:?}")]
    UnexpectedMenu { text: Option<String> },
    /// Pressing 2 did not switch Boot Select to Calc OS.
    #[error(
        "Boot Select still reads {label:?} {} ms after pressing 2",
        u64::from(TOGGLE_POLLS) * SETTLE_MS
    )]
    BootSelectUnchanged { label: Option<String> },
    /// After the power cycle the BSP started the diagnostic app again: the flag did not
    /// persist.
    #[error("after the power cycle the BSP started the diagnostic app at cycle {cycle}")]
    NotPersisted { cycle: u64 },
}

/// Provision `m` (`docs/hardware/boot-flow.md` §3.5): power it on with the Engineering combo,
/// set Engineering → Boot Select to Calc OS, exit, then power-cycle and check that the boot
/// reaches [`IMAGE1_ENTRY`] with no key input. Whatever `m` was doing is lost (it is
/// power-cycled first); its flash, including what the run before stored, is kept.
///
/// On success `m` is stopped at the verification boot's [`IMAGE1_ENTRY`] with no breakpoints,
/// and [`Machine::nv_state`] is the provisioned flash to save. Provisioning an already
/// provisioned unit leaves Boot Select as it is (`toggled` false).
pub fn provision(m: &mut Machine) -> Result<ProvisionReport, ProvisionError> {
    // 1. Power on with the combo held until the BSP has read it.
    m.power_cycle();
    for name in ENGINEERING_COMBO {
        m.set_key(key(name), true);
    }
    m.breakpoints.insert(AFTER_GET_COMBO);
    run_to_breakpoint(m, COMBO_BUDGET, "waiting for the boot combo")?;
    let combo = m.cpu.regs.r[4];
    let combo_cycle = m.sched.now;
    for name in ENGINEERING_COMBO {
        m.set_key(key(name), false);
    }
    if combo != COMBO_ENGINEERING {
        return Err(ProvisionError::ComboNotMatched { combo });
    }

    // 2. The diagnostic app's info screen; ENTER until it chooses its menu.
    m.breakpoints.clear();
    m.breakpoints.insert(DIAG_APP_ENTRY);
    run_to_breakpoint(m, APP_BUDGET, "waiting for the diagnostic app")?;
    m.breakpoints.clear();
    m.breakpoints.insert(MENU_CHOSEN);
    let mut chosen = false;
    for _ in 0..ENTER_TRIES {
        if run_for(m, ENTER_EVERY_MS * MS, "waiting for the info screen")?.is_some()
            || tap(m, "enter", "pressing ENTER on the info screen")?.is_some()
        {
            chosen = true;
            break;
        }
    }
    if !chosen {
        return Err(timeout(m, "pressing ENTER on the info screen"));
    }
    let mode = m.cpu.regs.r[0];
    let menu_cycle = m.sched.now;
    m.breakpoints.clear();
    if mode != DIAG_MODE_ENGINEERING {
        return Err(ProvisionError::NotEngineering { mode });
    }
    run_for(m, SETTLE_MS * MS, "drawing the Engineering menu")?;
    let title = menu_text(m, MENU_TITLE);
    if title.as_deref() != Some(TITLE) {
        return Err(ProvisionError::UnexpectedMenu { text: title });
    }

    // 3. Boot Select: Self-Test → Calc OS.
    let mut label = menu_text(m, BOOT_SELECT_LABEL);
    let toggled = match label.as_deref() {
        Some(BOOT_CALC_OS) => false,
        Some(BOOT_SELF_TEST) => {
            tap(m, "2", "pressing 2 (Boot Select)")?;
            for _ in 0..TOGGLE_POLLS {
                run_for(m, SETTLE_MS * MS, "redrawing Boot Select")?;
                label = menu_text(m, BOOT_SELECT_LABEL);
                if label.as_deref() == Some(BOOT_CALC_OS) {
                    break;
                }
            }
            if label.as_deref() != Some(BOOT_CALC_OS) {
                return Err(ProvisionError::BootSelectUnchanged { label });
            }
            true
        }
        _ => return Err(ProvisionError::UnexpectedMenu { text: label }),
    };

    // 4. Exit: the BSP launches image 1.
    m.breakpoints.insert(IMAGE1_ENTRY);
    if tap(m, "0", "pressing 0 (Exit this menu)")?.is_none() {
        run_to_breakpoint(m, LAUNCH_BUDGET, "waiting for the OS launch")?;
    }
    let launch_cycle = m.sched.now;

    // 5. Power-cycle with no key held: the BSP must launch the OS on its own.
    m.power_cycle();
    m.breakpoints.insert(IMAGE1_ENTRY);
    m.breakpoints.insert(DIAG_APP_ENTRY);
    let pc = run_to_breakpoint(m, VERIFY_BUDGET, "verifying the boot to the OS")?;
    m.breakpoints.clear();
    if pc != IMAGE1_ENTRY {
        return Err(ProvisionError::NotPersisted { cycle: m.sched.now });
    }
    Ok(ProvisionReport {
        combo_cycle,
        menu_cycle,
        toggled,
        launch_cycle,
        boot_cycle: m.sched.now,
    })
}

/// Run up to `cycles`; `Some(pc)` if a breakpoint stopped the run early.
fn run_for(
    m: &mut Machine,
    cycles: u64,
    stage: &'static str,
) -> Result<Option<u32>, ProvisionError> {
    let end = m.sched.now.saturating_add(cycles);
    while m.sched.now < end {
        match m.run(end - m.sched.now) {
            StopReason::CycleLimit => {}
            StopReason::Breakpoint(pc) => return Ok(Some(pc)),
            stop => {
                return Err(ProvisionError::Stopped {
                    stage,
                    stop,
                    cycle: m.sched.now,
                });
            }
        }
    }
    Ok(None)
}

/// Run until a breakpoint, at most `budget` cycles; the breakpoint's address.
fn run_to_breakpoint(
    m: &mut Machine,
    budget: u64,
    stage: &'static str,
) -> Result<u32, ProvisionError> {
    run_for(m, budget, stage)?.ok_or_else(|| timeout(m, stage))
}

fn timeout(m: &Machine, stage: &'static str) -> ProvisionError {
    ProvisionError::Timeout {
        stage,
        cycle: m.sched.now,
    }
}

/// Press the key `name` for [`TAP_MS`], release it, wait [`TAP_MS`]. A breakpoint ends it early (the key
/// is released first): `Some(pc)`.
fn tap(m: &mut Machine, name: &str, stage: &'static str) -> Result<Option<u32>, ProvisionError> {
    let key = key(name);
    m.set_key(key, true);
    let held = run_for(m, TAP_MS * MS, stage);
    m.set_key(key, false);
    if let Some(pc) = held? {
        return Ok(Some(pc));
    }
    run_for(m, TAP_MS * MS, stage)
}

/// The NUL-terminated ASCII string the Engineering menu's pointer at `offset` points to;
/// `None` if a byte is unreadable, not printable ASCII, or no NUL ends the string within 64
/// characters (at most 65 bytes are read: 64 characters and the NUL).
fn menu_text(m: &Machine, offset: u32) -> Option<String> {
    let ptr = m.bus.peek(ENGINEERING_MENU + offset, 4)?;
    let mut s = String::new();
    for i in 0..=64u32 {
        let b = u8::try_from(m.bus.peek(ptr.wrapping_add(i), 1)?).ok()?;
        match b {
            0 => return Some(s),
            0x20..=0x7E => s.push(char::from(b)),
            _ => return None,
        }
    }
    None
}

/// The key called `name` in [`evo_hw::dev::keypad::KEY_NAMES`]; this module names only keys
/// in that table (`key_names_resolve`).
fn key(name: &str) -> Key {
    Key::from_name(name).unwrap_or_else(|| unreachable!("{name} is not in KEY_NAMES"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_resolve() {
        for name in ENGINEERING_COMBO.iter().chain(&["enter", "2", "0"]) {
            assert!(Key::from_name(name).is_some(), "{name}");
        }
    }
}
