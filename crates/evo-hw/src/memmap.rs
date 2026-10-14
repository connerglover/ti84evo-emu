//! The TI-84 Evo physical memory map, as seen by the CPU through [`armv8m::Bus`].
//!
//! Anything nobody has modeled yet reads as zero and swallows writes, but is recorded in
//! [`MemoryMap::log`] (address, width, kind, value, PC, security) so the undocumented SoC's
//! peripherals can be discovered by running the real firmware. The one exception is an
//! instruction fetch from unmapped space: it is logged too, but answered with
//! `BusError::Decode` so the CPU raises IBUSERR instead of executing zeros (`movs r0, r0`)
//! forever. Data accesses never fault in Plan 1.

use armv8m::{Access, AccessKind, Bus, BusError};
use serde::{Deserialize, Serialize};

use crate::region::{self, Buffer, Device, Region, Target};
use crate::unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};

/// Sorted table of RAM, ROM and device windows plus the log of accesses that hit none of them.
///
/// The CPU intercepts the internal PPB (0xE000_0000..=0xE003_FFFF) itself, so it never reaches
/// this map. The external PPB (0xE004_0000..=0xE00F_FFFF) is reached over the system bus: data
/// accesses there come here like any other address (and land in the unmapped log if no window
/// covers them).
///
/// # Serialization
///
/// RAM/ROM contents, the window table, `pc_hint` and the log are serialized. Devices are not:
/// they get their own state in Plan 2, and a map restored from a save state has to have its
/// devices re-attached. Until then a restored device window behaves like unmapped space
/// (reads 0, writes dropped and logged) rather than panicking.
#[derive(Default, Serialize, Deserialize)]
pub struct MemoryMap {
    /// Accesses to unmapped space, and writes to ROM.
    pub log: UnmappedLog,
    /// PC of the instruction being executed; the `Machine` sets this before each step so log
    /// entries say who touched what.
    pub pc_hint: u32,
    /// Sorted by base, non-overlapping.
    regions: Vec<Region>,
    /// Backing bytes of every RAM and ROM; aliased windows share one entry.
    buffers: Vec<Buffer>,
    // Plan 2: serialize device state and restore it into re-attached devices.
    #[serde(skip)]
    devices: Vec<Box<dyn Device>>,
}

/// Where an access lands relative to the window table.
enum Span {
    /// Entirely inside one window; `off` is relative to its base.
    Inside { region: usize, off: u32 },
    /// No byte of it is mapped.
    Unmapped,
    /// Crosses a window boundary, or is partly unmapped.
    Straddle,
}

/// All-ones mask for an access `width` in bytes.
fn mask(width: u8) -> u32 {
    match width {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => u32::MAX,
    }
}

