//! SPI master at `0x4010_7000` and the ST7789 panel driver IC on its bus
//! (`docs/hardware/spi-panel.md`, "Minimal model specification").
//!
//! The bus only configures the LCD panel (3-wire, 9-bit frames: bit 8 is D/C) and reads its ID;
//! pixels go through the LCDC. Transfers complete instantly: STATUS.BUSY is always 0, and every
//! MOSI frame shifted produces exactly one MISO frame in the RX FIFO. Frames are shifted when
//! DATA is written while CTRL2 bit 0 (enable) is set, or, if queued in the TX FIFO while
//! disabled, when CTRL2 is written with bit 0 set. The block drives no IRQ (the firmware clears
//! every interrupt enable and installs default stubs).
//!
//! Only aligned 32-bit accesses reach registers; any other width or alignment, unmodeled
//! offsets and writes to the read-only STATUS/PARAM words read 0, are ignored and are logged as
//! unhandled.
//!
//! A System reset clears the SPI registers and both FIFOs but keeps the panel's state (the
//! panel sits outside the SoC and is reset over GPIO, `gpio.md`). A PowerOn reset also returns
//! the panel to its power-on state; the panel ID never changes.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// TX FIFO depth; +0x64 reads `(DEPTH - 1) << 16`.
pub const SPI_FIFO_DEPTH: usize = 8;
/// RX FIFO capacity; when a frame arrives while it is full, the oldest frame is dropped.
pub const SPI_RX_CAP: usize = 16;
/// Panel ID (ID1, ID2, ID3): a generic ST7789V. ID2 0x85 selects the firmware's "Unknown
/// Display" entry and its default init table.
pub const DEFAULT_PANEL_ID: [u8; 3] = [0x85, 0x85, 0x52];

const CTRL0: u32 = 0x00;
const CTRL1: u32 = 0x04;
const CTRL2: u32 = 0x08;
const STATUS: u32 = 0x0C;
const DATA: u32 = 0x18;
const INTEN: u32 = 0x1C;
const PARAM: u32 = 0x64;
/// Size of the register window.
const WINDOW: u32 = 0x1000;

/// CTRL2 bit 0: enable.
const CTRL2_EN: u32 = 1 << 0;
/// CTRL2 bit 6: written 1 on every disable; with EN clear it flushes both FIFOs. It is
/// self-clearing (reads 0), so the read-modify-writes of the enable sequence that follow do not
/// flush the frames queued in between (`spi-panel.md`, open question 3).
const CTRL2_FLUSH: u32 = 1 << 6;
/// STATUS bit 1: TX FIFO not full.
const STATUS_TNF: u32 = 1 << 1;

// --- ST7789 panel ---------------------------------------------------------------------------

/// Panel commands the responder acts on.
const SWRESET: u8 = 0x01;
const RDDID: u8 = 0x04;
const RDDST: u8 = 0x09;
const RDDPM: u8 = 0x0A;
const SLPIN: u8 = 0x10;
const SLPOUT: u8 = 0x11;
const DISPOFF: u8 = 0x28;
const DISPON: u8 = 0x29;
const MADCTL: u8 = 0x36;
const COLMOD: u8 = 0x3A;
const RDID1: u8 = 0xDA;
const RDID3: u8 = 0xDC;
/// Registers the BSP's "LCD Info" screen reads back (VDV/VRH, VCOMS offset, VCOMS setting).
const DIAG_CMDS: [u8; 3] = [0xC2, 0xC5, 0xBB];
/// Bytes a diagnostic read returns (after the dummy bit).
const DIAG_LEN: usize = 5;
/// The longest read: dummy bit + 5 bytes.
const MAX_QUEUE_BITS: usize = 1 + 8 * DIAG_LEN;
/// COLMOD after reset (ST7789V: 18 bits per pixel).
const COLMOD_RESET: u8 = 0x66;

/// A minimal ST7789 responder on the 3-wire bus: MSB first, a frame with bit 8 clear is a
/// command, bit 8 set a parameter, and command 0x00 clocks out the pending read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct St7789 {
    id: [u8; 3],
    /// The last command (not 0x00, which only clocks out read data).
    cmd: u8,
    /// Parameter frames received since `cmd` (saturating).
    nparam: u8,
    /// Last written parameters of [`DIAG_CMDS`], at most [`DIAG_LEN`] bytes each.
    diag: [Vec<u8>; 3],
    /// Read data still to be clocked out, MSB first.
    queue: VecDeque<bool>,
    sleeping: bool,
    display_on: bool,
    madctl: u8,
    colmod: u8,
    /// The last MOSI frame, masked to the frame size.
    last_frame: u16,
}

