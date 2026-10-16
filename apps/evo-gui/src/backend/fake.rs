//! A deterministic stand-in machine for tests, golden snapshots and demo mode.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use super::{
    AccessDir, BackendError, BusRecord, CpuView, DeviceView, DisasmLine, EmuBackend, Frame, Goal,
    HardwareView, HwChange, KeyEvent, MemRegion, RegRow, RunStatus, UartId, UnmappedRow,
    Watchpoint,
};
use crate::keypad::KeyId;

/// The program counter the fake always reports.
pub const FAKE_PC: u32 = 0x1000_0200;
/// Readable fake memory; the byte at `a` is `((a - start) % 251) as u8` unless poked.
pub const FAKE_MEM: std::ops::Range<u32> = 0x2000_0000..0x2001_0000;
/// The address the fake's guest writes `0x2A` to on every run: write watchpoints and the
/// bus monitor see it.
pub const FAKE_WATCH_ADDR: u32 = 0x2000_0100;
/// The most bus records the fake keeps.
pub const FAKE_BUS_CAP: usize = 4096;

/// The magic that starts a fake RAM image.
const RAM_MAGIC: &[u8] = b"FAKERAM1";
/// The whole content of a fake flash file.
const FLASH_IMAGE: &[u8] = b"FAKEFLASH1";
/// The most cycles an armed goal runs before it is reached.
const GOAL_CYCLES: u64 = 1000;
/// What the fake's guest writes to [`FAKE_WATCH_ADDR`].
const WATCH_VALUE: u32 = 0x2A;
/// The banked registers the fake lets the debugger write.
const BANKED: [&str; 2] = ["msp_s", "msp_ns"];
const BANKED_RESET: [u32; 2] = [0x3000_0800, 0x2009_3000];
/// Seconds in a day: the RTC counts below this.
const DAY_SECONDS: u32 = 86_400;

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
    /// `provision` calls started.
    provisions: u32,
    /// Key events not yet taken by `take_key_events`.
    key_queue: Vec<KeyEvent>,
    /// Every key event; never drained.
    key_events: Vec<KeyEvent>,
    pokes: Vec<(u32, Vec<u8>)>,
    goals: Vec<Goal>,
    goal_cancels: u32,
    /// The armed goal's target.
    goal: Option<u32>,
    hw_changes: Vec<HwChange>,
    uart_sent: Vec<(UartId, Vec<u8>)>,
    flash_imports: u32,
    /// `peripherals` and `bus_records` calls.
    inspect_calls: u32,
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
    /// How many times `provision` was called.
    pub fn provisions(&self) -> u32 {
        lock(&self.0).provisions
    }
    /// Every successful `poke`, in order.
    pub fn pokes(&self) -> Vec<(u32, Vec<u8>)> {
        lock(&self.0).pokes.clone()
    }
    /// Every `begin_goal`, in order.
    pub fn goals(&self) -> Vec<Goal> {
        lock(&self.0).goals.clone()
    }
    /// Every applied `set_hardware` change, in order.
    pub fn hw_changes(&self) -> Vec<HwChange> {
        lock(&self.0).hw_changes.clone()
    }
    /// Every `send_uart`, in order.
    pub fn uart_sent(&self) -> Vec<(UartId, Vec<u8>)> {
        lock(&self.0).uart_sent.clone()
    }
    /// Successful `import_flash` calls.
    pub fn flash_imports(&self) -> u32 {
        lock(&self.0).flash_imports
    }
    /// `peripherals` plus `bus_records` calls.
    pub fn inspect_calls(&self) -> u32 {
        lock(&self.0).inspect_calls
    }
    /// Every `set_key` with its cycle; never drained (unlike `take_key_events`).
    pub fn key_events(&self) -> Vec<KeyEvent> {
        lock(&self.0).key_events.clone()
    }
    /// `cancel_goal` calls.
    pub fn goal_cancels(&self) -> u32 {
        lock(&self.0).goal_cancels
    }
    /// True while a goal is armed.
    pub fn goal_armed(&self) -> bool {
        lock(&self.0).goal.is_some()
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
    /// What `name` reports.
    name: String,
    /// What `needs_provision` reports; `provision` clears it on success.
    fresh: bool,
    provision_fails: bool,
    /// When set, `provision` waits for a message (or the sender's drop) before it ends.
    gate: Option<Receiver<()>>,
    /// Poked bytes, read by `peek` before the pattern.
    overlay: BTreeMap<u32, u8>,
    /// `set_core_reg` overrides: r0..r15, then xPSR.
    core_regs: [Option<u32>; 17],
    /// `set_banked_reg` overrides, as [`BANKED`].
    banked_regs: [Option<u32>; 2],
    /// Armed goals never finish (`goal_never_reached`).
    goal_never: bool,
    watchpoints: Vec<Watchpoint>,
    /// The next `run_for` stops on the write watchpoint.
    watch_hit: bool,
    monitor: Option<(u32, u32)>,
    bus: VecDeque<BusRecord>,
    uart2: Vec<u8>,
    hardware: HardwareView,
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
            name: "demo".to_owned(),
            fresh: false,
            provision_fails: false,
            gate: None,
            overlay: BTreeMap::new(),
            core_regs: [None; 17],
            banked_regs: [None; 2],
            goal_never: false,
            watchpoints: Vec::new(),
            watch_hit: false,
            monitor: None,
            bus: VecDeque::new(),
            uart2: Vec::new(),
            hardware: HardwareView {
                backlight: 1.0,
                charger_line: true,
                battery_ok: true,
                usb_attached: false,
                power_hold: true,
                deep_sleep: false,
                rtc_seconds: 0,
            },
        }
    }

    /// A fake whose armed goals are never reached: `run_for` runs as if unarmed, and the
    /// goal stays armed until `cancel_goal`, `reset` or `load_state`.
    pub fn goal_never_reached(mut self) -> Self {
        self.goal_never = true;
        self
    }

    /// Drops the debugger's overrides (poked bytes, register writes, an armed goal, a
    /// pending watchpoint hit), as a reset or a state load does.
    fn clear_overrides(&mut self) {
        self.overlay.clear();
        self.core_regs = [None; 17];
        self.banked_regs = [None; 2];
        self.watch_hit = false;
        self.shared().goal = None;
    }

    /// Adds `cycles`, prints the UART lines due, and records the guest's write for the bus
    /// monitor.
    fn advance(&mut self, cycles: u64) {
        let mut s = self.shared();
        let before = s.cycles;
        let after = before.saturating_add(cycles);
        s.cycles = after;
        drop(s);
        for n in before / UART_PERIOD + 1..=after / UART_PERIOD {
            self.uart
                .extend_from_slice(format!("fake: {} Mcycles\r\n", n * 100).as_bytes());
        }
        let watched = self
            .monitor
            .is_some_and(|(start, len)| FAKE_WATCH_ADDR.wrapping_sub(start) < len);
        if after > before && watched {
            if self.bus.len() >= FAKE_BUS_CAP {
                self.bus.pop_front();
            }
            self.bus.push_back(BusRecord {
                addr: FAKE_WATCH_ADDR,
                width: 4,
                dir: AccessDir::Write,
                value: WATCH_VALUE,
                pc: FAKE_PC,
                cycle: before,
            });
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

    /// A fake that reports `name` instead of "demo" (the header shows it).
    pub fn named(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    /// A fake that reports `needs_provision` until it is provisioned.
    pub fn needing_provision(mut self) -> Self {
        self.fresh = true;
        self
    }

    /// A fake whose `provision` fails.
    pub fn failing_provision(mut self) -> Self {
        self.provision_fails = true;
        self
    }

    /// Makes `provision` wait until the returned sender sends (or is dropped), so a test
    /// can look at the app while provisioning runs.
    pub fn provision_gate(&mut self) -> Sender<()> {
        let (tx, rx) = channel();
        self.gate = Some(rx);
        tx
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
        &self.name
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
        let event = KeyEvent {
            key,
            pressed,
            cycle: s.cycles,
        };
        s.key_queue.push(event);
        s.key_events.push(event);
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
        if self.watch_hit {
            self.watch_hit = false;
            return RunStatus::Watchpoint {
                addr: FAKE_WATCH_ADDR,
                dir: AccessDir::Write,
                value: WATCH_VALUE,
                pc: FAKE_PC,
            };
        }
        let goal = if self.goal_never { None } else { s.goal.take() };
        drop(s);
        match goal {
            Some(pc) => {
                self.advance(cycles.min(GOAL_CYCLES));
                RunStatus::Goal { pc }
            }
            None => {
                self.advance(cycles);
                RunStatus::Running
            }
        }
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
        drop(s);
        self.clear_overrides();
        Ok(())
    }

    fn reset(&mut self) {
        let mut s = lock(&self.shared);
        s.cycles = 0;
        s.held.clear();
        s.resets = s.resets.saturating_add(1);
        drop(s);
        self.uart.clear();
        self.uart2.clear();
        self.clear_overrides();
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        let mut s = self.shared();
        s.persists = s.persists.saturating_add(1);
        Ok(())
    }

    fn needs_provision(&self) -> bool {
        self.fresh
    }

    fn provision(&mut self) -> Result<(), BackendError> {
        {
            let mut s = self.shared();
            s.provisions = s.provisions.saturating_add(1);
        }
        if let Some(gate) = &self.gate {
            // A dropped sender ends the wait too.
            let _ = gate.recv();
        }
        if self.provision_fails {
            return Err(BackendError::Provision("the fake always fails".to_owned()));
        }
        self.fresh = false;
        // Like the real machine, provisioning ends in a fresh boot.
        let mut s = self.shared();
        s.cycles = 0;
        s.held.clear();
        Ok(())
    }

    fn cpu(&self) -> CpuView {
        let mut core = [0; 16];
        core[0] = self.cycles() as u32;
        core[13] = 0x2000_8000;
        core[15] = FAKE_PC;
        for (reg, over) in core.iter_mut().zip(self.core_regs) {
            *reg = over.unwrap_or(*reg);
        }
        let banked = BANKED
            .iter()
            .zip(BANKED_RESET)
            .zip(self.banked_regs)
            .map(|((&name, reset), over)| (name, over.unwrap_or(reset)))
            .collect();
        CpuView {
            core,
            xpsr: self.core_regs[16].unwrap_or(0x0100_0000),
            secure: true,
            handler: false,
            banked,
        }
    }

    fn peek(&self, addr: u32, len: usize) -> Vec<Option<u8>> {
        (0..len)
            .map(|i| {
                u32::try_from(i)
                    .ok()
                    .and_then(|i| addr.checked_add(i))
                    .filter(|a| FAKE_MEM.contains(a))
                    .map(|a| {
                        self.overlay
                            .get(&a)
                            .copied()
                            .unwrap_or(((a - FAKE_MEM.start) % 251) as u8)
                    })
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

    fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), BackendError> {
        // FAKE_MEM ends well below u32::MAX, so a run of addresses leaves it (and fails)
        // before `addr + i` could wrap.
        let mut a = addr;
        for _ in bytes {
            if !FAKE_MEM.contains(&a) {
                return Err(BackendError::Unwritable(a));
            }
            a = a.wrapping_add(1);
        }
        for (a, &b) in (addr..).zip(bytes) {
            self.overlay.insert(a, b);
        }
        self.shared().pokes.push((addr, bytes.to_vec()));
        Ok(())
    }

    fn memory_regions(&self) -> Vec<MemRegion> {
        vec![MemRegion {
            name: "fake RAM".to_owned(),
            start: FAKE_MEM.start,
            len: FAKE_MEM.end - FAKE_MEM.start,
        }]
    }

    fn set_core_reg(&mut self, index: usize, value: u32) -> Result<(), BackendError> {
        let slot = self
            .core_regs
            .get_mut(index)
            .ok_or_else(|| BackendError::BadRegister(format!("#{index}")))?;
        *slot = Some(value);
        Ok(())
    }

    fn set_banked_reg(&mut self, name: &str, value: u32) -> Result<(), BackendError> {
        let i = BANKED
            .iter()
            .position(|&n| n == name)
            .ok_or_else(|| BackendError::BadRegister(name.to_owned()))?;
        self.banked_regs[i] = Some(value);
        Ok(())
    }

    fn set_breakpoints(&mut self, mut addrs: Vec<u32>) {
        addrs.sort_unstable();
        addrs.dedup();
        self.breakpoints = addrs;
    }

    fn begin_goal(&mut self, goal: Goal) -> Result<Option<RunStatus>, BackendError> {
        self.shared().goals.push(goal);
        let target = match goal {
            Goal::StepOver => {
                self.shared().goal = None;
                return Ok(Some(self.step()));
            }
            Goal::StepOut => FAKE_PC + 0x40,
            Goal::RunTo(a) => a & !1,
        };
        self.shared().goal = Some(target);
        Ok(None)
    }

    fn cancel_goal(&mut self) {
        let mut s = self.shared();
        s.goal = None;
        s.goal_cancels = s.goal_cancels.saturating_add(1);
    }

    fn watchpoints(&self) -> Vec<Watchpoint> {
        self.watchpoints.clone()
    }

    fn set_watchpoints(&mut self, w: Vec<Watchpoint>) -> Result<(), BackendError> {
        if !w.iter().all(Watchpoint::is_valid) {
            return Err(BackendError::Invalid("invalid watchpoint".to_owned()));
        }
        self.watch_hit = w
            .iter()
            .any(|w| w.kinds.write && (w.start..=w.last()).contains(&FAKE_WATCH_ADDR));
        self.watchpoints = w;
        Ok(())
    }

    fn set_bus_monitor(&mut self, range: Option<(u32, u32)>) {
        self.monitor = range;
    }

    fn bus_records(&self) -> Vec<BusRecord> {
        let mut s = self.shared();
        s.inspect_calls = s.inspect_calls.saturating_add(1);
        self.bus.iter().copied().collect()
    }

    fn clear_bus_records(&mut self) {
        self.bus.clear();
    }

    fn peripherals(&self) -> Vec<DeviceView> {
        let mut s = self.shared();
        s.inspect_calls = s.inspect_calls.saturating_add(1);
        drop(s);
        let row = |name: &str, offset, value, note: Option<&str>| RegRow {
            name: name.to_owned(),
            offset,
            value,
            note: note.map(str::to_owned),
        };
        vec![
            DeviceView {
                name: "timer".to_owned(),
                base: 0x4010_1000,
                regs: vec![
                    row("ctrl", 0x30, 0x1, None),
                    row("int_stat", 0x34, 0x0, None),
                ],
            },
            DeviceView {
                name: "nvic".to_owned(),
                base: 0xE000_E100,
                regs: vec![row("irq13", 13, 0x1, Some("enabled"))],
            },
        ]
    }

    fn take_uart2(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart2)
    }

    fn send_uart(&mut self, port: UartId, bytes: &[u8]) {
        self.shared().uart_sent.push((port, bytes.to_vec()));
        match port {
            UartId::Uart0 => self.uart.extend_from_slice(bytes),
            UartId::Uart2 => self.uart2.extend_from_slice(bytes),
        }
    }

    fn take_key_events(&mut self) -> Vec<KeyEvent> {
        std::mem::take(&mut self.shared().key_queue)
    }

    fn hardware(&self) -> HardwareView {
        self.hardware
    }

    fn set_hardware(&mut self, change: HwChange) -> Result<(), BackendError> {
        let hw = &mut self.hardware;
        match change {
            HwChange::ChargerLine(on) => hw.charger_line = on,
            HwChange::BatteryOk(ok) => hw.battery_ok = ok,
            HwChange::UsbAttached(on) => hw.usb_attached = on,
            HwChange::RtcSeconds(s) if s >= DAY_SECONDS => {
                return Err(BackendError::Invalid(
                    "RTC time must be below 24 h".to_owned(),
                ));
            }
            HwChange::RtcSeconds(s) => hw.rtc_seconds = s,
        }
        self.shared().hw_changes.push(change);
        Ok(())
    }

    fn export_ram(&self) -> Result<Vec<u8>, BackendError> {
        let bytes: Vec<(u32, u8)> = self.overlay.iter().map(|(&a, &b)| (a, b)).collect();
        let mut out = RAM_MAGIC.to_vec();
        serde_json::to_writer(&mut out, &bytes)
            .map_err(|e| BackendError::BadState(e.to_string()))?;
        Ok(out)
    }

    fn import_ram(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        let bad = || BackendError::BadState("not a RAM image".to_owned());
        let json = bytes.strip_prefix(RAM_MAGIC).ok_or_else(bad)?;
        let pairs: Vec<(u32, u8)> = serde_json::from_slice(json).map_err(|_| bad())?;
        if !pairs.iter().all(|(a, _)| FAKE_MEM.contains(a)) {
            return Err(bad());
        }
        self.overlay = pairs.into_iter().collect();
        Ok(())
    }

    fn export_flash(&self, path: &Path) -> Result<(), BackendError> {
        std::fs::write(path, FLASH_IMAGE).map_err(|e| BackendError::Io(e.to_string()))
    }

    fn import_flash(&mut self, path: &Path) -> Result<(), BackendError> {
        let io = |e: std::io::Error| BackendError::Io(e.to_string());
        let meta = std::fs::metadata(path).map_err(io)?;
        if !meta.is_file() {
            return Err(BackendError::Io(format!(
                "{} is not a file",
                path.display()
            )));
        }
        // Checked before reading, so an oversized file is never read.
        if meta.len() != FLASH_IMAGE.len() as u64 {
            return Err(BackendError::BadState("not a flash image".to_owned()));
        }
        if std::fs::read(path).map_err(io)? != FLASH_IMAGE {
            return Err(BackendError::BadState("not a flash image".to_owned()));
        }
        let mut s = self.shared();
        s.cycles = 0;
        s.flash_imports = s.flash_imports.saturating_add(1);
        Ok(())
    }
}