impl MemoryMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add zero-filled RAM of `size` bytes, visible at each address in `aliases` (all windows
    /// share the same bytes).
    ///
    /// # Panics
    ///
    /// If a window would overlap another or run past 0xFFFF_FFFF (a static layout bug).
    pub fn add_ram(&mut self, name: &'static str, size: u32, aliases: &[u32]) {
        self.add_backed(name, vec![0; size as usize], aliases, false);
    }

    /// Add read-only memory holding `data`, visible at each address in `aliases`. Guest writes
    /// are dropped and logged (QSPI programming is Plan 2); [`MemoryMap::poke`] can still patch it.
    ///
    /// # Panics
    ///
    /// If a window would overlap another or run past 0xFFFF_FFFF, or `data` is 4 GiB or more.
    pub fn add_rom(&mut self, name: &'static str, data: Vec<u8>, aliases: &[u32]) {
        self.add_backed(name, data, aliases, true);
    }

    fn add_backed(&mut self, name: &'static str, data: Vec<u8>, aliases: &[u32], rom: bool) {
        let size = u32::try_from(data.len()).expect("region larger than the address space");
        let b = self.buffers.len();
        self.buffers.push(Buffer {
            name: name.to_owned(),
            data,
        });
        let target = if rom { Target::Rom(b) } else { Target::Ram(b) };
        for &base in aliases {
            let region = Region {
                name: name.to_owned(),
                base,
                size,
                target,
            };
            region::insert(&mut self.regions, region);
        }
    }

    /// Map `dev` over `base..base + size`. The device sees offsets relative to `base`.
    ///
    /// # Panics
    ///
    /// If the window would overlap another or run past 0xFFFF_FFFF (a static layout bug).
    pub fn add_device(&mut self, name: &'static str, base: u32, size: u32, dev: Box<dyn Device>) {
        let d = self.devices.len();
        self.devices.push(dev);
        let region = Region {
            name: name.to_owned(),
            base,
            size,
            target: Target::Device(d),
        };
        region::insert(&mut self.regions, region);
    }

    /// Backing bytes of the RAM or ROM called `name` (the first one if names repeat).
    pub fn region_bytes(&self, name: &str) -> Option<&[u8]> {
        self.buffers
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.data.as_slice())
    }

    // ---- debugger access: no logging, no device side effects ----

    /// Read `width` (1, 2 or 4) bytes for a debugger. Only RAM and ROM can be peeked: devices
    /// return `None` because reading a register can have side effects, and unmapped space
    /// returns `None` without being logged. Reads change nothing, so this takes `&self`.
    pub fn peek(&self, addr: u32, width: u8) -> Option<u32> {
        if !matches!(width, 1 | 2 | 4) {
            return None;
        }
        let mut v = 0u32;
        for i in 0..u32::from(width) {
            v |= u32::from(self.byte(addr.wrapping_add(i))?) << (8 * i);
        }
        Some(v)
    }

    /// Write `width` (1, 2 or 4) bytes for a debugger; returns whether it was applied. RAM and
    /// ROM only (the debugger may patch ROM). Devices and unmapped space are refused and
    /// nothing is logged. All-or-nothing: an access that is only partly in RAM/ROM writes nothing.
    pub fn poke(&mut self, addr: u32, width: u8, v: u32) -> bool {
        if !matches!(width, 1 | 2 | 4) {
            return false;
        }
        let at = |i: u32| addr.wrapping_add(i);
        if (0..u32::from(width)).any(|i| self.byte(at(i)).is_none()) {
            return false;
        }
        for (i, b) in (0..u32::from(width)).zip(v.to_le_bytes()) {
            if let Some(slot) = self.byte_mut(at(i)) {
                *slot = b;
            }
        }
        true
    }

    fn byte(&self, addr: u32) -> Option<u8> {
        let i = region::find(&self.regions, addr)?;
        let r = &self.regions[i];
        match r.target {
            Target::Ram(b) | Target::Rom(b) => self
                .buffers
                .get(b)?
                .data
                .get((addr - r.base) as usize)
                .copied(),
            Target::Device(_) => None,
        }
    }

    fn byte_mut(&mut self, addr: u32) -> Option<&mut u8> {
        let i = region::find(&self.regions, addr)?;
        let r = &self.regions[i];
        match r.target {
            Target::Ram(b) | Target::Rom(b) => self
                .buffers
                .get_mut(b)?
                .data
                .get_mut((addr - r.base) as usize),
            Target::Device(_) => None,
        }
    }

    // ---- guest access ----

    fn log_access(
        &mut self,
        addr: u32,
        width: u8,
        kind: UnmappedKind,
        write: Option<u32>,
        secure: bool,
    ) {
        self.log.record(UnmappedAccess {
            addr,
            width,
            write,
            kind,
            pc: self.pc_hint,
            secure,
        });
    }

    /// Classify an access of `width` bytes at `addr` (addresses wrap at 2^32, like the CPU's).
    fn span(&self, addr: u32, width: u8) -> Span {
        if let Some(i) = region::find(&self.regions, addr) {
            let r = &self.regions[i];
            let off = addr - r.base;
            return if u64::from(off) + u64::from(width) <= u64::from(r.size) {
                Span::Inside { region: i, off }
            } else {
                Span::Straddle
            };
        }
        let all_unmapped = (1..u32::from(width))
            .all(|k| region::find(&self.regions, addr.wrapping_add(k)).is_none());
        if all_unmapped {
            Span::Unmapped
        } else {
            Span::Straddle
        }
    }

    /// Read from inside one window. `None` means the window has nothing behind it (only
    /// possible in a corrupt or device-less restored state), which the caller treats as unmapped.
    fn read_inside(&mut self, region: usize, off: u32, width: u8) -> Option<u32> {
        match self.regions.get(region)?.target {
            Target::Ram(b) | Target::Rom(b) => {
                let start = off as usize;
                let bytes = self
                    .buffers
                    .get(b)?
                    .data
                    .get(start..start + usize::from(width))?;
                let mut le = [0u8; 4];
                le[..bytes.len()].copy_from_slice(bytes);
                Some(u32::from_le_bytes(le))
            }
            Target::Device(d) => {
                let dev = self.devices.get_mut(d)?;
                Some(dev.read(off, width) & mask(width))
            }
        }
    }

    /// Write inside one window. Returns whether the write took effect; ROM and windows with
    /// nothing behind them return `false` and the caller logs the dropped write.
    fn write_inside(&mut self, region: usize, off: u32, width: u8, v: u32) -> bool {
        let Some(target) = self.regions.get(region).map(|r| r.target) else {
            return false;
        };
        match target {
            Target::Ram(b) => {
                let start = off as usize;
                let dst = self
                    .buffers
                    .get_mut(b)
                    .and_then(|buf| buf.data.get_mut(start..start + usize::from(width)));
                match dst {
                    Some(dst) => {
                        dst.copy_from_slice(&v.to_le_bytes()[..usize::from(width)]);
                        true
                    }
                    None => false,
                }
            }
            Target::Rom(_) => false,
            Target::Device(d) => match self.devices.get_mut(d) {
                Some(dev) => {
                    dev.write(off, width, v & mask(width));
                    true
                }
                None => false,
            },
        }
    }

    /// A guest read of `width` bytes: the value, and whether any byte of it was unmapped.
    /// Unmapped bytes read as 0 and are logged as `kind`. An access that straddles windows or
    /// is partly unmapped is split into byte accesses, so each unmapped byte is logged on its
    /// own and devices never see an access that runs off their end.
    fn read(&mut self, addr: u32, width: u8, kind: UnmappedKind, secure: bool) -> (u32, bool) {
        match self.span(addr, width) {
            Span::Inside { region, off } => {
                if let Some(v) = self.read_inside(region, off, width) {
                    return (v, false);
                }
                self.log_access(addr, width, kind, None, secure);
                (0, true)
            }
            Span::Unmapped => {
                self.log_access(addr, width, kind, None, secure);
                (0, true)
            }
            Span::Straddle => {
                let (mut v, mut unmapped) = (0, false);
                for i in 0..u32::from(width) {
                    let (b, miss) = self.read(addr.wrapping_add(i), 1, kind, secure);
                    v |= b << (8 * i);
                    unmapped |= miss;
                }
                (v, unmapped)
            }
        }
    }

    /// The bus-facing read: a fetch that touched unmapped space is a bus error (after being
    /// logged); any other read just gets the zeros.
    fn bus_read(&mut self, addr: u32, width: u8, a: Access) -> Result<u32, BusError> {
        let fetch = a.kind == AccessKind::Fetch;
        let kind = if fetch {
            UnmappedKind::Fetch
        } else {
            UnmappedKind::Read
        };
        let (v, unmapped) = self.read(addr, width, kind, a.secure);
        if fetch && unmapped {
            Err(BusError::Decode)
        } else {
            Ok(v)
        }
    }

    fn log_write(&mut self, addr: u32, width: u8, v: u32, secure: bool) {
        self.log_access(
            addr,
            width,
            UnmappedKind::Write,
            Some(v & mask(width)),
            secure,
        );
    }

    /// A guest write of `width` bytes. Never fails: writes to unmapped space and ROM are
    /// dropped and logged. Splits like [`MemoryMap::read`].
    fn write(&mut self, addr: u32, width: u8, v: u32, secure: bool) {
        match self.span(addr, width) {
            Span::Inside { region, off } => {
                if !self.write_inside(region, off, width, v) {
                    self.log_write(addr, width, v, secure);
                }
            }
            Span::Unmapped => self.log_write(addr, width, v, secure),
            Span::Straddle => {
                for i in 0..u32::from(width) {
                    self.write(addr.wrapping_add(i), 1, (v >> (8 * i)) & 0xFF, secure);
                }
            }
        }
    }
}

impl Bus for MemoryMap {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        Ok(self.bus_read(addr, 1, a)? as u8)
    }
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        Ok(self.bus_read(addr, 2, a)? as u16)
    }
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        self.bus_read(addr, 4, a)
    }
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        self.write(addr, 1, u32::from(v), a.secure);
        Ok(())
    }
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        self.write(addr, 2, u32::from(v), a.secure);
        Ok(())
    }
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        self.write(addr, 4, v, a.secure);
        Ok(())
    }
}
