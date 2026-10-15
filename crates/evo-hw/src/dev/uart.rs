//! 16550-style UARTs (`docs/hardware/uart.md` §8): UART0 at `0x4010_6000` (IRQ 17, TF-M and
//! BSP log, polled) and UART2 at `0x4010_B000` (IRQ 27, interrupt-driven RX link transport).
//!
//! Registers are decoded at `off & !3` for any access width; only bits 7:0 carry meaning.
//! Transmit is instant (LSR.THRE is always 1); transmitted bytes collect in a host-drained
//! buffer of at most [`UART_TX_CAP`] bytes. Received bytes come from the host: they wait in an
//! unbounded host queue and move into the 16-byte hardware FIFO whenever it has room.
//!
//! The IRQ line is level-sensitive and re-evaluated after every access and injection:
//! asserted while `(IER.ERBFI && RX FIFO non-empty) || IER.ETBEI`.
//!
//! A System reset clears the registers and the RX FIFO but keeps the bytes the host queued
//! and not yet received, and the transmitted bytes not yet drained (they belong to the host
//! side of the wire). A PowerOn reset clears everything.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::{DevCtx, Device, ResetKind, StateError, load_serde, save_serde};

/// Undrained TX bytes kept; when more arrive, the oldest are dropped.
pub const UART_TX_CAP: usize = 1 << 20;
/// Depth of the hardware RX FIFO.
const RX_FIFO: usize = 16;

const RBR_THR_DLL: u32 = 0x00;
const IER_DLH: u32 = 0x04;
const IIR_FCR: u32 = 0x08;
const LCR: u32 = 0x0C;
const MCR: u32 = 0x10;
const LSR: u32 = 0x14;
const MSR: u32 = 0x18;
const SCR: u32 = 0x1C;

/// LCR divisor latch access bit.
const DLAB: u8 = 0x80;
/// LSR: THR empty | transmitter empty (TX is instant).
const LSR_IDLE: u32 = 0x60;
/// MSR: CTS | DSR | DCD.
const MSR_VALUE: u32 = 0xB0;

/// Everything a save state carries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Regs {
    ier: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlh: u8,
    fifo_en: bool,
    /// The hardware RX FIFO (at most [`RX_FIFO`] bytes).
    rx: VecDeque<u8>,
    /// Host bytes not yet in the FIFO. Invariant: non-empty only while `rx` is full.
    host_rx: VecDeque<u8>,
    /// Transmitted bytes not yet drained by the host (at most [`UART_TX_CAP`]).
    tx: VecDeque<u8>,
}

/// One 16550 UART driving IRQ line `irq`.
#[derive(Clone, Debug)]
pub struct Uart16550 {
    irq: u16,
    regs: Regs,
}

impl Uart16550 {
    /// A UART in its power-on state on IRQ line `irq`.
    pub fn new(irq: u16) -> Self {
        Uart16550 {
            irq,
            regs: Regs::default(),
        }
    }

