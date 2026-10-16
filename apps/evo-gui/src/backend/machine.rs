//! [`MachineBackend`]: the emulated TI-84 Evo behind [`EmuBackend`].
//!
//! This is the only GUI file that names the core crates (`evo_core`, `evo_hw`, `evo_image`,
//! `armv8m`); `tests/boundary.rs` enforces that. Besides Plan 1's `Machine` API (`run`,
//! `step`, `breakpoints`, `bus.peek`, `bus.log`, `cpu.regs`, `sched.now`) it uses Plan 2's
//! host API: `screen`, the backlight `Pwm`, `set_key`, `take_uart_tx`, `save_state` /
//! `load_state`, and the flash file (`from_bundle_nv`, `nv_state`, `NvState`). Plan 4's
//! debugger API: watchpoints, goals (`arm_*`, `disarm`), `write_reg`, `peripherals`, and the
//! bus monitor and `poke` on `bus`.

use std::path::{Path, PathBuf};

use armv8m::decode::is_32bit;
use armv8m::{Sec, decode16, decode32, disasm};
use evo_core::machine::{FLASH_NS, FLASH_S, FLASH_SIZE, SRAM_NS, SRAM_S, SRAM_SIZE};
use evo_core::{AccessDir as CoreDir, BusRecord as CoreRecord};
use evo_core::{
    Key, Machine, NvError, NvState, StopReason, UartPort, WatchKinds as CoreKinds, WatchRange,
};
use evo_hw::dev::lcdc::Pwm;
use evo_image::{Bundle, ImageError, Layout};

use crate::backend::{
    AccessDir, BackendError, BusRecord, CpuView, DeviceView, DisasmLine, EmuBackend, Frame, Goal,
    HardwareView, HwChange, KeyEvent, MemRegion, RegRow, RunStatus, UartId, UnmappedRow,
    WatchKinds, Watchpoint,
};
use crate::keypad::KeyId;

/// Why a bundle could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot read bundle: {0}")]
    Read(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] ImageError),
    /// The flash file exists but cannot be read; it is left untouched.
    #[error("{path}: {source}")]
    Flash { path: PathBuf, source: NvError },
}

/// What a bundle's metadata says about the OS it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleInfo {
    /// e.g. `7.1.0.4421`.
    pub build_version: String,
    pub target_device: String,
    pub prodid: u32,
}

/// Parses the bundle in `bytes` and returns its metadata, without booting it.
pub fn inspect_bundle(bytes: &[u8]) -> Result<BundleInfo, OpenError> {
    let meta = Bundle::from_bytes(bytes)?.metadata;
    Ok(BundleInfo {
        build_version: meta.build_version,
        target_device: meta.target_device,
        prodid: meta.prodid,
    })
}

const _: () = assert!(crate::backend::CLOCK_HZ == evo_core::CPU_HZ);
const _: () =
    assert!(crate::input::MIN_HOLD_CYCLES == evo_core::keys::TAP_HOLD_MS * evo_core::CPU_HZ / 1000);

/// Converts RGB888 pixels to RGBA8: clears `dst`, then appends `(r, g, b, 255)` per pixel.
pub fn rgb888_to_rgba(src: &[u8], dst: &mut Vec<u8>) {
    dst.clear();
    dst.reserve(src.len() / 3 * 4);
    for &[r, g, b] in src.as_chunks::<3>().0 {
        dst.extend_from_slice(&[r, g, b, 255]);
    }
}

/// The core key a GUI key presses: the key named [`KeyId::core_name`] in the core's keypad
/// table (`evo_hw::dev::keypad::KEY_NAMES`, the TI-84 Evo matrix of `docs/hardware/keypad.md`
/// "Key map"), so the GUI keeps no matrix of its own. Every `KeyId` names a core key
/// (`every_gui_key_is_the_core_key_of_its_name`).
pub fn core_key(id: KeyId) -> Key {
    Key::from_name(id.core_name())
        .unwrap_or_else(|| unreachable!("{id:?}: no core key {}", id.core_name()))
}

/// 64-bit FNV-1a of `bytes`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes
        .iter()
        .fold(OFFSET, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME))
}

