//! [`Placeholder`]: a backend with nothing behind it.
//!
//! It stands in while the real backend is away on the provisioning thread, and when no ROM
//! is loaded. No display, no time, nothing to change.

use std::path::Path;

use super::{
    BackendError, BusRecord, CpuView, DeviceView, DisasmLine, EmuBackend, Frame, Goal,
    HardwareView, HwChange, KeyEvent, MemRegion, RunStatus, UartId, UnmappedRow, Watchpoint,
};
use crate::keypad::KeyId;

/// An inert backend that reports a name and a state tag and does nothing else.
#[derive(Clone, Debug)]
pub struct Placeholder {
    name: String,
    tag: String,
}

impl Placeholder {
    /// A placeholder named `name` whose state tag is `tag` (the real backend's, while it
    /// is away).
    pub fn new(name: &str, tag: &str) -> Self {
        Self {
            name: name.to_owned(),
            tag: tag.to_owned(),
        }
    }

    /// The backend while no ROM is loaded: name "No ROM", tag "none".
    pub fn no_rom() -> Self {
        Self::new("No ROM", "none")
    }
}

impl EmuBackend for Placeholder {
    fn name(&self) -> &str {
        &self.name
    }
    fn state_tag(&self) -> String {
        self.tag.clone()
    }
    fn framebuffer(&mut self) -> Option<&Frame> {
        None
    }
    fn backlight(&self) -> f32 {
        0.0
    }
    fn set_key(&mut self, _key: KeyId, _pressed: bool) {}
    fn run_for(&mut self, _cycles: u64) -> RunStatus {
        RunStatus::Idle
    }
    fn step(&mut self) -> RunStatus {
        RunStatus::Idle
    }
    fn cycles(&self) -> u64 {
        0
    }
    fn take_uart(&mut self) -> Vec<u8> {
        Vec::new()
    }
    fn save_state(&self) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("saving state without a machine"))
    }
    fn load_state(&mut self, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("loading state without a machine"))
    }
    fn reset(&mut self) {}
    fn persist(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
    fn needs_provision(&self) -> bool {
        false
    }
    fn provision(&mut self) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("provisioning without a machine"))
    }
    fn cpu(&self) -> CpuView {
        CpuView::default()
    }
    fn peek(&self, _addr: u32, len: usize) -> Vec<Option<u8>> {
        vec![None; len]
    }
    fn disasm(&self, _addr: u32, _count: usize) -> Vec<DisasmLine> {
        Vec::new()
    }
    fn unmapped(&self) -> Vec<UnmappedRow> {
        Vec::new()
    }
    fn breakpoints(&self) -> Vec<u32> {
        Vec::new()
    }
    fn toggle_breakpoint(&mut self, _addr: u32) {}
    fn poke(&mut self, _addr: u32, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "writing memory without a machine",
        ))
    }
    fn memory_regions(&self) -> Vec<MemRegion> {
        Vec::new()
    }
    fn set_core_reg(&mut self, _index: usize, _value: u32) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "writing registers without a machine",
        ))
    }
    fn set_banked_reg(&mut self, _name: &str, _value: u32) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "writing registers without a machine",
        ))
    }
    fn set_breakpoints(&mut self, _addrs: Vec<u32>) {}
    fn begin_goal(&mut self, _goal: Goal) -> Result<Option<RunStatus>, BackendError> {
        Err(BackendError::Unsupported(
            "debugger goals without a machine",
        ))
    }
    fn cancel_goal(&mut self) {}
    fn watchpoints(&self) -> Vec<Watchpoint> {
        Vec::new()
    }
    fn set_watchpoints(&mut self, _w: Vec<Watchpoint>) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("watchpoints without a machine"))
    }
    fn set_bus_monitor(&mut self, _range: Option<(u32, u32)>) {}
    fn bus_records(&self) -> Vec<BusRecord> {
        Vec::new()
    }
    fn clear_bus_records(&mut self) {}
    fn peripherals(&self) -> Vec<DeviceView> {
        Vec::new()
    }
    fn take_uart2(&mut self) -> Vec<u8> {
        Vec::new()
    }
    fn send_uart(&mut self, _port: UartId, _bytes: &[u8]) {}
    fn take_key_events(&mut self) -> Vec<KeyEvent> {
        Vec::new()
    }
    fn hardware(&self) -> HardwareView {
        HardwareView {
            backlight: 0.0,
            charger_line: false,
            battery_ok: false,
            usb_attached: false,
            power_hold: false,
            deep_sleep: false,
            rtc_seconds: 0,
        }
    }
    fn set_hardware(&mut self, _change: HwChange) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "hardware changes without a machine",
        ))
    }
    fn export_ram(&self) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("exporting RAM without a machine"))
    }
    fn import_ram(&mut self, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("importing RAM without a machine"))
    }
    fn export_flash(&self, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "exporting flash without a machine",
        ))
    }
    fn import_flash(&mut self, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "importing flash without a machine",
        ))
    }
}
