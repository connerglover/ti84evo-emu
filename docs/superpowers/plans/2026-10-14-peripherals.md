# TI-84 Evo Emulator — Plan 2: Peripherals → booting OS

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Model the TI-84 Evo SoC peripherals the firmware needs, in boot order, so the real OS boots through TF-M (M1), hands off to the Non-secure BSP (M2), shows the boot logo (M3) and launches OS image 1 (M4), with flash persistence, save states and the CLI flags of spec §6.2.

**Architecture:** `evo-hw` gets a device runtime: `Device` methods receive a `DevCtx` that carries the cycle count, the device's IRQ lines, a one-event-per-device scheduling request, access to named backing buffers (flash) and the unmapped log. `Machine` folds device IRQ lines into the NVIC (level-sensitive), turns event requests into `Scheduler` events and resets devices on `SYSRESETREQ`. Each peripheral is one file under `crates/evo-hw/src/dev/`, registered in `evo_hw::soc::attach`. Register semantics come from the research imported into `docs/hardware/` by Task 1; tasks cite those docs and pin the values tests need.

**Tech Stack:** Rust 2024 (toolchain 1.98), existing crates (`armv8m`, `evo-hw`, `evo-image`, `evo-core`, `evo-cli`), new: `postcard` 1.1 (`use-std`; save-state encoding), `sha2` 0.10 (`compress256` for the SHA engine), `png` 0.17 (screenshots, `evo-cli` only), `thiserror` 2 in `evo-hw`/`evo-core`.

**Spec:** `docs/superpowers/specs/2026-10-13-ti84evo-emulator-design.md` (§4 CPU, §5 hardware, §6.1–6.2 API/CLI, §6.5 save states, §8.2 env-gated tests). Plan 1: `docs/superpowers/plans/2026-10-13-foundation.md`.

**Follow-on (not in this plan):** Plan 3 — `evo-gui` per spec §6.4 (classic TI-84 Plus CE web-emulator layout, Evo styling, no TI logo/artwork, keypad as a data table). It consumes `Machine::screen`, `Machine::set_key`, `Machine::save_state/load_state` from this plan. Named keys (key → row/column) are also Plan 3/M4 work; this plan addresses keys by matrix position.

## Global Constraints

- All Plan 1 Global Constraints carry over (no TI material committed; `EVO_OS_IMAGE` env-gated tests that print a skip and return when unset; no panics on guest behavior or user input; `thiserror` host errors; unmapped accesses never panic; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass at every commit).
- No TI code, data or artwork is committed. `docs/hardware/` may hold RE notes, evidence addresses and disassembly excerpts of **at most 8 instructions**; no byte tables or hex dumps of more than 8 bytes. No screenshots of the OS are committed. M3 compares the frame against a committed SHA-256 constant; a hash of a frame contains no TI data.
- The bundle path is never committed; local paths (RE folder, scratchpad, temp dirs) never appear in committed files.
- **Research input.** The Plan 2 research files live outside the repo. The orchestrator gives their folder in env `EVO_RE_DIR`; it holds `security-mpc-spctrl-cru.md`, `timer.md`, `uart.md`, `spi-master.md`, `spif-flash-icache.md`, `boot-after-its.md` and `critique.md`. Task 1's implementer and reviewer both read these files from `$EVO_RE_DIR`. After Task 1, every task cites only `docs/hardware/`.
- **Host access to devices.** Every `Machine` host API that changes a device (Tasks 8, 13, 15, 17, 19) goes through the private helper `Machine::with_dev` of Task 4, which sets `bus.now = sched.now` first and calls `sync_bus` afterwards.
- Time: one `Scheduler::now` unit is one CPU cycle; `evo_hw::CPU_HZ = 156_000_000`. Devices derive every rate from it.
- The machine is deterministic: no host clock or host RNG inside `Machine` (TRNG uses a fixed seed, RTC starts at 00:00:00). Same bundle + same inputs ⇒ same cycles, UART bytes and screen.
- Device IRQ lines are level-sensitive (`DevCtx::set_irq`); the only edge source is the ON key (`DevCtx::pulse_irq`).
- A register a device does not model reads 0, ignores writes and is reported with `DevCtx::unhandled` (it appears in the unmapped log). No device panics on any offset, width or value.
- A device that keeps state across a System reset says so in its doc and in its `reset`.
- When a task decides something its `docs/hardware/<block>.md` lists as open, the same commit updates that doc.
- Milestone tests run in release, with `EVO_OS_IMAGE` set: `cargo test --release -p evo-core --test milestones -- --nocapture`; a milestone step passes only if no output line contains `skipping`. `Os::boot` returns `None` (printing `skipping: milestone tests need a release build`) under `cfg!(debug_assertions)`, so the debug `cargo test --workspace` gate stays fast even with `EVO_OS_IMAGE` set. An executor without `EVO_OS_IMAGE` stops at the first milestone step and reports; it does not mark the step done.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **An asserted device IRQ line across `SYSRESETREQ`** — `Cpu::reset` clears NVIC levels; a line a device still holds high after the reset must pend again, not stay silently lost. Test `asserted_line_repends_after_system_reset`, Task 4.
2. **A corrupt, truncated or wrong-size `--flash-file`** — must be a reported error (exit 1) that leaves the file untouched, not a panic or an overwrite with an erased image. Tests in Task 11 (`NvState::read_file`) and `cli_bad_flash_file_exits_1_and_keeps_file`, Task 13.
3. **A save state from another bundle, another version or a truncated file** — `load_state` returns `StateError` and the machine is byte-for-byte unchanged. The header carries a bundle fingerprint for the first case. Tests `load_state_rejects_*` (incl. `load_state_rejects_other_bundle`), Task 22.
4. **A guest SPIF command with a huge data length** (`LEN = 0xFFFF_FFFF`) — must not allocate gigabytes; read data is generated lazily. Tests `read_phase_is_lazy` (Task 11) and `huge_read_len_is_lazy` (Task 12).
5. **A screenshot while the LCDC points outside SRAM or uses another pixel format** — `Lcdc::frame` returns `None`, never panics or reads out of bounds. Test `frame_outside_sram_is_none`, Task 18.

---

## File Structure

```
docs/hardware/
  memory-map.md, discovery-log.md        updated (Task 1)
  security.md      MPCs, SPCTRL/PPC, CRU, PPB 0xE004_3018, LED pattern generator
  timer.md uart.md spi-panel.md spif-nor.md crypto.md (TRNG, SHA)
  gpio.md lcdc.md (LCDC + backlight PWM) keypad.md rtc.md usb.md (USB + Type-C)
  boot-flow.md     secure boot steps, BL2 shared data, NS hand-off, BSP, image-1 entry, milestones
crates/armv8m/src/scs/mod.rs             internal PPB only; PPB misses recorded (Task 2)
crates/armv8m/src/cpu.rs                 Cpu::restore_from (Task 22)
crates/evo-hw/src/
  lib.rs           re-exports, CPU_HZ
  device.rs        Device, DevCtx, ResetKind, StateError, save_serde/load_serde, DevHarness
  memmap.rs        device slots (IRQ lines, event requests), with_device/device, reset, states
  soc.rs           attach(): every SoC device at its base; FLASH_BUFFER / SRAM_BUFFER names
  nor.rs           NorFlash (W25Q128-style command engine), NvState (flash file)
  dev/mod.rs storage.rs mpc.rs sysctl.rs timer.rs uart.rs spi.rs spif.rs crypto.rs
      gpio.rs lcdc.rs keypad.rs rtc.rs usb.rs   (types at evo_hw::dev::<file>::<Type>, Task 5)
crates/evo-hw/tests/   devices.rs + one file per dev module
crates/evo-core/src/
  machine.rs       device sync, with_dev, resets, 16 MiB flash, 1.5 MiB SRAM, BL2 header, bundle_id
  host.rs          host APIs: UART, keys, screen, GPIO inputs, USB CC, power-off, NV state
  keys.rs          key-script parser
  state.rs         save/load state
crates/evo-core/tests/  machine.rs, state.rs, milestones.rs (env-gated M1–M4)
apps/evo-cli/src/run.rs  sliced run loop, --uart/--uart-in/--flash-file/--screenshot/--after/--keys/--load-state/--save-state
```

---

### Task 1: Import the hardware research into `docs/hardware/`

The research register maps live outside the repo. This task commits them, scrubbed and corrected, so every later task cites repo paths. It writes docs only.

**Files:**
- Create: `docs/hardware/{security,timer,uart,spi-panel,spif-nor,crypto,boot-flow,gpio,lcdc,keypad,rtc,usb}.md`
- Modify: `docs/hardware/memory-map.md`, `docs/hardware/discovery-log.md`

**Interfaces — Produces:** the doc paths above; later tasks cite them by section.

Source → destination. The research files are read from `$EVO_RE_DIR` (Global Constraints); the reviewer checks the docs against the same files.

| research file / section | doc |
|---|---|
| `security-mpc-spctrl-cru.md` (all) + its Critic additions A–D | `security.md` |
| `timer.md` minus "Related blocks → RTC", with Critic additions A–B | `timer.md` |
| `timer.md` "Related blocks → RTC" + Critic addition C | `rtc.md` |
| `uart.md` | `uart.md` |
| `spi-master.md` | `spi-panel.md` |
| `spif-flash-icache.md` + Critic additions A–D, merged with `boot-after-its.md` §4.3 (+0x18 bit 0 TX-ready wait 0x100b6edc, TX loop 0x100b1e2a that spins forever on level 0, XIP coherence); duplicates removed | `spif-nor.md` (ICACHE stays in it) |
| `boot-after-its.md` §4.1–4.2 | `crypto.md` |
| `boot-after-its.md` §0–3, §4.4, §5–6, C.1, C.6, C.7 | `boot-flow.md` |
| `boot-after-its.md` C.2 / C.3+C.4 / C.5 / §4.5+C.1 (0x4010_C000 row) | `gpio.md` / `lcdc.md` / `keypad.md` / `usb.md` |
| `critique.md` §7 (which unmapped access each block owns) | the peripheral table of `memory-map.md` (an "accesses seen" column) |
| `critique.md` §0 X1–X8 | `memory-map.md` § "Device runtime decisions": one line each, pointing to Tasks 3–4 (X1 `DevCtx::now`, X2 `CPU_HZ`, X3 `set_irq`, X4/X5 `buffer_mut`, X6 16 MiB, X7 `ResetKind`, X8 `--flash-file` Task 13) |

- [ ] **Step 1: Write the docs, applying every correction in place** (delete the superseded text; no "Critic additions" sections survive):
  1. 0x5020_2B00 is a two-channel LED pattern generator reached on every boot (writes at 0x100b4704..0x100b474c; `pattern_set(9)`, `delay_ms(1900)`, `pattern_set(0)`); it is not SPIF clocking or QSPI drive. Keep pattern rows idx 0 and 9 only; say "10-entry table at 0x100b8184, not reproduced".
  2. SPCTRL +0xC0: the two-consumer table (OTP service 0x100b6748 → 0x3000_1B20 vs 0x1008_1A00; boot data 0x100bfd6a → 0x1800_E000 vs 0x1008_1800).
  3. CRU: downstream table (SPI CTRL1 0x12 and UART divisor 13 hold only with `+0x04 = 0x0002_0001`, `+0x1C = 0x3433`, `+0xFC = 0x0011_0000`); `+0x20` bit 4 releases the backlight PWM; `+0x10` bit 30 is a chip-variant bit, not a ready flag.
  4. Timer: keep Critic addition B's arithmetic (u64/u128; LOAD + 1 overflows u32; `since` advances by whole ticks only) as the model, written as the Task 7 pseudocode, replacing the original formula.
  5. Block identities: 0x4010_5000 GPIO (IRQ 24), 0x4010_3000 keypad (IRQ 13; ON key IRQ 0), 0x4010_2000 RTC (IRQ 15), 0x4010_4000 backlight PWM, 0x4010_C000 USB Type-C (probable, IRQ 28), 0x4000_4000 Arm PL111 CLCD (IRQ 14).
  6. SPIF: the 0x00CF_0000 erase is conditional (blank check at 0x100d78b6; with 16 MiB of 0xFF it never runs); flash sharing uses `DevCtx::buffer_mut("flash")` (Task 3), not `Rc<RefCell>`; START is level-triggered; FIFO_LEVEL is capped at 64.
  7. UART: the descriptors at 0x0017_88E8/EC/F0 are PWM, RTC and SPI (not one SPI config); `[0x3000_19C8]` is the CRU reference clock (24 MHz); the BSP runs and prints `Starting BSP 7.1.0.4413`.
  8. SPI: the NS BSP runs, so the panel is initialised twice (secure splash, then BSP). Replace the default init table's parameter bytes by the command names only.
  9. Boot flow: the BL2 shared-data header is written at power-on **and** on every system reset; LCDC VComp → IRQ 14 is the first launch-gate candidate.
  10. `discovery-log.md`: add a "Corrections from Plan 2 research" section — SPI uses 9-bit frames, not 16; CRU +0x10 bit 30 is a variant bit; 0x4010_5000 is GPIO; the 0x00CF_0000 reads were an artifact of the 8 MiB flash; ITS/PS live at 0x0009_0000–0x000A_FFFF.
  11. `memory-map.md`: flash 16 MiB (0x0000_0000 / 0x1000_0000), SRAM 1.5 MiB = 3 × 512 KiB (0x2000_0000..0x2017_FFFF / 0x3000_0000..), BL2 shared data at 0x3000_0000, a peripheral table (name, base, size, IRQ, doc link, accesses seen — the devices Tasks 5–20 implement, in `soc::attach` order), the "Device runtime decisions" section above, and answers to open questions 2, 4, 5 and 6.
