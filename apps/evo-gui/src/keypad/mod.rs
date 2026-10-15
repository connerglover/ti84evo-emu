//! Calculator keys and the keypad layout table.

pub mod geometry;
pub mod paint;

use serde::{Deserialize, Serialize};

use geometry::key_rect;

/// One calculator key, in keypad layout order (top to bottom, left to right).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum KeyId {
    YEqu,
    Window,
    Zoom,
    Trace,
    Graph,
    Second,
    Mode,
    Del,
    Up,
    Left,
    Right,
    Down,
    Alpha,
    XTThetaN,
    Stat,
    Math,
    /// The fraction key (n/d), where the TI-84 Plus CE has APPS. Preferences saved before
    /// the GUI followed the Evo keypad call it `Apps`.
    #[serde(alias = "Apps")]
    Frac,
    Prgm,
    Vars,
    Clear,
    /// `^`, where the TI-84 Plus CE has x⁻¹. Preferences saved before the GUI followed the
    /// Evo keypad call the key here `Inverse`.
    #[serde(alias = "Inverse")]
    Power,
    Sin,
    Cos,
    Tan,
    Div,
    Square,
    Comma,
    LParen,
    RParen,
    Mul,
    Log,
    Seven,
    Eight,
    Nine,
    Sub,
    Ln,
    Four,
    Five,
    Six,
    Add,
    Sto,
    One,
    Two,
    Three,
    /// TOGGLE (answer ↔ fraction), where the TI-84 Plus CE has +.
    Toggle,
    On,
    Zero,
    Dot,
    Neg,
    Enter,
}

impl KeyId {
    /// Every key, in layout order.
    pub const ALL: [KeyId; 50] = [
        KeyId::YEqu,
        KeyId::Window,
        KeyId::Zoom,
        KeyId::Trace,
        KeyId::Graph,
        KeyId::Second,
        KeyId::Mode,
        KeyId::Del,
        KeyId::Up,
        KeyId::Left,
        KeyId::Right,
        KeyId::Down,
        KeyId::Alpha,
        KeyId::XTThetaN,
        KeyId::Stat,
        KeyId::Math,
        KeyId::Frac,
        KeyId::Prgm,
        KeyId::Vars,
        KeyId::Clear,
        KeyId::Power,
        KeyId::Sin,
        KeyId::Cos,
        KeyId::Tan,
        KeyId::Div,
        KeyId::Square,
        KeyId::Comma,
        KeyId::LParen,
        KeyId::RParen,
        KeyId::Mul,
        KeyId::Log,
        KeyId::Seven,
        KeyId::Eight,
        KeyId::Nine,
        KeyId::Sub,
        KeyId::Ln,
        KeyId::Four,
        KeyId::Five,
        KeyId::Six,
        KeyId::Add,
        KeyId::Sto,
        KeyId::One,
        KeyId::Two,
        KeyId::Three,
        KeyId::Toggle,
        KeyId::On,
        KeyId::Zero,
        KeyId::Dot,
        KeyId::Neg,
        KeyId::Enter,
    ];

