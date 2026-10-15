# TI-84 Evo Emulator — Plan 4: CEmu-style Features for `evo-gui`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `evo-gui` feel like CEmu: a ROM Setup Wizard on first launch, and CEmu's tools (file import/export, capture, full debugger, peripherals, macros/AutoTester, hardware, instances, updates) in a grouped left sidebar with popovers and a right-side debug dock.

**Architecture:** Layering is unchanged (`armv8m` → `evo-hw` → `evo-core` → `apps/evo-gui`; only `backend/machine.rs` names core crates). Lane 0 widens the `EmuBackend` trait once (every new method, with a full `FakeBackend` and stubbed `MachineBackend`), adds a `Platform` seam (dialogs, clipboard, browser, HTTP, process spawn, backend opening) and a `Feature` seam (each feature owns one module and is driven by the app through `on_action` / `frame` / `ui`), and replaces the toolbar with grouped sidebar popovers. Lanes A–F then work in parallel on disjoint files; Lane W finally wires `MachineBackend` to the new core API from Lane A. Long debugger runs (step over/out, run to) are non-blocking goals the frame loop runs like normal emulation.

**Tech Stack:** Rust 2024 (toolchain 1.98), egui/eframe/egui_kittest 0.36.2, new GUI deps (exact pins, checked with `cargo info` on 2026-10-15): `rfd = "0.17.2"`, `arboard = "3.6.1"`, `png = "0.18.1"` (APNG), `openh264 = "0.9.9"` (default feature `source`: builds OpenH264 from C sources), `mp4 = "0.14.0"`, `ureq = "3.4.2"`, `webbrowser = "1.2.4"`, `sha2 = "0.10.9"`. Tool (not a dependency): `cargo-about 0.9.2`.

**Spec:** `docs/superpowers/specs/2026-10-15-cemu-features-design.md`. Prior plans for conventions: `docs/superpowers/plans/2026-10-14-gui.md` (incl. its Amendments A1–A5) and `2026-10-14-peripherals.md`.

## Global Constraints