- [ ] **Step 2: Scrub.** Remove every mention of scratch directories, harness binaries and command lines, log-file names, user names and paths outside the repo; call the experiments "a throwaway emulator build with register fakes". Trim every code excerpt to ≤ 8 instruction lines. Replace byte tables (panel init tables, LED pattern tables, keypad maps) and long firmware strings with their table address and a one-line description; short boot-log lines the milestone tests match (e.g. `Booting TF-M v2.1.1`) stay.
- [ ] **Step 3: Verify.**
  - `rg -n -i "scratch|appdata|ti84evo-re|documents[\\/]|[\\/]temp[\\/]|c:[\\/]|/c/users|users[\\/]conner|un11|explore\.exe|\.js\b|EVO_RE_DIR" docs/hardware` → no output.
  - `rg -n "([0-9A-Fa-f]{2}[ ,]+){8,}" docs/hardware` → every hit is reviewed by hand; none is a firmware byte table (register values and addresses are fine).
  - `awk 'FNR==1{n=0} /^```/{if(f){if(n>8)print FILENAME": block ending line "FNR" has "n" lines"; f=0}else{f=1;n=0}; next} f{n++}' docs/hardware/*.md` → lists every fenced block longer than 8 lines; each listed block is model pseudocode or a table, never disassembly (the reviewer checks the list).
- [ ] **Step 4: Commit** `docs: hardware register maps from the Plan 2 research`.

### Task 2: Make every PPB access the CPU does not implement visible

**Files:**
- Modify: `crates/armv8m/src/scs/mod.rs`, `crates/armv8m/src/lib.rs`, `crates/armv8m/src/scs/mpu.rs` (comment only), `crates/evo-hw/src/memmap.rs` (doc comment only)
- Test: `crates/armv8m/tests/exceptions.rs`

**Interfaces — Produces:**
```rust
// armv8m::scs — PPB_END is renamed so the change of meaning is explicit (its only user is is_ppb)
pub const INTERNAL_PPB_END: u32 = 0xE003_FFFF; // internal PPB; 0xE004_0000.. (EPPB) now goes to the Bus
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpbMiss { pub addr: u32, pub width: u8, pub write: Option<u32>, pub secure: bool }
impl Cpu { pub fn take_ppb_misses(&mut self) -> Vec<PpbMiss>; }   // drains; guest accesses only
```
On the Cortex-M33 0xE004_0000..=0xE00F_FFFF is the external PPB, reached over the system bus: `is_ppb` covers only 0xE000_0000..=0xE003_FFFF, so EPPB data accesses reach `Bus` (and the unmapped log). A guest `ppb_read`/`ppb_write` to an unimplemented internal register still reads 0 / ignores the write but also pushes a `PpbMiss` (field `#[serde(skip)] ppb_misses: Vec<PpbMiss>` on `Cpu`; the `log::warn!` becomes `log::debug!`). Host calls (`ppb_read32`, `ppb_peek32`, `ppb_write32`) record nothing. Docs: the `scs/mod.rs` module doc and the `memmap.rs` comment ("The CPU intercepts the PPB (0xE000_0000..=0xE00F_FFFF)") now say the CPU intercepts 0xE000_0000..=0xE003_FFFF and EPPB reaches the bus. `mpu.rs` `permissions` keeps 0xE00F_FFFF on purpose (all of the PPB, EPPB included, stays privileged-only); add a comment saying so. The existing `exceptions.rs:564` assertion `ppb_peek32(0xE00F_FFFC) == None` still holds (host peeks outside the internal PPB return `None`).

- [ ] **Step 1: Write the failing tests** in `tests/exceptions.rs`:
  - `eppb_access_reaches_the_bus`: a test bus wrapping `FlatBus` that answers `0x5555_5555` for reads ≥ 0xE004_0000 and records writes; program `ldr r0,=0xE004_3018; ldr r1,[r0]; orr r1,#2; str r1,[r0]` → r1 = 0x5555_5557, recorded write `(0xE004_3018, 0x5555_5557)`, `take_ppb_misses()` empty.
  - `unimplemented_internal_ppb_is_recorded`: `ldr r1,[0xE000_1000]` (DWT_CTRL) then `str r2,[0xE000_0E80]` (ITM_TCR, r2 = 1) → r1 = 0; `take_ppb_misses() == [PpbMiss{addr:0xE000_1000,width:4,write:None,secure:true}, PpbMiss{addr:0xE000_0E80,width:4,write:Some(1),secure:true}]`; a second call returns `[]`.
  - `host_ppb_reads_record_nothing`: `cpu.ppb_read32(0xE000_1000)` → 0, `take_ppb_misses()` empty.
- [ ] **Step 2:** `cargo test -p armv8m --test exceptions` → FAIL (`take_ppb_misses` missing).
- [ ] **Step 3:** Implement as described.
- [ ] **Step 4:** `cargo test -p armv8m` → PASS.
- [ ] **Step 5: Commit** `feat(armv8m): EPPB goes to the bus; record unimplemented PPB accesses`.

### Task 3: Device runtime in `evo-hw`

**Files:**
- Create: `crates/evo-hw/src/device.rs`, `crates/evo-hw/tests/devices.rs`
- Modify: `crates/evo-hw/src/{lib.rs,memmap.rs,region.rs}` (move `Device` out of `region.rs`), `crates/evo-hw/Cargo.toml` (`postcard`, `thiserror`, `log`), root `Cargo.toml` (`[workspace.dependencies] postcard = { version = "1.1", default-features = false, features = ["use-std"] }`), `crates/evo-hw/tests/memmap.rs` (its `Probe` uses the new trait)

**Interfaces — Produces:**
```rust
// evo_hw (lib.rs re-exports all of these)
pub const CPU_HZ: u64 = 156_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetKind { PowerOn, System }
pub trait Device: std::any::Any + Send {
    fn read(&mut self, off: u32, width: u8, ctx: &mut DevCtx<'_>) -> u32;
    fn write(&mut self, off: u32, width: u8, v: u32, ctx: &mut DevCtx<'_>);
    /// The event requested with `DevCtx::schedule_at` is due (`ctx.now` >= its time).
    fn event(&mut self, _ctx: &mut DevCtx<'_>) {}
    fn reset(&mut self, kind: ResetKind, ctx: &mut DevCtx<'_>);
    fn save(&self) -> Vec<u8>;
    fn load(&mut self, state: &[u8]) -> Result<(), StateError>;
}
pub struct DevCtx<'a> { pub now: u64, /* private: irq, pulses, event, buffers, log, base, pc, secure */ }
impl DevCtx<'_> {
    pub fn set_irq(&mut self, irq: u16, asserted: bool);   // irq < 64; else log::warn + ignore
    pub fn pulse_irq(&mut self, irq: u16);
    pub fn schedule_at(&mut self, at: u64);                // replaces this device's pending event
    pub fn cancel_event(&mut self);
    pub fn buffer_mut(&mut self, name: &str) -> Option<&mut [u8]>;   // RAM or ROM bytes by name
    pub fn unhandled(&mut self, off: u32, width: u8, write: Option<u32>); // -> unmapped log at base+off
}
#[derive(thiserror::Error, Debug)]
pub enum StateError {
    #[error("state decode: {0}")] Decode(#[from] postcard::Error),
    #[error("not a save state")] BadMagic,
    #[error("save-state version {found}, expected {expected}")] Version { found: u32, expected: u32 },
    #[error("save state does not fit this machine: {0}")] Mismatch(String),
}
pub fn save_serde<T: Serialize>(v: &T) -> Vec<u8>;
pub fn load_serde<T: serde::de::DeserializeOwned>(dst: &mut T, state: &[u8]) -> Result<(), StateError>;
/// Drives one device outside a MemoryMap (unit tests).
pub struct DevHarness { pub now: u64, pub irq: u64, pub pulses: Vec<u16>,
                        pub event: Option<Option<u64>>, pub log: UnmappedLog, /* buffers */ }
impl DevHarness {
    pub fn new() -> Self;
    pub fn with_buffer(self, name: &str, data: Vec<u8>) -> Self;
    pub fn buffer(&self, name: &str) -> &[u8];
    pub fn read(&mut self, d: &mut dyn Device, off: u32, width: u8) -> u32;
    pub fn write(&mut self, d: &mut dyn Device, off: u32, width: u8, v: u32);
    pub fn reset(&mut self, d: &mut dyn Device, kind: ResetKind);
    /// If an event is pending: `now = max(now, at)`, clear it, call `d.event`. Returns whether it fired.
    pub fn fire(&mut self, d: &mut dyn Device) -> bool;
}
// memmap.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceState { pub name: String, pub irq_out: u64, pub state: Vec<u8> }
pub struct MemoryMap {
    /// Cycle count devices see as `ctx.now`; set from `sched.now` by Machine. Not saved state.
    #[serde(skip)] pub now: u64,
    /* existing fields; device slots (device, irq_out, event request, pulses) are #[serde(skip)] like `devices` */
}
impl MemoryMap {
    pub fn add_device(&mut self, name: &'static str, base: u32, size: u32, dev: Box<dyn Device>) -> u16; // id = add order
    pub fn device<T: Device>(&self, name: &str) -> Option<&T>;
    pub fn with_device<T: Device, R>(&mut self, name: &str,
                                     f: impl FnOnce(&mut T, &mut DevCtx<'_>) -> R) -> Option<R>;
    pub fn device_event(&mut self, id: u16);               // unknown id: log::warn + ignore
    pub fn reset_devices(&mut self, kind: ResetKind);
    pub fn irq_lines(&self) -> u64;                         // OR of every device's lines
    pub fn take_pulses(&mut self) -> Vec<u16>;
    pub fn take_event_requests(&mut self) -> Vec<(u16, Option<u64>)>; // (id, Some(at) | None = cancel)
    pub fn buffer_mut(&mut self, name: &str) -> Option<&mut [u8]>;
    pub fn device_states(&self) -> Vec<DeviceState>;
    pub fn load_device_states(&mut self, s: &[DeviceState]) -> Result<(), StateError>; // names/order must match
}
```
Each device slot keeps its own `irq_out: u64`, so devices sharing a line are OR-ed. `with_device` and `device` downcast through `&mut dyn Device as &mut dyn Any` (trait upcasting). `DevCtx` borrows the slot's fields and `buffers`/`log` disjointly; the context's `pc`/`secure` are `pc_hint` and the access's security. The existing `memmap.rs` serde tests (round trip through `serde_json`, `deserialized_map_without_devices_logs_instead_of_panicking`) keep their assertions: a deserialized map has no devices and `now == 0`.

- [ ] **Step 1: Write the failing tests** in `tests/devices.rs` (test devices defined in the file):
  - `irq_lines_or_across_devices`: devices A and B both `set_irq(5, true)`; A deasserts → `irq_lines() == 1 << 5`; B deasserts → 0.
  - `event_requests_drain_in_order`: A `schedule_at(100)`, B `schedule_at(50)`, A `cancel_event()` → `take_event_requests() == [(0, Some(100)), (1, Some(50)), (0, None)]`, then `[]`.
  - `device_event_sees_now`: `map.now = 777; map.device_event(0)` → the device recorded `ctx.now == 777`.
  - `with_device_downcasts`: right type → `Some`, wrong type or name → `None`.
  - `unhandled_goes_to_log`: device at 0x4010_1000 calls `unhandled(0x3C, 4, Some(9))` on a guest write with `pc_hint = 0x1234` → log entry `{addr:0x4010_103C, width:4, kind:Write, write:Some(9), pc:0x1234}`.
  - `buffer_mut_reaches_rom`: ROM "flash" of 16 bytes 0xFF; a device write handler does `ctx.buffer_mut("flash").unwrap()[3] = 0` → `peek(3, 1) == Some(0)`; a guest write to the ROM is still dropped and logged.
  - `reset_devices_reaches_each`: both devices saw `ResetKind::System`.
  - `device_states_roundtrip_and_mismatch`: save → load into a map with the same names restores counters and `irq_out`; a map whose device names differ → `Err(StateError::Mismatch(_))`.
  - `harness_fire`: `schedule_at(10)` then `fire` → `now == 10`, `event == None`, returns true; second `fire` → false.
