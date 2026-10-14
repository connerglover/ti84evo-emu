//! Memory bus abstraction used by the CPU core.

/// Kind of bus access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessKind {
    Fetch,
    Read,
    Write,
}

/// Attributes of a bus access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub kind: AccessKind,
    pub secure: bool,
    pub privileged: bool,
}

/// Error returned by a bus access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    Decode,
    SecureViolation,
}

/// A memory bus the CPU reads and writes through.
pub trait Bus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError>;
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError>;
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError>;
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError>;
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError>;
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError>;
}
