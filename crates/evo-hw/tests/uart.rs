//! UART0 / UART2 16550 model (`docs/hardware/uart.md` §8).

use evo_hw::dev::uart::{UART_TX_CAP, Uart16550};
use evo_hw::{DevHarness, Device, ResetKind};

const RBR_THR: u32 = 0x00;
const IER: u32 = 0x04;
const FCR_IIR: u32 = 0x08;
const LCR: u32 = 0x0C;
const MCR: u32 = 0x10;
const LSR: u32 = 0x14;
const MSR: u32 = 0x18;
const SCR: u32 = 0x1C;

const UART0_IRQ: u16 = 17;
const UART2_IRQ: u16 = 27;

/// The secure `uart_init` sequence for 115200 baud at 24 MHz (uart.md §2): DLAB on,
/// DLH = 0, DLL = 13, 8N1, DLAB off, FCR = 7.
fn init(h: &mut DevHarness, u: &mut Uart16550) {
    let lcr = h.read(u, LCR, 4);
    h.write(u, LCR, 4, lcr | 0x80);
    h.write(u, IER, 4, 0); // DLH
    h.write(u, RBR_THR, 4, 13); // DLL
    for f in [
        |l: u32| l & !8,
        |l: u32| l & !4,
        |l: u32| l | 3,
        |l: u32| l & !0x80,
    ] {
        let lcr = h.read(u, LCR, 4);
        h.write(u, LCR, 4, f(lcr));
    }
    h.write(u, FCR_IIR, 4, 7);
}

/// Inject `bytes` from the host with the harness's context.
fn inject(h: &mut DevHarness, u: &mut Uart16550, bytes: &[u8]) {
    h.with_ctx(|ctx| u.inject_rx(bytes, ctx));
}

#[test]
fn init_sequence() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    init(&mut h, &mut u);
    assert_eq!(u.divisor(), 13);
    assert_eq!(u.lcr(), 3);
    // With DLAB clear, +0x00 is THR again.
    assert_eq!(h.read(&mut u, LSR, 4) & 0x20, 0x20);
    h.write(&mut u, RBR_THR, 4, u32::from(b'B'));
    assert_eq!(u.take_tx(), b"B");
    assert_eq!(u.take_tx(), b"");
    assert_eq!(h.irq, 0);
    assert!(h.log.is_empty());
}

#[test]
fn lsr_and_rx() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
    inject(&mut h, &mut u, b"x");
    assert_eq!(h.read(&mut u, LSR, 4), 0x61);
    assert_eq!(h.read(&mut u, RBR_THR, 4), u32::from(b'x'));
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
    // An empty FIFO reads 0.
    assert_eq!(h.read(&mut u, RBR_THR, 4), 0);
}

#[test]
fn uart2_irq_level() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART2_IRQ);
    init(&mut h, &mut u);
    h.write(&mut u, IER, 4, 1);
    assert_eq!(h.irq, 0);
    inject(&mut h, &mut u, b"ab");
    assert_eq!(h.irq, 1 << 27);
    // IIR: RX data available, FIFOs enabled.
    assert_eq!(h.read(&mut u, FCR_IIR, 4), 0xC4);
    h.write(&mut u, IER, 4, 0);
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut u, FCR_IIR, 4), 0xC1);
    h.write(&mut u, IER, 4, 1);
    assert_eq!(h.irq, 1 << 27);
    assert_eq!(h.read(&mut u, RBR_THR, 4), u32::from(b'a'));
    assert_eq!(h.irq, 1 << 27);
    assert_eq!(h.read(&mut u, RBR_THR, 4), u32::from(b'b'));
    assert_eq!(h.irq, 0);
}

#[test]
fn thre_interrupt_enable_asserts() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART2_IRQ);
    h.write(&mut u, IER, 4, 2);
    assert_eq!(h.irq, 1 << 27);
    assert_eq!(h.read(&mut u, FCR_IIR, 4), 0x02);
    assert_eq!(h.read(&mut u, IER, 4), 2);
    // Only the low 4 bits of IER exist.
    h.write(&mut u, IER, 4, 0xF0);
    assert_eq!(h.read(&mut u, IER, 4), 0);
    assert_eq!(h.irq, 0);
}

#[test]
fn fcr_clears_rx() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    inject(&mut h, &mut u, b"abc");
    assert_eq!(h.read(&mut u, LSR, 4), 0x61);
    h.write(&mut u, FCR_IIR, 4, 7);
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
}

#[test]
fn host_rx_refills_fifo() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    let bytes: Vec<u8> = (0..40u8).map(|i| b'0' + i).collect();
    inject(&mut h, &mut u, &bytes);
    let got: Vec<u8> = (0..40)
        .map(|_| {
            assert_eq!(h.read(&mut u, LSR, 4) & 1, 1);
            h.read(&mut u, RBR_THR, 4) as u8
        })
        .collect();
    assert_eq!(got, bytes);
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
}

#[test]
fn tx_cap_drops_oldest() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    for i in 0..=UART_TX_CAP {
        h.write(&mut u, RBR_THR, 4, i as u32 & 0xFF);
    }
    let tx = u.take_tx();
    assert_eq!(tx.len(), UART_TX_CAP);
    assert_eq!(tx[0], 1);
    assert_eq!(*tx.last().unwrap(), UART_TX_CAP as u8);
}