- [ ] **Step 2:** `cargo test -p evo-hw` → FAIL.
- [ ] **Step 3:** Implement. Update `tests/memmap.rs`'s `Probe` to the new trait (no behavior change).
- [ ] **Step 4:** `cargo test -p evo-hw` → PASS; workspace builds (`evo-core` has no devices yet).
- [ ] **Step 5: Commit** `feat(evo-hw): device runtime (time, IRQ lines, events, buffers, reset, state)`.

### Task 4: `Machine` on the device runtime; real flash/SRAM sizes; BL2 shared data

**Files:**
- Create: `crates/evo-hw/src/soc.rs` (empty `attach` for now)
- Modify: `crates/evo-hw/src/lib.rs` (`pub mod soc;`), `crates/evo-core/src/machine.rs` (also its module doc: 16 MiB flash, 1.5 MiB SRAM instead of 8 MiB / 2 MiB), `crates/evo-core/src/lib.rs`
- Test: `crates/evo-core/tests/machine.rs` (new tests + the size updates below), `crates/evo-core/tests/gdb.rs` (one address)

**Interfaces — Consumes:** Task 2 `Cpu::take_ppb_misses`; Task 3 runtime. **Produces:**
```rust
// evo_hw::soc
pub const FLASH_BUFFER: &str = "flash";
pub const SRAM_BUFFER: &str = "sram";
pub fn attach(map: &mut MemoryMap);   // Tasks 5–20 append their devices here, in this order
// evo_core::machine
pub use evo_hw::CPU_HZ;
pub const FLASH_SIZE: u32 = 16 << 20;         // erased 0xFF, at FLASH_NS and FLASH_S
pub const SRAM_SIZE: u32 = 0x0018_0000;       // 1.5 MiB = 3 x 512 KiB, at SRAM_NS and SRAM_S
pub const BL2_SHARED_DATA: u32 = 0x3000_0000;
pub const BL2_SHARED_HEADER: u32 = 0x0004_2016; // u16 magic 0x2016, u16 total length 4
impl Machine {
    /// The only way host APIs reach a device: `bus.now = sched.now`, `bus.with_device(name, f)`, `sync_bus()`.
    pub(crate) fn with_dev<T: Device, R>(&mut self, name: &str,
                                         f: impl FnOnce(&mut T, &mut DevCtx<'_>) -> R) -> Option<R>;
}
```
Behavior:
- `from_bundle` adds flash/SRAM under `FLASH_BUFFER`/`SRAM_BUFFER`, calls `soc::attach`, writes `BL2_SHARED_HEADER` at `BL2_SHARED_DATA` (via `poke`), then resets the CPU.
- New private `sync_bus(&mut self)`: (1) `lines = bus.irq_lines()`; for each bit that differs from field `applied_irqs: u64` (`#[serde(default)]`) call `cpu.set_irq_level(bit, level)`; store; (2) `cpu.pulse_irq` for each `take_pulses()`; (3) `take_event_requests()` → `sched.schedule(at, EventKind::Peripheral(id))` / `sched.cancel(..)`; (4) each `cpu.take_ppb_misses()` → `bus.log.record` with kind Read/Write and `pc = bus.pc_hint`.
- `bus.now = sched.now` before every CPU step and before firing events; `sync_bus` after every CPU step and after each fired event (so a device IRQ raised during sleep wakes the CPU in the same `idle` pass).
- `fire(EventKind::Peripheral(id))` → `bus.device_event(id)`.
- `system_reset`, in exactly this order: `cpu.set_vtor` + `cpu.reset`; `sched = Scheduler::new()` keeping `now`; `sleep = None; stopped_at = None`; `applied_irqs = 0`; `bus.now = sched.now`; `bus.reset_devices(ResetKind::System)`; rewrite the BL2 header; `sync_bus` (re-pends lines still high and schedules re-requested events on the **new** scheduler). Devices that run across a System reset re-request their event in `reset`.

- [ ] **Step 1: Write the failing tests** (a test device `Probe` at 0x6000_0000 added after `from_bundle`; its register +0 sets `irq_level` IRQ 5, +4 schedules an event at `now + v` that asserts IRQ 5, +8 pulses IRQ 6; it counts `event` and `reset` calls):
  - `flash_is_16_mib_erased`: `peek(0x00FF_FFFC, 4) == Some(0xFFFF_FFFF)`, `peek(0x1000_0000 | 0x00FF_FFFC, 4)` same, `peek(0x0100_0000, 1) == None`.
  - `sram_is_1_5_mib`: `peek(0x2017_FFFC, 4).is_some()`, `peek(0x3017_FFFC, 4).is_some()`, `peek(0x2018_0000, 1) == None`.
  - `bl2_header_at_boot_and_after_system_reset`: `peek(0x3000_0000, 4) == Some(0x0004_2016)`; poke 0; a program that writes AIRCR `0x05FA_0004` → after the reset the word is `0x0004_2016` again.
  - `device_irq_reaches_nvic`: guest stores 1 to 0x6000_0000 with IRQ 5 enabled → after that step `nvic.level(5)` and the handler runs.
  - `asserted_line_repends_after_system_reset` (Review Focus 1): line 5 held high, guest triggers SYSRESETREQ, reset handler enables IRQ 5 → the handler runs after the reset.
  - `device_event_fires_at_deadline`: store 100 to +4 at cycle t → `Probe.events == 1` once `now >= t + 100`, not before.
  - `wfi_wakes_on_device_event`: IRQ 5 enabled, store 1000 to +4 at cycle t, `wfi` → `run(10_000)` executes the handler (r4 incremented) and the handler's first instruction runs at a `now` in `t + 1000..=t + 1003`; without the store → `StopReason::Idle`.
  - `event_rescheduled_in_reset_survives_system_reset`: the Probe's `reset(System)` calls `schedule_at(now + 500)`; guest triggers SYSRESETREQ at cycle t → `Probe.events == 1` once `now >= t + 500` (the request reached the new scheduler).  - `pulse_pends_once`: store to +8 with IRQ 6 enabled → handler runs once.
  - `ppb_misses_and_eppb_land_in_unmapped_log`: program reads 0xE000_1000 and 0xE004_3018 → log has both addresses, kind Read, with the reading PCs.
  - `devices_see_system_reset`: `Probe.resets == 1` after SYSRESETREQ.
- [ ] **Step 2:** `cargo test -p evo-core --test machine` → FAIL.
- [ ] **Step 3:** Implement, and update the existing tests the size change and the BL2 header break:
  - `machine.rs` `machine_boots_synthetic_bundle`: flash bounds `peek(0x0100_0000, 1) == None` and `peek(0x1100_0000, 1) == None` (were 0x0080_0000/0x1080_0000); SRAM `peek(0x3017_FFFC, 4) == Some(0)` (was 0x301F_FFFC), `peek(0x2018_0000, 1) == None`, `peek(0x3018_0000, 1) == None` (were 0x2020_0000/0x3020_0000); comments say 16 MiB / 1.5 MiB.
  - `machine.rs` `system_reset_request_reboots`: the boot counter moves to 0x3000_0100 (literal word at `CODE + 0x1C` and the final `peek`), because 0x3000_0000 now holds the BL2 header.
  - `machine.rs` `image_outside_flash_is_an_error`: `secure_slot_base: 0x00FF_FF00`; its comment says 16 MiB.
  - `gdb.rs` (the refused straddling write): packet `M2017fffe,4:01020304` (was `M201ffffe`) still answers `E..`, and `peek(0x2017_FFFE, 2) == Some(0)` (was 0x201F_FFFE).
  Tests that use an address a later device takes over are moved to 0x6000_0000+ by that later task.
- [ ] **Step 4:** `cargo test --workspace` → PASS.
- [ ] **Step 5:** `cargo run --release -p evo-cli -- run "$EVO_OS_IMAGE" --max-cycles 50000000 --unmapped-out target/unmapped.md` → still the MPC reset loop of `discovery-log.md`, now with the 0xE004_3018 row visible. (Local check, nothing committed from it.)
- [ ] **Step 6: Commit** `feat(evo-core): devices in the machine; 16 MiB flash, 1.5 MiB SRAM, BL2 shared data`.

### Task 5: SIE-200 MPCs (×4)

**Files:** Create `crates/evo-hw/src/dev/{mod.rs,mpc.rs}`, test `crates/evo-hw/tests/mpc.rs`; modify `crates/evo-hw/src/lib.rs` (`pub mod dev;`), `soc.rs`, `docs/hardware/security.md` (geometry decision).

**Path convention (all device tasks):** each device file is a public module of `evo_hw::dev` (`dev/mod.rs` gets one `pub mod <file>;` line per task), and its types live at `evo_hw::dev::<file>::<Type>`, e.g. `evo_hw::dev::mpc::Mpc`, `evo_hw::dev::storage::Storage`, `evo_hw::dev::uart::Uart16550`, `evo_hw::dev::spif::Spif`, `evo_hw::dev::lcdc::{Lcdc, Pwm, Frame}`, `evo_hw::dev::keypad::Key`. No re-exports from `evo_hw::dev`; `nor.rs` and `soc.rs` sit at `evo_hw::nor` / `evo_hw::soc`.

**Interfaces — Produces:**
```rust
pub const MPC_BLK_CFG: u32 = 7;                 // 4 KiB blocks
pub struct Mpc { /* ctrl, idx, lut: Vec<u32>, int_en, int_stat */ }
impl Mpc { pub fn new(blk_max: u32) -> Self; pub fn lut(&self) -> &[u32]; pub fn locked(&self) -> bool; }
// soc::attach: "mpc_sram0" 0x5020_4000, "mpc_sram1" 0x5020_5000, "mpc_sram2" 0x5020_6000 (Mpc::new(3)),
//              "mpc_flash" 0x5020_8000 (Mpc::new(127)); each 0x1000. IRQ 7 never asserted.
```
Registers per `security.md` §1 "Minimal MPC model": CTRL +0x00 (bit 31 sticky lock: afterwards CTRL and LUT writes are ignored), BLK_MAX +0x10, BLK_CFG +0x14, BLK_IDX +0x18 (clamped to `blk_max`), BLK_LUT +0x1C (auto-increment while CTRL bit 8, saturating at `blk_max`), INT_STAT +0x20 (0), INT_CLEAR +0x24 (W1C), INT_EN +0x28 (bit 0), INT_INFO1/2 +0x2C/+0x30 (0), INT_SET +0x34. No enforcement. System reset keeps everything; PowerOn clears to 0.

- [ ] **Step 1: Write the failing tests:** `geometry` (sram +0x10 = 3, +0x14 = 7; flash +0x10 = 127); `lut_explicit_index` (IDX=8, LUT=0xFFFF_FFFF; IDX=8, read LUT → 0xFFFF_FFFF; `lut()[8]`); `autoinc_when_ctrl_bit8` (CTRL=0x100, IDX=0, two LUT writes → `lut()[0..2]` set, IDX reads 2); `lockdown_freezes` (CTRL=0x8000_0100, then CTRL=0 and a LUT write → CTRL reads 0x8000_0100, LUT unchanged, `locked()`); `int_regs` (INT_EN=1 reads 1; INT_STAT 0; `h.irq == 0`); `system_reset_keeps_power_on_clears`; `state_roundtrip`; `unknown_offset_logged` (+0x40 read → 0 and one log entry).
- [ ] **Step 2:** `cargo test -p evo-hw --test mpc` → FAIL. **Step 3:** Implement; register in `soc::attach`. **Step 4:** PASS.
- [ ] **Step 5:** Local: `evo-cli run "$EVO_OS_IMAGE" --max-cycles 20000000` → `resets: 0` instead of the ≈ 600 of the MPC reset loop (boot now waits in the SPIF Read-ID poll, whose timeout needs the Task 7 timer).
- [ ] **Step 6: Commit** `feat(evo-hw): SIE-200 MPC model`.

### Task 6: SPCTRL, CRU, LED pattern generator, ICACHE

**Files:** Create `crates/evo-hw/src/dev/{storage.rs,sysctl.rs}`, test `crates/evo-hw/tests/sysctl.rs`; modify `soc.rs`.

**Interfaces — Produces:**
```rust
/// Plain register storage, byte/halfword/word lanes, reset 0. Used for blocks with no behavior.
pub struct Storage { /* bytes: Vec<u8> */ }
impl Storage { pub fn new(size: u32) -> Self; pub fn word(&self, off: u32) -> u32; }
pub struct Spctrl; impl Spctrl { pub fn new() -> Self; pub fn reg(&self, off: u32) -> u32; }
pub struct Cru;    impl Cru    { pub fn new() -> Self; pub fn reg(&self, off: u32) -> u32; }
pub const CRU_ID: u32 = 0x0011_0000;           // +0xFC, read-only
pub const CRU_CLKSEL_RESET: u32 = 0x0002_0001; // +0x04
pub const CRU_PLL_RESET: u32 = 0x0000_3433;    // +0x1C
pub const CRU_UID_LO: u32 = 0x4556_4F31;       // +0x0C, read-only ("EVO1")
pub const CRU_CHIPINFO: u32 = 0;               // +0x10, read-only
// soc::attach: "cru" 0x5020_0000/0x1000, "spctrl" 0x5020_1000/0x1000,
//              "ledpat" 0x5020_2B00/0x100 Storage, "icache" 0x5000_5000/0x1000 Storage
```
SPCTRL (`security.md` §2): word storage over 0x000..0xFFF, reset 0, except +0x20 read-only (0), +0x24 W1C on +0x20 and reads 0, +0xC0 read-only 0. CRU (§3 "Minimal CRU model"): word storage, reset 0 except the constants above; +0x1C reads with bit 1 = bit 0. Both keep state across System reset. IRQs 8 and none: never asserted.

