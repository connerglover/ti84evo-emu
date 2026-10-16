//! The debug panels: run controls, then one tab of registers, disassembly,
//! memory, UART output or unmapped accesses.

mod disasm;
mod memory;
mod regs;
mod uart;
mod unmapped;

use egui::{Button, ScrollArea, Sense};

pub use disasm::{DISASM_LINES, format_disasm};
pub use memory::DEFAULT_MEM_ADDR;
pub use uart::{UART_CAP, UartConsole};

use crate::backend::{EmuBackend, Goal, UartId};
use crate::logbuf::LOG_CAP;

/// Default width of the debug panel, in points; also how much the window
/// grows when the panel opens.
pub const DEBUG_W: f32 = 420.0;
/// Narrowest the debug panel may be dragged, in points (the window-width cap
/// wins when it is smaller).
pub const DEBUG_MIN_W: f32 = 240.0;

/// Parses a hex address: surrounding whitespace is trimmed, a `0x`/`0X`
/// prefix is optional and `_` separators are ignored. `None` for anything
/// else, including an empty string and values above `u32::MAX`.
pub fn parse_addr(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let digits: String = s.chars().filter(|&c| c != '_').collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

/// The debug panel's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DebugTab {
    #[default]
    Registers,
    Disassembly,
    Memory,
    Uart,
    Unmapped,
}

impl DebugTab {
    /// Every tab, in selector order.
    pub const ALL: [DebugTab; 5] = [
        DebugTab::Registers,
        DebugTab::Disassembly,
        DebugTab::Memory,
        DebugTab::Uart,
        DebugTab::Unmapped,
    ];

    /// The selector label.
    pub fn label(self) -> &'static str {
        match self {
            DebugTab::Registers => "Registers",
            DebugTab::Disassembly => "Disassembly",
            DebugTab::Memory => "Memory",
            DebugTab::Uart => "UART",
            DebugTab::Unmapped => "Unmapped",
        }
    }
}

/// What the run controls asked for this frame; the app applies it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DebugOutput {
    /// `Pause` or `Continue` was clicked.
    pub toggle_pause: bool,
    /// `Step` was clicked (only possible while paused).
    pub step: bool,
    /// A debugger goal to start (only while paused): the app calls `begin_goal`.
    pub goal: Option<Goal>,
    /// Errors to show as Error toasts.
    pub notes: Vec<String>,
}

/// The debug panels' state, kept while the panel is closed.
#[derive(Clone, Debug, Default)]
pub struct DebugPanel {
    tab: DebugTab,
    disasm: disasm::DisasmState,
    memory: memory::MemoryState,
    uart: UartConsole,
    uart2: UartConsole,
    /// The newest log lines, at most [`LOG_CAP`].
    log: Vec<String>,
}

/// A button that senses clicks but takes no keyboard focus, so Space and
/// Enter (calculator keys) never activate it.
fn button(ui: &mut egui::Ui, enabled: bool, b: Button<'_>) -> bool {
    ui.add_enabled(enabled, b.sense(Sense::CLICK)).clicked()
}

impl DebugPanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tab(&self) -> DebugTab {
        self.tab
    }

    /// Appends bytes `port` sent to its console.
    pub fn push_uart(&mut self, port: UartId, bytes: &[u8]) {
        match port {
            UartId::Uart0 => self.uart.push(bytes),
            UartId::Uart2 => self.uart2.push(bytes),
        }
    }

    /// The console of `port`.
    pub fn console(&self, port: UartId) -> &UartConsole {
        match port {
            UartId::Uart0 => &self.uart,
            UartId::Uart2 => &self.uart2,
        }
    }

    /// Appends log lines, keeping the newest [`LOG_CAP`].
    pub fn push_log(&mut self, lines: Vec<String>) {
        self.log.extend(lines);
        if self.log.len() > LOG_CAP {
            let excess = self.log.len() - LOG_CAP;
            self.log.drain(..excess);
        }
    }

    /// The kept log lines, oldest first.
    pub fn log_lines(&self) -> &[String] {
        &self.log
    }

    /// Draws the panel contents into `ui`.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        backend: &mut dyn EmuBackend,
        paused: bool,
    ) -> DebugOutput {
        let mut out = DebugOutput::default();
        ui.horizontal(|ui| {
            let run = if paused { "Continue" } else { "Pause" };
            out.toggle_pause = button(ui, true, Button::new(run));
            out.step = button(ui, paused, Button::new("Step"));
        });
        ui.horizontal_wrapped(|ui| {
            for tab in DebugTab::ALL {
                if button(ui, true, Button::selectable(self.tab == tab, tab.label())) {
                    self.tab = tab;
                }
            }
        });
        ui.separator();
        match self.tab {
            DebugTab::Registers => {
                // Scrolls instead of widening the panel past its cap.
                let cpu = backend.cpu();
                ScrollArea::both()
                    .id_salt("evo-regs-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| regs::show(ui, &cpu));
            }
            DebugTab::Disassembly => disasm::show(ui, &mut self.disasm, backend),
            DebugTab::Memory => memory::show(ui, &mut self.memory, backend),
            DebugTab::Uart => uart::show(ui, &mut self.uart),
            DebugTab::Unmapped => unmapped::show(ui, &backend.unmapped()),
        }
        out
    }
}