- **Gates** (every commit, every lane, and after every merge): `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Tasks that add or change goldens also run `cargo test -p evo-gui --features snapshots --test "snapshots*"`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Commit on the lane branch only; never push, tag or publish.
- Only `apps/evo-gui/src/backend/machine.rs` may contain the strings `evo_core`, `evo_hw`, `evo_image`, `armv8m` — anywhere in `src/**/*.rs`, comments and UI text included (`tests/boundary.rs`).
- Golden PNGs are renders on `FakeBackend` only; no TI OS frame, logo or artwork is committed. Real-OS tests (`EVO_OS_IMAGE`, release only) write images to the temp dir only and print `skipping:` and pass when the env var is unset or in debug builds (pattern of `tests/real_os.rs`).
- No panic on any user file, dropped file, typed value or network reply: errors become toasts (`Import failed: …`) or inline wizard text; on any import error the machine is unchanged.
- No dependency is added after Lane 0. A lane that finds it needs one stops and reports; the merger adds it and regenerates `Cargo.lock`.
- Network only when the user clicks "Check for updates"; processes are spawned only by "New instance" / "Change ID". Repository constant: `UPDATE_REPO = "connerglover/ti84evo-emu"`.
- Emulation stays on the UI thread (Plan 3 constraint); the only worker threads are provisioning (A2) and the update check.
- Each file has one owner lane (table below). A lane edits only its files; Lane 0 edits are done before any other lane starts.
- English only. Labels below are exact strings (tests find widgets by them).
- `CLOCK_HZ = 156_000_000`; one LCD frame is `LCD_FRAME_CYCLES = CLOCK_HZ / 60 = 2_600_000` cycles.

## Review Focus

1. **Continue after a watchpoint stop.** A data watchpoint stops after the accessing instruction; Continue must move on, not stop on the same access forever; an exec watchpoint must be stepped over like a breakpoint. Tests: Lane A `continue_after_data_watch_moves_on`, `exec_watch_resumes_like_a_breakpoint`; Lane 0 fake `watchpoint_fires_once_per_arm`.
2. **A step over / step out / run-to that never finishes** (an endless loop, an address never reached). Expected: the UI keeps running frames, Pause cancels the goal, a later Continue runs freely. Tests: Lane 0 `pause_cancels_goal`, `endless_goal_keeps_running_until_pause` (fake `goal_never_reached()`); Lane D `endless_step_out_pause_then_continue`; Lane A `goal_is_disarmed_by_disarm`, `unreached_goal_returns_at_budget_and_stays_armed`.
3. **Importing the wrong file** (garbage, truncated, wrong size, a directory, a 2 GiB file, another backend's state). Expected: `Import failed: …` toast, machine unchanged, no unbounded read (every import, flash included, checks `metadata().len()` before reading). Tests: Lane F `import_*_garbage_keeps_machine`, `import_oversized_file_is_refused_unread`, `import_flash_oversized_is_refused_unread`, `import_flash_directory_fails`, `import_state_from_other_backend_keeps_machine`, `import_settings_wrong_format_keeps_prefs`; Lane A `import_ram_rejects_*`; Lane W `load_state_from_other_backend_is_refused_unchanged`.
4. **A recording that cannot be written or never gets a frame** (unwritable dir, display off, paused, app closed mid-recording). Expected: recording stops with a toast; when the cap is reached, on stop and on app exit the frames so far form a valid file; a write error removes the partial file (neither APNG nor MP4 can be finalized after one); no frames means no file. Tests: Lane C `recording_into_unwritable_dir_stops_with_toast`, `no_frames_writes_no_file`, `paused_or_no_display_pushes_nothing`, `on_exit_finishes_recording`, `apng_write_error_stops_cleanly`, `mp4_into_unwritable_dir_stops_with_toast`, `mp4_on_exit_finishes_recording`.
5. **Macro timing at speed "unlimited" or across a pause.** Expected: key events land at their scripted cycles (the run budget stops at the next scripted event), so playback is identical at any speed; the schedule is cycle-based, so a pause freezes it with the held keys still held and a resume continues it unchanged. Tests: Lane 0 `deadline_caps_run_budget`, `feature_deadline_stops_the_run`; Lane E `playback_at_unlimited_speed_hits_exact_cycles`, `playback_across_pause_hits_exact_cycles`.

---

## Lanes, dependencies and merge order

```
main ── Lane 0 (alone) ──┬── Lane A  core debug hooks (crates/**, evo-cli)
                          ├── Lane B  ROM wizard, instances, reload/change ROM, reset everything
                          ├── Lane C  capture: clipboard, APNG, MP4
                          ├── Lane D  debug dock panels (FakeBackend)
                          ├── Lane E  tools: key history, macros/AutoTester, hardware
                          └── Lane F  files import/export, help (licenses, updates, bug report)
            merge A, D, B, C, E, F (any order; gates after each) ── Lane W (MachineBackend wiring + real-OS tests)
```

- Lane 0 merges into `main` first. Then each lane `X` in `A B C D E F`: `git worktree add ../ti84evo-emu-lane-X -b lane/X main` (all from the same post-Lane-0 commit).
- Merge with `git merge --no-ff lane/X` into `main`, gates after each merge. A conflict means a lane broke file ownership: resolve in favour of the owner.
- Lane W starts from `main` after all six merged (its real-OS tests use the wizard and macros).
- Lane A also edits `backend/machine.rs`, but only the `run_status` match (two new `StopReason` arms), so the build stays green; nobody else touches `machine.rs` before W. `evo_core` re-exports `AccessDir` and `BusRecord`, the same names as the GUI's `backend::{AccessDir, BusRecord}`: in `machine.rs` both A and W import the core types as `use evo_core::{AccessDir as CoreDir, BusRecord as CoreRecord};` and keep the GUI names unqualified.
- Lane tests drive the app only through `tests/common/mod.rs` helpers (`harness_with`, `snap_harness_with`) with `FakePlatform`; no test anywhere builds `AppOptions` with `DesktopPlatform` (it would open real dialogs or a browser under `cargo test`).

### File ownership

| Files | Owner |
|---|---|
| `Cargo.toml`, `Cargo.lock`, `apps/evo-gui/Cargo.toml`, `.github/workflows/ci.yml` | 0 |
| `apps/evo-gui/src/{app,main,lib,toolbar,prefs,pacer,platform,logbuf,feature,features}.rs`, `src/backend/{mod,fake,placeholder}.rs`, `src/input/latch.rs` (`next_release`) | 0 |
| `src/backend/machine.rs` | 0 (stubs, `inspect_bundle`) → A (`run_status` arms only) → W |
| `crates/**` (src and tests), `apps/evo-cli/src/run.rs` | A |
| `src/wizard/**`, `src/instance.rs` (Lane 0 writes stubs) | B |
| `src/capture/**` (stub by 0) | C |
| `src/debug/**` (Lane 0 edits `debug/mod.rs` seam only) | D |
| `src/tools/**` (stub by 0) | E |
| `src/files/**`, `src/help/**` (stubs by 0), `src/store.rs`, `apps/evo-gui/about.toml`, `apps/evo-gui/about.hbs`, `apps/evo-gui/assets/third-party-licenses.txt` | F |
| tests `foundation.rs`, `logbuf.rs`, `fake.rs`, `keymap.rs`, `store.rs`, `snapshots.rs`, `common/mod.rs`, `boundary.rs`, `app.rs` (all but its debug tests) | 0 |
| tests `wizard.rs`, `instance.rs`, `snapshots_wizard.rs` | B |
| tests `capture.rs` | C |
| tests `debug_dock.rs`, `debug.rs` (explicitly D after Lane 0), `snapshots_debug.rs`, the debug tests in `app.rs`, golden `debug_panels.png` | D |
| tests `tools.rs`, `snapshots_tools.rs` | E |
| tests `files.rs`, `help.rs`, `snapshots_help.rs` | F |
| tests `machine_backend.rs`, `real_os.rs` | 0 (stub test) → W |
| every other existing file (`src/{keymap_window,lcd,theme,toast}.rs`, `src/input/**` except `latch.rs`, `src/keypad/**`, `tests/{input,layout}.rs`, `README.md`, `release.yml`, …) | 0 may edit; frozen after Lane 0 (no lane edits; a lane that needs one stops and reports) |

No test owned by Lane 0 may depend on an action staying unowned: once all lanes merge, every one of the 39 actions has an owner. Lane 0 tests the fallback text only as the pure function `not_available`.

## File Structure

```
Cargo.toml                    + workspace deps (0)
.github/workflows/ci.yml      gui-snapshots job runs --test "snapshots*" (0)
crates/armv8m/src/exception.rs  + Cpu::active_exceptions (A)
crates/evo-hw/src/watch.rs      WatchRange, WatchKinds, AccessDir, WatchHit, BusRecord, BusMonitor (A)
crates/evo-hw/src/memmap.rs     watch/monitor hook, inspect_devices (A)
crates/evo-hw/src/device.rs     RegView, Device::inspect (A)
crates/evo-hw/src/dev/*.rs      inspect for timer rtc uart lcdc(+pwm) gpio keypad spif mpc sysctl; Rtc::set_seconds_of_day (A)
crates/evo-core/src/machine.rs  StopReason::{Watchpoint, Goal}, exec watch, goals, batched_runs (A)
crates/evo-core/src/debug.rs    RunGoal, write_reg, peripherals, power_state, set_rtc (A)
crates/evo-core/src/ram.rs      EVORAM image (A)
apps/evo-gui/src/
  backend/mod.rs        trait + new value types (0)
  backend/fake.rs       FakeBackend for every new method (0)
  backend/placeholder.rs  Placeholder (was app.rs `Away`; also "No ROM") (0)
  backend/machine.rs    stubs + inspect_bundle (0); real wiring (W)
  platform.rs           Platform, PlatformRef, FileFilter, DesktopPlatform, FakePlatform (0)
  logbuf.rs             tee logger → console "Log" (0)
  feature.rs            Feature, FeatureCtx, AppRequest (0)
  features.rs           Features registry (0)
  toolbar.rs            Group, Action, sidebar, popovers, slot picker, About (0)
  pacer.rs              + Speed (0)
  prefs.rs              + bundle, compact, speed (0)
  instance.rs           instance dirs, ids, lock, rename (B)
  wizard/{mod,validate,instances,reset}.rs   ROM Setup Wizard, Reload/Change ROM, Instances, Reset everything (B)
  capture/{mod,sampler,apng,mp4}.rs          Copy screen, APNG, MP4 (C)
  debug/{mod,control,regs,disasm,memory,search,visualizer,breakpoints,watchpoints,peripherals,busmon,stacks,console,unmapped}.rs (D)
  tools/{mod,script,history,macros,hardware}.rs (E)
  files/mod.rs, help/{mod,update}.rs, store.rs (F)
apps/evo-gui/about.toml, about.hbs, assets/third-party-licenses.txt (F)
```

---

## Lane 0 — Foundation (runs alone, first)

### Task 0.1: Backend trait extension, FakeBackend, Placeholder, MachineBackend stubs

**Files:**
- Modify: `apps/evo-gui/src/backend/mod.rs`, `src/backend/fake.rs`, `src/backend/machine.rs`, `src/app.rs` (remove `Away`; `status_text` arms)
- Create: `src/backend/placeholder.rs`
- Test: `tests/fake.rs`, `tests/app.rs` (`status_text_strings`), `tests/machine_backend.rs`

**Interfaces — Produces** (`backend/mod.rs`; all `Clone, Debug, PartialEq`, plus `Copy, Eq` where every field allows, plus `Serialize, Deserialize` on `UartId`, `AccessDir`, `WatchKinds`, `Watchpoint`, `Goal`):
```rust
pub enum UartId { Uart0, Uart2 }
pub enum AccessDir { Read, Write, Exec }
impl AccessDir { pub fn as_str(self) -> &'static str }        // "read" | "write" | "exec"
#[derive(Default)] pub struct WatchKinds { pub read: bool, pub write: bool, pub exec: bool }
pub struct Watchpoint { pub start: u32, pub len: u32, pub kinds: WatchKinds }
impl Watchpoint { pub fn is_valid(&self) -> bool;             // len >= 1, start + len - 1 <= u32::MAX, any kind
                  pub fn last(&self) -> u32; }                 // start + len - 1 (saturating)
pub struct BusRecord { pub addr: u32, pub width: u8, pub dir: AccessDir, pub value: u32, pub pc: u32, pub cycle: u64 }
pub struct RegRow { pub name: String, pub offset: u32, pub value: u32, pub note: Option<String> }
pub struct DeviceView { pub name: String, pub base: u32, pub regs: Vec<RegRow> }
pub enum Goal { StepOver, StepOut, RunTo(u32) }
pub struct KeyEvent { pub key: KeyId, pub pressed: bool, pub cycle: u64 }
pub struct MemRegion { pub name: String, pub start: u32, pub len: u32 }
pub struct HardwareView { pub backlight: f32, pub charger_line: bool, pub battery_ok: bool, pub usb_attached: bool,
                          pub power_hold: bool, pub deep_sleep: bool, pub rtc_seconds: u32 }
pub enum HwChange { ChargerLine(bool), BatteryOk(bool), UsbAttached(bool), RtcSeconds(u32) }
// RunStatus gains (both is_stop):
//   Watchpoint { addr: u32, dir: AccessDir, value: u32, pc: u32 }, Goal { pc: u32 }
// BackendError gains:
//   #[error("address 0x{0:08x} is not writable")] Unwritable(u32),
//   #[error("no register {0}")] BadRegister(String),
//   #[error("{0}")] Invalid(String),
//   #[error("{0}")] Io(String),
// EmuBackend gains (no default methods):
fn poke(&mut self, addr: u32, bytes: &[u8]) -> Result<(), BackendError>;   // all or nothing
fn memory_regions(&self) -> Vec<MemRegion>;
fn set_core_reg(&mut self, index: usize, value: u32) -> Result<(), BackendError>; // 0..=15 as CpuView::core, 16 = xpsr
fn set_banked_reg(&mut self, name: &str, value: u32) -> Result<(), BackendError>; // CpuView::banked names
fn set_breakpoints(&mut self, addrs: Vec<u32>);
fn begin_goal(&mut self, goal: Goal) -> Result<bool, BackendError>;  // Ok(true) armed; Ok(false) done at once
fn cancel_goal(&mut self);
fn watchpoints(&self) -> Vec<Watchpoint>;
fn set_watchpoints(&mut self, w: Vec<Watchpoint>) -> Result<(), BackendError>; // any invalid ⇒ Err(Invalid), unchanged
fn set_bus_monitor(&mut self, range: Option<(u32, u32)>);  // (start, len)
fn bus_records(&self) -> Vec<BusRecord>;                    // oldest first; only while the tab is visible
fn clear_bus_records(&mut self);
fn peripherals(&self) -> Vec<DeviceView>;                   // only while the tab is visible
fn take_uart2(&mut self) -> Vec<u8>;
fn send_uart(&mut self, port: UartId, bytes: &[u8]);
fn take_key_events(&mut self) -> Vec<KeyEvent>;             // every set_key since the last call
fn hardware(&self) -> HardwareView;
fn set_hardware(&mut self, change: HwChange) -> Result<(), BackendError>;
fn export_ram(&self) -> Result<Vec<u8>, BackendError>;
fn import_ram(&mut self, bytes: &[u8]) -> Result<(), BackendError>;   // Err ⇒ unchanged
fn export_flash(&self, path: &Path) -> Result<(), BackendError>;
fn import_flash(&mut self, path: &Path) -> Result<(), BackendError>;  // Err ⇒ unchanged
// backend/placeholder.rs
pub struct Placeholder { .. }
impl Placeholder { pub fn new(name: &str, tag: &str) -> Self; pub fn no_rom() -> Self; } // no_rom: name "No ROM", tag "none"
// backend/machine.rs
pub struct BundleInfo { pub build_version: String, pub target_device: String, pub prodid: u32 }
pub fn inspect_bundle(bytes: &[u8]) -> Result<BundleInfo, OpenError>;   // Bundle::from_bytes metadata
// backend/fake.rs
pub const FAKE_WATCH_ADDR: u32 = 0x2000_0100;
pub const FAKE_BUS_CAP: usize = 4096;
impl FakeProbe { pub fn pokes(&self) -> Vec<(u32, Vec<u8>)>; pub fn goals(&self) -> Vec<Goal>;
                 pub fn hw_changes(&self) -> Vec<HwChange>; pub fn uart_sent(&self) -> Vec<(UartId, Vec<u8>)>;
                 pub fn flash_imports(&self) -> u32;
                 pub fn inspect_calls(&self) -> u32;     // peripherals() + bus_records() calls
                 pub fn key_events(&self) -> Vec<KeyEvent>;  // every set_key with its cycle; never drained
                 pub fn goal_cancels(&self) -> u32;      // cancel_goal calls
                 pub fn goal_armed(&self) -> bool; }
impl FakeBackend { pub fn goal_never_reached(self) -> Self; }  // builder: armed goals never finish
```
`Placeholder`: no frame, backlight 0, every run/step `Idle`, cycles 0, empty vectors, `Err(Unsupported(..))` for state/RAM/flash/poke/regs/goals/watchpoints/hardware, `needs_provision` false. `EvoApp::start_provision` uses `Placeholder::new(name, tag)` where it built `Away`.

`MachineBackend` (Lane 0): every new method returns `Err(BackendError::Unsupported("<what>"))` or an empty value / no-op; `run_status` maps nothing new yet. Lane W replaces these.

Fake behaviour (`FakeBackend`; overrides are cleared by `reset` and `load_state`):
- `poke`: every byte in `FAKE_MEM` goes to an overlay `BTreeMap<u32, u8>` that `peek` reads first; any byte outside ⇒ `Err(Unwritable(first such addr))`, nothing written. Logged in `pokes`.
- `memory_regions` = `[MemRegion { name: "fake RAM", start: 0x2000_0000, len: 0x1_0000 }]`.
- `set_core_reg(i, v)`: `i <= 16` overrides `cpu().core[i]` (16: `xpsr`); else `Err(BadRegister(format!("#{i}")))`. `set_banked_reg`: `msp_s`/`msp_ns` override; any other name ⇒ `Err(BadRegister(name))`.
- `set_breakpoints`: replaces the list, sorted and deduplicated.
- `begin_goal`: logged in `goals`. `StepOver` ⇒ `step()`, `Ok(false)`. `StepOut` arms target `FAKE_PC + 0x40`; `RunTo(a)` arms `a & !1`; both `Ok(true)`. While armed, `run_for(c)` adds `min(c, 1000)` cycles, disarms and returns `Goal { pc: target }`. With `goal_never_reached()`, an armed goal never finishes: `run_for(c)` behaves as unarmed (adds `c`, `Running`) and the goal stays armed until `cancel_goal`, `reset` or `load_state`. `cancel_goal` disarms and counts in `goal_cancels`.
- `set_watchpoints`: validates (`Err(Invalid("invalid watchpoint"))`), stores, and arms a one-shot hit when any watchpoint with `write` covers `FAKE_WATCH_ADDR`. The next `run_for` with the hit armed returns `Watchpoint { addr: FAKE_WATCH_ADDR, dir: Write, value: 0x2A, pc: FAKE_PC }` without advancing and clears the arm.
- `set_bus_monitor(Some((s, l)))`: while the range covers `FAKE_WATCH_ADDR`, each advancing `run_for` appends `BusRecord { addr: FAKE_WATCH_ADDR, width: 4, dir: Write, value: 0x2A, pc: FAKE_PC, cycle: <cycles before> }`, oldest dropped beyond `FAKE_BUS_CAP`. `None` stops recording, keeps records.
- `peripherals` = `[DeviceView{"timer", 0x4010_1000, [("ctrl",0x30,0x1,None), ("int_stat",0x34,0x0,None)]}, DeviceView{"nvic", 0xE000_E100, [("irq13",13,0x1,Some("enabled"))]}]`.
- `send_uart(p, b)`: logged in `uart_sent`, and echoed into that port's TX (`take_uart` / `take_uart2`).
- `set_key` also pushes `KeyEvent { key, pressed, cycle: cycles }` into two lists in `Shared`: the queue drained by `take_key_events` (the app drains it every frame) and the probe log `key_events()` (never drained; tests assert cycles on it).
- `save_state` stays JSON (unchanged format); `load_state` of non-JSON bytes (a `MachineBackend` state included) already fails ⇒ `Err(BadState(..))`, unchanged. A test pins this.
- `hardware()` starts `{1.0, true, true, false, true, false, 0}`; `set_hardware` applies and logs; `RtcSeconds(s)` with `s >= 86_400` ⇒ `Err(Invalid("RTC time must be below 24 h"))`.
- `export_ram` = `b"FAKERAM1"` + `serde_json` of the overlay as `Vec<(u32, u8)>`; `import_ram` without that magic or with bad JSON ⇒ `Err(BadState("not a RAM image"))`.
- `export_flash(path)` writes exactly `b"FAKEFLASH1"` (I/O ⇒ `Err(Io(e.to_string()))`); `import_flash(path)` needs exactly those bytes (else `Err(BadState("not a flash image"))`, read errors `Err(Io(..))`), then zeroes cycles and increments `flash_imports`.

`status_text`: `Watchpoint` ⇒ `format!("Watchpoint ({}) at 0x{addr:08x}, pc 0x{pc:08x}", dir.as_str())`; `Goal` ⇒ `format!("Stopped at 0x{pc:08x}")`; both shown while paused, like `Breakpoint`.

- [ ] **Step 1: Write the failing tests.** `tests/fake.rs`: `poke_writes_overlay_and_peek_sees_it` (`poke(0x2000_0010,&[1,2])` then `peek(0x2000_0010,2)==[Some(1),Some(2)]`); `poke_outside_is_all_or_nothing` (`poke(0x2000_FFFF,&[9,9])==Err(Unwritable(0x2001_0000))`, `peek(0x2000_FFFF,1)` unchanged); `register_overrides` (core 13, 16, banked `msp_ns`; `set_core_reg(17,0)` and `set_banked_reg("r99",0)` are `BadRegister`); `goals_arm_and_finish` (`StepOver` ⇒ `Ok(false)`, cycles +1; `RunTo(0x1000_0203)` ⇒ `Ok(true)` then `run_for(5000)==Goal{pc:0x1000_0202}` and cycles +1000, next `run_for` is `Running`); `cancel_goal_disarms` (`goal_cancels()==1`, `goal_armed()==false`); `never_reached_goal_stays_armed` (`goal_never_reached()`, `RunTo(..)` ⇒ `Ok(true)`, three `run_for(5000)` ⇒ `Running`, cycles +15000, `goal_armed()`); `watchpoint_fires_once_per_arm` (write watch over `FAKE_WATCH_ADDR`: first `run_for` = `Watchpoint{..}` with cycles unchanged, second = `Running`; invalid `Watchpoint{len:0,..}` ⇒ `Err(Invalid)` and `watchpoints()` unchanged); `bus_monitor_ring_is_bounded` (`FAKE_BUS_CAP + 10` runs ⇒ `bus_records().len()==FAKE_BUS_CAP`, oldest cycle dropped; clear ⇒ empty); `uart_send_echoes_per_port`; `key_events_carry_cycles` (`run_for(500)`, `set_key(Enter,true)` ⇒ `take_key_events()==[KeyEvent{Enter,true,500}]`, second take empty; `probe.key_events()` still `[KeyEvent{Enter,true,500}]`); `state_of_other_backend_is_refused` (`run_for(500)`, `load_state(b"EVOSTATE\x01\x00binary")` ⇒ `BadState`, cycles still 500); `hardware_changes` (incl. `RtcSeconds(86_400)` ⇒ `Invalid`); `ram_image_roundtrip_and_garbage`; `flash_file_roundtrip_and_garbage` (temp dir via `common::TempDir`); `set_breakpoints_sorts_and_dedups`. `tests/app.rs::status_text_strings` adds the two new strings. `tests/machine_backend.rs`: `inspect_bundle_reads_metadata` (testutil bundle ⇒ `build_version "9.8.7.6543"`, `target_device "84XX"`, `prodid 42`; `inspect_bundle(b"nope")` is `Err`) and `new_methods_are_unsupported_until_wired` (`poke` ⇒ `Err(Unsupported(_))`).
- [ ] **Step 2: Run** `cargo test -p evo-gui --test fake --test app --test machine_backend` → FAIL (compile errors: missing types/methods).
- [ ] **Step 3: Implement** the types, trait methods, fake behaviour, `Placeholder` (move `Away` out of `app.rs`), `MachineBackend` stubs and `inspect_bundle`.
- [ ] **Step 4: Run** the same tests → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): widen EmuBackend for the CEmu-style tools; fake and placeholder backends`.

### Task 0.2: Speed and new preferences

**Files:**
- Modify: `src/pacer.rs`, `src/prefs.rs`
- Test: `tests/foundation.rs` (create)

**Interfaces — Produces:**
```rust
// pacer.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]   // Hash: Action derives it
pub enum Speed { #[default] X1, X2, X4, Unlimited }
impl Speed { pub const ALL: [Speed; 4]; pub fn factor(self) -> f64;   // 1.0, 2.0, 4.0, 1000.0
             pub fn label(self) -> &'static str; }                    // "Speed 1×", "Speed 2×", "Speed 4×", "Speed unlimited"
// prefs.rs — new fields (serde(default) already on the struct)
pub bundle: Option<PathBuf>,   // the ROM the wizard linked
pub compact: bool,             // default false
pub speed: Speed,              // default X1
```
`Unlimited` is 1000× real time; `WALL_BUDGET` still caps each frame at 12 ms, so it means "as fast as the host allows".

- [ ] **Step 1: Write the failing tests** in `tests/foundation.rs`: `speed_factors_and_labels`; `unlimited_budget_is_wall_capped` (`Pacer{clock_hz:CLOCK_HZ, speed:Speed::Unlimited.factor()}.budget(1.0/60.0) == 2_600_000_000`); `old_prefs_load_with_new_defaults` (the JSON `{"filter":"Smooth"}` deserializes with `bundle None`, `compact false`, `speed X1`); `prefs_roundtrip_new_fields`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test foundation` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): speed setting and new preferences`.

### Task 0.3: Platform seam, dependencies, log buffer

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`: `rfd = "0.17.2"`, `arboard = "3.6.1"`, `png = "0.18.1"`, `openh264 = "0.9.9"`, `mp4 = "0.14.0"`, `ureq = "3.4.2"`, `webbrowser = "1.2.4"`, `sha2 = "0.10.9"`), `apps/evo-gui/Cargo.toml` (all eight as `x.workspace = true`), `Cargo.lock`, `src/lib.rs`, `src/main.rs` (`logbuf::init()` replaces `env_logger::init()`), `.github/workflows/ci.yml` (comment only), `README.md` (OpenH264 sentence, see Decisions)
- Create: `src/platform.rs`, `src/logbuf.rs`
- Test: `tests/foundation.rs`

**Interfaces — Produces:**
```rust
// platform.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileFilter { pub name: &'static str, pub extensions: &'static [&'static str] }
pub trait Platform: Send + Sync + std::fmt::Debug {
    fn pick_open(&self, title: &str, filters: &[FileFilter]) -> Option<PathBuf>;
    fn pick_save(&self, title: &str, default_name: &str, filters: &[FileFilter]) -> Option<PathBuf>;
    fn open_backend(&self, bundle: &Path, nv_dir: &Path) -> Result<Box<dyn EmuBackend>, String>;
    fn copy_image(&self, frame: &Frame) -> Result<(), String>;
    fn open_url(&self, url: &str) -> Result<(), String>;
    fn http_get(&self, url: &str) -> Result<String, String>;   // blocking; call from a worker thread; non-2xx ⇒ Err("HTTP {code}")
    fn spawn_instance(&self, args: &[String]) -> Result<(), String>;  // the current exe with these args
}
pub type PlatformRef = Arc<dyn Platform>;
#[derive(Debug, Default)] pub struct DesktopPlatform;
impl DesktopPlatform { pub fn shared() -> PlatformRef; }
#[derive(Debug, Default)] pub struct FakePlatform { /* Mutex-guarded queues and logs */ }
impl FakePlatform {
    pub fn shared() -> Arc<FakePlatform>;                       // coerces to PlatformRef with .clone()
    pub fn queue_open(&self, answer: Option<PathBuf>);          // FIFO; empty queue ⇒ None
    pub fn queue_save(&self, answer: Option<PathBuf>);
    pub fn set_http(&self, reply: Result<String, String>);      // default Err("offline")
    pub fn fail_copy(&self, msg: &str);
    pub fn set_backend_factory(&self, f: Box<dyn Fn(&Path, &Path) -> Result<Box<dyn EmuBackend>, String> + Send + Sync>); // (bundle, nv_dir), as open_backend
    pub fn asked(&self) -> Vec<String>;                         // dialog titles, in order
    pub fn copied(&self) -> usize; pub fn urls(&self) -> Vec<String>;
    pub fn spawned(&self) -> Vec<Vec<String>>; pub fn opened(&self) -> Vec<PathBuf>;
}
// logbuf.rs
pub const LOG_CAP: usize = 1000;
pub struct LogBuf { .. }          // Mutex<VecDeque<String>>; the logic lives here and is unit-tested directly
impl LogBuf { pub fn new(cap: usize) -> Self; pub fn record(&self, line: String); pub fn drain(&self) -> Vec<String>; }
pub fn init();                    // env_logger (env-configured) + a copy of Info and above into the global LogBuf
pub fn record(line: String);      // thin wrapper over the global LogBuf (a OnceLock<LogBuf> with LOG_CAP)
pub fn drain() -> Vec<String>;    // new lines since the last drain, oldest first, at most LOG_CAP
```
The global ring is shared by every test thread of one binary and every app frame drains it, so only `LogBuf` instances are tested in `foundation.rs`; the one end-to-end test that uses the global (`uart2_and_log_reach_the_debug_panel`, Task 0.5a) lives alone in its own binary `tests/logbuf.rs`, and no other test calls `logbuf::record`.
`DesktopPlatform`: `rfd::FileDialog` (synchronous) with the filters; `MachineBackend::open(..).map(Box::new).map_err(to_string)`; `arboard::Clipboard::new()?.set_image(ImageData { width, height, bytes: Cow::Borrowed(&frame.rgba) })`; `webbrowser::open`; `ureq::get(url).header("User-Agent", concat!("evo-gui/", env!("CARGO_PKG_VERSION"))).header("Accept", "application/vnd.github+json").call()`, body read to string, `ureq::Error::StatusCode(c)` ⇒ `Err(format!("HTTP {c}"))`; `std::process::Command::new(std::env::current_exe()?).args(args).spawn()`. The fake's default backend factory returns `FakeBackend::new().named(<file name>)`. Log line format: `"{LEVEL} {target}: {message}"`.

- [ ] **Step 1: Write the failing tests** in `tests/foundation.rs`: `fake_platform_queues_answers_in_order` (two `queue_open`, third call `None`, `asked()` has three titles); `fake_platform_default_backend_is_named_after_file`; `fake_platform_http_default_is_offline`; `platform_ref_is_send_sync`; `fake_platform_factory_gets_nv_dir` (factory sees both paths); `logbuf_drains_new_lines_once` (`let b = LogBuf::new(LOG_CAP)`, `b.record("INFO x: a")`, `b.drain()==["INFO x: a"]`, second drain empty); `logbuf_keeps_the_last_cap_lines` (`LogBuf::new(3)`, five lines ⇒ the last three).
- [ ] **Step 2: Run** `cargo test -p evo-gui --test foundation` → FAIL.
- [ ] **Step 3: Implement**; add the dependencies.
- [ ] **Step 4: Run** → PASS. Then `cargo build --release -p evo-gui` on this Windows host: it must compile `openh264-sys2` from source (verified in a scratch crate on 2026-10-15: openh264 0.9.9 builds with MSVC and encodes a 640×480 frame). If it does not, replace `openh264` + `mp4` with `gif = "0.14.2"`, append "MP4 → GIF fallback taken: <reason>" under a new `## Amendments` heading at the end of this plan, and Lane C follows Task C3's GIF branch. Linux: if `wsl -l -q` lists a distro, also run `cargo build --release -p evo-gui` there (after `apt-get install -y build-essential libxkbcommon-dev libwayland-dev`) and record the result in the commit message; otherwise record "Linux build unverified locally". `rfd` 0.17 (xdg-portal default) and `arboard` 3 (x11rb) need no new apt packages; `openh264-sys2` needs only a C/C++ compiler, which ubuntu runners have, so `ci.yml`'s apt line stays as is — add a comment above it naming these three crates. The fallback is taken only when the local release build fails; CI on Linux is verified at the user's first push (see Decisions). Gates.
- [ ] **Step 5: Commit** `feat(evo-gui): platform seam (dialogs, clipboard, browser, HTTP, spawn), log buffer, new dependencies`.

### Task 0.4: Grouped sidebar with popovers, compact mode, reset layout

**Files:**
- Modify: `src/toolbar.rs` (`Tool` → `Group` + `Action`), `src/app.rs`, `tests/app.rs`, `tests/keymap.rs`, `tests/store.rs` (`tool_table` → `group_and_action_tables`; icons test over `Group::ALL`), `tests/snapshots.rs`, `tests/common/mod.rs`, `.github/workflows/ci.yml` (gui-snapshots: `--test "snapshots*"`), goldens `tests/snapshots/*.png` (regenerated) + new `sidebar_calculator_popover.png`
- Test: `tests/foundation.rs`, the migrated tests

