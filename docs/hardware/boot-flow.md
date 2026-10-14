# Boot flow: secure boot, BL2 shared data, NS hand-off, BSP, image-1 entry, milestones

OS bundle 7.1.0.4421 (secure image TF-M v2.1.1_co2_v1.00, NS BSP 7.1.0.4413). Method: static
analysis of the secure image plus a throwaway emulator build with register fakes.

**Address convention.** Secure code runs at 0x100b_xxxx+. The NS BSP runs at 0x001x_xxxx,
the NS alias of image 0 from slot+0x50000 on. The same bytes appear at the S alias
0x101x_xxxx, and some evidence addresses (from a disassembly of image 0 at its S alias) are
written that way: 0x1015_2716 and 0x0015_2716 name the same instruction.

## 0. Result in one paragraph

With the four MPCs, TIMER0, the SPI, the UART, a working **SPIF** with flash security
registers, a **CC312-style TRNG** at 0x5010_A000, the **BL2 shared-data header** at
0x3000_0000 and the CONTROL.SPSEL fix (repo commit `5fe119b`), the secure image boots every
partition. It then hands off to the Non-secure BSP:

- VTOR_NS = 0x0010_0400
- MSP_NS = 0x2004_6D20
- entry 0x0011_6D51

The BSP prints `Starting BSP 7.1.0.4413` and draws the TI logo into the framebuffer at
0x2004_8000. The framebuffer is 240x320 RGB565, portrait, and the panel shows it rotated
(`lcdc.md`). The BSP then idles in its RTOS idle thread (WFI loop at 0x00160b88) and never
launches OS image 1 (§3.3, §5). A **SHA-256 engine** at 0x5000_2000 is also used
(`crypto.md`). It must compute real SHA-256 for correctness but is not what stops the boot.
The secure image never reaches a fatal error with these models (0 resets in 300 M cycles).

## 1. Why the SPM panics at 0x100b6c0e after "Creating an empty ITS flash layout."

It is neither TRNG nor SHA: the cause is the **SPIF flash driver**.

- **Where.** 0x100b6ba0 is `psa_reply()`, inlined into the SFN backend. The panic branch
  reads ([r4,#20] is the client id; the branch is taken when the client is Secure and the
  status is PROGRAMMER_ERROR):

  ```
  100b6c00: adds.w r0, r6, #129     ; status == PSA_ERROR_PROGRAMMER_ERROR (-129)?
  100b6c08: ldr    r0, [r4, #20]    ; client id
  100b6c0a: cmp    r0, #0
  100b6c0c: it     pl               ; secure client (id >= 0)
  100b6c0e: blpl   0x100b618c       ; tfm_core_panic -> NVIC_SystemReset
  ```

- **Who.** The PS partition's init (code around 0x100d6da0, the PS module that prints its
  encryption algorithm) is the first ITS client. It calls
  `psa_its_get(uid=1, offset 0, 1360 bytes -> 0x3000_1258)` via the wrapper 0x100ca3c4,
  which does `psa_call(0x4000_0102, type 1002 = TFM_ITS_GET, ...)`.
- **SFN lazy init.** The SFN backend's message delivery (0x100b26d8) sees that the ITS
  partition is not initialised yet. It calls the partition entry 0x100d5cd4 (ITS init). When
  the entry returns a negative value, the backend returns PROGRAMMER_ERROR:

  ```
  100b2702: blx r1               ; partition init (ITS: 0x100d5cd4)
  100b2704: cmp r0, #0
  100b2706: bmi 0x100b271e       ; -> mvn r0, #128 (-129)
  ```

- **Why ITS init fails.** ITS init prints "Creating an empty ITS flash layout." and then
  erases through the spi-nor ops table at 0x3000_2c58. The erase op (+12) is NULL, so
  0x100b168c returns 0x9D (-99). The table is filled only after a successful JEDEC Read-ID
  probe (0x100b16e8 -> 0x100b23e0). The probe never succeeds because 0x5000_1018 bit 1 (RX
  data available) never sets.
- **Fix.** A working SPIF (`spif-nor.md`). Any Read-ID value works, because the driver
  forces the capacity byte to 24 (16 MiB; `spif-nor.md` §6.1).

