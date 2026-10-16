//! The interface between the GUI and an emulated machine.

pub mod fake;
pub mod machine;
pub mod placeholder;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::keypad::KeyId;

/// The emulated core clock in Hz (spec section 1, discovery log).
pub const CLOCK_HZ: u64 = 156_000_000;

/// One LCD frame, RGBA8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// True when the size is non-zero and `rgba` holds exactly `width * height` pixels.
    pub fn is_valid(&self) -> bool {
        if self.width == 0 || self.height == 0 {
            return false;
        }
        (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|px| px.checked_mul(4))
            .is_some_and(|len| len == self.rgba.len())
    }
}

/// Why a run returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// The cycle budget was used up.
    Running,
    /// The guest is asleep (WFI/WFE) and nothing will wake it within the budget.
    Idle,
    /// Execution reached a breakpoint at this address.
    Breakpoint(u32),
    /// The core is locked up.
    Lockup { pc: u32 },
    /// The guest executed `BKPT #imm`.
    Bkpt { pc: u32, imm: u8 },
    /// A watchpoint matched an access to `addr` by the instruction at `pc`. A data
    /// watchpoint stops after the access, an exec watchpoint before the instruction.
    Watchpoint {
        addr: u32,
        dir: AccessDir,
        value: u32,
        pc: u32,
    },
    /// A [`Goal`] was reached; the core stopped at `pc`.
    Goal { pc: u32 },
}

impl RunStatus {
    /// True for the statuses that stop free-running emulation.
    pub fn is_stop(self) -> bool {
        matches!(
            self,
            RunStatus::Breakpoint(_)
                | RunStatus::Lockup { .. }
                | RunStatus::Bkpt { .. }
                | RunStatus::Watchpoint { .. }
                | RunStatus::Goal { .. }
        )
    }
}

/// A snapshot of the CPU registers for the debug panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuView {
    /// r0..r12, sp, lr, pc.
    pub core: [u32; 16],
    pub xpsr: u32,
    pub secure: bool,
    pub handler: bool,
    /// Banked and special registers, by display name.
    pub banked: Vec<(&'static str, u32)>,
}

/// One disassembled instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisasmLine {
    pub addr: u32,
    /// 2 or 4 bytes.
    pub len: u8,
    /// The halfword, or `hw1 << 16 | hw2` when `len` is 4.
    pub raw: u32,
    pub text: String,
}

/// One aggregated access to an address no device models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmappedRow {
    pub addr: u32,
    pub width: u8,
    pub kind: &'static str,
    pub value: Option<u32>,
    pub pc: u32,
    pub secure: bool,
    pub count: u64,
}

/// A UART the GUI can talk to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UartId {
    /// The TF-M / BSP console.
    Uart0,
    /// The link transport.
    Uart2,
}

/// The direction of a bus access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessDir {
    Read,
    Write,
    Exec,
}

impl AccessDir {
    /// `"read"`, `"write"` or `"exec"`.
    pub fn as_str(self) -> &'static str {
        match self {
            AccessDir::Read => "read",
            AccessDir::Write => "write",
            AccessDir::Exec => "exec",
        }
    }
}

/// Which accesses a watchpoint stops on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchKinds {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

/// A watched address range: `start..=last()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watchpoint {
    pub start: u32,
    pub len: u32,
    pub kinds: WatchKinds,
}

impl Watchpoint {
    /// True when the range is non-empty, ends at or below `u32::MAX` and watches some kind.
    pub fn is_valid(&self) -> bool {
        let k = self.kinds;
        self.len >= 1
            && self.start.checked_add(self.len - 1).is_some()
            && (k.read || k.write || k.exec)
    }

    /// The last watched address (`start + len - 1`, saturating; `start` when `len` is 0).
    pub fn last(&self) -> u32 {
        self.start.saturating_add(self.len.saturating_sub(1))
    }
}

/// One access the bus monitor recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusRecord {
    pub addr: u32,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub dir: AccessDir,
    pub value: u32,
    pub pc: u32,
    pub cycle: u64,
}

/// One device register in the Peripherals view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegRow {
    pub name: String,
    /// From the device base.
    pub offset: u32,
    pub value: u32,
    pub note: Option<String>,
}

/// One device and its registers in the Peripherals view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceView {
    pub name: String,
    pub base: u32,
    pub regs: Vec<RegRow>,
}

/// A debugger run that ends by itself (see [`EmuBackend::begin_goal`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Goal {
    StepOver,
    StepOut,
    RunTo(u32),
}

/// One key change, at the cycle it reached the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: KeyId,
    pub pressed: bool,
    pub cycle: u64,
}

/// A named memory range for the memory views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemRegion {
    pub name: String,
    pub start: u32,
    pub len: u32,
}

/// The board conditions the Hardware panel shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HardwareView {
    /// `0.0..=1.0`.
    pub backlight: f32,
    pub charger_line: bool,
    pub battery_ok: bool,
    pub usb_attached: bool,
    pub power_hold: bool,
    pub deep_sleep: bool,
    /// Seconds since midnight, below 86 400.
    pub rtc_seconds: u32,
}

