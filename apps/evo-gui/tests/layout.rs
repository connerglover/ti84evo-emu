use egui::{FontId, Pos2, RawInput, Rect, Vec2, pos2, vec2};
use evo_gui::keypad::geometry::{
    BODY_H, BODY_W, CalcTransform, DPAD_CENTER, KEYPAD_H, KEYPAD_ORIGIN, KEYPAD_W, bounds,
    dpad_bounds, hit_test,
};
use evo_gui::keypad::{Dir, KEYS, KeyId, KeyShape, KeyStyle, key_def};
use evo_gui::theme::{has_glyphs, install_fonts};

fn legends(id: KeyId) -> (Option<&'static str>, Option<&'static str>) {
    let k = key_def(id);
    (k.second, k.alpha)
}

#[test]
fn table_order_matches_key_ids() {
    assert_eq!(KEYS.map(|k| k.id), KeyId::ALL);
    for id in KeyId::ALL {
        assert_eq!(key_def(id).id, id);
    }
}

#[test]
fn evo_legends() {
    use KeyId::*;
    #[rustfmt::skip]
    let expected: [(KeyId, Option<&str>, Option<&str>); 50] = [
        (YEqu, Some("plot"), Some("f1")),
        (Window, Some("tblset"), Some("f2")),
        (Zoom, Some("format"), Some("f3")),
        (Trace, Some("calc"), Some("f4")),
        (Graph, Some("table"), Some("f5")),
        (Second, None, None),
        (Mode, Some("quit"), None),
        (Del, Some("insert"), None),
        (Up, None, None),
        (Left, None, None),
        (Right, None, None),
        (Down, None, None),
        (Alpha, Some("A-lock"), None),
        (XTThetaN, Some("link"), None),
        (Stat, Some("list"), None),
        (Math, Some("test"), Some("A")),
        (Apps, Some("angle"), Some("B")),
        (Prgm, Some("draw"), Some("C")),
        (Vars, Some("distr"), None),
        (Clear, None, None),
        (Inverse, Some("matrix"), Some("D")),
        (Sin, Some("sin\u{207B}\u{B9}"), Some("E")),
        (Cos, Some("cos\u{207B}\u{B9}"), Some("F")),
        (Tan, Some("tan\u{207B}\u{B9}"), Some("G")),
        (Power, Some("\u{3C0}"), Some("H")),
        (Square, Some("\u{221A}"), Some("I")),
        (Comma, Some("EE"), Some("J")),
        (LParen, Some("{"), Some("K")),
        (RParen, Some("}"), Some("L")),
        (Div, Some("e"), Some("M")),
        (Log, Some("10\u{2E3}"), Some("N")),
        (Seven, Some("u"), Some("O")),
        (Eight, Some("v"), Some("P")),
        (Nine, Some("w"), Some("Q")),
        (Mul, Some("["), Some("R")),
        (Ln, Some("e\u{2E3}"), Some("S")),
        (Four, Some("L4"), Some("T")),
        (Five, Some("L5"), Some("U")),
        (Six, Some("L6"), Some("V")),
        (Sub, Some("]"), Some("W")),
        (Sto, Some("recall"), Some("X")),
        (One, Some("L1"), Some("Y")),
        (Two, Some("L2"), Some("Z")),
        (Three, Some("L3"), Some("\u{3B8}")),
        (Add, Some("mem"), Some("\"")),
        (On, Some("off"), None),
        (Zero, Some("catalog"), Some("\u{2423}")),
        (Dot, Some("i"), Some(":")),
        (Neg, Some("ans"), Some("?")),
        (Enter, Some("entry"), Some("solve")),
    ];
    for (id, second, alpha) in expected {
        assert_eq!(legends(id), (second, alpha), "{id:?}");
    }

    // Spot checks named by the plan.
    assert_eq!(legends(Del), (Some("insert"), None));
    assert_eq!(legends(Sto), (Some("recall"), Some("X")));
    assert_eq!(legends(YEqu), (Some("plot"), Some("f1")));
    assert_eq!(legends(Apps), (Some("angle"), Some("B")));
    assert_eq!(legends(Math), (Some("test"), Some("A")));
    assert_eq!(legends(Vars), (Some("distr"), None));
    assert_eq!(legends(Stat), (Some("list"), None));
    assert_eq!(legends(Inverse), (Some("matrix"), Some("D")));
    assert_eq!(legends(Clear), (None, None));
    assert_eq!(legends(Zero), (Some("catalog"), Some("\u{2423}")));
    assert_eq!(legends(Enter), (Some("entry"), Some("solve")));
    assert_eq!(legends(Add), (Some("mem"), Some("\"")));
    assert_eq!(legends(On), (Some("off"), None));
    assert_eq!(legends(Second), (None, None));
}

