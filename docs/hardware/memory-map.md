# TI-84 Evo memory map

Status: flash placement of OS images 0 and 1 for OS 7.1 (bundle
`7.1.0.4421`), established by `evo-cli inspect --score-bases` and hard-coded
as `evo_image::Layout::EVO_7_1`. The memory sizes and the peripheral table
come from the Plan 2 research (the per-block docs in this folder); the rest is
refined by the discovery loop (spec §5.3).

## Memory

| region | NS alias | S alias | size | notes |
|---|---|---|---|---|
| Flash (QSPI NOR, XIP) | `0x0000_0000..0x00FF_FFFF` | `0x1000_0000..0x10FF_FFFF` | **16 MiB**, erased 0xFF | the flash MPC covers 16 MiB (`security.md` §1) and the SPIF probe always selects the 16M part (`spif-nor.md` §6.1) |
| SRAM | `0x2000_0000..0x2017_FFFF` | `0x3000_0000..0x3017_FFFF` | **1.5 MiB** = 3 × 512 KiB banks (`0x..00_0000`, `0x..08_0000`, `0x..10_0000`) | one SRAM MPC per bank (`security.md` §1) |
| BL2 shared data | | `0x3000_0000` | one word, `0x0004_2016` | written by the emulator at power-on and on every system reset (`boot-flow.md` §4) |

Flash areas the firmware uses besides the image slots: PS at `0x0009_0000..0x000A_BFFF`,
ITS at `0x000A_C000..0x000A_FFFF` (`boot-flow.md` §2), and a one-shot mailbox at
`0x00CF_0000` that is read and left alone while it is blank (`spif-nor.md` §6.3).

## Address conventions

- **Flash offsets are Non-secure (NS) alias addresses.** Flash is mapped at
  `0x0000_0000`; `Layout` fields and `Layout::place` addresses are flash
  offsets (place with `flash_base` 0).
- **The Secure (S) alias of flash offset `o` is `o | 0x1000_0000`.** Both
  aliases name the same bytes.
- The secure image runs from the S alias. Boot sets
  `VTOR_S = 0x1000_0000 | (secure_slot_base + header size of image 0)`
  = `0x1000_0000 | (0x000B_0000 + 0x400)` = **`0x100B_0400`**.
- SRAM, as the images use it: the secure initial SP is `0x3000_07F8` and the
  secure reset handler sets `MSPLIM = 0x3000_0000` (S alias at
  `0x3000_0000`); the BSP's initial SP is `0x2004_6D20`; image 1's first word
  is `0x2009_3000` (NS alias at `0x2000_0000`).
- Peripherals: the S alias of an NS peripheral address `p` is
  `p | 0x1000_0000` (0x4xxx_xxxx NS, 0x5xxx_xxxx S). The firmware uses the NS
  alias for the blocks SAU region 3 makes Non-secure, even from secure code.
- BSP code addresses are NS (`0x001x_xxxx`); the same bytes sit at the S alias
  `0x101x_xxxx`, which some evidence addresses use (`boot-flow.md`).

## Result

| image | role | pkg offset | header | payload | slot incl. TLVs | slot base (flash offset) | runs from | slot end incl. TLVs |
|---|---|---|---|---|---|---|---|---|
| 0 | Secure | `0x0000_0000` | `0x400` | `0x000D_AEB8` | `0x000D_B4F3` | **`0x000B_0000`** | S alias `0x100B_0000` | `0x0018_B4F3` |
| 1 | Non-secure | `0x0010_0000` | `0x200` | `0x0029_3678` | `0x0029_3A99` | **`0x0038_0000`** | NS alias `0x0038_0000` | `0x0061_3A99` |