The Update, TRNG, Power and PTT partitions are not involved: they initialise before this
point, and none of them touches TRNG or SHA hardware at init time.

## 2. The rest of the secure boot, step by step

Cycle counts are for the first boot with all models. Each step names the hardware it needs.

| # | ~cycle | step (first message) | hardware needed | what happens without it |
|---|---|---|---|---|
| 1 | 0–33 k | reset, SystemInit, scatter-load, SAU | CRU 0x5020_0000 (reads 0 OK), ICACHE 0x5000_5004 | — |
| 2 | 33 k | `tfm_hal_set_up_static_boundaries` | 4 MPCs (BLK_CFG/BLK_MAX) | the MPC reset loop of `discovery-log.md` |
| 3 | ~3.0 M | platform init, banner | TIMER0 0x4010_1000, SPI 0x4010_7000, UART 0x4010_6000, **SPIF Read-ID** | hang / no banner |
| 4 | ~3.9 M | the secure partitions (Update, TRNG, Power, Data, Boot, PTT, …) initialise | none new | — |
| 5 | ~3.95 M | ITS lazy init: "Creating an empty ITS flash layout." | **SPIF** erase 0x20, program 0x02, status 0x05, WREN 0x06; TX path (`spif-nor.md` §5.2) | **panic at 0x100b6c0e** |
| 6 | | "Creating an empty PS flash layout." | SPIF | same panic |
| 7 | ~4.8 M | `[Crypto] Init Mbed TLS 3.6.2 ... complete` | **TRNG 0x5010_A000** (entropy) | hang: SAMPLE_CNT1 read-back loop at 0x100b55a4 never ends |
| 8 | ~4.85 M | crypto key derivation / hashing | **SHA 0x5000_2000** (Mbed TLS SHA256_ALT) | no hang, but every digest is 0 (wrong keys and hashes) |
| 9 | ~4.9 M | NV counters init (0x100b6630 -> 0x100b3df4) | **SPIF security registers** (0x48 read, 0x44 erase, 0x42 program) | init returns -147, then panic at 0x100b6c0e again |
| 10 | ~6.5 M | "OS Partition", "Info Partition", "AM Storage Partition initializing" | SPIF / SHA | — |
| 11 | ~6.78 M | Initial Attestation init: SVC 3 `tfm_core_get_boot_data(major 1)` | **BL2 shared data at 0x3000_0000** (magic 0x2016, §4) | -135, partition init fails, `tfm_core_panic` from the init loop at 0x100b52f4 |
| 12 | 6,780,858 | NS hand-off: `bxns r0` at 0x100b3dea | none | — |

Details:

- **Step 9.** NV counters live in three SPI-NOR security registers at command address
  0x1000, 0x2000 and 0x3000. Each holds 8 slots of 32 bytes, and each slot ends with magic
  0xC0DE_0042 at slot+28. The scan at 0x100b3df4 reads +28 of each register. If the word is
  neither the magic nor 0xFFFF_FFFF it erases the register (0x44). Erased registers (0xFF)
  are accepted, and the firmware then provisions them itself.
- **Step 11.** The validation, `tfm_core_validate_boot_data` (0x100b6190, called from `main`
  at 0x100b3504), is:

  ```
  100b6190: mov.w r0, #0x30000000
  100b6194: ldrh  r0, [r0]
  100b6196: movw  r1, #0x2016         ; SHARED_DATA_TLV_INFO_MAGIC
  100b619a: cmp   r0, r1
  100b619c: it ne
  100b619e: bxne  lr                  ; leaves is_boot_data_valid (0x3000_2CA4) = 0
  ```

- **Step 12.** The hand-off sequence:

  ```
  100b64d8: push {r7, lr}
  100b64da: bl   0x100b6300          ; returns 0x0010_0400
  100b64de: movw r1, #0xed08
  100b64e2: movt r1, #0xe002         ; VTOR_NS
  100b64e6: str  r0, [r1]
  100b64e8: bl   0x100b62f0          ; returns [0x0010_0400] = 0x2004_6D20
  100b64ec: msr  MSP_NS, r0
  ```

  Then 0x100b3dc4 clears r1–r12 and LR and does `bxns r0` with r0 = [0x0010_0404] =
  0x0011_6D51.