- [ ] **Step 1: Write the failing tests:** `spctrl_ppc_bits_store` (+0x64 = 0x1BFF, +0x60 = 0x59, +0x80 |= 1 read back); `spctrl_status_is_read_only` (+0x20 write ignored; +0x24 write reads 0; +0xC0 write → reads 0); `cru_reset_values` (+0x04 = 0x0002_0001, +0x1C = 0x3433, +0xFC = 0x0011_0000, +0x0C = 0x4556_4F31, +0x10 = 0); `cru_id_read_only` (write 0xFFFF_FFFF to +0xFC, +0x0C and +0x10 → they still read 0x0011_0000, 0x4556_4F31, 0); `cru_pll_lock_follows_enable` (write 0x3430 → 0x3430; 0x3431 → 0x3433); `cru_system_reset_keeps_clock_gates` (+0x14 = 0x197D0 survives System, PowerOn → 0, +0x04 back to 0x0002_0001); `storage_lanes` (byte write at +0x05 of 0xAB → `word(4) == 0x0000_AB00`; halfword reads); `state_roundtrip` for each.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): SPCTRL, CRU, LED pattern and ICACHE blocks`.

### Task 7: TIMER (3 channels, computed from cycles)

**Files:** Create `crates/evo-hw/src/dev/timer.rs`, test `crates/evo-hw/tests/timer.rs`; modify `soc.rs`, `docs/hardware/timer.md`.

**Interfaces — Produces:**
```rust
pub struct Timer; impl Timer { pub fn new() -> Self; }
// soc::attach: "timer" 0x4010_1000/0x1000
```
Model (`timer.md`): channel n VALUE at +0x10n, LOAD at +0x10n+4; CTRL +0x30 (bit 3n enable, 3n+1 clock 32 kHz), INT_STAT +0x34 (W1C), INT_EN +0x38. Per channel store `value`, `load`, `since`. Reads do not change state:
```
hz(n)   = if ctrl >> (3n+1) & 1 { 32_768 } else { 39_000 }
ticks   = (now - since) as u128 * hz / CPU_HZ                     // u128
cur(n)  = if disabled { value } else if ticks <= value { value - ticks }
          else { load - ((ticks - value - 1) % (load as u64 + 1)) }   // u64: LOAD+1 overflows u32
```
`latch(n)` (`timer.md` Critic B): if disabled, `since = now`; else `value = cur(n)` and `since += ceil(ticks * CPU_HZ / hz)` (u128; never past `now`), so the part-tick remainder is carried, not dropped. Write VALUE: `latch(n)`, then `value = v; since = now`. Write LOAD: store only. Write CTRL: latch every channel, then store. Setting an INT_EN bit logs `log::warn!` once ("timer interrupts not modeled"); IRQs 1–3 are never asserted. System reset keeps state (the firmware re-inits it).

- [ ] **Step 1: Write the failing tests** (`h.now` driven by hand): `counts_down_at_32k` (CTRL bit 1|0; LOAD = VALUE = 0xFFFF_FFFF at now 0; at now 4_760 VALUE = 0xFFFF_FFFF, at 4_761 = 0xFFFF_FFFE); `hundred_ms_is_3276_ticks` (at now 15_600_000: `0xFFFF_FFFF - VALUE == 3276`); `39k_when_clock_bit_clear` (at now 4_000 → 1 tick); `frozen_while_disabled`; `underflow_reloads_from_load` (LOAD 0xFFFF_FFFF, VALUE 1, ticks 3 → 0xFFFF_FFFE); `load_reads_back`; `ctrl_write_latches_carrying_remainder` (CTRL = 0b011, VALUE = 1000 at now 0; at now 50_000 CTRL = 0b001 (10 ticks at 32 kHz consume 47_608 cycles; 2_392 carried) → VALUE 990; at now 51_607 → 990; at 51_608 → 989); `int_stat_w1c`; `no_overflow_at_u64_scale` (now = 1 << 62 does not panic); `state_roundtrip`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): 3-channel timer computed from the cycle count`.

### Task 8: UART0 and UART2 (16550) and the UART host API

**Files:** Create `crates/evo-hw/src/dev/uart.rs`, `crates/evo-core/src/host.rs`, tests `crates/evo-hw/tests/uart.rs`, `crates/evo-core/tests/machine.rs` (one test); modify `soc.rs`, `crates/evo-core/src/lib.rs`.

**Interfaces — Produces:**
```rust
pub const UART_TX_CAP: usize = 1 << 20;   // undrained TX bytes kept; older ones dropped
pub struct Uart16550 { /* ier, lcr, mcr, scr, dll, dlh, fifo_en, rx: VecDeque<u8> (16), host_rx, tx */ }
impl Uart16550 {
    pub fn new(irq: u16) -> Self;
    pub fn take_tx(&mut self) -> Vec<u8>;
    pub fn inject_rx(&mut self, bytes: &[u8], ctx: &mut DevCtx<'_>);
    pub fn divisor(&self) -> u16;
    pub fn lcr(&self) -> u8;
}
// soc::attach: "uart0" 0x4010_6000/0x1000 (IRQ 17), "uart2" 0x4010_B000/0x1000 (IRQ 27)
// evo_core::host
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum UartPort { Uart0, Uart2 }
impl Machine {
    pub fn take_uart_tx(&mut self, port: UartPort) -> Vec<u8>;
    pub fn inject_uart_rx(&mut self, port: UartPort, bytes: &[u8]);   // via with_dev (time + sync_bus)
}
```
Model: `uart.md` §8 exactly (reads/writes, LSR = 0x60 | DR, IIR, IRQ = `(ier & 1 && !rx.is_empty()) || ier & 2 != 0`, re-evaluated after every access and injection). System reset: registers to 0, `rx` cleared; `host_rx` and undrained `tx` kept.

