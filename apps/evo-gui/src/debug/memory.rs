//! The Memory tab: a search row and 1..=[`MAX_VIEWS`] hex views of 16 rows each.
//!
//! Each view has its own address and format ("Bytes", or "Words": little-endian `u32`).
//! While paused, clicking a byte (Bytes) or a word (Words) opens a 2- or 8-digit field in
//! its place: Enter writes a [`parse_value`] value with `poke`, Escape (or clicking
//! elsewhere) cancels, and an unparsable or too-large value shows "Not a value" and keeps
//! the field. The search row's "Next" runs a [`Search`] for at most
//! [`SEARCH_CHUNKS_PER_FRAME`] chunks per frame; a hit moves the first view to its row and
//! highlights it, and the next "Next" searches on from the byte after the hit.

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{
    Button, CursorIcon, EventFilter, FontId, Id, Key, Label, Margin, RichText, ScrollArea, Sense,
    TextEdit, TextFormat,
};

use super::search::{SEARCH_CHUNKS_PER_FRAME, Search, parse_pattern};
use super::{button, parse_addr, parse_value};
use crate::backend::EmuBackend;

/// The most views the tab shows at once.
pub const MAX_VIEWS: usize = 4;
/// Bytes per row and rows per view.
const ROW: usize = 16;
/// A view's first address until the user types another.
pub const DEFAULT_MEM_ADDR: u32 = 0x2000_0000;
/// Characters before a row's first value: the address and two spaces.
const PREFIX: usize = 10;

/// How a view shows its bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Format {
    #[default]
    Bytes,
    /// Little-endian `u32`s.
    Words,
}

impl Format {
    /// Bytes per value.
    fn width(self) -> usize {
        match self {
            Format::Bytes => 1,
            Format::Words => 4,
        }
    }

    /// Characters per value plus its separating space.
    fn stride(self) -> usize {
        2 * self.width() + 1
    }

    /// Length of a row's value columns.
    fn hex_len(self) -> usize {
        (ROW / self.width()) * self.stride() - 1
    }

    /// Column of a row's first ASCII character (after `"  |"`).
    fn ascii_start(self) -> usize {
        PREFIX + self.hex_len() + 3
    }

    /// The columns of the value holding byte `j` (0..16) of a row.
    fn cell_span(self, j: usize) -> std::ops::Range<usize> {
        let start = PREFIX + (j / self.width()) * self.stride();
        start..start + 2 * self.width()
    }

    /// The first byte (0..16) of the value at column `col`, if `col` is on a value or its
    /// ASCII character.
    fn cell_at(self, col: usize) -> Option<usize> {
        let w = self.width();
        if (PREFIX..PREFIX + self.hex_len()).contains(&col) {
            return Some((col - PREFIX) / self.stride() * w);
        }
        let ascii = self.ascii_start();
        (ascii..ascii + ROW)
            .contains(&col)
            .then(|| (col - ascii) / w * w)
    }
}

/// The ASCII column: printable bytes as themselves, the rest (and unreadable bytes) `.`.
fn ascii(bytes: &[Option<u8>]) -> String {
    bytes
        .iter()
        .map(|b| match b {
            Some(c @ 0x20..=0x7E) => char::from(*c),
            _ => '.',
        })
        .collect()
}

/// One row: `{addr:08x}  values  |ascii|`. Bytes are `bb`; words are little-endian
/// `wwwwwwww`. Unreadable values show as `?`.
fn format_row(format: Format, addr: u32, bytes: &[Option<u8>]) -> String {
    let values: Vec<String> = match format {
        Format::Bytes => bytes
            .iter()
            .map(|b| b.map_or_else(|| "??".to_owned(), |b| format!("{b:02x}")))
            .collect(),
        Format::Words => bytes
            .chunks(4)
            .map(|w| match w {
                [Some(a), Some(b), Some(c), Some(d)] => {
                    format!("{:08x}", u32::from_le_bytes([*a, *b, *c, *d]))
                }
                _ => "????????".to_owned(),
            })
            .collect(),
    };
    format!("{addr:08x}  {}  |{}|", values.join(" "), ascii(bytes))
}

/// The value text an edit field starts with; empty when any byte is unreadable.
fn edit_text(bytes: &[Option<u8>]) -> String {
    match bytes {
        [Some(v)] => format!("{v:02x}"),
        [Some(a), Some(b), Some(c), Some(d)] => {
            format!("{:08x}", u32::from_le_bytes([*a, *b, *c, *d]))
        }
        _ => String::new(),
    }
}

