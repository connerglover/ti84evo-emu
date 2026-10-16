# Boot flow: secure boot, BL2 shared data, NS hand-off, BSP, image-1 entry, milestones

OS bundle 7.1.0.4421 (secure image TF-M v2.1.1_co2_v1.00, NS BSP 7.1.0.4413). Method: static
analysis of the secure image plus a throwaway emulator build with register fakes.

**Address convention.** Secure code runs at 0x100b_xxxx+. The NS BSP runs at 0x001x_xxxx,
the NS alias of image 0 from slot+0x50000 on. The same bytes appear at the S alias
0x101x_xxxx, and some evidence addresses (from a disassembly of image 0 at its S alias) are
written that way: 0x1015_2716 and 0x0015_2716 name the same instruction.

### Booting persisted flash

`Machine::from_nv(nv, layout)` boots the secure image already stored in an `NvState`.
It keeps guest data and NOR security-register bytes and derives `VTOR_S` from the
stored MCUboot header. Use this when reopening flash after a guest USB OS update.
`Machine::from_bundle_nv` retains its original installation behavior: bundle images
overwrite their saved slots, while other data survives.

Persisted boot rejects a missing or out-of-bounds secure header/body, an unaligned
vector table, an initial stack pointer outside mapped SRAM, or a reset vector outside
the secure image's Thumb code. The active image remains trusted under the existing
bootloader bypass. A recognized pending permanent update gets the additional checks
below; an invalid pending update makes this constructor fail without altering its NV.

Its save-state identity hashes the stored secure header and payload plus slot addresses;
the existing NV fingerprint separately covers all flash and security registers. User-data
writes therefore preserve boot identity, but loading a state still requires exactly its
saved NV contents. Bundle-created and persisted-image-created machines can have different
boot identities even when their flash bytes match.

### Completed permanent secure updates

For `Layout::EVO_7_1`, persisted boot, `power_cycle`, and AIRCR.SYSRESETREQ recognize a
completed permanent update in the secure secondary slot. The firmware flash-area table
at physical `0xCE74C` identifies primary `0xB0000` and secondary `0x280000`, each 1 MiB.
An actual official TI Connect Evo `sendBundle` transfer wrote the secondary image and
its trailer: MCUboot magic at `0x37FFF0`, `image_ok=1` at `0x37FFE8`, and erased
`copy_done` at `0x37FFE0`. Transfer success alone therefore means staging succeeded.

Before activation, `evo-core/src/boot.rs` requires that trailer and validates bounded
MCUboot header/TLV lengths, image flags, SHA-256 over header/body/protected TLVs, a
nondecreasing protected security counter, and RSA-3072 PSS with SHA-256 and a 32-byte
salt. The candidate's key-hash TLV must match a PKCS#1 DER public key embedded inside
the active secure image body. No TI key is distributed with the emulator. This trusts
the current image; it does not recreate an immutable hardware root of trust or OTP
anti-rollback policy. The signed candidate's vectors must also pass persisted boot's
bounds checks before any live flash changes.

