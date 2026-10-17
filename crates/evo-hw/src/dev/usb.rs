//! Faraday-compatible USB device controller and the USB Type-C companion.
//!
//! Register identities follow the Linux FOTG210 UDC definitions; Evo-specific behavior is
//! documented in `docs/hardware/usb.md`. Host packets are carried through guest FIFOs and DMA.

use super::storage::Storage;
use crate::{DevCtx, Device, RegView, ResetKind, StateError, load_serde, save_serde};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const USB_ID: u32 = 0x0001_0000;
pub const USB_CONFIG: u32 = 0x0000_0880;
const WINDOW: u32 = 0x1000;
const TC_CC_BYTE: u32 = 0x1D;
const TC_CC_MASK: u8 = 0x0F;
const FIFO_CAP: usize = 65536;

fn lane_offsets(off: u32, width: u8) -> impl Iterator<Item = (usize, u32)> {
    (0..u32::from(width)).filter_map(move |i| off.checked_add(i).map(|o| (i as usize * 8, o)))
}

/// Result of a host token. NAK means the guest has not supplied/consumed its FIFO yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UsbError {
    #[error("USB device is disconnected")]
    Disconnected,
    #[error("USB endpoint is not ready (NAK)")]
    Nak,
    #[error("USB endpoint is stalled")]
    Stall,
    #[error("invalid USB endpoint")]
    InvalidEndpoint,
    #[error("USB packet exceeds endpoint maximum packet size")]
    PacketTooLarge,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Fifo {
    bytes: VecDeque<u8>,
    /// Zero-length packets need a separate representation from an empty FIFO.
    packet: bool,
}

/// Device-side USB controller; endpoint zero is the control FIFO, the other endpoints are
/// routed through the four programmable FIFOs. IRQ 16 is derived from unmasked sources.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usb {
    regs: Storage,
    attached: bool,
    setup: VecDeque<u8>,
    cx: Fifo,
    fifo: [Fifo; 4],
    control_in: bool,
    control_done: bool,
}

impl Default for Usb {
    fn default() -> Self {
        Self::new()
    }
}

impl Usb {
    pub fn new() -> Self {
        Self {
            regs: Storage::new(WINDOW),
            attached: false,
            setup: VecDeque::new(),
            cx: Fifo::default(),
            fifo: std::array::from_fn(|_| Fifo::default()),
            control_in: false,
            control_done: false,
        }
    }

    fn set(&mut self, off: u32, value: u32) {
        self.regs.set(off, 4, value);
    }
    fn bits(&mut self, off: u32, value: u32) {
        self.set(off, self.regs.word(off) | value);
    }
    fn clear(&mut self, off: u32, value: u32) {
        self.set(off, self.regs.word(off) & !value);
    }

    fn abort_transfers(&mut self) {
        self.setup.clear();
        self.cx = Fifo::default();
        self.fifo = std::array::from_fn(|_| Fifo::default());
        self.control_in = false;
        self.control_done = false;
        for off in (0x300..0x328).step_by(4) {
            self.set(off, 0);
        }
        for off in [0x144, 0x148, 0x328] {
            self.set(off, 0);
        }
        self.clear(0x1C8, 1);
    }

    fn groups(&self) -> u32 {
        let groups = (0..3).fold(0, |v, n| {
            v | (u32::from(self.status(0x144 + n * 4) & !self.regs.word(0x134 + n * 4) != 0) << n)
        });
        groups | (u32::from(self.regs.word(0x328) & !self.regs.word(0x32C) != 0) << 3)
    }

    fn status(&self, off: u32) -> u32 {
        match off {
            0xE0 => USB_ID,
            0xE4 => USB_CONFIG,
            0xC0 => {
                if self.groups() & !self.regs.word(0x130) != 0 {
                    1
                } else {
                    0
                }
            }
            0x140 => self.groups(),
            0x120 => {
                let mut v = self.regs.word(off) & 0x4;
                if self.cx.bytes.is_empty() {
                    v |= 1 << 5;
                }
                v |= (self.cx.bytes.len().min(127) as u32) << 24;
                for (n, f) in self.fifo.iter().enumerate() {
                    if f.bytes.is_empty() {
                        v |= 1 << (8 + n);
                    }
                }
                v
            }
            0x1B0 | 0x1B4 | 0x1B8 | 0x1BC => {
                self.fifo[((off - 0x1B0) / 4) as usize].bytes.len() as u32
            }
            _ => self.regs.word(off),
        }
    }

