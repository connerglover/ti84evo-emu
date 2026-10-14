//! [`MachineBackend`]: the emulated TI-84 Evo behind [`EmuBackend`].
//!
//! This is the only GUI file that names the core crates (`evo_core`, `evo_image`, `armv8m`);
//! `tests/boundary.rs` enforces that. It uses only the `Machine` API that Plan 1 introduced
//! and Plan 2 keeps: `Machine::from_bundle`, `run`, `step`, `breakpoints`, `bus.peek`,
//! `bus.log`, `cpu.regs` and `sched.now`. The LCD, keypad, UART, flash file and save states
//! arrive with Plan 2.

use std::path::Path;

use armv8m::decode::is_32bit;
use armv8m::{Sec, decode16, decode32, disasm};
use evo_core::{Machine, StopReason};
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
}

impl MachineBackend {
    /// Reads and boots the bundle at `path`; the backend is named after its file name.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::from_bytes(&bytes, name)
    }

    /// Boots the bundle in `bytes`; `name` is for display only.
    pub fn from_bytes(bytes: &[u8], name: String) -> Result<Self, OpenError> {
        let bundle = Bundle::from_bytes(bytes)?;
        let machine = Machine::from_bundle(&bundle, &Layout::EVO_7_1)?;
        Ok(Self {
            machine,
            bundle,
            name,
            tag: format!("machine:{:016x}", fnv1a64(bytes)),
        })
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
        // The LCD arrives with Plan 2.
        None
    }

    fn backlight(&self) -> f32 {
        1.0
    }

    fn set_key(&mut self, key: KeyId, pressed: bool) {
        // The keypad arrives with Plan 2.
        log::debug!("key {key:?} pressed={pressed} ignored: no keypad model yet");
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
        Vec::new()
    }

    fn save_state(&self) -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unsupported("save states"))
    }

    fn load_state(&mut self, _bytes: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported("save states"))
    }

    fn reset(&mut self) {
        match Machine::from_bundle(&self.bundle, &Layout::EVO_7_1) {
            Ok(mut m) => {
                m.breakpoints = std::mem::take(&mut self.machine.breakpoints);
                self.machine = m;
            }
            // The same bundle booted once already, so this does not happen.
            Err(e) => log::error!("reset failed, machine kept: {e}"),
        }
    }

    fn persist(&mut self) -> Result<(), BackendError> {
        // No flash file before Plan 2.
        Ok(())
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
