//! The interface between the GUI and an emulated machine.

pub mod fake;
pub mod machine;

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
}

impl RunStatus {
    /// True for the statuses that stop free-running emulation.
    pub fn is_stop(self) -> bool {
        matches!(
            self,
            RunStatus::Breakpoint(_) | RunStatus::Lockup { .. } | RunStatus::Bkpt { .. }
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
    /// Backlight level in `0.0..=1.0`; the LCD view dims the frame by it.
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
    /// Writes flash (non-volatile state) to disk; a no-op where there is none.
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
}
