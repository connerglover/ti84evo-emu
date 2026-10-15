//! The device runtime: the [`Device`] trait, the [`DevCtx`] a device sees on every call (time,
//! IRQ lines, its event request, RAM/ROM buffers, the unmapped log), reset kinds, device save
//! state, and [`DevHarness`] for driving one device in unit tests.

use std::any::Any;
use std::ops::Range;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::region::Buffer;
use crate::unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};

/// Number of IRQ lines a device can drive (`irq < 64`).
const IRQ_LINES: u16 = 64;

/// All-ones mask for an access `width` in bytes.
pub(crate) fn mask(width: u8) -> u32 {
    match width {
        1 => 0xFF,
        2 => 0xFFFF,
        _ => u32::MAX,
    }
}

/// Which reset a device is going through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ResetKind {
    /// Power applied: every device returns to its power-on state.
    PowerOn,
    /// `SYSRESETREQ` (AIRCR): devices reset, but some (documented per device) keep state.
    System,
}

/// A memory-mapped peripheral. Offsets are relative to the region base; `width` is the access
/// size in bytes (1, 2 or 4) and is never wider than the bytes left in the region. The map
/// masks read results to `width` and passes write values already masked.
///
/// A device never panics on any offset, width or value: a register it does not model reads 0,
/// ignores writes and is reported with [`DevCtx::unhandled`].
///
/// `Send` so a whole `Machine` can move to another thread (emulation thread vs UI); `Any` so the
/// host can reach a concrete device through [`crate::MemoryMap::with_device`].
pub trait Device: Any + Send {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32;
    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>);
    /// The event requested with `DevCtx::schedule_at` is due (`ctx.now` >= its time).
    fn event(&mut self, _ctx: &mut DevCtx<'_>) {}
    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>);
    /// The device's state, for a save state (see [`save_serde`]).
    fn save(&self) -> Vec<u8>;
    /// Restore what [`Device::save`] produced. On error the device must be unchanged.
    fn load(&mut self, state: &[u8]) -> Result<(), StateError>;
}

/// What a device can see and touch while one of its methods runs.
pub struct DevCtx<'a> {
    /// Current time in CPU cycles ([`crate::CPU_HZ`] per second).
    pub now: u64,
    /// This device's IRQ output lines (bit n = line n asserted).
    irq: &'a mut u64,
    /// Edge-triggered IRQs requested, in order.
    pulses: &'a mut Vec<u16>,
    /// Event requests `(device id, Some(at) | None = cancel)`, in order.
    events: &'a mut Vec<(u16, Option<u64>)>,
    /// This device's id (its add order in the map).
    id: u16,
    buffers: &'a mut [Buffer],
    log: &'a mut UnmappedLog,
    /// Base address of the device's window, for [`DevCtx::unhandled`].
    base: u32,
    /// PC of the instruction that caused the call (`MemoryMap::pc_hint`).
    pc: u32,
    /// Security of the access (host calls use secure).
    secure: bool,
    /// What the device declared it changed in the buffers ([`DevCtx::buffer_mut`],
    /// [`DevCtx::buffer_mut_range`]): the map checks cached code against it.
    written: Written,
}

/// The buffer bytes a device call may have changed, as the device declared them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Written {
    Nothing,
    /// Bytes `start..end` of buffer `buf` (an index into the map's buffers).
    Range {
        buf: usize,
        start: usize,
        end: usize,
    },
    /// Anything in any buffer.
    Everything,
}

impl Written {
    /// This declaration widened by bytes `start..end` of buffer `buf`.
    fn with(self, buf: usize, start: usize, end: usize) -> Written {
        match self {
            Written::Nothing => Written::Range { buf, start, end },
            Written::Range {
                buf: b,
                start: s,
                end: e,
            } if b == buf => Written::Range {
                buf,
                start: s.min(start),
                end: e.max(end),
            },
            _ => Written::Everything,
        }
    }
}