    /// Drain the transmitted bytes, oldest first.
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.regs.tx).into()
    }

    /// Bytes arriving on the RX wire from the host: queued, then moved into the FIFO while it
    /// has room. Raises the IRQ line if ERBFI is set.
    pub fn inject_rx(&mut self, bytes: &[u8], ctx: &mut DevCtx<'_>) {
        self.regs.host_rx.extend(bytes);
        self.refill();
        self.update_irq(ctx);
    }

    /// The baud-rate divisor latch (`DLH:DLL`).
    pub fn divisor(&self) -> u16 {
        u16::from_le_bytes([self.regs.dll, self.regs.dlh])
    }

    /// The line control register.
    pub fn lcr(&self) -> u8 {
        self.regs.lcr
    }

    fn dlab(&self) -> bool {
        self.regs.lcr & DLAB != 0
    }

    /// Move host bytes into the FIFO while it has room.
    fn refill(&mut self) {
        let r = &mut self.regs;
        while r.rx.len() < RX_FIFO {
            let Some(b) = r.host_rx.pop_front() else {
                break;
            };
            r.rx.push_back(b);
        }
    }

    fn rx_irq(&self) -> bool {
        self.regs.ier & 1 != 0 && !self.regs.rx.is_empty()
    }

    fn update_irq(&self, ctx: &mut DevCtx<'_>) {
        let asserted = self.rx_irq() || self.regs.ier & 2 != 0;
        ctx.set_irq(self.irq, asserted);
    }

    fn transmit(&mut self, b: u8) {
        let tx = &mut self.regs.tx;
        if tx.len() >= UART_TX_CAP {
            tx.pop_front();
        }
        tx.push_back(b);
    }

    /// A register read at word offset `reg`, or `None` if it is not a register.
    fn read_reg(&mut self, reg: u32) -> Option<u32> {
        let r = &self.regs;
        Some(match reg {
            RBR_THR_DLL if self.dlab() => u32::from(r.dll),
            RBR_THR_DLL => {
                let b = self.regs.rx.pop_front().unwrap_or(0);
                self.refill();
                u32::from(b)
            }
            IER_DLH if self.dlab() => u32::from(r.dlh),
            IER_DLH => u32::from(r.ier & 0x0F),
            IIR_FCR => {
                let id = if self.rx_irq() {
                    0x04
                } else if r.ier & 2 != 0 {
                    0x02
                } else {
                    0x01
                };
                id | if r.fifo_en { 0xC0 } else { 0 }
            }
            LCR => u32::from(r.lcr),
            MCR => u32::from(r.mcr),
            LSR => LSR_IDLE | u32::from(!r.rx.is_empty()),
            MSR => MSR_VALUE,
            SCR => u32::from(r.scr),
            _ => return None,
        })
    }

    /// A register write of byte `v` at word offset `reg`; false if it is not a register.
    fn write_reg(&mut self, reg: u32, v: u8) -> bool {
        let dlab = self.dlab();
        let r = &mut self.regs;
        match reg {
            RBR_THR_DLL if dlab => r.dll = v,
            RBR_THR_DLL => self.transmit(v),
            IER_DLH if dlab => r.dlh = v,
            IER_DLH => r.ier = v & 0x0F,
            IIR_FCR => {
                r.fifo_en = v & 1 != 0;
                if v & 2 != 0 {
                    // RX FIFO reset; bytes still on the host side arrive afterwards.
                    r.rx.clear();
                    self.refill();
                }
            }
            LCR => r.lcr = v,
            MCR => r.mcr = v,
            SCR => r.scr = v,
            LSR | MSR => {}
            _ => return false,
        }
        true
    }
}

impl Device for Uart16550 {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32 {
        let v = match self.read_reg(off & !3) {
            Some(v) => v,
            None => {
                ctx.unhandled(off, width, None);
                0
            }
        };
        self.update_irq(ctx);
        v
    }

    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>) {
        if !self.write_reg(off & !3, v as u8) {
            ctx.unhandled(off, width, Some(v));
        }
        self.update_irq(ctx);
    }

    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>) {
        let r = &mut self.regs;
        match kind {
            // The host side of the wire survives a System reset (module doc).
            ResetKind::System => {
                let host_rx = std::mem::take(&mut r.host_rx);
                let tx = std::mem::take(&mut r.tx);
                *r = Regs {
                    host_rx,
                    tx,
                    ..Regs::default()
                };
                self.refill();
            }
            ResetKind::PowerOn => *r = Regs::default(),
        }
        self.update_irq(ctx);
    }

    fn save(&self) -> Vec<u8> {
        save_serde(&self.regs)
    }

    fn load(&mut self, state: &[u8]) -> Result<(), StateError> {
        let mut regs = self.regs.clone();
        load_serde(&mut regs, state)?;
        // The invariants `refill` and `write` keep: the FIFO holds at most RX_FIFO bytes, host
        // bytes wait only while it is full, TX holds at most UART_TX_CAP bytes.
        if regs.rx.len() > RX_FIFO
            || (!regs.host_rx.is_empty() && regs.rx.len() < RX_FIFO)
            || regs.tx.len() > UART_TX_CAP
        {
            return Err(StateError::Mismatch("UART state out of range".into()));
        }
        self.regs = regs;
        Ok(())
    }
}
