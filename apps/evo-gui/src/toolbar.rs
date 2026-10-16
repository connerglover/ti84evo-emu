//! The sidebar: its groups and actions, the group buttons, the group popovers, the slot
//! picker and the About window.

use egui::text::LayoutJob;
use egui::{
    Align, Align2, Button, FontId, Frame, Id, Layout, Margin, Order, Pos2, Rect, RichText, Sense,
    TextFormat, TextWrapMode, Vec2, WidgetInfo, WidgetType, vec2,
};

use crate::app::TITLE;
use crate::pacer::Speed;
use crate::store::SLOTS;
use crate::theme::{LABEL_DARK, TOOLBAR_BG, has_glyphs};

/// Width of the sidebar, in points.
pub const TOOLBAR_W: f32 = 48.0;
/// Size of a group button, in points.
const BUTTON_SIZE: Vec2 = vec2(40.0, 36.0);
/// Size of an icon glyph.
const ICON_SIZE: f32 = 18.0;
/// Size of a fallback word (it must fit a 40-point button).
const FALLBACK_SIZE: f32 = 10.0;
/// Gap between the sidebar and a popover or the slot picker, in points.
const PICKER_GAP: f32 = 4.0;
/// Narrowest a popover entry is, in points.
const POPOVER_MIN_W: f32 = 180.0;

/// A sidebar button: a group of actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Calculator,
    Capture,
    Files,
    Debug,
    Tools,
    Settings,
    Help,
}

impl Group {
    /// Every group, top to bottom.
    pub const ALL: [Group; 7] = [
        Group::Calculator,
        Group::Capture,
        Group::Files,
        Group::Debug,
        Group::Tools,
        Group::Settings,
        Group::Help,
    ];

    /// The button's glyph.
    pub fn icon(self) -> &'static str {
        match self {
            Group::Calculator => "\u{1F5A9}",
            Group::Capture => "\u{1F4F7}",
            Group::Files => "\u{1F4C1}",
            Group::Debug => "\u{1F41B}",
            Group::Tools => "\u{1F527}",
            Group::Settings => "\u{2699}",
            Group::Help => "\u{2753}",
        }
    }

    /// The button's text when the fonts cannot draw [`Group::icon`].
    pub fn fallback(self) -> &'static str {
        match self {
            Group::Calculator => "Calc",
            Group::Capture => "Shot",
            Group::Files => "Files",
            Group::Debug => "Debug",
            Group::Tools => "Tools",
            Group::Settings => "Prefs",
            Group::Help => "Help",
        }
    }

    /// The tooltip, which is also the button's accessibility label.
    pub fn tooltip(self) -> &'static str {
        match self {
            Group::Calculator => "Calculator",
            Group::Capture => "Capture",
            Group::Files => "Files",
            Group::Debug => "Debug panels",
            Group::Tools => "Tools",
            Group::Settings => "Settings",
            Group::Help => "Help",
        }
    }
}

/// Something the user can do from the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    UsbTools,
    Reset,
    Reprovision,
    ReloadRom,
    ChangeRom,
    SaveState,
    LoadState,
    TogglePause,
    Speed(Speed),
    Screenshot,
    CopyScreen,
    RecordApng,
    RecordVideo,
    ImportRam,
    ExportRam,
    ImportState,
    ExportState,
    ImportDebug,
    ExportDebug,
    ImportSettings,
    ExportSettings,
    ImportFlash,
    ExportFlash,
    ToggleDebug,
    KeyHistory,
    Macros,
    Hardware,
    Keymap,
    Smooth,
    Instances,
    Compact,
    ResetLayout,
    ResetEverything,
    About,
    Licenses,
    CheckUpdates,
    ReportBug,
}

impl Action {
    /// Every action, in popover order.
    pub const ALL: [Action; 40] = [
        Action::UsbTools,
        Action::Reset,
        Action::Reprovision,
        Action::ReloadRom,
        Action::ChangeRom,
        Action::SaveState,
        Action::LoadState,
        Action::TogglePause,
        Action::Speed(Speed::X1),
        Action::Speed(Speed::X2),
        Action::Speed(Speed::X4),
        Action::Speed(Speed::Unlimited),
        Action::Screenshot,
        Action::CopyScreen,
        Action::RecordApng,
        Action::RecordVideo,
        Action::ImportRam,
        Action::ExportRam,
        Action::ImportState,
        Action::ExportState,
        Action::ImportDebug,
        Action::ExportDebug,
        Action::ImportSettings,
        Action::ExportSettings,
        Action::ImportFlash,
        Action::ExportFlash,
        Action::ToggleDebug,
        Action::KeyHistory,
        Action::Macros,
        Action::Hardware,
        Action::Keymap,
        Action::Smooth,
        Action::Instances,
        Action::Compact,
        Action::ResetLayout,
        Action::ResetEverything,
        Action::About,
        Action::Licenses,
        Action::CheckUpdates,
        Action::ReportBug,
    ];