**Interfaces — Produces** (`toolbar.rs`):
```rust
pub enum Group { Calculator, Capture, Files, Debug, Tools, Settings, Help }   // Copy, Eq, Debug
impl Group { pub const ALL: [Group; 7];
  pub fn icon(self) -> &'static str;     // "\u{1F5A9}" "\u{1F4F7}" "\u{1F4C1}" "\u{1F41B}" "\u{1F527}" "\u{2699}" "\u{2753}"
  pub fn fallback(self) -> &'static str; // "Calc" "Shot" "Files" "Debug" "Tools" "Prefs" "Help"
  pub fn tooltip(self) -> &'static str;  // "Calculator" "Capture" "Files" "Debug panels" "Tools" "Settings" "Help"
}
pub enum Action {  // Copy, Eq, Hash, Debug
  Reset, Reprovision, ReloadRom, ChangeRom, SaveState, LoadState, TogglePause, Speed(Speed),
  Screenshot, CopyScreen, RecordApng, RecordVideo,
  ImportRam, ExportRam, ImportState, ExportState, ImportDebug, ExportDebug, ImportSettings, ExportSettings, ImportFlash, ExportFlash,
  ToggleDebug, KeyHistory, Macros, Hardware,
  Keymap, Smooth, Instances, Compact, ResetLayout, ResetEverything,
  About, Licenses, CheckUpdates, ReportBug,
}
impl Action { pub const ALL: [Action; 39];   // the order above, Speed as X1, X2, X4, Unlimited
  pub fn group(self) -> Group;              // explicit mapping below
  pub fn label(self) -> &'static str; }
// group(): Reset, Reprovision, ReloadRom, ChangeRom, SaveState, LoadState, TogglePause, Speed(_) → Calculator;
//   Screenshot, CopyScreen, RecordApng, RecordVideo → Capture; every Import*/Export* → Files;
//   ToggleDebug → Debug; KeyHistory, Macros, Hardware → Tools;
//   Keymap, Smooth, Instances, Compact, ResetLayout, ResetEverything → Settings;
//   About, Licenses, CheckUpdates, ReportBug → Help.
pub fn drawable_icons(ctx: &egui::Context) -> [bool; 7];
pub fn sidebar_buttons(ui: &mut egui::Ui, icons: &[bool; 7], open: Option<Group>, active: impl Fn(Group) -> bool) -> SidebarOutput;
pub struct SidebarOutput { pub clicked: Option<Group>, pub clicked_rect: Option<Rect> }
pub fn group_popover(ctx: &egui::Context, pos: Pos2, group: Group, active: impl Fn(Action) -> bool) -> PopoverOutput;
pub struct PopoverOutput { pub picked: Option<Action>, pub clicked_elsewhere: bool }
pub fn not_available(action: Action) -> String;   // format!("{} is not available yet", action.label())
// app.rs
impl EvoApp { pub fn run_action(&mut self, a: Action); pub fn compact(&self) -> bool; pub fn speed(&self) -> Speed; }
// AppOptions gains: pub platform: PlatformRef (AppOptions::new(): DesktopPlatform::shared(); the app keeps a clone)
// tests/common/mod.rs (gets #![allow(dead_code)])
pub fn open_action(h: &mut Harness<'static, EvoApp>, a: Action);   // clicks a.group().tooltip(), then a.label(), runs 3 steps; ToggleDebug clicks the group only
pub fn gpu_lock() -> MutexGuard<'static, ()>;
pub fn test_options(dir: &Path, platform: PlatformRef) -> AppOptions;   // AppOptions::new(dir) with this platform (never Desktop)
pub fn harness_with(fake: FakeBackend, opts: AppOptions) -> Harness<'static, EvoApp>;          // 520×960, steps once
pub fn snap_harness_with(fake: FakeBackend, size: [f32; 2], opts: AppOptions) -> Harness<'static, EvoApp>; // light theme, ppp 1, 10 steps
pub fn snap_harness(fake: FakeBackend, size: [f32; 2], platform: PlatformRef) -> Harness<'static, EvoApp>;
    // = snap_harness_with(fake, size, test_options(tempdir, platform) with start_paused: true)
```
Lanes set `open_wizard`, `instance`, `rom`, `prefs` or `start_paused` on the `AppOptions` from `test_options` and pass it to `harness_with` / `snap_harness_with`.
Labels: `Reset` "Reset", `Reprovision` "Re-provision", `ReloadRom` "Reload ROM", `ChangeRom` "Change ROM…", `SaveState` "Save state", `LoadState` "Load state", `TogglePause` "Pause", `Speed(s)` `s.label()`, `Screenshot` "Screenshot", `CopyScreen` "Copy screen", `RecordApng` "Record animated PNG", `RecordVideo` "Record MP4", `ImportRam` "Import RAM image…", `ExportRam` "Export RAM image…", `ImportState` "Import calculator state…", `ExportState` "Export calculator state…", `ImportDebug` "Import debug state…", `ExportDebug` "Export debug state…", `ImportSettings` "Import settings…", `ExportSettings` "Export settings…", `ImportFlash` "Import flash image…", `ExportFlash` "Export flash image…", `ToggleDebug` "Debug panels", `KeyHistory` "Key history", `Macros` "Macros / AutoTester", `Hardware` "Hardware", `Keymap` "Keyboard mapping", `Smooth` "Smooth scaling (better at non-integer sizes)", `Instances` "Instances…", `Compact` "Compact mode", `ResetLayout` "Reset layout", `ResetEverything` "Reset everything…", `About` "About", `Licenses` "Third-party licenses", `CheckUpdates` "Check for updates", `ReportBug` "Report a bug".

Behaviour:
- The sidebar (`TOOLBAR_W` 48, same frame) has one button per group, focus-free (`Sense::CLICK`). `Debug` toggles the dock directly; every other group toggles its popover (an `Area` beside the button like the slot picker, one focus-free button per action of the group in `Action::ALL` order, `selected` when active). Picking an action closes the popover; `SaveState`/`LoadState` then open the slot picker at the same place. A click elsewhere (not on the sidebar) closes it.
- App-owned actions in this task: `Reset`, `Reprovision`, `SaveState`, `LoadState`, `TogglePause` (active while paused), `Speed(s)` (sets `pacer.speed = s.factor()`, active when current), `Screenshot`, `ToggleDebug`, `Keymap`, `Smooth`, `Compact`, `ResetLayout`, `About`. Any other action toasts `not_available(a)` (Info) — Task 0.5a routes them to features. Power: the Calculator group has no separate power entry; the spec's "power" is served by Tools › Hardware › "Press ON" (Task E4), recorded under Decisions. The provisioning guard applies to `Reset`, `Reprovision`, `SaveState`, `LoadState`, `Screenshot`, `ReloadRom`, `ChangeRom`, every `Import*`/`Export*`.
- `Compact`: header and sidebar are not shown; the central panel gets a background `interact` (before the keys) whose `context_menu` holds one button, "Exit compact mode".
- `ResetLayout`: `debug_open = false`, `compact = false`, removes the debug `PanelState`, `ctx.memory_mut(|m| m.reset_areas())`, sends `ViewportCommand::InnerSize(vec2(520.0, 960.0))`, `grown = false`, toast "Layout reset".
- `prefs()` now also carries `bundle`, `compact`, `speed`; `EvoApp::new` applies `opts.prefs.speed`/`compact`.