    fn irq(&self, ctx: &mut DevCtx<'_>) {
        log::trace!(target: "evo_hw::usb", "cycles={} irq16={} dmcr={:08x} gmir={:08x} groups={:08x} masks={:08x}/{:08x}/{:08x}/{:08x} sources={:08x}/{:08x}/{:08x} vdma={:08x}/{:08x}",
            ctx.now,
            self.regs.word(0x100) & 4 != 0 && self.regs.word(0xC4) & 1 == 0 && self.groups() & !self.regs.word(0x130) != 0,
            self.regs.word(0x100), self.regs.word(0xC4), self.groups(),
            self.regs.word(0x130), self.regs.word(0x134), self.regs.word(0x138), self.regs.word(0x13C),
            self.status(0x144), self.status(0x148), self.status(0x14C), self.regs.word(0x328), self.regs.word(0x32C));
        ctx.set_irq(
            16,
            self.regs.word(0x100) & 4 != 0
                && self.regs.word(0xC4) & 1 == 0
                && self.groups() & !self.regs.word(0x130) != 0,
        );
    }

    /// Attach or unplug a host cable. This does not generate a USB bus reset.
    pub fn attach(&mut self, attached: bool, ctx: &mut DevCtx<'_>) {
        if self.attached == attached {
            return;
        }
        self.attached = attached;
        // B-device, peripheral role, B-session valid (FOTG210 OTGCSR).
        if attached {
            self.bits(0x80, (1 << 21) | (1 << 20) | (1 << 17));
            self.clear(0x80, 1 << 16);
        } else {
            self.abort_transfers();
            self.clear(0x80, 1 << 17);
            self.bits(0x80, 1 << 16);
            self.bits(0x14C, 1 << 1);
        }
        self.irq(ctx);
    }

    /// A host-driven bus reset, negotiated at full speed (64-byte bulk packets).
    pub fn bus_reset(&mut self, ctx: &mut DevCtx<'_>) -> Result<(), UsbError> {
        if !self.attached {
            return Err(UsbError::Disconnected);
        }
        self.abort_transfers();
        self.set(0x104, 0);
        self.set(0x144, 0);
        self.set(0x148, 0);
        self.set(0x14C, 1);
        self.clear(0x100, 1 << 6);
        self.irq(ctx);
        Ok(())
    }

    /// Send a SETUP packet. A new SETUP aborts the preceding control transfer and clears stall.
    pub fn host_setup(&mut self, setup: [u8; 8], ctx: &mut DevCtx<'_>) -> Result<(), UsbError> {
        log::trace!(target: "evo_hw::usb", "cycles={} SETUP {setup:02x?}", ctx.now);
        if !self.attached {
            return Err(UsbError::Disconnected);
        }
        self.setup = setup.into();
        self.cx = Fifo::default();
        self.set(0x300, 0);
        self.clear(0x328, 0x10001);
        self.control_in = setup[0] & 0x80 != 0;
        self.control_done = false;
        self.clear(0x120, 4);
        self.set(0x144, 1);
        self.irq(ctx);
        Ok(())
    }

    fn endpoint(&self, ep: u8, input: bool) -> Result<(usize, usize), UsbError> {
        if !self.attached {
            return Err(UsbError::Disconnected);
        }
        if ep == 0 {
            return if self.regs.word(0x120) & 4 != 0 {
                Err(UsbError::Stall)
            } else {
                Ok((4, 64))
            };
        }
        if ep > 8 {
            return Err(UsbError::InvalidEndpoint);
        }
        let mps = self
            .regs
            .word((if input { 0x160 } else { 0x180 }) + (u32::from(ep) - 1) * 4);
        if mps & (1 << 11) != 0 {
            return Err(UsbError::Stall);
        }
        let map = self.regs.word(0x1A0 + u32::from((ep - 1) / 4) * 4);
        let f = ((map >> (u32::from((ep - 1) % 4) * 8 + if input { 0 } else { 4 })) & 0xF) as usize;
        if f >= 4 || self.regs.word(0x1AC) & (0x20 << (f * 8)) == 0 || mps & 0x7FF == 0 {
            return Err(UsbError::Nak);
        }
        Ok((f, (mps & 0x7FF) as usize))
    }