    /// The group whose popover lists this action.
    pub fn group(self) -> Group {
        use Action::*;
        match self {
            UsbTools => Group::Files,
            Reset | Reprovision | ReloadRom | ChangeRom | SaveState | LoadState | TogglePause
            | Speed(_) => Group::Calculator,
            Screenshot | CopyScreen | RecordApng | RecordVideo => Group::Capture,
            ImportRam | ExportRam | ImportState | ExportState | ImportDebug | ExportDebug
            | ImportSettings | ExportSettings | ImportFlash | ExportFlash => Group::Files,
            ToggleDebug => Group::Debug,
            KeyHistory | Macros | Hardware => Group::Tools,
            Keymap | Smooth | Instances | Compact | ResetLayout | ResetEverything => {
                Group::Settings
            }
            About | Licenses | CheckUpdates | ReportBug => Group::Help,
        }
    }

    /// The popover entry's text, which is also its accessibility label.
    pub fn label(self) -> &'static str {
        use Action::*;
        match self {
            UsbTools => "USB tools…",
            Reset => "Reset",
            Reprovision => "Re-provision",
            ReloadRom => "Reload ROM",
            ChangeRom => "Change ROM\u{2026}",
            SaveState => "Save state",
            LoadState => "Load state",
            TogglePause => "Pause",
            Speed(s) => s.label(),
            Screenshot => "Screenshot",
            CopyScreen => "Copy screen",
            RecordApng => "Record animated PNG",
            RecordVideo => "Record MP4",
            ImportRam => "Import RAM image\u{2026}",
            ExportRam => "Export RAM image\u{2026}",
            ImportState => "Import calculator state\u{2026}",
            ExportState => "Export calculator state\u{2026}",
            ImportDebug => "Import debug state\u{2026}",
            ExportDebug => "Export debug state\u{2026}",
            ImportSettings => "Import settings\u{2026}",
            ExportSettings => "Export settings\u{2026}",
            ImportFlash => "Import flash image\u{2026}",
            ExportFlash => "Export flash image\u{2026}",
            ToggleDebug => "Debug panels",
            KeyHistory => "Key history",
            Macros => "Macros / AutoTester",
            Hardware => "Hardware",
            Keymap => "Keyboard mapping",
            Smooth => "Smooth scaling (better at non-integer sizes)",
            Instances => "Instances\u{2026}",
            Compact => "Compact mode",
            ResetLayout => "Reset layout",
            ResetEverything => "Reset everything\u{2026}",
            About => "About",
            Licenses => "Third-party licenses",
            CheckUpdates => "Check for updates",
            ReportBug => "Report a bug",
        }
    }
}

/// The toast for an action nothing handles yet.
pub fn not_available(action: Action) -> String {
    format!("{} is not available yet", action.label())
}

/// For each group in [`Group::ALL`] order, whether its icon is drawable.
pub fn drawable_icons(ctx: &egui::Context) -> [bool; 7] {
    let font = FontId::proportional(ICON_SIZE);
    Group::ALL.map(|g| has_glyphs(ctx, &font, g.icon()))
}

/// What the sidebar did this frame.
pub struct SidebarOutput {
    /// The group clicked, if any.
    pub clicked: Option<Group>,
    /// The screen rect of the clicked button.
    pub clicked_rect: Option<Rect>,
}