- [ ] **Step 1: Write the failing tests.** `tests/foundation.rs`: `group_and_action_tables` (`Action::ALL.len()==39`, labels unique, every group but `Debug` has ≥ 1 action, `ToggleDebug.group()==Debug`); `every_action_label_is_drawable` (fallbacks and labels via `has_glyphs`); `popover_lists_its_group` (click "Capture" ⇒ labels "Screenshot", "Copy screen", "Record animated PNG", "Record MP4" exist, "Save state" does not); `popover_closes_after_pick_and_on_outside_click`; `speed_action_sets_pacer` (`open_action(Speed(X4))` ⇒ `h.state().speed()==X4`, `prefs().speed==X4`); `pause_action_toggles`; `not_available_text` (pure: `not_available(Action::ReportBug) == "Report a bug is not available yet"`; no click, because Lane F later owns `ReportBug`); `compact_hides_sidebar_and_context_menu_restores` (after `Compact`, `query_by_label("Calculator")` is `None` and "TI-84 Evo Emulator" is gone; secondary click at `calc_transform().to_screen(pos2(20.0, 20.0))`, click "Exit compact mode" ⇒ "Calculator" is back); `reset_layout_closes_dock_and_compact`. Migrate `tests/app.rs`, `tests/keymap.rs`, `tests/snapshots.rs` clicks on old tool labels to `open_action(&mut h, Action::…)`; `reset_button_resets_backend` now asserts `query_all_by_label("Reset").count() == 1` (only the toast: the popover closed after the pick); replace `toolbar_has_every_tool` with `sidebar_has_every_group`; every existing test that builds `AppOptions::new(..)` for a harness switches to `common::test_options(dir, FakePlatform::shared())` (or sets `platform` to a `FakePlatform`).
- [ ] **Step 2: Run** `cargo test -p evo-gui` → FAIL (no `Group`/`Action`).
- [ ] **Step 3: Implement**; then add `snapshot_sidebar_calculator_popover` (520×960, click "Calculator", hover at (500, 950)) to `tests/snapshots.rs` and regenerate every golden: `UPDATE_SNAPSHOTS=1 cargo test -p evo-gui --features snapshots --test snapshots`. Open each PNG and check it shows only FakeBackend content and the new sidebar.
- [ ] **Step 4: Run** `cargo test -p evo-gui --features snapshots --test "snapshots*"` (no `UPDATE_SNAPSHOTS`) → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): grouped sidebar with popovers, speed, compact mode, reset layout`.

### Task 0.5a: Feature framework, app requests, debug seam

**Files:**
- Create: `src/feature.rs`, `src/features.rs`, stub modules `src/wizard/mod.rs`, `src/instance.rs`, `src/capture/mod.rs`, `src/tools/mod.rs`, `src/files/mod.rs`, `src/help/mod.rs`, `tests/logbuf.rs`
- Modify: `src/app.rs`, `src/lib.rs`, `src/pacer.rs` (`capped_budget`), `src/input/latch.rs` (`next_release`), `src/debug/mod.rs` (seam only)
- Test: `tests/foundation.rs`, `tests/logbuf.rs`

**Interfaces — Produces:**
```rust
// feature.rs
pub struct FeatureCtx<'a> {
    pub backend: &'a mut dyn EmuBackend, pub toasts: &'a mut Toasts, pub platform: &'a PlatformRef,
    pub data_dir: &'a Path, pub prefs: &'a Prefs, pub rom: Option<&'a Path>, pub instance: Option<&'a str>,
    pub paused: bool, pub provisioning: bool, pub key_events: &'a [KeyEvent], pub requests: &'a mut Vec<AppRequest>,
}
pub enum AppRequest {
    Resync, Persist, Reset, Provision,
    ReplaceBackend { backend: Box<dyn EmuBackend>, rom: Option<PathBuf>, persist_old: bool },
    ApplyPrefs(Prefs), SetPaused(bool), ResetLayout, Close,
}
pub trait Feature {
    fn on_action(&mut self, action: Action, cx: &mut FeatureCtx<'_>) -> bool;  // false: not mine
    fn frame(&mut self, cx: &mut FeatureCtx<'_>);                          // in logic, after emulate
    fn ui(&mut self, ctx: &egui::Context, cx: &mut FeatureCtx<'_>);        // windows/modals, in ui
    fn active(&self, action: Action) -> bool;                               // popover highlight
    fn next_deadline(&self) -> Option<u64>;                                 // absolute cycle the run must stop at
    fn blocks_input(&self) -> bool;                                         // true: calculator keys ignored (modal open)
    fn on_exit(&mut self, cx: &mut FeatureCtx<'_>);                         // window closing; must not change prefs (already saved)
}
// features.rs
pub struct Features { pub rom: crate::wizard::RomSetup, pub capture: crate::capture::Capture,
                      pub tools: crate::tools::Tools, pub files: crate::files::Files, pub help: crate::help::Help,
                      extra: Vec<Box<dyn Feature>> }
impl Features { pub fn new(opts: &AppOptions) -> Self;
                pub fn each(&mut self) -> Vec<&mut dyn Feature>;   // the five in field order, then extra
                pub fn next_deadline(&self) -> Option<u64>;       // min over all
                pub fn blocks_input(&self) -> bool; }
// stubs: `pub struct X { _priv: () }` + `impl X { pub fn new(opts: &AppOptions) -> Self }` + Feature with no-op
// bodies (on_action false, active false, deadline None, blocks_input false). Owners replace the struct body with
// their own fields and keep `pub fn new(opts: &AppOptions) -> Self`; none of the five is a unit struct.
// instance.rs (stub; Lane B implements)
pub struct InstanceLock { pub dir: PathBuf }
#[derive(Debug, thiserror::Error)] pub enum InstanceError { #[error("{0}")] Invalid(String), #[error("instance {0} is already running")] InUse(String), #[error("{0}")] Io(String) }
pub fn prepare(base: &Path, id: Option<&str>, rename_from: Option<&str>) -> Result<InstanceLock, InstanceError>; // stub: dir = base or base/instances/id, create_dir_all
// pacer.rs
pub fn capped_budget(budget: u64, now: u64, deadline: Option<u64>) -> u64;   // min(budget, deadline - now), saturating
// input/latch.rs
impl KeyLatch { pub fn next_release(&self) -> Option<u64>; }   // earliest pending release cycle (pressed_at + MIN_HOLD_CYCLES)
// app.rs
// AppOptions gains (platform came in 0.4): pub open_wizard: bool,
//                   pub rom: Option<PathBuf>, pub instance: Option<String>
impl EvoApp { pub fn apply_request(&mut self, r: AppRequest); pub fn rom(&self) -> Option<&Path>;
              pub fn features(&self) -> &Features; pub fn features_mut(&mut self) -> &mut Features;
              #[doc(hidden)] pub fn push_feature(&mut self, f: Box<dyn Feature>);   // tests only: appended to Features::extra
              pub fn debug(&self) -> &DebugPanel; pub fn close_requested(&self) -> bool; }   // true once `Close` was applied
// eframe::App for EvoApp gains (eframe 0.36 default features include glow):
//   fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>)   — tests call eframe::App::on_exit(h.state_mut(), None)
// debug/mod.rs seam (no longer Copy: it holds a Vec)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DebugOutput { pub toggle_pause: bool, pub step: bool, pub goal: Option<Goal>, pub notes: Vec<String> }
impl DebugPanel { pub fn push_uart(&mut self, port: UartId, bytes: &[u8]); pub fn console(&self, port: UartId) -> &UartConsole;
                  pub fn push_log(&mut self, lines: Vec<String>); pub fn log_lines(&self) -> &[String]; } // log keeps LOG_CAP
```
**Request rule** (every lane relies on it): each pass that hands features a `FeatureCtx` — `run_action` (→ `on_action`), the `logic` pass (→ every `frame`), the `ui` pass (→ every `ui`) and `on_exit` — collects into one local `Vec<AppRequest>` and applies it, in push order, right after that pass ends and before the next pass starts. A request pushed from `on_action` or `ui` therefore takes effect in the same frame; `on_exit` requests apply before the final persist. Requests are never carried across frames.

App loop changes:
- `logic`: after `emulate`, `let events = backend.take_key_events()`; then every feature's `frame` with a `FeatureCtx` (prefs snapshot `self.prefs()` built once per frame); then that pass's requests. UART: `take_uart` → `push_uart(Uart0)`, `take_uart2` → `push_uart(Uart2)`; `logbuf::drain()` → `push_log`.
- `emulate`: `budget = capped_budget(pacer budget, cycles, min of features.next_deadline() and latch.next_release())`; a budget of 0 runs nothing this frame. Capping at the latch's next release makes a clicked key's release land exactly at `press + MIN_HOLD_CYCLES` on `FakeBackend` (the real machine may overshoot by one instruction).
- `keyboard`: `typing |= features.blocks_input()`.
- `run_action`: the app's own actions first (Task 0.4); else each feature's `on_action` until one returns `true`; else `not_available`. Popover `active` asks the app, then features.
- `ui`: features' `ui` after the overlays. `on_exit` (eframe calls it after `save`, so prefs are already stored): features' `on_exit`, apply their requests, then persist the backend.
- `debug_panel`: `goal` while paused ⇒ `begin_goal`: `Ok(true)` unpauses (status `Running`), `Ok(false)` stays paused, `Err(e)` toasts. `notes` become Error toasts. Pausing (toggle, `SetPaused(true)`) calls `cancel_goal`; so do reset, load, `Resync` and `ReplaceBackend`. Code that copied `DebugOutput` now moves or clones it.
- Requests: `Resync` ⇒ `resync()`; `Persist`; `Reset` ⇒ `reset_machine()`; `Provision` ⇒ `start_provision()`; `ReplaceBackend` ⇒ persist the old one if `persist_old`, swap, set `rom`, `resync()`, provision if `needs_provision()`; `ApplyPrefs(p)` ⇒ keymap, filter, debug_open, compact, speed, bundle; `SetPaused(b)`; `ResetLayout`; `Close` ⇒ `ViewportCommand::Close`.

- [ ] **Step 1: Write the failing tests** in `tests/foundation.rs` (each harness from `common::harness_with(fake, common::test_options(dir, FakePlatform::shared()))`; "test feature" = a small `Feature` impl inside the test file, added with `push_feature`):
  - `replace_backend_persists_swaps_and_resyncs` (old probe `persists()==1`; `backend().name()=="b2"`; `rom()` set); `replace_backend_without_persist_skips_it`; `replace_with_unprovisioned_starts_provisioning` (`FakeBackend::new().needing_provision()` ⇒ `provisioning()` true); `apply_prefs_updates_app` (speed, compact, filter); `set_paused_true_cancels_goal` (`probe.goal_cancels()==1`); `close_request_is_recorded` (`apply_request(Close)` ⇒ `close_requested()`).
  - `deadline_caps_run_budget` (pure `capped_budget`: `(5_000, 100, Some(1_100)) ⇒ 1_000`, `(5_000, 100, Some(50)) ⇒ 0`, `(5_000, 100, None) ⇒ 5_000`); `feature_deadline_stops_the_run` (test feature with `next_deadline() == Some(start + 1_000)`; one running step ⇒ cycles are exactly `start + 1_000`); `click_release_lands_at_min_hold` (click the "enter" key on a running harness, step until released ⇒ `probe.key_events()` release cycle `== press + MIN_HOLD_CYCLES`).
  - `requests_from_action_and_ui_apply_in_the_same_frame` (test feature owning `Action::KeyHistory`, which the Tools stub declines: its `on_action` pushes `SetPaused(true)`, its `ui` pushes `ApplyPrefs` with `compact = true` once; `h.state_mut().run_action(KeyHistory)` ⇒ `paused()` at once; one `h.step()` ⇒ `compact()`); `on_exit_applies_feature_requests_then_persists` (test feature pushes `Persist` in `on_exit`; `eframe::App::on_exit(h.state_mut(), None)` ⇒ `persists()==2`).
  - `pause_cancels_goal` (paused harness: `backend_mut().begin_goal(Goal::RunTo(0x1000_0300))`, `apply_request(SetPaused(false))`, `apply_request(SetPaused(true))`, then `backend_mut().run_for(10) == Running`, not `Goal`); `endless_goal_keeps_running_until_pause` (`FakeBackend::new().goal_never_reached()`; arm `RunTo(0x1000_0300)`, `SetPaused(false)`, 20 steps ⇒ not paused, status `Running`, cycles grow every step; `SetPaused(true)` ⇒ `goal_cancels()==1`, `goal_armed()==false`; `SetPaused(false)`, 5 steps ⇒ still running, no `Goal` stop).
  - `features_are_reachable` (`h.state().features().rom.page() == None` against the stub's `page()` that Lane B keeps); `instance_stub_dirs` (`prepare(base, None, None).dir == base`, `Some("a")` ⇒ `base/instances/a`).
  - `tests/logbuf.rs` (exactly one `#[test]`): `uart2_and_log_reach_the_debug_panel` (fake `send_uart(Uart2, b"x")`, `logbuf::record("INFO t: y")`, one frame ⇒ `debug().console(Uart2).text()=="x"`, `debug().log_lines()` ends with the line).
- [ ] **Step 2: Run** `cargo test -p evo-gui --test foundation --test logbuf` → FAIL.
- [ ] **Step 3: Implement** (stubs incl. `RomSetup::page() -> Option<Page>` returning `None` and `Capture::recording() -> Option<RecordKind>` returning `None`, with `Page`/`RecordKind` declared in the stubs as in B2/C1; registry; requests; loop changes; latch accessor; debug seam).
- [ ] **Step 4: Run** → PASS; gates; `cargo test -p evo-gui --features snapshots --test "snapshots*"` still passes unchanged.
- [ ] **Step 5: Commit** `feat(evo-gui): feature framework, app requests, debug seam`.

### Task 0.5b: Start-up without a ROM, instance arguments

**Files:**
- Modify: `src/app.rs` (`Choice`, `choose_backend`, `start_backend`; `DEMO_NOTICE` removed), `src/main.rs`, `tests/app.rs` (`choose_backend_rules` → `choose_backend_cases`; delete the `DEMO_NOTICE` test near the end of the file, ~lines 860–880)
- Test: `tests/app.rs`, `tests/foundation.rs`

**Interfaces — Produces:**
```rust
// app.rs
pub enum Choice { Demo, Bundle(PathBuf), FromPrefs }   // Demo has no fields now
pub fn choose_backend(bundle_arg: Option<PathBuf>, demo: bool, env: Option<OsString>) -> Choice; // no bundle ⇒ FromPrefs
pub struct StartUp { pub backend: Box<dyn EmuBackend>, pub notice: Option<String>, pub open_wizard: bool }
pub fn start_backend(prefs: &Prefs, platform: &PlatformRef, dir: &Path) -> StartUp;   // the FromPrefs decision
```
- `main.rs`: new args `--instance <ID>` and `--rename-from <ID>`; `instance::prepare(base, ..)` (error ⇒ stderr, exit 1; the lock lives until exit); for an instance, `NativeOptions.persistence_path = Some(dir.join("app.ron"))`. `Choice::Bundle` opens before the window as today (error ⇒ exit 1). `Choice::FromPrefs` calls `start_backend` inside the creator closure: `prefs.bundle` ⇒ `platform.open_backend(path, &dir.join("flash"))`; on error `Placeholder::no_rom()` plus notice `format!("Cannot open {}: {e}", path.display())` and `open_wizard`; no bundle ⇒ `Placeholder::no_rom()` and `open_wizard`.

- [ ] **Step 1: Write the failing tests:** `choose_backend_cases` in `tests/app.rs` (`(None,false,None)==FromPrefs`, `--demo`==`Demo`, arg/env as before); `start_backend_cases` in `tests/foundation.rs` (no bundle ⇒ name "No ROM", no notice, wizard; bundle with a `FakePlatform` factory returning `Err("bad")` ⇒ "No ROM", notice "Cannot open …: bad", wizard; bundle that opens ⇒ that backend, no wizard, and the factory saw `dir/flash`).
- [ ] **Step 2: Run** `cargo test -p evo-gui --test app --test foundation` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): start without a ROM; instance arguments`.

---

## Lane A — Core debug hooks (`crates/**`, `apps/evo-cli/src/run.rs`)

### Task A1: Data watchpoints and bus monitor in `MemoryMap`

**Files:**
- Create: `crates/evo-hw/src/watch.rs`, `crates/evo-hw/tests/watch.rs`
- Modify: `crates/evo-hw/src/memmap.rs`, `crates/evo-hw/src/lib.rs` (re-exports)

**Interfaces — Produces** (`evo_hw::watch`, re-exported at `evo_hw::`):
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] pub enum AccessDir { Read, Write, Exec }
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)] pub struct WatchKinds { pub read: bool, pub write: bool, pub exec: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] pub struct WatchRange { pub start: u32, pub len: u32, pub kinds: WatchKinds }
impl WatchRange { pub fn validate(&self) -> Result<(), WatchError>; pub fn overlaps(&self, addr: u32, width: u8) -> bool; }
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError { #[error("watchpoint length is 0")] Empty, #[error("watchpoint runs past 0xffffffff")] Overflow, #[error("watchpoint has no access kind")] NoKind }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct WatchHit { pub addr: u32, pub width: u8, pub dir: AccessDir, pub value: u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct BusRecord { pub addr: u32, pub width: u8, pub dir: AccessDir, pub value: u32, pub pc: u32, pub cycle: u64 }
pub const BUS_RING_CAP: usize = 4096;
impl MemoryMap {
    pub fn set_watchpoints(&mut self, ranges: Vec<WatchRange>) -> Result<(), WatchError>; // all validated first; Err ⇒ unchanged
    pub fn watchpoints(&self) -> Vec<WatchRange>;
    pub fn set_bus_monitor(&mut self, range: Option<(u32, u32)>);   // (start, len); None or len 0 = off, records kept
    pub fn bus_records(&self) -> Vec<BusRecord>;                     // oldest first
    pub fn clear_bus_records(&mut self);
    pub fn take_watch_hit(&mut self) -> Option<WatchHit>;
    pub fn watching(&self) -> bool;                                  // any read/write watchpoint or an active monitor
}
```
Model: one `#[serde(skip)] watch: Option<Box<WatchState>>` field; it is `None` whenever there are no read/write ranges and no active monitor (an empty `set_watchpoints` sets it back to `None`), so the hot path costs one `is_some()` branch per guest data access. Every `Bus` read*/write* that is not a fetch is observed — page-table fast path, devices and unmapped space alike — with the value read or written; host `peek`/`poke` and fetches are not. The first matching access per hit is kept until `take_watch_hit`; later matches are ignored until then. The monitor records accesses overlapping its range with `cycle = self.now`, `pc = self.pc_hint`, dropping the oldest beyond `BUS_RING_CAP`. Exec ranges are stored but not checked here (Task A2).

- [ ] **Step 1: Write the failing tests** in `tests/watch.rs` (a `MemoryMap` with 64 KiB RAM at 0x2000_0000 and a `Storage` device at 0x4000_0000, driven through `Bus` with `Access{kind: Read|Write, ..}`): `write_watch_hits_with_value` (`write32(0x2000_0010, 0xDEAD_BEEF)` over a W range ⇒ `WatchHit{0x2000_0010,4,Write,0xDEAD_BEEF}`, then `None`); `read_watch_sees_read_value`; `partial_overlap_hits` (range 0x2000_0012 len 1, `write32` at 0x2000_0010 hits); `fetch_is_not_a_read`; `device_and_unmapped_accesses_are_watched`; `peek_poke_are_not_watched`; `first_hit_is_kept`; `invalid_ranges_rejected_unchanged` (len 0 ⇒ `Empty`; `start 0xFFFF_FFFF len 2` ⇒ `Overflow`; no kinds ⇒ `NoKind`); `empty_set_turns_watching_off`; `monitor_records_and_is_bounded` (`BUS_RING_CAP + 3` writes ⇒ len `BUS_RING_CAP`, first record is write #4, `pc`/`cycle` from `pc_hint`/`now`); `monitor_off_keeps_records`; `clear_bus_records_empties`.
- [ ] **Step 2: Run** `cargo test -p evo-hw --test watch` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p evo-hw` → PASS. Bench: run `cargo test --release -p evo-core --test bench -- --nocapture` once on the commit before this task and once after, with `EVO_OS_IMAGE` set; record both numbers in the commit message; accept a slowdown of at most 3 % (else move the `is_some()` check out of the per-access path and re-measure). If the bench prints `skipping:` (no image), write "bench skipped: no EVO_OS_IMAGE" in the commit message instead. Gates.
- [ ] **Step 5: Commit** `feat(evo-hw): data watchpoints and a bus monitor on the memory map`.

### Task A2: Watchpoints in `Machine` (stop reasons, exec watch, batching)

**Files:**
- Modify: `crates/evo-core/src/machine.rs`, `crates/evo-core/src/lib.rs`, `apps/evo-cli/src/run.rs` (new arms), `crates/evo-core/src/gdb.rs` (only if a match there is exhaustive), `apps/evo-gui/src/backend/machine.rs` (`run_status`: `Watchpoint` ⇒ `RunStatus::Watchpoint` with `AccessDir` mapped, `Goal{pc}` ⇒ `RunStatus::Goal{pc}`)
- Create: `crates/evo-core/tests/debug.rs`

**Interfaces — Consumes:** A1. **Produces:**
```rust
pub enum StopReason { /* existing */, Watchpoint { addr: u32, dir: AccessDir, value: u32, pc: u32 }, Goal { pc: u32 } }
impl Machine {
    pub fn set_watchpoints(&mut self, ranges: Vec<WatchRange>) -> Result<(), WatchError>; // exec ranges kept on Machine (serde skip), all passed to the bus
    pub fn watchpoints(&self) -> Vec<WatchRange>;
    pub fn batched_runs(&self) -> u64;   // run_batch calls since power-on; statistics, not saved
}
// evo_core re-exports AccessDir, WatchKinds, WatchRange, WatchError, BusRecord, BUS_RING_CAP
```
Semantics: `run` takes the checked path (no batches) while `bus.watching()`, any exec range, or a goal (A3) is set. Exec: before stepping at `pc` inside an exec range, stop with `Watchpoint{addr: pc, dir: Exec, value: 0, pc}` and set `stopped_at = Some(pc)` (the next run steps over it, as for breakpoints). Data: after `step_cpu`, `bus.take_watch_hit()` ⇒ `Watchpoint{addr, dir, value, pc}` with `pc` the instruction that accessed (it has completed). `step()` returns a data hit too (exec ranges are ignored by `step`, like breakpoints). A pending hit is dropped at `load_state` and `power_cycle`.

Test programs: copy `Payload` (`new`, `word`, `code`, `vector`, `asm`, `bundle`, `boot`), `enum I`, `addr` and the constants `VTOR = 0x100B_0400`, `CODE = 0x200`, `SP_INIT`, `NOP`, `BX_LR`, `B_SELF` from `crates/evo-core/tests/machine.rs` into the top of `tests/debug.rs` (test files cannot share private helpers; do not edit `machine.rs`). `addr(off) = 0x100B_0400 + off`. **Store loop** (`STORE`, A2): literal `word(0x300, 0x3000_0200)`; `asm(0x200, &[I::Ld(0, 0x300) /*ldr r0,=0x30000200*/, I::H(0x2100) /*movs r1,#0*/, I::H(0x3101) /*0x204 adds r1,#1*/, I::H(0x6001) /*0x206 str r1,[r0]*/, I::H(0xE7FC) /*0x208 b 0x204*/])`. Store pc = `addr(0x206)` = 0x100B_0606; loop head = `addr(0x204)` = 0x100B_0604. (Secure SRAM 0x3000_0200 is used, not 0x2000_0100: the guest runs Secure, below the stack at `SP_INIT`.)

- [ ] **Step 1: Write the failing tests** in `crates/evo-core/tests/debug.rs` on `STORE`: `data_watch_stops_after_the_store` (W range `0x3000_0200` len 4 ⇒ `run(1000) == Watchpoint{addr:0x3000_0200, dir:Write, value:1, pc:0x100B_0606}`, `cpu.regs.pc == 0x100B_0608`); `continue_after_data_watch_moves_on` (next `run(1000)` ⇒ same stop with `value: 2`); `exec_watch_resumes_like_a_breakpoint` (X range `0x100B_0604` len 2: stop with `pc 0x100B_0604` and r1 = 0; next run ⇒ the same stop with r1 = 1); `step_reports_a_data_hit`; `batching_disabled_while_watching` (`batched_runs` unchanged across a run with a watchpoint, grows after `set_watchpoints(vec![])`; then `bus.set_bus_monitor(Some((0x3000_0000, 0x1000)))` alone ⇒ unchanged across a run; after `set_bus_monitor(None)` ⇒ grows again); `empty_watch_set_keeps_results_identical` (same cycles, r0 and UART output with and without `set_watchpoints(vec![])` after 1 M cycles).
- [ ] **Step 2: Run** `cargo test -p evo-core --test debug` → FAIL.
- [ ] **Step 3: Implement**; fix every exhaustive `StopReason` match the compiler reports (`evo-cli` prints `"watchpoint"` / `"goal"` with the pc).
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-core): read/write/exec watchpoints stop the run`.

### Task A3: Step over, step out, run to (goals)

**Files:**
- Modify: `crates/armv8m/src/exception.rs` (pub wrapper), `crates/evo-core/src/machine.rs`
- Create: `crates/evo-core/src/debug.rs` (goal types; Task A5 adds more here)
- Test: `crates/evo-core/tests/debug.rs`

**Interfaces — Produces:**
```rust
impl armv8m::Cpu { pub fn active_exceptions(&self) -> u32; }   // = active_count()
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunGoal { Return { pc: u32, sp: u32 }, ExceptionReturn { depth: u32 }, Address(u32) }
impl Machine {
    pub fn arm_step_over(&mut self) -> bool;   // BL / BLX(reg) at PC ⇒ Return{pc: pc+len, sp: current SP}, true; else one step(), false
    pub fn arm_step_out(&mut self);            // LR >= 0xFF00_0000 in handler mode ⇒ ExceptionReturn{depth: active_exceptions()}; else Return{pc: LR & !1, sp: current SP}
    pub fn arm_run_to(&mut self, addr: u32);   // Address(addr & !1)
    pub fn disarm(&mut self);
    pub fn goal(&self) -> Option<RunGoal>;     // serde skip; cleared by load_state and power_cycle
}
```
Reached (checked in `checked_step` before the breakpoint check): `Return` when `pc == goal.pc && sp >= goal.sp` (a deeper recursive return has a smaller SP); `ExceptionReturn` when `active_exceptions() < depth`; `Address` when `pc == addr`. Reaching it disarms, sets `stopped_at = Some(pc)` and returns `StopReason::Goal{pc}`. Breakpoints and watchpoints still stop first-come; the goal stays armed across them.

Programs (helpers as in A2; Thumb halfwords, payload offsets in comments):
- **CALL**: `asm(0x200, &[I::H(0xF000), I::H(0xF80E) /*bl 0x220*/, I::H(NOP) /*0x204*/, I::H(B_SELF) /*0x206*/])`, `asm(0x220, &[I::H(0x2264) /*movs r2,#100*/, I::H(0x3A01) /*0x222 subs r2,#1*/, I::Bne(0x222) /*0x224*/, I::H(BX_LR) /*0x226*/])`. After-bl pc = 0x100B_0604.
- **RECURSE**: `asm(0x200, &[I::H(0x2203) /*movs r2,#3*/, I::H(0xF000), I::H(0xF81D) /*0x202 bl 0x240*/, I::H(B_SELF) /*0x206*/])`, `asm(0x240, &[I::H(0xB500) /*push {lr}*/, I::H(0x3A01) /*0x242 subs r2,#1*/, I::H(0xD001) /*0x244 beq 0x24A*/, I::H(0xF7FF), I::H(0xFFFB) /*0x246 bl 0x240*/, I::H(0xBD00) /*0x24A pop {pc}*/])`. Every inner call returns to 0x100B_064A.
- **IRQ**: literals `word(0x300, 0xE000_E100)` (ISER0), `word(0x304, 0xE000_E200)` (ISPR0); `vector(16, 0x280)`; `asm(0x200, &[I::Ld(0, 0x300), I::H(0x2101) /*movs r1,#1*/, I::H(0x6001) /*str: enable IRQ0*/, I::Ld(0, 0x304), I::H(0x6001) /*0x208 str: pend IRQ0*/, I::H(B_SELF) /*0x20A*/])`, handler `asm(0x280, &[I::H(0x2305) /*movs r3,#5*/, I::H(0x3B01) /*0x282 subs r3,#1*/, I::Bne(0x282) /*0x284*/, I::H(BX_LR) /*0x286*/])`.

- [ ] **Step 1: Write the failing tests:** `step_over_a_call_stops_after_it` (CALL at reset: `arm_step_over()==true`, `run(1_000_000)==Goal{pc: 0x100B_0604}`, r2 == 0); `step_over_a_plain_instruction_steps_once` (CALL: run to `addr(0x220)` first with `arm_run_to`; `arm_step_over()==false`, PC 0x100B_0622); `step_over_recursion_waits_for_the_outer_return` (RECURSE: `arm_run_to(addr(0x246))` + run ⇒ `Goal{pc:0x100B_0646}` (depth 1, r2 = 2); note SP; `arm_step_over()==true`; run ⇒ `Goal{pc:0x100B_064A}` with SP equal to the noted SP and r2 == 0, i.e. not the deeper returns that reach 0x100B_064A first with a smaller SP); `step_out_returns_to_lr` (CALL: run to `addr(0x222)`, `arm_step_out()`, run ⇒ `Goal{pc:0x100B_0604}`); `step_out_of_a_handler_waits_for_exception_return` (IRQ: `arm_run_to(addr(0x282))` + run ⇒ goal in the handler with `active_exceptions()==1`; `arm_step_out()`; run ⇒ `Goal{pc: 0x100B_060A}`, `active_exceptions()==0`); `run_to_stops_before_the_address` (STORE: `arm_run_to(0x100B_0607)` ⇒ stops with pc 0x100B_0606 before the store: SRAM word still 0); `breakpoint_before_goal_keeps_goal_armed` (CALL: breakpoint at 0x100B_0622, step over the bl ⇒ `Breakpoint` first, `goal().is_some()`, next run ⇒ `Goal{pc:0x100B_0604}`); `goal_is_disarmed_by_disarm` (`disarm()` then `run` ends with `CycleLimit`); `unreached_goal_returns_at_budget_and_stays_armed` (STORE: `arm_run_to(addr(0x300))`, never executed ⇒ `run(1000) == CycleLimit`, cycles advanced by about 1000, `goal().is_some()`).
- [ ] **Step 2: Run** `cargo test -p evo-core --test debug` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-core): step over, step out and run-to goals`.

### Task A4: Device inspection

**Files:**
- Modify: `crates/evo-hw/src/device.rs`, `crates/evo-hw/src/memmap.rs`, `crates/evo-hw/src/dev/{timer,rtc,uart,lcdc,gpio,keypad,spif,mpc,sysctl}.rs`, `crates/evo-core/src/debug.rs`
- Create: `crates/evo-hw/tests/inspect.rs`
- Test: `crates/evo-core/tests/debug.rs`

**Interfaces — Produces:**
```rust
#[derive(Clone, Debug, PartialEq, Eq)] pub struct RegView { pub name: String, pub offset: u32, pub value: u32, pub note: Option<String> }
trait Device { fn inspect(&self, now: u64) -> Vec<RegView> { Vec::new() } }   // side-effect free (takes &self)
#[derive(Clone, Debug, PartialEq, Eq)] pub struct DeviceInspect { pub name: &'static str, pub base: u32, pub regs: Vec<RegView> }
impl MemoryMap { pub fn inspect_devices(&self, now: u64) -> Vec<DeviceInspect>; }  // add order, non-empty only
impl Machine { pub fn peripherals(&self) -> Vec<DeviceInspect>; } // inspect_devices(sched.now) + systick_s, systick_ns, nvic
```
`inspect` takes `now` (deviation from the spec's `inspect(&self)`): timer counts and RTC time derive from the cycle count. Rows: one per register the device's `read` decodes, named as the register constant in lower case (e.g. `lsr`, `data_in`, `int_stat`; per-channel timer registers `ch{n}_<name>`), in offset order, with the value a guest read would return at `now` but without its side effects (UART `rbr` shows the RX front without popping; status registers are not cleared). Notes only where a value needs decoding (e.g. LCDC `control`: `"enabled, power on"`). `systick_s`/`systick_ns`: base 0xE000_E010, rows `csr` 0x0, `rvr` 0x4, `cvr` 0x8 from `SysTick::peek`. `nvic`: base 0xE000_E100, one row per IRQ (< `NUM_IRQS`) with any of enabled/pending/active/level: name `irq{n}`, offset `n`, value `enabled | pending << 1 | active << 2 | level << 3`, note the set words joined by spaces (`"enabled pending"`).

- [ ] **Step 1: Write the failing tests.** `tests/inspect.rs`: `inspect_has_no_side_effects` — for each of `Timer, Rtc, Uart16550, Lcdc, Pwm, Gpio, Keypad, Spif, Mpc, Cru, Spctrl` in a `DevHarness`, after a few register writes: `save()` bytes equal before and after `inspect(now)`, and a following `read` of each row's offset returns `row.value`; spot values: `Uart16550` row `lsr` offset 0x14 value 0x60 at reset; `Pwm` rows `duty` 0x00, `period` 0x04; `Cru` row at 0xFC = `CRU_ID`; `Rtc` row `time` at 0x44 after a `now` of `3 * CPU_HZ` = 3; `Gpio` row `data_in` 0x04. `crates/evo-core/tests/debug.rs`: `peripherals_lists_devices_in_order` (names `["mpc_sram0","mpc_sram1","mpc_sram2","mpc_flash","cru","spctrl","timer","uart0","uart2","spif","gpio","lcdc","pwm","keypad","rtc","systick_s","systick_ns","nvic"]`; `gpio` `data_in` has bits 9 and 11 set at power-on); `nvic_rows_show_pending_irq` (after `set_irq_level(13, true)` a row `irq13` with bit 1 and bit 3 set).
- [ ] **Step 2: Run** `cargo test -p evo-hw --test inspect` and `cargo test -p evo-core --test debug` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-hw,evo-core): side-effect-free device and SCS inspection`.

### Task A5: RAM image, register writes, RTC set, power state

**Files:**
- Create: `crates/evo-core/src/ram.rs`
- Modify: `crates/evo-core/src/debug.rs`, `crates/evo-core/src/lib.rs`, `crates/evo-hw/src/dev/rtc.rs`
- Test: `crates/evo-core/tests/debug.rs`, `crates/evo-hw/tests/rtc.rs`

**Interfaces — Produces:**
```rust
// evo_core::ram
pub const RAM_MAGIC: &[u8; 6] = b"EVORAM";
pub const RAM_VERSION: u16 = 1;
pub const RAM_HEADER_LEN: usize = 12;      // magic, version u16 LE, size u32 LE
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RamError { #[error("not a RAM image")] BadMagic,
                    #[error("RAM image version {0} is not supported")] Version(u16),
                    #[error("RAM image holds {found} bytes, this machine has {expected}")] Size { expected: usize, found: usize } }
impl Machine { pub fn export_ram(&self) -> Vec<u8>;                         // header + 1.5 MiB SRAM (three banks in address order)
               pub fn import_ram(&mut self, bytes: &[u8]) -> Result<(), RamError>; } // checks magic, version, header size == SRAM_SIZE == body len; Err ⇒ unchanged
// evo_core::debug
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)] pub enum RegError { #[error("no register {0}")] Unknown(String) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct PowerState { pub hold: bool, pub deep_sleep: bool }
impl Machine {
    pub fn write_reg(&mut self, name: &str, v: u32) -> Result<(), RegError>;
    pub fn set_rtc(&mut self, seconds_of_day: u32);   // % 86_400, through MemoryMap::with_device
    pub fn rtc_seconds(&self) -> u32;
    pub fn power_state(&self) -> PowerState;          // gpio power_hold(), spctrl deep_sleep_armed()
}
impl Rtc { pub fn set_seconds_of_day(&mut self, s: u32, now: u64); }   // base = s % 86_400, set_at = now
```
`write_reg` names: `r0`–`r12`, `sp` (current SP via `set_sp`), `lr`, `pc` (`v & !1`, clears `stopped_at`, ends sleep with `wake()`), `xpsr` (`Xpsr::from_u32`), and `msp_s msp_ns psp_s psp_ns msplim_s msplim_ns psplim_s psplim_ns control_s control_ns primask_s primask_ns basepri_s basepri_ns faultmask_s faultmask_ns` (bools from `v & 1`, `basepri` as `u8`).

- [ ] **Step 1: Write the failing tests:** `ram_image_roundtrip` (poke SRAM, export, poke again, import ⇒ first bytes back; `export_ram().len() == 12 + 0x18_0000`); `import_ram_rejects_bad_magic`, `import_ram_rejects_version_2`, `import_ram_rejects_short_and_long` (each leaves SRAM unchanged); `write_reg_sets_core_and_banked` (`pc` 0x100B_0603 ⇒ 0x100B_0602; `control_ns`; `primask_s` 1 ⇒ true); `write_reg_unknown_is_err`; `set_rtc_reads_back` (`set_rtc(3600)` then 2 s of cycles ⇒ `rtc_seconds()==3602`; `set_rtc(86_401)` ⇒ 1); `power_state_at_power_on` (the synthetic image drives no GPIO: `PowerState{hold:false, deep_sleep:false}`); `crates/evo-hw/tests/rtc.rs::set_seconds_of_day_restarts_count`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-core): RAM images, register writes, RTC set and power state`.

---

## Lane B — ROM Setup Wizard, instances, reload/change ROM, reset everything

### Task B1: Instances on disk

**Files:**
- Modify: `apps/evo-gui/src/instance.rs` (replace the stub)
- Create: `tests/instance.rs`

**Interfaces — Produces:**
```rust
pub const LOCK_FILE: &str = "instance.lock";
pub fn validate_id(id: &str) -> Result<(), InstanceError>;  // 1–32 of [A-Za-z0-9_-]; Err(Invalid("IDs use 1–32 letters, digits, - or _"))
pub fn instance_dir(base: &Path, id: Option<&str>) -> PathBuf;   // None ⇒ base; Some ⇒ base/instances/id
pub fn next_free_id(base: &Path) -> String;                       // "instance-2", "instance-3", … first without a dir
pub fn prepare(base: &Path, id: Option<&str>, rename_from: Option<&str>) -> Result<InstanceLock, InstanceError>;
pub struct InstanceLock { pub dir: PathBuf, file: std::fs::File }   // holds an exclusive File::try_lock on dir/LOCK_FILE
```
`prepare`: validates ids; with `rename_from = Some(old)` (needs `id = Some(new)`, `new` must not exist): opens `instances/old/LOCK_FILE` and blocks on `File::lock` (the old process holds it until it exits), drops it, renames `instances/old` → `instances/new`. Then creates the dir, opens `LOCK_FILE` and `try_lock`s it: held ⇒ `InUse(id or "default")`.

- [ ] **Step 1: Write the failing tests:** `ids_are_validated` (`""`, 33 chars, `"a b"`, `"../x"` invalid; `"lab-1_A"` valid); `dirs_per_instance`; `next_free_id_skips_existing` (dirs `instance-2` ⇒ `instance-3`); `second_lock_is_refused` (two `prepare(base, Some("a"), None)` ⇒ second `InUse("a")`; after dropping the first, a third succeeds); `rename_moves_the_dir` (file `instances/old/flash/x` ends up at `instances/new/flash/x`); `rename_onto_existing_is_refused`; `rename_without_new_id_is_invalid`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test instance` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): instance directories, ids and locks`.

### Task B2: ROM Setup Wizard, Change ROM, Reload ROM

**Files:**
- Modify: `src/wizard/mod.rs` (replace the stub)
- Create: `src/wizard/validate.rs`, `tests/wizard.rs`, `tests/snapshots_wizard.rs`, goldens `wizard_welcome.png`, `wizard_choose_error.png`, `wizard_prepare_failed.png`

**Interfaces — Consumes:** `inspect_bundle`, `BundleInfo` (0.1); `Platform` (0.3); `Feature`, `AppRequest` (0.5a); `harness_with`, `snap_harness_with` (0.4). **Produces:**
```rust
// wizard/validate.rs
pub const EVO_PRODID: u32 = 23;                 // bundle_target_prodid of the TI-84 Evo OS bundles
pub const SUPPORTED_OS: &str = "7.1.0.4421";    // provisioning is tied to its BSP 7.1.0.4413
pub const BUNDLE_MAX_BYTES: u64 = 256 << 20;
pub struct Checked { pub info: BundleInfo, pub warning: Option<String> }
pub fn check_bundle(path: &Path) -> Result<Checked, String>;   // reads metadata size first; error texts below
// wizard/mod.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Page { Welcome, Choose, Prepare, Done }
pub struct RomSetup { .. }
impl RomSetup { pub fn new(opts: &AppOptions) -> Self; pub fn page(&self) -> Option<Page>; } // None = closed; opens at Welcome when opts.open_wizard
```
Texts: modal title "ROM Setup Wizard". Welcome: heading "Welcome", body "This emulator needs a TI-84 Evo OS bundle (.84b2). It is not included: use your own copy.", "Next". Choose: heading "Choose an OS bundle", "Browse…" (`pick_open("Choose an OS bundle", [FileFilter{name:"TI-84 Evo OS bundle", extensions:&["84b2"]}])`), drop target (`raw.dropped_files`; more than one ⇒ "Drop one file at a time"), info line `format!("OS {} · {}", build_version, target_device)`, "Back", "Next" (enabled only after a successful check). Errors (inline, red): `format!("Cannot read {name}: {e}")`, `"File is too large to be an OS bundle"`, `format!("Not an OS bundle: {e}")`, `format!("This is not a TI-84 Evo OS bundle (product id {p})")`. Warning (inline, Next still enabled) when the version differs: `format!("OS {v}: provisioning is tied to OS 7.1.0.4421 and may fail with this version")`. Next ⇒ `platform.open_backend(path, &data_dir.join("flash"))`: Ok ⇒ requests `ReplaceBackend{backend, rom: Some(path), persist_old: true}` and `ApplyPrefs(prefs with bundle = Some(path))`, page Prepare; Err ⇒ inline `format!("Cannot open {name}: {e}")`. Prepare: heading "Prepare calculator", `PROVISION_NOTICE` + spinner + `format!("{} s", elapsed)` (whole seconds since Prepare opened) while `cx.provisioning` — the provisioning worker reports no progress fraction, so the spec's "progress" is an indeterminate spinner with elapsed time (see Decisions); when it ends with `needs_provision()` still true ⇒ "Preparing failed." and "Retry" (`AppRequest::Provision`); else page Done. Done: heading "Done", "The calculator is ready.", "Finish" closes. "Cancel" on every page closes. `blocks_input()` while open. Actions: `ChangeRom` opens at Choose; `ReloadRom` re-opens `cx.rom` (else toast "No ROM to reload") with `persist_old: true`, toast "ROM reloaded" or `format!("Reload failed: {e}")`.

- [ ] **Step 1: Write the failing tests** in `tests/wizard.rs` (harness from `common::harness_with(fake, opts)` with `opts = common::test_options(dir, platform)` and `opts.open_wizard` as needed; bundles built with `evo_image::testutil::zip_bytes(&[("METADATA", meta), ("Synthetic_Package.84pk2", &package(&[(0, &Img::new(0x400, body))])), ("Archive/", b"")], Default::default())` — the method argument is `Default::default()` because evo-gui has no `zip` dependency — and a METADATA with all four keys `Metadata::parse` requires: `bundle_build_version:7.1.0.4421`, `bundle_target_device:84US`, `bundle_target_prodid:23`, `bundle_target_type:RESTORE`): `check_accepts_evo_bundle`; `check_warns_on_other_version` (`9.8.7.6543` ⇒ `warning` contains "7.1.0.4421"); `check_rejects_other_product` (prodid 42); `check_rejects_garbage_and_directories`; `check_rejects_oversized_unread` (a sparse file of `BUNDLE_MAX_BYTES + 1` made with `File::set_len`); `opens_at_start_without_rom` (`open_wizard: true` ⇒ "Welcome" shown; keys typed while open do not reach the probe); `browse_validate_next_replaces_backend` (queue the path ⇒ Next ⇒ `backend().name()` is the file name, `prefs().bundle == Some(path)`, page `Done` because the fake does not need provisioning); `prepare_waits_for_provisioning_and_retries` (factory returns `needing_provision().failing_provision()` ⇒ "Preparing failed.", Retry ⇒ `provisions()==2`); `drop_two_files_is_refused` (`h.input_mut().dropped_files = vec![egui::DroppedFile { path: Some(a), ..Default::default() }, egui::DroppedFile { path: Some(b), ..Default::default() }]`, one step ⇒ "Drop one file at a time"); `cancel_keeps_backend`; `change_rom_opens_choose`; `reload_rom_reopens_same_path` and `reload_without_rom_toasts`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test wizard` → FAIL.
- [ ] **Step 3: Implement**; add `tests/snapshots_wizard.rs` (`snapshot_wizard_welcome` with `opts.open_wizard = true`, `snapshot_wizard_choose_error` after dropping a garbage file, `snapshot_wizard_prepare_failed` with a factory returning `needing_provision().failing_provision()`) using `common::snap_harness_with`; generate with `UPDATE_SNAPSHOTS=1`, open the PNGs (goldens `wizard_welcome.png`, `wizard_choose_error.png`, `wizard_prepare_failed.png`).
- [ ] **Step 4: Run** `cargo test -p evo-gui --test wizard` and `cargo test -p evo-gui --features snapshots --test snapshots_wizard` → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): ROM Setup Wizard, change and reload ROM`.

### Task B3: Instances window and Reset everything

**Files:**
- Modify: `src/wizard/mod.rs`
- Create: `src/wizard/instances.rs`, `src/wizard/reset.rs`, goldens `wizard_instances.png`, `wizard_reset_everything.png`
- Test: `tests/wizard.rs`, `tests/snapshots_wizard.rs`

**Interfaces — Consumes:** B1, B2. **Produces:** handling of `Action::Instances` and `Action::ResetEverything` inside `RomSetup`.

Instances window "Instances": `format!("Instance: {}", id.unwrap_or("default"))`; "New instance" ⇒ `platform.spawn_instance(["--instance", next_free_id(base)])`, toast `format!("Started instance {id}")` or `format!("Could not start an instance: {e}")`; "New ID" text field + "Change ID" (disabled for the default instance, tooltip "The default instance cannot be renamed") ⇒ `validate_id` (error inline), then requests `Persist`, spawns `["--instance", new, "--rename-from", old]`, then `AppRequest::Close`. `base` = `data_dir` for the default instance, else `data_dir/../..`.

Reset everything: modal "Reset everything?" with "This deletes the flash, state slots, screenshots, recordings, macros and settings of this instance. The emulator then starts over with the ROM Setup Wizard." and buttons "Export flash first…" (`pick_save("Export flash image", "evo.evoflash", [FileFilter{"Flash image", &["evoflash"]}])` ⇒ `backend.export_flash`; toast on result; modal stays), "Reset everything", "Cancel". Confirm: delete every entry of `data_dir` except `LOCK_FILE`, `app.ron` and `instances` (for the default instance `data_dir == base`, so `instances/` holds the other instances, some possibly running with locks inside; it is always kept) (errors ⇒ toast, continue with the rest); requests `ReplaceBackend{Placeholder::no_rom(), rom: None, persist_old: false}`, `ApplyPrefs(Prefs::default())`, `ResetLayout`; wizard opens at Welcome; toast "Everything was reset".

- [ ] **Step 1: Write the failing tests:** `new_instance_spawns_next_id` (`spawned()==[["--instance","instance-2"]]`); `change_id_validates_then_spawns_and_closes` (instance `Some("lab")`; bad id `"a b"` ⇒ inline error, nothing spawned; `"lab2"` ⇒ `spawned()==[["--instance","lab2","--rename-from","lab"]]`, probe `persists()==1`, `close_requested()`); `change_id_disabled_for_default`; `reset_everything_cancel_changes_nothing`; `reset_everything_deletes_and_restarts` (default instance: files under `flash/`, `states/` removed, `instance.lock`, `app.ron` and `instances/other/flash/x` kept, `backend().name()=="No ROM"`, old probe `persists()==0`, wizard "Welcome" shown, `prefs()==Prefs::default()` but keymap default); `export_flash_first_writes_file`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement**; add goldens to `tests/snapshots_wizard.rs`: `snapshot_wizard_instances` (instance `Some("lab")`, Instances window open) and `snapshot_wizard_reset_everything` (modal open); generate with `UPDATE_SNAPSHOTS=1` and open both PNGs.
- [ ] **Step 4: Run** → PASS; `cargo test -p evo-gui --features snapshots --test snapshots_wizard` → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): instances window and reset everything`.