fn hex_addr(a: u32) -> String {
    format!("0x{a:08x}")
}

/// A hex view.
#[derive(Clone, Debug)]
struct View {
    /// Stays the same when other views close (widget ids, the edit field's view).
    id: u64,
    field: String,
    /// The last address `field` parsed to.
    addr: u32,
    format: Format,
}

impl View {
    fn new(id: u64, addr: u32) -> Self {
        Self {
            id,
            field: hex_addr(addr),
            addr,
            format: Format::Bytes,
        }
    }
}

/// An open value edit field.
#[derive(Clone, Debug)]
struct Edit {
    view: u64,
    addr: u32,
    /// Bytes written: 1 or 4.
    width: usize,
    text: String,
    /// The field takes the keyboard focus this frame (just opened, or Enter on an
    /// unparsable value).
    focus: bool,
    /// The text was submitted and did not parse; cleared by the next edit.
    bad: bool,
}

/// What the search row last reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchNote {
    NotFound,
    BadPattern,
}

/// The Memory tab's own state, kept while the tab or the dock is hidden.
#[derive(Clone, Debug)]
pub(super) struct MemoryState {
    views: Vec<View>,
    next_id: u64,
    edit: Option<Edit>,
    find: String,
    /// The running search and its pattern's length.
    search: Option<(Search, usize)>,
    /// Where the next "Next" starts: the byte after the last hit.
    next_from: Option<u32>,
    note: Option<SearchNote>,
    /// The last hit: its address and length.
    highlight: Option<(u32, usize)>,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            views: vec![View::new(0, DEFAULT_MEM_ADDR)],
            next_id: 1,
            edit: None,
            find: String::new(),
            search: None,
            next_from: None,
            note: None,
            highlight: None,
        }
    }
}

/// What a view asked for this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewAction {
    Close(usize),
    Open { view: u64, addr: u32, width: usize },
    Apply { addr: u32, width: usize, value: u32 },
    CloseEdit,
}

/// Lays out a row's text with the characters of bytes `marked` (0..16) highlighted.
fn row_job(
    ui: &egui::Ui,
    text: &str,
    format: Format,
    marked: &[usize],
    font: &FontId,
) -> LayoutJob {
    let mut lit = vec![false; text.len()];
    for &j in marked {
        let ascii = std::iter::once(format.ascii_start() + j);
        for c in format.cell_span(j).chain(ascii) {
            if let Some(l) = lit.get_mut(c) {
                *l = true;
            }
        }
    }
    let visuals = ui.visuals();
    let plain = TextFormat::simple(font.clone(), visuals.text_color());
    let hit = TextFormat {
        background: visuals.selection.bg_fill,
        color: visuals.selection.stroke.color,
        ..plain.clone()
    };
    let mut job = LayoutJob::default();
    let mut start = 0;
    // Row text is ASCII, so byte and character indices agree.
    while start < text.len() {
        let on = lit[start];
        let end = (start..text.len())
            .find(|&i| lit[i] != on)
            .unwrap_or(text.len());
        let fmt = if on { hit.clone() } else { plain.clone() };
        job.append(&text[start..end], 0.0, fmt);
        start = end;
    }
    job
}

/// Draws the open edit field; returns what it asked for.
fn edit_field(ui: &mut egui::Ui, e: &mut Edit, char_w: f32) -> Option<ViewAction> {
    let id = Id::new(("evo-mem-edit", e.view));
    let mut out = TextEdit::singleline(&mut e.text)
        .id(id)
        .font(egui::TextStyle::Monospace)
        .desired_width(char_w * (2 * e.width) as f32)
        .margin(Margin::symmetric(1, 0))
        // The field keeps Escape: egui would otherwise drop the focus before the app
        // reads the keyboard, and Escape would reach the calculator as CLEAR.
        .event_filter(EventFilter {
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
            ..Default::default()
        })
        .show(ui);
    let r = out.response;
    if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
        r.surrender_focus();
        return Some(ViewAction::CloseEdit);
    }
    if e.focus {
        e.focus = false;
        r.request_focus();
        // Select the whole value, so typing replaces it.
        let end = e.text.chars().count();
        out.state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(end))));
        out.state.store(ui.ctx(), id);
    }
    if r.changed() {
        e.bad = false;
    }
    if !r.lost_focus() {
        return None;
    }
    if !ui.input(|i| i.key_pressed(Key::Enter)) {
        // A click elsewhere.
        return Some(ViewAction::CloseEdit);
    }
    let max = if e.width == 1 { 0xFF } else { u32::MAX };
    match parse_value(&e.text).filter(|&v| v <= max) {
        Some(value) => Some(ViewAction::Apply {
            addr: e.addr,
            width: e.width,
            value,
        }),
        None => {
            e.bad = true;
            e.focus = true;
            ui.ctx().request_repaint();
            None
        }
    }
}

