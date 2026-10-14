//! Calculator geometry in design units ("du").
//!
//! Body du have their origin at the top-left of the calculator body. Keypad du
//! have their origin at [`KEYPAD_ORIGIN`] in body du. [`CalcTransform`] maps
//! body du to screen points.

use egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::{Dir, KeyId, KeyShape, key_def};

/// Width of the calculator body.
pub const BODY_W: f32 = 372.0;
/// Height of the calculator body.
pub const BODY_H: f32 = 880.0;
/// Top-left of the keypad, in body du.
pub const KEYPAD_ORIGIN: Pos2 = pos2(22.0, 312.0);
/// Width of the keypad.
pub const KEYPAD_W: f32 = 328.0;
/// Height of the keypad.
pub const KEYPAD_H: f32 = 540.0;
/// The 320x240 LCD, in body du.
pub const LCD_RECT: Rect = Rect::from_min_max(pos2(26.0, 40.0), pos2(346.0, 280.0));
/// The dark bezel around the LCD, in body du (corner radius [`BEZEL_RADIUS`]).
pub const BEZEL_RECT: Rect = Rect::from_min_max(pos2(18.0, 30.0), pos2(354.0, 290.0));
/// Corner radius of [`BEZEL_RECT`].
pub const BEZEL_RADIUS: f32 = 12.0;

/// Center of the D-pad, in keypad du.
pub const DPAD_CENTER: Pos2 = pos2(263.0, 111.0);
/// Inner radius of a D-pad wedge.
pub const DPAD_R_IN: f32 = 14.0;
/// Outer radius of a D-pad wedge.
pub const DPAD_R_OUT: f32 = 44.0;
/// Angle left empty between neighboring wedges.
pub const DPAD_GAP_DEG: f32 = 6.0;

/// Left edge of rect-key column 0; column `c` starts at `KEY_X0 + KEY_PITCH_X * c`.
pub const KEY_X0: f32 = 4.0;
/// Horizontal distance between key columns.
pub const KEY_PITCH_X: f32 = 66.0;
/// Width of a rect key.
pub const KEY_W: f32 = 56.0;
/// Top of each key row; the 14 du above each top hold the legends.
pub const ROW_TOPS: [f32; 10] = [
    14.0, 68.0, 122.0, 176.0, 230.0, 284.0, 338.0, 392.0, 446.0, 500.0,
];
/// Height of the row-0 keys.
pub const ROW0_H: f32 = 26.0;
/// Height of the keys in rows 1–9.
pub const KEY_H: f32 = 32.0;
/// Corner radius of a rect key.
pub const KEY_RADIUS: f32 = 8.0;

/// The rect of the key at `row`, `col`, in keypad du.
pub const fn key_rect(row: usize, col: usize) -> Rect {
    let x = KEY_X0 + KEY_PITCH_X * col as f32;
    let y = ROW_TOPS[row];
    let h = if row == 0 { ROW0_H } else { KEY_H };
    Rect::from_min_max(pos2(x, y), pos2(x + KEY_W, y + h))
}

/// The D-pad's bounding box in keypad du: `DPAD_CENTER ± DPAD_R_OUT`.
pub fn dpad_bounds() -> Rect {
    Rect::from_center_size(DPAD_CENTER, Vec2::splat(2.0 * DPAD_R_OUT))
}

/// Direction of a wedge's center line in degrees (screen y points down).
pub fn wedge_center_deg(dir: Dir) -> f32 {
    match dir {
        Dir::Right => 0.0,
        Dir::Down => 90.0,
        Dir::Left => 180.0,
        Dir::Up => -90.0,
    }
}

/// Half of the angle a wedge spans.
pub fn wedge_half_span_deg() -> f32 {
    (90.0 - DPAD_GAP_DEG) / 2.0
}

/// Signed angle difference `a - b` folded into `(-180, 180]`.
fn angle_diff_deg(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

fn wedge_contains(dir: Dir, p: Pos2) -> bool {
    let v = p - DPAD_CENTER;
    let r = v.length();
    if !(DPAD_R_IN..=DPAD_R_OUT).contains(&r) {
        return false;
    }
    let ang = v.y.atan2(v.x).to_degrees();
    angle_diff_deg(ang, wedge_center_deg(dir)).abs() <= wedge_half_span_deg()
}

/// Bounding box of a wedge in keypad du (sampled along both arcs).
pub fn wedge_bounds(dir: Dir) -> Rect {
    let c = wedge_center_deg(dir);
    let h = wedge_half_span_deg();
    let mut r = Rect::NOTHING;
    const STEPS: usize = 32;
    for i in 0..=STEPS {
        let a = (c - h + 2.0 * h * i as f32 / STEPS as f32).to_radians();
        let u = vec2(a.cos(), a.sin());
        r.extend_with(DPAD_CENTER + u * DPAD_R_IN);
        r.extend_with(DPAD_CENTER + u * DPAD_R_OUT);
    }
    // The outer arc's extreme point lies exactly on the center line.
    let a = c.to_radians();
    r.extend_with(DPAD_CENTER + vec2(a.cos(), a.sin()) * DPAD_R_OUT);
    r
}

/// The key's bounds in keypad du; for a wedge, its bounding box.
pub fn bounds(id: KeyId) -> Rect {
    match key_def(id).shape {
        KeyShape::Rect(r) => r,
        KeyShape::Wedge(d) => wedge_bounds(d),
    }
}

/// The key under `p` (keypad du), if any.
pub fn hit_test(p: Pos2) -> Option<KeyId> {
    super::KEYS
        .iter()
        .find(|k| match k.shape {
            KeyShape::Rect(r) => r.contains(p),
            KeyShape::Wedge(d) => wedge_contains(d, p),
        })
        .map(|k| k.id)
}

/// Maps calculator body du to screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalcTransform {
    /// Screen position of body du (0, 0).
    pub origin: Pos2,
    /// Screen points per du.
    pub scale: f32,
}

impl CalcTransform {
    /// Smallest allowed scale.
    pub const MIN_SCALE: f32 = 0.4;
    /// Largest allowed scale.
    pub const MAX_SCALE: f32 = 3.0;

    /// Fits the body into `avail`: `scale = min(w/BODY_W, h/BODY_H)` clamped to
    /// `MIN_SCALE..=MAX_SCALE`, with the body centered in `avail`.
    pub fn fit(avail: Rect) -> Self {
        let raw = (avail.width() / BODY_W).min(avail.height() / BODY_H);
        let scale = if raw.is_finite() {
            raw.clamp(Self::MIN_SCALE, Self::MAX_SCALE)
        } else {
            Self::MIN_SCALE
        };
        let origin = avail.center() - vec2(BODY_W, BODY_H) * scale / 2.0;
        Self { origin, scale }
    }

    /// Body du to screen points.
    pub fn to_screen(&self, body_du: Pos2) -> Pos2 {
        self.origin + body_du.to_vec2() * self.scale
    }

    /// Screen points to body du.
    pub fn to_body(&self, screen: Pos2) -> Pos2 {
        Pos2::ZERO + (screen - self.origin) / self.scale
    }
}