---

## Lane C — Capture

### Task C1: Frame sampler and Copy screen

**Files:**
- Modify: `src/capture/mod.rs` (replace the stub)
- Create: `src/capture/sampler.rs`, `tests/capture.rs`

**Interfaces — Produces:**
```rust
pub const LCD_FRAME_CYCLES: u64 = CLOCK_HZ / 60;   // 2_600_000
pub struct FrameSampler { .. }
impl FrameSampler { pub fn new(now: u64) -> Self;
                    pub fn ticks(&mut self, now: u64) -> u32; } // whole 1/60 s ticks since the last call (saturating), rest kept
pub struct Capture { .. }
impl Capture { pub fn new(opts: &AppOptions) -> Self; pub fn recording(&self) -> Option<RecordKind>; }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum RecordKind { Apng, Mp4 }
```
`CopyScreen`: `platform.copy_image(frame)` ⇒ toast "Screen copied" or `format!("Copy failed: {e}")`; no valid frame ⇒ "Copy failed: no display frame to capture".

- [ ] **Step 1: Write the failing tests:** `sampler_counts_whole_ticks` (`new(0)`, `ticks(2_599_999)==0`, `ticks(2_600_000)==1`, `ticks(13_000_000)==4`); `sampler_tolerates_time_going_back` (after a reset to 0 ⇒ `ticks` 0, then counts from 0); `copy_screen_copies` (`FakePlatform::copied()==1`, toast); `copy_screen_failure_toasts` (`fail_copy("busy")` ⇒ "Copy failed: busy"); `copy_without_display_toasts`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test capture` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): copy screen to clipboard; LCD frame sampler`.