Images 2–9 (header `0x40`) are placed at `ns_slot_base` (`0x0038_0000`) plus
their package distance from image 1: flash offset = pkg offset − `0x10_0000` +
`0x38_0000`. The OS image's protected TLVs 0xB1..0xB8 give the payload
addresses of images 2..9, and the BSP's image-validation service requires an
MCUboot header `0x40` bytes before each (`boot-flow.md` §3.3). For the 7.1
bundle (package offsets `0x0039_A6D8..0x003F_495C`) they occupy
`0x0061_A6D8..0x0067_495C`. Their roles are still open (see below).

The package layout is not the flash layout. Image 1 sits at package offset
`0x10_0000` but flash offset `0x38_0000`.

## Method

`evo-cli inspect <bundle> --score-bases`. The scorer lives in
`apps/evo-cli/src/score.rs`.

For each image, every candidate slot base `B` from `0x0000_0000` to
`0x0080_0000` (exclusive) in `0x1000` steps (2048 candidates) is tried under
both aliases: `B` (NS) and `B | 0x1000_0000` (S). The score for `B` is the
sum over both aliases, and the tables report the split. Offsets `o` are slot
offsets, header included.

1. **+10 per vector-table entry** (the words after the initial SP at
   `B + hdr`, up to the first even non-zero word, at most 495) that is odd,
   lands in `[B, B + hdr + size)` and does not decode to `Undefined` at
   `entry & !1`. This is scored for image 0 only. Image 1's words after its
   reset vector are code, not a vector table.
2. **+1 per word-aligned 32-bit literal** in the payload equal to `B + o`,
   where `o` is the start of a NUL-terminated run of at least 6 printable
   characters in the payload. The `distinct` column (not scored) counts how
   many different strings those hits name.
3. **+1 per BL in the first 64 KiB of payload** (linear sweep) whose target
   is in the payload and decodes as `PUSH`.

The brief's candidate range `[min_handler - size, min_handler]` is replaced
by this wider search. It is meaningless for image 1, and the wide range also
shows that nothing outside it competes.

### Stop rule as applied

The brief requires the winner to beat the runner-up by at least 2x. Two of
the three criteria cannot tell candidates apart, so a ratio of totals would
measure constants rather than evidence:

- Criterion 3 is identical for every base, because BL is PC-relative. It is
  621 for image 0 and 2134 for image 1.
- Criterion 1 is flat at 40 hits for every base whose slot covers all of
  image 0's handlers (177 candidates by address range). A handler address
  that lands on arbitrary code almost always decodes as something.

The rule is therefore applied to criterion 2. The winner (best total) is
accepted only if:

- its string-literal hits are at least twice those of the best other
  candidate, and
- no candidate explains more vector-table entries.

`real_bundle_scores_reproduce_evo_7_1` (env-gated on `EVO_OS_IMAGE`)
enforces this rule, together with a 2x margin on distinct strings, against
`Layout::EVO_7_1`.

## Evidence

### Image 0 (Secure): `0x000B_0000`

48 vector-table words checked, 1312 strings. The 48 words are 40 handler
addresses, 7 zero entries, and one SG-veneer word. The SG word is odd, so it
is counted as an entry; the run ends at the even word after it.

```
  rank  base         total  vectors NS/S  strings NS/S  distinct  BL->PUSH
     1  0x000b0000   1287          0/40        236/30       264       621
     2  0x00086000   1049          0/40          28/0         7       621
     3  0x0008a000   1049          0/40          28/0         1       621
     4  0x0000f000   1043          0/39          26/6         3       621
     5  0x0009a000   1043          0/40          21/1         5       621
image 0: ACCEPT 0x000b0000: string hits 266 vs 118 at runner-up 0x000ba000 (margin 2.3x, need 2x); 264 different strings vs at most 19 elsewhere; vector entries 40; total 1287
```

- Winner: 266 string hits naming 264 different strings through 264
  different literal values. The ratio of totals to rank 2 is 1.23x, which is
  the plateau effect described above.