The emulator applies the **completed permanent swap outcome atomically in memory**:
swap the two secure slots, set primary `copy_done=1`, and erase the secondary's final
4 KiB trailer sector. Other images and NOR security registers are unchanged. This
matches the permanent-update state described by [MCUboot's design](https://docs.mcuboot.com/design.html)
and its [swap-type table](https://github.com/mcu-tools/mcuboot/blob/main/boot/bootutil/src/bootutil_public.c).
It does not emulate BL2 instructions, scratch-sector writes, interrupted-swap recovery,
test/revert updates, or bootloader replacement. Unsupported or incomplete trailers are
left alone. A recognized but invalid candidate leaves all flash unchanged; reset logs
one warning and boots the existing image, whereas `from_nv` returns an error.

Synthetic signed tests execute different old/new instructions, reject damaged hashes,
signatures, counters and vectors, and exercise power-cycle and guest reset activation.
The optional `EVO_STAGED_UPDATE` test validates an external, uncommitted saved flash in
memory. The observed TI transfer used bundle 7.1.0.4421 against the same installed
version: it verifies real signed staging and activation, not a cross-version upgrade.

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
(`lcdc.md`). With the board inputs, the product-ID strap, a correct SHA-256 engine and all
ten package images placed (Task 24), the BSP validates OS image 1 and, on a fresh unit,
starts the Self-Test app; leaving it launches image 1 (§3.3, "M4 notes"). A **SHA-256 engine** at 0x5000_2000 is also used
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

### 3.3 After the logo: the launch decision (Task 24)

The BSP's control logic is three table-driven state machines (LAUNCH 0x2001_02B8, POWER
0x2001_0318, CHARGE 0x2001_01B8) run by one engine (thread body 0x001664d0); the LAUNCH
thread waits on its own message queue. `discovery-log.md` §"Post-logo stall" has the state
and event names and the trace. In order:

1. POWER "Check State" (0x00152eb4) reads the charger status (GPIO pin 9) and the battery
   comparator (pin 11 against thresholds on pins 5/7/12) and sends LAUNCH "Battery
   Charging", "Battery Critical" or "Battery Good" (`gpio.md`).
2. LAUNCH "Battery Good" (0x0015c958) dispatches on the boot target byte 0x2001_47A4, set by
   the BSP init: 0 → state OS, 1 → Selftest (the diagnostic app), 2 → Install.
3. The BSP init (0x0015d474..0x0015d4e0) chooses the target. Reset reason 8 (retained record
   at 0x2017_DDD0; set at 0x0014f786, probably by the USB OS-install path before its reboot) first writes ITS UID
   0x1F61E867_8CE70001 ("boot to OS") = 1. The target is 1 when that UID is missing or 0, or
   when UID ..0004 (diagnostic-menu request) is nonzero; combo 8 (`2nd` + `del`) gives 2;
   a nonzero answer from secure service 0x4000_0108 type 3 forces 0.
4. State OS (0x00160364) asks secure service 0x4000_0107 type 0 (0x00114108) to validate
   image 1; status 0 → the launch code below; 1 (invalid image) or 2 (bad header or a
   missing image 2..9) throws 0x13/0x12 → OS Exit → OS Missing → Waiting Install ("Install
   OS & Apps").

The service validates like MCUboot over the slot at 0x0038_0000: SHA-256 TLV 0x10 over
header + payload + protected TLVs (42,212 SHA blocks, about 35 M cycles), RSA-3072 TLVs
0x01/0x23 (about 11 M cycles in software), security counter TLV 0x50 (0x0701_0000) against
NV counter 3, and TI TLV 0xA0 (product ID 1) against the hardware product ID
(`security.md`, CRU +0x10). It then requires an MCUboot header 0x40 bytes before each
payload address in TLVs 0xB1..0xB8 (images 2..9).

On a fresh unit (empty ITS) the target is 1: the BSP shows the unit info screen, then (after
ENTER) the "Self-Test" menu; "0. Exit this menu" sends LAUNCH "OS" once. Provisioning
(§3.5) sets UID ..0001 so that later power-ons launch the OS directly.

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

The OS entry trampoline stores r0, the BSP's service table (0x0018_2024 in BSP 7.1.0.4413),
to 0x2009_B100. It checks a
compatibility pair (0x380aee: 0x380c6c vs 0x380c80; on mismatch it calls 0x380930(15) and
then 0x44bce4), then jumps to 0x0038_0209.

All OS-to-BSP calls go through `ip = [[0x2009_B100] + r0*4]`; `bx ip`. The stub is at
0x44bc9e.

Image 1 holds very few peripheral literals: blocks 0x4000_0000, 0x4000_4000 and 0x4010_0000,
plus sparse hits that are mostly data false positives. The OS therefore reaches hardware
mostly through the BSP API, and the first peripherals the NS OS touches are the BSP's
(§3.2).

### 3.5 Factory provisioning: the Engineering menu (fix wave Task 3)

The BSP's own Engineering menu sets the persistent "boot to OS" flag. `evo_core::provision`
and `evo-cli provision <bundle> --flash-file F` drive it with the keypad; nothing writes ITS
or flash directly. Found with breakpoints and screenshots in a throwaway probe on bundle
7.1.0.4421; every step below was observed.

1. **Power on with `alpha` + `sin` + `enter` held** (boot combo 9, `keypad.md`). `get_combo`
   returns 9 at about 92.5 M cycles (breakpoint 0x0015_D488, r4). The BSP writes ITS UID
   ..0004 := 2 (0x0010_8414, from 0x0015_D4D6) and sets the boot target to 1, the diagnostic
   app. The keys can be released after `get_combo`.
2. **The diagnostic app** starts at 0x0011_7AFC (about 110 M). It reads UIDs ..0002 and ..0004,
   shows the unit info screen (0x0010_7290 waits for ENTER), then calls the menu chooser
   0x0011_7BE0. The chooser reads UID ..0004 again (r0 at 0x0011_7BE6): **2 opens the
   Engineering menu** (object 0x2001_0EF4), anything else the Self-Test menu. After the
   chooser the app writes UID ..0004 := 0, so the Engineering menu is one-shot.
3. **ENTER** on the info screen opens the Engineering menu. Retrying ENTER every 500 ms until
   the chooser runs works; the first ENTER at about 1.1 s already lands (chooser at about
   189 M). Its items are "1. About", "2. Boot Select: Self-Test", "3. Self Test",
   "4. Utilities", "5. Reset", "6. Logging", "7. Tests" and "0. Exit this menu". Digit keys
   select directly; no confirmation and no reboot are needed.
4. **2 (Boot Select)** toggles the flag. The item fills +0x38..+0x4B of the menu object: its
   label pointer is at +0x38, its action code 8 at +0x40 and the dispatcher 0x0015_DAFC at
   +0x44. The handler writes
   UID ..0001 through 0x0010_83C8 at once. The label pointer at +0x38 changes from "Boot
   Select: Self-Test" to "Boot Select: Calc OS" (written by 0x0016_8808..0x0016_8888), and the
   screen shows the new line. The label follows the stored flag: on a unit that is already
   provisioned it reads "Calc OS" when the menu opens, so `provision` does not press 2 again.
5. **0 (Exit this menu)** launches the OS in the same boot. The BSP first validates image 1
   (about 160 M cycles), then enters 0x0044_BC90.
6. **Persistence.** The flag is in the ITS area of flash (0x000A_C000..), so
   `Machine::nv_state()` and a flash file keep it. A new power-on with no key held goes
   through Battery Good to state OS and enters image 1 at about 300 M cycles. It never
   reaches the diagnostic app. A third power-on, from the flash the second boot re-saved, does
   the same (`provisioned_flash_boots_to_os_without_keys`).

`provision` checks the menu title ("Engineering") and the Boot Select label before it presses
anything. Its waits are breakpoints at the BSP 7.1.0.4413 addresses above, so another BSP
version needs them checked. `Machine::power_cycle` does the power-off/on: the machine that
`from_bundle_nv` would build from `nv_state()`.

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
8. **The BSP launch gate (resolved, Task 24).** Board inputs on GPIO pins 9 and 11, the
   CRU product-ID strap, the SHA-256 continue bit and the placement of images 2..9 (§3.3,
   `discovery-log.md` §"Post-logo stall"). A fresh unit then needs the Self-Test menu's
   "Exit" to launch the OS, once; a provisioned unit (§3.5) launches it on every power-on.
9. Later: image 1 runs through the BSP service table (0x2009_B100), so NS OS hardware needs
   follow the BSP's drivers.

## 6. Open questions

- What is at 0x1800_E000 (16 B read twice, HUK/OTP?), and what does 0x5020_10C0 bits 4/6
  select (`security.md` §2)?
- What should flash offset 0 hold? Service 0x106 type 0xD expects an MCUboot image (magic
  0x96F3_B83D, the Launcher/BL2?) and returns -132 without it. The launch does not depend
  on it (Task 24); the info screen shows BL1/BL2 0.0.0.0.
- Resolved (fix wave Task 3): an emulated unit gets its factory ITS state (UID ..0001 "boot
  to OS") from the Engineering menu's Boot Select, driven by `evo-cli provision` (§3.5).
- Resolved: the BSP uses RTX5 (CMSIS-RTOS2). 0x2004_4DC0 is an RTX timer ("Animation"),
  not a thread; the LAUNCH thread's control block is at 0x2003_9DD8.

## Milestones

The milestone tests (`crates/evo-core/tests/milestones.rs`) pin this boot flow. Their cycle
counts are recorded here as each one passes.

| milestone | what it shows | cycle |
|---|---|---|
| M1 | `Booting TF-M v2.1.1` on UART0 (steps 1–3) | 81,955,780 (first UART0 byte, without a SPIF model: the 100 ms Read-ID timeout and the SPI-device delays come first; Task 10) |
| M2 | secure boot completes; NS hand-off at 0x0011_6D50; `Starting BSP` on UART0 | hand-off 70,784,677 (first boot, 0 resets; 70,282,731 on the second power-on from the saved NV state); `Starting BSP` by 80,784,677 (the test checks UART0 every 10 M cycles, so this is an upper bound); Task 16 |
| M3 | the boot logo on the emulated screen | 90,000,000 (the TI logo alone is on the screen from about 80 M to 100 M; Task 23, "M3 notes") |
| M4 | OS image 1 entered at 0x0044_BC90; the OS home screen at 1,400 M | 656,271,401 (fresh unit: ENTER at 2.5 s and 0 at 3.1 s in the Self-Test app; Task 24, "M4 notes"; 656,271,393 in Task 24) |
| provisioned | image 1 entered with no key input after `provision` (§3.5) | 299,930,397 (power-on 2: 299,944,694 from a flash file; power-on 3: 299,868,494 from the flash power-on 2 re-saved; fix wave Task 3) |

The M1 banner now arrives by cycle 70,000,001 (same 10 M-cycle granularity): with the SPIF
the Read-ID probe no longer times out.

### M2 notes (Task 16)

- **UART text.** TF-M colours parts of its log with ANSI SGR sequences, e.g. the Mbed TLS
  line is `Init ESC[0;32mMbed TLS 3.6.2ESC[0m... ESC[0;32mcompleteESC[0m.`. The M2 checks
  match the text with the escape sequences removed.
- **Second power-on.** Booted again from `nv_state()` after the first boot, the secure image
  finds the ITS layout and does not print `Creating an empty ITS flash layout.`
- **Peripheral accesses before the hand-off (the splash devices).** The secure image
  shows its splash before `bxns`: GPIO 0x4010_5000 (+00/+08/+10/+18; `gpio_*`
  0x100b2ec0..0x100b2fb2, panel reset), the PL111 LCDC 0x4000_4000 (TIM0–3, UPBASE/LPBASE =
  0x2004_8000, CONTROL = 0x312D then `|= 0x800`; `panel_on` 0x100b3200..0x100b3276) and the
  backlight PWM 0x4010_4000 (+00 = 0xA0, +04 = 0xFF at 0x100b285a/0x100b285e). Tasks 17
  (GPIO) and 18 (LCDC, PWM) model them, so they no longer reach the unmapped log; the Secure
  aliases (0x5000_4000, 0x5010_4000) are not accessed, and M2 fails if they are.
- **First unmapped NS accesses** (BSP init, after the hand-off), in order:
  - keypad 0x4010_3000: +04 = 0x808 (0x0015f1d8), +00 = 0x5B68_0C33 (0x0015f1e2), +08 read
    then `= 0`, +0C read then `= 2` (0x0015f1e4..0x0015f1f2);
  - LCDC 0x4000_4000: +18 read (0x0015f384), then TIM0 = 0x0A0A_0A38, TIM1 = 0x042D_093F,
    TIM2 = 0x00EF_7801, TIM3 = 0, UPBASE = 0x2004_8000 (0x0015f41e..0x0015f428).

  This matches the order of §3.2.

### M3 notes (Task 23)

- **What the screen shows, in order** (screenshots every few M cycles with
  `evo-cli run --screenshot`): the secure splash (the TI logo) from about 70 M; the BSP
  redraws the same logo with `Starting BSP` (a different frame hash, the same picture) and
  it stays alone on the screen up to about 100 M; by 110 M a grey busy spinner is drawn above
  it. On a fresh unit the BSP then starts the Self-Test app (§3.3): by 130 M its unit-info
  screen (the model name, the BL1/BL2/BSP versions, the product number, the unit ID and a
  QR code, drawn last) replaces the logo, and from about 150 M it stays there until a key is
  pressed (fix wave Task 5 re-check). In Task 23, before the M4 fixes, the BSP showed a
  charging animation (a battery icon whose bars fill one by one) instead; that no longer
  happens.
- **The pinned frame.** `m3_boot_logo` hashes the frame at 90,000,000 (the middle of the
  logo-only window; the window bounds are not exact). The `m3_find_settled_logo` search
  (50 M slices from the `Starting BSP` cycle, three equal frames in a row) never sees the
  logo-only frame: its first slice ends at about 130 M, and it now settles on the Self-Test
  unit-info screen at 180,784,677 (in Task 23 it stopped on a step of the charging animation).
  That screen is not the boot logo, so it is not the pinned frame.
- **Approval.** The frame at 90 M was checked by eye by the Task 23 implementer (Claude), not
  by the user: the Texas Instruments wordmark with its logo, horizontally centred in the lower
  third of a black screen, upright, reads left to right, not mirrored. This confirms the
  `Lcdc::frame` orientation (`lcdc.md`). No image is committed; the test compares a SHA-256 of
  the decoded RGB bytes.
- **The colour order is not confirmed by M3.** The logo and spinner are grey and the battery
  bars are green; none of them tells R from B apart. The OS launcher settled it (`lcdc.md`
  "Colour order").
- **Input for M4 (Task 24), resolved.** The charging animation came with the board inputs
  undriven: since the M4 fixes (GPIO pins 9/11, the charger status among them, driven high as
  the board does, `gpio.md`) the BSP boots to the Self-Test app instead, and M4 leaves it
  through its menu ("M4 notes").
- **Checks at 90 M:** the backlight PWM brightness is above 0; no SRAM access past 1.5 MiB;
  no fatal stub IRQ line; no unmapped access outside `KNOWN_UNMODELLED`; a second boot gives
  the same frame hash.

### M4 notes (Task 24)

- **What unblocked the launch:** four model fixes, each with its own test and commit:
  GPIO pins 9/11 driven high by the board (`gpio.md`), CRU +0x10 strap = 1 (`security.md`),
  SHA CTRL bit 2 = continue (`crypto.md`), images 2..9 placed after image 1
  (`evo_image::Layout::place`). Then the Self-Test menu's "0. Exit this menu" (§3.3).
- **Launch cycle:** 656,271,401 (656,271,393 in Task 24; budget 1,000 M in
  `m4_image1_launches`); 0 system resets.
- **BSP → OS service table:** r0 = 0x0018_2024 at the entry; the OS stores it at
  0x2009_B100 (checked by the test after 500 M more cycles). OS-to-BSP calls go through
  `[[0x2009_B100] + n*4]` (stub 0x0044bc9e).
- **After the entry (500 M cycles):** no new unmapped access, no UART0 output. The screen
  shows the OS home screen (an app grid titled "CALCULATOR") by about 7.9 s of guest time.
  The OS reaches hardware through the BSP, so the next work (keypad input, RTC, Plan 3)
  follows the BSP's drivers.
- **The OS runs (fix wave Task 5).** `m4_image1_launches` also hashes the screen at
  1,400,000,000 cycles (about 9 s): the OS home screen, the app launcher titled "CALCULATOR"
  with its first app selected, checked by eye by the Task 5 implementer (Claude). The frame is
  written to `m4-home.png` in the test's scratch directory (`CARGO_TARGET_TMPDIR`) for that
  check; only its SHA-256 is committed.
- **Key source.** With correct SHA-256 digests the secure image reads the unmapped OTP page
  at 0x1800_E03C and 0x1800_E0F4 (16 bytes each, reads 0); the milestone tests accept the
  whole 4 KiB page 0x1800_E000..0x1800_EFFF.
- **M3** is unchanged: the logo frame at 90 M has the same hash.
