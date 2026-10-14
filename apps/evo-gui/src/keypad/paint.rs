//! Painting the calculator body and keys from the layout table.
//!
//! Everything is drawn from [`super::KEYS`] and the geometry constants with
//! egui shapes and plain-text legends; there is no key artwork.

use egui::epaint::{Mesh, PathShape};
use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, vec2};

use super::geometry::{
    BEZEL_RADIUS, BEZEL_RECT, BODY_H, BODY_W, CalcTransform, DPAD_CENTER, DPAD_R_IN, DPAD_R_OUT,
    KEY_RADIUS, KEYPAD_ORIGIN, ROW_TOPS, wedge_center_deg, wedge_half_span_deg,
};
use super::{Dir, KeyDef, KeyShape, KeyStyle};
use crate::theme::{
    BEZEL, BODY_FILL, BODY_OUTLINE, BODY_OUTLINE_W, BODY_RADIUS, HOVER_OUTLINE, HOVER_OUTLINE_W,
    KEY_GRAY_STROKE, KEY_SHADOW, KEY_SHADOW_OFFSET, LABEL_SIZE, LABEL_SIZE_ROW0, LEGEND_ALPHA,
    LEGEND_SECOND, LEGEND_SIZE, key_colors, pressed,
};

/// Width of the [`KEY_GRAY_STROKE`] outline, in du.
const KEY_STROKE_W: f32 = 1.0;
/// How far a fully pressed key moves down, in du.
const PRESS_DROP: f32 = 1.0;
/// Height of the legend band above each key row, in du.
const LEGEND_BAND: f32 = 14.0;
/// Arc segments per wedge edge.
const WEDGE_STEPS: usize = 16;
/// Half the base and the height of a wedge's arrow mark, in du.
const ARROW_HALF: f32 = 4.0;
const ARROW_LEN: f32 = 5.0;

/// A rect in keypad du to screen points.
pub fn keypad_rect_to_screen(t: &CalcTransform, r: Rect) -> Rect {
    Rect::from_min_max(
        t.to_screen(KEYPAD_ORIGIN + r.min.to_vec2()),
        t.to_screen(KEYPAD_ORIGIN + r.max.to_vec2()),
    )
}

/// A rect in body du to screen points.
pub fn body_rect_to_screen(t: &CalcTransform, r: Rect) -> Rect {
    Rect::from_min_max(t.to_screen(r.min), t.to_screen(r.max))
}

/// Paints the body (fill and outline) and the LCD bezel.
pub fn paint_body(painter: &Painter, t: &CalcTransform) {
    let body = body_rect_to_screen(t, Rect::from_min_size(Pos2::ZERO, vec2(BODY_W, BODY_H)));
    painter.rect(
        body,
        BODY_RADIUS * t.scale,
        BODY_FILL,
        Stroke::new(BODY_OUTLINE_W * t.scale, BODY_OUTLINE),
        StrokeKind::Middle,
    );
    painter.rect_filled(
        body_rect_to_screen(t, BEZEL_RECT),
        BEZEL_RADIUS * t.scale,
        BEZEL,
    );
}

/// Paints one key with its main label and legends.
///
/// `press` in `0.0..=1.0` blends the fill toward the pressed color, moves the
/// key down by up to 1 du and fades its shadow. `hovered` adds the hover
/// outline.
pub fn paint_key(painter: &Painter, t: &CalcTransform, def: &KeyDef, press: f32, hovered: bool) {
    let press = if press.is_nan() {
        0.0
    } else {
        press.clamp(0.0, 1.0)
    };
    let (base, label) = key_colors(def.style);
    let fill = base.lerp_to_gamma(pressed(base), press);
    let shadow = KEY_SHADOW.gamma_multiply(1.0 - press);
    let drop = vec2(0.0, PRESS_DROP * press * t.scale);
    let shadow_offset = KEY_SHADOW_OFFSET * t.scale;
    match def.shape {
        KeyShape::Rect(r) => {
            let rest = keypad_rect_to_screen(t, r);
            let rect = rest.translate(drop);
            let radius = KEY_RADIUS * t.scale;
            painter.rect_filled(rest.translate(shadow_offset), radius, shadow);
            let stroke = match def.style {
                KeyStyle::Function | KeyStyle::Gray => {
                    Stroke::new(KEY_STROKE_W * t.scale, KEY_GRAY_STROKE)
                }
                _ => Stroke::NONE,
            };
            painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
            if hovered {
                painter.rect_stroke(
                    rect,
                    radius,
                    Stroke::new(HOVER_OUTLINE_W * t.scale, HOVER_OUTLINE),
                    StrokeKind::Outside,
                );
            }
            let size = if r.min.y < ROW_TOPS[1] {
                LABEL_SIZE_ROW0
            } else {
                LABEL_SIZE
            };
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                def.id.name(),
                FontId::proportional(size * t.scale),
                label,
            );
            paint_legends(painter, t, def, r);
        }
        KeyShape::Wedge(dir) => {
            let outline = wedge_outline(t, dir);
            painter.add(wedge_shape(&outline, shadow_offset, shadow));
            painter.add(wedge_shape(&outline, drop, fill));
            if hovered {
                let pts = outline.iter().map(|p| *p + drop).collect();
                painter.add(PathShape::closed_line(
                    pts,
                    Stroke::new(HOVER_OUTLINE_W * t.scale, HOVER_OUTLINE),
                ));
            }
            painter.add(arrow_mark(t, dir, drop, label));
        }
    }
}

