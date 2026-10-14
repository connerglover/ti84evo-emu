//! A deterministic stand-in machine for tests, golden snapshots and demo mode.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use super::{BackendError, CpuView, DisasmLine, EmuBackend, Frame, RunStatus, UnmappedRow};
use crate::keypad::KeyId;

/// The program counter the fake always reports.
pub const FAKE_PC: u32 = 0x1000_0200;
/// Readable fake memory; the byte at `a` is `((a - start) % 251) as u8`.
pub const FAKE_MEM: std::ops::Range<u32> = 0x2000_0000..0x2001_0000;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
const BG: [u8; 3] = [0xF4, 0xF6, 0xF8];
const BLUE: [u8; 3] = [0x1F, 0x6F, 0xB5];
const KEY_UP: [u8; 3] = [0xD0, 0xD6, 0xDE];
const GREEN: [u8; 3] = [0x3E, 0x9B, 0x4F];
const UART_PERIOD: u64 = 100_000_000;

#[derive(Default)]
struct Shared {
    cycles: u64,
    held: BTreeSet<KeyId>,
    key_log: Vec<(KeyId, bool)>,
    resets: u32,
    persists: u32,
    run_calls: u32,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// A view of a [`FakeBackend`]'s state that tests keep after the backend is
/// boxed and handed to the app.
#[derive(Clone)]
pub struct FakeProbe(Arc<Mutex<Shared>>);

impl FakeProbe {
    /// Keys currently held, in `KeyId` order.
    pub fn held(&self) -> Vec<KeyId> {
        lock(&self.0).held.iter().copied().collect()
    }
    /// Every `set_key` call, in order.
    pub fn key_log(&self) -> Vec<(KeyId, bool)> {
        lock(&self.0).key_log.clone()
    }
    pub fn cycles(&self) -> u64 {
        lock(&self.0).cycles
    }
    pub fn resets(&self) -> u32 {
        lock(&self.0).resets
    }
    pub fn persists(&self) -> u32 {
        lock(&self.0).persists
    }
    pub fn run_calls(&self) -> u32 {
        lock(&self.0).run_calls
    }
}

enum Display {
    /// Rendered on demand from the fake's state.
    Rendered,
    /// The frame given to `with_frame`, unchanged.
    Fixed,
    Off,
}

#[derive(Serialize, Deserialize)]
struct SavedState {
    keys: Vec<KeyId>,
    cycles: u64,
}

/// A fake machine: cycles advance as asked, the screen shows the held keys and
/// a cycle bar, and the UART prints a line every 100 Mcycles.
pub struct FakeBackend {
    shared: Arc<Mutex<Shared>>,
    display: Display,
    frame: Frame,
    idle: bool,
    uart: Vec<u8>,
    breakpoints: Vec<u32>,
    state_tag: String,
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            display: Display::Rendered,
            frame: Frame {
                width: WIDTH,
                height: HEIGHT,
                rgba: Vec::new(),
            },
            idle: false,
            uart: Vec::new(),
            breakpoints: Vec::new(),
            state_tag: "demo".to_owned(),
        }
    }

    /// A fake whose display is off: `framebuffer` returns `None`.
    pub fn without_display() -> Self {
        Self {
            display: Display::Off,
            ..Self::new()
        }
    }

    /// A fake that always shows `f`, unchanged (it need not be valid).
    pub fn with_frame(f: Frame) -> Self {
        Self {
            display: Display::Fixed,
            frame: f,
            ..Self::new()
        }
    }

    /// A fake whose `run_for` returns `Idle` and never advances.
    pub fn idle() -> Self {
        Self {
            idle: true,
            ..Self::new()
        }
    }

    pub fn with_state_tag(mut self, tag: &str) -> Self {
        self.state_tag = tag.to_owned();
        self
    }

    pub fn probe(&self) -> FakeProbe {
        FakeProbe(Arc::clone(&self.shared))
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    fn render(&mut self) {
        let (cycles, held) = {
            let s = lock(&self.shared);
            (s.cycles, s.held.clone())
        };
        let frame = &mut self.frame;
        frame.width = WIDTH;
        frame.height = HEIGHT;
        frame.rgba.clear();
        frame.rgba.reserve((WIDTH * HEIGHT * 4) as usize);
        for _ in 0..WIDTH * HEIGHT {
            frame.rgba.extend_from_slice(&[BG[0], BG[1], BG[2], 0xFF]);
        }
        fill(frame, 0, 0, WIDTH, 16, BLUE);
        for (i, key) in (0u32..).zip(KeyId::ALL) {
            let color = if held.contains(&key) { BLUE } else { KEY_UP };
            fill(frame, 40 + (i % 5) * 48, 28 + (i / 5) * 20, 40, 14, color);
        }
        let bar = ((cycles / 1_000_000) % 321) as u32;
        fill(frame, 0, 232, bar, 6, GREEN);
    }
}

