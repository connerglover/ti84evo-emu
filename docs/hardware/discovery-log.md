# TI-84 Evo discovery log: first boot of the real OS

Status: result of Plan 1, Task 17. Input for Plan 2 (peripheral modeling). OS bundle
`7.1.0.4421`, `Layout::EVO_7_1` (see `memory-map.md`). Everything about peripherals
here is inferred from the secure firmware's behaviour and is a first guess unless stated
otherwise. No TI code is reproduced beyond the short disassembly excerpts below.

## Summary

- The secure image (TF-M v2.1.1, banner `Booting TF-M v2.1.1_co2_v1.00`) boots, runs its
  startup code and reaches TF-M `main`. It never prints anything.
- **Steady state: a reset loop.** Every boot takes about 33,100 cycles and ends in
  `NVIC_SystemReset`: 1,510 resets in 50M cycles. The loop is stable; the CPU never locks up.
- **Root cause.** `tfm_hal_set_up_static_boundaries()` fails because `mpc_init_cfg()` fails.
  The Arm SIE-200-style MPC driver reads the flash MPC's `BLK_CFG` (0x5020_8014) and
  `BLK_MAX` (0x5020_8010). Both are unmapped and read 0, so the MPC seems to control a single
  32-byte block. The first `ConfigRegion(0x0010_0000, 0x00FF_FFFF, NS)` call fails with
  "block index too high", `main` calls `tfm_core_panic()`, and that resets the system.
- **CPU bug found and fixed.** `STRD` with `Rt == Rt2` was decoded as UNDEFINED (commit
  `fix(armv8m): STRD with Rt == Rt2 is valid`). The firmware's memset uses
  `strd r2, r2, [ip]`. The bug only showed up once boot got past the MPC, in an exploration
  run (see "Beyond the reset loop").
- **TF-M log UART: 0x4010_6000.** It is a 16550-style UART at 115200 baud, used through its
  Non-secure alias. Boot never reaches it today.