    /// One host OUT packet. The guest receives it through the programmed FIFO and interrupt.
    pub fn host_out(&mut self, ep: u8, bytes: &[u8], ctx: &mut DevCtx<'_>) -> Result<(), UsbError> {
        let (f, mps) = self.endpoint(ep, false)?;
        if bytes.len() > mps {
            return Err(UsbError::PacketTooLarge);
        }
        if ep == 0 && self.control_in && bytes.is_empty() {
            self.bits(0x144, 1 << 3);
            self.irq(ctx);
            return Ok(());
        }
        let fifo = if f == 4 {
            &mut self.cx
        } else {
            &mut self.fifo[f]
        };
        if fifo.packet {
            return Err(UsbError::Nak);
        }
        fifo.bytes.extend(bytes);
        fifo.packet = true;
        if f == 4 {
            self.bits(0x144, 1 << 2);
        } else {
            self.bits(
                0x148,
                (1 | if bytes.len() < mps { 2 } else { 0 }) << (f * 2),
            );
        }
        self.vdma(if f == 4 { 0x300 } else { 0x308 + f as u32 * 8 }, ctx);
        self.irq(ctx);
        Ok(())
    }

    /// One host IN token. Successful packets are acknowledged immediately by the host.
    pub fn host_in(&mut self, ep: u8, ctx: &mut DevCtx<'_>) -> Result<Vec<u8>, UsbError> {
        let (f, mps) = self.endpoint(ep, true)?;
        if ep == 0 && !self.control_in && self.control_done {
            self.control_done = false;
            self.bits(0x144, 1 << 3);
            self.irq(ctx);
            return Ok(Vec::new());
        }
        let fifo = if f == 4 {
            &mut self.cx
        } else {
            &mut self.fifo[f]
        };
        if !fifo.packet {
            if f == 4 {
                self.bits(0x144, 1 << 1);
            } else {
                self.bits(0x148, 1 << (16 + f));
            }
            self.irq(ctx);
            return Err(UsbError::Nak);
        }
        let bytes: Vec<_> = fifo.bytes.drain(..fifo.bytes.len().min(mps)).collect();
        fifo.packet = !fifo.bytes.is_empty();
        if f == 4 {
            self.bits(0x144, 1 << 1);
        } else {
            self.bits(0x148, 1 << (16 + f));
        }
        self.irq(ctx);
        Ok(bytes)
    }

    fn dma(&mut self, ctx: &mut DevCtx<'_>) {
        let ctrl = self.regs.word(0x1C8);
        if ctrl & 1 == 0 {
            return;
        }
        let target = self.regs.word(0x1C0) & 0x1F;
        let n = (ctrl >> 8 & 0xFFFF) as usize;
        let addr = self.regs.word(0x1CC);
        let start = (addr & 0x0FFF_FFFF) as usize;
        let ram = addr >> 28 == 2 || addr >> 28 == 3;
        let buffer = if ram {
            crate::soc::SRAM_BUFFER
        } else {
            crate::soc::FLASH_BUFFER
        };
        let range = start..start.saturating_add(n);
        let valid = addr >> 28 <= 3
            && target.count_ones() == 1
            && ctx.buffer(buffer).is_some_and(|b| range.end <= b.len())
            && (ctrl & 2 != 0 || ram);
        self.clear(0x1C8, 1);
        if !valid {
            self.bits(0x14C, 1 << 8);
            return;
        }
        let f = target.trailing_zeros() as usize;
        let fifo = if f == 4 {
            &mut self.cx
        } else {
            &mut self.fifo[f]
        };
        if ctrl & 2 != 0 {
            if n > FIFO_CAP - fifo.bytes.len() {
                self.bits(0x14C, 1 << 8);
                return;
            }
            fifo.bytes.extend(&ctx.buffer(buffer).unwrap()[range]);
            fifo.packet = true;
        } else {
            let actual = n.min(fifo.bytes.len());
            let range = start..start + actual;
            let ram = ctx.buffer_mut_range(buffer, range.clone()).unwrap();
            for p in range {
                ram[p] = fifo.bytes.pop_front().unwrap();
            }
            fifo.packet = !fifo.bytes.is_empty();
        }
        if f == 4 {
            self.clear(0x144, 6);
        } else {
            self.clear(0x148, (3 << (f * 2)) | (1 << (16 + f)));
        }
        self.bits(0x14C, 1 << 7);
    }