### Task C2: Animated PNG recording

**Files:**
- Create: `src/capture/apng.rs`
- Modify: `src/capture/mod.rs`
- Test: `tests/capture.rs`

**Interfaces — Produces:**
```rust
pub const APNG_MAX_FRAMES: usize = 1800;   // 30 s of distinct 60 Hz frames
#[derive(Debug, thiserror::Error)] pub enum CaptureError { #[error("{0}")] Io(String), #[error("{0}")] Encode(String), #[error("recording is full")] Full }
pub struct ApngRecorder { .. }   // each distinct frame kept as a compressed single-frame PNG (Vec<u8>) + tick count
impl ApngRecorder { pub fn new(path: PathBuf) -> Self;            // = with_cap(path, APNG_MAX_FRAMES)
                    pub fn with_cap(path: PathBuf, cap: usize) -> Self;
                    pub fn push(&mut self, frame: &Frame, ticks: u32) -> Result<(), CaptureError>; // identical to the last ⇒ its ticks grow; Full past APNG_MAX_FRAMES
                    pub fn finish(self) -> Result<Option<PathBuf>, CaptureError>; }   // None (no file) when no frame was pushed
```
Memory: `push` encodes each new distinct frame at once into a standalone PNG in memory (`png::Encoder`, RGBA 8-bit, default compression) and keeps only those bytes plus the last frame's raw RGBA for the identical-frame check; LCD frames compress to tens of KiB, so the cap bounds memory at roughly `APNG_MAX_FRAMES` × 50 KiB (≈ 90 MiB worst case), not 1.1 GB of raw RGBA. `finish` decodes each kept PNG back (`png::Decoder`) and writes it into the APNG. Encoding with `png` 0.18: RGBA 8-bit, `set_animated(n, 0)`, per frame `set_frame_delay(ticks, 60)` (ticks above `u16::MAX` split into repeated frames). Path: `{data_dir}/recordings/evo-{unix_ms}.png` (dir created at start; failure ⇒ toast `format!("Recording failed: {e}")`, nothing starts). `RecordApng` toggles: start ⇒ toast "Recording animated PNG…"; each `frame()` with a valid framebuffer pushes it with `sampler.ticks(cycles)` when > 0 (paused / no display ⇒ nothing pushed, the sampler still advances); stop ⇒ toast `format!("Recording saved to {}", p.display())`, or "Nothing was recorded" when `finish` returns `None` (no file). `Full` or a write error ⇒ finish what is there, toast `format!("Recording stopped: {e}")`. Starting one kind while the other runs ⇒ toast "Stop the current recording first". `active(RecordApng)` while recording; `on_exit` finishes.

- [ ] **Step 1: Write the failing tests:** `apng_has_one_frame_per_distinct_screen` (push A×3 ticks, A×2, B×1 ⇒ decoding with `png::Decoder` gives `animation_control.num_frames==2` and delays 5/60, 1/60); `no_frames_writes_no_file`; `apng_full_stops_and_keeps_frames` (`with_cap(path, 3)`, 4 distinct frames ⇒ the 4th push is `Full`; finish writes a file with `num_frames==3`); `apng_write_error_stops_cleanly` (unit: `with_cap(dir/"a"/"x.png", 10)` where `dir/a` is a file, push 2 frames, `finish` ⇒ `Err(Io)`, no panic, no partial file; via the action: record a few frames, then delete `recordings/` and create a file named `recordings` before stop ⇒ toast starting "Recording stopped: ", `recording()==None`). Note: a write error cannot leave a valid file for either format (the APNG is written whole at `finish` because its frame count is in the header; MP4's `moov` box is written at `finish`), so a failed finish removes the partial file and toasts. Review Focus 4's "frames so far form a valid file" holds where `finish` itself succeeds: cap reached (`Full`), stop, and app exit; `record_apng_via_action` (running harness, `open_action(RecordApng)`, run 30 steps so the fake's cycle bar moves, `open_action(RecordApng)` ⇒ a valid file under `recordings/` and the toast); `paused_or_no_display_pushes_nothing` (paused harness: start, 30 steps, stop ⇒ no file under `recordings/`, toast "Nothing was recorded"; again with a running `FakeBackend::without_display()` ⇒ same); `recording_into_unwritable_dir_stops_with_toast` (`recordings` pre-created as a file ⇒ "Recording failed: …", `recording()==None`); `on_exit_finishes_recording` (start, run 30 steps, `eframe::App::on_exit(h.state_mut(), None)` ⇒ a valid file exists, `recording()==None`).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): record the screen as an animated PNG`.

### Task C3: MP4 recording (H.264)

**Files:**
- Create: `src/capture/mp4.rs`
- Modify: `src/capture/mod.rs`
- Test: `tests/capture.rs`

**Interfaces — Produces:**
```rust
pub struct Mp4Recorder { .. }
impl Mp4Recorder { pub fn new(path: PathBuf) -> Self;
                   pub fn push(&mut self, frame: &Frame, ticks: u32) -> Result<(), CaptureError>;
                   pub fn finish(self) -> Result<Option<PathBuf>, CaptureError>; }
pub fn scale2x_rgb(frame: &Frame) -> (u32, u32, Vec<u8>);   // nearest ×2, RGB888, width/height rounded down to even
```
Encoder: `Encoder::with_api_config(OpenH264API::from_source(), EncoderConfig::new().skip_frames(false).bitrate(BitRate::from_bps(2_000_000)).max_frame_rate(FrameRate::from_hz(60.0)))` (the `Encoder::new()` defaults skip frames and target 120 kbps, which breaks one-sample-per-distinct-frame and looks poor at 640×480), on `YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (w, h)))`; the first frame is IDR (`force_intra_frame`). An encode that yields no NAL units (after dropping SPS/PPS/AUD) adds no sample: its ticks extend the pending sample's duration. The `mp4` writer (`Mp4Writer<BufWriter<File>>`) starts lazily at the first frame with `Mp4Config { major_brand: "isom".parse()?, minor_version: 512, compatible_brands: ["isom", "iso2", "avc1", "mp41"] parsed to FourCC, timescale: 1000 }` and one track `TrackConfig { track_type: TrackType::Video, timescale: 60, language: "und".into(), media_conf: MediaConfig::AvcConfig(AvcConfig { width, height, seq_param_set, pic_param_set }) }` (SPS/PPS from the first frame's NAL units); `finish` calls `write_end`; samples are AVCC (4-byte length-prefixed NAL units, SPS/PPS/AUD dropped); an identical frame extends the pending sample's duration instead of being encoded (one pending sample, written when the next distinct frame comes or at finish). Path `{data_dir}/recordings/evo-{unix_ms}.mp4`; `RecordVideo` behaves like `RecordApng` with toast "Recording MP4…". **GIF branch** (only if Task 0.3 took the fallback, in which case Task 0.4 labels `RecordVideo` "Record GIF"): `GifRecorder` in `src/capture/gif.rs` with the same three methods, `gif` encoder at the native size, delay in 1/100 s (`ticks * 100 / 60`, at least 1), file `evo-{unix_ms}.gif`, toast "Recording GIF…"; the tests below then check the GIF with `gif::Decoder` (frame count, size 320×240).

- [ ] **Step 1: Write the failing tests:** `scale2x_doubles_pixels` (2×1 frame ⇒ 4×2 RGB with each pixel repeated); `mp4_is_readable` (push 3 distinct fake frames ⇒ `mp4::Mp4Reader::read_header` sees one track, `media_type` `H264`, width 640, height 480, `sample_count()==3`); `mp4_identical_frames_merge` (A×2, A×3 ⇒ 1 sample of duration 5); `mp4_no_frames_writes_no_file`; `record_video_via_action`; `only_one_recording_at_a_time`; `mp4_into_unwritable_dir_stops_with_toast` (`recordings` pre-created as a file ⇒ "Recording failed: …", `recording()==None`); `mp4_on_exit_finishes_recording` (start `RecordVideo`, 30 running steps, `eframe::App::on_exit(h.state_mut(), None)` ⇒ a file `Mp4Reader` reads with `sample_count() >= 1`). (GIF branch: the same tests on `.gif`.)
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** (in release too: `cargo test --release -p evo-gui --test capture`) → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): record the screen as H.264 MP4`.

---

## Lane D — Debug dock panels (against `FakeBackend`)

Common rules for every tab: the tab body draws only what is visible (no backend calls for hidden tabs); edits are enabled only while paused (`show(ui, backend, paused)`); text fields are monospace and take keyboard input (the app's `egui_wants_keyboard_input` keeps keys from the calculator); every backend error becomes a `DebugOutput.notes` entry. Tab order and labels (`DebugTab::ALL`): "Control", "Registers", "Disassembly", "Memory", "Visualizer", "Breakpoints", "Watchpoints", "Peripherals", "Bus monitor", "Stacks", "Console", "Unmapped"; default "Registers". The run row above the tabs: "Continue"/"Pause", "Step", "Step over", "Step out" (the last three enabled while paused).

### Task D1: Tab registry, run row, Control tab

**Files:**
- Modify: `src/debug/mod.rs`, `tests/app.rs` (debug tests: tab labels, "UART" → "Console"), `tests/debug.rs`
- Create: `src/debug/control.rs`, `tests/debug_dock.rs`, `tests/snapshots_debug.rs`; regenerate `tests/snapshots/debug_panels.png`

**Interfaces — Produces:**
```rust
pub enum DebugTab { Control, Registers, Disassembly, Memory, Visualizer, Breakpoints, Watchpoints, Peripherals, BusMonitor, Stacks, Console, Unmapped }
impl DebugTab { pub const ALL: [DebugTab; 12]; pub fn label(self) -> &'static str; }
pub fn cycle_text(cycles: u64, mark: u64) -> (String, String);   // ("Cycles {cycles}", "Since mark {cycles - mark}") with saturating sub
```
Control tab: "Run to" field (hint `0x10000000`, `parse_addr`) + "Run" button ⇒ `goal = Some(Goal::RunTo(a))`; invalid ⇒ inline "Not a hex address"; cycle labels from `cycle_text` and a "Mark" button (mark = cycles). "Step over"/"Step out" set `goal`. While a goal is armed and running, the run row shows "Pause" (it is a normal run); Pause goes through `toggle_pause`, which the app turns into `cancel_goal` (0.5a).

- [ ] **Step 1: Write the failing tests** in `tests/debug_dock.rs` (wide paused harness with the dock open): `tabs_in_order` (all 12 labels); `step_over_is_a_goal` (`probe.goals()==[StepOver]`, still paused); `step_out_runs_until_goal` (after "Step out": not paused, a few steps later paused with "Stopped at 0x10000240"); `run_to_field` (type `0x10000300`, click "Run" ⇒ `goals()==[RunTo(0x1000_0300)]`); `run_to_rejects_garbage`; `endless_step_out_pause_then_continue` (`FakeBackend::new().goal_never_reached()`: click "Step out", 20 steps ⇒ not paused, header not a stop, cycles advance each step; click "Pause" ⇒ paused, `probe.goal_cancels()==1`, `goal_armed()==false`; click "Continue", 5 steps ⇒ running, no "Stopped at" text); `mark_resets_since_mark` (`cycle_text(500, 200)==("Cycles 500","Since mark 300")`, and via the UI). Update `tests/debug.rs::constants` (`DebugTab::default()` stays `Registers`) and the label list in `tests/app.rs::debug_toggle_shows_registers`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test debug_dock --test debug --test app` → FAIL.
- [ ] **Step 3: Implement**; start `tests/snapshots_debug.rs` with `snapshot_debug_control`; regenerate `debug_panels.png` (`UPDATE_SNAPSHOTS=1 … --test snapshots`) and check it.
- [ ] **Step 4: Run** the tests and `--features snapshots --test "snapshots*"` → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): debug dock tabs, run row with step over/out, Control tab`.

### Task D2: Editable registers

**Files:** Modify `src/debug/regs.rs`; Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

**Interfaces — Produces:** `pub fn parse_value(s: &str) -> Option<u32>` in `debug/mod.rs` (= `parse_addr` rules; also accepts a leading `#` for decimal: `#10` ⇒ 10).

Clicking a value (paused) turns it into a text field prefilled with `0x{v:08x}`; Enter applies (`set_core_reg(i, v)` for `r0..pc` and `xpsr` (index 16), `set_banked_reg(name, v)` for banked rows); Escape cancels; an unparsable value shows "Not a value" and keeps the field. Running: plain labels.

- [ ] **Step 1: Failing tests:** `parse_value_cases`; `edit_sp_applies` (click "0x20008000", select all, type `0x20001000`, Enter ⇒ "0x20001000" shown); `edit_banked_applies` (`msp_ns`); `unparsable_value_keeps_field` (`zz` + Enter ⇒ "Not a value", field still open, register unchanged); `escape_cancels_edit`; `no_edit_while_running`. Golden `snapshot_debug_registers_editing`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): editable registers`.

### Task D3: Memory views, editing, search

**Files:** Modify `src/debug/memory.rs`; Create `src/debug/search.rs`; Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

**Interfaces — Produces:**
```rust
pub const MAX_VIEWS: usize = 4;
pub const SEARCH_CHUNK: usize = 0x4000;
pub fn parse_pattern(s: &str) -> Option<Vec<u8>>;   // hex bytes "de ad be ef" / "deadbeef", or "\"text\"" (ASCII); empty ⇒ None
pub const SEARCH_CHUNKS_PER_FRAME: usize = 256;     // 256 × SEARCH_CHUNK = 4 MiB per frame
pub struct Search { .. }
impl Search { pub fn new(regions: Vec<MemRegion>, from: u32, pattern: Vec<u8>) -> Self;
              pub fn step(&mut self, backend: &dyn EmuBackend, max_chunks: usize) -> Option<Option<u32>>; } // None: not done yet; Some(hit or None)
pub fn find(backend: &dyn EmuBackend, regions: &[MemRegion], from: u32, pattern: &[u8]) -> Option<u32>; // Search::step to completion; first match at >= from, in region order, then wraps once to the first region
```
The real machine's regions total about 35 MiB, so "Next" runs one `step(.., SEARCH_CHUNKS_PER_FRAME)` per frame and shows "Searching…" until it is done (no multi-second UI freeze); editing the pattern or address cancels it.
The tab shows 1..=`MAX_VIEWS` views ("Add view", per view "Close" when more than one), each: address field, format "Bytes"/"Words" (u32 LE), 16 rows. Paused: clicking a byte (Bytes view) or a word opens a 2- or 8-digit field; Enter ⇒ `poke(addr, bytes)`; errors ⇒ notes. Search row: "Find" field + "Next": hit ⇒ first view jumps to `hit & !0xF` and the byte is highlighted, next "Next" searches from `hit + 1`; none ⇒ inline "Not found". `find` reads `SEARCH_CHUNK`-byte chunks with `pattern.len() - 1` bytes of overlap.

- [ ] **Step 1: Failing tests:** `parse_pattern_cases` (`"de ad"`⇒`[0xDE,0xAD]`, `"dead"` same, `"\"ab\""` ⇒ `b"ab"`, `"xyz"`/`""` ⇒ None, odd hex digits ⇒ None); `find_in_fake_memory` (pattern `[5,6,7]` from 0x2000_0000 ⇒ 0x2000_0005; from 0x2000_0006 ⇒ 0x2000_0100; wraps from 0x2000_FFF0 back to 0x2000_0005); `find_across_chunk_boundary` (poke `[0xAB,0xCD,0xEF]` at 0x2000_3FFF, across the first chunk boundary ⇒ found at 0x2000_3FFF); `search_steps_are_bounded` (`Search::new` over the fake region, pattern absent, from 0x2000_0000, `step(.., 1)` ⇒ `None` on the first call (one 0x4000-byte chunk of the 0x1_0000-byte region) and `Some(None)` within 5 calls); `edit_byte_pokes` (`pokes()==[(0x2000_0003, vec![0xAA])]`, view shows `aa`); `edit_outside_notes_error`; `add_and_close_views` (max 4). Golden `snapshot_debug_memory_two_views`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): memory views with editing and search`.