- [ ] **Step 1: Write the failing tests** (`uart.md` §8 "Tests to write" 1–4, with these exact values): `init_sequence` (LCR |= 0x80; DLL = 13; DLH = 0; LCR = 3; FCR = 7 → `divisor() == 13`, `lcr() == 3`; THR 'B' → `take_tx() == b"B"`); `lsr_and_rx` (LSR 0x60; `inject_rx(b"x")` → 0x61; RBR = b'x'; LSR 0x60); `uart2_irq_level` (IER = 1, inject → `h.irq == 1 << 27`; IER = 0 → 0; IER = 1 with data → 1 << 27; drain → 0); `fcr_clears_rx` (FCR = 7 with 3 queued → LSR 0x60); `host_rx_refills_fifo` (inject 40 bytes → 40 reads return them in order); `tx_cap_drops_oldest` (write `UART_TX_CAP + 1` bytes `i as u8` for i in 0..=UART_TX_CAP → `take_tx().len() == UART_TX_CAP` and its first byte is 1); `uart0_never_irq_without_ier`; `system_reset_clears_registers_keeps_tx`; `state_roundtrip`. In `evo-core/tests/machine.rs`: `uart_host_api` (guest writes 'A' to 0x4010_6000 → `take_uart_tx(Uart0) == b"A"`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: 16550 UARTs and the UART host API`.

### Task 9: SPI master and ST7789 panel responder

**Files:** Create `crates/evo-hw/src/dev/spi.rs`, test `crates/evo-hw/tests/spi.rs`; modify `soc.rs`, `docs/hardware/spi-panel.md` (chosen panel ID).

**Interfaces — Produces:**
```rust
pub const SPI_FIFO_DEPTH: usize = 8;            // +0x64 reads (DEPTH - 1) << 16
pub const SPI_RX_CAP: usize = 16;
pub const DEFAULT_PANEL_ID: [u8; 3] = [0x85, 0x85, 0x52];   // "Unknown Display", default table
pub struct St7789 { /* id, cmd, params, bit queue, sleeping, display_on, madctl, colmod */ }
impl St7789 { pub fn new(id: [u8; 3]) -> Self; pub fn display_on(&self) -> bool; pub fn sleeping(&self) -> bool;
              pub fn madctl(&self) -> u8; pub fn clock(&mut self, frame: u16, nbits: u32) -> u16; }
pub struct Spi; impl Spi { pub fn new(panel: St7789) -> Self; pub fn ctrl1(&self) -> u32; pub fn panel(&self) -> &St7789; }
// soc::attach: "spi" 0x4010_7000/0x1000, Spi::new(St7789::new(DEFAULT_PANEL_ID))
```
Model: `spi-panel.md` "Minimal model specification" (STATUS = `1<<1 if tx < DEPTH | min(rx,63)<<4`, BUSY 0; frames shifted when CTRL2 bit 0 is set or on its 0→1 edge; one MISO frame per MOSI frame; RX full drops the oldest; CTRL2 write with bit 6 set and bit 0 clear clears both FIFOs; DATA read on empty → 0). Panel: commands with bit 8 clear, parameters with bit 8 set, command 0x00 clocks out a pending read (next 9 queue bits, MSB first, zero-padded). Read table (`spi-panel.md` "Minimal model"): 0x04 RDDID → dummy bit + ID1 ID2 ID3; 0x09 RDDST → dummy bit + 4 bytes 0; 0x0A..0x0F → 1 byte, no dummy (0x0A = `(!sleeping) << 4 | display_on << 2`, others 0); 0xDA/0xDB/0xDC → ID1/ID2/ID3, 1 byte, no dummy; 0xC2/0xC5/0xBB → dummy bit + the parameters last written to that command, zero-padded to 5 bytes (the queue is loaded before the command's new parameter frames replace them). Tracks SLPIN 0x10 / SLPOUT 0x11, DISPOFF 0x28 / DISPON 0x29, MADCTL 0x36, COLMOD 0x3A. SWRESET 0x01 resets the panel state (not the ID). System reset: SPI registers and FIFOs to 0, panel state kept.

- [ ] **Step 1: Write the failing tests:** `status_at_reset` (+0x0C = 0x2, +0x64 = 0x0007_0000); `rddid_frames` (CTRL1 = 0x0008_0012, CTRL2 = 0x183, DATA ← 0x004, 0, 0, 0 → STATUS = 0x42, pops 0x000, 0x085, 0x10A, 0x148); `frames_wait_for_enable` (CTRL2 = 0, two frames → STATUS rx 0; CTRL2 = 1 → rx 2); `flush_bit6` (CTRL2 = 1, three frames → rx 3; CTRL2 = 0x40 → STATUS = 0x2); `rx_caps_at_16` (CTRL2 = 1; frames 0x0DA then 19 × 0x000 → STATUS rx field 16; the first pop is the 5th frame's MISO, 0x000); `rdid_bytes_no_dummy` (0x0DA, 0x000 → pops 0x000, 0x10A); `diag_read_echoes_params` (0x0C2, 0x101, 0x0C2, 0x000, 0x000 → pops 0x000, 0x000, 0x000, 0x001, 0x000); `panel_tracks_commands` (0x011, 0x029, 0x036 + 0x128 → `!sleeping()`, `display_on()`, `madctl() == 0x28`); `empty_read_is_zero`; `nine_bit_mask` (DATA ← 0xFFFF with 9-bit frames → panel saw 0x1FF); `state_roundtrip`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): SPI master with ST7789 panel responder`.

### Task 10: M1 — TF-M banner; CLI `--uart` and `--uart-in`

**Files:** Create `crates/evo-core/tests/milestones.rs`; modify `apps/evo-cli/src/{main.rs,run.rs}`, `apps/evo-cli/tests/run.rs`, `docs/hardware/boot-flow.md` (M1 cycle count).

**Interfaces — Consumes:** Tasks 5–9. **Produces:**
```rust
// milestones.rs helpers (reused by Tasks 16, 23, 24)
struct Os { m: Machine, uart0: Vec<u8> }
impl Os {
    /// None, printing `skipping: <reason>`, when EVO_OS_IMAGE is unset or under cfg!(debug_assertions).
    fn boot() -> Option<Os>;
    /// Runs 10_000_000-cycle slices, appending UART0 TX to `uart0` after each. Returns `Halted` once
    /// `done` is true, the machine's own stop if it is not `CycleLimit` (panics with
    /// `recent_trace()` on `Lockup`), or `CycleLimit` when `budget` is used up.
    fn run_until(&mut self, budget: u64, done: impl FnMut(&Os) -> bool) -> StopReason;
    fn uart_text(&self) -> String;                     // lossy UTF-8 of uart0
}
// evo-cli run: --uart (UART0 TX to stdout, raw bytes), --uart-in FILE (bytes injected into UART0 RX at cycle 0)
pub const UART_SLICE: u64 = 1_560_000;                 // 10 ms; the run loop drains UART0 after each slice
```
`run.rs` runs in slices of at most `UART_SLICE` cycles (`run_with` per slice keeps tracing and breakpoints), draining UART0 after each; without `--uart` the bytes are discarded. UART2 TX is drained too and counted; when the count is non-zero at the end of the run, stderr gets one line `uart2: N bytes` (UART2 is the link port; M4 must see any traffic on it). Stop reasons, summary and exit codes are unchanged.

M1 passes without a SPIF model (Tasks 11–12 come later): the SPIF Read-ID poll times out after 100 ms of TIMER0 time, the probe fails and the banner still prints (`timer.md`: one secure-boot pass ≈ 97.6 M cycles, including that timeout). The boot then panics at 0x100b6c0e after "Creating an empty ITS flash layout." and resets; that is expected until Task 12. The 400 M budget covers more than one full pass.

- [ ] **Step 1: Write the failing tests:** `m1_tfm_banner` — `os.run_until(400_000_000, |o| o.uart_text().contains("Booting TF-M v2.1.1")) == StopReason::Halted`; the text also contains `[Sec Thread] Secure image initializing!`; `m.bus.device::<Spi>("spi").unwrap().ctrl1() == 0x0008_0012`; `device::<Uart16550>("uart0").divisor() == 13`. CLI (`apps/evo-cli/tests/run.rs`, synthetic bundle whose secure image writes "hi" to 0x4010_6000 then loops): `cli_uart_streams_to_stdout` (`--uart` → stdout contains `hi` before the `stop:` line); `cli_uart_in_feeds_rx` (program echoes one RX byte to TX; `--uart-in` file `Z` → stdout contains `Z`); `cli_without_uart_prints_no_guest_bytes`; `cli_reports_uart2_bytes` (program writes "ab" to 0x4010_B000 → stderr contains `uart2: 2 bytes`).
- [ ] **Step 2:** `cargo test -p evo-cli` and (with `EVO_OS_IMAGE` set) `cargo test --release -p evo-core --test milestones -- --nocapture` → FAIL.
- [ ] **Step 3:** Implement the CLI flags and the sliced loop.
- [ ] **Step 4:** Both PASS; the milestone output has no line containing `skipping`. Record in `boot-flow.md` the cycle at which the banner appears.
- [ ] **Step 5: Commit** `feat: M1 — TF-M banner on UART0; evo-cli --uart/--uart-in`.

### Task 11: NOR flash engine and the flash file

**Files:** Create `crates/evo-hw/src/nor.rs`, test `crates/evo-hw/tests/nor.rs`; modify `crates/evo-hw/src/lib.rs` (`pub mod nor;`).

**Interfaces — Produces:**
```rust
pub const NOR_SIZE: usize = 16 << 20;
pub const SECREGS_LEN: usize = 768;                    // 3 x 256 B, command addresses 0x1000/0x2000/0x3000
pub const JEDEC_ID: [u8; 3] = [0xEF, 0x40, 0x18];      // W25Q128JV
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] pub enum DataPhase { None, Read, Write }
#[derive(Clone, Serialize, Deserialize)]
pub struct NorFlash {
    pub sr1: u8, pub sr2: u8, pub sr3: u8,
    /* private: phase: Option<OpenPhase> where
       #[derive(Clone, Copy, Debug, Serialize, Deserialize)] struct OpenPhase { op: u8, addr: u32, remaining: u32, kind: DataPhase } */
    /// Non-volatile: persisted by the flash file (NvState), never by save states.
    #[serde(skip)] secregs: Vec<u8>,
}
impl NorFlash {
    pub fn new() -> Self;                              // sr1 0x00, sr2 0x02, sr3 0x60, secregs 0xFF
    pub fn begin(&mut self, op: u8, addr: u32, len: u32, flash: &mut [u8]) -> DataPhase;
    pub fn remaining(&self) -> u32;                    // bytes left in the open data phase
    pub fn read_byte(&mut self, flash: &[u8]) -> u8;   // 0 when no read phase is open
    pub fn write_byte(&mut self, b: u8, flash: &mut [u8]) -> bool; // true when it completed the phase
    pub fn abort(&mut self);
    pub fn secregs(&self) -> &[u8];
    pub fn set_secregs(&mut self, b: &[u8]);           // len must be SECREGS_LEN (else log + ignore)
}
pub struct NvState { /* flash: Vec<u8> (NOR_SIZE), secregs: Vec<u8> (SECREGS_LEN) */ }
impl NvState {
    pub fn erased() -> Self;
    pub fn new(flash: Vec<u8>, secregs: Vec<u8>) -> Result<Self, NvError>;
    pub fn flash(&self) -> &[u8];
    pub fn secregs(&self) -> &[u8];
    pub fn read_file(path: &Path) -> Result<Self, NvError>;
    pub fn write_file(&self, path: &Path) -> Result<(), NvError>;   // PATH.tmp then rename
}
#[derive(thiserror::Error, Debug)]
pub enum NvError { #[error(transparent)] Io(#[from] std::io::Error), #[error("not a flash file")] BadMagic,
                   #[error("flash file version {0}")] Version(u32),
                   #[error("flash file size: expected {expected} bytes, found {found}")] Size { expected: usize, found: usize } }
```
File format: `b"EVOFLASH"`, u32 LE version 1, u32 LE flash length (0x0100_0000), the flash bytes, the 768 security-register bytes. Command table: `spif-nor.md` §8.5–8.6 (ops 0x9F, 0x05/0x35/0x15, 0x06/0x04, 0x03/0x0B/0x3B/0x6B/0xBB/0xEB, 0x48, 0x20/0x52/0xD8 (aligned 4/32/64 KiB), 0xC7/0x60, 0x44, 0x02/0x32/0x42 (page-wrapping AND programming), 0x01/0x31/0x11 (one byte; SR1 keeps bits 0–1 as computed), other ops = no data). Erase/program/WRSR need WEL and clear it; without WEL data is accepted and discarded. Reads generate bytes on demand (never a buffer of `len`). Addresses wrap at `flash.len()`.

- [ ] **Step 1: Write the failing tests:** `jedec_and_status_defaults` (0x9F → EF 40 18 then 0x00; 0x35 → 0x02; 0x15 → 0x60); `erase_needs_wel` (0x20 without 0x06 → bytes unchanged; with → 4 KiB of 0xFF, WEL clear); `erase_sizes_align_down` (0xD8 at 0x1_2345 → 0x1_0000..0x2_0000 erased); `program_is_and_and_wraps_page` (0x02 at 0xFE, 4 bytes 0x0F over 0xF0 → 0x00 at 0xFE, 0xFF, 0x00, 0x01; completes on the 4th byte); `secreg_cycle` (0x06; 0x44 at 0x2000; 0x06; 0x42 at 0x201C bytes 42 00 DE C0; 0x48 at 0x201C len 4 → 42 00 DE C0; `secregs()[256 + 28..256 + 32]`); `secreg_program_needs_wel` (0x44 then 0x42 without 0x06 → `secregs()` all 0xFF); `wrsr_needs_wel`; `read_phase_is_lazy` (Review Focus 4: `begin(0x03, 0, u32::MAX)` returns `Read`, three reads give flash[0..3], `remaining() == u32::MAX - 3`); `nvstate_file_roundtrip`; `nvstate_rejects_bad_magic`, `nvstate_rejects_version_2`, `nvstate_rejects_truncated` (`Size`), each leaving the file bytes unchanged; `nvstate_new_checks_lengths`; `serde_skips_secregs` (secreg byte 0 = 0x12, a read phase open; postcard round trip → same `sr1..sr3` and `remaining()`, `secregs()` empty).
- [ ] **Step 2:** `cargo test -p evo-hw --test nor` → FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): W25Q128-style NOR engine and flash file format`.

### Task 12: SPIF (QSPI controller)

**Files:** Create `crates/evo-hw/src/dev/spif.rs`, test `crates/evo-hw/tests/spif.rs`; modify `soc.rs`.

**Interfaces — Consumes:** Task 11 `NorFlash`; Task 3 `DevCtx::buffer_mut(FLASH_BUFFER)`. **Produces:**
```rust
pub const SPIF_FIFO: u32 = 64;
pub struct Spif; impl Spif { pub fn new() -> Self; pub fn nor(&self) -> &NorFlash; pub fn nor_mut(&mut self) -> &mut NorFlash; }
// soc::attach: "spif" 0x5000_1000/0x1000
```
Registers per `spif-nor.md` §3 and §8.3–8.4: +0x00 ADDR, +0x04 CFG, +0x08 LEN, +0x0C OP (stored); +0x10, +0x30 storage; +0x18 STATUS = `1 | (((rx_available > 0) as u32) << 1)` (bit 1 = RX not empty, a flag, never the count); +0x20 CTRL stored, and **every** write with bit 1 set starts the descriptor (`op = OP >> 24`, `addr = ADDR & 0xFF_FFFF` when `CFG & 7 == 3`, `len = LEN`; aborts any open phase); a write with bit 1 clear neither starts nor clears RX; +0x24 bit 0 DONE (W1C); +0x34 stored, bit 10 reads 0; +0x54 = `min(nor.remaining(), 64) << 8` while a phase is open, else 0; +0x100 FIFO port: byte/halfword/word pops or pushes 1/2/4 bytes LE. DONE is set at start for no-data and read commands and when the last write byte arrives. Flash bytes come from `ctx.buffer_mut(FLASH_BUFFER)`; a missing buffer behaves as unmapped (reads 0xFF, programs dropped, `log::warn!`). System reset (`spif-nor.md`, Critic B.6): abort the phase, clear RX, DONE = 0, CTRL bit 1 = 0, clear WEL (sr1 bit 1); flash, secregs and the other SR bits kept. `save` stores the SPIF registers, SR1–3 and the open phase only (the `NorFlash` serde skips `secregs`); `load` keeps the current secregs.

- [ ] **Step 1: Write the failing tests** (harness buffer "flash" of `NOR_SIZE` zero bytes; helper `cmd(op_word, cfg, addr, len)` = W1C +0x24, write +0x00/+0x04/+0x08/+0x0C, then +0x20 = 0x1102): `jedec_id` (0x9F00_0000, cfg 0x0100_0000, len 3 → +0x24 = 1, +0x18 bit 1, +0x54 = 0x300, byte pops EF 40 18); `rx_survives_done_clear` (0x0500_0000 len 1, W1C +0x24, +0x20 = 0x1100 → +0x18 bit 1 still set, pop 0x00); `erase_and_program_hit_flash_buffer` (WREN 0x0600_0002; 0x2000_0006 cfg 0x0100_0003 at 0x9_0000 → buffer 0x9_0000..0x9_1000 = 0xFF; WREN; 0x0200_0002 at 0x910F8 len 8 → +0x54 = 0x800; word writes 0x4433_2211, 0x8877_6655 → DONE, buffer[0x910F8..0x91100] = 11 22 … 88; RDSR → 0x00); `start_is_level_triggered` (+0x20 = 0x1102 twice without clearing; the second command runs); `fifo_level_multiple_of_four` (0x0300_0000 cfg 0x0100_0003 len 100 → +0x54 = 0x4000 and +0x18 = 0x3 (even count: bit 1 is a flag); after 64 pops → +0x54 = 0x2400, +0x18 = 0x3; after the last 36 pops → +0x18 = 0x1); `huge_read_len_is_lazy` (Review Focus 4: len 0xFFFF_FFFF → +0x54 = 0x4000, a word pop returns flash[0..4]); `secreg_program_via_fifo` (WREN 0x0600_0002; 0x4200_0002 at 0x101C, 4 bytes → `nor().secregs()[28..32]`); `xip_ctrl_bit10_self_clears` (+0x34 = 0x400 → 0); `system_reset_keeps_flash_and_secregs` (after a program and a secreg program, WREN, then `reset(System)` → flash and secregs unchanged, RDSR 0x0500_0000 reads 0x00); `state_roundtrip` (mid-read phase survives; secregs changed after `save` are still the changed values after `load`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): SPIF QSPI controller over the NOR engine`.

### Task 13: Flash persistence — `Machine` NV state and `--flash-file`

**Files:** Modify `crates/evo-core/src/{machine.rs,host.rs,lib.rs}`, `apps/evo-cli/src/{main.rs,run.rs}`; tests `crates/evo-core/tests/machine.rs`, `apps/evo-cli/tests/run.rs`.

**Interfaces — Consumes:** Task 11 `NvState`, Task 12 `Spif`. **Produces:**
```rust
// evo_core (lib.rs) — the CLI depends on evo-core only, so it names these through it
pub use evo_hw::nor::{NvError, NvState};
pub use evo_hw::StateError;
impl Machine {
    /// Start from `nv` (flash + security registers), then place the bundle's images over it.
    pub fn from_bundle_nv(b: &Bundle, layout: &Layout, nv: Option<NvState>) -> Result<Self, ImageError>;
    pub fn nv_state(&self) -> NvState;      // current flash bytes + SPIF security registers
}
// evo-cli run: --flash-file PATH. Exists → NvState::read_file (error → exit 1, file untouched);
// missing → erased. Written back (NvState::write_file) whenever the run ends: normal stop, lockup,
// end of a --gdb session, a trace that stops early, an I/O error on stdout. A write-back failure
// prints `evo-cli: PATH: <err>` to stderr and exits 1.
```
`from_bundle` = `from_bundle_nv(b, layout, None)`. `from_bundle_nv` restores the security registers through `with_dev::<Spif, _>("spif", ..)` (`nor_mut().set_secregs`).