/// Fills a rectangle, clipped to the frame.
fn fill(frame: &mut Frame, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
    let x_end = x.saturating_add(w).min(frame.width);
    let y_end = y.saturating_add(h).min(frame.height);
    for py in y..y_end {
        for px in x..x_end {
            let i = ((py * frame.width + px) * 4) as usize;
            frame.rgba[i..i + 3].copy_from_slice(&rgb);
        }
    }
}

impl EmuBackend for FakeBackend {
    fn name(&self) -> &str {
        "demo"
    }

    fn state_tag(&self) -> String {
        self.state_tag.clone()
    }

    fn framebuffer(&mut self) -> Option<&Frame> {
        match self.display {
            Display::Off => None,
            Display::Fixed => Some(&self.frame),
            Display::Rendered => {
                self.render();
                Some(&self.frame)
            }
        }
    }

    fn backlight(&self) -> f32 {
        1.0
    }

    fn set_key(&mut self, key: KeyId, pressed: bool) {
        let mut s = self.shared();
        s.key_log.push((key, pressed));
        if pressed {
            s.held.insert(key);
        } else {
            s.held.remove(&key);
        }
    }

    fn run_for(&mut self, cycles: u64) -> RunStatus {
        let mut s = lock(&self.shared);
        s.run_calls = s.run_calls.saturating_add(1);
        if self.idle {
            return RunStatus::Idle;
        }
        if self.breakpoints.contains(&FAKE_PC) {
            return RunStatus::Breakpoint(FAKE_PC);
        }
        let before = s.cycles;
        let after = before.saturating_add(cycles);
        s.cycles = after;
        drop(s);
        for n in before / UART_PERIOD + 1..=after / UART_PERIOD {
            self.uart
                .extend_from_slice(format!("fake: {} Mcycles\r\n", n * 100).as_bytes());
        }
        RunStatus::Running
    }

    fn step(&mut self) -> RunStatus {
        let mut s = self.shared();
        s.cycles = s.cycles.saturating_add(1);
        RunStatus::Running
    }

    fn cycles(&self) -> u64 {
        self.shared().cycles
    }

    fn take_uart(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart)
    }

    fn save_state(&self) -> Result<Vec<u8>, BackendError> {
        let s = self.shared();
        let state = SavedState {
            keys: s.held.iter().copied().collect(),
            cycles: s.cycles,
        };
        serde_json::to_vec(&state).map_err(|e| BackendError::BadState(e.to_string()))
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        let state: SavedState =
            serde_json::from_slice(bytes).map_err(|e| BackendError::BadState(e.to_string()))?;
        let mut s = self.shared();
        s.cycles = state.cycles;
        s.held = state.keys.into_iter().collect();
        Ok(())
    }

    fn reset(&mut self) {
        let mut s = lock(&self.shared);
        s.cycles = 0;
        s.held.clear();
        s.resets = s.resets.saturating_add(1);
        drop(s);
        self.uart.clear();
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        let mut s = self.shared();
        s.persists = s.persists.saturating_add(1);
        Ok(())
    }

    fn cpu(&self) -> CpuView {
        let mut core = [0; 16];
        core[0] = self.cycles() as u32;
        core[13] = 0x2000_8000;
        core[15] = FAKE_PC;
        CpuView {
            core,
            xpsr: 0x0100_0000,
            secure: true,
            handler: false,
            banked: vec![("msp_s", 0x3000_0800), ("msp_ns", 0x2009_3000)],
        }
    }

    fn peek(&self, addr: u32, len: usize) -> Vec<Option<u8>> {
        (0..len)
            .map(|i| {
                u32::try_from(i)
                    .ok()
                    .and_then(|i| addr.checked_add(i))
                    .filter(|a| FAKE_MEM.contains(a))
                    .map(|a| ((a - FAKE_MEM.start) % 251) as u8)
            })
            .collect()
    }

    fn disasm(&self, addr: u32, count: usize) -> Vec<DisasmLine> {
        (0..count)
            .map(|i| DisasmLine {
                addr: addr.wrapping_add((i as u32).wrapping_mul(2)),
                len: 2,
                raw: 0xBF00,
                text: "nop".to_owned(),
            })
            .collect()
    }

    fn unmapped(&self) -> Vec<UnmappedRow> {
        vec![UnmappedRow {
            addr: 0x5020_8014,
            width: 4,
            kind: "read",
            value: None,
            pc: FAKE_PC,
            secure: true,
            count: 1,
        }]
    }

    fn breakpoints(&self) -> Vec<u32> {
        self.breakpoints.clone()
    }

    fn toggle_breakpoint(&mut self, addr: u32) {
        if let Some(i) = self.breakpoints.iter().position(|&a| a == addr) {
            self.breakpoints.remove(i);
        } else {
            self.breakpoints.push(addr);
        }
    }
}