    /// Evo's per-FIFO DMA extension. BSP 0x127024 programs a count at +0, address at
    /// +4, then START; CX is +0x300 and the four FIFOs start at +0x308, stride 8.
    fn vdma(&mut self, off: u32, ctx: &mut DevCtx<'_>) {
        let ctrl = self.regs.word(off);
        if ctrl & 1 == 0 {
            return;
        }
        let index = ((off - 0x300) / 8) as usize;
        let n = ((ctrl >> 8) & 0x1FFFF) as usize;
        let addr = self.regs.word(off + 4);
        let start = (addr & 0x0FFF_FFFF) as usize;
        let ram = addr >> 28 == 2 || addr >> 28 == 3;
        let setup = index == 0 && !self.setup.is_empty();
        let input = !setup && ctrl & 2 != 0;
        let buffer = if ram {
            crate::soc::SRAM_BUFFER
        } else {
            crate::soc::FLASH_BUFFER
        };
        let valid = addr >> 28 <= 3
            && n <= FIFO_CAP
            && ctx
                .buffer(buffer)
                .is_some_and(|b| start.checked_add(n).is_some_and(|end| end <= b.len()))
            && (input || ram);
        if !valid {
            self.clear(off, 1);
            self.bits(0x328, 1 << (index + 16));
            return;
        }
        if setup {
            let actual = n.min(self.setup.len());
            let range = start..start + actual;
            let ram = ctx.buffer_mut_range(buffer, range.clone()).unwrap();
            for p in range {
                ram[p] = self.setup.pop_front().unwrap();
            }
            self.set(off, ctrl & 0xFE00_00FE);
            self.clear(0x144, 1);
            self.bits(0x328, 1);
            return;
        }
        let fifo = if index == 0 {
            &mut self.cx
        } else {
            &mut self.fifo[index - 1]
        };
        if input {
            if n > FIFO_CAP - fifo.bytes.len() {
                return;
            }
            fifo.bytes
                .extend(&ctx.buffer(buffer).unwrap()[start..start + n]);
            fifo.packet = true;
        } else {
            if !fifo.packet {
                return;
            }
            let available = fifo.bytes.len();
            let actual = n.min(available);
            let range = start..start + actual;
            let ram = ctx.buffer_mut_range(buffer, range.clone()).unwrap();
            for p in range {
                ram[p] = fifo.bytes.pop_front().unwrap();
            }
            fifo.packet = !fifo.bytes.is_empty();
            let mps = if index == 0 {
                64
            } else {
                let ep = ((self.regs.word(0x1A8) >> ((index - 1) * 8)) & 15).max(1);
                (self.regs.word(0x180 + (ep - 1) * 4) & 0x7FF) as usize
            };
            if actual < n && available >= mps && mps != 0 {
                self.set(off, (ctrl & 0xFE00_00FF) | (((n - actual) as u32) << 8));
                self.set(off + 4, addr + actual as u32);
                self.clear(0x148, 3 << ((index.saturating_sub(1)) * 2));
                return;
            }
            self.set(off + 4, addr + actual as u32);
            self.set(off, (ctrl & 0xFE00_00FE) | (((n - actual) as u32) << 8));
        }
        if input {
            self.set(off, ctrl & 0xFE00_00FE);
        }
        if index == 0 {
            self.clear(0x144, 6);
        } else {
            self.clear(0x148, (3 << ((index - 1) * 2)) | (1 << (15 + index)));
        }
        self.bits(0x328, 1 << index);
    }
}