The first NS code is therefore **the BSP inside image 0**, at flash 0x0010_0000+ (slot+0x50000,
the part SAU region 0 makes NS), not image 1. Its NS vector table is at 0x0010_0400: SP
0x2004_6D20, Reset 0x0011_6D51, HardFault 0x0010_C669, UsageFault 0x0011_D8C9, PendSV
0x0011_4189, SysTick 0x0011_872D. The IRQ table starts at +0x40. IRQ 24 -> 0x0010_C095 reads
0x4010_5028 and 0x4010_5020 (GPIO, `gpio.md`).

Flash use seen through the SPIF:

| region | content |
|---|---|
| 0x0009_0000–0x000A_BFFF | PS (4 KiB sector erases 0x20, page programs 0x02) |
| 0x000A_C000–0x000A_FFFF | ITS (first erases are 0x0AC000 and 0x0AD000; first program is 12 B at 0x0AD008) |
| security registers 1–3 | NV counters |

All of this sits below the secure slot at 0x000B_0000. The 0x00CF_0000 mailbox is read but
not erased when it is blank (`spif-nor.md` §6.3).

Another unmapped read source: the function at 0x100bfd7a reads 2 x 16 bytes from
**0x1800_E000** when `0x5020_10C0 & 0x50 == 0`. Otherwise it takes a slot-based address
(0x100bafac). It looks like an OTP or ROM key source, probably for HUK derivation
(`security.md` §2, +0xC0). Unmapped, it reads zeros and boot continues.

## 3. After the hand-off: the NS BSP and image 1

### 3.1 An emulator bug that blocked the BSP (fixed in `5fe119b`)

At 8fb3eed the BSP took a UsageFault INVSTATE about 1.1 M cycles after the hand-off:

- **Symptom.** `ldmia.w sp!, {..., pc}` at 0x00168b16 popped 0. That instruction ends the NS
  `tfm_ns_interface_dispatch` wrapper after a long secure call.
- **Cause.** NS SysTick (exception 15) preempted Secure thread code. Exception entry cleared
  CONTROL_NS.SPSEL. The return to Secure thread restored only CONTROL_S, so on return to NS
  the thread used MSP_NS instead of PSP_NS.
