# CEmu-style features for evo-gui — Design

Date: 2026-10-15. Status: approved in conversation (triage + design); this document records it.

## Goal

Make `evo-gui` feel like CEmu: a ROM setup wizard on first launch, and CEmu's useful tools
(state/file import-export, capture, a full debugger, peripherals, macros), adapted to this
emulator and organised in the existing left sidebar instead of a menu bar.

## Scope (triage, approved)

In: ROM setup wizard; instances; import/export of RAM image, calculator state, debug state,
settings (incl. keypad mapping), flash ("ROM") image; reload ROM; console; screenshot to
clipboard; animated PNG and MP4 recording; extended debug control (step over/out, run to
cursor), editable registers + cycle counter; editable memory with search and multiple views;
memory visualizer; breakpoint list; read/write/execute watchpoints; Peripherals panel;
Bus monitor; Stacks panel; Hardware panel; key history; macro record/playback with optional
screen check (AutoTester); speed control; check for updates; report a bug; third-party
licenses; compact mode; reset layout; reset everything.

Out (with reason): opening TI variables and OS variables / TI-Basic debug / OS panel
(USB link protocol and OS internals not emulated or reversed); bootable image (covered by
flash + state export); language (English only); Exit (window close).

## Architecture

Unchanged layering: `armv8m` → `evo-hw` → `evo-core` → `apps/evo-gui` (only
`backend/machine.rs` may name core crates; enforced by `tests/boundary.rs`). New GUI features
go through the `EmuBackend` trait (extended) and are covered by `FakeBackend` tests and
snapshot goldens; real-OS behaviour by env-gated tests (`EVO_OS_IMAGE`).

### Core additions

1. **Watchpoints and bus monitor** (`evo-hw::MemoryMap` + `evo-core::Machine`): an optional
   access hook on the CPU's data path. A `WatchSet` of `(range, kinds: R|W|X)`; when non-empty,
   every data access (and fetch, for X) is checked; a hit stops `run` with a new
   `StopReason::Watchpoint { addr, kind, value, pc }`. A bus monitor records accesses in a
   configurable range into a bounded ring (address, width, R/W, value, PC, cycle). Zero cost
   when both are empty (one branch per access); batching (`run_batch`) is disabled while
   either is active. Exec watchpoints reuse breakpoints.
2. **Device inspection**: `Device::inspect(&self) -> Vec<RegView>` (name, offset, value,
   optional note), side-effect free, default empty; implemented for timer, RTC, UARTs, LCDC,
   PWM, keypad, GPIO, SPIF, MPCs, CRU/SPCTRL. Plus SysTick (S/NS) and NVIC enabled/pending
   from `armv8m`.
3. **Debug stepping**: step over (BL/BLX → run until return address or breakpoint), step out
   (run until LR/EXC_RETURN target), run to address.
4. **RAM image**: export/import raw SRAM (1.5 MiB, three banks concatenated) with a small
   header (`EVORAM`, version, size); import validates size.
5. **Register writes**: set core and special registers (incl. banked) from the GUI.
6. **Key events**: the backend reports key presses with cycle stamps for key history and
   macro recording.

### GUI additions

- **ROM Setup Wizard** (modal, CEmu-like): Welcome → Choose bundle (file dialog via `rfd`,
  drag-and-drop) with validation (parse, product id, OS version; non-7.1.0.4421 warns that
  provisioning is tied to that BSP) → Prepare calculator (provision with progress) → Done.
  Shown when no bundle is linked; the bundle path is stored in prefs. Sidebar "Change ROM"
  reopens it; "Reload ROM" re-reads the file and reboots.
- **Sidebar** (grouped icon sidebar, popovers / right-side dock tabs, no menu bar):
  Calculator (power/reset, reload/change ROM, states, speed) · Capture (screenshot, copy,
  record APNG, record MP4) · Files (all imports/exports) · Debug (dock) · Tools (key history,
  macros/AutoTester, Hardware) · Settings (keymap, display, instance, compact mode, reset
  layout, reset everything) · Help (about + licenses, check for updates, report a bug).
- **Debug dock tabs**: Control (continue/pause/step/over/out/run-to, cycle counter),
  Registers (editable), Disassembly (existing + run-to-cursor), Memory (editable, search,
  N views), Visualizer (address/format/width/height → image), Breakpoints (list),
  Watchpoints, Peripherals, Bus monitor, Stacks, Console (UART0/UART2 + emulator log,
  UART input line), Unmapped (existing).
- **Hardware panel**: backlight level, battery/charger lines (GPIO 9/11), USB-C attach,
  ON key, power state, RTC set from host clock.
- **Capture**: PNG (existing), copy to clipboard (`arboard`, already a transitive dep),
  animated PNG (`png` crate APNG), MP4 (H.264 via `openh264` built from source + an MP4
  muxer; frames at the LCD's 60 Hz, 320×240 scaled ×2 for codec-friendly size). If
  `openh264` cannot build on CI, fall back to GIF and record the decision.
- **Macros / AutoTester**: record key presses (with timing) to a key script (existing
  `parse_key_script` grammar); play back; optional assertion at the end: framebuffer
  SHA-256 must equal a stored value; results shown as pass/fail.
- **Instances**: `--instance <id>` → data dir `…/evo-gui/instances/<id>`; "New instance"
  spawns the same exe with a fresh id; "Change instance ID" renames the current one.
- **Speed control**: 1×, 2×, 4×, unlimited, pause (pacer).
- **Updates / bug report**: "Check for updates" queries GitHub releases of
  `connerglover/ti84evo-emu` only when clicked (configurable constant); "Report a bug" opens
  the issues page in the browser.
- **Compact mode / reset layout / reset everything** (the last with confirmation; deletes
  the instance's data dir contents after a final export prompt).

## Error handling

Every import validates format/version/size and reports a toast; nothing panics on bad files.
Recording failures stop the recording and keep what was written. Wizard validation errors are
shown inline. Network errors in update checks are toasts.

## Testing

FakeBackend unit tests and kittest snapshots for every new panel and the wizard; core unit
tests for watchpoints (R/W/X, zero-cost when empty, batching disabled while active), bus
monitor ring bounds, device inspection side-effect freedom, step over/out, RAM image
round-trip; env-gated real-OS tests: wizard → provision → home screen; a watchpoint on the
framebuffer stops the run; macro playback of `2 + 3 enter` with screen-hash assertion passes.
CI release workflow keeps building `evo-gui.exe` with the new dependencies.