impl Device for Usb {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        log::trace!(target: "evo_hw::usb", "cycles={} read +{off:03x}/{width} word={:08x}", ctx.now, self.status(off & !3));
        if self.regs.get(off, width).is_none() {
            ctx.unhandled(off, width, None);
            return 0;
        }
        if off == 0x1D0 {
            let mut b = [0u8; 4];
            for byte in b.iter_mut().take(width as usize) {
                *byte = self.setup.pop_front().unwrap_or(0);
            }
            if self.setup.is_empty() {
                self.clear(0x144, 1);
                self.irq(ctx);
            }
            return u32::from_le_bytes(b);
        }
        lane_offsets(off, width).fold(0, |v, (shift, o)| {
            v | (((self.status(o & !3) >> ((o & 3) * 8)) & 0xFF) << shift)
        })
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        log::trace!(target: "evo_hw::usb", "cycles={} write +{off:03x}/{width} value={v:08x}", ctx.now);
        let Some(old) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, Some(v));
            return;
        };
        let mut new = v;
        for (shift, o) in lane_offsets(off, width) {
            let keep = if (0xE0..0xE8).contains(&o) {
                0xFF
            } else if o == 0 {
                2
            } else {
                0
            };
            let mask = keep << shift;
            new = (new & !mask) | (old & mask);
            // The Evo revision acknowledges by writing one (BSP ISR
            // 0x126100/0x1263d4), including narrow accesses to upper lanes.
            if (0x144..0x15C).contains(&o) || o & !3 == 0x328 {
                let mask = 0xFF << shift;
                new = (new & !mask) | (old & !v & mask);
            }
        }
        self.regs.set(off, width, new);
        let base = off & !3;
        match base {
            0x100 => {
                if self.regs.word(0x100) & (1 << 4) != 0 {
                    self.abort_transfers();
                }
                self.clear(0x100, 1 << 4);
            }
            0x120 => {
                let command = self.regs.word(0x120);
                if command & 8 != 0 {
                    self.cx = Fifo::default();
                    self.setup.clear();
                }
                if command & 1 != 0 {
                    self.control_done = true;
                }
                self.clear(0x120, 9);
            }
            0x1B0 | 0x1B4 | 0x1B8 | 0x1BC => {
                if self.regs.word(base) & 0x1000 != 0 {
                    self.fifo[((base - 0x1B0) / 4) as usize] = Fifo::default();
                    self.set(base, 0);
                }
            }
            0x1C8 => self.dma(ctx),
            0x300 | 0x308 | 0x310 | 0x318 | 0x320 => self.vdma(base, ctx),
            _ => {}
        }
        self.irq(ctx);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        let attached = self.attached && kind == ResetKind::System;
        *self = Self::new();
        if attached {
            self.attach(true, ctx);
        }
        ctx.set_irq(16, false);
    }
    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }
    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.regs.size() != WINDOW
            || s.setup.len() > 8
            || std::iter::once(&s.cx)
                .chain(&s.fifo)
                .any(|f| f.bytes.len() > FIFO_CAP || (!f.bytes.is_empty() && !f.packet))
        {
            return Err(StateError::Mismatch("USB register/FIFO size".into()));
        }
        *self = s;
        Ok(())
    }
    fn inspect(&self, _now: u64) -> Vec<RegView> {
        [
            0x80, 0xC0, 0xC4, 0x100, 0x104, 0x114, 0x120, 0x130, 0x134, 0x138, 0x13C, 0x140, 0x144,
            0x148, 0x14C, 0x160, 0x164, 0x180, 0x184, 0x1A0, 0x1A8, 0x1AC, 0x1C0, 0x1C8, 0x1CC,
            0x300, 0x304, 0x308, 0x30C, 0x310, 0x314, 0x318, 0x31C, 0x320, 0x324, 0x328, 0x32C,
            0x330, 0x334, 0x338, 0x33C,
        ]
        .into_iter()
        .map(|o| RegView::new(format!("usb_{o:03x}"), o, self.status(o)))
        .collect()
    }
}

/// USB Type-C controller at `0x4010_C000`: TCPCI ALERT/MASK, CC and power status.
/// IRQ 28 follows unmasked alerts; ALERT is write-one-to-clear. Other registers remain
/// storage pending controller research. System reset keeps the connected cable and VBUS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsbTypeC {
    regs: Storage,
    cc1: u8,
    cc2: u8,
}

impl Default for UsbTypeC {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbTypeC {
    /// The controller in its power-on state: storage 0, CC unattached.
    pub fn new() -> Self {
        UsbTypeC {
            regs: Storage::new(WINDOW),
            cc1: 0,
            cc2: 0,
        }
    }

    /// Set the CC1/CC2 attach state the guest reads at `+0x1C` bits 9:8 / 11:10. Each value
    /// is masked to 2 bits.
    pub fn set_cc(&mut self, cc1: u8, cc2: u8) {
        let cc = (cc1 & 3, cc2 & 3);
        if cc != self.cc() {
            self.cc1 = cc.0;
            self.cc2 = cc.1;
            self.alert(1);
        }
    }

    fn alert(&mut self, bits: u32) {
        let pending = self.regs.get(0x10, 2).unwrap_or(0);
        self.regs.set(0x10, 2, pending | bits);
    }

    fn power_status(&mut self, value: u32) {
        let old = self.regs.get(0x1E, 1).unwrap_or(0);
        self.regs.set(0x1E, 1, value);
        if (old ^ value) & self.regs.get(0x14, 1).unwrap_or(0) != 0 {
            self.alert(2);
        }
    }