- The runner-up by string hits is `0x000B_A000` (118). It is not in the
  top 5 because most handlers fall below its slot. All 118 hits are a single
  constant, `0x0018_F2C0`, repeated 118 times. That constant lands on one
  string start at `0xBA000` by coincidence. At the winning base it points to
  flash `0x0018_F2C0`, just past image 0's end (`0x0018_B4F3`); see the open
  questions.
- On distinct strings the next best is `0x000A_0000` with 19 (21 hits), so
  264 vs 19 = 13.9x.
- Alias split: the 30 S-alias hits come from literals in slot+`0x9000`..
  `0x23000`, and the 236 NS-alias hits from literals in slot+`0x5A000`..
  `0xD6000`; see the open questions.

Vector table (slot+`0x400` = `0x100B_0400` S), first 16 words:

| # | name | value | slot offset |
|---|---|---|---|
| 0 | SP | `0x3000_07F8` | |
| 1 | Reset | `0x100B_1ADD` | `+0x1ADC` |
| 2 | NMI | `0x100B_15C1` | `+0x15C0` |
| 3 | HardFault | `0x100B_B685` | `+0xB684` |
| 4 | MemManage | `0x100B_B8D5` | `+0xB8D4` |
| 5 | BusFault | `0x100B_AE15` | `+0xAE14` |
| 6 | UsageFault | `0x100B_D0D5` | `+0xD0D4` |
| 7 | SecureFault | `0x100B_CDE1` | `+0xCDE0` |
| 8–10 | reserved | 0 | |
| 11 | SVCall | `0x100B_1F09` | `+0x1F08` |
| 12 | DebugMonitor | `0x100B_0EC5` | `+0x0EC4` |
| 13 | reserved | 0 | |
| 14 | PendSV | `0x100B_1955` | `+0x1954` |
| 15 | SysTick | `0x100B_1FA9` | `+0x1FA8` |

At this base each of the 6 fault and SVCall handlers starts with
`mrs r0, MSP`.

Reset handler, first 8 instructions:

```
    100b1adc:  cpsid i
    100b1ade:  movw r0, #0
    100b1ae2:  movt r0, #0x3000
    100b1ae6:  msr MSPLIM, r0
    100b1aea:  bl 0x100b1fe0
    100b1aee:  bl 0x100b93c0
    100b1af2:  movs r0, r0
    100b1af4:  push {r7, lr}
```

The handler masks interrupts, sets the main stack limit to the bottom of
secure SRAM, and calls two functions. The second presumably does not return:
the next instruction is a padding halfword and then the next function's
prologue.

### Image 1 (Non-secure): `0x0038_0000`

Criterion 1 is not scored. 10968 strings.

```
  rank  base         total  vectors NS/S  strings NS/S  distinct  BL->PUSH
     1  0x00380000  16504             -       14370/0      6824      2134
     2  0x00378000   3427             -        1293/0       421      2134
     3  0x0030d000   3411             -      159/1118       111      2134
     4  0x002eb000   3291             -       41/1116        36      2134
     5  0x00350000   3285             -        1150/1       334      2134
image 1: ACCEPT 0x00380000: string hits 14370 vs 1293 at runner-up 0x00378000 (margin 11.1x, need 2x); 6824 different strings vs at most 445 elsewhere; vector entries 0; total 16504
```

- Winner: 14370 hits naming 6824 strings, all through the NS alias. The
  margin is 11.1x on hits and 15.3x on distinct strings (next best is
  `0x0037_A000` with 445).
- The runners-up are inflated by single repeated constants:
  - `0x0037_8000` and `0x0035_0000`: the value `0x0056_0D00` appears 444
    times. At the winning base it is an address inside image 1.
  - `0x0030_D000` and `0x002E_B000`: the S-alias value `0x1050_F64C` appears
    1115 times.
  - The rest are near neighbours where a shifted pointer into the dense
    string area lands on another string start.

First words at slot+`0x200`: SP `0x2009_3000`, reset `0x0044_BC91`
(slot+`0xCBC90`). Words 2–15 are not handler addresses (they are even, or
odd but outside the slot), so image 1's real NS vector table is elsewhere.

