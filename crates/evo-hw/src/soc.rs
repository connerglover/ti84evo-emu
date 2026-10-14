//! The TI-84 Evo SoC: every peripheral at its base address (see `docs/hardware/memory-map.md`).

use crate::MemoryMap;
use crate::dev::crypto::{Sha256Engine, TRNG_SEED, Trng};
use crate::dev::gpio::Gpio;
use crate::dev::keypad::Keypad;
use crate::dev::lcdc::{Lcdc, Pwm};
use crate::dev::mpc::Mpc;
use crate::dev::spi::{DEFAULT_PANEL_ID, Spi, St7789};
use crate::dev::spif::Spif;
use crate::dev::storage::Storage;
use crate::dev::sysctl::{Cru, Spctrl};
use crate::dev::timer::Timer;
use crate::dev::uart::Uart16550;
use crate::dev::usb::{Usb, UsbTypeC};

/// Name of the flash buffer (the `MemoryMap` ROM a flash controller writes through
/// `DevCtx::buffer_mut`).
pub const FLASH_BUFFER: &str = "flash";
/// Name of the SRAM buffer (the `MemoryMap` RAM a display controller reads frames from).
pub const SRAM_BUFFER: &str = "sram";

/// Add every SoC device to `map`, which already holds flash and SRAM under [`FLASH_BUFFER`]
/// and [`SRAM_BUFFER`]. Devices are added in a fixed order (their ids and save-state order).
pub fn attach(map: &mut MemoryMap) {
    // SIE-200 MPCs (docs/hardware/security.md §1): secure-only windows, 4 KiB blocks.
    // 512 KiB SRAM banks = 128 blocks = 4 LUT words; 16 MiB flash = 4096 blocks = 128 words.
    map.add_device("mpc_sram0", 0x5020_4000, 0x1000, Box::new(Mpc::new(3)));
    map.add_device("mpc_sram1", 0x5020_5000, 0x1000, Box::new(Mpc::new(3)));
    map.add_device("mpc_sram2", 0x5020_6000, 0x1000, Box::new(Mpc::new(3)));
    map.add_device("mpc_flash", 0x5020_8000, 0x1000, Box::new(Mpc::new(127)));
    // System control (security.md §2, §3, §6) and the XIP instruction cache (spif-nor.md §7).
    map.add_device("cru", 0x5020_0000, 0x1000, Box::new(Cru::new()));
    map.add_device("spctrl", 0x5020_1000, 0x1000, Box::new(Spctrl::new()));
    map.add_device("ledpat", 0x5020_2B00, 0x100, Box::new(Storage::new(0x100)));
    map.add_device(
        "icache",
        0x5000_5000,
        0x1000,
        Box::new(Storage::new(0x1000)),
    );
    // TIMER (timer.md): 3 down-counters; the firmware uses the NS alias only.
    map.add_device("timer", 0x4010_1000, 0x1000, Box::new(Timer::new()));
    // UARTs (uart.md §1, §8): the firmware uses the NS aliases only.
    map.add_device("uart0", 0x4010_6000, 0x1000, Box::new(Uart16550::new(17)));
    map.add_device("uart2", 0x4010_B000, 0x1000, Box::new(Uart16550::new(27)));
    // SPI master and the LCD panel on its bus (spi-panel.md): NS alias only, no IRQ.
    map.add_device(
        "spi",
        0x4010_7000,
        0x1000,
        Box::new(Spi::new(St7789::new(DEFAULT_PANEL_ID))),
    );
    // SPIF QSPI controller over the NOR part (spif-nor.md): secure alias only, no IRQ. It
    // erases and programs the FLASH_BUFFER bytes the XIP windows read.
    map.add_device("spif", 0x5000_1000, 0x1000, Box::new(Spif::new()));
    // Polled crypto blocks (crypto.md): secure aliases only, IRQs 26/23 never asserted.
    map.add_device("trng", 0x5010_A000, 0x1000, Box::new(Trng::new(TRNG_SEED)));
    map.add_device("sha", 0x5000_2000, 0x1000, Box::new(Sha256Engine::new()));
    // USB OTG minimum and the Type-C stub (usb.md): NS BSP only, IRQs 16/28 never asserted.
    map.add_device("usb", 0x4000_3000, 0x1000, Box::new(Usb::new()));
    map.add_device("usbtc", 0x4010_C000, 0x1000, Box::new(UsbTypeC::new()));
    // GPIO (gpio.md): 13 pins, IRQ 24; pin 1 is the power hold. NS alias, used by S and NS.
    map.add_device("gpio", 0x4010_5000, 0x1000, Box::new(Gpio::new()));
    // PL111 LCDC (lcdc.md): VComp IRQ 14 once per frame; Machine::screen decodes its
    // framebuffer from SRAM. The backlight PWM next to it is plain storage, no IRQ.
    map.add_device("lcdc", 0x4000_4000, 0x1000, Box::new(Lcdc::new()));
    map.add_device("pwm", 0x4010_4000, 0x1000, Box::new(Pwm::new()));
    // Keypad scanner (keypad.md): IRQ 13 for the matrix; the ON key pulses IRQ 0.
    map.add_device("keypad", 0x4010_3000, 0x1000, Box::new(Keypad::new()));
}
