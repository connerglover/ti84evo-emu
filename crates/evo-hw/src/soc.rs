//! The TI-84 Evo SoC: every peripheral at its base address (see `docs/hardware/memory-map.md`).

use crate::MemoryMap;

/// Name of the flash buffer (the `MemoryMap` ROM a flash controller writes through
/// `DevCtx::buffer_mut`).
pub const FLASH_BUFFER: &str = "flash";
/// Name of the SRAM buffer (the `MemoryMap` RAM a display controller reads frames from).
pub const SRAM_BUFFER: &str = "sram";

/// Add every SoC device to `map`, which already holds flash and SRAM under [`FLASH_BUFFER`]
/// and [`SRAM_BUFFER`]. Devices are added in a fixed order (their ids and save-state order).
pub fn attach(map: &mut MemoryMap) {
    // No peripheral is modeled yet; they are appended here one by one.
    let _ = map;
}