Reset vector target, first 8 instructions:

```
    0044bc90:  ldr.w r1, [pc, #-8]
    0044bc94:  str r0, [r1]
    0044bc96:  bl 0x380aee
    0044bc9a:  ldr r0, [pc, #20]
    0044bc9c:  bx r0
    0044bc9e:  ldr.w r12, [pc, #-20]
    0044bca2:  ldr.w r12, [r12]
    0044bca6:  ldr.w r12, [r12, r0, lsl #2]
```

This is an entry trampoline: it stores `r0` to a global, calls into early
image-1 code (slot+`0xAEE`), then branches to an address loaded from a
literal. The instructions from `0044bc9e` on belong to the next routine.

## Flash coverage

- `Layout::place` copies header, payload and TLVs. Flash must therefore cover
  offsets up to **`0x0067_495C`** (the end of image 9 with its TLVs). All ten
  images fit in the first 8 MiB, below the mailbox at `0x00CF_0000`
  (`real_bundle_fits_8_mib_flash_with_evo_7_1` checks this, including the
  range of images 2..9); the flash itself is 16 MiB.
- Occupied: `0x000B_0000..0x0018_B4F3` (image 0),
  `0x0038_0000..0x0061_3A99` (image 1) and `0x0061_A6D8..0x0067_495C`
  (images 2..9, at their package distance from image 1).
- Used at run time: PS/ITS at `0x0009_0000..0x000A_FFFF` and the mailbox at
  `0x00CF_0000` (see "Memory").
- Unknown: the rest of `0x0000_0000..0x000B_0000`,
  `0x0018_B4F3..0x0038_0000` (about 1.96 MiB), the gap
  `0x0061_3A99..0x0061_A6D8` (between the package's images 1 and 2) and
  everything above `0x0067_495C` except the mailbox.

## Open questions

1. **What lives in `0x0..0xB_0000`?** Presumably BL1/BL2 (MCUboot) and
   provisioning or config data, none of which is in the package. The
   emulator replaces BL2 (spec §5.1). Until the OS reads this range, it is
   left as erased flash.
2. **What lives in `0x0018_B4F3..0x0038_0000`?** Image 0 uses the constant
   `0x0018_F2C0` 118 times, which is just past its own end.
   **Answer (Plan 2 research):** not TF-M storage. ITS/PS live at
   `0x0009_0000..0x000A_FFFF`, below the secure slot, and the research runs
   through the BSP idle loop saw no erase, program or blank check in this gap.
   It is left as erased flash. What `0x0018_F2C0` names is still open.
3. **Where do images 2–9 go, and what are they?** The package does not say
   and this scorer was not run on them. Their headers are `0x40`.
   **Answer (addresses, Task 24):** at `ns_slot_base` plus their package
   distance from image 1, `0x0061_A6D8..0x0067_495C` for the 7.1 bundle. The
   OS image's protected TLVs 0xB1..0xB8 give their payload addresses, and the
   BSP's validation of image 1 requires an MCUboot header `0x40` bytes before
   each (`boot-flow.md` §3.3). `Layout::place` places them there. What the
   images are (their roles) is still open.
4. **Does image 0 contain a part that runs Non-secure?** Its S-alias string
   references sit in its first `0x23000` bytes. Its NS-alias references sit
   in slot+`0x5A000..0xD6000`, and the strings they name include USB-stack
   identifiers. Either the upper part of image 0 runs NS, or secure code reads that
   data through the NS alias.
   **Answer:** yes. SAU region 0 makes flash `0x0010_0000..0x00FF_FFFF`
   Non-secure, and image 0 from slot+`0x50000` on (NS `0x0010_0000..0x0018_B4F3`)
   is the **NS BSP**: RTOS, MCCI USB stack, drivers and factory tests. Secure
   boot hands off to it, not to image 1 (`boot-flow.md` §2).
5. **Where is image 1's NS vector table?**
   **Answer:** image 1 has none of its own. Secure firmware writes
   `VTOR_NS = 0x0010_0400`, the BSP's vector table (SP `0x2004_6D20`, reset
   `0x0011_6D51`). The BSP later calls image 1's reset word (`0x0044_BC91`)
   with `blx` from `0x0015_2716`, in the same NS state, without changing
   VTOR (`boot-flow.md` §3.4).
