# TI-84 Evo Emulator — Design

Date: 2026-10-13
Status: Draft for review

## 1. Goal

A low-level (hardware) emulator for the TI-84 Evo, written in Rust, that boots
the real, unmodified TI OS image supplied by the user.

Success criteria, in order:

1. **M1** — The secure firmware boots and prints `Booting TF-M v2.1.1` on the emulated UART.
2. **M2** — Control passes from TF-M to the non-secure OS reset handler.
3. **M3** — The LCD shows the OS home screen.
4. **M4** — The calculator is usable via keypad input (ON key, RTC, timers working).
5. **M5** (stretch) — USB file transfer.

Assumptions (confirmed with the user):

- Native desktop application, Windows first, cross-platform by construction.
- Intended to be shareable/open source: **no TI code or data is ever committed**.
  The user supplies the OS bundle at runtime.
- Accuracy over speed; real-time at 156 MHz is expected from an interpreter.

## 2. Background and evidence

All reverse-engineering material lives outside the repo
(`Documents\ti84evo-re\`) and is never committed.

### 2.1 TI's web emulator

`ti84evo.ti.com` serves an Emscripten build (`ti84.js`, `ti84-<ver>.wasm`,
`ti84-<ver>.data`, a pthread worker). Its exports (`_setScanCode`,
`_getGraphicsBuffer`, `_setAngleMode`, `_isAlphaOn`, `_appTimerCb`, …) show it
is the **OS compiled directly to WebAssembly** — there is no CPU or peripheral
emulation. It reveals nothing about the hardware. It is used only as a
behavioral oracle (Section 8.3). The `.wasm` has not been captured yet.

### 2.2 The OS bundle

`TI84Evo_Bundle_7.1.0.4421.84b2` is a ZIP containing `METADATA`
(`bundle_target_device:84US`, `bundle_target_prodid:23`,
`bundle_target_type:RESTORE`) and `TI84Evo_Package.84pk2`.

The `.84pk2` is **unencrypted** and is a sequence of **MCUboot images**
(magic `0x96f3b83d`):

| Pkg offset | Header | Image size | Version     | Initial SP   | Reset vector | Role                     |
|-----------:|-------:|-----------:|-------------|--------------|--------------|--------------------------|
| `0x000000` | 0x400  | 0x0DAEB8   | 7.1.0+4413  | `0x300007F8` | `0x100B1ADD` | Secure: TF-M             |
| `0x100000` | 0x200  | 0x293678   | 7.1.0+4421  | `0x20093000` | `0x0044BC91` | Non-secure: TI OS        |
| `0x39A6D8`…`0x3F4350` | 0x40 | small | 7.1.0+4421 | — | — | 8 non-code images (apps/resources, TBD) |

Firmware strings establish:

- **CPU:** Arm Cortex-M33 with TrustZone (TF-M v2.1.1, SecureFault, SAU/MPC/PPC).
- **Libraries:** Mbed TLS 3.6.2, MCCI USB stack (Type-C / OTG).
- **Peripherals** (from IRQ handler names): APD, PAPD1/2, I2C, ICACHE,
  Keypad, OnKey, LCDC, MPC, PPC, MSC, OTG, USBTC, PLL2, RTC, SHA, TRNG,
  SPI1, SPI2, SPIF (QSPI flash), UART, UART2, Wakeup, SWTRIG, SecureWatchdog.
- **Memory layout** consistent with the Arm TrustZone convention:
  NS code `0x0000_0000`, S code `0x1000_0000`, NS SRAM `0x2000_0000`,
  S SRAM `0x3000_0000`, peripherals `0x4000_0000` (NS) / `0x5000_0000` (S).
  Exact bases and sizes are unverified.
- BL1/BL2 are **not** in the package (they reside on the device).
- The SoC vendor is **not yet identified**.

## 3. Architecture

Cargo workspace:

```
ti84evo-emu/
├─ crates/
│  ├─ armv8m/     Cortex-M33 core. Knows nothing about the TI-84 Evo.
│  ├─ evo-hw/     Memory map + peripheral models. Implements armv8m::Bus.
│  ├─ evo-image/  .84b2 / .84pk2 / MCUboot parsing and flash layout.
│  └─ evo-core/   Machine: CPU + hardware + scheduler; public emulator API.
├─ apps/
│  ├─ evo-cli/    Headless runner and RE/debug tool.
│  └─ evo-gui/    Desktop frontend (eframe/egui).
├─ tools/         Ghidra loader script, test-program sources.
└─ docs/hardware/ One file per peripheral: register map + firmware evidence.
```

Dependency direction: `apps → evo-core → {evo-hw, evo-image} → armv8m`.
`armv8m` and `evo-image` have no dependency on each other.

Principles:

- **Interpreter, not JIT.** Debuggability during RE matters more than speed.
- **The CPU core is hardware-independent** so it can be verified exhaustively
  in isolation; CPU bugs must not masquerade as peripheral bugs.
- **Unknown hardware never panics.** Unmapped accesses return 0 / are ignored
  and are logged (address, width, value, PC, security state). This log is the
  primary peripheral-discovery input.
- **Headless first.** M1–M2 need only `evo-cli`.
- **No TI material in the repo** — code, images, artwork, or dumps.

## 4. CPU core (`armv8m`)

### 4.1 Scope

ARMv8-M Mainline as implemented by the Cortex-M33:

- Full Thumb-2 (16- and 32-bit): data processing, shifts, loads/stores incl.
  exclusives, multiply/divide, bitfield, IT blocks, CBZ/CBNZ, TBB/TBH,
  barriers, hints (WFI/WFE treated as scheduler yields).
- DSP extension (SIMD add/sub, saturating arithmetic, packing).
- v8-M security: `SG`, `BXNS`, `BLXNS`, `TT/TTT/TTA/TTAT`; banked S/NS
  registers (MSP, PSP, MSPLIM, PSPLIM, CONTROL, PRIMASK, BASEPRI,
  FAULTMASK); VTOR_S/VTOR_NS.
- System peripherals: NVIC (priorities, preemption, tail-chaining, ITNS
  targeting), SysTick ×2 (S/NS), SCB, SAU + IDAU hook, MPU (checked;
  permissive until needed).
- Exceptions: complete entry/return incl. EXC_RETURN forms, secure stacking
  with integrity signature, lazy-state flags; HardFault, MemManage, BusFault,
  UsageFault, SecureFault with accurate CFSR/HFSR/SFSR/BFAR/MMFAR/SFAR (the
  firmware's fault handler prints these over UART).
- **FPU (VFPv5-SP) deferred**: until implemented, coprocessor instructions
  raise UsageFault NOCP so the need is detected immediately.

### 4.2 Structure

- Decode: a precomputed 64K-entry table for 16-bit encodings; a `match`-based
  decoder for 32-bit encodings. Both produce an `Instr` enum; execution is a
  separate function. The disassembler (for traces/GUI) reuses the decoder.
- `Bus` trait: `read{8,16,32}` / `write{8,16,32}` with an access descriptor
  (security state, privilege, instruction vs data) returning
  `Result<T, BusError>`. The core maps errors to BusFault/SecureFault.
- Timing: approximate per-instruction cycle costs; not cycle-exact.

## 5. Hardware (`evo-hw`) and images (`evo-image`)

### 5.1 Boot (bootloader replacement)

`evo-image` parses the bundle and MCUboot images. `evo-core` acts as BL2:
place each image at its flash address, load SP/PC from the secure image's
vector table, start in secure privileged thread mode. Signatures are not
verified. Image flash placement and the 8 non-code images' roles are
determined during M1/M2.

### 5.2 Memory map

- Flash and SRAM are aliased at secure and non-secure addresses onto the same
  backing storage; MPC/PPC models gate access.
- Flash is the external QSPI device, memory-mapped (XIP) and also programmable
  through the QSPI controller (the OS persists variables/archive there).
- Flash content persists to a file on disk across runs.
- All bases/sizes are verified against firmware usage and recorded in
  `docs/hardware/memory-map.md`.

### 5.3 Peripheral discovery loop

For each peripheral:

1. Run until the unmapped-access log shows the next device.
2. Locate the driver at the logged PC (IRQ handler names, TF-M/Mbed TLS
   strings help identify functions).
3. Infer register semantics from driver usage.
4. Record the register map with firmware-address evidence in
   `docs/hardware/<periph>.md`.
5. Implement the minimal model that satisfies the driver, with a unit test.

Early task: try to match IRQ numbering, peripheral bases and clock/QSPI
strings against published MCUs; a datasheet would shortcut much of this.

### 5.4 Peripheral priority

| Milestone | Peripherals |
|-----------|-------------|
| M1 | Clock/PLL + power stubs, UART, SysTick, SAU, MPC/PPC |
| M2 | SHA, TRNG, QSPI flash controller, ICACHE stub |
| M3 | LCDC (framebuffer format, refresh) |
| M4 | Keypad, OnKey, RTC, timers, APD/Wakeup |
| M5 | USB OTG / Type-C (MCCI stack) |

### 5.5 Scheduler

A single cycle-keyed event queue in `evo-core`. Peripherals schedule their own
events (timer expiry, LCD frame, RTC tick); nothing polls per instruction.
WFI advances time to the next event.

## 6. Frontend and state

### 6.1 `evo-core` API

`Machine::new(bundle, flash_file)`, `run(cycles)`, `step()`,
`set_key(key, pressed)`, `framebuffer()`, `uart_output()`,
`save_state()/load_state()`, debug accessors (registers, memory, breakpoints).

### 6.2 `evo-cli` (M1)

Flags: `--uart`, `--trace [range]`, `--break <addr>`,
`--screenshot <file> --after <cycles>`, `--keys "<script>"`,
`--load-state/--save-state`, `--gdb <port>`.

### 6.3 GDB remote stub (M1–M2)

GDB RSP server on the machine so `arm-none-eabi-gdb`, Ghidra and IDA can
attach to the running firmware (registers, memory, breakpoints, step).

### 6.4 `evo-gui` (M3)

eframe/egui. Scaled LCD (nearest/smooth), clickable keypad from a layout
table (no TI artwork), configurable PC-keyboard mapping, toggleable debug
panels (registers, disassembly, memory, UART, unmapped-access log),
save-state slots.

Visual design (user direction): the **layout of the classic TI-84 Plus CE web
emulator** — a single vertical calculator, screen on top, full keypad below —
rendered in the **styling of TI's Evo web emulator**:

- Light blue app background; calculator body is a white rounded rectangle
  with a thick blue outline, screen inset at the top of the body.
- Keys: light-gray rounded keys with dark main labels; dark-slate number
  keys; teal `2nd`; green `alpha`; secondary legends in blue (2nd functions,
  left) and green (alpha letters, right) above each key, using the **Evo
  legends** (e.g. `insert`, `plot`, `angle`, `recall`, `on ⌂`).
- Four-way D-pad as dark separated wedges with a center gap.
- Slim left toolbar (Evo style): screenshot, save/load state, reset,
  debug-panel toggle, about. Debug panels open to the right of the
  calculator so the calculator column stays intact.
- Key press feedback: pressed-state color + optional hover outline; physical
  PC-key presses animate the matching on-screen key.
- No TI logo or wordmark; the header shows a neutral title (e.g. "TI-84 Evo
  Emulator") in the Evo header position.
- Keypad geometry and legends are data (a layout table), not hard-coded
  drawing, so the styling can be tuned without touching input logic.

### 6.5 Save states

`serde`-serialized, versioned binary format covering CPU, peripherals, RAM and
scheduler queue. Flash is stored separately (it persists across resets on
real hardware).

## 7. Error handling

- Guest faults are emulated faults, not host errors.
- Unmapped/unknown accesses: logged, non-fatal (configurable to break into
  the debugger).
- Host-side errors (bad bundle, I/O, incompatible save state) are returned as
  typed errors (`thiserror`) from library crates; apps report them and exit
  non-zero. No `unwrap` on user-supplied input.
- An unimplemented instruction or required feature raises the architecturally
  correct fault and is logged so it can't fail silently.

## 8. Testing

### 8.1 CI (no TI material)

- Per-instruction unit tests in `armv8m` (flags, edge cases, alignment).
- Differential testing: randomized instruction sequences executed in
  `armv8m` and a reference model (QEMU `cortex-m33`, or Unicorn if its M33
  support suffices), comparing registers/memory per step.
- Hand-written test programs (assembled with `arm-none-eabi`, sources in
  `tools/`) for exceptions, tail-chaining, S/NS transitions; results via
  semihosting.
- Peripheral model unit tests; `evo-image` tests on synthetic MCUboot images.

### 8.2 Local (gated on `EVO_OS_IMAGE`, skipped if unset)

- M1: UART output contains `Booting TF-M v2.1.1`.
- M2: PC reaches the non-secure reset handler.
- M3/M4: scripted key sequences produce screenshots matching approved images.

### 8.3 Oracle (later)

Once the TI web emulator `.wasm` is captured, run identical key scripts there
and compare screens to catch OS-level behavioral differences.

## 9. Out of scope

JIT; cycle-exact timing; TI Connect integration; native C/ASM program
support; audio; a web/WASM build (possible later — `evo-core` has no
GUI/OS dependencies).

## 10. Risks

| Risk | Mitigation |
|------|-----------|
| SoC undocumented; peripherals inferred only from code | Discovery loop + GDB stub + SoC identification attempt early |
| Subtle CPU bugs derail peripheral RE | Differential testing before M1 debugging |
| TF-M depends on BL2-provided state (shared data, boot status) | Inspect TF-M's boot-data reads during M1; synthesize what it expects |
| Flash layout / XIP mapping guessed wrong | Verify against literal pointers in both images; document evidence |
| USB stack complexity | M5 is a stretch goal; file transfer could fall back to direct flash-variable injection |