/// The emulated calculator, booted from an OS bundle.
pub struct MachineBackend {
    machine: Machine,
    /// Kept so [`EmuBackend::reset`] can rebuild the machine.
    bundle: Bundle,
    name: String,
    /// `machine:{fnv1a64 of the bundle bytes}`.
    tag: String,
    /// `{nv_dir}/{fnv1a64 of the bundle bytes:016x}.nv`.
    nv_path: PathBuf,
    /// True from an open with no flash file until provisioning succeeds.
    fresh: bool,
    /// [`Machine::nv_fingerprint`] of the flash the file at `nv_path` holds (read at open or
    /// last written); `None` when there is no such file yet. `persist` skips the write while
    /// it matches.
    on_disk: Option<[u8; 32]>,
    /// The last LCD image, converted for the GUI.
    frame: Frame,
    /// The bus-monitor range as last set (the core has no getter), so `reset` can keep it.
    monitor: Option<(u32, u32)>,
}

impl MachineBackend {
    /// Reads and boots the bundle at `path`; the backend is named after its file name. Its
    /// flash file lives in `nv_dir` (see [`from_bytes`](Self::from_bytes)).
    pub fn open(path: &Path, nv_dir: &Path) -> Result<Self, OpenError> {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::from_bytes(&bytes, name, nv_dir)
    }

    /// Boots the bundle in `bytes`; `name` is for display only. If the bundle's flash file
    /// [`nv_path`](Self::nv_path) exists, the machine starts from it; a flash file that cannot
    /// be read is an [`OpenError::Flash`] and is left untouched. Otherwise flash starts erased
    /// and the backend [`needs_provision`](EmuBackend::needs_provision).
    pub fn from_bytes(bytes: &[u8], name: String, nv_dir: &Path) -> Result<Self, OpenError> {
        let bundle = Bundle::from_bytes(bytes)?;
        let hash = fnv1a64(bytes);
        let nv_path = nv_dir.join(format!("{hash:016x}.nv"));
        let fresh = !nv_path.exists();
        let nv = if !fresh {
            match NvState::read_file(&nv_path) {
                Ok(nv) => Some(nv),
                Err(source) => {
                    return Err(OpenError::Flash {
                        path: nv_path,
                        source,
                    });
                }
            }
        } else {
            None
        };
        let machine = Machine::from_bundle_nv(&bundle, &Layout::EVO_7_1, nv)?;
        let on_disk = (!fresh).then(|| machine.nv_fingerprint());
        Ok(Self {
            machine,
            bundle,
            name,
            tag: format!("machine:{hash:016x}"),
            nv_path,
            fresh,
            on_disk,
            frame: Frame {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            },
            monitor: None,
        })
    }

    /// Where [`EmuBackend::persist`] writes the flash file.
    pub fn nv_path(&self) -> &Path {
        &self.nv_path
    }

    /// The instruction at `addr`, read through `bus.peek` as `Machine::disasm_at` does.
    fn disasm_one(&self, addr: u32) -> Option<DisasmLine> {
        let bus = &self.machine.bus;
        let hw1 = bus.peek(addr, 2)? as u16;
        let (instr, len, raw) = if is_32bit(hw1) {
            let hw2 = bus.peek(addr.wrapping_add(2), 2)? as u16;
            (decode32(hw1, hw2), 4, u32::from(hw1) << 16 | u32::from(hw2))
        } else {
            (decode16(hw1), 2, u32::from(hw1))
        };
        Some(DisasmLine {
            addr,
            len,
            raw,
            text: disasm(&instr, addr, len),
        })
    }
}

fn access_dir(dir: CoreDir) -> AccessDir {
    match dir {
        CoreDir::Read => AccessDir::Read,
        CoreDir::Write => AccessDir::Write,
        CoreDir::Exec => AccessDir::Exec,
    }
}

fn run_status(stop: StopReason) -> RunStatus {
    match stop {
        StopReason::CycleLimit | StopReason::Halted | StopReason::Stepped => RunStatus::Running,
        StopReason::Idle => RunStatus::Idle,
        StopReason::Breakpoint(a) => RunStatus::Breakpoint(a),
        StopReason::Lockup { pc } => RunStatus::Lockup { pc },
        StopReason::Bkpt { pc, imm } => RunStatus::Bkpt { pc, imm },
        StopReason::Watchpoint {
            addr,
            dir,
            value,
            pc,
        } => RunStatus::Watchpoint {
            addr,
            dir: access_dir(dir),
            value,
            pc,
        },
        StopReason::Goal { pc } => RunStatus::Goal { pc },
    }
}