impl<'a> DevCtx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        now: u64,
        irq: &'a mut u64,
        pulses: &'a mut Vec<u16>,
        events: &'a mut Vec<(u16, Option<u64>)>,
        id: u16,
        buffers: &'a mut [Buffer],
        log: &'a mut UnmappedLog,
        base: u32,
        pc: u32,
        secure: bool,
    ) -> Self {
        DevCtx {
            now,
            irq,
            pulses,
            events,
            id,
            buffers,
            log,
            base,
            pc,
            secure,
            written: Written::Nothing,
        }
    }
}

impl DevCtx<'_> {
    /// What the device declared it changed through this context.
    pub(crate) fn written(&self) -> Written {
        self.written
    }

    /// Drive level-sensitive IRQ line `irq` (`irq < 64`; anything else is logged and ignored).
    pub fn set_irq(&mut self, irq: u16, asserted: bool) {
        if irq >= IRQ_LINES {
            log::warn!("device at {:#010x}: set_irq({irq}) out of range", self.base);
            return;
        }
        if asserted {
            *self.irq |= 1 << irq;
        } else {
            *self.irq &= !(1 << irq);
        }
    }

    /// Pend IRQ `irq` once (an edge; `irq < 64`, anything else is logged and ignored).
    pub fn pulse_irq(&mut self, irq: u16) {
        if irq >= IRQ_LINES {
            log::warn!(
                "device at {:#010x}: pulse_irq({irq}) out of range",
                self.base
            );
            return;
        }
        self.pulses.push(irq);
    }

    /// Ask for [`Device::event`] at cycle `at`. Replaces this device's pending event.
    pub fn schedule_at(&mut self, at: u64) {
        self.events.push((self.id, Some(at)));
    }

    /// Drop this device's pending event, if any.
    pub fn cancel_event(&mut self) {
        self.events.push((self.id, None));
    }

    /// The bytes of the RAM or ROM called `name` (the first one if names repeat), read-only.
    pub fn buffer(&self, name: &str) -> Option<&[u8]> {
        self.buffers
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.data.as_slice())
    }

    /// The bytes of the RAM or ROM called `name` (the first one if names repeat), to change
    /// them. The map then treats the buffer as changed (code the CPU cached from it is stale),
    /// so a device that only reads uses [`buffer`](Self::buffer).
    pub fn buffer_mut(&mut self, name: &str) -> Option<&mut [u8]> {
        self.written = Written::Everything;
        self.buffers
            .iter_mut()
            .find(|b| b.name == name)
            .map(|b| b.data.as_mut_slice())
    }

    /// The whole RAM or ROM called `name`, like [`buffer_mut`](Self::buffer_mut), with the
    /// promise that the device changes only bytes `range` of it: the map then treats only code
    /// cached from `range` as stale. Changing bytes outside `range` is a device bug (the CPU
    /// may keep running the old instructions there).
    pub fn buffer_mut_range(&mut self, name: &str, range: Range<usize>) -> Option<&mut [u8]> {
        let i = self.buffers.iter().position(|b| b.name == name)?;
        self.written = self.written.with(i, range.start, range.end);
        Some(self.buffers[i].data.as_mut_slice())
    }

    /// Report an access to a register the device does not model: it goes to the unmapped log
    /// at `base + off`, as a write if `write` is `Some`, else a read.
    pub fn unhandled(&mut self, off: u32, width: u8, write: Option<u32>) {
        let kind = if write.is_some() {
            UnmappedKind::Write
        } else {
            UnmappedKind::Read
        };
        self.log.record(UnmappedAccess {
            addr: self.base.wrapping_add(off),
            width,
            write: write.map(|v| v & mask(width)),
            kind,
            pc: self.pc,
            secure: self.secure,
        });
    }
}