### Task D4: Memory visualizer

**Files:** Create `src/debug/visualizer.rs`; Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

**Interfaces — Produces:**
```rust
pub enum PixelFormat { Rgb565, Rgb888, Gray8, Mono1 }   // labels "RGB565", "RGB888", "Gray 8-bit", "1-bit"
impl PixelFormat { pub fn bits(self) -> u32; }          // 16, 24, 8, 1
pub const VIS_MAX: u32 = 1024;                          // max width and height
pub fn render(bytes: &[Option<u8>], fmt: PixelFormat, width: u32, height: u32) -> Vec<u8>; // RGBA; unreadable ⇒ magenta (0xFF,0x00,0xFF,0xFF); Mono1 MSB first, 1 = black
```
Controls: address (default `0x20048000`), format (default RGB565, LE, R in bits 15:11), width (default 320), height (default 240), each clamped to 1..=`VIS_MAX`; one `peek` of `width*height*bits/8` bytes per frame while visible; texture shown at the panel width keeping aspect, nearest filter.

- [ ] **Step 1: Failing tests:** `rgb565_white_and_red` (`[0xFF,0xFF]` ⇒ white, `[0x00,0xF8]` ⇒ (0xFF,0,0)); `rgb888_and_gray`; `mono1_bits`; `unreadable_is_magenta`; `short_input_pads_magenta`; `sizes_are_clamped` (width field `5000` ⇒ 1024). Golden `snapshot_debug_visualizer` (FAKE_MEM at 0x20000000, Gray 8-bit, 64×64).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): memory visualizer`.

### Task D5: Breakpoints and Watchpoints tabs

**Files:** Create `src/debug/breakpoints.rs`, `src/debug/watchpoints.rs`; Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

**Interfaces — Produces:** `pub fn watch_text(w: &Watchpoint) -> String` = `format!("0x{:08x}..0x{:08x} {}{}{}", start, last, r, w, x)` with `R`/`W`/`X` or `-`.

Breakpoints: rows `format!("0x{a:08x}")` + "Remove" each; "Address" field + "Add" (`toggle_breakpoint` only when absent); "Remove all" (`set_breakpoints(vec![])`). Watchpoints: rows `watch_text` + "Remove"; add row: "Start" field, "Length" field (`parse_value`), checkboxes "R", "W", "X", "Add" ⇒ `set_watchpoints(old + new)`; invalid ⇒ inline "Invalid watchpoint"; "Remove all".

- [ ] **Step 1: Failing tests:** `watch_text_format` (`{0x2000_0100, 4, W}` ⇒ "0x20000100..0x20000103 -W-"); `add_and_remove_breakpoints`; `remove_all_breakpoints`; `add_watchpoint_then_run_stops` (add W at `0x20000100` len 4, Continue ⇒ header "Watchpoint (write) at 0x20000100, pc 0x10000200", paused, dock open); `invalid_watchpoint_is_refused` (length 0). Goldens `snapshot_debug_breakpoints`, `snapshot_debug_watchpoints`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): breakpoint and watchpoint lists`.

### Task D6: Peripherals, Bus monitor, Stacks

**Files:** Create `src/debug/{peripherals,busmon,stacks}.rs`; Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

**Interfaces — Produces:**
```rust
pub const BUS_SHOWN: usize = 1000;   // newest records shown
pub fn bus_line(r: &BusRecord) -> String;   // format!("{:>12} {:08x} {} {:08x} {} {:0w$x}", cycle, pc, R|W|X, addr, width, value, w = 2*width)
pub fn stack_rows(backend: &dyn EmuBackend, sp: u32, words: usize) -> Vec<String>; // format!("{addr:08x}  {word:08x}") or "{addr:08x}  ????????"
pub const STACK_WORDS: usize = 16;
```
Peripherals: one collapsing header per `DeviceView`, `format!("{} @ 0x{:08x}", name, base)`, grid rows `name`, `format!("+0x{off:03x}")`, `format!("0x{v:08x}")`, note. Bus monitor: "Start" and "Length" fields, "Monitor" (⇒ `set_bus_monitor(Some(..))`), "Stop" (`None`), "Clear"; lines `bus_line` of the last `BUS_SHOWN`. Stacks: sections "sp" (`core[13]`) and each banked `msp_*`/`psp_*` present in `CpuView::banked`, each `stack_rows(.., STACK_WORDS)`.

- [ ] **Step 1: Failing tests:** `bus_line_format` (record `{0x2000_0100,4,Write,0x2A,0x1000_0200,7}` ⇒ "           7 10000200 W 20000100 4 0000002a"); `peripherals_tab_lists_devices` ("timer @ 0x40101000", "+0x030", "0x00000001", "enabled"); `bus_monitor_tab_records_runs` (Monitor `0x20000000` len `0x10000`, Continue a few frames, Pause ⇒ a line ending "W 20000100 4 0000002a"); `stack_rows_read_words` (`stack_rows(fake, 0x2000_8000, 1)==["20008000  8d8c8b8a"]`, `0x3000_0800` ⇒ `["30000800  ????????"]`); `peripherals_not_fetched_when_hidden` (`probe.inspect_calls()` stays 0 over 10 frames on the Registers tab, grows on the Peripherals tab). Goldens `snapshot_debug_peripherals`, `snapshot_debug_bus_monitor`, `snapshot_debug_stacks`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): peripherals, bus monitor and stacks tabs`.

### Task D7: Console, run to cursor

**Files:** Create `src/debug/console.rs`; Modify `src/debug/disasm.rs`, `src/debug/uart.rs` (keep `UartConsole`; the old UART tab body moves into the console); Test `tests/debug_dock.rs`, `tests/snapshots_debug.rs`

Console: source selector "UART0" / "UART2" / "Log" (`console(port)` text or `log_lines()` joined by `\n`), "Clear" (clears the shown source), and for UART sources an input line + "Send" (or Enter) ⇒ `send_uart(port, line + "\r")`, field cleared. Disassembly: each line's `context_menu` has "Run to here" ⇒ `goal = Some(RunTo(addr))` (paused only).

- [ ] **Step 1: Failing tests:** `console_shows_uart0_by_default` (fake UART line "fake: 100 Mcycles" after running); `console_sends_line_with_cr` (`uart_sent()==[(Uart0, b"hi\r".to_vec())]` and "hi" appears via the echo); `console_uart2_and_log_sources`; `run_to_here_from_disassembly` (secondary click on the second line, "Run to here" ⇒ `goals()==[RunTo(0x1000_0202)]`, then "Stopped at 0x10000202"). Golden `snapshot_debug_console`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates, `--features snapshots --test "snapshots*"`.
- [ ] **Step 5: Commit** `feat(evo-gui): console with UART input and log, run to cursor`.

---

## Lane E — Tools

### Task E1: Key scripts in the GUI

**Files:** Modify `src/tools/mod.rs` (replace the stub: `Tools` feature, owns E2–E4 windows); Create `src/tools/script.rs`, `tests/tools.rs`

**Interfaces — Produces:**
```rust
pub const TAP_HOLD_MS: u64 = 50;   // = the core's key-script constants (key scripts must mean the same in CLI and GUI)
pub const TAP_GAP_MS: u64 = 50;
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct ScriptEvent { pub at_ms: u64, pub key: KeyId, pub pressed: bool }
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError { #[error("key script token {index} `{token}` is not valid")] BadToken { index: usize, token: String } }
pub fn key_by_name(name: &str) -> Option<KeyId>;                  // KeyId::core_name, ASCII case-insensitive
pub fn parse_script(s: &str) -> Result<Vec<ScriptEvent>, ScriptError>;   // the grammar of evo-core keys.rs minus `r<R>c<C>`
pub fn to_script(events: &[ScriptEvent]) -> String;
pub fn ms_between(from_cycle: u64, cycle: u64) -> u64;           // (cycle - from) * 1000 / CLOCK_HZ, saturating
```
`to_script`: events sorted by time; a press at t whose release of the same key is at t + h with `TAP_HOLD_MS ≤ h ≤ TAP_HOLD_MS + 17` (one 60 Hz frame of slack: the real machine may release a clicked key up to an instruction or a frame late), with no other event in between and the next event at ≥ t + h + `TAP_GAP_MS` (or none), becomes the bare name (re-parsed, it holds exactly `TAP_HOLD_MS`); otherwise `+name` / `-name`; a gap from the cursor becomes `wait=N` before the token; tokens joined by one space. Lane W's real-OS test checks `tools::script::TAP_HOLD_MS` against the core's constant.

- [ ] **Step 1: Failing tests:** `parses_core_grammar` (`"2 add 3 enter"` ⇒ 8 events at 0/50, 100/150, 200/250, 300/350; `"+alpha wait=20 -alpha"`; `";"` separators; case-insensitive); `rejects_bad_tokens` (`"r1c2"`, `"wait=-1"`, `"+nokey"` with indices); `to_script_roundtrips_taps` (`to_script(parse("2 add 3 enter")) == "2 add 3 enter"`); `to_script_holds_and_waits` (`"+alpha wait=20 -alpha"` round trip); `to_script_tap_slack` (press 0, release 60 ⇒ `"enter"`; release 68 ⇒ `"+enter wait=68 -enter"`); `ms_between_cases`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test tools` → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): key script parser and writer`.

### Task E2: Key history

**Files:** Create `src/tools/history.rs`; Test `tests/tools.rs`, `tests/snapshots_tools.rs`

**Interfaces — Produces:** `pub const KEY_HISTORY_CAP: usize = 1000;` `pub fn history_line(e: &KeyEvent) -> String` = `format!("{:>10.3} s  {:<8} {}", e.cycle as f64 / CLOCK_HZ as f64, e.key.core_name(), if e.pressed { "down" } else { "up" })`.

Window "Key history" (toggled by `Action::KeyHistory`, `active` while open): newest at the bottom, stick to bottom; "Clear"; "Copy as key script" ⇒ `ctx.copy_text(to_script(events since the first, times from the first event))`. Fed from `cx.key_events` every frame, also while closed.

- [ ] **Step 1: Failing tests:** `history_line_format` (`{Enter, true, 156_000_000}` ⇒ "     1.000 s  enter    down"); `history_window_lists_presses` (click "enter" key ⇒ lines "… enter    down" and "… enter    up"); `history_is_capped`; `copy_as_script` (after a tap of "enter" and a click on "Copy as key script", the frame's `platform_output.commands` (read through `h.output()`) contains `egui::OutputCommand::CopyText("enter".into())`). Golden `snapshot_tools_key_history`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): key history window`.

### Task E3: Macros / AutoTester

**Files:** Create `src/tools/macros.rs`; Test `tests/tools.rs`, `tests/snapshots_tools.rs`

**Interfaces — Produces:**
```rust
pub const DEFAULT_SETTLE_MS: u64 = 500;
pub fn screen_sha256(frame: &Frame) -> String;               // lowercase hex SHA-256 of frame.rgba
pub struct MacroFile { pub name: String, pub script: String, pub expect_sha256: Option<String>, pub settle_ms: u64 }
impl MacroFile { pub fn to_text(&self) -> String; pub fn parse(name: &str, text: &str) -> Result<Self, String>; }
// header lines: "# expect-sha256: <64 hex>", "# settle-ms: <n>"; other '#' lines ignored; the rest is the script
pub fn macro_path(data_dir: &Path, name: &str) -> Result<PathBuf, String>;  // {data_dir}/macros/{name}.keys; name 1–64 of [A-Za-z0-9 _-]
#[derive(Clone, Debug, PartialEq, Eq)] pub enum MacroResult { Pass, Fail { got: String }, NoFrame, Done }
```
Window "Macros / AutoTester": "Name" field, script text box, "Record"/"Stop recording" (records `cx.key_events` as `ScriptEvent`s with `ms_between(record start cycle, e.cycle)`; stop writes `to_script` into the text box), "Play" (parses; error inline; schedules each event at `start + at_ms * CLOCK_HZ / 1000`, `start = cycles()` at Play; unpauses with `SetPaused(false)`; `next_deadline` = the next event's cycle, then the check cycle `last + settle_ms`), "Stop", checkbox "Check screen at end", "Settle (ms)", "Expected SHA-256" field, "Use current screen", "Save", "Load" (list of `macros/*.keys`). Each `frame()` applies every event whose cycle ≤ `cycles()` with `backend.set_key`; at the check cycle: result label "Result: PASS" / `format!("Result: FAIL (screen sha256 {got})")` / "Result: no display frame" / "Result: done" (check off). Playback keys are released when stopped early. Pause during playback (any source: Pause action, debug run row, a breakpoint stop) keeps the schedule: it is cycle-based and cycles do not advance while paused, so held keys stay held and resuming continues at the same cycles; only "Stop" (or Reset/Load state/`ReplaceBackend`, detected as `cycles()` going below the last applied event's cycle) ends playback. `blocks_input()` false.

- [ ] **Step 1: Failing tests:** `screen_sha256_matches_sha2` (fixed `Frame`, expected computed with `sha2::Sha256` in the test); `macro_file_roundtrip` (headers kept, unknown `#` ignored, bad settle ⇒ Err); `macro_names_validated` (`"../x"` Err); `record_then_stop_fills_script` (click "enter" key while recording ⇒ text "enter"); `play_presses_at_scripted_cycles` (`"2 add"` ⇒ `probe.key_events()` (the never-drained log; the app drains `take_key_events` itself) is `[(Two,true,start), (Two,false,start+7_800_000), (Add,true,start+15_600_000), (Add,false,start+23_400_000)] (as `KeyEvent`s)`); `playback_at_unlimited_speed_hits_exact_cycles` (same with `Speed::Unlimited`); `playback_across_pause_hits_exact_cycles` (Play `"2 add"`, step until `probe.key_events().len()==1`, `open_action(TogglePause)`, 10 steps (cycles unchanged, `Two` still held), `open_action(TogglePause)`, step until done ⇒ the same four events and cycles as the unpaused case); `check_passes_with_current_screen_hash` (FakeBackend::with_frame fixed ⇒ "Use current screen", Play ⇒ "Result: PASS"); `check_fails_with_other_hash`; `save_and_load_macro`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates. Golden `snapshot_tools_macros`.
- [ ] **Step 5: Commit** `feat(evo-gui): macros with AutoTester screen check`.

### Task E4: Hardware panel

**Files:** Create `src/tools/hardware.rs`; Test `tests/tools.rs`, `tests/snapshots_tools.rs`

**Interfaces — Produces:** `pub fn hms(seconds: u32) -> String` (`"01:02:03"`); `pub fn utc_seconds_of_day(now: SystemTime) -> u32`.

Window "Hardware": `format!("Backlight {:.0}%", backlight * 100.0)`, checkboxes "Charger line high (GPIO 9)", "Battery OK (GPIO 11)", "USB-C attached" (each ⇒ `set_hardware`), "Press ON" (press `KeyId::On` now, release at `cycles + MIN_HOLD_CYCLES` via `next_deadline`), labels `format!("Power hold: {}", on/off)`, `format!("Deep sleep: {}", "armed"/"not armed")`, `format!("RTC {}", hms(rtc_seconds))`, button "Set RTC to host clock (UTC)" ⇒ `HwChange::RtcSeconds(utc_seconds_of_day(SystemTime::now()))`. Errors ⇒ toasts.

