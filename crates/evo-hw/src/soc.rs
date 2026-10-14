//! The TI-84 Evo SoC: every peripheral at its base address (see `docs/hardware/memory-map.md`).

use crate::MemoryMap;
use crate::dev::mpc::Mpc;
use crate::dev::storage::Storage;
use crate::dev::sysctl::{Cru, Spctrl};
use crate::dev::timer::Timer;

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
}