6. **How big is the physical flash?** Image 0 holds strings naming 4M, 8M and
   16M parts (QSPI device identification), so the size is detected at run
   time.
   **Answer: 16 MiB.** The flash MPC covers `0x0000_0000..0x00FF_FFFF`, and
   the SPIF probe forces the capacity code to 24, so the driver always uses
   the 16M geometry (`spif-nor.md` §6.1). With 8 MiB the mailbox at
   `0x00CF_0000` reads 0 and triggers a spurious erase.
7. The search assumed 4 KiB-aligned slots (`step 0x1000`) inside the first
   8 MiB. Both winners are 64 KiB-aligned, and nothing in the range competes.

## Peripherals

The devices `evo_hw::soc::attach` registers, in its order. Each window is mapped at the
address the firmware uses (the S alias for 0x5xxx_xxxx blocks, the NS alias otherwise). IRQ
lines are level-sensitive; "stub" means both vector tables point that IRQ at the fatal
"unexpected interrupt" routine, so the model never asserts it. "Accesses seen" lists the
offsets the firmware touches in the research runs (boot through the BSP idle loop, with
register fakes); a count is the number of accesses that run's unmapped log attributed to a
block nothing modelled yet. "BSP" marks blocks only the NS BSP touches.

| name | base | size | IRQ | doc | accesses seen |
|---|---|---|---|---|---|
| `mpc_sram0` | `0x5020_4000` | `0x1000` | 7 (stub) | `security.md` §1 | CTRL, BLK_MAX, BLK_CFG, BLK_IDX, BLK_LUT, INT_EN |
| `mpc_sram1` | `0x5020_5000` | `0x1000` | 7 (stub) | `security.md` §1 | as `mpc_sram0` |
| `mpc_sram2` | `0x5020_6000` | `0x1000` | 7 (stub) | `security.md` §1 | as `mpc_sram0` |
| `mpc_flash` | `0x5020_8000` | `0x1000` | 7 (stub) | `security.md` §1 | as `mpc_sram0`; BLK_CFG/BLK_MAX are the reads behind the first-boot reset loop (`discovery-log.md`) |
| `cru` | `0x5020_0000` | `0x1000` | none (21, 25 stubs) | `security.md` §3 | +00, +04, +10, +14, +18, +1C, +20, +4C, +50, +FC at boot; +08, +0C, +60, +70, +94 at run time |
| `spctrl` | `0x5020_1000` | `0x1000` | 8 (stub) | `security.md` §2 | +10, +14, +24, +28, +60, +64, +70, +74, +80 at boot; +C0, +D4 at run time |
| `ledpat` | `0x5020_2B00` | `0x100` | none | `security.md` §6 | +00..+10, 6 writes per boot |
| `icache` | `0x5000_5000` | `0x1000` | 10 (stub) | `spif-nor.md` §7 | +04 (RMW `\|= 0x1D`, 1 + 118 per boot) |
| `timer` | `0x4010_1000` | `0x1000` | 1–3 (never enabled) | `timer.md` | +00, +04, +10, +14, +30, +34, +38 |
| `uart0` | `0x4010_6000` | `0x1000` | 17 (stub) | `uart.md` | +00, +04, +08, +0C, +14 |
| `uart2` | `0x4010_B000` | `0x1000` | 27 | `uart.md` | +00..+14 when the link transport opens it (not on a normal boot) |
| `spi` | `0x4010_7000` | `0x1000` | 19 or 20 (stubs) | `spi-panel.md` | +00, +04, +08, +0C, +18, +1C, +64 |
| `spif` | `0x5000_1000` | `0x1000` | 22 (stub) | `spif-nor.md` | +00..+0C, +10, +18, +20, +24, +30, +34, +54, +100 |
| `trng` | `0x5010_A000` | `0x1000` | 26 (stub) | `crypto.md` | +100..+138 |
| `sha` | `0x5000_2000` | `0x1000` | 23 (stub) | `crypto.md` | +00, +10..+4C, +60..+7C |
| `usb` | `0x4000_3000` | `0x1000` | 16 | `usb.md` | +000, +88, +C4, +E0, +E4, +100.., +1A0..+1BC, +32C.. (BSP) |
| `usbtc` | `0x4010_C000` | `0x1000` | 28 | `usb.md` | +00..+2F (byte/halfword), +1C, +84, +1A4; 34 accesses (BSP) |
| `gpio` | `0x4010_5000` | `0x1000` | 24 | `gpio.md` | +00, +04, +08, +10, +18, +20, +30, +34, +38, +3C; 29 accesses |
| `lcdc` | `0x4000_4000` | `0x1000` | 14 | `lcdc.md` | +00..+1C, +28; 25 accesses |
| `pwm` | `0x4010_4000` | `0x1000` | none | `lcdc.md` | +00, +04; 4 accesses |
| `keypad` | `0x4010_3000` | `0x1000` | 13 (ON key: 0) | `keypad.md` | +00..+0C; 6 accesses (BSP) |
| `rtc` | `0x4010_2000` | `0x1000` | 15 | `rtc.md` | +20, +40; 6 accesses (BSP) |