impl St7789 {
    /// A panel with ID `id` in its power-on state (asleep, display off).
    pub fn new(id: [u8; 3]) -> Self {
        St7789 {
            id,
            cmd: 0,
            nparam: 0,
            diag: Default::default(),
            queue: VecDeque::new(),
            sleeping: true,
            display_on: false,
            madctl: 0,
            colmod: COLMOD_RESET,
            last_frame: 0,
        }
    }

    /// The panel ID (ID1, ID2, ID3).
    pub fn id(&self) -> [u8; 3] {
        self.id
    }

    /// DISPON (0x29) seen since the last DISPOFF (0x28) or reset.
    pub fn display_on(&self) -> bool {
        self.display_on
    }

    /// In sleep mode: at reset and after SLPIN (0x10), until SLPOUT (0x11).
    pub fn sleeping(&self) -> bool {
        self.sleeping
    }

    /// The MADCTL (0x36) parameter.
    pub fn madctl(&self) -> u8 {
        self.madctl
    }

    /// The COLMOD (0x3A) parameter.
    pub fn colmod(&self) -> u8 {
        self.colmod
    }

    /// The last MOSI frame the panel was clocked with.
    pub fn last_frame(&self) -> u16 {
        self.last_frame
    }

    /// Clock one `nbits`-bit frame `frame` in; returns the MISO frame (the low `nbits` bits,
    /// at most 16, of the pending read data, zero-padded).
    pub fn clock(&mut self, frame: u16, nbits: u32) -> u16 {
        let miso = if frame & 0x100 != 0 {
            self.param(frame as u8);
            0
        } else if frame as u8 == 0 {
            self.shift_out(nbits)
        } else {
            self.command(frame as u8);
            0
        };
        self.last_frame = frame;
        miso
    }

    fn shift_out(&mut self, nbits: u32) -> u16 {
        (0..nbits.min(16)).fold(0, |v, _| {
            (v << 1) | u16::from(self.queue.pop_front().unwrap_or(false))
        })
    }

    fn load_read(&mut self, dummy: bool, bytes: &[u8]) {
        if dummy {
            self.queue.push_back(false);
        }
        for &b in bytes {
            self.queue.extend((0..8).rev().map(|i| (b >> i) & 1 != 0));
        }
    }

    fn command(&mut self, c: u8) {
        self.cmd = c;
        self.nparam = 0;
        self.queue.clear();
        match c {
            SWRESET => *self = St7789::new(self.id),
            RDDID => {
                let id = self.id;
                self.load_read(true, &id);
            }
            RDDST => self.load_read(true, &[0; 4]),
            RDDPM => {
                let pm = (u8::from(!self.sleeping) << 4) | (u8::from(self.display_on) << 2);
                self.load_read(false, &[pm]);
            }
            0x0B..=0x0F => self.load_read(false, &[0]),
            RDID1..=RDID3 => {
                let b = self.id[usize::from(c - RDID1)];
                self.load_read(false, &[b]);
            }
            SLPIN => self.sleeping = true,
            SLPOUT => self.sleeping = false,
            DISPOFF => self.display_on = false,
            DISPON => self.display_on = true,
            _ => {
                if let Some(i) = DIAG_CMDS.iter().position(|&d| d == c) {
                    let mut bytes = [0u8; DIAG_LEN];
                    bytes[..self.diag[i].len()].copy_from_slice(&self.diag[i]);
                    self.load_read(true, &bytes);
                }
            }
        }
    }

    fn param(&mut self, b: u8) {
        let first = self.nparam == 0;
        self.nparam = self.nparam.saturating_add(1);
        match self.cmd {
            MADCTL if first => self.madctl = b,
            COLMOD if first => self.colmod = b,
            c => {
                if let Some(i) = DIAG_CMDS.iter().position(|&d| d == c) {
                    let p = &mut self.diag[i];
                    if first {
                        p.clear();
                    }
                    if p.len() < DIAG_LEN {
                        p.push(b);
                    }
                }
            }
        }
    }

    /// Whether a decoded state keeps the invariants `clock` relies on.
    fn valid(&self) -> bool {
        self.queue.len() <= MAX_QUEUE_BITS && self.diag.iter().all(|p| p.len() <= DIAG_LEN)
    }
}

// --- SPI master -----------------------------------------------------------------------------

