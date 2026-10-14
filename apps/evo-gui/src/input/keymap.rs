//! PC-keyboard bindings for calculator keys, and the tracker that turns egui key
//! events into calculator key presses and releases.
//!
//! egui-winit 0.36.2 reports the logical key when egui has a variant for it, else
//! the physical key. On a US layout Shift+= arrives as [`Key::Plus`] with shift
//! set, while Shift+8/9/0/6 arrive as `Num8`/`Num9`/`Num0`/`Num6` with shift set,
//! because egui has no `*`, `(`, `)` or `^` key. The default shift bindings
//! assume a US layout; other layouts rebind them.

use std::collections::HashMap;

use egui::{Event, Key};
use serde::{Deserialize, Serialize};

use crate::keypad::{KEYS, KeyId};

/// A PC key, with or without shift held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PcKey {
    pub key: egui::Key,
    pub shift: bool,
}

impl PcKey {
    const fn plain(key: Key) -> Self {
        Self { key, shift: false }
    }

    const fn shifted(key: Key) -> Self {
        Self { key, shift: true }
    }
}

/// PC-key to calculator-key bindings. A PC key appears at most once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keymap {
    pub bindings: Vec<(PcKey, KeyId)>,
}

/// The fixed (non-letter) part of the default map.
const DEFAULT_FIXED: [(PcKey, KeyId); 44] = {
    use KeyId::*;
    const fn p(key: Key) -> PcKey {
        PcKey::plain(key)
    }
    const fn s(key: Key) -> PcKey {
        PcKey::shifted(key)
    }
    [
        (p(Key::Num0), Zero),
        (p(Key::Num1), One),
        (p(Key::Num2), Two),
        (p(Key::Num3), Three),
        (p(Key::Num4), Four),
        (p(Key::Num5), Five),
        (p(Key::Num6), Six),
        (p(Key::Num7), Seven),
        (p(Key::Num8), Eight),
        (p(Key::Num9), Nine),
        (p(Key::Period), Dot),
        (p(Key::Comma), Comma),
        // Shift+= arrives as `Plus` with shift set and falls back to this binding.
        (p(Key::Plus), Add),
        (p(Key::Minus), Sub),
        (s(Key::Minus), Neg),
        (s(Key::Num8), Mul),
        (p(Key::Slash), Div),
        (s(Key::Num6), Power),
        (s(Key::Num9), LParen),
        (s(Key::Num0), RParen),
        (p(Key::Enter), Enter),
        (p(Key::Backspace), Del),
        (p(Key::Delete), Del),
        (p(Key::Escape), Clear),
        (p(Key::ArrowUp), Up),
        (p(Key::ArrowDown), Down),
        (p(Key::ArrowLeft), Left),
        (p(Key::ArrowRight), Right),
        (p(Key::F1), YEqu),
        (p(Key::F2), Window),
        (p(Key::F3), Zoom),
        (p(Key::F4), Trace),
        (p(Key::F5), Graph),
        (p(Key::F6), Second),
        (p(Key::F7), Alpha),
        (p(Key::F8), Mode),
        (p(Key::F9), Math),
        (p(Key::F10), Apps),
        (p(Key::F11), Vars),
        (p(Key::F12), Stat),
        (p(Key::Home), On),
        (p(Key::Insert), Sto),
        (s(Key::Num2), Square),
        (p(Key::Semicolon), XTThetaN),
    ]
};

