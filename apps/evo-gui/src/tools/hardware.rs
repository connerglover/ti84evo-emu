//! The Hardware panel: board conditions the OS reads (backlight, charger line, battery,
//! USB-C, power hold, deep sleep, RTC) and a "Press ON" button.
//!
//! "Press ON" presses [`KeyId::On`] at the current cycle `p` and releases it at
//! `p + MIN_HOLD_CYCLES`. [`Hardware::next_deadline`] reports that release cycle, so the app's
//! run stops there and the release lands at its exact cycle at any speed. While paused,
//! cycles do not advance, so the key stays held until the run resumes. When the cycle counter
//! goes below the last cycle the panel saw (Reset, a state load, a new backend), the pending
//! release is dropped: the app has already released every key.

use std::time::{SystemTime, UNIX_EPOCH};

use egui::{Align2, Button, Window};

use crate::backend::{EmuBackend, HwChange};
use crate::feature::FeatureCtx;
use crate::input::MIN_HOLD_CYCLES;
use crate::keypad::KeyId;
use crate::toast::ToastKind;

/// The window title.
pub const HARDWARE_TITLE: &str = "Hardware";

const DAY_SECONDS: u64 = 86_400;
const WINDOW_W: f32 = 280.0;

/// `seconds` as `HH:MM:SS` (hours are not wrapped at 24).
pub fn hms(seconds: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Whole seconds since UTC midnight of `now`, below 86 400 (also before 1970).
pub fn utc_seconds_of_day(now: SystemTime) -> u32 {
    let secs = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() % DAY_SECONDS,
        Err(e) => {
            // `now` is `d` before the epoch: floor(-d) mod one day.
            let d = e.duration();
            let back = d.as_secs() + u64::from(d.subsec_nanos() > 0);
            (DAY_SECONDS - back % DAY_SECONDS) % DAY_SECONDS
        }
    };
    // Below 86 400, so it fits.
    secs as u32
}

/// The Hardware window and its pending "Press ON" release.
#[derive(Debug, Default)]
pub struct Hardware {
    open: bool,
    /// The cycle "Press ON" pressed the key at, while its release is pending.
    on_pressed_at: Option<u64>,
    /// The latest cycle count the panel saw while a release was pending.
    last_seen: u64,
}

impl Hardware {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window when closed, closes it when open.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// The cycle the pending ON release is due at, if any.
    pub fn next_deadline(&self) -> Option<u64> {
        self.on_pressed_at
            .map(|p| p.saturating_add(MIN_HOLD_CYCLES))
    }

    fn press_on(&mut self, backend: &mut dyn EmuBackend) {
        let now = backend.cycles();
        backend.set_key(KeyId::On, true);
        self.on_pressed_at = Some(now);
        self.last_seen = now;
    }

    /// Releases ON once its hold has elapsed; drops the release after a restart.
    pub fn frame(&mut self, cx: &mut FeatureCtx<'_>) {
        let Some(due) = self.next_deadline() else {
            return;
        };
        let now = cx.backend.cycles();
        if now < self.last_seen {
            // Reset, a state load or a new backend: the app released every key already.
            self.on_pressed_at = None;
            return;
        }
        self.last_seen = now;
        if now >= due {
            cx.backend.set_key(KeyId::On, false);
            self.on_pressed_at = None;
        }
    }

    /// The app is closing: releases ON if it is still held.
    pub fn on_exit(&mut self, cx: &mut FeatureCtx<'_>) {
        if self.on_pressed_at.take().is_some() && cx.backend.cycles() >= self.last_seen {
            cx.backend.set_key(KeyId::On, false);
        }
    }

    fn apply(change: HwChange, cx: &mut FeatureCtx<'_>) {
        if let Err(e) = cx.backend.set_hardware(change) {
            cx.toasts
                .push(format!("Hardware change failed: {e}"), ToastKind::Error);
        }
    }

    /// Draws the window while open.
    pub fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>) {
        if !self.open {
            return;
        }
        let mut open = true;
        Window::new(HARDWARE_TITLE)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .vscroll(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(WINDOW_W)
            .show(ctx, |ui| self.contents(ui, cx));
        if !open {
            self.open = false;
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, cx: &mut FeatureCtx<'_>) {
        let hw = cx.backend.hardware();
        let enabled = !cx.provisioning;
        ui.label(format!("Backlight {:.0}%", hw.backlight * 100.0));
        ui.separator();
        ui.add_enabled_ui(enabled, |ui| {
            let mut charger = hw.charger_line;
            if ui
                .checkbox(&mut charger, "Charger line high (GPIO 9)")
                .changed()
            {
                Self::apply(HwChange::ChargerLine(charger), cx);
            }
            let mut battery = hw.battery_ok;
            if ui.checkbox(&mut battery, "Battery OK (GPIO 11)").changed() {
                Self::apply(HwChange::BatteryOk(battery), cx);
            }
            let mut usb = hw.usb_attached;
            if ui.checkbox(&mut usb, "USB-C attached").changed() {
                Self::apply(HwChange::UsbAttached(usb), cx);
            }
        });
        ui.separator();
        let can_press = enabled && self.on_pressed_at.is_none();
        if ui.add_enabled(can_press, Button::new("Press ON")).clicked() {
            self.press_on(cx.backend);
        }
        ui.label(format!(
            "Power hold: {}",
            if hw.power_hold { "on" } else { "off" }
        ));
        ui.label(format!(
            "Deep sleep: {}",
            if hw.deep_sleep { "armed" } else { "not armed" }
        ));
        ui.separator();
        ui.label(format!("RTC {}", hms(hw.rtc_seconds)));
        if ui
            .add_enabled(enabled, Button::new("Set RTC to host clock (UTC)"))
            .clicked()
        {
            Self::apply(
                HwChange::RtcSeconds(utc_seconds_of_day(SystemTime::now())),
                cx,
            );
        }
    }
}