    /// Recompute the level interrupt after a host changes CC state.
    pub fn refresh_irq(&self, ctx: &mut DevCtx<'_>) {
        let pending = self.regs.get(0x10, 2).unwrap_or(0);
        let mask = self.regs.get(0x12, 2).unwrap_or(0);
        log::trace!(target: "evo_hw::usb", "cycles={} irq28={} alert={pending:04x} mask={mask:04x}", ctx.now, pending & mask != 0);
        ctx.set_irq(28, pending & mask != 0);
    }

    /// Connect a powered computer to CC1. TCPCI CC_STATUS bit 4 indicates that the
    /// calculator presents Rd (sink), and POWER_STATUS bit 2 indicates external VBUS.
    /// This models ordinary USB attachment, not USB-PD message negotiation.
    pub fn attach_host(&mut self, attached: bool, ctx: &mut DevCtx<'_>) {
        let old_term = self.regs.get(TC_CC_BYTE, 1).unwrap_or(0);
        let term = if attached { 0x10 } else { 0 };
        self.regs.set(TC_CC_BYTE, 1, term);
        self.set_cc(u8::from(attached), 0);
        if old_term != term {
            self.alert(1);
        }
        let power = self.regs.get(0x1E, 1).unwrap_or(0);
        self.power_status(if attached { power | 4 } else { power & !5 });
        self.refresh_irq(ctx);
    }

    /// The current host CC1/CC2 state.
    pub fn cc(&self) -> (u8, u8) {
        (self.cc1, self.cc2)
    }

    /// The low nibble of byte `+0x1D`: `cc1 | cc2 << 2`.
    fn cc_nibble(&self) -> u8 {
        self.cc1 | (self.cc2 << 2)
    }
}

impl Device for UsbTypeC {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        let Some(mut v) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, None);
            return 0;
        };
        for (shift, o) in lane_offsets(off, width) {
            if o == TC_CC_BYTE {
                let m = u32::from(TC_CC_MASK) << shift;
                v = (v & !m) | (u32::from(self.cc_nibble()) << shift);
            }
        }
        v
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        let Some(old) = self.regs.get(off, width) else {
            ctx.unhandled(off, width, Some(v));
            return;
        };
        let mut new = v;
        for (shift, o) in lane_offsets(off, width) {
            if (0x10..0x12).contains(&o) {
                let mask = 0xFF << shift;
                new = (new & !mask) | (old & !v & mask);
            }
            if o == TC_CC_BYTE {
                // The stored CC bits stay 0; reads take them from the host state.
                new &= !(u32::from(TC_CC_MASK) << shift);
            }
        }
        self.regs.set(off, width, new);
        for (shift, o) in lane_offsets(off, width) {
            if o == 0x23 {
                if (v >> shift) & 0xFF == 0x99 && self.cc() == (0, 0) {
                    // Look4Connection is asynchronous. Reporting a settled zero here
                    // makes the BSP leave its initial DRP search before a cable arrives.
                    self.regs.set(TC_CC_BYTE, 1, 0x20);
                }
                let power = self.regs.get(0x1E, 1).unwrap_or(0);
                let power = match (v >> shift) & 0xFF {
                    0x22 => power & !8, // Disable VBUS detection.
                    0x33 => power | 8,  // Enable VBUS detection.
                    0x44 => power & !1, // Disable sinking VBUS.
                    0x55 => power | 1,  // Sink VBUS.
                    _ => power,
                };
                self.power_status(power);
            }
        }
        self.refresh_irq(ctx);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        let term = self.regs.get(TC_CC_BYTE, 1).unwrap_or(0) & 0x10;
        let vbus = self.regs.get(0x1E, 1).unwrap_or(0) & 4;
        self.regs.clear();
        if kind == ResetKind::PowerOn {
            self.cc1 = 0;
            self.cc2 = 0;
        } else {
            self.regs.set(TC_CC_BYTE, 1, term);
            self.regs.set(0x1E, 1, vbus);
            if self.cc() != (0, 0) {
                self.alert(1);
            }
        }
        self.refresh_irq(ctx);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(self)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut s = self.clone();
        load_serde(&mut s, state)?;
        if s.regs.size() != WINDOW {
            return Err(StateError::Mismatch(format!(
                "Type-C register storage of {} bytes, expected {WINDOW}",
                s.regs.size()
            )));
        }
        s.cc1 &= 3;
        s.cc2 &= 3;
        *self = s;
        Ok(())
    }
}