impl Keymap {
    /// The default US-layout map: the fixed table plus one binding per letter
    /// `A`–`Z`, to the key carrying that letter as its alpha legend.
    pub fn default_map() -> Self {
        let mut bindings: Vec<(PcKey, KeyId)> = DEFAULT_FIXED.to_vec();
        for def in &KEYS {
            let Some(legend) = def.alpha else { continue };
            let mut chars = legend.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                continue;
            };
            if !c.is_ascii_uppercase() {
                continue;
            }
            if let Some(key) = Key::from_name(c.encode_utf8(&mut [0; 4])) {
                bindings.push((PcKey::plain(key), def.id));
            }
        }
        Self { bindings }
    }

    /// The calculator key bound to `k`. A shifted key with no binding of its own
    /// falls back to the unshifted binding.
    pub fn lookup(&self, k: PcKey) -> Option<KeyId> {
        self.exact(k)
            .or_else(|| k.shift.then(|| self.exact(PcKey::plain(k.key))).flatten())
    }

    fn exact(&self, k: PcKey) -> Option<KeyId> {
        self.bindings
            .iter()
            .find_map(|&(pk, id)| (pk == k).then_some(id))
    }

    /// Binds `k` to `id`, replacing any existing binding of `k`.
    pub fn bind(&mut self, k: PcKey, id: KeyId) {
        self.bindings.retain(|&(pk, _)| pk != k);
        self.bindings.push((k, id));
    }

    /// Removes every binding to `id`.
    pub fn unbind_key(&mut self, id: KeyId) {
        self.bindings.retain(|&(_, bound)| bound != id);
    }

    /// The PC keys bound to `id`, in binding order.
    pub fn keys_for(&self, id: KeyId) -> Vec<PcKey> {
        self.bindings
            .iter()
            .filter_map(|&(pk, bound)| (bound == id).then_some(pk))
            .collect()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::default_map()
    }
}

/// Human-readable label of a PC key, for example `Shift+8`.
pub fn pc_key_label(k: PcKey) -> String {
    if k.shift {
        format!("Shift+{}", k.key.name())
    } else {
        k.key.name().to_owned()
    }
}

/// What the PC keyboard did to the calculator keypad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Press(KeyId),
    Release(KeyId),
    /// Release every key the keyboard holds (the window lost focus).
    ReleaseAll,
}

/// Turns egui key events into [`KeyAction`]s, remembering which calculator key
/// each held PC key pressed so that its release matches whatever the shift
/// state is by then.
///
/// Held keys are tracked by the physical key when egui reports one. The
/// logical key of a release can differ from the press when shift changes in
/// between (Shift+= pressed as `Plus`, released as `Equals` after Shift goes
/// up; `1` pressed as `Num1`, released as `Exclamationmark` after Shift goes
/// down). The logical key is still what the keymap looks up.
#[derive(Default)]
pub struct KeyboardTracker {
    held: HashMap<Key, KeyId>,
}

impl KeyboardTracker {
    /// Processes one frame's events. While `typing` (a text field or a
    /// rebinding capture has focus) presses are dropped, but releases of keys
    /// pressed earlier are still honored.
    pub fn actions(&mut self, events: &[Event], keymap: &Keymap, typing: bool) -> Vec<KeyAction> {
        let mut out = Vec::new();
        for ev in events {
            match *ev {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    repeat,
                    modifiers,
                } => {
                    let hk = physical_key.unwrap_or(key);
                    if repeat
                        || typing
                        || modifiers.ctrl
                        || modifiers.alt
                        || modifiers.command
                        || modifiers.mac_cmd
                        || self.held.contains_key(&hk)
                    {
                        continue;
                    }
                    let pc = PcKey {
                        key,
                        shift: modifiers.shift,
                    };
                    if let Some(id) = keymap.lookup(pc) {
                        // Two PC keys bound to one calculator key (Backspace and
                        // Delete) press it once and release it when both are up.
                        let already = self.held.values().any(|&h| h == id);
                        self.held.insert(hk, id);
                        if !already {
                            out.push(KeyAction::Press(id));
                        }
                    }
                }
                Event::Key {
                    key,
                    physical_key,
                    pressed: false,
                    ..
                } => {
                    if let Some(id) = self.held.remove(&physical_key.unwrap_or(key))
                        && !self.held.values().any(|&h| h == id)
                    {
                        out.push(KeyAction::Release(id));
                    }
                }
                Event::WindowFocused(false) => {
                    self.held.clear();
                    out.push(KeyAction::ReleaseAll);
                }
                _ => {}
            }
        }
        out
    }
}
