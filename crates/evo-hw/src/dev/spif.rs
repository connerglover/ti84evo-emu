//! SPIF, the QSPI NOR controller at `0x5000_1000`, over the [`NorFlash`] command engine
//! (`docs/hardware/spif-nor.md` §3, §5, §8).
//!
//! The firmware loads a command descriptor into +0x00..+0x0C and starts it by writing CTRL
//! (+0x20) with bit 1 set. START is level-triggered: **every** such write runs the descriptor,
//! aborting any open data phase, because a timed-out command leaves bit 1 set (§8.4). Data
//! moves through the FIFO port at +0x100; FIFO_LEVEL (+0x54) reports at most [`SPIF_FIFO`]
//! bytes so the firmware's 4-byte-step chunk loop stays aligned (§5.2). Read data is produced
//! one byte per pop, so a guest length of 0xFFFF_FFFF costs nothing.
//!
//! Erases and programs go straight into the memory map's [`FLASH_BUFFER`], so the next XIP
//! read sees them. Without that buffer, flash behaves as unmapped: reads give 0xFF and
//! programs and erases are dropped (logged as a warning).
//!
//! The block drives no IRQ (IRQ 22 is a default stub, §1). Registers take aligned 32-bit
//! accesses; the FIFO port takes 8-, 16- and 32-bit accesses (1, 2 or 4 bytes, little-endian).
//! Anything else reads 0, is ignored and is logged as unhandled.
//!
//! **Reset.** A System reset aborts the data phase (dropping RX data), clears DONE, CTRL bit 1
//! and the NOR's WEL, and keeps the other registers, the status-register bits other than WEL
//! and the security registers; the flash bytes live in the map and are not touched (§8.8). A
//! PowerOn reset also clears every register and returns SR1-SR3 to their factory values; the
//! security registers are non-volatile and are kept (the flash file restores them).
//!
//! **Save state** holds the registers, SR1-SR3 and the open data phase; the security registers
//! are not part of it ([`NorFlash`] skips them) and [`Device::load`] keeps the current ones.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::nor::{DataPhase, NorFlash};
use crate::soc::FLASH_BUFFER;
use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// The largest FIFO_LEVEL (+0x54 `[15:8]`) the controller reports, in bytes.
pub const SPIF_FIFO: u32 = 64;

const ADDR: u32 = 0x00;
const CFG: u32 = 0x04;
const LEN: u32 = 0x08;
const OP: u32 = 0x0C;
const CFG0: u32 = 0x10;
const STATUS: u32 = 0x18;
const CTRL: u32 = 0x20;
const INT_STATUS: u32 = 0x24;
const XIP_CFG: u32 = 0x30;
const XIP_CTRL: u32 = 0x34;
const FIFO_LEVEL: u32 = 0x54;
const FIFO: u32 = 0x100;

/// CTRL bit 1: START.
const CTRL_START: u32 = 1 << 1;
/// INT_STATUS bit 0: DONE (W1C).
const DONE: u32 = 1 << 0;
/// XIP_CTRL bit 10: a request that completes at once (reads 0).
const XIP_CTRL_REQ: u32 = 1 << 10;
/// SR1 write-enable latch.
const WEL: u8 = 0x02;
/// STATUS bit 0: TX FIFO has space (always).
const STATUS_TX_SPACE: u32 = 1 << 0;
/// STATUS bit 1: RX FIFO not empty.
const STATUS_RX_NOT_EMPTY: u32 = 1 << 1;

/// The SPIF registers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Regs {
    addr: u32,
    cfg: u32,
    len: u32,
    op: u32,
    cfg0: u32,
    ctrl: u32,
    xip_cfg: u32,
    xip_ctrl: u32,
    done: bool,
}

/// The SPIF controller and the NOR part behind it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spif {
    regs: Regs,
    /// Kind of the data phase the last command opened (the NOR closes it when it runs out).
    kind: DataPhase,
    nor: NorFlash,
}

impl Default for Spif {
    fn default() -> Self {
        Self::new()
    }
}

/// The flash bytes for a NOR operation that `changes` the given range of them, or an empty
/// slice when the map has no flash buffer (unmapped: reads 0xFF, writes dropped; `Spif::start`
/// warns). An operation that changes nothing gets an empty slice too. The buffer is borrowed
/// with exactly the changed range, so the map drops only code cached from those bytes.
fn flash<'c>(ctx: &'c mut DevCtx<'_>, changes: Option<Range<usize>>) -> &'c mut [u8] {
    let Some(range) = changes else {
        return &mut [];
    };
    ctx.buffer_mut_range(FLASH_BUFFER, range).unwrap_or(&mut [])
}

/// Length of the map's flash buffer (0 without one).
fn flash_len(ctx: &DevCtx<'_>) -> usize {
    ctx.buffer(FLASH_BUFFER).map_or(0, <[u8]>::len)
}

/// Whether command `op` reads, programs or erases the flash array.
fn touches_array(op: u8) -> bool {
    matches!(
        op,
        0x03 | 0x0B | 0x3B | 0x6B | 0xBB | 0xEB | 0x02 | 0x32 | 0x20 | 0x52 | 0xD8 | 0xC7 | 0x60
    )
}

