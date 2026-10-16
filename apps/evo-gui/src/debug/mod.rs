//! The debug dock: the run row (continue/pause, step, step over, step out), then one tab
//! of [`DebugTab::ALL`]. Only the selected tab is drawn, so hidden tabs make no backend
//! calls.

mod breakpoints;
mod busmon;
mod console;
mod control;
mod disasm;
mod memory;
mod peripherals;
mod regs;
mod search;
mod stacks;
mod uart;
mod unmapped;
mod visualizer;
mod watchpoints;

use egui::{Button, ScrollArea, Sense};

pub use busmon::{BUS_SHOWN, bus_line};
pub use control::cycle_text;
pub use disasm::{DISASM_LINES, format_disasm};
pub use memory::{DEFAULT_MEM_ADDR, MAX_VIEWS};
pub use search::{SEARCH_CHUNK, SEARCH_CHUNKS_PER_FRAME, Search, find, parse_pattern};
pub use stacks::{STACK_WORDS, stack_rows};
pub use uart::{UART_CAP, UartConsole};
pub use visualizer::{DEFAULT_VIS_ADDR, PixelFormat, VIS_MAX, parse_size, render};
pub use watchpoints::watch_text;

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

/// Parses a register value: hex by the [`parse_addr`] rules, or decimal after a
/// leading `#` (`#10` is 10; `_` separators are ignored, nothing may sit between `#`
/// and the digits). `None` for anything else, including values above `u32::MAX`.
pub fn parse_value(s: &str) -> Option<u32> {
    let s = s.trim();
    let Some(dec) = s.strip_prefix('#') else {
        return parse_addr(s);
    };
    let digits: String = dec.chars().filter(|&c| c != '_').collect();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The debug dock's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DebugTab {
    Control,
    #[default]
    Registers,
    Disassembly,
    Memory,
    Visualizer,
    Breakpoints,
    Watchpoints,
    Peripherals,
    BusMonitor,
    Stacks,
    Console,
    Unmapped,
}

impl DebugTab {
    /// Every tab, in selector order.
    pub const ALL: [DebugTab; 12] = [
        DebugTab::Control,
        DebugTab::Registers,
        DebugTab::Disassembly,
        DebugTab::Memory,
        DebugTab::Visualizer,
        DebugTab::Breakpoints,
        DebugTab::Watchpoints,
        DebugTab::Peripherals,
        DebugTab::BusMonitor,
        DebugTab::Stacks,
        DebugTab::Console,
        DebugTab::Unmapped,
    ];

    /// The selector label.
    pub fn label(self) -> &'static str {
        match self {
            DebugTab::Control => "Control",
            DebugTab::Registers => "Registers",
            DebugTab::Disassembly => "Disassembly",
            DebugTab::Memory => "Memory",
            DebugTab::Visualizer => "Visualizer",
            DebugTab::Breakpoints => "Breakpoints",
            DebugTab::Watchpoints => "Watchpoints",
            DebugTab::Peripherals => "Peripherals",
            DebugTab::BusMonitor => "Bus monitor",
            DebugTab::Stacks => "Stacks",
            DebugTab::Console => "Console",
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
    /// "Step over", "Step out", the Control tab's "Run" and the Disassembly tab's
    /// "Run to here" set it.
    pub goal: Option<Goal>,
    /// Errors to show as Error toasts.
    pub notes: Vec<String>,
}

/// The debug panels' state, kept while the panel is closed.
#[derive(Clone, Debug, Default)]
pub struct DebugPanel {
    tab: DebugTab,
    control: control::ControlState,
    regs: regs::RegsState,
    disasm: disasm::DisasmState,
    memory: memory::MemoryState,
    visualizer: visualizer::VisualizerState,
    breakpoints: breakpoints::BreakpointsState,
    watchpoints: watchpoints::WatchpointsState,
    busmon: busmon::BusState,
    console: console::ConsoleState,
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
        // Wraps in a narrow dock instead of pushing the panel past its width cap.
        ui.horizontal_wrapped(|ui| {
            let run = if paused { "Continue" } else { "Pause" };
            out.toggle_pause = button(ui, true, Button::new(run));
            out.step = button(ui, paused, Button::new("Step"));
            if button(ui, paused, Button::new("Step over")) {
                out.goal = Some(Goal::StepOver);
            }
            if button(ui, paused, Button::new("Step out")) {
                out.goal = Some(Goal::StepOut);
            }
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
            DebugTab::Control => {
                if let Some(goal) = control::show(ui, &mut self.control, backend, paused) {
                    out.goal = Some(goal);
                }
            }
            DebugTab::Registers => {
                // Scrolls instead of widening the panel past its cap.
                ScrollArea::both()
                    .id_salt("evo-regs-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        regs::show(ui, &mut self.regs, backend, paused, &mut out.notes);
                    });
            }
            DebugTab::Disassembly => {
                if let Some(goal) = disasm::show(ui, &mut self.disasm, backend, paused) {
                    out.goal = Some(goal);
                }
            }
            DebugTab::Memory => {
                memory::show(ui, &mut self.memory, backend, paused, &mut out.notes);
            }
            DebugTab::Console => {
                let src = console::Sources {
                    uart0: &mut self.uart,
                    uart2: &mut self.uart2,
                    log: &mut self.log,
                };
                console::show(ui, &mut self.console, src, backend);
            }
            DebugTab::Unmapped => unmapped::show(ui, &backend.unmapped()),
            DebugTab::Visualizer => visualizer::show(ui, &mut self.visualizer, backend),
            DebugTab::Breakpoints => {
                breakpoints::show(ui, &mut self.breakpoints, backend, paused);
            }
            DebugTab::Watchpoints => {
                watchpoints::show(ui, &mut self.watchpoints, backend, paused, &mut out.notes);
            }
            DebugTab::Peripherals => peripherals::show(ui, backend),
            DebugTab::BusMonitor => busmon::show(ui, &mut self.busmon, backend),
            DebugTab::Stacks => stacks::show(ui, backend),
        }
        out
    }
}