/// The SPI master's registers and FIFOs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Regs {
    ctrl0: u32,
    ctrl1: u32,
    ctrl2: u32,
    inten: u32,
    /// Frames written while disabled (at most [`SPI_FIFO_DEPTH`]).
    tx: VecDeque<u16>,
    /// MISO frames not yet read (at most [`SPI_RX_CAP`]).
    rx: VecDeque<u16>,
}

/// The SPI master at `0x4010_7000` with its panel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Spi {
    regs: Regs,
    panel: St7789,
}

impl Spi {
    /// An SPI master in its reset state, wired to `panel`.
    pub fn new(panel: St7789) -> Self {
        Spi {
            regs: Regs::default(),
            panel,
        }
    }

    /// CTRL1: `[15:0]` clock divider − 1, `[22:16]` frame size − 1.
    pub fn ctrl1(&self) -> u32 {
        self.regs.ctrl1
    }

    /// The panel on the bus.
    pub fn panel(&self) -> &St7789 {
        &self.panel
    }

    /// Frame size in bits, from CTRL1[22:16].
    fn nbits(&self) -> u32 {
        ((self.regs.ctrl1 >> 16) & 0x7F) + 1
    }

    /// Mask of the frame bits the 16-bit data path carries.
    fn mask(&self) -> u16 {
        match self.nbits() {
            n if n >= 16 => u16::MAX,
            n => (1u16 << n) - 1,
        }
    }

    fn shift(&mut self, f: u16) {
        let miso = self.panel.clock(f, self.nbits()) & self.mask();
        if self.regs.rx.len() >= SPI_RX_CAP {
            self.regs.rx.pop_front();
        }
        self.regs.rx.push_back(miso);
    }

    fn status(&self) -> u32 {
        let tnf = if self.regs.tx.len() < SPI_FIFO_DEPTH {
            STATUS_TNF
        } else {
            0
        };
        tnf | ((self.regs.rx.len().min(63) as u32) << 4)
    }

    fn write_ctrl2(&mut self, v: u32) {
        self.regs.ctrl2 = v & !CTRL2_FLUSH;
        if v & CTRL2_FLUSH != 0 && v & CTRL2_EN == 0 {
            self.regs.tx.clear();
            self.regs.rx.clear();
        }
        if v & CTRL2_EN != 0 {
            while let Some(f) = self.regs.tx.pop_front() {
                self.shift(f);
            }
        }
    }

    fn write_data(&mut self, v: u32) {
        let f = (v as u16) & self.mask();
        if self.regs.ctrl2 & CTRL2_EN != 0 {
            self.shift(f);
        } else if self.regs.tx.len() < SPI_FIFO_DEPTH {
            self.regs.tx.push_back(f);
        }
    }
}

fn word_access(off: u32, width: u8) -> bool {
    width == 4 && off.is_multiple_of(4) && off < WINDOW
}

impl Device for Spi {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        if !word_access(off, width) {
            ctx.unhandled(off, width, None);
            return 0;
        }
        match off {
            CTRL0 => self.regs.ctrl0,
            CTRL1 => self.regs.ctrl1,
            CTRL2 => self.regs.ctrl2,
            STATUS => self.status(),
            DATA => u32::from(self.regs.rx.pop_front().unwrap_or(0)),
            INTEN => self.regs.inten,
            PARAM => ((SPI_FIFO_DEPTH as u32) - 1) << 16,
            _ => {
                ctx.unhandled(off, width, None);
                0
            }
        }
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !word_access(off, width) {
            ctx.unhandled(off, width, Some(v));
            return;
        }
        match off {
            CTRL0 => self.regs.ctrl0 = v,
            CTRL1 => self.regs.ctrl1 = v,
            CTRL2 => self.write_ctrl2(v),
            DATA => self.write_data(v),
            INTEN => self.regs.inten = v,
            // STATUS and PARAM are read-only; the rest is not modeled.
            _ => ctx.unhandled(off, width, Some(v)),
        }
    }

    fn reset(&mut self, kind: ResetKind, _ctx: &mut DevCtx<'_>) {
        self.regs = Regs::default();
        // The panel keeps its state across a System reset (module doc).
        if kind == ResetKind::PowerOn {
            self.panel = St7789::new(self.panel.id);
        }
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.regs.tx.len() > SPI_FIFO_DEPTH || s.regs.rx.len() > SPI_RX_CAP || !s.panel.valid() {
            return Err(StateError::Mismatch("SPI state out of range".into()));
        }
        *self = s;
        Ok(())
    }
}