/// What one view draws with.
struct ViewCtx<'a> {
    index: usize,
    views: usize,
    highlight: Option<(u32, usize)>,
    backend: &'a dyn EmuBackend,
    paused: bool,
}

/// Draws a view's header and rows. Sets `moved` when the user edited its address.
fn show_view(
    ui: &mut egui::Ui,
    cx: &ViewCtx<'_>,
    view: &mut View,
    edit: &mut Option<Edit>,
    moved: &mut bool,
) -> Option<ViewAction> {
    let mut action = None;
    let editing_here = edit.as_ref().is_some_and(|e| e.view == view.id);
    ui.horizontal_wrapped(|ui| {
        ui.label("Address");
        let r = ui.add(
            TextEdit::singleline(&mut view.field)
                .id(Id::new(("evo-mem-addr", view.id)))
                .desired_width(110.0)
                .font(egui::TextStyle::Monospace),
        );
        *moved = r.changed();
        for (format, label) in [(Format::Bytes, "Bytes"), (Format::Words, "Words")] {
            let selected = view.format == format;
            if button(ui, true, Button::selectable(selected, label)) && !selected {
                view.format = format;
                if editing_here {
                    action = Some(ViewAction::CloseEdit);
                }
            }
        }
        if cx.views > 1 && button(ui, true, Button::new("Close")) {
            action = Some(ViewAction::Close(cx.index));
        }
    });
    match parse_addr(&view.field) {
        Some(a) => {
            if a != view.addr && editing_here {
                action = action.or(Some(ViewAction::CloseEdit));
            }
            view.addr = a;
        }
        None => {
            ui.label(RichText::new("Not a hex address").color(ui.visuals().warn_fg_color));
        }
    }
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0')).max(1.0);
    let format = view.format;
    let bytes = cx.backend.peek(view.addr, ROW * ROW);
    let mut bad = false;
    for (r, row) in (0u32..).zip(bytes.chunks(ROW)) {
        let row_addr = view.addr.wrapping_add(r * ROW as u32);
        let text = format_row(format, row_addr, row);
        let in_row = |a: u32| (a.wrapping_sub(row_addr) as usize) < ROW;
        if let Some(e) = edit
            .as_mut()
            .filter(|e| e.view == view.id && in_row(e.addr))
        {
            let span = format.cell_span(e.addr.wrapping_sub(row_addr) as usize);
            bad |= e.bad;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add(Label::new(RichText::new(&text[..span.start]).monospace()).extend());
                if let Some(a) = edit_field(ui, e, char_w) {
                    action = Some(a);
                }
                ui.add(Label::new(RichText::new(&text[span.end..]).monospace()).extend());
            });
            continue;
        }
        let marked: Vec<usize> = cx
            .highlight
            .map(|(hit, len)| {
                (0..ROW)
                    .filter(|&j| {
                        let a = row_addr.wrapping_add(j as u32);
                        (a.wrapping_sub(hit) as usize) < len
                    })
                    .collect()
            })
            .unwrap_or_default();
        let label = Label::new(row_job(ui, &text, format, &marked, &font)).extend();
        if !cx.paused {
            ui.add(label);
            continue;
        }
        // CLICK without FOCUSABLE: Space and Enter (calculator keys) never activate it.
        let resp = ui
            .add(label.sense(Sense::CLICK))
            .on_hover_cursor(CursorIcon::Text);
        let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) else {
            continue;
        };
        let col = ((pos.x - resp.rect.left()) / char_w).floor();
        if col < 0.0 {
            continue;
        }
        if let Some(j) = format.cell_at(col as usize) {
            action = Some(ViewAction::Open {
                view: view.id,
                addr: row_addr.wrapping_add(j as u32),
                width: format.width(),
            });
        }
    }
    if bad {
        ui.colored_label(ui.visuals().error_fg_color, "Not a value");
    }
    action
}

