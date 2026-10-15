//! [`MachineBackend`]: the emulated TI-84 Evo behind [`EmuBackend`].
//!
//! This is the only GUI file that names the core crates (`evo_core`, `evo_hw`, `evo_image`,
//! `armv8m`); `tests/boundary.rs` enforces that. Besides Plan 1's `Machine` API (`run`,
//! `step`, `breakpoints`, `bus.peek`, `bus.log`, `cpu.regs`, `sched.now`) it uses Plan 2's
//! host API: `screen`, the backlight `Pwm`, `set_key`, `take_uart_tx`, `save_state` /
//! `load_state`, and the flash file (`from_bundle_nv`, `nv_state`, `NvState`).

use std::path::{Path, PathBuf};

use armv8m::decode::is_32bit;
use armv8m::{Sec, decode16, decode32, disasm};
use evo_core::{Key, Machine, NvError, NvState, StopReason, UartPort};
use evo_hw::dev::lcdc::Pwm;
use evo_image::{Bundle, ImageError, Layout};

use crate::backend::{
    BackendError, CpuView, DisasmLine, EmuBackend, Frame, RunStatus, UnmappedRow,
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

const fn mx(row: u8, col: u8) -> Key {
    Key::Matrix { row, col }
}

/// Every GUI key's position in the keypad matrix, in [`KeyId::ALL`] order.
///
/// Hypothesis: the TI-84 Plus CE matrix, CE keypad group = `row`, bit = `col` (the CE port map
/// on WikiTI and CEmu's keypad, the same as the TI-83+/84+ `GetCSC` groups); it matches
/// `evo_core`'s `KEY_NAMES`. Task 11 Step 4 of the GUI plan checks it against the real OS
/// (`docs/hardware/discovery-log.md`, "GUI bring-up").
///
/// | row | col 0 | col 1 | col 2 | col 3 | col 4 | col 5 | col 6 | col 7 |
/// |---|---|---|---|---|---|---|---|---|
/// | 1 | graph | trace | zoom | window | y= | 2nd | mode | del |
/// | 2 | – | sto→ | ln | log | x² | x⁻¹ | math | alpha |
/// | 3 | 0 | 1 | 4 | 7 | , | sin | apps | X,T,θ,n |
/// | 4 | . | 2 | 5 | 8 | ( | cos | prgm | stat |
/// | 5 | (−) | 3 | 6 | 9 | ) | tan | vars | – |
/// | 6 | enter | + | − | × | ÷ | ^ | clear | – |
/// | 7 | down | left | right | up | – | – | – | – |
///
/// `on` is [`Key::On`]: 49 matrix keys plus ON.
pub const KEY_MATRIX: [(KeyId, Key); 50] = [
    (KeyId::YEqu, mx(1, 4)),
    (KeyId::Window, mx(1, 3)),
    (KeyId::Zoom, mx(1, 2)),
    (KeyId::Trace, mx(1, 1)),
    (KeyId::Graph, mx(1, 0)),
    (KeyId::Second, mx(1, 5)),
    (KeyId::Mode, mx(1, 6)),
    (KeyId::Del, mx(1, 7)),
    (KeyId::Up, mx(7, 3)),
    (KeyId::Left, mx(7, 1)),
    (KeyId::Right, mx(7, 2)),
    (KeyId::Down, mx(7, 0)),
    (KeyId::Alpha, mx(2, 7)),
    (KeyId::XTThetaN, mx(3, 7)),
    (KeyId::Stat, mx(4, 7)),
    (KeyId::Math, mx(2, 6)),
    (KeyId::Apps, mx(3, 6)),
    (KeyId::Prgm, mx(4, 6)),
    (KeyId::Vars, mx(5, 6)),
    (KeyId::Clear, mx(6, 6)),
    (KeyId::Inverse, mx(2, 5)),
    (KeyId::Sin, mx(3, 5)),
    (KeyId::Cos, mx(4, 5)),
    (KeyId::Tan, mx(5, 5)),
    (KeyId::Power, mx(6, 5)),
    (KeyId::Square, mx(2, 4)),
    (KeyId::Comma, mx(3, 4)),
    (KeyId::LParen, mx(4, 4)),
    (KeyId::RParen, mx(5, 4)),
    (KeyId::Div, mx(6, 4)),
    (KeyId::Log, mx(2, 3)),
    (KeyId::Seven, mx(3, 3)),
    (KeyId::Eight, mx(4, 3)),
    (KeyId::Nine, mx(5, 3)),
    (KeyId::Mul, mx(6, 3)),
    (KeyId::Ln, mx(2, 2)),
    (KeyId::Four, mx(3, 2)),
    (KeyId::Five, mx(4, 2)),
    (KeyId::Six, mx(5, 2)),
    (KeyId::Sub, mx(6, 2)),
    (KeyId::Sto, mx(2, 1)),
    (KeyId::One, mx(3, 1)),
    (KeyId::Two, mx(4, 1)),
    (KeyId::Three, mx(5, 1)),
    (KeyId::Add, mx(6, 1)),
    (KeyId::On, Key::On),
    (KeyId::Zero, mx(3, 0)),
    (KeyId::Dot, mx(4, 0)),
    (KeyId::Neg, mx(5, 0)),
    (KeyId::Enter, mx(6, 0)),
];

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
    /// The last LCD image, converted for the GUI.
    frame: Frame,
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
    /// be read is an [`OpenError::Flash`] and is left untouched. Otherwise flash starts erased.
    pub fn from_bytes(bytes: &[u8], name: String, nv_dir: &Path) -> Result<Self, OpenError> {
        let bundle = Bundle::from_bytes(bytes)?;
        let hash = fnv1a64(bytes);
        let nv_path = nv_dir.join(format!("{hash:016x}.nv"));
        let nv = if nv_path.exists() {
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
        Ok(Self {
            machine,
            bundle,
            name,
            tag: format!("machine:{hash:016x}"),
            nv_path,
            frame: Frame {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            },
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

fn run_status(stop: StopReason) -> RunStatus {
    match stop {
        StopReason::CycleLimit | StopReason::Halted | StopReason::Stepped => RunStatus::Running,
        StopReason::Idle => RunStatus::Idle,
        StopReason::Breakpoint(a) => RunStatus::Breakpoint(a),
        StopReason::Lockup { pc } => RunStatus::Lockup { pc },
        StopReason::Bkpt { pc, imm } => RunStatus::Bkpt { pc, imm },
    }
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
        // `KEY_MATRIX` lists every `KeyId` (`key_matrix_table_is_complete`).
        if let Some(&(_, k)) = KEY_MATRIX.iter().find(|e| e.0 == key) {
            self.machine.set_key(k, pressed);
        }
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
                m.breakpoints = std::mem::take(&mut self.machine.breakpoints);
                self.machine = m;
            }
            // The same bundle booted once already, so this does not happen.
            Err(e) => log::error!("reset failed, machine kept: {e}"),
        }
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        if let Some(dir) = self.nv_path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| BackendError::Persist(e.to_string()))?;
        }
        self.machine
            .nv_state()
            .write_file(&self.nv_path)
            .map_err(|e| BackendError::Persist(e.to_string()))
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
}
