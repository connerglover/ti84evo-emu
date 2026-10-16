//! RAM images: the whole SRAM (three 512 KiB banks, in address order) behind a 12-byte header,
//! for the debugger's "export / import RAM".
//!
//! Layout: [`RAM_MAGIC`], [`RAM_VERSION`] as u16 LE, the body length as u32 LE, then the body.
//! An image holds only SRAM bytes: CPU, devices and flash are not in it (a save state carries
//! those), so importing one is like the debugger writing every SRAM byte at once.

use evo_hw::soc::SRAM_BUFFER;

use crate::Machine;
use crate::machine::SRAM_SIZE;

/// First bytes of a RAM image.
pub const RAM_MAGIC: &[u8; 6] = b"EVORAM";
/// The RAM image format this build writes and reads.
pub const RAM_VERSION: u16 = 1;
/// Header length: magic, version (u16 LE), body size (u32 LE).
pub const RAM_HEADER_LEN: usize = 12;

/// Why [`Machine::import_ram`] refused an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RamError {
    /// The bytes do not start with a whole header with [`RAM_MAGIC`].
    #[error("not a RAM image")]
    BadMagic,
    #[error("RAM image version {0} is not supported")]
    Version(u16),
    /// The header's size, or else the body's length, is not this machine's SRAM size.
    #[error("RAM image holds {found} bytes, this machine has {expected}")]
    Size { expected: usize, found: usize },
}

impl Machine {
    /// The SRAM as a RAM image: header plus 1.5 MiB. Reading it changes nothing.
    pub fn export_ram(&self) -> Vec<u8> {
        let sram = self.sram();
        let mut out = Vec::with_capacity(RAM_HEADER_LEN + sram.len());
        out.extend_from_slice(RAM_MAGIC);
        out.extend_from_slice(&RAM_VERSION.to_le_bytes());
        // The SRAM is 1.5 MiB: its length fits u32.
        out.extend_from_slice(&(sram.len() as u32).to_le_bytes());
        out.extend_from_slice(sram);
        out
    }

    /// Replace the whole SRAM with a RAM image's body. The header must carry [`RAM_MAGIC`] and
    /// [`RAM_VERSION`], and its size, like the body's length, must be the SRAM size. On any
    /// error the machine is unchanged. The CPU drops code it cached from SRAM.
    pub fn import_ram(&mut self, bytes: &[u8]) -> Result<(), RamError> {
        let header = bytes
            .get(..RAM_HEADER_LEN)
            .filter(|h| h.starts_with(RAM_MAGIC))
            .ok_or(RamError::BadMagic)?;
        let version = u16::from_le_bytes([header[6], header[7]]);
        if version != RAM_VERSION {
            return Err(RamError::Version(version));
        }
        let expected = SRAM_SIZE as usize;
        let size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        let body = &bytes[RAM_HEADER_LEN..];
        let found = if size != expected { size } else { body.len() };
        if found != expected {
            return Err(RamError::Size { expected, found });
        }
        match self.bus.buffer_mut(SRAM_BUFFER) {
            Some(sram) if sram.len() == body.len() => sram.copy_from_slice(body),
            // Unreachable: the machine always maps SRAM_SIZE bytes of SRAM.
            _ => return Err(RamError::Size { expected, found }),
        }
        Ok(())
    }

    /// The SRAM's backing bytes.
    fn sram(&self) -> &[u8] {
        // The machine always maps the SRAM buffer.
        self.bus.region_bytes(SRAM_BUFFER).unwrap_or_default()
    }
}