- **Status.** Repo commit `5fe119b` ("keep each security state's CONTROL.SPSEL across
  cross-state exceptions") fixes it.

### 3.2 What the BSP does, in order of first access

Every access below comes from NS code; values are as written. The BSP init function is at
0x0015_d438 (it loads the "Starting BSP" string first).

| order | block | IRQ | accesses (offset: action) | identity |
|---|---|---|---|---|
| 1 | 0x4010_3000 | 13 (ON key: 0) | +04 = 0x808, +00 = 0x5B68_0C33, +08 &= ~2, +0C \|= 2 (0x0015f1c4) | **keypad scanner** (`keypad.md`) |
| 2 | 0x4000_4000 | 14 | +18 R, +00..+14 timing/FB = 0x2004_8000, +18 = 0x312D then \|= 0x800, +28 R/W 8 (110x, polled), +1C = 8 | **Arm PL111 CLCD** (`lcdc.md`) |
| 3 | 0x4010_5000 | 24 | +18, +08, +00 RMW; later +20/+30/+34/+38/+3C = 0x200 etc., +04 polled 21x | **GPIO controller, 13 pins** (`gpio.md`) |
| 4 | 0x4010_4000 | - | +00 = 0x75, +04 = 0xFF | **backlight PWM** (`lcdc.md`) |
| 5 | 0x4010_2000 | 15 | +40 R, +20 RMW (0x80, then 1) | **RTC** (`rtc.md`) |
| 6 | 0x4010_C000 | 28 | +84 RMW 0, +1A4 = 0x95, byte and halfword accesses +00..+2F (0x00130310/0x0013034e) | **USB Type-C controller (probable)** (`usb.md`) |
| 7 | 0x4000_3000 | 16 | +E0 ID and +E4 config, read at 0x0012665c; then +88, +C4 = 3, +100 = 0x10 (polled ~2000x), +100 = 0x220, +134/138/32C/33C/104/1C0/330/120, +1B0..+1BC = 0x1000, +1A0..+1AC, +13C, +000 = 2 (soft reset, poll until bit 1 clears) | **USB OTG** (MCCI "UHIL"; `usb.md`) |
| — | Secure services | | via the veneer 0x100b04d0 (`tfm_psa_call_veneer`): handles 0x4000_0100 (crypto), 0101 (ITS), 0104, 0106, 0108, 0109, 010A, 010E | TI partitions |

Identity evidence (handles, descriptors and IRQ handlers) is in each block's doc. Without a
USB model the BSP prints a `UHIL_swc: fatal error` line and resets (`usb.md`).

Secure-service return values seen: ITS `get` returns -140 (DOES_NOT_EXIST, normal on a blank
device). Handle 0x106 type 0xD returns -132. That service (0x100d79fc) reads 32 bytes at
flash offset 0 (S 0x1000_0000) and expects the MCUboot magic 0x96F3_B83D there, probably the
"Launcher" (BL2) image header for the version screen. The emulated flash has 0xFF there.

### 3.3 Where the BSP stops

After the logo the BSP creates a thread with entry 0x0015_1A55 (TCB 0x2004_4DC0) and signals
object "LAUNCH" (0x2001_02B8) with 6 (0x0015_d538 -> 0x0011_8238). That thread never runs:
no hit on 0x0015_1A54 in 300 M cycles. The OS validation path (0x0015_2BA8: "Validating
OS...", called from 0x0010_bb82 and 0x0010_bce2) and the image-1 launch are never reached.

NVIC at idle:

- enabled = 0x1100_6181, that is IRQ 0 (OnKey), 7 (MPC), 8 (PPC), 13 (Keypad), 14 (LCDC),
  24 (GPIO) and 28 (USBTC);
- ITNS = 0xFFFF_FE7F (all NS except 7 and 8);
- SysTick_NS = 1 ms (RVR 0x2615F at 156 MHz).

Pending IRQs 0, 13, 14, 24 or 28 every 16 ms without device status changes nothing. Which
event or device status the BSP's state machine waits for is open; §5 lists the candidates.

### 3.4 How image 1 (the NS OS) is entered

The BSP launch code at 0x0015_26f0 runs image 1 as a callee in the same NS state, without
changing VTOR (so image 1 has no NS vector table of its own; VTOR_NS stays 0x0010_0400). It
reads the entry from image 1's MCUboot header:

```
101526f0: movw r0, #0
101526f4: movt r0, #0x38          ; 0x0038_0000 slot
101526f8: ldrh r1, [r0, #8]       ; ih_hdr_size (0x200)
101526fa: add  r0, r1
101526fc: ldr  r6, [r0, #4]       ; reset word = 0x0044_BC91
...
10152716: blx  r6
```

The OS entry trampoline stores r0, the BSP's service table, to 0x2009_B100. It checks a
compatibility pair (0x380aee: 0x380c6c vs 0x380c80; on mismatch it calls 0x380930(15) and
then 0x44bce4), then jumps to 0x0038_0209.

All OS-to-BSP calls go through `ip = [[0x2009_B100] + r0*4]`; `bx ip`. The stub is at
0x44bc9e.

Image 1 holds very few peripheral literals: blocks 0x4000_0000, 0x4000_4000 and 0x4010_0000,
plus sparse hits that are mostly data false positives. The OS therefore reaches hardware
mostly through the BSP API, and the first peripherals the NS OS touches are the BSP's
(§3.2).

## 4. BL2 shared data (not a peripheral, but required)

The emulator replaces BL2, so it writes the TLV header TF-M expects at S SRAM 0x3000_0000:
`u16 magic = 0x2016, u16 tlv_tot_len = 4` (word **0x0004_2016**). With no TLVs every
partition initialises. If attestation tokens are ever needed, add the IAS TLVs (major 1)
that MCUboot would write, and major 0 for the FWU partition 0x10A.

**Write it at power-on and on every system reset**, before the first instruction fetch.
TF-M reads 0x3000_0000 in `main` (0x100b3504 → 0x100b6190) after each boot. SRAM survives
`SYSRESETREQ` in the emulator, but the word sits at the bottom of the secure MSP stack (top
0x3000_07F8, MSPLIM 0x3000_0000), so a deep secure stack may overwrite it during the
previous boot.

## 5. Ordered blockers (after MPCs, TIMER0, SPI, UART and SPIF Read-ID are modelled)

1. **SPIF erase, program and status path**, including TX free space on +54 and +18 bit 0,
   and the done W1C. Without it ITS init fails and the SPM panics at 0x100b6c0e.
2. **TRNG at 0x5010_A000** (`crypto.md`). Without it the boot hangs forever at 0x100b55a4
   during Mbed TLS init.
3. **SHA-256 at 0x5000_2000** (`crypto.md`). Not fatal to secure boot but required for
   correct crypto (keys, hashes, OS validation).
4. **SPIF security registers 0x48/0x44/0x42.** Without them NV-counter init fails (-147) and
   the SPM panics.
5. **BL2 shared data at 0x3000_0000** (§4). Without it attestation init fails and
   `tfm_core_panic` fires at 0x100b52f4.
6. **CPU: CONTROL.SPSEL preserved across cross-state exceptions.** Done in `5fe119b`.
7. **USB ID and soft reset at 0x4000_3000** (`usb.md`). Without them the BSP reports a
   `UHIL_swc` fatal error and resets.
8. **The BSP launch gate (open).** The BSP sits idle after the TI logo, and the "LAUNCH"
   thread 0x0015_1A55 never runs. Candidates, in the order to test them:
   1. **LCDC VComp → IRQ 14** (the first candidate). The BSP LCD init
      (0x0010_9b18..0x0010_9bd8) creates a display thread (handle at 0x2004_5080), clears
      ICR bit 3, sets IMSC bit 3, then does NVIC ICPR/ISER for IRQ 14. The IRQ 14 handler
      (0x0010_e2d0) masks NVIC 14 and posts event 0x800 to that thread; only the thread
      re-arms the line. A one-shot pend therefore wakes the thread at most once, and pending
      IRQ 14 every 16 ms achieves nothing more after that. A real model must latch RIS.VComp
      once per frame (`lcdc.md`).
   2. The keypad (`keypad.md`) and GPIO pin 9/11 interrupts (`gpio.md`).
   3. The ON key and keypad presses.
   4. USB-C attach (IRQ 28, 0x4010_C000; `usb.md`), only after the above are refuted.
   5. RTC busy/status bits (`rtc.md`), if a trace shows the BSP polling them.
   The next step in any case: trace the RTOS ready list or the "LAUNCH" object 0x2001_02B8
   to see what the thread waits on.
9. Later: image 1 runs through the BSP service table (0x2009_B100), so NS OS hardware needs
   follow the BSP's drivers.

## 6. Open questions

- What is at 0x1800_E000 (16 B read twice, HUK/OTP?), and what does 0x5020_10C0 bits 4/6
  select (`security.md` §2)?
- What should flash offset 0 hold? Service 0x106 type 0xD expects an MCUboot image (magic
  0x96F3_B83D, the Launcher/BL2?) and returns -132 without it. Does the BSP's launch gate
  depend on it?
- The thread object at 0x2004_4DC0 does not look like an RTX5 TCB. Which RTOS does the BSP
  use? The CMSIS-RTOS2 API names suggest a wrapper.

## Milestones

The milestone tests (`crates/evo-core/tests/milestones.rs`) pin this boot flow. Their cycle
counts are recorded here as each one passes.

| milestone | what it shows | cycle |
|---|---|---|
| M1 | `Booting TF-M v2.1.1` on UART0 (steps 1–3) | 81,955,780 (first UART0 byte, without a SPIF model: the 100 ms Read-ID timeout and the SPI-device delays come first; Task 10) |
| M2 | secure boot completes; NS hand-off at 0x0011_6D50; `Starting BSP` on UART0 | not yet recorded |
| M3 | the boot logo on the emulated screen | not yet recorded |
| M4 | OS image 1 entered at 0x0044_BC90 | not yet recorded |