/// A save state that cannot be restored.
#[derive(thiserror::Error, Debug)]
pub enum StateError {
    #[error("state decode: {0}")]
    Decode(#[from] postcard::Error),
    #[error("not a save state")]
    BadMagic,
    #[error("save-state version {found}, expected {expected}")]
    Version { found: u32, expected: u32 },
    #[error("save state does not fit this machine: {0}")]
    Mismatch(String),
    /// The body does not match the checksum in the header: damaged, truncated or edited.
    #[error("save state is corrupt (checksum mismatch)")]
    Corrupt,
}

/// Encode a device's state (postcard).
pub fn save_serde<T: Serialize>(v: &T) -> Vec<u8> {
    // Encoding plain data into a Vec cannot fail; an empty state would fail to load, loudly.
    postcard::to_stdvec(v).unwrap_or_default()
}

/// Decode `state` into `dst`. On error `dst` is unchanged. Trailing bytes are a
/// [`StateError::Mismatch`]: the state was made by a different device layout.
pub fn load_serde<T: DeserializeOwned>(dst: &mut T, state: &[u8]) -> Result<(), StateError> {
    let (v, rest) = postcard::take_from_bytes::<T>(state)?;
    if !rest.is_empty() {
        return Err(StateError::Mismatch(format!(
            "{} trailing bytes in device state",
            rest.len()
        )));
    }
    *dst = v;
    Ok(())
}

/// Drives one device outside a MemoryMap (unit tests). The device sits at base 0, every call is
/// secure with PC 0.
#[derive(Default)]
pub struct DevHarness {
    pub now: u64,
    /// The device's IRQ lines.
    pub irq: u64,
    /// Pulses requested so far, in order.
    pub pulses: Vec<u16>,
    /// The latest event request: `None` = none, `Some(Some(at))` = pending, `Some(None)` = cancelled.
    pub event: Option<Option<u64>>,
    /// `DevCtx::unhandled` reports.
    pub log: UnmappedLog,
    buffers: Vec<Buffer>,
}

impl DevHarness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a buffer the device can reach with [`DevCtx::buffer_mut`].
    pub fn with_buffer(mut self, name: &str, data: Vec<u8>) -> Self {
        self.buffers.push(Buffer {
            name: name.to_owned(),
            data,
        });
        self
    }

    /// # Panics
    ///
    /// If there is no buffer called `name` (a test bug).
    pub fn buffer(&self, name: &str) -> &[u8] {
        self.buffers
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.data.as_slice())
            .unwrap_or_else(|| panic!("no buffer named {name:?}"))
    }

    fn call<R>(&mut self, f: impl FnOnce(&mut DevCtx<'_>) -> R) -> R {
        let mut events = Vec::new();
        let r = f(&mut DevCtx::new(
            self.now,
            &mut self.irq,
            &mut self.pulses,
            &mut events,
            0,
            &mut self.buffers,
            &mut self.log,
            0,
            0,
            true,
        ));
        if let Some(&(_, last)) = events.last() {
            self.event = Some(last);
        }
        r
    }

    /// Run `f` with this harness's context: for device host APIs that take a [`DevCtx`]
    /// (e.g. injecting UART bytes), like `Machine` host APIs do through the map.
    pub fn with_ctx<R>(&mut self, f: impl FnOnce(&mut DevCtx<'_>) -> R) -> R {
        self.call(f)
    }

    /// A read, masked to `width` like the map does.
    pub fn read(&mut self, d: &mut dyn Device, off: u32, width: u8) -> u32 {
        self.call(|ctx| d.read(off, width, ctx)) & mask(width)
    }

    /// A write; `v` is masked to `width` like the map does.
    pub fn write(&mut self, d: &mut dyn Device, off: u32, width: u8, v: u32) {
        self.call(|ctx| d.write(off, width, v & mask(width), ctx));
    }

    pub fn reset(&mut self, d: &mut dyn Device, kind: ResetKind) {
        self.call(|ctx| d.reset(kind, ctx));
    }

    /// If an event is pending: `now = max(now, at)`, clear it, call `d.event`. Returns whether it fired.
    pub fn fire(&mut self, d: &mut dyn Device) -> bool {
        let Some(Some(at)) = self.event else {
            return false;
        };
        self.now = self.now.max(at);
        self.event = None;
        self.call(|ctx| d.event(ctx));
        true
    }
}
