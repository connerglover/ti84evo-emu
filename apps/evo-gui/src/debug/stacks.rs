//! The Stacks tab: the words at the current `sp` and at each banked `msp_*` / `psp_*`
//! stack pointer, [`STACK_WORDS`] each, upward from the pointer. Read-only.

use egui::{CollapsingHeader, RichText, ScrollArea};

use crate::backend::EmuBackend;

/// How many words each stack section shows.
pub const STACK_WORDS: usize = 16;

/// `words` rows from `sp` upward: `"{addr:08x}  {word:08x}"` (little-endian), or
/// `"{addr:08x}  ????????"` when any byte of the word is unreadable. Rows stop at the top of
/// the address space.
pub fn stack_rows(backend: &dyn EmuBackend, sp: u32, words: usize) -> Vec<String> {
    let mut rows = Vec::with_capacity(words.min(STACK_WORDS * 4));
    for i in 0..words {
        let Some(addr) = u32::try_from(i)
            .ok()
            .and_then(|i| i.checked_mul(4))
            .and_then(|off| sp.checked_add(off))
        else {
            break;
        };
        let bytes: Option<Vec<u8>> = backend.peek(addr, 4).into_iter().collect();
        rows.push(match bytes.as_deref() {
            Some(&[a, b, c, d]) => {
                format!("{addr:08x}  {:08x}", u32::from_le_bytes([a, b, c, d]))
            }
            _ => format!("{addr:08x}  ????????"),
        });
    }
    rows
}

/// The Stacks tab body. Reads the registers and the stacks once per frame, only while it
/// is drawn.
pub(super) fn show(ui: &mut egui::Ui, backend: &dyn EmuBackend) {
    let cpu = backend.cpu();
    let mut sections = vec![("sp", cpu.core[13])];
    sections.extend(
        cpu.banked
            .iter()
            .filter(|(name, _)| name.starts_with("msp_") || name.starts_with("psp_"))
            .copied(),
    );
    ScrollArea::vertical()
        .id_salt("evo-stacks-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (name, sp) in sections {
                CollapsingHeader::new(format!("{name} 0x{sp:08x}"))
                    .id_salt(("evo-stack", name))
                    .default_open(true)
                    .show(ui, |ui| {
                        for row in stack_rows(backend, sp, STACK_WORDS) {
                            ui.label(RichText::new(row).monospace());
                        }
                    });
            }
        });
}
