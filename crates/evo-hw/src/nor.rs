//! W25Q128-style NOR flash command engine and the flash file (`docs/hardware/spif-nor.md`
//! §6.2, §8.5-8.6).
//!
//! [`NorFlash`] holds the part's status registers, its security registers and the open data
//! phase of the current command. The flash array itself is not owned: every call takes the
//! `"flash"` buffer of the memory map, so XIP reads see erases and programs at once.
//!
//! Every operation completes instantly (WIP always reads 0). Read data is generated one byte at
//! a time from the open phase; no buffer of the command length is ever built, so a guest length
//! of 0xFFFF_FFFF costs nothing.
//!
//! [`NvState`] is the non-volatile part of the machine (flash bytes and security registers) and
//! its on-disk format, the flash file.

use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Size of the flash array in bytes (16 MiB).
pub const NOR_SIZE: usize = 16 << 20;
/// Size of the security registers: 3 x 256 bytes, command addresses 0x1000/0x2000/0x3000.
pub const SECREGS_LEN: usize = 768;
/// JEDEC ID of the modeled part (Winbond W25Q128JV).
pub const JEDEC_ID: [u8; 3] = [0xEF, 0x40, 0x18];

const SECREG_SIZE: usize = 256;
const ERASED: u8 = 0xFF;
/// SR1 write-enable latch.
const WEL: u8 = 0x02;
/// SR1 bits a WRSR never sets directly (WIP, WEL).
const SR1_COMPUTED: u8 = 0x03;

/// The data phase a command opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataPhase {
    /// No data phase: the command is complete.
    None,
    /// Bytes are read with [`NorFlash::read_byte`].
    Read,
    /// Bytes are written with [`NorFlash::write_byte`].
    Write,
}

/// The open data phase. `addr` is the address of the next byte (for 0x9F, the ID index).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct OpenPhase {
    op: u8,
    addr: u32,
    remaining: u32,
    kind: DataPhase,
}

/// The NOR part's command engine.
///
/// Across a System reset the security registers and the status-register bits other than WEL
/// are kept (the owner aborts the phase and clears WEL; `spif-nor.md` §8.8).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NorFlash {
    /// Status register 1 (WIP = bit 0, WEL = bit 1).
    pub sr1: u8,
    /// Status register 2 (QE = bit 1).
    pub sr2: u8,
    /// Status register 3.
    pub sr3: u8,
    phase: Option<OpenPhase>,
    /// Non-volatile: persisted by the flash file (NvState), never by save states.
    #[serde(skip)]
    secregs: Vec<u8>,
}

impl Default for NorFlash {
    fn default() -> Self {
        Self::new()
    }
}

/// `addr` reduced into a buffer of `len` bytes; `None` when the buffer is empty.
fn wrap(addr: u32, len: usize) -> Option<usize> {
    (len > 0).then(|| addr as usize % len)
}

/// The next address inside the same 256-byte page.
fn page_next(addr: u32) -> u32 {
    (addr & !0xFF) | (addr.wrapping_add(1) & 0xFF)
}

/// Index of the security register at command address `addr`, if it is one.
fn secreg_base(addr: u32) -> Option<usize> {
    match (addr >> 12) & 0xFFF {
        n @ 1..=3 => Some((n as usize - 1) * SECREG_SIZE),
        _ => None,
    }
}

impl NorFlash {
    /// A part in its factory state: SR1 0x00, SR2 0x02 (QE), SR3 0x60, security registers
    /// erased.
    pub fn new() -> Self {
        Self {
            sr1: 0x00,
            sr2: 0x02,
            sr3: 0x60,
            phase: None,
            secregs: vec![ERASED; SECREGS_LEN],
        }
    }

    fn wel(&self) -> bool {
        self.sr1 & WEL != 0
    }

    fn clear_wel(&mut self) {
        self.sr1 &= !WEL;
    }

    /// The bytes of a `flash_len`-byte array that [`begin`](Self::begin) of `op` at `addr` would
    /// change now: an erase with the write-enable latch set. `None` for any other command;
    /// `begin` then leaves `flash` alone (an empty slice does), so the caller need not borrow
    /// the array for writing.
    pub fn begin_changes(&self, op: u8, addr: u32, flash_len: usize) -> Option<Range<usize>> {
        if !self.wel() {
            return None;
        }
        erase_range(op, addr, flash_len)
    }

    /// The bytes of a `flash_len`-byte array the next [`write_byte`](Self::write_byte) would
    /// change: one byte while a page program phase is open and the write-enable latch is set.
    /// `None` otherwise; `write_byte` then leaves `flash` alone (an empty slice does).
    pub fn write_changes(&self, flash_len: usize) -> Option<Range<usize>> {
        let p = self.phase?;
        if !self.wel() || p.kind != DataPhase::Write || !matches!(p.op, 0x02 | 0x32) {
            return None;
        }
        let a = wrap(p.addr, flash_len)?;
        Some(a..a + 1)
    }

