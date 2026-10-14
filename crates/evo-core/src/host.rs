//! Host APIs on [`Machine`]: what a front end (CLI, GUI) feeds into the machine and reads out
//! of it besides running the CPU. Every API that touches a device goes through
//! `Machine::with_dev`, so the device sees the current time and its IRQ changes reach the CPU.

use evo_hw::dev::uart::Uart16550;
use evo_hw::dev::usb::UsbTypeC;

use crate::Machine;

/// One of the SoC's UARTs (`docs/hardware/uart.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UartPort {
    /// `0x4010_6000`: the TF-M and BSP log, C-library stdio.
    Uart0,
    /// `0x4010_B000`: an interrupt-driven link transport.
    Uart2,
}

impl UartPort {
    /// The device's name in the memory map (`evo_hw::soc::attach`).
    fn device(self) -> &'static str {
        match self {
            UartPort::Uart0 => "uart0",
            UartPort::Uart2 => "uart2",
        }
    }
}

impl Machine {
    /// Drain the bytes the guest transmitted on `port` since the last call, oldest first.
    /// At most `evo_hw::dev::uart::UART_TX_CAP` undrained bytes are kept.
    pub fn take_uart_tx(&mut self, port: UartPort) -> Vec<u8> {
        self.with_dev(port.device(), |u: &mut Uart16550, _| u.take_tx())
            .unwrap_or_default()
    }

    /// Bytes arriving on `port`'s RX wire. The guest receives them through the RX FIFO, in
    /// order; UART2 raises IRQ 27 if the guest enabled the RX interrupt.
    pub fn inject_uart_rx(&mut self, port: UartPort, bytes: &[u8]) {
        self.with_dev(port.device(), |u: &mut Uart16550, ctx| {
            u.inject_rx(bytes, ctx)
        });
    }

    /// Set the USB Type-C CC1/CC2 attach state (`docs/hardware/usb.md`), each masked to 2
    /// bits; 0 = unattached. The guest reads it at `0x4010_C01C` bits 9:8 / 11:10. It survives
    /// a System reset (the cable stays plugged in).
    pub fn set_usb_cc(&mut self, cc1: u8, cc2: u8) {
        self.with_dev("usbtc", |t: &mut UsbTypeC, _| t.set_cc(cc1, cc2));
    }
}