/// The blue legend left-aligned and the green legend right-aligned in the
/// band above the key.
fn paint_legends(painter: &Painter, t: &CalcTransform, def: &KeyDef, r: Rect) {
    let y = r.min.y - LEGEND_BAND / 2.0;
    let font = FontId::proportional(LEGEND_SIZE * t.scale);
    if let Some(s) = def.second {
        let p = t.to_screen(KEYPAD_ORIGIN + vec2(r.min.x, y));
        painter.text(p, Align2::LEFT_CENTER, s, font.clone(), LEGEND_SECOND);
    }
    if let Some(s) = def.alpha {
        let p = t.to_screen(KEYPAD_ORIGIN + vec2(r.max.x, y));
        painter.text(p, Align2::RIGHT_CENTER, s, font, LEGEND_ALPHA);
    }
}

/// A unit vector at `deg` degrees (screen y down).
fn unit(deg: f32) -> Vec2 {
    let a = deg.to_radians();
    vec2(a.cos(), a.sin())
}

/// The wedge's outline in screen points: the outer arc, then the inner arc
/// reversed.
fn wedge_outline(t: &CalcTransform, dir: Dir) -> Vec<Pos2> {
    let c = wedge_center_deg(dir);
    let h = wedge_half_span_deg();
    let center = KEYPAD_ORIGIN + DPAD_CENTER.to_vec2();
    let arc = |radius: f32, i: usize| {
        let deg = c - h + 2.0 * h * i as f32 / WEDGE_STEPS as f32;
        t.to_screen(center + unit(deg) * radius)
    };
    let mut pts: Vec<Pos2> = (0..=WEDGE_STEPS).map(|i| arc(DPAD_R_OUT, i)).collect();
    pts.extend((0..=WEDGE_STEPS).rev().map(|i| arc(DPAD_R_IN, i)));
    pts
}

/// A filled wedge (a triangle mesh, since the sector is not convex) with an
/// anti-aliased edge of the same color.
fn wedge_shape(outline: &[Pos2], offset: Vec2, color: Color32) -> Shape {
    let n = WEDGE_STEPS + 1;
    let mut mesh = Mesh::default();
    for p in outline {
        mesh.colored_vertex(*p + offset, color);
    }
    // outline[i] is outer step i; outline[2n-1-i] is inner step i.
    let inner = |i: usize| (2 * n - 1 - i) as u32;
    for i in 0..WEDGE_STEPS {
        let (o0, o1) = (i as u32, (i + 1) as u32);
        let (i0, i1) = (inner(i), inner(i + 1));
        mesh.add_triangle(o0, o1, i0);
        mesh.add_triangle(i0, o1, i1);
    }
    let pts = outline.iter().map(|p| *p + offset).collect();
    Shape::Vec(vec![
        Shape::mesh(mesh),
        Shape::Path(PathShape::closed_line(pts, Stroke::new(1.0, color))),
    ])
}

/// A small triangle pointing outward in the middle of a wedge.
fn arrow_mark(t: &CalcTransform, dir: Dir, offset: Vec2, color: Color32) -> Shape {
    let u = unit(wedge_center_deg(dir));
    let n = vec2(-u.y, u.x);
    let mid = KEYPAD_ORIGIN + DPAD_CENTER.to_vec2() + u * (DPAD_R_IN + DPAD_R_OUT) / 2.0;
    let base = mid - u * (ARROW_LEN / 2.0);
    let pts = [
        base + n * ARROW_HALF,
        mid + u * (ARROW_LEN / 2.0),
        base - n * ARROW_HALF,
    ]
    .iter()
    .map(|p| t.to_screen(*p) + offset)
    .collect();
    Shape::convex_polygon(pts, color, Stroke::NONE)
}
