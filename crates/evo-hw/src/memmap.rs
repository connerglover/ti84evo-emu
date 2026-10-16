//! The TI-84 Evo physical memory map, as seen by the CPU through [`armv8m::Bus`].
//!
//! Anything nobody has modeled yet reads as zero and swallows writes, but is recorded in
//! [`MemoryMap::log`] (address, width, kind, value, PC, security) so the undocumented SoC's
//! peripherals can be discovered by running the real firmware. The one exception is an
//! instruction fetch from unmapped space: it is logged too, but answered with
//! `BusError::Decode` so the CPU raises IBUSERR instead of executing zeros (`movs r0, r0`)
//! forever. Data accesses never fault in Plan 1.

use std::sync::atomic::{AtomicU64, Ordering};

use armv8m::{Access, AccessKind, Bus, BusError};
use serde::{Deserialize, Serialize};

use crate::device::{DevCtx, Device, DeviceInspect, ResetKind, StateError, Written, mask};
use crate::region::{self, Buffer, Region, Target};
use crate::unmapped::{UnmappedAccess, UnmappedKind, UnmappedLog};
use crate::watch::{AccessDir, BusRecord, Watch, WatchError, WatchHit, WatchRange};

/// One device in the map: the device, where it sits, and the IRQ lines it drives.
struct Slot {
    name: &'static str,
    base: u32,
    dev: Box<dyn Device>,
    /// This device's IRQ lines; [`MemoryMap::irq_lines`] ORs every slot's.
    irq_out: u64,
}

/// One device's part of a save state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceState {
    pub name: String,
    pub irq_out: u64,
    pub state: Vec<u8>,
}

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
/// their state travels separately ([`MemoryMap::device_states`] /
/// [`MemoryMap::load_device_states`]), and a map restored from a save state has to have its
/// devices re-attached. Until then a restored device window behaves like unmapped space
/// (reads 0, writes dropped and logged) rather than panicking.
///
/// # Devices
///
/// Each device gets a [`DevCtx`] on every call: `now`, its own IRQ lines (OR-ed across devices
/// by [`MemoryMap::irq_lines`]), pulse and event requests (queued in order, drained by the
/// `Machine` with [`MemoryMap::take_pulses`] / [`MemoryMap::take_event_requests`]), the RAM/ROM
/// buffers and the unmapped log.
#[derive(Default, Serialize, Deserialize)]
pub struct MemoryMap {
    /// Cycle count devices see as `ctx.now`; set from `sched.now` by Machine. Not saved state.
    #[serde(skip)]
    pub now: u64,
    /// Accesses to unmapped space, and writes to ROM.
    pub log: UnmappedLog,
    /// PC of the instruction being executed; the `Machine` sets this before each step so log
    /// entries say who touched what.
    pub pc_hint: u32,
    /// Sorted by base, non-overlapping.
    regions: Vec<Region>,
    /// Backing bytes of every RAM and ROM; aliased windows share one entry.
    buffers: Vec<Buffer>,
    /// Devices in add order (index = device id). Re-attached, not deserialized.
    #[serde(skip)]
    slots: Vec<Slot>,
    /// Edge IRQs requested by devices, in order.
    #[serde(skip)]
    pulses: Vec<u16>,
    /// Event requests `(device id, Some(at) | None = cancel)`, in order.
    #[serde(skip)]
    events: Vec<(u16, Option<u64>)>,
    /// The fast path: one entry per 64 KiB page of the address space ([`Page`]), filled in as
    /// RAM and ROM windows are added (windows are never removed, and a device can never share
    /// a page with a whole-page window). Not serialized: a deserialized map has an empty table
    /// and takes the general path for every access, with the same results.
    #[serde(skip)]
    pages: Vec<Page>,
    /// Set whenever a device ran (or had its state replaced), which is the only way IRQ
    /// lines, pulses and event requests change; cleared by [`MemoryMap::take_device_activity`].
    #[serde(skip)]
    device_activity: bool,
    /// [`Bus::code_epoch`]: replaced by a fresh value whenever bytes the CPU may have cached as
    /// code can have changed (a write to a code page, `buffer_mut`, `poke`, a device writing a
    /// buffer). Fresh per map, so a CPU moved to another map never mistakes its old entries.
    #[serde(skip)]
    code_epoch: Epoch,
    /// Per buffer, per 4 KiB page of the buffer ([`CODE_BITS`]): whether [`Bus::cache_code`]
    /// accepted code in it, so writes to it must change `code_epoch`. Grown on demand.
    #[serde(skip)]
    code_pages: Vec<Vec<bool>>,
    /// Index of the window the last general-path access hit (a hint; any value is safe).
    #[serde(skip)]
    last_region: usize,
    /// The OR of every slot's `irq_out`, updated whenever one changes.
    #[serde(skip)]
    irq_lines: u64,
    /// Data watchpoints and the bus monitor ([`crate::watch`]). Debugger state, not saved.
    #[serde(skip)]
    watch: Watch,
}

