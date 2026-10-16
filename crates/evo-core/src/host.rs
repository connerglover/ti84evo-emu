//! Host APIs on [`Machine`]: what a front end (CLI, GUI) feeds into the machine and reads out
//! of it besides running the CPU. Every API that touches a device goes through
//! `Machine::with_dev`, so the device sees the current time and its IRQ changes reach the CPU.

use evo_hw::dev::gpio::Gpio;
use evo_hw::dev::keypad::{Key, Keypad};
use evo_hw::dev::lcdc::{Frame, Lcdc};
use evo_hw::dev::sysctl::{CRU_WAKE_ON_KEY, Cru, Spctrl};
use evo_hw::dev::uart::Uart16550;
pub use evo_hw::dev::usb::UsbError;
use evo_hw::dev::usb::{Usb, UsbTypeC};

use evo_hw::soc::SRAM_BUFFER;

use crate::Machine;
use crate::machine::SRAM_NS;

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
    /// Connect or disconnect a USB host. Enumeration is performed by subsequent bus tokens.
    pub fn usb_attach(&mut self, attached: bool) {
        self.with_dev("usbtc", |t: &mut UsbTypeC, ctx| {
            t.attach_host(attached, ctx)
        });
        self.with_dev("usb", |u: &mut Usb, ctx| u.attach(attached, ctx));
    }

    /// Reset the USB bus at full speed; the guest handles the reset interrupt.
    pub fn usb_bus_reset(&mut self) -> Result<(), UsbError> {
        self.with_dev("usb", |u: &mut Usb, ctx| u.bus_reset(ctx))
            .unwrap()
    }

    /// Deliver a control SETUP packet to the guest controller.
    pub fn usb_setup(&mut self, setup: [u8; 8]) -> Result<(), UsbError> {
        self.with_dev("usb", |u: &mut Usb, ctx| u.host_setup(setup, ctx))
            .unwrap()
    }

    /// Send one OUT packet (endpoint number, without the direction bit).
    pub fn usb_out(&mut self, endpoint: u8, bytes: &[u8]) -> Result<(), UsbError> {
        self.with_dev("usb", |u: &mut Usb, ctx| u.host_out(endpoint, bytes, ctx))
            .unwrap()
    }

    /// Issue an IN token (endpoint number, without the direction bit).
    pub fn usb_in(&mut self, endpoint: u8) -> Result<Vec<u8>, UsbError> {
        self.with_dev("usb", |u: &mut Usb, ctx| u.host_in(endpoint, ctx))
            .unwrap()
    }

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
        self.with_dev("usbtc", |t: &mut UsbTypeC, ctx| {
            t.set_cc(cc1, cc2);
            t.refresh_irq(ctx);
        });
    }

    /// Drive GPIO input `pin` (`docs/hardware/gpio.md`): `Some(level)`, or `None` = undriven
    /// (its pull, else low). Pins >= `evo_hw::dev::gpio::GPIO_PINS` are ignored. An edge or
    /// level the guest armed raises IRQ 24 and wakes a sleeping CPU. The levels survive a
    /// System reset.
    pub fn set_gpio_input(&mut self, pin: u8, level: Option<bool>) {
        self.with_dev("gpio", |g: &mut Gpio, ctx| g.set_input(pin, level, ctx));
    }

    /// Press or release `key` (`docs/hardware/keypad.md`). A matrix key latches in its row
    /// register, sets the key-down status and schedules a scan (IRQ 13 when the guest unmasked
    /// it); the ON key pulses IRQ 0 on press. Held keys survive a System reset. A matrix
    /// position outside 0..8 is logged and ignored.
    ///
    /// Pressing ON while the guest has armed deep sleep (SPCTRL +0xD4 bit 4: the OS switched
    /// the unit off, e.g. by auto power down) is the wake-up source as well: it sets the CRU
    /// WAKE_STATUS ON bit and ends the CPU's sleep, so the OS resumes where it stopped
    /// (`docs/hardware/security.md` §3, "Wake from deep sleep").
    pub fn set_key(&mut self, key: Key, pressed: bool) {
        self.with_dev("keypad", |k: &mut Keypad, ctx| k.set_key(key, pressed, ctx));
        let off = || {
            self.bus
                .device::<Spctrl>("spctrl")
                .is_some_and(Spctrl::deep_sleep_armed)
        };
        if key == Key::On && pressed && off() {
            self.with_dev("cru", |c: &mut Cru, _| c.set_wake(CRU_WAKE_ON_KEY));
            self.wake();
        }
    }

    /// True once after the guest released the power hold (GPIO pin 1, an output, went from
    /// high to low: the unit would switch itself off); the report is drained.
    pub fn take_power_off(&mut self) -> bool {
        self.with_dev("gpio", |g: &mut Gpio, _| g.take_power_off())
            .unwrap_or(false)
    }

    /// The image the LCD shows now (`docs/hardware/lcdc.md`), decoded from the framebuffer
    /// the PL111 LCDC points at: `None` while the LCD is off, in a pixel format other than
    /// RGB565, or pointing outside SRAM. Reading it changes nothing.
    pub fn screen(&self) -> Option<Frame> {
        let sram = self.bus.region_bytes(SRAM_BUFFER)?;
        self.bus.device::<Lcdc>("lcdc")?.frame(sram, SRAM_NS)
    }
}