- **Minimal Plan 2 set for the banner, verified with throwaway fakes** (see "Plan 2
  starting points"):
  - the four MPCs;
  - TIMER0 at 0x4010_1000;
  - the SPI master at 0x4010_7000;
  - the UART.

  With these the banner and the initialization of all secure partitions print. The next
  blocker is the QSPI flash controller (0x5000_1000), which ITS needs.
- **SoC identification: no public MCU matches.** The SoC looks like a custom ASIC built from
  Arm's SSE-200-style secure infrastructure (SIE-200 MPCs, an SPCTRL-like block) plus
  third-party IP.

## Corrections from Plan 2 research

The Plan 2 research (the per-block docs in this folder) corrects these first guesses. The
log below is kept as it was written.

- **The SPI master uses 9-bit frames, not 16.** 3-wire SPI to the panel's ST7789V-class
  driver IC, bit 8 = D/C, mode 3, 1 MHz (`spi-panel.md`). CTRL1 = 0x0008_0012.
- **CRU +0x10 bit 30 is a chip-variant bit, not a status flag.** It selects the revision 9
  vs 10 path; nothing waits on it (`security.md` §3).
- **0x4010_5000 is the GPIO controller** (13 pins, IRQ 24), not a display or DMA helper
  (`gpio.md`). Its configuration word (0x100B_7BC0) merely sits next to the LCDC's
  (0x100B_7BC4); the secure `panel_on` does use GPIO pin 2 as the panel reset.
- **The 0x00CF_0000 reads were an artifact of the 8 MiB flash.** They are a blank check of a
  one-shot mailbox: with the space unmapped it read 0, looked "not blank", and the
  `Restored Saved Data.` path ran. With 16 MiB of erased flash it reads 0xFF and nothing
  happens (`spif-nor.md` §6.3).
- **ITS/PS live at 0x0009_0000–0x000A_FFFF** (PS 0x0009_0000–0x000A_BFFF, ITS
  0x000A_C000–0x000A_FFFF), below the secure slot, written through the SPIF and read
  through the XIP window (`boot-flow.md` §2). The answer to `memory-map.md` Q2 at the end of
  this log is superseded accordingly.
- Also: 0x4010_4000 is the backlight PWM, whose reset is CRU +0x20 bit 4 (`lcdc.md`); flash
  is 16 MiB and SRAM 1.5 MiB (`memory-map.md`); VTOR_NS is 0x0010_0400, the NS BSP's vector
  table (`boot-flow.md` §2).

## Unmapped accesses (first boot)

`evo-cli run "$EVO_OS_IMAGE" --max-cycles 50000000 --unmapped-out <file>`:

```
stop: cycle-limit pc=0x100b9482 cycles=50000003
resets: 1510 (system resets the guest requested through AIRCR.SYSRESETREQ)
unmapped: 29 distinct accesses logged
```

The run ends inside the 1,511th boot. The last three rows come after the boot's
startup code, so their count is one lower.

| addr | width | kind | value | pc | sec | count |
|---|---|---|---|---|---|---|
| 0x50200020 | 4 | read | - | 0x100b5f00 | S | 1511 |
| 0x50200020 | 4 | write | 0x001fffef | 0x100b5f06 | S | 1511 |
| 0x50200014 | 4 | read | - | 0x100b5f08 | S | 1511 |
| 0x50200014 | 4 | write | 0x00000400 | 0x100b5f0e | S | 1511 |
| 0x50200000 | 4 | read | - | 0x100b5f10 | S | 1511 |
| 0x50200000 | 4 | write | 0x00000040 | 0x100b5f16 | S | 1511 |
| 0x50200000 | 4 | read | - | 0x100b5f18 | S | 1511 |
| 0x50200000 | 4 | write | 0x00000080 | 0x100b5f1e | S | 1511 |
| 0x5020004c | 4 | read | - | 0x100b5f30 | S | 1511 |
| 0x5020004c | 4 | write | 0x00004000 | 0x100b5f36 | S | 1511 |
| 0x5020004c | 4 | read | - | 0x100b5f38 | S | 1511 |
| 0x5020004c | 4 | write | 0x00000004 | 0x100b5f3e | S | 1511 |
| 0x50200050 | 4 | read | - | 0x100b5f40 | S | 1511 |
| 0x50200050 | 4 | write | 0x00000001 | 0x100b5f46 | S | 1511 |
| 0x50200050 | 4 | read | - | 0x100b5f48 | S | 1511 |
| 0x50200050 | 4 | write | 0x00000100 | 0x100b5f4e | S | 1511 |
| 0x50200010 | 4 | read | - | 0x100b5f50 | S | 1511 |
| 0x50005004 | 4 | read | - | 0x100b2000 | S | 1511 |
| 0x50005004 | 4 | write | 0x0000001d | 0x100b2006 | S | 1511 |
| 0x50200004 | 4 | read | - | 0x100b5bbc | S | 1511 |
| 0x502000fc | 4 | read | - | 0x100b5bbe | S | 1511 |
| 0x50200004 | 4 | read | - | 0x100b5c54 | S | 1511 |
| 0x50200004 | 4 | read | - | 0x100b5c58 | S | 1511 |
| 0x502000fc | 4 | read | - | 0x100b5c5a | S | 1511 |
| 0x502000fc | 4 | read | - | 0x100b5ccc | S | 1511 |
| 0x50201014 | 4 | read | - | 0x100b46d6 | S | 1510 |
| 0x50201014 | 4 | write | 0x00000001 | 0x100b46dc | S | 1510 |
| 0x50208014 | 4 | read | - | 0x100b2e12 | S | 1510 |
| 0x50208010 | 4 | read | - | 0x100b2e64 | S | 1510 |

One access does not appear in this table. `mpc_init_cfg` also writes the PPB address
0xE004_3018 (`|= 2`), but only after the failing call, so the first boot never makes it. The
CPU handles the whole PPB itself, so such an access is never logged here. An unimplemented
PPB register reads as zero and ignores writes, with a `log::warn!` that `evo-cli` does not
print (it installs no logger). See the concerns at the end.

## The boot, step by step (one pass of the loop)

| cycles | PC | what (TF-M / CMSIS name, inferred) |
|---|---|---|
| 0 | 0x100b1adc | Reset handler: `cpsid i`; MSPLIM = 0x3000_0000 |
| 6 | 0x100b1fe0 | `SystemInit`: VTOR = 0x100b0400; clock setup on 0x5020_0000 (rows 1-17); 0x5000_5004 \|= 0x1d; `SystemCoreClock` from 0x5020_0004/0x5020_00fc (rows 20-25) |
| 129 | 0x100b93c0 | C library startup: scatter-load (`.data` copy, `.bss` zero), about 32,600 cycles |
| 32,755 | 0x100b349c | TF-M `main` |
| 32,769 | 0x100b4650 | `sau_and_idau_cfg()`: 4 SAU regions, then 0x5020_1014 \|= 1 |
| 32,820 | 0x100b38f4 | `mpc_init_cfg()`: flash MPC `Initialize`, then `ConfigRegion` fails |
| 33,086 | 0x100b34b6 | `cbnz r0` in `main`: the failed check |
| 33,088 | 0x100b618c | `tfm_core_panic()` → 0x100b64d4 → `NVIC_SystemReset` (0x100b2628) |
| 33,102 | 0x100b2642 | write AIRCR = 0x05FA_0004 \| PRIGROUP: system reset; the next boot starts at cycle 33,103 |

Notes on this table:

- `SystemInit`'s clock values do not survive: scatter-load re-initializes `SystemCoreClock`
  (0x3000_19c0..0x3000_19c8) to its link-time values. At `main` these are 156,000,000,
  39,000,000 and 24,000,000 Hz.
- SAU regions programmed by `sau_and_idau_cfg()` (they answer open question 4 in
  `memory-map.md`):

  | # | range | attribute |
  |---|---|---|
  | 0 | 0x0010_0000-0x00FF_FFFF | NS (flash above 1 MiB, so image 0 from slot+0x50000 on is Non-secure) |
  | 1 | 0x2001_0000-0x2017_FFFF | NS (SRAM) |
  | 2 | 0x100B_04C0-0x100B_051F | NSC (the SG veneers) |
  | 3 | 0x4000_0000-0x4FFF_FFFF | NS (peripherals) |

## Where execution settles: the failed check

TF-M `main` (0x100b349c), first 8 instructions (emulator disassembly):

```
100b349c: push {r7, lr}
100b349e: movw r0, #0
100b34a2: movt r0, #0x3000
100b34a6: msr MSPLIM, r0            ; tfm_arch_set_msplim(SPM boot stack bottom)
100b34aa: movw r0, #0x5174
100b34ae: movt r0, #0x3000
100b34b2: bl 0x100b64b0             ; tfm_hal_set_up_static_boundaries(&spm_boundary)
100b34b6: cbnz r0, 0x100b34cc       ; != 0 -> bl tfm_core_panic (0x100b618c)
```

`tfm_hal_set_up_static_boundaries` (0x100b64b0). Its shape matches TF-M's Arm-platform
`tfm_hal_isolation.c`: SAU, then MPC, then PPC (`ppc_init_cfg` at 0x100b4278 is never
reached).

```
100b64b0: push {r4, lr}
100b64b2: mov r4, r0
100b64b4: bl 0x100b4650             ; sau_and_idau_cfg()
100b64b8: bl 0x100b38f4             ; mpc_init_cfg() -> returns -10 here
100b64bc: cmp r0, #0
100b64be: itt ne
100b64c0: movw r0, #0x3bda          ; HAL error code returned to main
100b64c4: pop {r4, pc}
```

The panic path ends in CMSIS `NVIC_SystemReset` (0x100b2628; r0 = 0xE000_ED0C, AIRCR):

```
100b2634: ldr r1, [r0]
100b2636: movs r2, #4
100b2638: and.w r1, r1, #0x700      ; keep PRIGROUP
100b263c: movt r2, #0x5fa           ; VECTKEY | SYSRESETREQ
100b2640: orrs r1, r2
100b2642: str r1, [r0]
```

## Root cause of the failed check

`mpc_init_cfg()` (0x100b38f4) is TF-M's Arm-platform MPC setup on top of CMSIS `Driver_MPC`.
The driver underneath is Arm's `mpc_sie200_drv.c`: `mpc_sie200_init` only stores the range
list, so it cannot fail, and `mpc_sie200_config_region` does the work. In order,
`mpc_init_cfg` does this:

1. Flash MPC (driver 0x100b7b28, device registers at **0x5020_8000**): `Initialize()`
   succeeds. Then `ConfigRegion(0x0010_0000, 0x00FF_FFFF, ARM_MPC_ATTR_NONSECURE)` **fails**.
2. Not reached: SRAM MPC at 0x5020_4000, `ConfigRegion(0x2001_0000, 0x2007_FFFF, NS)`.
3. Not reached: SRAM MPC at 0x5020_5000, `ConfigRegion(0x2008_0000, 0x200F_FFFF, NS)`.
4. Not reached: SRAM MPC at 0x5020_6000, `ConfigRegion(0x2010_0000, 0x2017_FFFF, NS)`.
5. Not reached:
   - 0xE004_3018 `|= 2`;
   - 0x5020_1080 `|= 1`;
   - `LockDown()` on all four MPCs;
   - `dsb; isb`.

Each MPC is configured with two ranges, an NS alias and an S alias, both at offset 0. These
come from the device configurations in flash:

| MPC | NS range | S range |
|---|---|---|
| 0x5020_4000 | 0x2000_0000-0x2007_FFFF | 0x3000_0000-0x3007_FFFF |
| 0x5020_5000 | 0x2008_0000-0x200F_FFFF | 0x3008_0000-0x300F_FFFF |
| 0x5020_6000 | 0x2010_0000-0x2017_FFFF | 0x3010_0000-0x3017_FFFF |
| 0x5020_8000 | 0x0000_0000-0x00FF_FFFF | 0x1000_0000-0x10FF_FFFF |

So SRAM is 3 × 512 KiB = 1.5 MiB and flash is 16 MiB.

The failing step is in the driver's `get_lut_masks` (0x100b2cf4). The register map is
SIE-200: CTRL +0x00, BLK_MAX +0x10, BLK_CFG +0x14, BLK_IDX +0x18, BLK_LUT +0x1C,
INT_STAT/CLEAR/EN/INFO1/INFO2/SET +0x20..+0x34.

```
100b2e12: ldr r0, [r5, #20]         ; BLK_CFG (0x5020_8014): unmapped, reads 0
100b2e14: adds r1, r0, #5           ; block size = 1 << (BLK_CFG + 5) = 32 bytes
```

```
100b2e64: ldr r6, [r5, #16]         ; BLK_MAX (0x5020_8010): unmapped, reads 0
100b2e66: movs r0, #255
100b2e68: cmp r1, r6                ; limit LUT word index vs BLK_MAX
100b2e6a: it ls
100b2e6c: cmp r2, r6                ; base LUT word index vs BLK_MAX
100b2e6e: bhi 0x100b2df6            ; -> return (int8_t)0xFF = -1 (index too high)
```

- **What it read and from where.** BLK_CFG = 0 at 0x5020_8014 and BLK_MAX = 0 at
  0x5020_8010. Both are unmapped reads (the last two rows of the table).
- **What it expected.** A block geometry under which the range fits in the LUT.
  - With 32-byte blocks, 0x0010_0000..0x00FF_FFFF covers LUT words 0x400..0x3FFF.
  - With BLK_MAX = 0 only word 0 exists, so the driver returns its internal "out of range"
    error -1.
  - `mpc_sie200_config_region` maps -1 to 5, CMSIS `ConfigRegion` maps 5 to -10 (table at
    0x100b793c), and `tfm_hal_set_up_static_boundaries` turns that into 0x3BDA.
- **Values that work.**
  - Any consistent pair works. The exploration below used 4 KiB blocks (BLK_CFG = 7) with
    BLK_MAX = 127 for flash and 3 for each 512 KiB SRAM bank.
  - The base and limit of every `ConfigRegion` are 64 KiB-aligned, so any block size up to
    64 KiB is accepted.
- **Assert or log text nearby.** None is printed. TF-M's `main` would print the banner
  (`\033[1;34mBooting TF-M v2.1.1_co2_v1.00\033[0m`, string at 0x100b350d) only after
  `tfm_hal_platform_init()` (0x100b645c). That function also sets up the UART
  (`stdio_init`), and the failure comes before both.

## The TF-M log UART

`stdio_init` (0x100b561c, called from `tfm_hal_platform_init`) drives CMSIS `Driver_USART`
0x100b7b88:

- `Initialize`;
- `PowerControl(FULL)`;
- `Control(ASYNC, 115200)`;
- `Control(TX, 1)`.

Its device configuration is {base **0x4010_6000**, default baud 115200}. The low-level
driver (0x100b6fc8..0x100b70f2) treats it as a **16550-style UART with 4-byte register
stride**:

| offset | register | use seen |
|---|---|---|
| +0x00 | RBR / THR, DLL when DLAB | divisor low = 13 (24 MHz / (16 × 115200)); TX data |
| +0x04 | IER, DLH when DLAB | divisor high = 0 |
| +0x08 | FCR | 7 (enable and reset the FIFOs) |
| +0x0C | LCR | DLAB (bit 7) set/clear, 8N1 (`\|= 3`, clear bits 2-3) |
| +0x14 | LSR | bit 0 DR (RX ready), bit 5 THRE (TX ready) |

Without a model, `Send()` spins forever on LSR.THRE: a polling loop at 0x100b0c9c that calls
0x100b70c8. The secure code uses the Non-secure alias 0x4010_6000 (SAU region 3 is NS), so
the S alias would be 0x5010_6000. IRQ 17 is `UART_Handler`. None of 0x5020_xxxx is a UART:
that block is clock, security and MPC control.

## Beyond the reset loop (exploration only, nothing modeled)

To see what Plan 2 will meet next, the boot was pushed further in two ways:

- GDB (`evo-cli run --gdb`) changed the BLK_CFG/BLK_MAX values in registers.
- A throwaway emulator build with register fakes on `Machine::bus`.

Neither is committed. In order, the boot met these blockers:

1. **CPU bug: `strd r2, r2, [ip]` was UNDEFINED.** UsageFault UNDEFINSTR at 0x100c8bbe, in
   memset. TF-M's fault handler then logs `FATAL ERROR: UsageFault` and the log hangs on the
   UART. It is fixed in armv8m: only LDRD forbids `Rt == Rt2` (DDI0553). There is a
   regression test, and the Unicorn difftest now generates the case.
2. **SPI-flash Read-ID wait** (with the MPC geometry faked). `tfm_hal_platform_init`
   performs these steps:
   - it initializes the 3-channel timer at 0x4010_1000, the SPI-flash controller ("SPIF",
     0x5000_1000) and the UART;
   - it issues JEDEC Read-ID: command word 0x9F00_0000 to 0x5000_100c, start at 0x5000_1020;
   - it waits for 0x5000_1018 bit 1 in a wait-with-timeout helper (0x100b6e7c).

   The helper's clock is TIMER0's counter at 0x4010_1000, read as a down-counter at
   32.768 kHz (timeout_ms × 32768 / 1000 ticks). That counter reads 0 forever, so the wait
   never times out.
3. **SPI master transfer** (with TIMER0 ticking, so the Read-ID wait times out after 3,276
   polls). Platform init then configures the SPI master at **0x4010_7000** (1 MHz, 16-bit
   frames) and transfers. Its loop has no timeout:
   - it polls status +0x0C: bit 1 is TX not full, bits 9:4 are the RX level;
   - data goes through +0x18.
4. **Banner.** With a loopback SPI fake and a UART fake (LSR = THRE | TEMT), TF-M prints:

   ```
   Booting TF-M v2.1.1_co2_v1.00
   [Sec Thread] Secure image initializing!
   TF-M isolation level is:  0x00000001
   Update / TRNG / Power / ... / Data / Boot / PTT Partition initializing
   [INF][PS] Encryption alg: 0x5500100
   Creating an empty ITS flash layout.
   ```

   Then comes an SPM panic and a reset, about 4.0M cycles per boot.
   - The panic is at 0x100b6c0e: a secure partition replied `PSA_ERROR_PROGRAMMER_ERROR`
     (-129) to a secure client.
   - ITS cannot create its layout. Read-ID never succeeded, so the flash driver never came
     up, and no erase or program command is ever issued.
   - The ITS/PS area is read through the flash window at **0x00CF_0000-0x00CF_23FF**, which
     is past the emulator's 8 MiB flash.
   - Mapping 16 MiB of erased flash (0xFF) changes one thing only: the Data partition's
     `Restored Saved Data.` line disappears. With unmapped space reading as zero, that
     partition had taken the zeros for saved data. The panic stays.

Also seen in that run:

- **LCD controller at 0x4000_4000.** Its timing words use 239 and 319 (320x240), and its
  framebuffer address is 0x2004_8000.
- **0x4010_4000** is written 0xA0, 0xFF. It might be a backlight or PWM.
- **0x4010_5000** (+0x00..+0x18) shares a configuration struct with the LCDC, and it is the
  device the IRQ 24 handler services.

## Grouped first guesses per address block

Secure peripherals are at 0x5xxx_xxxx and Non-secure ones at 0x4xxx_xxxx (the same
peripheral 0x1000_0000 lower). IRQ numbers come from the secure vector table at 0x100b0400.
Each handler stub loads its own name string (`adr r0, "<name>_Handler"`) and calls a common
"unexpected interrupt" routine at 0x100b0e84, so the stubs name the IRQs.

| block | guess | evidence |
|---|---|---|
| 0x5020_0000 | System and clock controller (CRU) | Touched first, in `SystemInit` (read-modify-write of +0x00/+0x14/+0x20/+0x4C/+0x50). Clock dividers are in +0x04 bits 10:8 and 18:16, and a PLL in +0x1C. +0x10 bit 30 is a status flag: when clear, `SystemInit` returns early. +0xFC looks like an ID register: an upper half of 0x0011 selects a 24 MHz reference path, otherwise 48 MHz. |
| 0x5020_1000 | Secure privilege control, SSE-200 SPCTRL layout | +0x14 `\|= 1` in `sau_and_idau_cfg` matches SSE-200 NSCCFG.CODENSC exactly. Exploration also saw +0x10 (SECRESPCFG), +0x24/+0x28 (SECPPCINTCLR/EN), +0x60/+0x64 (AHBNSPPCEXP0/1), +0x70/+0x74 (APBNSPPC0/1) and +0x80 (APBNSPPCEXP0), all at their SSE-200 offsets. |
| 0x5020_4000, 0x5020_5000, 0x5020_6000 | SIE-200 MPCs, SRAM banks 0-2 (512 KiB each) | Range table above; IRQ 7 is `MPC_Handler`. |
| 0x5020_8000 | SIE-200 MPC, 16 MiB flash/XIP window | BLK_CFG/BLK_MAX reads; this is the root cause. |
| 0x5000_1000 | QSPI NOR controller "SPIF" (IRQ 22) | Command words at +0x00..+0x0C (0x9F = JEDEC Read-ID), control at +0x20/+0x24, status at +0x18 (bit 1), XIP config at +0x30/+0x34. Strings "QSPI Clock Divider" and "QSPI Drive Settings". |
| 0x5000_5000 | Instruction cache / XIP cache controller (IRQ 10 `ICACHE_Handler`) | +0x04 `\|= 0x1D` in `SystemInit` and after every flash-driver operation, each time followed by `dsb; isb` (enable plus invalidate). |
| 0x4010_1000 | Timer block: 3 channels at +0x00/+0x10/+0x20, common registers at +0x30.. (IRQ 1-3) | IRQ 1-3 are real handlers. Each increments an overflow counter and clears its bit through +0x34. Channel 0 is the 32.768 kHz time base of every bounded wait. |
| 0x4010_6000 | UART, 16550-style (IRQ 17) | See above. |
| 0x4010_7000 | SPI master, 1 MHz, 16-bit frames | Configuration {0x4010_7000, 1,000,000}; transfer loop at 0x100b49c8. One of IRQ 19/20 (`SPI1`/`SPI2`). |
| 0x4000_4000 | LCD controller (IRQ 14) | 320x240 timing; framebuffer at 0x2004_8000. |
| 0x4010_5000 | Display-related helper, perhaps DMA (IRQ 24, unnamed) | Paired with the LCDC in configuration {0x4010_5000, 0x4000_4000}. The IRQ 24 handler (0x100b0f38) masks itself in NVIC_ICER0 and services this block. |
| 0x4010_4000 | Unknown: backlight, PWM or GPIO? | Written 0xA0, 0xFF during platform init. |
| PPB 0xE004_3018 | Implementation-defined PPB register | `\|= 2` in `mpc_init_cfg`. It is outside the SCS, so it reads 0 and writes are ignored (warning only). |

IRQ map (secure vector table, entries 16-44):

| IRQ | handler | IRQ | handler | IRQ | handler |
|---|---|---|---|---|---|
| 0 | OnKey | 10 | ICACHE | 20 | SPI2 |
| 1 | (timer ch 0, real handler) | 11 | SWTRIG | 21 | Wakeup |
| 2 | (timer ch 1, real handler) | 12 | SecureWatchdog | 22 | SPIF |
| 3 | (timer ch 2, real handler) | 13 | Keypad | 23 | SHA |
| 4 | APD | 14 | LCDC | 24 | (0x4010_5000, real handler) |
| 5 | PAPD1 | 15 | RTC | 25 | PLL2 |
| 6 | PAPD2 | 16 | OTG | 26 | TRNG |
| 7 | MPC | 17 | UART | 27 | UART2 |
| 8 | PPC | 18 | I2C | 28 | USBTC |
| 9 | MSC | 19 | SPI1 | 29-31 | 0 (unused) |

## SoC identification

**Candidates: none found.** Against known Cortex-M33 + TrustZone parts:

- **Arm SSE-200 platforms** (Musca-A/B1/S1, MPS2 AN521): the closest family.
  - Shared: the SIE-200 MPC programming model, an SPCTRL register layout identical at every
    offset seen, the 0x4/0x5 NS/S peripheral aliasing, and TF-M's Arm-platform HAL code.
  - Different: SSE-200 puts SPCTRL at 0x5008_0000 and the MPCs at 0x5008_3000+, uses
    CMSDK/PL011 UARTs (this one is 16550-style), and numbers its IRQs differently (IRQ 0 is
    the NS watchdog reset there).
- **Nordic nRF5340/nRF91.** They use the same 0x5xxx secure alias, but security is handled
  by the SPU, not SIE-200 MPCs, and their IRQ numbers follow peripheral addresses. Here they
  do not (the UART at 0x4010_6000 is IRQ 17).
- **NXP LPC55S6x, ST STM32L5/U5, Nuvoton M2351, Renesas RA4/RA6, TI CC27xx.** Each has its
  own security controller (AHB secure controller, GTZC, SCU, ...) and none has an SIE-200 MPC
  plus a 16550 UART at these addresses.

No SoC or vendor name appears in the image; it contains TI product strings only. The TF-M
version suffix `_co2_v1.00` is unexplained.

What is known about the SoC:

- Cortex-M33 with TrustZone, reference clock 24 MHz, `SystemCoreClock` 156 MHz.
- 1.5 MiB SRAM in three 512 KiB banks.
- External QSPI NOR flash, 16 MiB window.
- Calculator-specific blocks: Keypad, OnKey, APD/PAPD (automatic power-down), LCDC, RTC,
  Wakeup.
- USB OTG and Type-C (MCCI USB stack strings).
- SHA and TRNG.

That points to a custom ASIC that licenses Arm's SSE-200 secure infrastructure.

## Post-logo stall (Task 24)

**Result: resolved.** OS image 1 is entered at 0x0044_BC90 at cycle 656,271,393 of a fresh
power-on (`m4_image1_launches`). Five gates stood between the logo and the launch; four were
emulator bugs or missing board facts, one is the unit's factory state.

**Baseline.** With the Plan 2 devices the BSP no longer idles silently: by 253 M cycles it
draws a charging animation (`boot-flow.md` "M3 notes") and never reaches 0x0015_2BA8
("Validating OS...") or 0x0044_BC90 within 3,000 M cycles.

**What the BSP waits on (Step 2).** The BSP's control logic is three table-driven state
machines run by one engine (thread body 0x001664d0: wait on the machine's message queue,
look up `transitions[state][event]`, call the new state's handler). Each machine is a struct
in SRAM: name, event count, state count, the state table (flash), state and event name
tables, its thread and queue, current state at +0x44, previous at +0x48.

| machine | struct | states (index: name) |
|---|---|---|
| LAUNCH | 0x2001_02B8 | 0 Battery Check, 1 Initialize, 2 Battery Good, 3 Selftest, 4 OS, 5 OS Exit, 6 Low Battery, 7 Charging, 8 Waiting Install, ... |
| POWER | 0x2001_0318 | 0 Off, 1 Check State, 2 Running, 3 Charging, 4 Run Update, 5 Charge Update, 6 Polling |
| CHARGE | 0x2001_01B8 | USB charger states |

The research note "LAUNCH thread 0x0015_1A55, TCB 0x2004_4DC0" was a misreading:
0x2004_4DC0 is an RTX timer named "Animation" (750 ms period) whose callback 0x0015_1A54
posts event 6 to the LAUNCH queue while the animation flag is set. The LAUNCH *thread*
(entry 0x001664d1) was never stuck: it waits on its own queue (RTX state 0x83, waiting for
a message) because the machine had settled in state 7, Charging.

Event trace of the stall (LAUNCH events: 0 Initialize, 1 Selftest, 2 OS, 3 OS Missing,
4 OS Exit, 5 Install, 6 Return, 7 Battery Critical, 8 Battery Charging, 9 Battery Good):
LAUNCH 0 → 1 (BSP init), POWER "On" → Check State, POWER "Charge" → Charging, LAUNCH
"Return" → Battery Check, LAUNCH "Battery Charging" → 7. The event comes from POWER's
Check State handler (0x00152eb4).

**The gates, in the order the boot meets them** (a throwaway emulator build with register
fakes found each one; the fix is a model change with its own test):

1. **GPIO pin 9 (charger status) read low** → "Battery Charging". Check State reads pin 9
   (0x001147d4) and treats low as charger present. Fix: the board drives pin 9 high
   (`gpio.md`). Experiment 3 of the plan.
2. **GPIO pin 11 (battery comparator) read low** → "Battery Critical", state 6 Low Battery.
   Check State sets a 3-bit threshold on pins 5/7/12 and samples pin 11 at each level
   (0x00152f06..). Fix: the board drives pin 11 high (`gpio.md`). With 1 and 2 the machine
   reaches Battery Good and then posts Selftest (gate 5) or OS.
3. **Product ID.** In state OS the BSP calls secure service 0x4000_0107 type 0
   (0x00114108), which validates image 1 like MCUboot (hash TLV 0x10, RSA-3072 key/signature
   TLVs 0x01/0x23, security counter 0x50 against NV counter 3) plus a TI TLV 0xA0 = product
   ID 1, compared with the hardware product ID taken from CRU +0x10[29:25]. Status 1 made
   the BSP throw 0x13 (0x00105e44) → OS Exit → OS Missing → "Install OS & Apps". Fix: CRU
   strap = 1 (`security.md`).
4. **SHA-256 engine.** The image hash did not match: the model loaded the IV on CTRL bit 2
   set, the firmware means "continue" (`crypto.md`). Fix in `evo-hw`.
5. **Image placement.** With a valid image the service returned status 2: it walks image
   1's protected TLVs 0xB1..0xB8 (payload addresses of images 2..9) and requires an MCUboot
   header 0x40 bytes before each. `Layout::place` now places images 2.. at `ns_slot_base` +
   their package distance from image 1 (`evo-image`).
6. **Factory state (not a bug).** The BSP init (0x0015d488..0x0015d4a0) starts the
   diagnostic app instead of the OS when ITS UID 0x1F61E867_8CE70001 ("boot to OS") is
   missing or 0, or when UID ..0004 (diagnostic-menu request) is nonzero. A fresh emulated
   unit has an empty ITS, so it boots into the Self-Test app; its menu item "0. Exit this
   menu" launches the OS once (the next power-on returns to Self-Test). The firmware sets
   UID ..0001 itself after an OS install (probably: reset reason 8 in the retained record at
   0x2017_DDD0, 0x0014f786) or when all ten self-tests pass (0x00109520, which also writes
   UID ..0004 = 0); the Engineering menu's "Self Test" item toggles it (case 8 of
   0x0015dafc). `m4_image1_launches` uses the Exit item: ENTER at 2.5 s, 0 at 3.1 s.

**Hypotheses of the plan, and what became of them.**

| # | hypothesis | outcome |
|---|---|---|
| 1 | LCDC VComp, IRQ 14 runs once | refuted as the gate: the IRQ 14 handler (0x0010e2d0) runs 86 times in the first 300 M cycles (once per 16.7 ms frame from 78 M on), so the LCDC model already re-arms; the BSP was waiting for a battery state |
| 2 | flash offset 0 MCUboot header for service 0x106 type 0xD | not needed: still returns -132 and the OS launches; open |
| 3 | GPIO pins 9 and 11 high | **confirmed** (gates 1 and 2) |
| 4 | ON key / keypad 2 s after the logo | refuted as the gate (`wait=2000 on` and a 1.5 s ON hold leave the machine in Charging); a keypad press is needed later, for the Self-Test menu (gate 6) |
| 5 | USB Type-C attach | not tried: refuted indirectly, the launch needs no cable |
| 6 | RTC busy/status bits | not needed |

**Unmodelled accesses at the launch.** None new: `m4_image1_launches` logs no unmapped
access in the 500 M cycles after the OS entry, and UART0 prints nothing after
`Starting BSP 7.1.0.4413`. With correct digests the secure image's key-source reads moved
inside the unmapped OTP page 0x1800_E000 (16 bytes at 0x1800_E03C and at 0x1800_E0F4), so
the milestone tests now accept that whole 4 KiB page. By 7.9 s of guest time the screen shows the OS home screen (the
"CALCULATOR" app grid).

## Plan 2 starting points

To get past the failed check and print the TF-M banner, model these peripherals. The order
is the order boot needs them, and the set was verified with throwaway fakes, as described
above.

1. **Four SIE-200 MPCs** at 0x5020_4000, 0x5020_5000, 0x5020_6000 and 0x5020_8000. This
   ends the reset loop.
   - Registers: CTRL (lockdown), BLK_MAX and BLK_CFG consistent with the ranges above (for
     example, 4 KiB blocks: BLK_CFG = 7, BLK_MAX = 127 for flash and 3 for SRAM), BLK_IDX
     and BLK_LUT (LUT storage, auto-increment), and INT_STAT/CLEAR/EN/INFO.
   - Enforcing the LUT on bus accesses can wait.
   - Also give 0x5020_1000 (SPCTRL) and 0x5020_0000 (clocks) plain read/write storage so
     the PPC and clock read-modify-writes are kept. Zero reads are already accepted today.
2. **TIMER0 at 0x4010_1000**: a down-counter at 32.768 kHz, driven by the scheduler. Every
   bounded hardware wait (`0x100b6e7c`) uses it as its clock. Without it, the first wait that
   never succeeds hangs boot. Overflow IRQs 1-3 can come later.
3. **SPI master at 0x4010_7000**. A FIFO-level status model (TX not full; RX level =
   frames pushed − frames popped) gets platform init through its unbounded transfer loop.
   Which device sits on this bus is still open; one guess is a PMIC or the display.
4. **UART at 0x4010_6000** (16550-style): LSR.THRE and TEMT set, THR goes to the host
   console. This prints the banner.

Next, for TF-M's storage partitions and the hand-off to the Non-secure OS:

1. **QSPI controller "SPIF" at 0x5000_1000.** It needs JEDEC Read-ID (0x9F) with a
   plausible ID, then read, erase and program. The firmware sizes the flash at run time
   (strings name 4M/8M/16M parts).
2. **Flash size of 16 MiB.** `Machine::FLASH_SIZE` is 8 MiB today, but ITS/PS live at
   0x00CF_0000+ and the flash MPC covers 0x0000_0000-0x00FF_FFFF.
3. **Instruction-cache control at 0x5000_5000.** Read/write storage is enough.
4. **PPB 0xE004_3018**: decide what it is. Keep it read-as-zero/write-ignored, but make the
   access visible (see the concerns below).

## Answers to `memory-map.md` open questions

- **Q2 (0x0018_B4F3..0x0038_0000) and flash size (Q6).** TF-M ITS/PS storage is at flash
  offset 0x00CF_0000+, read through the NS alias. The flash MPC covers 16 MiB. The constant
  0x0018_F2C0 is not explained yet.
- **Q4 (does image 0 run partly Non-secure?).** SAU region 0 makes flash offsets
  0x0010_0000-0x00FF_FFFF Non-secure. That includes image 0 from slot+0x50000 on, the part
  whose strings are referenced through the NS alias.
- **Q5 (VTOR_NS).** Not reached; boot never gets to the Non-secure hand-off.

## Concerns for Plan 2

- **Silent PPB accesses.** Unimplemented PPB registers (for example 0xE004_3018) are
  invisible in the unmapped log, and `evo-cli` installs no logger to show the warning. Both
  hide discoveries. One fix is to route them into `MemoryMap::log` (or a sibling log);
  another is to initialize `env_logger` in `evo-cli`.
- **`MemoryMap::peek` takes `&self`** (changed in Task 17 so `Machine::recent_trace` can
  format lazily). Device windows return `None`. If Plan 2 wants debugger peeks into device
  registers, it needs a separate side-effect-free device API.
- **SRAM size.** The machine maps 2 MiB of SRAM; the MPC ranges say 1.5 MiB.

## Reproduce

Use your own bundle in `EVO_OS_IMAGE`.

```
cargo run -p evo-cli --release -- run "$EVO_OS_IMAGE" --max-cycles 50000000 --unmapped-out unmapped.md
cargo run -p evo-cli --release -- run "$EVO_OS_IMAGE" --max-cycles 40000 --trace > boot.trace
cargo test -p evo-core --test real_os
```

`boot.trace` shows one full pass: the failed check at about cycle 33,086 and the reset at
33,102. For GDB, run `evo-cli run "$EVO_OS_IMAGE" --gdb 3333`, then in
`arm-none-eabi-gdb` enter `target remote :3333` and `break *0x100b64bc` (the return from
`mpc_init_cfg`). `p $r0` there shows -10.

## GUI bring-up

Measured 2026-10-14 on an AMD Ryzen 7 5800X3D (Windows 11, release builds), with the 7.1 OS
bundle at Plan 1's `Machine` (no Plan 2 devices, so the guest stays in the reset loop described
above).