Accesses outside these windows that stay unmodelled on purpose:

- **PPB `0xE004_3018`** (`|= 2`, once per boot): a vendor register in the external PPB,
  RAZ/WI (`security.md` §4). The CPU passes it to the bus so it shows in the unmapped log.
- **`0x1800_E000..`** (byte and word reads from `0x1800_E064` up, 20 in the research run):
  the unprovisioned key source; it reads 0 (`security.md` §2, +0xC0).

## Device runtime decisions

The research found eight gaps that every block shares. Each is decided once, in the device
runtime (`crates/evo-hw/src/device.rs`, Plan 2 Tasks 3–4) or the CLI:

- **X1, time.** A device sees the cycle count as `DevCtx::now` (`MemoryMap::now`, set from
  `Scheduler::now` by `Machine`), and asks for a callback with `DevCtx::schedule_at`
  (Task 3).
- **X2, clock rate.** `evo_hw::CPU_HZ = 156_000_000`; one `Scheduler::now` unit is one CPU
  cycle. Devices derive every rate from it (Task 3).
- **X3, IRQ output.** A device drives its lines with `DevCtx::set_irq` (level-sensitive);
  `Machine` folds the lines into the NVIC after every step and event. The only edge source
  is the ON key, `DevCtx::pulse_irq` (Tasks 3–4).
- **X4/X5, shared flash.** The flash bytes stay a `MemoryMap` buffer named `"flash"`; the
  SPIF changes them through `DevCtx::buffer_mut("flash")`, so XIP reads see every erase and
  program at once (no `Rc<RefCell>`, no locking) (Task 3).
- **X6, flash size.** 16 MiB, erased to 0xFF, at both aliases (Task 4).
- **X7, resets.** `Device::reset(ResetKind)` with `PowerOn` and `System`; each device's doc
  says what survives a System reset, and `Machine` rewrites the BL2 header on every reset
  (Tasks 3–4).
- **X8, persistence.** The flash bytes and the NOR security registers persist in a flash
  file, `evo-cli run --flash-file` (Task 13); save states do not carry them.

## Reproduce

Set `EVO_OS_IMAGE` to your own bundle, then:

```
cargo run --release -p evo-cli -- inspect "$EVO_OS_IMAGE" --score-bases
cargo test -p evo-cli -p evo-image real_bundle
```