- [ ] **Step 1: Failing tests:** `hms_cases`; `utc_seconds_of_day_cases` (`UNIX_EPOCH + 86_400*3 + 3_723 s` ⇒ 3_723); `toggles_send_changes` (`hw_changes()==[ChargerLine(false)]` after unticking); `press_on_holds_then_releases` (`probe.key_events()`: `(On,true,p)` then after enough frames `(On,false,p + MIN_HOLD_CYCLES)` exactly, since `next_deadline` caps the run); `set_rtc_sends_seconds`. Golden `snapshot_tools_hardware`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): hardware panel`.

---

## Lane F — Files and Help

### Task F1: Import and export files

**Files:** Modify `src/files/mod.rs` (replace the stub), `src/store.rs`; Create `tests/files.rs`

**Interfaces — Produces:**
```rust
// store.rs (save_slot/load_slot now call these)
pub fn write_state_file(path: &Path, b: &dyn EmuBackend) -> Result<(), StoreError>;   // slot format, temp file + rename
pub fn read_state_file(path: &Path, b: &mut dyn EmuBackend) -> Result<(), StoreError>;
// files/mod.rs
pub const IMPORT_MAX_BYTES: u64 = 64 << 20;
pub fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, String>;  // metadata first; dir ⇒ "is a directory"; > max ⇒ "file is too large"
#[derive(Serialize, Deserialize)] pub struct DebugStateFile { pub format: String, pub version: u32, pub breakpoints: Vec<u32>, pub watchpoints: Vec<Watchpoint> }   // "evo-gui-debug", 1
#[derive(Serialize, Deserialize)] pub struct SettingsFile { pub format: String, pub version: u32, pub prefs: Prefs }   // "evo-gui-settings", 1
pub fn check_size(path: &Path, max: u64) -> Result<(), String>;        // the metadata half of read_limited; same errors
pub struct Files { .. }   // keeps `pub fn new(opts: &AppOptions) -> Self` from the stub
```
Every import validates format and version: `DebugStateFile` and `SettingsFile` with a `format` other than their constant or `version != 1` ⇒ `Import failed: not a debug state file` / `Import failed: not a settings file`, nothing applied. State files are refused by the slot header's state tag when they come from another backend (`StoreError` ⇒ `Import failed: …`). Flash: `import_flash` takes a path, so F1 first calls `check_size(path, IMPORT_MAX_BYTES)` (a directory or an oversized file is refused before the backend opens it); `MachineBackend` then reads through `NvState::read_file`, which itself checks the header and file size before reading the body (Note: W2 needs no extra limit).
| action | dialog title / default name / filter | backend | after |
|---|---|---|---|
| ExportRam / ImportRam | "Export RAM image" `evo.evoram` / "Import RAM image"; `FileFilter{"RAM image", &["evoram"]}` | `export_ram` / `import_ram(read_limited)` | import ⇒ `Resync` |
| ExportState / ImportState | "Export calculator state" `evo.evostate`; `{"Calculator state", &["evostate"]}` | `write_state_file` / `read_state_file` (size-limited read) | import ⇒ `Resync` |
| ExportDebug / ImportDebug | "Export debug state" `evo.evodebug`; `{"Debug state", &["evodebug"]}` | JSON; import ⇒ `set_breakpoints`, `set_watchpoints` (wrong `format`/`version` ⇒ error) | — |
| ExportSettings / ImportSettings | "Export settings" `evo-settings.json`; `{"Settings", &["json"]}` | JSON of `cx.prefs`; import ⇒ `ApplyPrefs(imported with bundle = current)` | — |
| ExportFlash / ImportFlash | "Export flash image" `evo.evoflash`; `{"Flash image", &["evoflash"]}` | `export_flash(path)` / `check_size(path, IMPORT_MAX_BYTES)` then `import_flash(path)` | import ⇒ `Resync` |

Toasts: `format!("Exported {what} to {}", path.display())`, `format!("Imported {what}")`, `format!("Export failed: {e}")`, `format!("Import failed: {e}")`, `what` ∈ "RAM image", "calculator state", "debug state", "settings", "flash image". Cancel ⇒ nothing. State, RAM and flash imports call `cx.backend.persist()` before loading, as slot loads do (a persist error is a toast and the import goes on).

- [ ] **Step 1: Failing tests** (FakePlatform answers, temp dirs): `ram_roundtrip` (poke, export, poke other, import ⇒ peek back, toasts); `state_roundtrip_and_slot_compat` (a state file and slot 1 file have the same bytes); `debug_state_roundtrip` (breakpoints and a watchpoint survive); `settings_roundtrip_keeps_bundle` (import a file with another keymap binding and `bundle` "x" ⇒ app keymap changed, `prefs().bundle` unchanged); `flash_roundtrip` (`flash_imports()==1`); `import_ram_garbage_keeps_machine`, `import_state_garbage_keeps_machine`, `import_debug_garbage_keeps_breakpoints`, `import_settings_garbage_keeps_prefs`, `import_flash_garbage_keeps_machine` (each ⇒ "Import failed: …"); `import_oversized_file_is_refused_unread` (sparse `IMPORT_MAX_BYTES + 1` ⇒ "Import failed: file is too large"); `import_directory_fails`; `import_flash_oversized_is_refused_unread` (sparse `IMPORT_MAX_BYTES + 1` `.evoflash` ⇒ "Import failed: file is too large", `flash_imports()==0`); `import_flash_directory_fails` (⇒ "Import failed: is a directory", `flash_imports()==0`); `import_state_from_other_backend_keeps_machine` (a state file written from `FakeBackend::new().with_state_tag("other")`, imported into the default fake ⇒ "Import failed: …", cycles unchanged); `import_settings_wrong_format_keeps_prefs` (`format: "evo-gui-debug"` and, separately, `version: 2` ⇒ "Import failed: not a settings file", prefs unchanged); `import_debug_wrong_version_keeps_breakpoints`; `cancel_does_nothing`; existing slot tests in `tests/store.rs` and `tests/app.rs` still pass.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test files` → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): import and export RAM, state, debug state, settings and flash`.

### Task F2: Licenses, update check, bug report

**Files:** Modify `src/help/mod.rs` (replace the stub); Create `src/help/update.rs`, `apps/evo-gui/about.toml`, `apps/evo-gui/about.hbs`, `apps/evo-gui/assets/third-party-licenses.txt`, `tests/help.rs`, `tests/snapshots_help.rs`

**Interfaces — Produces:**
```rust
pub const UPDATE_REPO: &str = "connerglover/ti84evo-emu";
pub fn latest_release_url() -> String;   // "https://api.github.com/repos/{UPDATE_REPO}/releases/latest"
pub const ISSUES_URL: &str = "https://github.com/connerglover/ti84evo-emu/issues/new";
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)>;   // optional leading 'v', "a.b.c", extra suffix after '-' ignored
#[derive(Clone, Debug, PartialEq, Eq)] pub struct Release { pub tag: String, pub url: String, pub version: (u64, u64, u64) }
pub fn parse_release(json: &str) -> Result<Release, String>;   // fields tag_name, html_url
pub fn licenses_text() -> String;   // include_str!("../../assets/third-party-licenses.txt") + "\n\nDejaVu fonts\n" + include_str!("../../assets/LICENSE")
pub struct Help { .. }   // update worker handle, found Release, open windows; keeps `pub fn new(opts: &AppOptions) -> Self`
```
Licenses: install `cargo install cargo-about --locked --version 0.9.2`; `about.toml` accepts the licenses the dependency tree uses (`MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `Unicode-3.0`, `MPL-2.0`, `BSL-1.0`, `CC0-1.0`, `CDLA-Permissive-2.0` (webpki-roots via ureq/rustls), `OFL-1.1`, `Ubuntu-font-1.0`; `ring`'s `Apache-2.0 AND ISC` is covered by these), `ignore-dev-dependencies = true`, `ignore-build-dependencies = false`, `private = { ignore = true }` and the six workspace crates (`armv8m`, `evo-hw`, `evo-image`, `evo-core`, `evo-cli`, `evo-gui`) excluded so only third-party crates are listed. If `cargo about generate` still reports a license not in this list, add it only if it is OSI-approved or a permissive data/font license, and name it in the commit message; otherwise stop and report; plain-text `about.hbs` (one section per license: name, the crates using it, the text); generate with `cargo about generate -m apps/evo-gui/Cargo.toml apps/evo-gui/about.hbs -o apps/evo-gui/assets/third-party-licenses.txt`. Window "Third-party licenses": scrollable monospace `licenses_text()`. The text file is not Rust source, so `boundary.rs` does not apply to it. "Check for updates": worker thread `platform.http_get(latest_release_url())` (a second click while running ⇒ toast "Already checking for updates"); polled in `frame()`: newer ⇒ toast `format!("Version {tag} is available")` and window "Update available" with "Open download page" (`open_url(release.url)`); same or older ⇒ `format!("You have the latest version ({})", env!("CARGO_PKG_VERSION"))`; `Err("HTTP 404")` ⇒ "No releases published yet"; other errors ⇒ `format!("Update check failed: {e}")`. "Report a bug" ⇒ `open_url(ISSUES_URL)`, failure ⇒ `format!("Could not open the browser: {e}")`.

- [ ] **Step 1: Failing tests:** `parse_version_cases` (`"v0.2.0"`, `"0.2.0-rc1"`, `"x"` None); `parse_release_cases` (missing field ⇒ Err, garbage ⇒ Err); `update_newer_shows_window` (`set_http(Ok(r#"{"tag_name":"v99.0.0","html_url":"https://example.invalid/r"}"#))`, click, then at most 200 × (`h.step()` + 10 ms sleep) until the toast appears, else panic "update check did not finish" (every update test uses this bound) ⇒ toast and "Open download page" ⇒ `urls()==["https://example.invalid/r"]`); `update_current_says_latest`; `update_404_says_no_releases`; `update_error_toasts` (default `offline`); `report_bug_opens_issues`; `licenses_mention_key_crates` (`licenses_text()` contains "egui", "openh264" (or "gif" under the fallback), "MIT", "Bitstream Vera"). Goldens `snapshot_help_licenses` and `snapshot_help_update_available` (the "Update available" window after the `v99.0.0` reply); run `cargo test -p evo-gui --features snapshots --test snapshots_help` in Step 4.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test help` → FAIL. **Step 3: Implement**; generate the licenses file. **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): third-party licenses, update check, bug report`.

---

## Lane W — `MachineBackend` wiring and real-OS tests (after all lanes merged)

### Task W1: Debug methods on the real machine

**Files:** Modify `apps/evo-gui/src/backend/machine.rs`; Test `tests/machine_backend.rs`

**Interfaces — Consumes:** Lane A (`Machine::{set_watchpoints, watchpoints, arm_step_over, arm_step_out, arm_run_to, disarm, peripherals, write_reg}`, `bus.{set_bus_monitor, bus_records, clear_bus_records, poke}`), Lane 0 trait.

Mapping: `poke` ⇒ all bytes checked with `bus.peek(a, 1)` first, then `bus.poke(a, 1, b)` each (`Unwritable(first bad)`); `memory_regions` ⇒ `[("Flash (NS)", 0x0000_0000, 16 MiB), ("Flash (S)", 0x1000_0000, 16 MiB), ("SRAM (NS)", 0x2000_0000, 0x18_0000), ("SRAM (S)", 0x3000_0000, 0x18_0000)]`; `set_core_reg(i)` ⇒ `write_reg` with `r{i}` / `sp` / `lr` / `pc` / `xpsr` (i > 16 ⇒ `BadRegister`); `set_banked_reg(name)` ⇒ `write_reg(name)` (`RegError` ⇒ `BadRegister`); `set_breakpoints` replaces the set; `begin_goal(StepOver)` ⇒ `Ok(arm_step_over())`, `StepOut` ⇒ arm, `Ok(true)`, `RunTo(a)` ⇒ arm, `Ok(true)`; `cancel_goal` ⇒ `disarm`; watchpoints map field by field (`WatchError` ⇒ `Invalid(e.to_string())`); `peripherals` ⇒ `DeviceView`/`RegRow` copies. `reset()` keeps breakpoints, watchpoints and the bus-monitor range on the new machine and drops the goal. `RunStatus::Watchpoint` already maps (Lane A).

- [ ] **Step 1: Failing tests** (synthetic images as in `machine_backend.rs`; a store program for watchpoints): `poke_writes_sram_and_refuses_devices`; `regions_cover_flash_and_sram`; `register_writes_reach_the_cpu` (pc, sp, `msp_ns`, `xpsr`; `"nope"` ⇒ `BadRegister`); `watchpoint_stops_run_for`; `bus_monitor_records_the_store`; `step_over_and_run_to_goals` (`RunStatus::Goal{pc}`); `peripherals_include_uart0_and_nvic`; `reset_keeps_watchpoints`.
- [ ] **Step 2: Run** `cargo test -p evo-gui --test machine_backend` → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): wire the debugger to the emulated machine`.

### Task W2: I/O, hardware, RAM and flash on the real machine

**Files:** Modify `src/backend/machine.rs`; Test `tests/machine_backend.rs`

Mapping: `set_key` also pushes `KeyEvent{key, pressed, cycle: sched.now}` (drained by `take_key_events`); `take_uart2` ⇒ `take_uart_tx(Uart2)`; `send_uart` ⇒ `inject_uart_rx`; `hardware()` ⇒ backlight from `Pwm`, `charger_line`/`battery_ok`/`usb_attached` from fields the backend keeps (start `true`, `true`, `false`; reapplied after `reset`), `power_state()`, `rtc_seconds()`; `set_hardware` ⇒ `set_gpio_input(9|11, Some(b))`, `set_usb_cc(1, 0)` / `(0, 0)`, `set_rtc(s)` (`s >= 86_400` ⇒ `Invalid`); `export_ram`/`import_ram` ⇒ Lane A (`RamError` ⇒ `BadState`); `export_flash(path)` ⇒ `nv_state().write_file(path)` (`Io`); `import_flash(path)` ⇒ `NvState::read_file(path)` (bounded: it checks the header and the file size before reading the body; error ⇒ `BadState`, unchanged), `Machine::from_bundle_nv(&bundle, &EVO_7_1, Some(nv))` keeping breakpoints and watchpoints, `fresh = false`, `on_disk = None` (the next persist writes it).

- [ ] **Step 1: Failing tests:** `key_events_have_machine_cycles`; `uart2_roundtrip_is_drained`; `hardware_inputs_reach_gpio` (after `ChargerLine(false)`, `peripherals()` `gpio` `data_in` bit 9 clear); `rtc_set_reads_back`; `ram_image_roundtrip_and_rejects_garbage`; `flash_export_import_roundtrip` (export to `a.evoflash`; `import_flash(a)` ⇒ `Ok`, `needs_provision()` false; export to `b.evoflash` ⇒ the two files are byte-identical; a slot saved before the import still loads after it, since the flash is the same); `import_flash_rejects_garbage_unchanged`; `load_state_from_other_backend_is_refused_unchanged` (`MachineBackend::load_state(&FakeBackend::new().save_state()?)` ⇒ `Err`, and cycles, `cpu()` registers and `peek(0x3000_0000, 16)` equal their values before the call).
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS; gates.
- [ ] **Step 5: Commit** `feat(evo-gui): wire UART2, key events, hardware, RAM and flash images`.

### Task W3: Real-OS end-to-end tests

**Files:** Modify `tests/real_os.rs`; Modify `tests/machine_backend.rs` (constant check)

- [ ] **Step 1: Write the env-gated tests** (skip pattern of `real_os.rs`; release; temp dirs only):
  - `real_os_wizard_provisions_to_home_screen`: harness with `open_wizard: true`, a `FakePlatform` whose factory `|bundle, nv_dir| MachineBackend::open(bundle, nv_dir).map(|b| Box::new(b) as _).map_err(|e| e.to_string())` (nv_dir is the temp data dir's `flash/`, passed by the wizard) and whose `queue_open` answers `$EVO_OS_IMAGE`; Next on Welcome, Next on Choose (no product/version error shown), wait (steps with 50 ms sleeps, ≤ 120 s) for page "Done"; Finish; run until the home-screen check of `real_os_home_screen_and_keypad` passes (selection-blue pixels present).
  - `real_os_framebuffer_watchpoint_stops`: provisioned backend, `set_watchpoints(vec![Watchpoint{start: 0x2004_8000, len: 320*240*2, kinds: W}])`, `run_for` in 10 M chunks up to 3 G cycles ⇒ `RunStatus::Watchpoint{dir: Write, addr}` with `addr` in the range.
  - `real_os_macro_screen_check_passes`: provisioned, booted to home, `save_state`; play `"2 add 3 enter"` through the Macros window with "Check screen at end" and an empty hash ⇒ read `screen_sha256` after settle; `load_state`; set that hash as expected, play again ⇒ "Result: PASS".
  - `gui_and_core_tap_constants_agree` (in `machine_backend.rs`, not env-gated): `evo_gui::tools::script::TAP_HOLD_MS == evo_core::keys::TAP_HOLD_MS` and the same for `TAP_GAP_MS`.
- [ ] **Step 2: Run** `cargo test --release -p evo-gui --features snapshots --test real_os -- --nocapture` with `EVO_OS_IMAGE` set → PASS with no `skipping:` line (without it: every test prints `skipping:`; then the executor reports instead of marking the step done).
- [ ] **Step 3:** `cargo build --release --locked -p evo-gui -p evo-cli` (what `release.yml` runs) succeeds; gates.
- [ ] **Step 4: Commit** `test(evo-gui): real-OS wizard, watchpoint and AutoTester checks`.

---

## Decisions made while planning

- **Product id 23, OS 7.1.0.4421.** The user's bundle METADATA reads `bundle_target_device:84US`, `bundle_target_prodid:23`; the wizard rejects other product ids and warns on other versions. A number from metadata is not TI code.
- **MP4 stays.** `openh264 0.9.9` (source build) + `mp4 0.14.0` built and encoded a frame on this Windows host (MSVC) on 2026-10-15; the GIF fallback is taken only if Task 0.3's local release build fails. Linux CI is verified when the user first pushes (agents never push); ubuntu runners have a C/C++ compiler, which is all the source build needs. If CI on Ubuntu then fails on `openh264-sys2`, the GIF fallback (Task C3's GIF branch) is the plan. `release.yml` keeps building: openh264-sys2 compiles with MSVC and `ring` with `cc`.
- **H.264 patent caveat accepted.** The published `evo-gui.exe` contains an H.264 encoder built from source; Cisco's OpenH264 patent licence covers only Cisco's prebuilt binaries. Decision: keep MP4 on by default (no cargo feature), and Task 0.3 adds one README sentence: "MP4 recording uses OpenH264 compiled from source; Cisco's patent licence for OpenH264 applies only to Cisco's binaries. Use animated PNG if this matters to you." The licenses file lists openh264's BSD-2-Clause licence.
- **Power is in Hardware.** The spec lists "power" under the Calculator group; the plan serves it with Tools › Hardware › "Press ON" (Task E4) and adds no Calculator entry, so ON-key hold/release logic exists once.
- **Provisioning progress is a spinner with elapsed seconds.** The provisioning worker reports no progress fraction, so the wizard's Prepare page cannot show a percentage.
- **APNG frames are kept PNG-compressed in memory, cap 1800.** Raw RGBA would need ≈ 1.1 GB at 3600 frames; compressed single-frame PNGs bound it to tens of MiB.
- **Reset everything keeps `instances/`.** For the default instance `data_dir` is the base dir, so `instances/` holds other (possibly running) instances.
- **Non-blocking debugger goals.** Step over/out and run-to arm a goal in the core; the normal frame loop runs until `RunStatus::Goal`; Pause cancels. No UI-thread blocking runs.
- **Exec watchpoints** stop before the instruction like breakpoints (the instruction cache bypasses the bus, so fetches cannot be watched on the bus); data watchpoints stop after the accessing instruction.
- **`Device::inspect(&self, now)`** takes the cycle count (timer and RTC values derive from it).
- **Instances** use `File::try_lock` (stable since Rust 1.89) on `instance.lock`; "Change ID" relaunches with `--rename-from` and the new process renames after the old one released its lock. The default instance cannot be renamed. Instance eframe storage lives in the instance dir (`persistence_path`).
- **RTC "host clock" is UTC** (no time-zone dependency); the button says so.
- **Licenses** are generated with `cargo-about 0.9.2` into a committed text file, plus the DejaVu license.
- **Platform and Feature seams** exist so parallel lanes share no files and every dialog, clipboard, browser, network and spawn call is testable with `FakePlatform`.
- **Speed "unlimited"** is factor 1000, still capped by the 12 ms wall budget per frame.