/// Draws the group buttons in `ui` (the sidebar panel). `open` is the group whose popover
/// is open; its button is drawn selected, as is every group `active` returns true for.
///
/// The buttons sense clicks but are not focusable, so Tab never focuses them and
/// Space/Enter (calculator keys) never click them.
pub fn sidebar_buttons(
    ui: &mut egui::Ui,
    icons: &[bool; 7],
    open: Option<Group>,
    active: impl Fn(Group) -> bool,
) -> SidebarOutput {
    let mut out = SidebarOutput {
        clicked: None,
        clicked_rect: None,
    };
    ui.vertical_centered(|ui| {
        ui.add_space(4.0);
        for (group, &icon) in Group::ALL.iter().zip(icons) {
            let text = if icon {
                RichText::new(group.icon()).size(ICON_SIZE)
            } else {
                RichText::new(group.fallback()).size(FALLBACK_SIZE)
            };
            let resp = ui.add(
                Button::new(text.color(LABEL_DARK))
                    .min_size(BUTTON_SIZE)
                    .selected(open == Some(*group) || active(*group))
                    .sense(Sense::CLICK),
            );
            let label = group.tooltip();
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
            let resp = resp.on_hover_text(label);
            if resp.clicked() {
                out.clicked = Some(*group);
                out.clicked_rect = Some(resp.rect);
            }
        }
    });
    out
}

/// What a group popover did this frame.
pub struct PopoverOutput {
    /// The action clicked, if any.
    pub picked: Option<Action>,
    /// True when the user clicked outside the popover.
    pub clicked_elsewhere: bool,
}

/// The popover of `group`: an area at `pos` with one focus-free button per action of the
/// group, in [`Action::ALL`] order, each selected when `active` returns true for it.
pub fn group_popover(
    ctx: &egui::Context,
    pos: Pos2,
    group: Group,
    active: impl Fn(Action) -> bool,
) -> PopoverOutput {
    let mut picked = None;
    let area = egui::Area::new(Id::new("evo-group-popover"))
        .order(Order::Foreground)
        .fixed_pos(pos + vec2(PICKER_GAP, 0.0))
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(POPOVER_MIN_W);
                ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
                    for a in Action::ALL.into_iter().filter(|a| a.group() == group) {
                        let resp = ui.add(
                            Button::new(a.label())
                                .wrap_mode(TextWrapMode::Extend)
                                .selected(active(a))
                                .sense(Sense::CLICK),
                        );
                        if resp.clicked() {
                            picked = Some(a);
                        }
                    }
                });
            });
        });
    PopoverOutput {
        picked,
        clicked_elsewhere: area.response.clicked_elsewhere(),
    }
}

/// What the slot picker is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Save,
    Load,
}

/// What the slot picker did this frame.
pub struct PickerOutput {
    /// The slot clicked, if any.
    pub picked: Option<u8>,
    /// True when the user clicked outside the picker.
    pub clicked_elsewhere: bool,
}

/// The slot picker: an area at `pos` with one row per slot, a `Slot {n}`
/// button then a `saved` or `empty` label.
pub fn slot_picker(
    ctx: &egui::Context,
    pos: Pos2,
    mode: PickerMode,
    has_slot: impl Fn(u8) -> bool,
) -> PickerOutput {
    let mut picked = None;
    let area = egui::Area::new(Id::new("evo-slot-picker"))
        .order(Order::Foreground)
        .fixed_pos(pos + vec2(PICKER_GAP, 0.0))
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(RichText::new(match mode {
                    PickerMode::Save => "Save to slot",
                    PickerMode::Load => "Load from slot",
                }));
                egui::Grid::new("evo-slot-rows")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for n in 1..=SLOTS {
                            let resp = ui.add(Button::new(format!("Slot {n}")).sense(Sense::CLICK));
                            if resp.clicked() {
                                picked = Some(n);
                            }
                            ui.label(if has_slot(n) { "saved" } else { "empty" });
                            ui.end_row();
                        }
                    });
            });
        });
    PickerOutput {
        picked,
        clicked_elsewhere: area.response.clicked_elsewhere(),
    }
}

/// The About window, centered over the app.
pub fn about_window(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("About")
        .open(open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .default_width(300.0)
        .show(ctx, |ui| {
            let mut job = LayoutJob::default();
            let body = |size: f32, color| TextFormat {
                font_id: FontId::proportional(size),
                color,
                ..Default::default()
            };
            job.append(TITLE, 0.0, body(16.0, ui.visuals().strong_text_color()));
            job.append(
                env!("CARGO_PKG_VERSION"),
                6.0,
                body(14.0, ui.visuals().text_color()),
            );
            ui.label(job);
            ui.add_space(6.0);
            ui.label(
                "An independent, open-source emulator. Not affiliated with Texas \
                 Instruments. TI-84 is a trademark of Texas Instruments.",
            );
            ui.add_space(6.0);
            ui.label("You must supply your own OS bundle.");
        });
}

/// The sidebar panel's frame.
pub fn toolbar_frame() -> Frame {
    Frame::NONE.fill(TOOLBAR_BG).inner_margin(Margin::ZERO)
}