#[test]
fn alpha_letters_run_a_to_theta() {
    let start = KEYS.iter().position(|k| k.id == KeyId::Math).unwrap();
    let end = KEYS.iter().position(|k| k.id == KeyId::Three).unwrap();
    let letters: String = KEYS[start..=end]
        .iter()
        .filter_map(|k| k.alpha)
        .filter_map(|a| {
            let mut chars = a.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_uppercase() || c == '\u{3B8}' => Some(c),
                _ => None,
            }
        })
        .collect();
    assert_eq!(letters, "ABCDEFGHIJKLMNOPQRSTUVWXYZ\u{3B8}");
}

#[test]
fn punctuation_alpha_legends() {
    let got = [
        KeyId::Add,
        KeyId::Zero,
        KeyId::Dot,
        KeyId::Neg,
        KeyId::Enter,
    ]
    .map(|id| key_def(id).alpha);
    assert_eq!(
        got,
        [
            Some("\""),
            Some("\u{2423}"),
            Some(":"),
            Some("?"),
            Some("solve")
        ]
    );
}

#[test]
fn styles() {
    use KeyId::*;
    for k in &KEYS {
        let want = match k.id {
            Zero | One | Two | Three | Four | Five | Six | Seven | Eight | Nine | Dot | Neg => {
                KeyStyle::Number
            }
            Second => KeyStyle::Second,
            Alpha => KeyStyle::Alpha,
            Up | Down | Left | Right => KeyStyle::Dpad,
            YEqu | Window | Zoom | Trace | Graph => KeyStyle::Function,
            _ => KeyStyle::Gray,
        };
        assert_eq!(k.style, want, "{:?}", k.id);
    }
}

#[test]
fn shapes_are_wedges_only_for_arrows() {
    for k in &KEYS {
        let want_dir = match k.id {
            KeyId::Up => Some(Dir::Up),
            KeyId::Down => Some(Dir::Down),
            KeyId::Left => Some(Dir::Left),
            KeyId::Right => Some(Dir::Right),
            _ => None,
        };
        match (k.shape, want_dir) {
            (KeyShape::Wedge(d), Some(w)) => assert_eq!(d, w, "{:?}", k.id),
            (KeyShape::Rect(_), None) => {}
            (shape, _) => panic!("{:?} has shape {shape:?}", k.id),
        }
    }
}

#[test]
fn rect_key_geometry() {
    // Column x = 4 + 66c, width 56; row 0 top 14 height 26; row 9 top 500 height 32.
    assert_eq!(
        bounds(KeyId::YEqu),
        Rect::from_min_size(pos2(4.0, 14.0), vec2(56.0, 26.0))
    );
    assert_eq!(
        bounds(KeyId::Enter),
        Rect::from_min_size(pos2(268.0, 500.0), vec2(56.0, 32.0))
    );
    assert_eq!(
        bounds(KeyId::Stat),
        Rect::from_min_size(pos2(136.0, 122.0), vec2(56.0, 32.0))
    );
}

#[test]
fn keys_fit_and_do_not_overlap() {
    let pad = Rect::from_min_max(Pos2::ZERO, pos2(KEYPAD_W, KEYPAD_H));
    for k in &KEYS {
        assert!(pad.contains_rect(bounds(k.id)), "{:?} outside keypad", k.id);
    }
    assert!(pad.contains_rect(dpad_bounds()));
    let rects: Vec<(KeyId, Rect)> = KEYS
        .iter()
        .filter_map(|k| match k.shape {
            KeyShape::Rect(r) => Some((k.id, r)),
            KeyShape::Wedge(_) => None,
        })
        .collect();
    assert_eq!(rects.len(), 46);
    for (i, (a, ra)) in rects.iter().enumerate() {
        assert!(!ra.intersects(dpad_bounds()), "{a:?} overlaps the D-pad");
        for (b, rb) in &rects[i + 1..] {
            assert!(!ra.intersects(*rb), "{a:?} overlaps {b:?}");
        }
    }
}

fn polar(deg: f32, r: f32) -> Pos2 {
    let a = deg.to_radians();
    DPAD_CENTER + vec2(a.cos(), a.sin()) * r
}

#[test]
fn hit_test_finds_each_key() {
    for k in &KEYS {
        if let KeyShape::Rect(r) = k.shape {
            assert_eq!(hit_test(r.center()), Some(k.id), "{:?}", k.id);
        }
    }
    let dirs: [(KeyId, Vec2); 4] = [
        (KeyId::Up, vec2(0.0, -1.0)),
        (KeyId::Down, vec2(0.0, 1.0)),
        (KeyId::Left, vec2(-1.0, 0.0)),
        (KeyId::Right, vec2(1.0, 0.0)),
    ];
    for (id, d) in dirs {
        assert_eq!(hit_test(DPAD_CENTER + d * 29.0), Some(id), "{id:?}");
    }
    assert_eq!(hit_test(polar(220.0, 25.0)), Some(KeyId::Left));
    assert_eq!(hit_test(polar(145.0, 29.0)), Some(KeyId::Left));
    assert_eq!(hit_test(DPAD_CENTER), None);
    // Diagonal gaps between wedges and beyond the outer radius hit nothing.
    assert_eq!(hit_test(polar(-45.0, 29.0)), None);
    assert_eq!(hit_test(polar(0.0, 44.5)), None);
    assert_eq!(hit_test(pos2(65.0, 27.0)), None);
}

