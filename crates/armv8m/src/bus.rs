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
    /// Security of the access after the core's attribution check (SAU/IDAU): Secure for
    /// Secure and Non-secure callable memory (only Secure state gets that far), Non-secure for
    /// Non-secure memory even from Secure state. Bus-side filters (MPC/PPC) check this.
    pub secure: bool,
    pub privileged: bool,
}

/// Error returned by a bus access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    Decode,
    /// Rejected by a bus-side security filter (MPC/PPC). Like `Decode`, the core turns it into
    /// a BusFault; SecureFault comes only from the core's own attribution check.
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
