//! The Console tab: UART0, UART2 or the log, with an input line for the UARTs.

use egui::{Button, RichText, ScrollArea, TextEdit};

use super::button;
use super::uart::UartConsole;
use crate::backend::{EmuBackend, UartId};

/// What the Console tab shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ConsoleSource {
    #[default]
    Uart0,
    Uart2,
    Log,
}

impl ConsoleSource {
    /// Every source, in selector order.
    pub const ALL: [ConsoleSource; 3] = [
        ConsoleSource::Uart0,
        ConsoleSource::Uart2,
        ConsoleSource::Log,
    ];

    /// The selector label.
    pub fn label(self) -> &'static str {
        match self {
            ConsoleSource::Uart0 => "UART0",
            ConsoleSource::Uart2 => "UART2",
            ConsoleSource::Log => "Log",
        }
    }

    /// The UART this source reads and writes; `None` for the log.
    pub fn port(self) -> Option<UartId> {
        match self {
            ConsoleSource::Uart0 => Some(UartId::Uart0),
            ConsoleSource::Uart2 => Some(UartId::Uart2),
            ConsoleSource::Log => None,
        }
    }
}

/// The Console tab's state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug, Default)]
pub(super) struct ConsoleState {
    source: ConsoleSource,
    /// The input line's text (shared by both UARTs).
    input: String,
}

/// The texts the Console tab shows and clears.
pub(super) struct Sources<'a> {
    pub uart0: &'a mut UartConsole,
    pub uart2: &'a mut UartConsole,
    pub log: &'a mut Vec<String>,
}

/// The Console tab body. Sending works paused or running: it is input to the guest, like a
/// key press, and changes no machine state the debugger shows.
pub(super) fn show(
    ui: &mut egui::Ui,
    st: &mut ConsoleState,
    src: Sources<'_>,
    backend: &mut dyn EmuBackend,
) {
    ui.horizontal_wrapped(|ui| {
        for s in ConsoleSource::ALL {
            if button(ui, true, Button::selectable(st.source == s, s.label())) {
                st.source = s;
            }
        }
        ui.separator();
        if button(ui, true, Button::new("Clear")) {
            match st.source {
                ConsoleSource::Uart0 => src.uart0.clear(),
                ConsoleSource::Uart2 => src.uart2.clear(),
                ConsoleSource::Log => src.log.clear(),
            }
        }
    });
    if let Some(port) = st.source.port() {
        ui.horizontal(|ui| {
            let field = ui.add(
                TextEdit::singleline(&mut st.input)
                    .hint_text("Send a line")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(240.0),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let send = button(ui, true, Button::new("Send"));
            if send || enter {
                let mut bytes = std::mem::take(&mut st.input).into_bytes();
                bytes.push(b'\r');
                backend.send_uart(port, &bytes);
                if enter {
                    // Keep typing lines without clicking back into the field.
                    field.request_focus();
                }
            }
        });
    }
    ui.separator();
    let joined;
    let text = match st.source {
        ConsoleSource::Uart0 => src.uart0.text(),
        ConsoleSource::Uart2 => src.uart2.text(),
        ConsoleSource::Log => {
            joined = src.log.join("\n");
            joined.as_str()
        }
    };
    ScrollArea::vertical()
        .id_salt("evo-console")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.label(RichText::new(text).monospace());
        });
}