/// Draws the search row and runs this frame's share of a search.
fn show_search(ui: &mut egui::Ui, st: &mut MemoryState, backend: &dyn EmuBackend) {
    let mut start = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("Find");
        let r = ui.add(
            TextEdit::singleline(&mut st.find)
                .id(Id::new("evo-mem-find"))
                .hint_text("de ad be ef or \"text\"")
                .desired_width(170.0)
                .font(egui::TextStyle::Monospace),
        );
        if r.changed() {
            // A new pattern: cancel, and search from the first view again.
            st.search = None;
            st.next_from = None;
            st.note = None;
            st.highlight = None;
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if enter {
            // Keep the field: Enter again finds the next match, and the keys stay away
            // from the calculator.
            r.request_focus();
        }
        start = button(ui, true, Button::new("Next")) || enter;
    });
    if start {
        st.note = None;
        match parse_pattern(&st.find) {
            None => {
                st.search = None;
                st.note = Some(SearchNote::BadPattern);
            }
            Some(pattern) => {
                let from = st.next_from.unwrap_or(st.views[0].addr);
                let len = pattern.len();
                st.search = Some((Search::new(backend.memory_regions(), from, pattern), len));
            }
        }
    }
    if let Some((search, len)) = st.search.as_mut() {
        let len = *len;
        match search.step(backend, SEARCH_CHUNKS_PER_FRAME) {
            None => ui.ctx().request_repaint(),
            Some(result) => {
                st.search = None;
                match result {
                    Some(hit) => {
                        let first = &mut st.views[0];
                        first.addr = hit & !0xF;
                        first.field = hex_addr(first.addr);
                        st.highlight = Some((hit, len));
                        st.next_from = Some(hit.wrapping_add(1));
                    }
                    None => st.note = Some(SearchNote::NotFound),
                }
            }
        }
    }
    let status = if st.search.is_some() {
        Some("Searching\u{2026}")
    } else {
        match st.note {
            Some(SearchNote::NotFound) => Some("Not found"),
            Some(SearchNote::BadPattern) => Some("Not a pattern"),
            None => None,
        }
    };
    if let Some(s) = status {
        ui.label(RichText::new(s).color(ui.visuals().warn_fg_color));
    }
}

/// The Memory tab body. Backend errors go to `notes`.
pub(super) fn show(
    ui: &mut egui::Ui,
    st: &mut MemoryState,
    backend: &mut dyn EmuBackend,
    paused: bool,
    notes: &mut Vec<String>,
) {
    if !paused {
        st.edit = None;
    }
    show_search(ui, st, backend);
    let can_add = st.views.len() < MAX_VIEWS;
    if button(ui, can_add, Button::new("Add view")) {
        let addr = st.views.last().map_or(DEFAULT_MEM_ADDR, |v| v.addr);
        st.views.push(View::new(st.next_id, addr));
        st.next_id += 1;
    }
    ui.separator();
    let mut action = None;
    let count = st.views.len();
    let highlight = st.highlight;
    ScrollArea::both()
        .id_salt("evo-memory")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (index, view) in st.views.iter_mut().enumerate() {
                if index > 0 {
                    ui.separator();
                }
                let cx = ViewCtx {
                    index,
                    views: count,
                    highlight,
                    backend: &*backend,
                    paused,
                };
                let mut moved = false;
                if let Some(a) = show_view(ui, &cx, view, &mut st.edit, &mut moved) {
                    action = Some(a);
                }
                if moved {
                    // Editing an address cancels a running search; moving the first view
                    // also makes the next search start from it.
                    st.search = None;
                    if index == 0 {
                        st.next_from = None;
                        st.highlight = None;
                    }
                }
            }
        });
    match action {
        None => {}
        Some(ViewAction::Close(i)) => {
            if st.views.len() > 1 && i < st.views.len() {
                let gone = st.views.remove(i);
                if st.edit.as_ref().is_some_and(|e| e.view == gone.id) {
                    st.edit = None;
                }
                if i == 0 {
                    // Another view is first now: the next search starts from it.
                    st.next_from = None;
                    st.highlight = None;
                }
            }
        }
        Some(ViewAction::Open { view, addr, width }) => {
            st.edit = Some(Edit {
                view,
                addr,
                width,
                text: edit_text(&backend.peek(addr, width)),
                focus: true,
                bad: false,
            });
            ui.ctx().request_repaint();
        }
        Some(ViewAction::CloseEdit) => st.edit = None,
        Some(ViewAction::Apply { addr, width, value }) => {
            st.edit = None;
            let bytes = value.to_le_bytes();
            if let Err(e) = backend.poke(addr, &bytes[..width]) {
                notes.push(format!("Cannot write 0x{addr:08x}: {e}"));
            }
            ui.ctx().request_repaint();
        }
    }
}