- `cargo run -p evo-gui --release` with `EVO_OS_IMAGE` set: the window opens with the
  `No display signal` placeholder; the Registers tab shows the secure core running (r1/r2
  change between samples). After 10 s the header reads `Running · 12%`.
- `evo-cli run "$EVO_OS_IMAGE" --max-cycles 1560000000` (10 s of emulated time) took
  78.6 s: 1.56e9 / (78.6 · 156e6) · 100 = **12.7 %** of real time for the bare interpreter.

The GUI (12 %) runs at the interpreter's own speed, so the UI-thread design costs almost
nothing. By the Global Constraints decision rule of the GUI plan (a worker thread is justified
only if the GUI is below 100 % while the interpreter alone is above it), the UI-thread design
stays. The interpreter itself is the bottleneck: reaching real time needs a roughly 8x faster
core.

### With Plan 2 (Task 11)

Checked 2026-10-14 on the same machine with Plan 2 merged, `MachineBackend` wired to the LCD,
backlight PWM, keypad, UART0, save states and the flash file. The check ran headless: the
`egui_kittest` harness with the real `MachineBackend` and the 7.1 bundle (release build). No
interactive session was possible, so no PC-keyboard typing was checked.

- **LCD and backlight.** The boot screens and, after the Self-Test exit (ENTER at 2.5 s, `0`
  at 3.1 s of guest time, as in M4), the OS home screen (an app grid titled `CALCULATOR`)
  show in the GUI's LCD. The first frame comes before the backlight PWM is raised, so
  `real_os_lcd_frame` waits for both. The LCD view is visibly dimmed by the OS's backlight
  level.
- **KEY_MATRIX.** Clicking the GUI keys `mode` (MODE settings screen opens), `clear` (back to
  the calculation screen), `2nd` (the 2nd indicator appears in the status bar), plus the
  `enter` and `0` taps above, behave as on a CE. Those five positions are verified. The other
  44 match `evo_core`'s `KEY_NAMES`, which uses the same CE matrix, but each one was not
  clicked individually.
- **Speed.** With the OS running, the header showed `Running · 67–97%` in the release build
  (the earlier 12 % was the Plan 1 reset loop).
- **Flash file.** `persist` writes `{data_dir}/flash/{fnv1a64}.nv`. After the OS launched, it
  held guest writes at 0x0009_1000..0x0009_3000 (the ITS/PS area), and reopening the bundle
  starts from that file. On that reboot the BSP still showed its Self-Test info screen. It is
  not yet known which flash state makes the BSP skip Self-Test.