    /// Starts command `op` at `addr` with a data length of `len` bytes, aborting any open phase.
    /// Non-data commands (erase, WEL, unknown ops) take effect at once and return
    /// [`DataPhase::None`].
    pub fn begin(&mut self, op: u8, addr: u32, len: u32, flash: &mut [u8]) -> DataPhase {
        self.phase = None;
        let kind = match op {
            0x9F | 0x05 | 0x35 | 0x15 | 0x03 | 0x0B | 0x3B | 0x6B | 0xBB | 0xEB | 0x48 => {
                DataPhase::Read
            }
            0x02 | 0x32 | 0x42 => DataPhase::Write,
            0x01 | 0x31 | 0x11 => {
                self.phase = Some(OpenPhase {
                    op,
                    addr,
                    remaining: 1,
                    kind: DataPhase::Write,
                });
                return DataPhase::Write;
            }
            0x06 => {
                self.sr1 |= WEL;
                return DataPhase::None;
            }
            0x04 => {
                self.clear_wel();
                return DataPhase::None;
            }
            0x20 | 0x52 | 0xD8 | 0xC7 | 0x60 => {
                if self.wel() {
                    erase(op, addr, flash);
                }
                self.clear_wel();
                return DataPhase::None;
            }
            0x44 => {
                if self.wel()
                    && let Some(base) = secreg_base(addr)
                    && let Some(s) = self.secregs.get_mut(base..base + SECREG_SIZE)
                {
                    s.fill(ERASED);
                }
                self.clear_wel();
                return DataPhase::None;
            }
            0xFF | 0x66 | 0x99 | 0xAB | 0xB9 => return DataPhase::None,
            _ => {
                log::debug!("nor: unknown op {op:#04x} treated as no-data");
                return DataPhase::None;
            }
        };
        if len == 0 {
            return DataPhase::None;
        }
        let addr = if op == 0x9F { 0 } else { addr };
        self.phase = Some(OpenPhase {
            op,
            addr,
            remaining: len,
            kind,
        });
        kind
    }

    /// Bytes left in the open data phase (0 when none is open).
    pub fn remaining(&self) -> u32 {
        self.phase.map_or(0, |p| p.remaining)
    }

    /// The next byte of the open read phase; 0 when no read phase is open.
    pub fn read_byte(&mut self, flash: &[u8]) -> u8 {
        let Some(mut p) = self.phase.filter(|p| p.kind == DataPhase::Read) else {
            return 0;
        };
        let (b, next) = match p.op {
            0x9F => (
                JEDEC_ID.get(p.addr as usize).copied().unwrap_or(0),
                p.addr.saturating_add(1),
            ),
            0x05 => (self.sr1, p.addr),
            0x35 => (self.sr2, p.addr),
            0x15 => (self.sr3, p.addr),
            0x48 => {
                let b = secreg_base(p.addr)
                    .and_then(|base| self.secregs.get(base + (p.addr & 0xFF) as usize))
                    .copied()
                    .unwrap_or(ERASED);
                (b, page_next(p.addr))
            }
            _ => {
                let b = wrap(p.addr, flash.len()).map_or(ERASED, |a| flash[a]);
                let next = wrap(p.addr.wrapping_add(1), flash.len()).map_or(0, |a| a as u32);
                (b, next)
            }
        };
        p.addr = next;
        p.remaining -= 1;
        self.phase = (p.remaining > 0).then_some(p);
        b
    }

    /// Pushes one byte into the open write phase. Returns true when this byte completed it
    /// (WEL is then clear). Without WEL the byte is accepted and discarded.
    pub fn write_byte(&mut self, b: u8, flash: &mut [u8]) -> bool {
        let Some(mut p) = self.phase.filter(|p| p.kind == DataPhase::Write) else {
            return false;
        };
        if self.wel() {
            match p.op {
                0x01 => self.sr1 = (b & !SR1_COMPUTED) | (self.sr1 & SR1_COMPUTED),
                0x31 => self.sr2 = b,
                0x11 => self.sr3 = b,
                0x42 => {
                    if let Some(s) = secreg_base(p.addr)
                        .and_then(|base| self.secregs.get_mut(base + (p.addr & 0xFF) as usize))
                    {
                        *s &= b;
                    }
                }
                _ => {
                    if let Some(a) = wrap(p.addr, flash.len()) {
                        flash[a] &= b;
                    }
                }
            }
        }
        p.addr = page_next(p.addr);
        p.remaining -= 1;
        if p.remaining == 0 {
            self.phase = None;
            self.clear_wel();
            true
        } else {
            self.phase = Some(p);
            false
        }
    }

    /// Closes any open data phase.
    pub fn abort(&mut self) {
        self.phase = None;
    }

    /// The 3 security registers, back to back (empty after a save-state round trip until
    /// [`NorFlash::set_secregs`] restores them).
    pub fn secregs(&self) -> &[u8] {
        &self.secregs
    }

    /// Replaces the security registers. `b` must be [`SECREGS_LEN`] bytes; anything else is
    /// logged and ignored.
    pub fn set_secregs(&mut self, b: &[u8]) {
        if b.len() != SECREGS_LEN {
            log::warn!(
                "nor: security registers need {SECREGS_LEN} bytes, got {}",
                b.len()
            );
            return;
        }
        self.secregs = b.to_vec();
    }
}