    /// The key's visible main label (for the D-pad: its accessibility label).
    pub fn name(self) -> &'static str {
        match self {
            KeyId::YEqu => "y=",
            KeyId::Window => "window",
            KeyId::Zoom => "zoom",
            KeyId::Trace => "trace",
            KeyId::Graph => "graph",
            KeyId::Second => "2nd",
            KeyId::Mode => "mode",
            KeyId::Del => "del",
            KeyId::Up => "up",
            KeyId::Left => "left",
            KeyId::Right => "right",
            KeyId::Down => "down",
            KeyId::Alpha => "alpha",
            KeyId::XTThetaN => "X,T,\u{3B8},n",
            KeyId::Stat => "stat",
            KeyId::Math => "math",
            KeyId::Frac => "n/d",
            KeyId::Prgm => "prgm",
            KeyId::Vars => "vars",
            KeyId::Clear => "clear",
            KeyId::Sin => "sin",
            KeyId::Cos => "cos",
            KeyId::Tan => "tan",
            KeyId::Power => "^",
            KeyId::Square => "x\u{B2}",
            KeyId::Comma => ",",
            KeyId::LParen => "(",
            KeyId::RParen => ")",
            KeyId::Div => "\u{F7}",
            KeyId::Log => "log",
            KeyId::Seven => "7",
            KeyId::Eight => "8",
            KeyId::Nine => "9",
            KeyId::Mul => "\u{D7}",
            KeyId::Ln => "ln",
            KeyId::Four => "4",
            KeyId::Five => "5",
            KeyId::Six => "6",
            KeyId::Sub => "\u{2212}",
            KeyId::Sto => "sto\u{2192}",
            KeyId::One => "1",
            KeyId::Two => "2",
            KeyId::Three => "3",
            KeyId::Add => "+",
            KeyId::Toggle => "toggle",
            KeyId::On => "on",
            KeyId::Zero => "0",
            KeyId::Dot => ".",
            KeyId::Neg => "(\u{2212})",
            KeyId::Enter => "enter",
        }
    }

    /// The emulator core's name for the key (its keypad table, `docs/hardware/keypad.md`
    /// "Key map"; key scripts use the same names). The machine backend presses the core key
    /// of this name.
    pub fn core_name(self) -> &'static str {
        match self {
            KeyId::YEqu => "yequ",
            KeyId::Window => "window",
            KeyId::Zoom => "zoom",
            KeyId::Trace => "trace",
            KeyId::Graph => "graph",
            KeyId::Second => "2nd",
            KeyId::Mode => "mode",
            KeyId::Del => "del",
            KeyId::Up => "up",
            KeyId::Left => "left",
            KeyId::Right => "right",
            KeyId::Down => "down",
            KeyId::Alpha => "alpha",
            KeyId::XTThetaN => "xton",
            KeyId::Stat => "stat",
            KeyId::Math => "math",
            KeyId::Frac => "frac",
            KeyId::Prgm => "prgm",
            KeyId::Vars => "vars",
            KeyId::Clear => "clear",
            KeyId::Power => "power",
            KeyId::Sin => "sin",
            KeyId::Cos => "cos",
            KeyId::Tan => "tan",
            KeyId::Div => "div",
            KeyId::Square => "square",
            KeyId::Comma => "comma",
            KeyId::LParen => "lparen",
            KeyId::RParen => "rparen",
            KeyId::Mul => "mul",
            KeyId::Log => "log",
            KeyId::Seven => "7",
            KeyId::Eight => "8",
            KeyId::Nine => "9",
            KeyId::Sub => "sub",
            KeyId::Ln => "ln",
            KeyId::Four => "4",
            KeyId::Five => "5",
            KeyId::Six => "6",
            KeyId::Add => "add",
            KeyId::Sto => "sto",
            KeyId::One => "1",
            KeyId::Two => "2",
            KeyId::Three => "3",
            KeyId::Toggle => "toggle",
            KeyId::On => "on",
            KeyId::Zero => "0",
            KeyId::Dot => "decpnt",
            KeyId::Neg => "chs",
            KeyId::Enter => "enter",
        }
    }
}

/// Color family of a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStyle {
    /// The row-0 graphing keys.
    Function,
    /// Ordinary keys.
    Gray,
    /// Digits, `.` and `(−)`.
    Number,
    /// The `2nd` key.
    Second,
    /// The `alpha` key.
    Alpha,
    /// The four arrow wedges.
    Dpad,
}

/// Direction of a D-pad wedge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// Outline of a key, in keypad du.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KeyShape {
    /// A rounded rectangle.
    Rect(egui::Rect),
    /// A D-pad wedge around [`geometry::DPAD_CENTER`].
    Wedge(Dir),
}

/// One row of the layout table.
#[derive(Clone, Copy, Debug)]
pub struct KeyDef {
    pub id: KeyId,
    /// Blue legend, left-aligned over the key.
    pub second: Option<&'static str>,
    /// Green legend, right-aligned over the key.
    pub alpha: Option<&'static str>,
    pub style: KeyStyle,
    pub shape: KeyShape,
}

const fn key(
    id: KeyId,
    second: Option<&'static str>,
    alpha: Option<&'static str>,
    style: KeyStyle,
    row: usize,
    col: usize,
) -> KeyDef {
    KeyDef {
        id,
        second,
        alpha,
        style,
        shape: KeyShape::Rect(key_rect(row, col)),
    }
}

const fn wedge(id: KeyId, dir: Dir) -> KeyDef {
    KeyDef {
        id,
        second: None,
        alpha: None,
        style: KeyStyle::Dpad,
        shape: KeyShape::Wedge(dir),
    }
}