#[test]
fn uart0_never_irq_without_ier() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    init(&mut h, &mut u);
    inject(&mut h, &mut u, b"hello");
    h.write(&mut u, RBR_THR, 4, u32::from(b'x'));
    while h.read(&mut u, LSR, 4) & 1 != 0 {
        h.read(&mut u, RBR_THR, 4);
    }
    assert_eq!(h.irq, 0);
    assert_eq!(h.read(&mut u, FCR_IIR, 4), 0xC1);
}

#[test]
fn other_registers() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    h.write(&mut u, MCR, 4, 0x03);
    h.write(&mut u, SCR, 4, 0x5A);
    assert_eq!(h.read(&mut u, MCR, 4), 0x03);
    assert_eq!(h.read(&mut u, SCR, 4), 0x5A);
    assert_eq!(h.read(&mut u, MSR, 4), 0xB0);
    // LSR and MSR ignore writes.
    h.write(&mut u, LSR, 4, 0xFF);
    h.write(&mut u, MSR, 4, 0xFF);
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
    assert!(h.log.is_empty());
    // Only bits 7:0 are written; with DLAB set, +0x00/+0x04 are DLL/DLH.
    h.write(&mut u, LCR, 4, 0x1_83);
    assert_eq!(h.read(&mut u, LCR, 4), 0x83);
    h.write(&mut u, RBR_THR, 4, 0x34);
    h.write(&mut u, IER, 4, 0x12);
    assert_eq!(h.read(&mut u, RBR_THR, 4), 0x34);
    assert_eq!(h.read(&mut u, IER, 4), 0x12);
    assert_eq!(u.divisor(), 0x1234);
    assert_eq!(u.take_tx(), b"");
    assert_eq!(h.irq, 0);
}

#[test]
fn unmodeled_offsets_read_0_and_are_logged() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    assert_eq!(h.read(&mut u, 0x7C, 4), 0);
    h.write(&mut u, 0xFFC, 4, 0xDEAD_BEEF);
    assert_eq!(h.read(&mut u, 0x20, 1), 0);
    assert_eq!(h.log.len(), 3);
    assert_eq!(u.take_tx(), b"");
}

#[test]
fn system_reset_clears_registers_keeps_tx() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART2_IRQ);
    init(&mut h, &mut u);
    h.write(&mut u, SCR, 4, 0x77);
    h.write(&mut u, RBR_THR, 4, u32::from(b'T'));
    h.write(&mut u, IER, 4, 1);
    // 20 bytes: 16 in the FIFO, 4 still waiting on the host side.
    let bytes: Vec<u8> = (0..20u8).collect();
    inject(&mut h, &mut u, &bytes);
    assert_eq!(h.irq, 1 << 27);
    h.reset(&mut u, ResetKind::System);
    assert_eq!(h.irq, 0);
    assert_eq!(u.divisor(), 0);
    assert_eq!(u.lcr(), 0);
    assert_eq!(h.read(&mut u, IER, 4), 0);
    assert_eq!(h.read(&mut u, SCR, 4), 0);
    assert_eq!(h.read(&mut u, FCR_IIR, 4), 0x01);
    // The FIFO's 16 bytes are gone; the 4 host bytes not yet received remain.
    let mut rest = Vec::new();
    while h.read(&mut u, LSR, 4) & 1 != 0 {
        rest.push(h.read(&mut u, RBR_THR, 4) as u8);
    }
    assert_eq!(rest, [16, 17, 18, 19]);
    assert_eq!(u.take_tx(), b"T");
}

#[test]
fn power_on_reset_clears_everything() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART0_IRQ);
    h.write(&mut u, RBR_THR, 4, u32::from(b'T'));
    inject(&mut h, &mut u, &[0; 20]);
    h.reset(&mut u, ResetKind::PowerOn);
    assert_eq!(h.read(&mut u, LSR, 4), 0x60);
    assert_eq!(u.take_tx(), b"");
}

#[test]
fn state_roundtrip() {
    let mut h = DevHarness::new();
    let mut u = Uart16550::new(UART2_IRQ);
    init(&mut h, &mut u);
    h.write(&mut u, IER, 4, 1);
    h.write(&mut u, SCR, 4, 0x42);
    h.write(&mut u, RBR_THR, 4, u32::from(b'Q'));
    let bytes: Vec<u8> = (0..30u8).collect();
    inject(&mut h, &mut u, &bytes);
    let st = u.save();

    let mut fresh = Uart16550::new(UART2_IRQ);
    fresh.load(&st).unwrap();
    assert_eq!(fresh.divisor(), 13);
    assert_eq!(fresh.lcr(), 3);
    assert_eq!(h.read(&mut fresh, SCR, 4), 0x42);
    assert_eq!(h.read(&mut fresh, IER, 4), 1);
    assert_eq!(fresh.take_tx(), b"Q");
    let got: Vec<u8> = (0..30)
        .map(|_| h.read(&mut fresh, RBR_THR, 4) as u8)
        .collect();
    assert_eq!(got, bytes);

    // A bad state is an error and leaves the device unchanged.
    assert!(u.load(&[0xFF; 3]).is_err());
    assert_eq!(u.divisor(), 13);
    assert_eq!(u.take_tx(), b"Q");
}