/// The bytes erase command `op` at `addr` clears in an array of `len` bytes: the 4K/32K/64K
/// block `addr` falls in, or the whole chip. `None` for other commands and an empty array.
fn erase_range(op: u8, addr: u32, len: usize) -> Option<Range<usize>> {
    let size = match op {
        0x20 => 4 << 10,
        0x52 => 32 << 10,
        0xD8 => 64 << 10,
        0xC7 | 0x60 => return (len > 0).then_some(0..len),
        _ => return None,
    };
    let start = wrap(addr, len)? & !(size - 1);
    Some(start..(start + size).min(len))
}

/// Erases the 4K/32K/64K block (or the whole chip) that `addr` falls in.
fn erase(op: u8, addr: u32, flash: &mut [u8]) {
    if let Some(r) = erase_range(op, addr, flash.len()) {
        flash[r].fill(ERASED);
    }
}

const MAGIC: &[u8; 8] = b"EVOFLASH";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 16;
const FILE_LEN: usize = HEADER_LEN + NOR_SIZE + SECREGS_LEN;

/// A flash-file error.
#[derive(thiserror::Error, Debug)]
pub enum NvError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("not a flash file")]
    BadMagic,
    #[error("flash file version {0}")]
    Version(u32),
    #[error("flash file size: expected {expected} bytes, found {found}")]
    Size { expected: usize, found: usize },
}

/// The non-volatile state: the flash array and the security registers.
///
/// File format: `b"EVOFLASH"`, u32 LE version 1, u32 LE flash length (0x0100_0000), the flash
/// bytes, then the 768 security-register bytes.
#[derive(Clone, Debug)]
pub struct NvState {
    flash: Vec<u8>,
    secregs: Vec<u8>,
}

impl NvState {
    /// A fully erased part.
    pub fn erased() -> Self {
        Self {
            flash: vec![ERASED; NOR_SIZE],
            secregs: vec![ERASED; SECREGS_LEN],
        }
    }

    /// Checks that `flash` is [`NOR_SIZE`] and `secregs` [`SECREGS_LEN`] bytes long.
    pub fn new(flash: Vec<u8>, secregs: Vec<u8>) -> Result<Self, NvError> {
        if flash.len() != NOR_SIZE {
            return Err(NvError::Size {
                expected: NOR_SIZE,
                found: flash.len(),
            });
        }
        if secregs.len() != SECREGS_LEN {
            return Err(NvError::Size {
                expected: SECREGS_LEN,
                found: secregs.len(),
            });
        }
        Ok(Self { flash, secregs })
    }

    pub fn flash(&self) -> &[u8] {
        &self.flash
    }

    pub fn secregs(&self) -> &[u8] {
        &self.secregs
    }

    /// Reads and validates a flash file. The file is never modified.
    ///
    /// The header is read first and the file's size is checked against its metadata before
    /// the body is read, so a file of the wrong size (however large) is rejected without being
    /// read whole.
    pub fn read_file(path: &Path) -> Result<Self, NvError> {
        let mut file = File::open(path)?;
        let found = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        let mut header = Vec::with_capacity(HEADER_LEN);
        (&mut file)
            .take(HEADER_LEN as u64)
            .read_to_end(&mut header)?;
        let n = MAGIC.len().min(header.len());
        if header[..n] != MAGIC[..n] {
            return Err(NvError::BadMagic);
        }
        if header.len() < HEADER_LEN {
            return Err(NvError::Size {
                expected: FILE_LEN,
                found,
            });
        }
        let word =
            |o: usize| u32::from_le_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
        let version = word(8);
        if version != VERSION {
            return Err(NvError::Version(version));
        }
        let flash_len = word(12) as usize;
        if flash_len != NOR_SIZE {
            return Err(NvError::Size {
                expected: NOR_SIZE,
                found: flash_len,
            });
        }
        if found != FILE_LEN {
            return Err(NvError::Size {
                expected: FILE_LEN,
                found,
            });
        }
        let mut flash = vec![0; NOR_SIZE];
        file.read_exact(&mut flash)?;
        let mut secregs = vec![0; SECREGS_LEN];
        file.read_exact(&mut secregs)?;
        // The file grew after its size was checked: not a file this build wrote.
        let mut extra = [0u8; 1];
        if file.read(&mut extra)? != 0 {
            return Err(NvError::Size {
                expected: FILE_LEN,
                found: FILE_LEN + 1,
            });
        }
        Self::new(flash, secregs)
    }

    /// Writes the flash file atomically ([`write_atomic`]).
    pub fn write_file(&self, path: &Path) -> Result<(), NvError> {
        let mut out = Vec::with_capacity(FILE_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(NOR_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&self.flash);
        out.extend_from_slice(&self.secregs);
        Ok(write_atomic(path, &out)?)
    }
}

/// Writes `bytes` to `path` so that `path` always holds either its old contents or all of
/// `bytes`: first to `PATH.tmp`, flushed to the disk (`sync_all`), then renamed over `path`.
/// On error the temporary file is removed and `path` is untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}