/// The layout table, in [`KeyId::ALL`] order.
///
/// Columns: key, second (blue) legend, alpha (green) legend, style, row, column.
// TI-84 Plus CE legends, lowercase, plus the Evo renames from spec §6.4 (`plot`,
// `insert`, `angle`, `recall`), on the TI-84 Evo keypad (`docs/hardware/keypad.md`
// "Key map"): the fraction key (n/d) where the CE has APPS, ^ where it has x⁻¹,
// ÷ × − + one row up from the CE's, and TOGGLE where the CE has +. The alpha
// letters, ^'s 2nd ˣ√ and TOGGLE's 2nd MEM / alpha " are checked on the OS
// (`all_keys_type_expected_text`). Unverified: the 2nd legends of ÷ × − +, kept
// from the CE key at the same place (π, e, [, ]); whether the Evo renames any
// other legend; and whether the `on` key face shows `⌂` (spec: `on ⌂`). Check
// against the Evo web emulator (the user offered access) and edit only this table.
#[rustfmt::skip]
pub static KEYS: [KeyDef; 50] = {
    use KeyId::*;
    use KeyStyle::{Alpha as A, Function as F, Gray as G, Number as N, Second as S};
    [
        key(YEqu,     Some("plot"),     Some("f1"),       F, 0, 0),
        key(Window,   Some("tblset"),   Some("f2"),       F, 0, 1),
        key(Zoom,     Some("format"),   Some("f3"),       F, 0, 2),
        key(Trace,    Some("calc"),     Some("f4"),       F, 0, 3),
        key(Graph,    Some("table"),    Some("f5"),       F, 0, 4),
        key(Second,   None,             None,             S, 1, 0),
        key(Mode,     Some("quit"),     None,             G, 1, 1),
        key(Del,      Some("insert"),   None,             G, 1, 2),
        wedge(Up, Dir::Up),
        wedge(Left, Dir::Left),
        wedge(Right, Dir::Right),
        wedge(Down, Dir::Down),
        key(Alpha,    Some("A-lock"),   None,             A, 2, 0),
        key(XTThetaN, Some("link"),     None,             G, 2, 1),
        key(Stat,     Some("list"),     None,             G, 2, 2),
        key(Math,     Some("test"),     Some("A"),        G, 3, 0),
        key(Frac,     Some("angle"),    Some("B"),        G, 3, 1),
        key(Prgm,     Some("draw"),     Some("C"),        G, 3, 2),
        key(Vars,     Some("distr"),    None,             G, 3, 3),
        key(Clear,    None,             None,             G, 3, 4),
        key(Power,    Some("\u{2E3}\u{221A}"), Some("D"), G, 4, 0),
        key(Sin,      Some("sin\u{207B}\u{B9}"), Some("E"), G, 4, 1),
        key(Cos,      Some("cos\u{207B}\u{B9}"), Some("F"), G, 4, 2),
        key(Tan,      Some("tan\u{207B}\u{B9}"), Some("G"), G, 4, 3),
        key(Div,      Some("\u{3C0}"),  Some("H"),        G, 4, 4),
        key(Square,   Some("\u{221A}"), Some("I"),        G, 5, 0),
        key(Comma,    Some("EE"),       Some("J"),        G, 5, 1),
        key(LParen,   Some("{"),        Some("K"),        G, 5, 2),
        key(RParen,   Some("}"),        Some("L"),        G, 5, 3),
        key(Mul,      Some("e"),        Some("M"),        G, 5, 4),
        key(Log,      Some("10\u{2E3}"), Some("N"),       G, 6, 0),
        key(Seven,    Some("u"),        Some("O"),        N, 6, 1),
        key(Eight,    Some("v"),        Some("P"),        N, 6, 2),
        key(Nine,     Some("w"),        Some("Q"),        N, 6, 3),
        key(Sub,      Some("["),        Some("R"),        G, 6, 4),
        key(Ln,       Some("e\u{2E3}"), Some("S"),        G, 7, 0),
        key(Four,     Some("L4"),       Some("T"),        N, 7, 1),
        key(Five,     Some("L5"),       Some("U"),        N, 7, 2),
        key(Six,      Some("L6"),       Some("V"),        N, 7, 3),
        key(Add,      Some("]"),        Some("W"),        G, 7, 4),
        key(Sto,      Some("recall"),   Some("X"),        G, 8, 0),
        key(One,      Some("L1"),       Some("Y"),        N, 8, 1),
        key(Two,      Some("L2"),       Some("Z"),        N, 8, 2),
        key(Three,    Some("L3"),       Some("\u{3B8}"),  N, 8, 3),
        key(Toggle,   Some("mem"),      Some("\""),       G, 8, 4),
        key(On,       Some("off"),      None,             G, 9, 0),
        key(Zero,     Some("catalog"),  Some("\u{2423}"), N, 9, 1),
        key(Dot,      Some("i"),        Some(":"),        N, 9, 2),
        key(Neg,      Some("ans"),      Some("?"),        N, 9, 3),
        key(Enter,    Some("entry"),    Some("solve"),    G, 9, 4),
    ]
};

/// The layout-table row of `id`.
pub fn key_def(id: KeyId) -> &'static KeyDef {
    // `KEYS` is in `KeyId::ALL` order, which is the enum's declaration order
    // (checked by the `table_order_matches_key_ids` test).
    &KEYS[id as usize]
}