/// One change the Hardware panel makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwChange {
    ChargerLine(bool),
    BatteryOk(bool),
    UsbAttached(bool),
    RtcSeconds(u32),
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("{0} is not supported by this backend yet")]
    Unsupported(&'static str),
    #[error("invalid save state: {0}")]
    BadState(String),
    #[error("could not save flash: {0}")]
    Persist(String),
    #[error("provisioning failed: {0}")]
    Provision(String),
    #[error("address 0x{0:08x} is not writable")]
    Unwritable(u32),
    #[error("no register {0}")]
    BadRegister(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
}

/// An emulated machine as the GUI sees it.
///
/// There are no default methods: every backend states each capability explicitly.
pub trait EmuBackend: Send {
    /// Display name (header).
    fn name(&self) -> &str;
    /// Identity stored in slot files; equal tags mean compatible states.
    fn state_tag(&self) -> String;
    /// The current LCD frame; `None` when the display is off or not modeled.
    fn framebuffer(&mut self) -> Option<&Frame>;
    /// Backlight level (PWM duty) in `0.0..=1.0`; the LCD view dims the frame by
    /// [`backlight_tint`](crate::lcd::backlight_tint) of it.
    fn backlight(&self) -> f32;
    fn set_key(&mut self, key: KeyId, pressed: bool);
    fn run_for(&mut self, cycles: u64) -> RunStatus;
    /// Runs one instruction, ignoring breakpoints.
    fn step(&mut self) -> RunStatus;
    fn cycles(&self) -> u64;
    /// Drains the bytes written since the last call.
    fn take_uart(&mut self) -> Vec<u8>;
    fn save_state(&self) -> Result<Vec<u8>, BackendError>;
    /// On `Err` the machine is unchanged.
    fn load_state(&mut self, bytes: &[u8]) -> Result<(), BackendError>;
    /// Resets the machine; flash is kept.
    fn reset(&mut self);
    /// Writes flash (non-volatile state) to disk; a no-op where there is none, while
    /// [`needs_provision`](Self::needs_provision) (so a failed first run is retried at the
    /// next launch) and when flash is unchanged since the last write. The app calls it from
    /// eframe's periodic save on the UI thread, so the unchanged case must be cheap.
    fn persist(&mut self) -> Result<(), BackendError>;
    /// True when the machine has never been set up to boot its OS (it started from an
    /// erased flash): the app then runs [`provision`](Self::provision) before anything else.
    fn needs_provision(&self) -> bool;
    /// Factory provisioning: sets the machine up so that it boots straight to its OS, and
    /// leaves it booting the OS. It can take many seconds, so the app calls it on a worker
    /// thread; it does not write flash to disk (the app persists afterwards). Breakpoints are
    /// kept. On `Err` the machine is left where provisioning stopped.
    fn provision(&mut self) -> Result<(), BackendError>;
    fn cpu(&self) -> CpuView;
    /// Reads memory without side effects; `None` for unreadable bytes.
    fn peek(&self, addr: u32, len: usize) -> Vec<Option<u8>>;
    fn disasm(&self, addr: u32, count: usize) -> Vec<DisasmLine>;
    /// Called only while the Unmapped tab is visible.
    fn unmapped(&self) -> Vec<UnmappedRow>;
    fn breakpoints(&self) -> Vec<u32>;
    fn toggle_breakpoint(&mut self, addr: u32);

    /// Writes `bytes` at `addr`: all of them, or (on `Err`) none.
    fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), BackendError>;
    /// The memory ranges the memory views offer.
    fn memory_regions(&self) -> Vec<MemRegion>;
    /// Sets core register `index`: 0..=15 as [`CpuView::core`], 16 = xPSR.
    fn set_core_reg(&mut self, index: usize, value: u32) -> Result<(), BackendError>;
    /// Sets the banked register named as in [`CpuView::banked`].
    fn set_banked_reg(&mut self, name: &str, value: u32) -> Result<(), BackendError>;
    /// Replaces every breakpoint.
    fn set_breakpoints(&mut self, addrs: Vec<u32>);
    /// Starts a goal: `Ok(None)` when armed (a later [`run_for`](Self::run_for) returns
    /// [`RunStatus::Goal`] when reached), `Ok(Some(status))` when one step completed it now.
    fn begin_goal(&mut self, goal: Goal) -> Result<Option<RunStatus>, BackendError>;
    /// Disarms the goal, if any.
    fn cancel_goal(&mut self);
    fn watchpoints(&self) -> Vec<Watchpoint>;
    /// Replaces every watchpoint; any invalid one is `Err(Invalid)` and changes nothing.
    fn set_watchpoints(&mut self, w: Vec<Watchpoint>) -> Result<(), BackendError>;
    /// Records the accesses to `(start, len)`; `None` stops recording and keeps the records.
    fn set_bus_monitor(&mut self, range: Option<(u32, u32)>);
    /// The recorded accesses, oldest first. Called only while the Bus monitor tab is visible.
    fn bus_records(&self) -> Vec<BusRecord>;
    fn clear_bus_records(&mut self);
    /// Called only while the Peripherals tab is visible.
    fn peripherals(&self) -> Vec<DeviceView>;
    /// Drains the bytes UART2 sent since the last call.
    fn take_uart2(&mut self) -> Vec<u8>;
    /// Feeds `bytes` to the receiver of `port`.
    fn send_uart(&mut self, port: UartId, bytes: &[u8]);
    /// Every [`set_key`](Self::set_key) since the last call, with its cycle.
    fn take_key_events(&mut self) -> Vec<KeyEvent>;
    fn hardware(&self) -> HardwareView;
    fn set_hardware(&mut self, change: HwChange) -> Result<(), BackendError>;
    /// The RAM image (`.evoram`).
    fn export_ram(&self) -> Result<Vec<u8>, BackendError>;
    /// On `Err` the machine is unchanged.
    fn import_ram(&mut self, bytes: &[u8]) -> Result<(), BackendError>;
    /// Writes the flash (non-volatile state) to `path`.
    fn export_flash(&self, path: &Path) -> Result<(), BackendError>;
    /// Replaces the flash with the file at `path`; on `Err` the machine is unchanged.
    fn import_flash(&mut self, path: &Path) -> Result<(), BackendError>;
}