fn watch_range(w: Watchpoint) -> WatchRange {
    WatchRange {
        start: w.start,
        len: w.len,
        kinds: CoreKinds {
            read: w.kinds.read,
            write: w.kinds.write,
            exec: w.kinds.exec,
        },
    }
}

fn watchpoint(r: WatchRange) -> Watchpoint {
    Watchpoint {
        start: r.start,
        len: r.len,
        kinds: WatchKinds {
            read: r.kinds.read,
            write: r.kinds.write,
            exec: r.kinds.exec,
        },
    }
}

fn bus_record(r: CoreRecord) -> BusRecord {
    BusRecord {
        addr: r.addr,
        width: r.width,
        dir: access_dir(r.dir),
        value: r.value,
        pc: r.pc,
        cycle: r.cycle,
    }
}

/// The core's name for [`EmuBackend::set_core_reg`] index `i` (`None` past xPSR).
fn core_reg_name(i: usize) -> Option<String> {
    Some(match i {
        0..=12 => format!("r{i}"),
        13 => "sp".into(),
        14 => "lr".into(),
        15 => "pc".into(),
        16 => "xpsr".into(),
        _ => return None,
    })
}

impl EmuBackend for MachineBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn state_tag(&self) -> String {
        self.tag.clone()
    }

    fn framebuffer(&mut self) -> Option<&Frame> {
        let f = self.machine.screen()?;
        rgb888_to_rgba(&f.rgb, &mut self.frame.rgba);
        self.frame.width = f.width;
        self.frame.height = f.height;
        self.frame.is_valid().then_some(&self.frame)
    }

    fn backlight(&self) -> f32 {
        self.machine
            .bus
            .device::<Pwm>("pwm")
            .map_or(1.0, |p| p.brightness())
    }

    fn set_key(&mut self, key: KeyId, pressed: bool) {
        self.machine.set_key(core_key(key), pressed);
    }

    fn run_for(&mut self, cycles: u64) -> RunStatus {
        run_status(self.machine.run(cycles))
    }

    fn step(&mut self) -> RunStatus {
        run_status(self.machine.step())
    }

    fn cycles(&self) -> u64 {
        self.machine.sched.now
    }

    fn take_uart(&mut self) -> Vec<u8> {
        // UART0 is the TF-M / BSP console. UART2 is a link transport the GUI does not show;
        // its TX buffer drops its oldest bytes by itself.
        self.machine.take_uart_tx(UartPort::Uart0)
    }

    fn save_state(&self) -> Result<Vec<u8>, BackendError> {
        Ok(self.machine.save_state())
    }

    fn load_state(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        self.machine
            .load_state(bytes)
            .map_err(|e| BackendError::BadState(e.to_string()))
    }

    fn reset(&mut self) {
        // Flash is kept: the new machine starts from the current non-volatile state.
        let nv = Some(self.machine.nv_state());
        match Machine::from_bundle_nv(&self.bundle, &Layout::EVO_7_1, nv) {
            Ok(mut m) => {
                // Debugger settings carry over; the goal (never set on `m`) is dropped.
                m.breakpoints = std::mem::take(&mut self.machine.breakpoints);
                if let Err(e) = m.set_watchpoints(self.machine.watchpoints()) {
                    // They were validated when set, so this does not happen.
                    log::error!("reset dropped the watchpoints: {e}");
                }
                m.bus.set_bus_monitor(self.monitor);
                self.machine = m;
            }
            // The same bundle booted once already, so this does not happen.
            Err(e) => log::error!("reset failed, machine kept: {e}"),
        }
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        // Until provisioning succeeds the flash file must not exist: it is what tells the
        // next launch that the unit is provisioned.
        if self.fresh {
            return Ok(());
        }
        // The periodic save runs on the UI thread: hashing the flash costs far less than
        // copying and writing 16 MiB with a sync, and the guest rarely writes flash.
        let now = self.machine.nv_fingerprint();
        if self.on_disk == Some(now) && self.nv_path.exists() {
            return Ok(());
        }
        if let Some(dir) = self.nv_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| BackendError::Persist(e.to_string()))?;
        }
        self.machine
            .nv_state()
            .write_file(&self.nv_path)
            .map_err(|e| BackendError::Persist(e.to_string()))?;
        self.on_disk = Some(now);
        Ok(())
    }

    fn needs_provision(&self) -> bool {
        self.fresh
    }

    fn provision(&mut self) -> Result<(), BackendError> {
        // `evo_core::provision` uses breakpoints of its own and clears them, and fails on any
        // other stop: the debugger's breakpoints, watchpoints, bus monitor and goal are put
        // aside while it runs (the goal is dropped, as by `reset`).
        let breakpoints = std::mem::take(&mut self.machine.breakpoints);
        let watchpoints = self.machine.watchpoints();
        self.machine.disarm();
        let cleared = self.machine.set_watchpoints(Vec::new());
        debug_assert!(cleared.is_ok(), "an empty set is valid");
        self.machine.bus.set_bus_monitor(None);
        let result = evo_core::provision(&mut self.machine);
        self.machine.breakpoints = breakpoints;
        if let Err(e) = self.machine.set_watchpoints(watchpoints) {
            // They were validated when set, so this does not happen.
            log::error!("provisioning dropped the watchpoints: {e}");
        }
        self.machine.bus.set_bus_monitor(self.monitor);
        match result {
            Ok(report) => {
                log::info!("provisioned: {report:?}");
                self.fresh = false;
                Ok(())
            }
            Err(e) => Err(BackendError::Provision(e.to_string())),
        }
    }

    fn cpu(&self) -> CpuView {
        let r = &self.machine.cpu.regs;
        let mut core = [0; 16];
        core[..13].copy_from_slice(&r.r);
        core[13] = r.sp();
        core[14] = r.lr;
        core[15] = r.pc;
        CpuView {
            core,
            xpsr: r.xpsr.to_u32(),
            secure: r.sec == Sec::Secure,
            handler: r.handler_mode(),
            banked: vec![
                ("msp_s", r.msp.s),
                ("msp_ns", r.msp.ns),
                ("psp_s", r.psp.s),
                ("psp_ns", r.psp.ns),
                ("msplim_s", r.msplim.s),
                ("msplim_ns", r.msplim.ns),
                ("psplim_s", r.psplim.s),
                ("psplim_ns", r.psplim.ns),
                ("control_s", r.control.s),
                ("control_ns", r.control.ns),
                ("primask_s", u32::from(r.primask.s)),
                ("primask_ns", u32::from(r.primask.ns)),
                ("basepri_s", u32::from(r.basepri.s)),
                ("basepri_ns", u32::from(r.basepri.ns)),
                ("faultmask_s", u32::from(r.faultmask.s)),
                ("faultmask_ns", u32::from(r.faultmask.ns)),
            ],
        }
    }

    fn peek(&self, addr: u32, len: usize) -> Vec<Option<u8>> {
        (0..len)
            .map(|i| {
                let a = u32::try_from(i).ok().and_then(|i| addr.checked_add(i))?;
                self.machine.bus.peek(a, 1).map(|v| v as u8)
            })
            .collect()
    }

    fn disasm(&self, addr: u32, count: usize) -> Vec<DisasmLine> {
        let mut lines = Vec::with_capacity(count.min(1024));
        let mut at = addr;
        for _ in 0..count {
            let Some(line) = self.disasm_one(at) else {
                break;
            };
            at = at.wrapping_add(u32::from(line.len));
            lines.push(line);
        }
        lines
    }

    fn unmapped(&self) -> Vec<UnmappedRow> {
        self.machine
            .bus
            .log
            .entries()
            .map(|(a, count)| UnmappedRow {
                addr: a.addr,
                width: a.width,
                kind: a.kind.as_str(),
                value: a.write,
                pc: a.pc,
                secure: a.secure,
                count,
            })
            .collect()
    }

    fn breakpoints(&self) -> Vec<u32> {
        self.machine.breakpoints.iter().copied().collect()
    }

    fn toggle_breakpoint(&mut self, addr: u32) {
        let bps = &mut self.machine.breakpoints;
        if !bps.remove(&addr) {
            bps.insert(addr);
        }
    }

    fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), BackendError> {
        // Every byte is checked before any is written, so a refused write changes nothing.
        let at = |i: usize| u32::try_from(i).ok().and_then(|i| addr.checked_add(i));
        for i in 0..bytes.len() {
            match at(i) {
                Some(a) if self.machine.bus.peek(a, 1).is_some() => {}
                // Past 0xffffffff: the first address that does not exist wraps to 0.
                bad => return Err(BackendError::Unwritable(bad.unwrap_or(0))),
            }
        }
        for (i, &b) in bytes.iter().enumerate() {
            let a = at(i).unwrap_or_else(|| unreachable!("checked above"));
            if !self.machine.bus.poke(a, 1, u32::from(b)) {
                // `peek` succeeded on this byte just now, so this does not happen.
                return Err(BackendError::Unwritable(a));
            }
        }
        Ok(())
    }

    fn memory_regions(&self) -> Vec<MemRegion> {
        [
            ("Flash (NS)", FLASH_NS, FLASH_SIZE),
            ("Flash (S)", FLASH_S, FLASH_SIZE),
            ("SRAM (NS)", SRAM_NS, SRAM_SIZE),
            ("SRAM (S)", SRAM_S, SRAM_SIZE),
        ]
        .into_iter()
        .map(|(name, start, len)| MemRegion {
            name: name.into(),
            start,
            len,
        })
        .collect()
    }

    fn set_core_reg(&mut self, index: usize, value: u32) -> Result<(), BackendError> {
        let name =
            core_reg_name(index).ok_or_else(|| BackendError::BadRegister(index.to_string()))?;
        self.set_banked_reg(&name, value)
    }

    fn set_banked_reg(&mut self, name: &str, value: u32) -> Result<(), BackendError> {
        self.machine
            .write_reg(name, value)
            .map_err(|_| BackendError::BadRegister(name.to_owned()))
    }

    fn set_breakpoints(&mut self, addrs: Vec<u32>) {
        self.machine.breakpoints = addrs.into_iter().collect();
    }

    fn begin_goal(&mut self, goal: Goal) -> Result<bool, BackendError> {
        Ok(match goal {
            Goal::StepOver => self.machine.arm_step_over(),
            Goal::StepOut => {
                self.machine.arm_step_out();
                true
            }
            Goal::RunTo(addr) => {
                self.machine.arm_run_to(addr);
                true
            }
        })
    }

    fn cancel_goal(&mut self) {
        self.machine.disarm();
    }

    fn watchpoints(&self) -> Vec<Watchpoint> {
        self.machine
            .watchpoints()
            .into_iter()
            .map(watchpoint)
            .collect()
    }

    fn set_watchpoints(&mut self, w: Vec<Watchpoint>) -> Result<(), BackendError> {
        self.machine
            .set_watchpoints(w.into_iter().map(watch_range).collect())
            .map_err(|e| BackendError::Invalid(e.to_string()))
    }

    fn set_bus_monitor(&mut self, range: Option<(u32, u32)>) {
        self.monitor = range;
        self.machine.bus.set_bus_monitor(range);
    }

    fn bus_records(&self) -> Vec<BusRecord> {
        self.machine
            .bus
            .bus_records()
            .into_iter()
            .map(bus_record)
            .collect()
    }

    fn clear_bus_records(&mut self) {
        self.machine.bus.clear_bus_records();
    }

    fn peripherals(&self) -> Vec<DeviceView> {
        self.machine
            .peripherals()
            .into_iter()
            .map(|d| DeviceView {
                name: d.name.to_owned(),
                base: d.base,
                regs: d
                    .regs
                    .into_iter()
                    .map(|r| RegRow {
                        name: r.name,
                        offset: r.offset,
                        value: r.value,
                        note: r.note,
                    })
                    .collect(),
            })
            .collect()
    }

    // Not wired to the core yet (Plan 4, Task W2): each returns `Unsupported`, an empty
    // value, or does nothing.

    fn take_uart2(&mut self) -> Vec<u8> {
        Vec::new()
    }

    fn send_uart(&mut self, _port: UartId, _bytes: &[u8]) {}

    fn take_key_events(&mut self) -> Vec<KeyEvent> {
        Vec::new()
    }

    fn hardware(&self) -> HardwareView {
        HardwareView {
            backlight: self.backlight(),
            charger_line: false,
            battery_ok: false,
            usb_attached: false,
            power_hold: false,
            deep_sleep: false,
            rtc_seconds: 0,
        }
    }

    fn set_hardware(&mut self, _change: HwChange) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("hardware changes"))
    }

    fn export_ram(&self) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("exporting RAM"))
    }

    fn import_ram(&mut self, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("importing RAM"))
    }

    fn export_flash(&self, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("exporting flash"))
    }

    fn import_flash(&mut self, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("importing flash"))
    }
}