#[test]
fn transform_round_trips_at_three_quarter_scale() {
    let avail = Rect::from_min_size(Pos2::ZERO, vec2(279.0, 660.0));
    let t = CalcTransform::fit(avail);
    assert!((t.scale - 0.75).abs() < 1e-6, "scale {}", t.scale);
    for p in [
        Pos2::ZERO,
        pos2(BODY_W, BODY_H),
        pos2(123.4, 567.8),
        pos2(-5.0, 900.0),
    ] {
        let back = t.to_body(t.to_screen(p));
        assert!((back - p).length() < 1e-3, "{p:?} -> {back:?}");
    }
    let enter_body = KEYPAD_ORIGIN + bounds(KeyId::Enter).center().to_vec2();
    let screen = t.to_screen(enter_body);
    let pad = t.to_body(screen) - KEYPAD_ORIGIN.to_vec2();
    assert_eq!(hit_test(pad), Some(KeyId::Enter));
}

#[test]
fn transform_clamps_scale_and_centers() {
    let tiny = CalcTransform::fit(Rect::from_min_size(Pos2::ZERO, vec2(10.0, 10.0)));
    assert_eq!(tiny.scale, 0.4);
    let huge = CalcTransform::fit(Rect::from_min_size(Pos2::ZERO, vec2(10_000.0, 10_000.0)));
    assert_eq!(huge.scale, 3.0);
    let avail = Rect::from_min_size(pos2(100.0, 50.0), vec2(1000.0, 660.0));
    let t = CalcTransform::fit(avail);
    let center = t.to_screen(pos2(BODY_W / 2.0, BODY_H / 2.0));
    assert!((center - avail.center()).length() < 1e-3);
}

/// A context with [`install_fonts`] applied (fonts load on the first pass).
fn font_ctx(install: bool) -> egui::Context {
    let ctx = egui::Context::default();
    if install {
        install_fonts(&ctx);
    }
    // egui 0.36 names the one-pass entry point `run_ui` (the plan says `run`).
    let mut out = ctx.run_ui(RawInput::default(), |_| {});
    // egui 0.36 asserts (debug) that texture deltas are consumed before drop.
    out.textures_delta.clear();
    ctx
}

#[test]
fn glyph_check_detects_missing_glyphs() {
    let ctx = font_ctx(true);
    // U+0378 is unassigned; no font has it.
    assert!(!has_glyphs(&ctx, &FontId::proportional(12.0), "\u{378}"));
    assert!(!has_glyphs(&ctx, &FontId::monospace(12.0), "a\u{378}"));
    // Without DejaVu, egui's bundled fonts lack the open-box legend.
    let bare = font_ctx(false);
    assert!(!has_glyphs(&bare, &FontId::proportional(12.0), "\u{2423}"));
}

#[test]
fn every_label_glyph_is_in_the_fonts() {
    let ctx = font_ctx(true);
    let prop = FontId::proportional(12.0);
    for k in &KEYS {
        for s in [Some(k.id.name()), k.second, k.alpha].into_iter().flatten() {
            assert!(
                has_glyphs(&ctx, &prop, s),
                "missing glyph in {s:?} ({:?})",
                k.id
            );
        }
    }
    assert!(has_glyphs(
        &ctx,
        &FontId::monospace(12.0),
        "\u{25CF}\u{25B6}?.|"
    ));
}

#[test]
fn theme_tokens() {
    use egui::Color32;
    use evo_gui::theme::*;
    assert_eq!(APP_BG, Color32::from_rgb(0xD6, 0xE9, 0xF8));
    assert_eq!(BODY_OUTLINE, Color32::from_rgb(0x1F, 0x6F, 0xB5));
    assert_eq!(KEY_SHADOW, Color32::from_rgba_unmultiplied(0, 0, 0, 0x26));
    assert_eq!(
        pressed(Color32::from_rgb(100, 200, 0)),
        Color32::from_rgb(82, 164, 0)
    );
    assert_eq!(key_colors(KeyStyle::Function), (KEY_GRAY, LABEL_DARK));
    assert_eq!(key_colors(KeyStyle::Dpad), (KEY_NUMBER, LABEL_LIGHT));
    assert_eq!(key_colors(KeyStyle::Second), (KEY_SECOND, LABEL_LIGHT));
    assert_eq!(key_colors(KeyStyle::Alpha), (KEY_ALPHA, LABEL_LIGHT));
}