/// Source of [`MemoryMap::code_epoch`] values, unique within the process.
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// A [`Bus::code_epoch`] value; `default` takes a fresh one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Epoch(u64);

impl Default for Epoch {
    fn default() -> Self {
        Epoch(NEXT_EPOCH.fetch_add(1, Ordering::Relaxed))
    }
}

/// log2 of the page size of [`MemoryMap::pages`].
const PAGE_BITS: u32 = 16;
const PAGE_SIZE: u32 = 1 << PAGE_BITS;
/// log2 of the granule in which the map tracks cached code ([`MemoryMap::code_pages`]): a write
/// changes the code epoch only if it touches a 4 KiB page the CPU cached code from, so data
/// next to code (and flash programmed away from it) leaves the decoded instructions valid.
const CODE_BITS: u32 = 12;
/// [`Page`] flag: the page is ROM (reads only; writes take the general path, which logs them).
const PAGE_ROM: u32 = 1 << 16;

/// A page wholly inside one RAM or ROM window: bits 0..16 are the page's index within the
/// buffer, bit 16 is [`PAGE_ROM`], bits 24..32 hold the buffer index plus one. 0 for every
/// other page (devices, unmapped space, partly covered pages, buffers past index 254), which
/// take the general path.
#[derive(Clone, Copy, Default)]
struct Page(u32);