impl Spif {
    /// A controller with every register 0 and a NOR part in its factory state.
    pub fn new() -> Self {
        Self {
            regs: Regs::default(),
            kind: DataPhase::None,
            nor: NorFlash::new(),
        }
    }

    /// The NOR part.
    pub fn nor(&self) -> &NorFlash {
        &self.nor
    }

    /// The NOR part, mutable (the host restores security registers through it).
    pub fn nor_mut(&mut self) -> &mut NorFlash {
        &mut self.nor
    }

    /// Bytes left in the open data phase of `kind`.
    fn left(&self, kind: DataPhase) -> u32 {
        if self.kind == kind {
            self.nor.remaining()
        } else {
            0
        }
    }

    /// Runs the loaded descriptor (§8.5).
    fn start(&mut self, ctx: &mut DevCtx<'_>) {
        self.nor.abort();
        self.regs.done = false;
        let op = (self.regs.op >> 24) as u8;
        let addr = match self.regs.cfg & 7 {
            0 => 0,
            1 => self.regs.addr & 0xFF,
            2 => self.regs.addr & 0xFFFF,
            _ => self.regs.addr & 0xFF_FFFF,
        };
        if touches_array(op) && ctx.buffer(FLASH_BUFFER).is_none() {
            log::warn!("spif: op {op:#04x} without a flash buffer: reads 0xFF, writes dropped");
        }
        let changes = self.nor.begin_changes(op, addr, flash_len(ctx));
        let kind = self.nor.begin(op, addr, self.regs.len, flash(ctx, changes));
        self.kind = kind;
        // DONE means "transfer finished": at once unless data is still to be written.
        self.regs.done = kind != DataPhase::Write;
    }

    fn pop(&mut self, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        let n = u32::from(width).min(4);
        let mut v = 0;
        for i in 0..n {
            if self.left(DataPhase::Read) == 0 {
                break;
            }
            let b = self.nor.read_byte(ctx.buffer(FLASH_BUFFER).unwrap_or(&[]));
            v |= u32::from(b) << (8 * i);
        }
        v
    }

    fn push(&mut self, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        for i in 0..u32::from(width).min(4) {
            if self.left(DataPhase::Write) == 0 {
                return;
            }
            let changes = self.nor.write_changes(flash_len(ctx));
            if self
                .nor
                .write_byte((v >> (8 * i)) as u8, flash(ctx, changes))
            {
                self.regs.done = true;
            }
        }
    }

    fn status(&self) -> u32 {
        let rx = if self.left(DataPhase::Read) > 0 {
            STATUS_RX_NOT_EMPTY
        } else {
            0
        };
        STATUS_TX_SPACE | rx
    }

    fn fifo_level(&self) -> u32 {
        self.nor.remaining().min(SPIF_FIFO) << 8
    }
}

fn reg_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < FIFO
}

impl Device for Spif {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if off == FIFO {
            return self.pop(width, ctx);
        }
        if !reg_access(off, width) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        match off {
            ADDR => self.regs.addr,
            CFG => self.regs.cfg,
            LEN => self.regs.len,
            OP => self.regs.op,
            CFG0 => self.regs.cfg0,
            STATUS => self.status(),
            CTRL => self.regs.ctrl,
            INT_STATUS => u32::from(self.regs.done),
            XIP_CFG => self.regs.xip_cfg,
            XIP_CTRL => self.regs.xip_ctrl,
            FIFO_LEVEL => self.fifo_level(),
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if off == FIFO {
            self.push(width, v, ctx);
            return;
        }
        if !reg_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            ADDR => self.regs.addr = v,
            CFG => self.regs.cfg = v,
            LEN => self.regs.len = v,
            OP => self.regs.op = v,
            CFG0 => self.regs.cfg0 = v,
            CTRL => {
                self.regs.ctrl = v;
                // Level-triggered; bit 1 clear only ends the command and keeps RX data.
                if v & CTRL_START != 0 {
                    self.start(ctx);
                }
            }
            INT_STATUS => {
                if v & DONE != 0 {
                    self.regs.done = false;
                }
            }
            XIP_CFG => self.regs.xip_cfg = v,
            XIP_CTRL => self.regs.xip_ctrl = v & !XIP_CTRL_REQ,
            // STATUS and FIFO_LEVEL are read-only; the rest is not modeled.
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.nor.abort();
        self.kind = DataPhase::None;
        match kind {
            ResetKind::System => {
                self.regs.done = false;
                self.regs.ctrl &= !CTRL_START;
                self.nor.sr1 &= !WEL;
            }
            ResetKind::PowerOn => {
                // The security registers are non-volatile (module doc).
                let secregs = self.nor.secregs().to_vec();
                *self = Self::new();
                self.nor.set_secregs(&secregs);
            }
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        // The state has no security registers: keep the current ones.
        s.nor.set_secregs(self.nor.secregs());
        *self = s;
        Ok(())
    }
}