- [ ] **Step 1: Write the failing tests:** `nv_roundtrip_keeps_data_outside_slots` (NvState with 0xA5 at 0x9_0000 and secreg byte 0 = 0x12 → machine `peek(0x9_0000, 1) == Some(0xA5)`; `nv_state()` returns both); `bundle_images_win_over_nv` (NvState with zeros over the secure slot → `peek(0x000B_0000, 4)` is the MCUboot magic 0x96F3_B83D); CLI: `cli_flash_file_created_and_reused` (run 1 creates a file of 8 + 4 + 4 + 16 MiB + 768 bytes; run 2 accepts it); `cli_bad_flash_file_exits_1_and_keeps_file` (Review Focus 2: a 100-byte file → exit 1, stderr names the file, file bytes unchanged); `cli_flash_file_written_after_lockup` (a program that locks up → the file exists afterwards).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: persist flash and NOR security registers with --flash-file`.

### Task 14: TRNG and SHA-256 engine

**Files:** Create `crates/evo-hw/src/dev/crypto.rs`, test `crates/evo-hw/tests/crypto.rs`; modify `soc.rs`, `crates/evo-hw/Cargo.toml` (`sha2 = "0.10"`).

**Interfaces — Produces:**
```rust
pub const TRNG_SEED: u64 = 0x9E37_79B9_7F4A_7C15;
pub struct Trng; impl Trng { pub fn new(seed: u64) -> Self; }          // xorshift64* stream
pub struct Sha256Engine; impl Sha256Engine { pub fn new() -> Self; pub fn state(&self) -> [u32; 8]; }
// soc::attach: "trng" 0x5010_A000/0x1000 (Trng::new(TRNG_SEED)), "sha" 0x5000_2000/0x1000
```
TRNG (`crypto.md` TRNG): storage for +0x100, +0x10C, +0x12C, +0x130, +0x138; +0x104 ISR = `RND_SOURCE_ENABLE & 1` (bits 1–3 always 0); +0x108 W1C, never sets; +0x114..+0x128 EHR words from the generator (one draw per read). SHA (`crypto.md` SHA): +0x10..+0x4C W[0..15] (big-endian message words), +0x60..+0x7C H[0..7]; a CTRL (+0x00) write with bit 1 set: if bit 2, H = SHA-256 IV; then `sha2::compress256(&mut H, &[block])` with block = W as big-endian bytes, synchronously; CTRL reads the stored value with bit 11 clear. IRQs 23/26 never asserted. System reset: both to power-on state except the TRNG generator, which keeps running.

- [ ] **Step 1: Write the failing tests:** `trng_valid_follows_enable` (+0x104 = 0; +0x12C = 1 → bit 0); `trng_storage_reads_back` (+0x130 = 0xC8, +0x138 = 0xA); `trng_not_constant_but_deterministic` (24 EHR reads not all equal; two `Trng::new(TRNG_SEED)` give the same 24 words); `trng_icr_never_sets_errors`; `sha_abc` (W0 = 0x6162_6380, W1..W14 = 0, W15 = 0x18, CTRL = 0x0E → H = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad); `sha_two_blocks` ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq" padded; first CTRL 0x0E, second 0x0A → 248d6a61 d20638b8 e5c02693 0c3e6039 a33ce459 64ff2167 f6ecedd4 19db06c1); `sha_ctrl_not_busy` (+0x00 reads 0x0E); `state_roundtrip` for both.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): TRNG and SHA-256 engine`.

### Task 15: USB minimum and USB Type-C stub

**Files:** Create `crates/evo-hw/src/dev/usb.rs`, test `crates/evo-hw/tests/usb.rs`; modify `soc.rs`, `crates/evo-core/src/host.rs`.

**Interfaces — Produces:**
```rust
pub const USB_ID: u32 = 0x0001_0000;      // +0xE0
pub const USB_CONFIG: u32 = 0x0000_0880;  // +0xE4
pub struct Usb; impl Usb { pub fn new() -> Self; }
pub struct UsbTypeC; impl UsbTypeC {
    pub fn new() -> Self;
    pub fn set_cc(&mut self, cc1: u8, cc2: u8);   // each masked to 2 bits; read at +0x1C bits 9:8 / 11:10
}
impl Machine { pub fn set_usb_cc(&mut self, cc1: u8, cc2: u8); }   // via with_dev
// soc::attach: "usb" 0x4000_3000/0x1000; "usbtc" 0x4010_C000/0x1000 (UsbTypeC)
```
`usb.md`: storage over the window except +0xE0/+0xE4 read-only constants and +0x000 bit 1 (soft reset) reading 0. Type-C: byte/halfword `Storage` for every offset, except that +0x1C bits 11:8 always read the host CC state (`cc1 << 8 | cc2 << 10`, power-on 0 = unattached) and guest writes to those bits are dropped. IRQs 16 and 28 never asserted. System reset: power-on state, host CC state kept.