/// Where an access lands relative to the window table.
enum Span {
    /// Entirely inside one window; `off` is relative to its base.
    Inside { region: usize, off: u32 },
    /// No byte of it is mapped.
    Unmapped,
    /// Crosses a window boundary, or is partly unmapped.
    Straddle,
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
    /// are dropped and logged (a flash controller device changes it through `DevCtx::buffer_mut`); [`MemoryMap::poke`] can still patch it.
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
            self.map_pages(base, size, b, rom);
        }
    }

    /// Enter the whole pages of a new RAM/ROM window (at `base`, `size` bytes, buffer `b`) in
    /// the page table. Partly covered pages stay on the general path.
    fn map_pages(&mut self, base: u32, size: u32, b: usize, rom: bool) {
        let Some(tag) = u32::try_from(b + 1).ok().filter(|&t| t <= 0xFF) else {
            return;
        };
        if !base.is_multiple_of(PAGE_SIZE) {
            return;
        }
        if self.pages.is_empty() {
            self.pages = vec![Page::default(); 1 << (32 - PAGE_BITS)];
        }
        let first = (base >> PAGE_BITS) as usize;
        let rom = if rom { PAGE_ROM } else { 0 };
        for i in 0..(size >> PAGE_BITS) {
            self.pages[first + i as usize] = Page(tag << 24 | rom | i);
        }
    }

    /// The bytes of a RAM/ROM access of `width` bytes at `addr` that lies inside one whole
    /// page of the page table (and, for `write`, not in ROM); `None` sends it down the
    /// general path.
    #[inline]
    fn page_bytes(&mut self, addr: u32, width: u32, write: bool) -> Option<&mut [u8]> {
        let page = self.pages.get((addr >> PAGE_BITS) as usize)?.0;
        let in_page = addr & (PAGE_SIZE - 1);
        if page == 0 || in_page + width > PAGE_SIZE || (write && page & PAGE_ROM != 0) {
            return None;
        }
        let b = ((page >> 24) as usize).checked_sub(1)?;
        let start = ((page & 0xFFFF) as usize) << PAGE_BITS | in_page as usize;
        if write {
            self.note_write(b, start, start + width as usize - 1);
        }
        let buf = self.buffers.get_mut(b)?;
        buf.data.get_mut(start..start + width as usize)
    }

    /// Whether the CPU may have cached code from 4 KiB page `page` of buffer `b`.
    #[inline]
    fn holds_code(&self, b: usize, page: usize) -> bool {
        self.code_pages
            .get(b)
            .and_then(|pages| pages.get(page))
            .copied()
            .unwrap_or(false)
    }

    /// A write is about to change bytes `first..=last` of buffer `b` (at most two code pages
    /// apart): if the CPU may have cached code from them, change the code epoch.
    #[inline]
    fn note_write(&mut self, b: usize, first: usize, last: usize) {
        let (p, q) = (first >> CODE_BITS, last >> CODE_BITS);
        if self.holds_code(b, p) || (q != p && self.holds_code(b, q)) {
            self.code_changed();
        }
    }

    /// A device changed bytes `start..end` of buffer `b` (any length).
    fn note_range(&mut self, b: usize, start: usize, end: usize) {
        if end > start
            && (start >> CODE_BITS..=(end - 1) >> CODE_BITS).any(|p| self.holds_code(b, p))
        {
            self.code_changed();
        }
    }

    /// Bytes the CPU may have cached as code can have changed.
    fn code_changed(&mut self) {
        self.code_epoch = Epoch::default();
    }

    /// Mark the 4 KiB page holding `addr` as code for [`Bus::cache_code`], if it lies in a whole
    /// RAM/ROM page of the page table; `false` otherwise.
    fn mark_code(&mut self, addr: u32) -> bool {
        let Some(&Page(page)) = self.pages.get((addr >> PAGE_BITS) as usize) else {
            return false;
        };
        let Some(b) = ((page >> 24) as usize).checked_sub(1) else {
            return false;
        };
        let code_page = (((page & 0xFFFF) as usize) << PAGE_BITS
            | (addr & (PAGE_SIZE - 1)) as usize)
            >> CODE_BITS;
        if self.code_pages.len() <= b {
            self.code_pages.resize(b + 1, Vec::new());
        }
        let pages = &mut self.code_pages[b];
        if pages.len() <= code_page {
            pages.resize(code_page + 1, false);
        }
        pages[code_page] = true;
        true
    }

    #[inline]
    fn fast_read<const N: usize>(&mut self, addr: u32) -> Option<[u8; N]> {
        let bytes = self.page_bytes(addr, N as u32, false)?;
        <[u8; N]>::try_from(&*bytes).ok()
    }

    #[inline]
    fn fast_write(&mut self, addr: u32, v: &[u8]) -> bool {
        match self.page_bytes(addr, v.len() as u32, true) {
            Some(dst) => {
                dst.copy_from_slice(v);
                true
            }
            None => false,
        }
    }

    /// Map `dev` over `base..base + size` and return its id (the add order, starting at 0). The
    /// device sees offsets relative to `base`.
    ///
    /// # Panics
    ///
    /// If the window would overlap another or run past 0xFFFF_FFFF, or more than 65 536
    /// devices are added (static layout bugs).
    pub fn add_device(
        &mut self,
        name: &'static str,
        base: u32,
        size: u32,
        dev: Box<dyn Device>,
    ) -> u16 {
        let d = self.slots.len();
        let id = u16::try_from(d).expect("too many devices");
        let region = Region {
            name: name.to_owned(),
            base,
            size,
            target: Target::Device(d),
        };
        region::insert(&mut self.regions, region);
        self.slots.push(Slot {
            name,
            base,
            dev,
            irq_out: 0,
        });
        id
    }

    // ---- device runtime ----

    /// Run `f` on slot `id` with a context. `None` if there is no such slot.
    fn call<R>(
        &mut self,
        id: usize,
        secure: bool,
        f: impl FnOnce(&mut dyn Device, &mut DevCtx<'_>) -> R,
    ) -> Option<R> {
        let slot = self.slots.get_mut(id)?;
        self.device_activity = true;
        let old_irq = slot.irq_out;
        // `id` fits: add_device checked every slot index.
        let mut ctx = DevCtx::new(
            self.now,
            &mut slot.irq_out,
            &mut self.pulses,
            &mut self.events,
            id as u16,
            &mut self.buffers,
            &mut self.log,
            slot.base,
            self.pc_hint,
            secure,
        );
        let r = f(slot.dev.as_mut(), &mut ctx);
        let written = ctx.written();
        if slot.irq_out != old_irq {
            self.update_irq_lines();
        }
        match written {
            Written::Nothing => {}
            Written::Range { buf, start, end } => self.note_range(buf, start, end),
            Written::Everything => self.code_changed(),
        }
        Some(r)
    }

    fn slot_id(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.name == name)
    }

    /// The device called `name`, if it is a `T`.
    pub fn device<T: Device>(&self, name: &str) -> Option<&T> {
        let slot = &self.slots[self.slot_id(name)?];
        (slot.dev.as_ref() as &dyn std::any::Any).downcast_ref::<T>()
    }

    /// Run `f` on the device called `name` with a (secure, host) context, if it is a `T`.
    pub fn with_device<T: Device, R>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut T, &mut DevCtx<'_>) -> R,
    ) -> Option<R> {
        let id = self.slot_id(name)?;
        self.call(id, true, |dev, ctx| {
            (dev as &mut dyn std::any::Any)
                .downcast_mut::<T>()
                .map(|t| f(t, ctx))
        })?
    }

    /// Every device's registers at cycle `now` ([`Device::inspect`]), in add order; devices
    /// with no rows are left out. Changes nothing (no device call, no log entry).
    pub fn inspect_devices(&self, now: u64) -> Vec<DeviceInspect> {
        self.slots
            .iter()
            .filter_map(|s| {
                let regs = s.dev.inspect(now);
                (!regs.is_empty()).then_some(DeviceInspect {
                    name: s.name,
                    base: s.base,
                    regs,
                })
            })
            .collect()
    }

    /// Device `id`'s requested event is due: call its [`Device::event`] at `self.now`. An
    /// unknown id is logged and ignored.
    pub fn device_event(&mut self, id: u16) {
        if self
            .call(usize::from(id), true, |d, ctx| d.event(ctx))
            .is_none()
        {
            log::warn!("device_event: no device {id}");
        }
    }

    /// Reset every device, in add order.
    pub fn reset_devices(&mut self, kind: ResetKind) {
        for id in 0..self.slots.len() {
            self.call(id, true, |d, ctx| d.reset(kind, ctx));
        }
    }

    /// Level IRQ lines: the OR of every device's lines (bit n = line n).
    pub fn irq_lines(&self) -> u64 {
        self.irq_lines
    }

    /// Recompute [`irq_lines`](Self::irq_lines) after a device's lines changed.
    fn update_irq_lines(&mut self) {
        self.irq_lines = self.slots.iter().fold(0, |acc, s| acc | s.irq_out);
    }

    /// Whether any device ran or had its state loaded since the last call: until one does,
    /// [`irq_lines`](Self::irq_lines), pulses and event requests cannot have changed.
    pub fn take_device_activity(&mut self) -> bool {
        std::mem::take(&mut self.device_activity)
    }

    /// [`take_device_activity`](Self::take_device_activity) without clearing it.
    #[inline]
    pub fn device_activity(&self) -> bool {
        self.device_activity
    }

    /// Edge IRQs requested since the last call, in order.
    pub fn take_pulses(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.pulses)
    }

    /// Event requests since the last call, in order: `(device id, Some(at))` to (re)schedule,
    /// `(device id, None)` to cancel.
    pub fn take_event_requests(&mut self) -> Vec<(u16, Option<u64>)> {
        std::mem::take(&mut self.events)
    }

    /// Backing bytes of the RAM or ROM called `name` (the first one if names repeat).
    pub fn buffer_mut(&mut self, name: &str) -> Option<&mut [u8]> {
        self.code_changed();
        self.buffers
            .iter_mut()
            .find(|b| b.name == name)
            .map(|b| b.data.as_mut_slice())
    }

    /// Every device's state and IRQ lines, in add order.
    pub fn device_states(&self) -> Vec<DeviceState> {
        self.slots
            .iter()
            .map(|s| DeviceState {
                name: s.name.to_owned(),
                irq_out: s.irq_out,
                state: s.dev.save(),
            })
            .collect()
    }

    /// Restore [`MemoryMap::device_states`]. The device names and their order must match this
    /// map's, else [`StateError::Mismatch`]. All or nothing: on any error every device is left
    /// as it was.
    pub fn load_device_states(&mut self, states: &[DeviceState]) -> Result<(), StateError> {
        let names = |it: &mut dyn Iterator<Item = &str>| it.collect::<Vec<_>>().join(", ");
        if states.len() != self.slots.len()
            || states
                .iter()
                .zip(&self.slots)
                .any(|(st, s)| st.name != s.name)
        {
            return Err(StateError::Mismatch(format!(
                "devices [{}] in the state, [{}] in this machine",
                names(&mut states.iter().map(|s| s.name.as_str())),
                names(&mut self.slots.iter().map(|s| s.name)),
            )));
        }
        let backups: Vec<Vec<u8>> = self.slots.iter().map(|s| s.dev.save()).collect();
        for (i, st) in states.iter().enumerate() {
            if let Err(e) = self.slots[i].dev.load(&st.state) {
                for (slot, backup) in self.slots[..i].iter_mut().zip(&backups) {
                    if slot.dev.load(backup).is_err() {
                        log::error!("device {}: could not restore its own state", slot.name);
                    }
                }
                return Err(e);
            }
        }
        self.device_activity = true;
        for (slot, st) in self.slots.iter_mut().zip(states) {
            slot.irq_out = st.irq_out;
        }
        self.update_irq_lines();
        Ok(())
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
        self.code_changed();
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

    // ---- watchpoints and bus monitor ----

    /// Replace the watchpoints. Every range is validated first; on `Err` nothing changes.
    /// Read/write ranges are checked on every guest data access; exec ranges are only stored
    /// (the `Machine` checks them before it steps).
    pub fn set_watchpoints(&mut self, ranges: Vec<WatchRange>) -> Result<(), WatchError> {
        self.watch.set_ranges(ranges)
    }

    /// The watchpoints as last set, exec ranges included.
    pub fn watchpoints(&self) -> Vec<WatchRange> {
        self.watch.ranges()
    }

    /// Record guest data accesses overlapping `(start, len)`; `None` or len 0 turns the monitor
    /// off. The records are kept either way.
    pub fn set_bus_monitor(&mut self, range: Option<(u32, u32)>) {
        self.watch.set_monitor(range);
    }

    /// The bus monitor's records, oldest first (at most [`crate::BUS_RING_CAP`]).
    pub fn bus_records(&self) -> Vec<BusRecord> {
        self.watch.records()
    }

    pub fn clear_bus_records(&mut self) {
        self.watch.clear_records();
    }

    /// The first access that matched a read/write watchpoint since the last call. Later
    /// matches are ignored until this is taken.
    pub fn take_watch_hit(&mut self) -> Option<WatchHit> {
        self.watch.take_hit()
    }

    /// Whether any read/write watchpoint or an active bus monitor is set, so guest data
    /// accesses are observed.
    #[inline]
    pub fn watching(&self) -> bool {
        self.watch.watching()
    }

    /// Hand a guest write to the watcher (only while [`watching`](Self::watching)).
    #[inline(always)]
    fn observe_write(&mut self, addr: u32, width: u8, value: u32) {
        if self.watch.watching() {
            let (pc, now) = (self.pc_hint, self.now);
            self.watch
                .observe(addr, width, AccessDir::Write, value, pc, now);
        }
    }

    /// Hand a guest read to the watcher unless it is an instruction fetch.
    #[inline(always)]
    fn observe_read(&mut self, addr: u32, width: u8, a: Access, value: u32) {
        if self.watch.watching() && a.kind != AccessKind::Fetch {
            let (pc, now) = (self.pc_hint, self.now);
            self.watch
                .observe(addr, width, AccessDir::Read, value, pc, now);
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
    fn span(&mut self, addr: u32, width: u8) -> Span {
        // Device polling hits the same window again and again: try the last one first.
        let last = self
            .regions
            .get(self.last_region)
            .filter(|r| r.base <= addr && u64::from(addr) < r.end())
            .map(|_| self.last_region);
        if let Some(i) = last.or_else(|| region::find(&self.regions, addr)) {
            self.last_region = i;
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
    fn read_inside(&mut self, region: usize, off: u32, width: u8, secure: bool) -> Option<u32> {
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
            Target::Device(d) => self
                .call(d, secure, |dev, ctx| dev.read(off, width, ctx))
                .map(|v| v & mask(width)),
        }
    }

    /// Write inside one window. Returns whether the write took effect; ROM and windows with
    /// nothing behind them return `false` and the caller logs the dropped write.
    fn write_inside(&mut self, region: usize, off: u32, width: u8, v: u32, secure: bool) -> bool {
        let Some(target) = self.regions.get(region).map(|r| r.target) else {
            return false;
        };
        match target {
            Target::Ram(b) => {
                let start = off as usize;
                self.note_write(b, start, start + usize::from(width) - 1);
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
            Target::Device(d) => self
                .call(d, secure, |dev, ctx| {
                    dev.write(off, width, v & mask(width), ctx)
                })
                .is_some(),
        }
    }

    /// A guest read of `width` bytes: the value, and whether any byte of it was unmapped.
    /// Unmapped bytes read as 0 and are logged as `kind`. An access that straddles windows or
    /// is partly unmapped is split into byte accesses, so each unmapped byte is logged on its
    /// own and devices never see an access that runs off their end.
    fn read(&mut self, addr: u32, width: u8, kind: UnmappedKind, secure: bool) -> (u32, bool) {
        match self.span(addr, width) {
            Span::Inside { region, off } => {
                if let Some(v) = self.read_inside(region, off, width, secure) {
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
                if !self.write_inside(region, off, width, v, secure) {
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

/// Every access first tries the page table (whole RAM/ROM pages: a plain copy, nothing to log),
/// then the general path. While anything is watching, every data access (not fetches) is then
/// shown to the watcher with the value read or written.
impl Bus for MemoryMap {
    #[inline]
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        let v = match self.fast_read(addr) {
            Some([b]) => b,
            None => self.bus_read(addr, 1, a)? as u8,
        };
        self.observe_read(addr, 1, a, u32::from(v));
        Ok(v)
    }
    #[inline]
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        let v = match self.fast_read(addr) {
            Some(b) => u16::from_le_bytes(b),
            None => self.bus_read(addr, 2, a)? as u16,
        };
        self.observe_read(addr, 2, a, u32::from(v));
        Ok(v)
    }
    #[inline]
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        let v = match self.fast_read(addr) {
            Some(b) => u32::from_le_bytes(b),
            None => self.bus_read(addr, 4, a)?,
        };
        self.observe_read(addr, 4, a, v);
        Ok(v)
    }
    #[inline]
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        if !self.fast_write(addr, &[v]) {
            self.write(addr, 1, u32::from(v), a.secure);
        }
        self.observe_write(addr, 1, u32::from(v));
        Ok(())
    }
    #[inline]
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        if !self.fast_write(addr, &v.to_le_bytes()) {
            self.write(addr, 2, u32::from(v), a.secure);
        }
        self.observe_write(addr, 2, u32::from(v));
        Ok(())
    }
    #[inline]
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        if !self.fast_write(addr, &v.to_le_bytes()) {
            self.write(addr, 4, v, a.secure);
        }
        self.observe_write(addr, 4, v);
        Ok(())
    }
    fn code_epoch(&self) -> u64 {
        self.code_epoch.0
    }
    /// Code in whole RAM/ROM pages (the page table) can be cached; writes to those pages,
    /// `buffer_mut`, `poke` and devices writing buffers change the epoch.
    fn cache_code(&mut self, addr: u32, len: u32) -> bool {
        let Some(last) = addr.checked_add(len.saturating_sub(1)) else {
            return false;
        };
        self.mark_code(addr) && self.mark_code(last)
    }
}