- [ ] **Step 1: Write the failing tests:** `id_and_config`; `soft_reset_self_clears` (+0x000 = 0x6 → 0x4); `storage_elsewhere` (+0x100 = 0x220 reads back); `typec_byte_storage` (byte +0x1A4 = 0x95 → word +0x1A4 = 0x95; +0x1C = 0); `typec_cc_is_host_controlled` (+0x1C = 0xFFFF_FFFF → reads 0xFFFF_F0FF; `set_cc(1, 2)` → 0xFFFF_F9FF); `state_roundtrip`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: USB ID/soft-reset minimum and Type-C stub with host CC state`.

### Task 16: M2 — secure boot completes and hands off to the NS BSP

**Files:** Modify `crates/evo-core/tests/milestones.rs`, `docs/hardware/boot-flow.md` (and `discovery-log.md` only under the stop rule).

**Interfaces — Consumes:** Tasks 10–15 (Task 13 `from_bundle_nv`, `nv_state`). **Produces** (milestones.rs, reused by Tasks 23–24):
```rust
impl Os { fn boot_nv(nv: NvState) -> Option<Os>; }   // as Os::boot, built with from_bundle_nv(.., Some(nv)); first used here
/// Peripheral/PPB unmapped accesses that are expected: 0xE004_3018 (RAZ/WI by design) and
/// internal-PPB misses (DWT/ITM, 0xE000_0000..=0xE003_FFFF). Later tasks append (range, reason) pairs
/// only together with a boot-flow.md entry explaining them.
const KNOWN_UNMODELLED: &[(RangeInclusive<u32>, &str)];
const FATAL_STUB_IRQS: u64 = 1 << 7 | 1 << 8 | 1 << 10 | 1 << 17 | 1 << 22 | 1 << 23 | 1 << 26;
/// Log entries in 0x4000_0000..=0x5FFF_FFFF or 0xE000_0000..=0xE00F_FFFF not covered by
/// KNOWN_UNMODELLED, plus any other non-memory entry outside 0x1800_E000..=0x1800_E0FF (key source, reads 0).
fn unexpected_unmapped(m: &Machine) -> Vec<UnmappedAccess>;
```
Note: the review suggested 0x1800_E000..0x1800_E01F; `critique.md` §7 shows reads at 0x1800_E064 and up, so the whole 0x1800_E000..=0x1800_E0FF window is allowed.

- [ ] **Step 1: Write the failing tests:**
  - `m2_ns_handoff`: `os.m.breakpoints.insert(0x0011_6D50)`; `os.run_until(1_500_000_000, |_| false) == Breakpoint(0x0011_6D50)`; `os.m.cpu.scs.scb.vtor.ns == 0x0010_0400`; `os.m.cpu.regs.msp.ns == 0x2004_6D20`; `os.m.resets == 0`; `uart_text()` contains `Creating an empty ITS flash layout.`, `Creating an empty PS flash layout.`, `Init Mbed TLS 3.6.2... complete` and neither `FATAL` nor `Restored Saved Data`; some 32-byte slot of `nv_state().secregs()` has bytes 28..32 = `42 00 DE C0` (NV counters provisioned); `device::<Sha256Engine>("sha").unwrap().state() != [0; 8]`; flash 0xCF_0000..0xCF_2400 is all 0xFF (the conditional mailbox erase did not run); `bus.irq_lines() & FATAL_STUB_IRQS == 0`; `unexpected_unmapped(&os.m)` is empty. Then `os.run_until(300_000_000, |o| o.uart_text().contains("Starting BSP")) == Halted`, still `resets == 0` and `irq_lines() & FATAL_STUB_IRQS == 0`.
  - `m2_its_survives_power_cycle`: after that run, `nv = os.m.nv_state()`; `Os::boot_nv(nv)` reaches the same breakpoint and its `uart_text()` does **not** contain `Creating an empty ITS flash layout.`
  - `m2_no_access_past_1_5_mib_sram`: no unmapped-log entry in 0x2018_0000..0x2020_0000 or 0x3018_0000..0x3020_0000 (keeping 2 MiB would have hidden such accesses).
- [ ] **Step 2:** With `EVO_OS_IMAGE` set, run `cargo test --release -p evo-core --test milestones m2 -- --nocapture` → FAIL only if a device is wrong. Diagnose with `evo-cli run --unmapped-out` and `--break`; a fix goes to the owning device with a regression test in its test file, committed on its own as `fix(evo-hw): …`. An unmapped peripheral access is fixed by modelling the register, not by extending `KNOWN_UNMODELLED`. **Stop rule:** after 5 fix commits or 10 diagnostic runs without all M2 tests passing, write the evidence (failing assertion, last UART line, stop PC, last unmapped accesses) into `discovery-log.md` §"M2 blockers", commit it as `docs: M2 investigation`, and report instead of continuing.
- [ ] **Step 3:** PASS. Record in `boot-flow.md`: hand-off cycle, BSP-banner cycle, the first unmapped NS accesses.
- [ ] **Step 4: Commit** `test: M2 — secure boot completes and hands off to the BSP`.

### Task 17: GPIO

**Files:** Create `crates/evo-hw/src/dev/gpio.rs`, test `crates/evo-hw/tests/gpio.rs`; modify `soc.rs`, `crates/evo-core/src/host.rs`, `crates/evo-core/tests/machine.rs`, `apps/evo-cli/src/run.rs`, `docs/hardware/gpio.md`.

**Interfaces — Produces:**
```rust
pub const GPIO_PINS: u8 = 13;
pub const GPIO_IRQ: u16 = 24;
pub struct Gpio; impl Gpio {
    pub fn new() -> Self;
    pub fn set_input(&mut self, pin: u8, level: Option<bool>, ctx: &mut DevCtx<'_>); // None = undriven
    pub fn pin_level(&self, pin: u8) -> bool;
    pub fn power_hold(&self) -> bool;        // pin 1 is an output driven high
    pub fn take_power_off(&mut self) -> bool; // true once after pin 1 (output) went high→low; drains
}
impl Machine {
    pub fn set_gpio_input(&mut self, pin: u8, level: Option<bool>);   // via with_dev (time + sync_bus)
    pub fn take_power_off(&mut self) -> bool;                         // forwards to Gpio
}
// soc::attach: "gpio" 0x4010_5000/0x1000
// evo-cli run: after the run, if take_power_off() → stderr line `power: off (GPIO pin 1 released)`
```
Pin 1 is the power hold (BSP Shipmode 0x101178c8 drives it low). Pin 2 low→high (secure panel_on 0x100b3244..0x100b3260, BSP 0x0015f444) would reset the panel; that hook is deferred to Plan 3, because the panel model only tracks sleep/display/MADCTL and the image comes from the LCDC — record this in `gpio.md`.
Model: `gpio.md` "Minimal model" (DATA_OUT +0x00, DATA_IN +0x04, DIR +0x08 (1 = output), SET +0x10, CLR +0x14, PULL_EN +0x18, PULL_SEL +0x1C, INT_EN +0x20, INT_STATUS +0x28 (reads `ists & ien`), INT_CLR +0x30 (W1C), INT_TYPE +0x34 (1 = level), INT_BOTH +0x38, INT_POL +0x3C). After every write and every `set_input`: level mode sets `ists` while level == polarity; edge mode sets on rising (pol or both) or falling (!pol or both). IRQ 24 = `ists & ien != 0`. Pin 1 high→low logs `log::info!("power-hold released")`. Pins ≥ 13 ignored. System reset: registers to 0, host inputs kept.

- [ ] **Step 1: Write the failing tests:** `set_clr_and_readback` (DIR = 2; SET = 2 → +0x00 = 2, +0x04 bit 1; CLR = 2 → 0; `power_hold()` follows); `pull_up_input` (PULL_EN = PULL_SEL = 1 << 3 → +0x04 bit 3); `host_input_overrides_pull`; `both_edges_irq` (pin 9: BOTH bit, INT_EN bit; `set_input(9, Some(true))` → +0x28 = 1 << 9, `h.irq == 1 << 24`; INT_CLR = 1 << 9 → 0; falling edge → set again); `level_irq_holds` (TYPE = POL = 1 << 11, input high → W1C does not clear while high); `falling_only` (TYPE 0, POL 0, BOTH 0: rising → nothing, falling → set); `power_off_once` (DIR = 2, SET = 2, CLR = 2 → `take_power_off()` true, then false); `out_of_range_pin_ignored`; `state_roundtrip`. In `evo-core/tests/machine.rs`: `gpio_input_irq_wakes_sleeping_cpu` (guest enables GPIO pin 9 both-edge IRQ and IRQ 24, then `wfi`; `run` → Idle; `set_gpio_input(9, Some(true))`; `run(1_000)` runs the handler).
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register; host API. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: GPIO controller (13 pins, IRQ 24), power-off report`.

### Task 18: PL111 LCDC, backlight PWM and `Machine::screen`

**Files:** Create `crates/evo-hw/src/dev/lcdc.rs`, test `crates/evo-hw/tests/lcdc.rs`; modify `soc.rs`, `crates/evo-core/src/host.rs`, `docs/hardware/lcdc.md`.

**Interfaces — Produces:**
```rust
pub const FRAME_CYCLES: u64 = 2_600_000;          // 60 Hz at CPU_HZ
pub const LCDC_IRQ: u16 = 14;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame { pub width: u32, pub height: u32, pub rgb: Vec<u8> }   // RGB888, row-major
pub struct Lcdc; impl Lcdc { pub fn new() -> Self; pub fn frame(&self, sram: &[u8], sram_base: u32) -> Option<Frame>; }
pub struct Pwm;  impl Pwm  { pub fn new() -> Self; pub fn brightness(&self) -> f32; } // +0 / +4, clamped 0..=1, 0 if +4 == 0
impl Machine { pub fn screen(&self) -> Option<Frame>; }  // lcdc.frame(sram bytes, SRAM_NS)
// soc::attach: "lcdc" 0x4000_4000/0x1000, "pwm" 0x4010_4000/0x1000
```
Registers: `lcdc.md` PL111 table (TIM0–3 +0x00..+0x0C, UPBASE +0x10, LPBASE +0x14, CONTROL +0x18, IMSC +0x1C (stores `v & 0x1E`), RIS +0x20, MIS +0x24, ICR +0x28 (W1C on RIS, reads 0), +0x2C/+0x30 current UPBASE/LPBASE). While CONTROL bit 0: an event every `FRAME_CYCLES` (next = previous deadline + FRAME_CYCLES) sets RIS bit 3 (VComp); clearing bit 0 cancels it. IRQ 14 = `RIS & IMSC != 0`.

`frame`: `None` unless CONTROL bits 0 and 11 are set, bpp field `(CONTROL >> 1) & 7 == 6`, and `base .. base + ppl*lpp*2` lies inside, where `base = UPBASE & !0x1000_0000` (the Secure alias 0x3xxx_xxxx maps onto the same SRAM as 0x2xxx_xxxx; `sram_base` is `SRAM_NS`) `[sram_base, sram_base + sram.len())`. `ppl = (((TIM0 >> 2) & 0x3F) + 1) * 16`, `lpp = (TIM1 & 0x3FF) + 1`. The panel shows the portrait buffer landscape: `width = lpp`, `height = ppl`, output pixel (x, y) = buffer pixel (line x, pixel y). Pixel `p` (u16 LE): `r5 = p >> 11, g6 = (p >> 5) & 63, b5 = p & 31`; CONTROL bit 8 (BGR) swaps `r5` and `b5`; expand `(v5 << 3) | (v5 >> 2)`, `(v6 << 2) | (v6 >> 4)`. Task 23 confirms or corrects the orientation and colour order against the real logo. System reset: LCDC and PWM to power-on state (event cancelled).

- [ ] **Step 1: Write the failing tests:** `registers_store` (CONTROL 0x312D; IMSC 0xFF → 0x1E); `vcomp_each_frame` (CONTROL = 0x312D at now 0 → `h.event == Some(Some(2_600_000))`; `fire` → RIS bit 3; IMSC = 8 → `h.irq == 1 << 14`; ICR = 8 → 0; next event at 5_200_000); `disable_cancels` (CONTROL = 0 → `h.event == Some(None)`); `frame_decode` (SRAM vec 0x18_0000 at 0x2000_0000; TIM0 0x0A0A_0A38, TIM1 0x042D_093F, UPBASE 0x2004_8000, CONTROL 0x312D | 0x800; buffer pixel (line 0, px 0) = 0xF800, (line 319, px 239) = 0x001F → 320×240; out(0,0) = (0,0,255), out(319,239) = (255,0,0); the same with UPBASE 0x3004_8000 → the same frame); `frame_none_when_off`; `frame_outside_sram_is_none` (Review Focus 5: UPBASE 0xFFFF_FF00, UPBASE 0x2017_FF00 (runs past the end), bpp 5 → `None`); `pwm_brightness` (0xA0/0xFF; +4 = 0 → 0.0; 0x200/0xFF → 1.0); `state_roundtrip`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register; `Machine::screen`. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-hw): PL111 LCDC with VComp IRQ, backlight PWM, Machine::screen`.

### Task 19: Keypad, ON key and key scripts

> **Controller amendment (binding, supersedes the provisional `r<R>c<C>` naming where they conflict):** the real key map is now known — `$EVO_RE_DIR/keypad-map.md` (two independent RE passes, reconciled). The matrix is the TI-84 Plus CE layout: row register `+0x10+2g`, bit `b` → scan code `(7−g)·8 + b + 1` (CE GetCSC numbering); the IRQ 13 handler needs key-down status first, then ≥6 stable scan-done interrupts, and reports ≤3 keys. Name keys by their TI-84 names (`enter`, `alpha`, `2nd`, `ln`, `sin`, digits, arrows, …; keep `r<R>c<C>` accepted as an alias in key scripts) and import the table into `docs/hardware/keypad.md`. Add a test that holding `alpha`+`ln`+`enter` across a reset makes the BSP's boot combo matcher pick Self-Test (combo table at NS `0x0018_3DF0`; result byte `0x2001_0DB8`), env-gated on `EVO_OS_IMAGE`, and document the Engineering combo (`alpha`+`sin`+`enter`).

**Files:** Create `crates/evo-hw/src/dev/keypad.rs`, `crates/evo-core/src/keys.rs`, tests `crates/evo-hw/tests/keypad.rs`, `crates/evo-core/tests/machine.rs` (keys); modify `soc.rs`, `crates/evo-core/{src/host.rs,src/lib.rs,Cargo.toml}` (`thiserror`), `docs/hardware/keypad.md`.

**Interfaces — Produces:**
```rust
// evo_hw::dev::keypad
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Key { Matrix { row: u8, col: u8 }, On }      // row, col in 0..8
pub const SCAN_CYCLES: u64 = 156_000;                 // 1 ms
pub const KEYPAD_IRQ: u16 = 13;
pub const ON_IRQ: u16 = 0;
pub struct Keypad; impl Keypad { pub fn new() -> Self; pub fn set_key(&mut self, key: Key, pressed: bool, ctx: &mut DevCtx<'_>); }
// soc::attach: "keypad" 0x4010_3000/0x1000
// evo_core::keys
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyAction { pub at_ms: u64, pub key: Key, pub pressed: bool }
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum KeyScriptError { #[error("key script token {index} `{token}` is not valid")] BadToken { index: usize, token: String } }
pub fn parse_key_script(s: &str) -> Result<Vec<KeyAction>, KeyScriptError>;
pub const TAP_HOLD_MS: u64 = 50;  pub const TAP_GAP_MS: u64 = 50;
pub use evo_hw::dev::keypad::Key;          // re-exported from evo_core
impl Machine { pub fn set_key(&mut self, key: Key, pressed: bool); }   // via with_dev (time + sync_bus)
```
Keypad (`keypad.md`, first cut): +0x00 and +0x04 storage, +0x08 status W1C, +0x0C mask storage, +0x10 + 2·r row r (halfword; word reads return rows r and r+1). A press sets the row bit and status bit 1 and schedules a scan at `now + SCAN_CYCLES`; any release also schedules a scan; the scan event sets status bit 0. IRQ 13 = `status & mask != 0`. ON key: `pulse_irq(0)` on press only (its handler reads no register). Out-of-range positions log a warning and change nothing. System reset: registers to 0, pressed keys kept.

Script grammar: tokens separated by whitespace or `;`; `KEY` = `on` | `r<R>c<C>` (R, C single digits 0–7). `KEY` taps (press at the cursor, release after `TAP_HOLD_MS`, cursor += hold + gap); `+KEY` presses and `-KEY` releases at the cursor; `wait=MS` advances the cursor. Times are milliseconds from the start of the run.

- [ ] **Step 1: Write the failing tests:** keypad: `press_sets_row_status_irq` (mask 2; press (2,5) → halfword +0x14 = 1 << 5, status bit 1, `h.irq == 1 << 13`, event at now + 156_000); `scan_sets_data_ready` (mask |= 1; `fire` → status bit 0); `status_w1c` (+0x08 = 3 → 0, irq 0); `release_clears_row`; `on_key_pulses_irq0` (`h.pulses == [0]`, nothing on release); `out_of_range_ignored`; `state_roundtrip`. keys: `parse_tap` ("r2c5" → [(0,(2,5),true), (50,(2,5),false)]); `parse_press_wait_release` ("+on wait=200;-on" → [(0,On,true), (200,On,false)]); `parse_sequence_cursor` ("on r0c0" → second press at 100); `parse_errors` ("r8c0", "wait=x", "foo", "r1c1 bogus" → `BadToken { index: 0, .. }` ×3 and `index: 1`). machine: `set_key_reaches_keypad` (`m.set_key(Key::On, true)` with IRQ 0 enabled → handler runs); `set_key_after_sleep_uses_current_time` (the `wfi_wakes_on_systick` guest with SysTick reload 999_999; `run(500_000)` → `CycleLimit`, asleep, `sched.now == 500_000` with no device event fired; `set_key(Key::Matrix { row: 0, col: 0 }, true)` → `sched.next_deadline() == Some(500_000 + SCAN_CYCLES)`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: keypad scanner, ON key and key scripts`.

### Task 20: RTC

**Files:** Create `crates/evo-hw/src/dev/rtc.rs`, test `crates/evo-hw/tests/rtc.rs`; modify `soc.rs`, `docs/hardware/rtc.md`.

**Interfaces — Produces:**
```rust
pub const RTC_IRQ: u16 = 15;
pub const RTC_BUSY: u32 = 0;              // +0x40 value; Step 5 confirms the polarity
pub struct Rtc; impl Rtc {
    pub fn new() -> Self;
    pub fn seconds_of_day(&self, now: u64) -> u32;
    pub fn raise_status(&mut self, bits: u32, ctx: &mut DevCtx<'_>);   // host/test: status |= bits & 0x3F
}
// soc::attach: "rtc" 0x4010_2000/0x1000
```
First cut from `rtc.md`: +0x10/+0x14/+0x18 alarm fields and +0x20 control: storage; +0x34 status: W1C, bits 0–5, nothing sets them yet; +0x40 reads `RTC_BUSY`; +0x44 time = `sec | min << 6 | hour << 12` of `(base + (now - set_at) / CPU_HZ) % 86_400`, starting at 0 at power-on; a write to +0x44 decodes the same fields into `base` and sets `set_at = now`. IRQ 15 = `status & 0x3F != 0`. System reset keeps everything (always-on domain).

- [ ] **Step 1: Write the failing tests:** `time_counts_from_midnight` (now = 3_725 · CPU_HZ → +0x44 = 0x1085); `write_sets_time` (write 0x1085 at now N; at N + CPU_HZ → seconds_of_day 3_726); `wraps_at_midnight` (write 0x1_7EFB (23:59:59) at now N; at N + CPU_HZ → +0x44 == 0); `busy_reads_zero`; `status_w1c_and_irq` (`raise_status(1)` → +0x34 = 1, `h.irq == 1 << 15`; +0x34 = 1 → 0, IRQ drops); `system_reset_keeps_time`; `state_roundtrip`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement; register. **Step 4:** PASS.
- [ ] **Step 5: Confirm the busy polarity.** `evo-cli run "$EVO_OS_IMAGE" --max-cycles 1500000000 --trace-range 0x1542a0..0x1542c0` and the same for `0x1543d8..0x1543f8`: each wait loop must exit within 10 iterations. If one runs until its 1100 ms timeout, set that bit in `RTC_BUSY`, add a test pinning the value, and record the decision with the evidence address in `rtc.md`.
- [ ] **Step 6: Commit** `feat(evo-hw): RTC first cut (time of day, IRQ 15)`.

### Task 21: CLI `--screenshot`/`--after` and `--keys`

**Files:** Modify `apps/evo-cli/{Cargo.toml (png = "0.17"),src/main.rs,src/run.rs}`, test `apps/evo-cli/tests/run.rs`.

**Interfaces — Consumes:** `Machine::screen`, `Machine::set_key`, `parse_key_script`, `CPU_HZ`. **Produces:**
```
--screenshot FILE.png   write Machine::screen() as an RGB8 PNG
--after N               cycle count (from the start of the run) at which to take it; default: when the run stops
--keys SCRIPT           key script (evo_core::keys grammar); actions fire at start + at_ms * CPU_HZ / 1000
```
The slice loop of Task 10 also ends a slice at the next key action and at `--after`. A screen of `None` prints `screenshot: display is off` to stderr, writes nothing and keeps the exit status. A bad key script is a usage error: message naming the token, exit 1, before the bundle is opened. `--after` without `--screenshot` is a usage error.

- [ ] **Step 1: Write the failing tests** (synthetic bundle A: the secure image programs the LCDC at 0x4000_4000 with the Task 18 timing words, fills a 240×320 buffer at 0x2004_8000 with 0x07E0 after a 2_000-cycle delay loop, sets CONTROL 0x392D, then loops; synthetic bundle B: enables IRQ 0 and writes 'K' to UART0 in its handler): `cli_screenshot_png` (PNG is 320×240, every pixel (0,255,0)); `cli_screenshot_after` (`--after 1000`, before the program draws → `display is off`, no file); `cli_keys_on_reaches_guest` (`--keys on --uart` → stdout contains `K`); `cli_bad_key_script_exits_1` (`--keys r9c9` → exit 1, stderr contains "r9c9"); `cli_after_requires_screenshot`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(evo-cli): --screenshot/--after and --keys`.

### Task 22: Save states

**Files:** Create `crates/evo-core/src/state.rs`, test `crates/evo-core/tests/state.rs`; modify `crates/evo-core/{Cargo.toml (postcard, sha2 = "0.10"),src/lib.rs,src/machine.rs (bundle_id)}`, `crates/evo-core/src/scheduler.rs` (derive `Serialize, Deserialize` on `Scheduler`; `EventKind` already has them), `crates/armv8m/src/cpu.rs` (`restore_from`) with its test in `crates/armv8m/tests/trustzone.rs`, `apps/evo-cli/src/{main.rs,run.rs}`, `apps/evo-cli/tests/run.rs`.

**Interfaces — Consumes:** `MemoryMap::device_states/load_device_states`, every device's `save`/`load`. **Produces:**
```rust
pub const STATE_MAGIC: &[u8; 8] = b"EVOSTATE";
pub const STATE_VERSION: u32 = 1;
impl Machine {
    pub fn save_state(&self) -> Vec<u8>;
    /// All or nothing: on error the machine is unchanged.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), evo_hw::StateError>;
}
// Machine gains `#[serde(default)] bundle_id: [u8; 32]`, set in from_bundle_nv to
// SHA-256(b.package.bytes ‖ secure_slot_base LE ‖ ns_slot_base LE).
// file = STATE_MAGIC | u32 LE STATE_VERSION | bundle_id [u8; 32] | postcard(MachineState)
// MachineState { cpu: Cpu, sched: Scheduler, sram: Vec<u8>, log: UnmappedLog, pc_hint: u32, resets: u64,
//                boot_vtor: u32, sleep: Option<SleepKind>, applied_irqs: u64, devices: Vec<DeviceState> }
// armv8m: impl Cpu { pub fn restore_from(&mut self, saved: Cpu); }  // replaces all state, keeps the installed IDAU
// evo-cli run: --load-state FILE (after building the machine and applying --flash-file), --save-state FILE (at the end)
```
Flash and security registers are not in the state (spec §6.5: they persist separately; `Spif::save` skips secregs, Task 12). `load_state` checks magic, version, then `bundle_id` (`StateError::Mismatch("save state is from another bundle")`), decodes fully, checks SRAM length and device names, keeps `save_state()` of the current machine as a backup, applies, and restores the backup if a device rejects its bytes. Applying: `cpu.restore_from(state.cpu)` (keeps the IDAU), `sched`, SRAM bytes, log, `pc_hint`, `resets`, `boot_vtor`, `sleep`, `applied_irqs`, devices; then `bus.now = sched.now`, `stopped_at = None`, `recent` cleared. Breakpoints are kept from the current machine; the recent trace is not saved.

- [ ] **Step 1: Write the failing tests:** `state_roundtrip_resumes_identically` (synthetic bundle with a SysTick-driven counter and a UART writer; run 50_000 cycles, save, run 50_000 more and record registers, `now`, SRAM and UART bytes; a fresh machine loads the state and runs 50_000 → identical); `state_keeps_device_events` (LCDC frame event pending at save → fires after load at the same cycle); `load_state_rejects_bad_magic`, `load_state_rejects_version_2`, `load_state_rejects_truncated`, `load_state_rejects_other_device_set` (machine with an extra test device), `load_state_rejects_other_bundle` (state from a synthetic bundle whose image body differs by one byte → `Mismatch`) — each returns `Err` and `save_state()` before == after (Review Focus 3); `load_state_keeps_secregs` (save; `m.bus.with_device::<Spif, _>("spif", |s, _| s.nor_mut().set_secregs(&[0x12; SECREGS_LEN]))`; load → `nv_state().secregs()[0] == 0x12`); in `crates/armv8m/tests/trustzone.rs`: `restore_from_keeps_idau` (a CPU with an IDAU that makes 0x6000_0000 Non-secure; `cpu.restore_from(Cpu::new())` → `security_of(0x6000_0000)` unchanged); CLI `cli_save_then_load_state` (run 1 `--save-state s.state --max-cycles 1000` prints `cycles=C1`; run 2 `--load-state s.state --max-cycles 1000` prints `cycles=C2` with `C1 + 1000 <= C2 <= C1 + 1010`; the overshoot bound covers the longest instruction).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat: versioned save states; evo-cli --load-state/--save-state`.

### Task 23: M3 — boot logo on the screen

**Files:** Modify `crates/evo-core/tests/milestones.rs`, `crates/evo-core/Cargo.toml` (dev-dep `png = "0.17"`; `sha2` is already a dependency from Task 22), `docs/hardware/{lcdc.md,boot-flow.md}`; possibly `crates/evo-hw/src/dev/lcdc.rs` (orientation fix with its test updated).

**Interfaces — Consumes:** `Machine::screen`, `Frame`, Task 16 `unexpected_unmapped`, `FATAL_STUB_IRQS`. **Produces** (milestones.rs):
```rust
const M3_CYCLES: u64;               // set in Step 1
const M3_SCREEN_SHA256: &str;       // lowercase hex SHA-256 of Frame::rgb, set in Step 2 (a hash holds no TI data)
/// #[ignore]d search helper, run by hand: run_until in 50_000_000-cycle slices from the M2 BSP-banner
/// cycle up to 3_000_000_000; after each slice hash screen(); stop after three identical non-blank
/// frames in a row; print the first cycle of the run of three; write that frame to target/m3.png.
#[test] #[ignore] fn m3_find_settled_logo();
```

- [ ] **Step 1: Find the settled logo** in one run: `cargo test --release -p evo-core --test milestones m3_find_settled_logo -- --ignored --nocapture`. `M3_CYCLES` = the printed cycle rounded up to a multiple of 50 M.
- [ ] **Step 2: Approve the image.** The executor opens `target/m3.png` (it stays in `target/`) and checks it against: a centred logo on a plain background, upright and not mirrored, in natural colours (a logo text, if any, reads left to right). If it is rotated or mirrored, change the mapping in `Lcdc::frame` to the one of the 8 orientations that is upright (and/or drop the BGR swap if the colours are swapped), update `frame_decode`'s expected pixels, rerun Step 1, and record the decision and the evidence (MADCTL 0x28 from the panel, CONTROL bit 8) in `lcdc.md`. Set `M3_SCREEN_SHA256` to the approved frame's hash and note in `boot-flow.md` that the image was approved and by whom (no image committed). If no orientation/colour choice gives an acceptable image, do not commit a hash: record the evidence in `boot-flow.md`, commit it as `docs: M3 image not approved`, and report to the user.
- [ ] **Step 3: Write the test** `m3_boot_logo`: run to `M3_CYCLES`; `screen()` is `Some`, 320×240, its SHA-256 equals `M3_SCREEN_SHA256`; the backlight PWM brightness is > 0; a second machine run to the same cycle gives the same hash (determinism); no unmapped SRAM access past 1.5 MiB; `irq_lines() & FATAL_STUB_IRQS == 0`; `unexpected_unmapped(&os.m)` is empty (a new entry is either modelled in its device with a `fix(evo-hw)` commit, or appended to `KNOWN_UNMODELLED` with a `boot-flow.md` line saying why it is harmless).
- [ ] **Step 4:** With `EVO_OS_IMAGE` set, `cargo test --release -p evo-core --test milestones m3 -- --nocapture` → PASS, no `skipping` line (`m3_find_settled_logo` is ignored).
- [ ] **Step 5: Commit** `test: M3 — boot logo on the emulated screen`.

### Task 24: M4 — resolve the post-logo stall; launch OS image 1

This is a discovery task. Its outcome is image 1 running, or a documented stop with evidence and a report to the user.

**Files:** Modify `crates/evo-core/tests/milestones.rs`, `docs/hardware/{boot-flow.md,discovery-log.md}`, and whichever device the evidence implicates (with its own test).

**Interfaces — Consumes:** all devices; GDB stub (`evo-cli run --gdb`); `Machine::set_key`, `set_gpio_input`, `set_usb_cc`, `take_power_off`; the `uart2: N bytes` stderr line (link-port traffic).

Known facts (`boot-flow.md`): after the logo the BSP creates the "LAUNCH" thread (entry 0x0015_1A55, TCB 0x2004_4DC0) and signals object 0x2001_02B8 with 6; the thread never ran in the research build. Image 1 is entered by `blx` to its reset word (0x0044_BC91) at 0x0015_2716; the OS-validation path is at 0x0015_2BA8 ("Validating OS...").

- [ ] **Step 1: Baseline.** Breakpoints 0x0015_1A54, 0x0015_2BA8, 0x0044_BC90; run up to 3_000_000_000 cycles with `--uart --unmapped-out target/m4.md`. If 0x0044_BC90 is hit, go to Step 4.
- [ ] **Step 2: Find what the BSP waits on.** At the stall, attach GDB: read the TCBs on the RTOS ready/wait lists starting from 0x2004_4DC0 and the object at 0x2001_02B8; identify the event flag or object the LAUNCH path waits for and the code that would set it (search for stores to that object). Record the finding in `discovery-log.md` §"Post-logo stall".
- [ ] **Step 3: Test hypotheses in this order, one experiment each** (a code change only if Step 2 or the experiment points at it; each change has a unit test in the owning device's test file and its own `fix(evo-hw): …` commit):
  1. LCDC VComp: IRQ 14's NS handler (0x0010_E2D0) runs more than once (break and count). If not, fix the LCDC model.
  2. Flash offset 0: service 0x106 type 0xD (secure 0x100d79fc) wants an MCUboot header (magic 0x96F3_B83D) at flash 0; trace which header fields it reads, then try a synthetic emulator-generated header with those fields (no TI bytes).
  3. GPIO pins 9 and 11 driven high (`set_gpio_input`).
  4. ON key, then a keypad press, 2 s after the logo (`--keys "wait=2000 on"`).
  5. USB Type-C CC fields (+0x1C bits 9:8 / 11:10) reporting attach (`Machine::set_usb_cc`, Task 15; read at 0x10119398/0x101193a8). Only after 3 and 4 are refuted (`critique.md` C.7 order).
  6. RTC busy/status bits (+0x40, +0x34) if the trace shows the BSP polling them.
  **Stop rule:** stop when 0x0044_BC90 is reached, or after these six experiments plus Step 2 without reaching it, or after 8 experiment runs that produce no new lead — then write the evidence (wait object, last unmapped accesses, which hypotheses were refuted and how) into `discovery-log.md`, commit it as `docs: post-logo stall investigation`, and report to the user instead of guessing further.
- [ ] **Step 4: Pin the launch.** Write `m4_image1_launches`: run to `Breakpoint(0x0044_BC90)` within the measured budget rounded up to the next 500 M; `resets == 0`; record the cycle. Run 500 M more cycles and record the first new unmapped accesses and any UART output from the OS.
- [ ] **Step 5: Document** in `boot-flow.md`: what unblocked the launch, the launch cycle, the BSP → OS service table at 0x2009_B100, and the next unmodelled accesses (input for the M4 keypad/RTC work and Plan 3).
- [ ] **Step 6:** With `EVO_OS_IMAGE` set, `cargo test --release -p evo-core --test milestones -- --nocapture` → PASS (M1–M4), no `skipping` line.
- [ ] **Step 7: Commit** `test: M4 — OS image 1 launches`; report to the user.
