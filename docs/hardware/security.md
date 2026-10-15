# Security and system-control blocks: MPCs, SPCTRL, CRU, PPB 0xE004_3018, LED pattern generator

OS bundle 7.1.0.4421, `Layout::EVO_7_1`. Addresses are secure-image VMAs (slot
`0x100B_0000`) unless marked NS. Disassembly excerpts are 8 instructions or fewer.

## Method and coverage

- **Static, secure image.** Image 0 was disassembled at its S alias and searched for every
  `movw/movt` pair and every literal-pool word that names `0x5020_xxxx`, `0x4020_xxxx` or
  `0xE004_xxxx`.
  - Only 7 `movt` hits exist. Six are real accesses: 0x100b39a8, 0x100b3996 (PPB),
    0x100b4390, 0x100b46d2, 0x100b475a and 0x100b6a38. The seventh, 0x10132778, is a
    constant compare in the NS half.
  - The CRU is reached only through a handle: global `0x100B_7C8C` → `0x100B_7C88` →
    `0x5020_0000`. All 23 users of that handle are in 0x100b5b04..0x100b5f60.
  - The PPC and MPC drivers reach their registers through device structs in RAM. Their
    initializers are compressed in `.data`, so they were read at run time.
- **Static, NS OS image (image 1).** It contains no `movt`, no literal load and no computed
  address for any of these blocks. The few matching words are float constants
  (`0x4020_0000` = 2.5f, used in `cmp`) and bytes inside strings.
  - The NS OS reaches the CRU only indirectly, through the TF-M Power partition's PSA
    service (0x100d7a48.., 0x100d83ac).
  - The NS BSP in the upper half of image 0 (flash 0x0010_0000+) does not touch these
    blocks either.
- **Dynamic.** A throwaway emulator build with register fakes (4 MPCs, CRU, SPCTRL, spare
  0x5020_2000/7000/9000 windows, a catch-all on 0x4020_xxxx, TIMER0, SPI, UART and upper
  flash) logged every access with its PC.
  - The access lists below are what that run produced. Registers used only at run time
    (deep sleep, the reset-cause service) come from static analysis and are marked
    **[static]**.
  - No access to 0x5020_2000..0x5020_2AFF, 0x5020_2C00..0x5020_3FFF, 0x5020_7000,
    0x5020_9000+ or 0x4020_xxxx was observed during boot. 0x5020_2B00 is reached on every
    boot (§6).

---

## 1. SIE-200 MPCs (4 instances)

### Instances

| MPC | base (S only) | CMSIS driver table | RAM dev | ranges (from `mpc_sie200_init`, 2 per MPC, offset 0) | size | `ConfigRegion` call in `mpc_init_cfg` |
|---|---|---|---|---|---|---|
| SRAM0 | `0x5020_4000` | `0x100B_7A8C` | - | NS `0x2000_0000-0x2007_FFFF` (attr 1), S `0x3000_0000-0x3007_FFFF` (attr 0) | 512 KiB | `(0x2001_0000, 0x2007_FFFF, NS)` |
| SRAM1 | `0x5020_5000` | `0x100B_7AC0` | - | NS `0x2008_0000-0x200F_FFFF`, S `0x3008_0000-0x300F_FFFF` | 512 KiB | `(0x2008_0000, 0x200F_FFFF, NS)` |
| SRAM2 | `0x5020_6000` | `0x100B_7AF4` | - | NS `0x2010_0000-0x2017_FFFF`, S `0x3010_0000-0x3017_FFFF` | 512 KiB | `(0x2010_0000, 0x2017_FFFF, NS)` |
| Flash | `0x5020_8000` | `0x100B_7B28` | `0x3000_1AE0` (cfg `0x100B_7C44`) | NS `0x0000_0000-0x00FF_FFFF`, S `0x1000_0000-0x10FF_FFFF` | 16 MiB | `(0x0010_0000, 0x00FF_FFFF, NS)` |

- There is no NS alias: the base addresses are only reached secure, and 0x4020_4000.. was
  never touched.
- IRQ: **7** (`MPC_Handler`), shared by all four. It is enabled in NVIC_ISER0 (bit 7) by
  `nvic_interrupt_enable` (0x100b41aa) and kept Secure by `nvic_interrupt_target_state_cfg`
  (0x100b41f4: ITNS0 = all ones, then bits 7 and 8 cleared).
- **The vector-7 handler (0x100b1599) is only the generic "unexpected interrupt" stub
  (0x100b0e84).** If MPC IRQ 7 is ever raised, the firmware treats it as fatal.

CMSIS `ARM_DRIVER_MPC` table layout. The flash MPC's table at 0x100b7b28 is shown, with the
low-level `mpc_sie200_*` function each entry calls:

| idx | entry | wrapper → low level | registers |
|---|---|---|---|
| 1 | Initialize | 0x100b1a51 → 0x100b3d34 `mpc_sie200_init` | none: stores the range list, sets the "initialized" byte |
| 3 | GetBlockSize | 0x100b19d5 → 0x100b3bd4 | reads BLK_CFG; returns `1 << (BLK_CFG+5)` |
| 4 | GetCtrlConfig | 0x100b19fd → 0x100b3bfc | reads CTRL |
| 5 | SetCtrlConfig | 0x100b1a9d → 0x100b3dac | writes CTRL (plain store) |
| 6 | ConfigRegion | 0x100b1979 → 0x100b39ec `mpc_sie200_config_region` | BLK_CFG, BLK_MAX (through `get_lut_masks` 0x100b2cf4), BLK_IDX, BLK_LUT |
| 7 | GetRegionConfig | 0x100b1a25 → 0x100b3c1c | BLK_CFG, BLK_IDX, BLK_LUT (reads) |
| 8 | EnableInterrupt | 0x100b19b1 → 0x100b3d64 | INT_EN \|= 1 |
| 9 | DisableInterrupt | 0x100b19a5 → 0x100b3d54 | INT_EN &= ~1 |
| 10 | ClearInterrupt | 0x100b196d → 0x100b39e0 | INT_CLEAR = 1 |
| 11 | InterruptState | 0x100b1a81 → 0x100b3d80 | INT_STAT & 1 |
| 12 | LockDown | 0x100b1a8d → 0x100b3d8c | CTRL \|= 0x8000_0100 |

The CMSIS error map is at 0x100b793c: low-level codes 0..6, anything above 6 becomes -11.

### Registers touched (identical for all 4 instances)

| off | name | access seen | when / who |
|---|---|---|---|
| +0x00 | CTRL | RMW `\|= 0x8000_0100` (bit 31 SEC_LOCK_DOWN, bit 8 AUTOINC); read by GetCtrlConfig (never called) | LockDown, from `mpc_init_cfg` 0x100b39ba.. (each MPC once per boot) |
| +0x10 | BLK_MAX | R | `get_lut_masks` 0x100b2e64 (every `ConfigRegion`/`GetRegionConfig`) |
| +0x14 | BLK_CFG | R | `get_lut_masks` 0x100b2e12, GetBlockSize, GetRegionConfig |
| +0x18 | BLK_IDX | W before **every** LUT read or write (absolute index) | `config_region` 0x100b3a52..0x100b3ba6 |
| +0x1C | BLK_LUT | R (first and last word, for read-modify-write), W | same |
| +0x20 | INT_STAT | R bit 0 (InterruptState) | **[static]**, not called during boot |
| +0x24 | INT_CLEAR | W 1 | **[static]** (ClearInterrupt), not called during boot |
| +0x28 | INT_EN | RMW `\|= 1` | `nvic_interrupt_enable` 0x100b4170 (all 4) |
| +0x2C/+0x30/+0x34 | INT_INFO1/2, INT_SET | never | |

Never accessed: the PIDR/CIDR registers. BLK_IDX is never read.

#### `get_lut_masks` (0x100b2cf4): what the geometry has to satisfy

```
100b2e12: ldr  r0, [r5, #20]     ; BLK_CFG
100b2e14: adds r1, r0, #5        ; log2(block size)
100b2e3e: lsr.w r3, r0, r1       ; limit block = (limit - range.base + range.offset) >> log2
100b2e42: lsr.w r7, r2, r1       ; base  block
100b2e54: lsrs r1, r3, #5        ; limit LUT word
100b2e56: lsrs r2, r7, #5        ; base  LUT word
100b2e64: ldr  r6, [r5, #16]     ; BLK_MAX: both words must be <= BLK_MAX, else -1 -> CMSIS -10
```

Rules the driver enforces:

- **Range.** `base` and `limit` must both lie in the same configured range, and `attr` must
  equal that range's attribute. Otherwise the error is 3, 5 or 6.
- **Alignment.** `base` must be block-aligned and `limit + 1` must be block-aligned
  (error 4).
- **LUT size.** The base and limit LUT word indices must both be `<= BLK_MAX`.
- **Bit meaning.** LUT bit `b % 32` of word `b / 32` set means block `b` is Non-secure.
  `config_region` sets the bits for NS and clears them for S.

### Geometry to report

The firmware does not care about the geometry, as long as the pair is consistent and
`block size <= 64 KiB`, because every `ConfigRegion` base and limit is 64 KiB-aligned. The
real silicon value is unknown (open question 1). Recommended values, which the research run
validated:

| MPC | BLK_CFG | block | blocks | BLK_MAX (= LUT words − 1) | LUT words |
|---|---|---|---|---|---|
| 0x5020_4000 / 5000 / 6000 (512 KiB each) | **7** | 4 KiB | 128 | **3** | 4 |
| 0x5020_8000 (16 MiB) | **7** | 4 KiB | 4096 | **127** | 128 |

LUT contents after boot with this geometry (observed):

- **Flash.** Words 0..7 = 0 (offsets 0x0..0xF_FFFF are Secure). Words 8..127 = 0xFFFF_FFFF.
- **SRAM0.** Word 0 = `0xFFFF_0000`: the first 64 KiB is Secure, which holds the secure
  stacks and data at 0x3000_0000. Words 1..3 = 0xFFFF_FFFF.
- **SRAM1 and SRAM2.** All 0xFFFF_FFFF.

Write order observed for the flash MPC:

- IDX = 8, read LUT, IDX = 8, write; then IDX = 9..0x7E, each followed by a LUT write of
  0xFFFF_FFFF (unrolled by 4);
- IDX = 0x7F, read LUT, IDX = 0x7F, write.

### LUT access pattern and auto-increment

The driver **writes BLK_IDX explicitly before every single BLK_LUT access**. That holds for
the read-modify-write of the first and last word, every middle word, and GetRegionConfig's
reads (0x100b3cde `str r3,[r6,#24]; ldr r4,[r6,#28]`). So it does not depend on
auto-increment either way.

- CTRL bit 8 (AUTOINC) is 0 throughout configuration and is set only by LockDown, after
  which the LUT is never written again.
- If the model implements auto-increment (BLK_IDX += 1 after each BLK_LUT access while
  CTRL.8 = 1, as in the SIE-200 TRM), it must still honour an explicit BLK_IDX write
  immediately. Nothing the firmware does can observe the difference.

### CTRL bits

| bit | name (SIE-200) | firmware use |
|---|---|---|
| 4 | SEC_RESP (0 = RAZ/WI, 1 = bus error) | never written. TF-M's `ARM_MPC_*` SetCtrlConfig is never called |
| 8 | AUTOINC | set by LockDown (with bit 31) |
| 31 | SEC_LOCK_DOWN | set by LockDown. Afterwards the firmware never writes CTRL or the LUT again. A model should make CTRL[31] sticky and ignore LUT/CTRL writes once it is set (TRM behaviour). This is optional for boot |

```
100b3d98: ldr r0, [r0, #0]       ; dev->cfg
100b3d9a: ldr r0, [r0, #0]       ; base
100b3d9c: ldr r1, [r0, #0]       ; CTRL
100b3d9e: orr.w r1, r1, #0x80000000
100b3da2: orr.w r1, r1, #0x100
100b3da6: str r1, [r0, #0]       ; lock down
```

`mpc_init_cfg` (0x100b38f4), as seen in the run:

1. Flash: Initialize, ConfigRegion.
2. SRAM0, SRAM1, SRAM2: Initialize, ConfigRegion each.
3. PPB `0xE004_3018 |= 2` (§4).
4. SPCTRL `0x5020_1080 |= 1`.
5. LockDown on Flash, SRAM0, SRAM1, SRAM2.
6. `dsb; isb`.

### Minimal MPC model (Rust `Device`, 0x1000 window each)

```
state: ctrl=0, idx=0, lut=[0; blk_max+1], int_en=0, int_stat=0
read(off):
  0x00 -> ctrl
  0x10 -> blk_max      (3 SRAM / 127 flash)
  0x14 -> blk_cfg      (7)
  0x18 -> idx
  0x1C -> v = lut[idx]; if ctrl&0x100 { idx = min(idx+1, blk_max) }; v
  0x20 -> int_stat (0; bit0 only if enforcement ever flags a violation)
  0x28 -> int_en
  0x2C/0x30 -> 0 (INT_INFO1/2)
  else -> 0  (PIDR/CIDR: optional SIE-200 IDs; never read)
write(off, v):
  0x00 -> if !(ctrl>>31) { ctrl = v } else { /* locked: ignore */ }
  0x18 -> idx = v (clamp to blk_max; the firmware never exceeds it)
  0x1C -> if !(ctrl>>31) { lut[idx] = v }; if ctrl&0x100 { idx = min(idx+1, blk_max) }
  0x24 -> if v&1 { int_stat &= !1 }   (INT_CLEAR, write-1-to-clear)
  0x28 -> int_en = v & 1
  0x34 -> (INT_SET) if v&1 { int_stat |= 1 }  (never used)
irq 7 = any(MPC.int_stat & int_en): keep it permanently low unless enforcement exists.
```

Reset values: everything 0. Only BLK_MAX and BLK_CFG are constants. The device state may
survive `SYSRESETREQ` or be reset with it: the firmware reprograms everything on every boot,
and the observed runs worked across resets with the state kept.

---

## 2. SPCTRL-like block `0x5020_1000` (Secure Privilege Control, custom SSE-200 derivative)

- Base: S `0x5020_1000`.
- The PPC driver's config also names an "NSPCTRL" base `0x4020_1000`. It sits in RAM
  device configs `0x3000_1A3C/4C/5C` = {`0x5020_1000`, `0x4020_1000`}, but **this
  driver build never uses the NS base**, and 0x4020_1000 is never accessed.
- IRQ: **8** (`PPC_Handler`), which is also only the "unexpected interrupt" stub and is
  enabled in ISER0 bit 8 (Secure).

### Registers touched

| off | SSE-200 name at that offset | this SoC's use | access | function |
|---|---|---|---|---|
| +0x10 | SECRESPCFG | bit 0 = 1: respond to security violations with a bus error | RMW `\|= 1` | `ppc_init_cfg` 0x100b438c (end) |
| +0x14 | NSCCFG | bit 0 CODENSC = 1: the IDAU marks the 0x1xxx_xxxx code region NSC-capable | RMW `\|= 1` | `sau_and_idau_cfg` 0x100b46ce |
| +0x20 | SECPPCINTSTAT | PPC IRQ status; bits = int masks below | R (InterruptState) **[static]** | 0x100b44a0 |
| +0x24 | SECPPCINTCLR | write-1-to-clear of +0x20 | W mask | `ppc_clear_irq` 0x100b43a0. Observed: W `0x1` (PPC#1) in `nvic_interrupt_enable` |
| +0x28 | SECPPCINTEN | PPC IRQ enables | RMW `\|= mask` / `&= ~mask` | 0x100b4484 / 0x100b4468. Observed: 1, then 3, then 7 |
| +0x60 | (AHBNSPPCEXP0) | **PPC#0 NS-attribute bits** (1 = Non-secure) | RMW | `ppc_config_periph` 0x100b43b4 |
| +0x64 | (AHBNSPPCEXP1) | **PPC#1 NS-attribute bits** | RMW | same |
| +0x68 | (AHBNSPPCEXP2) | **PPC#2 NS-attribute bits** | RMW **[static]** | same |
| +0x70 | (APBNSPPC0) | **PPC#0 unprivileged-access bits** (1 = unprivileged allowed) | RMW | same |
| +0x74 | (APBNSPPC1) | **PPC#1 unprivileged-access bits** | RMW | same |
| +0x78 | (APBNSPPCEXP?) | **PPC#2 unprivileged-access bits** | RMW **[static]** | same |
| +0x80 | (APBNSPPCEXP0) | unknown control: bit 0 is set once in `mpc_init_cfg`, just before the MPC LockDowns | RMW `\|= 1` | 0x100b39a4 |
| +0xC0 | (APBSPPPCEXP0) | read-only lifecycle/provisioning status: `(v & 0x50) != 0` selects the provisioned key sources (table below) | R **[static]** | 0x100b6a34, called from 0x100b6748 (OTP read service) and 0x100bfd6a (attestation/boot data) |
| +0xD4 | (custom) | deep-sleep control: W `0x10` before WFI, W `0x100` after wake | W **[static]** | 0x100b4750, called from 0x100d83e2/0x100d83f8 (Power partition sleep) |

### +0xC0: what each value selects

`0x100b6a34` returns `(SPCTRL+0xC0 & 0x50) != 0`. It has two consumers:

| caller | `!= 0` (provisioned) | `== 0` (model value) |
|---|---|---|
| 0x100b6748 (OTP read service) | source = `0x100bafac(3) + 0x200` = **0x1008_1A00** (flash S alias) | source = RAM default **0x3000_1B20** |
| 0x100bfd6a (attestation/boot data, HUK-like) | source = `0x100bafac(3)` = **0x1008_1800** | source = **0x1800_E000** (unmapped; reads 0) |

`0x100bafac(n)` returns `0x100bd27c() + n·0x800`, and `0x100bd27c` returns the constant
0x1008_0000. Both branches are guarded by the 0x5A5A_5A5A fault-injection pattern, and both
are followed by a read of the chip UID (`cru_get_uid64`, CRU +0x0C/+0x10).

```
100bfd6a: bl   0x100b6a34         ; (SPCTRL+0xC0 & 0x50) != 0 ?
100bfd6e: cbz  r0, 0x100bfd7a
100bfd70: movs r0, #3
100bfd72: bl   0x100bafac         ; 0x1008_0000 + 3*0x800
100bfd7a: movw sl, #0xe000
100bfd7e: movt sl, #0x1800        ; 0x1800_E000
```

**Model.** +0xC0 reads 0. Leave 0x1800_xxxx unmapped: it reads 0, which the emulator
already does, and the unmapped log shows the reads at 0x1800_E000.. (byte and word reads
from 0x1800_E064 up were seen). The derived keys are deterministic, though not the
device's. Do **not** map 0x1800_0000 as a flash alias: it is outside the 16 MiB flash S
window (0x1000_0000-0x10FF_FFFF).

### The PPC layout is custom: 3 PPCs, each with two 32-bit registers

`ppc_init` (0x100b4420) builds the data struct as `{p_ns, p_sp, p_nsp (= p_sp), int_mask,
state}`:

| PPC idx | CMSIS table | RAM dev / data | NS reg | priv reg | int_mask (in +0x20/24/28) |
|---|---|---|---|---|---|
| 0 | `0x100B_79E8` | `0x3000_1A44` / `0x3000_1F50` | +0x60 | +0x70 | `0x4` |
| 1 | `0x100B_7A10` | `0x3000_1A54` / `0x3000_1F64` | +0x64 | +0x74 | `0x1` |
| 2 | `0x100B_7A38` | `0x3000_1A64` / `0x3000_1F78` | +0x68 | +0x78 | `0x2` |

The table of the 3 drivers is at `0x100B_9044`.

```
100b443c: ldr  r0, [r0, #4]      ; data
100b443e: add.w r1, r2, #96      ; p_ns  = spctrl + 0x60   (idx 0)
100b4442: adds r2, #112          ; p_sp  = spctrl + 0x70
100b4444: movs r3, #4            ; int mask
100b4452: strd r1, r2, [r0]      ; p_ns, p_sp
100b4456: strd r2, r3, [r0, #8]  ; p_nsp = p_sp, mask
```

`ppc_config_periph(pos, sec, priv)` (0x100b43b4):

- **NS (`sec != 0`).** `NS |= 1<<pos`. Then the priv reg: `priv == 1` → `&= ~(1<<pos)`,
  else `|= (1<<pos)`.
- **Secure (`sec == 0`).** `NS &= ~(1<<pos)`. The priv reg follows the same rule.

### What boot writes (observed, `ppc_init_cfg` 0x100b4278)

1. PPC#1: positions 0..9, 11 and 12 are set NS and position 10 Secure, all priv-only.
   - Result: **+0x64 = 0x0000_1BFF**, +0x74 = 0.
2. PPC#2: Initialize only, no config. +0x68/+0x78 stay at their reset values.
3. PPC#0: positions 0, 4, 6 and 3 are set NS and position 1 Secure.
   - Result: **+0x60 = 0x0000_0059**, +0x70 = 0.
4. SECRESPCFG `|= 1`.

Later, `nvic_interrupt_enable`:

- PPC#1 ClearInterrupt (+0x24 = 1), then Enable (+0x28 `|= 1`);
- PPC#2 Enable (`|= 2`);
- PPC#0 Enable (`|= 4`).

There is also a **[static]** path, `tfm_hal_bind_boundary` (0x100b629e/0x100b62aa). It
calls `ppc_configure_to_secure` (0x100b4258/0x100b4238 → ConfigPeriph(pos, S, priv-only))
for partition-owned MMIO that carries a PPC index. Under isolation level 1 nothing was
written in the run.

The mapping from PPC position to peripheral is unknown (open question 2).

### Minimal SPCTRL model

- Plain 32-bit read/write storage for 0x000..0xFFF. Reset value 0 everywhere.
- **Exception: +0x24 is write-1-to-clear on +0x20.** A write to +0x24 clears those bits in
  +0x20 and is not stored (it reads 0).
- **+0x20 is read-only** (the hardware sets it). It stays 0 unless PPC enforcement is
  modelled.
- **+0xC0 is read-only 0** (the unprovisioned branch of each consumer above).
- IRQ 8 = `(+0x20 & +0x28) != 0`. Keep it low.
- Optional fidelity: let the IDAU consult NSCCFG (+0x14 bit 0 CODENSC for 0x1xxx_xxxx,
  bit 1 RAMNSC for 0x3xxx_xxxx).
  - Today `armv8m::DefaultIdau` reports the whole Secure half as NSC-capable regardless.
  - Boot is unaffected, because CODENSC is set (0x100b46ce) right after the SAU is
    programmed and before any NS code runs.

---

## 3. Clock/reset/system controller (CRU) `0x5020_0000`

- Base: S `0x5020_0000`. It is reached only through the handle
  `*(0x100B_7C8C) = 0x100B_7C88`, `*0x100B_7C88 = 0x5020_0000`. No NS alias is used.
- IRQ: none is serviced. IRQ 25 `PLL2` and IRQ 21 `Wakeup` are unexpected-IRQ stubs.

### Driver functions (all at 0x100b5b04..0x100b5f60) and their callers

| addr | inferred name | registers / semantics | callers |
|---|---|---|---|
| 0x100b5eec | `cru_system_init` (body of SystemInit) | see the sequence below | SystemInit 0x100b1ff4 |
| 0x100b5bac | `cru_get_sysclk` | `+0xFC`, `+0x04`, `+0x1C` | SystemCoreClockUpdate 0x100b1fc2, SystemInit 0x100b2008 |
| 0x100b5c44 | `cru_get_pclk` | sysclk `>> +0x04[18:16]` | 0x100b1fd0, 0x100b2016 |
| 0x100b5cbc | `cru_get_refclk` | `+0xFC[31:16] == 0x0011` → 24 MHz, else 48 MHz | 0x100b1fd6, 0x100b201c |
| 0x100b5d2c | `cru_is_id_0011` | `(+0xFC & 0xFFFF0000) == 0x0011_0000` | 0x100b4732 (LED pattern generator: delay 5 vs 200, §6) |
| 0x100b5b68 | `cru_clk_enable(mask, on)` | `mask[16:0]` → `+0x14`, `mask[31:17]` → `+0x18` (shifted down by 17); set if `on`, else clear | `ti_platform_init` 0x100b27b2..0x100b2812 (11 calls) |
| 0x100b5d50 | `cru_periph_reset(mask, assert)` | `+0x20`: `assert == 0` → `\|= mask` (release), else `&= ~mask` | 0x100b284c `(0x10, 0)`, just before the 0x4010_4000 writes |
| 0x100b5dc0 | `cru_pll_enable` | see below; polls `+0x1C` bit 1 with **no timeout** | 0x100d83f0 (deep-sleep resume) **[static]** |
| 0x100b5b24 | `cru_variant_bit30_clear` | returns `!(+0x10 bit 30)` | 0x100bcca2 (HW revision = 9 + that) **[static]** |
| 0x100b5cec | `cru_get_strap` | `+0x10[29:25]` | 0x100d7d9c, used only when OTP/ITS item 50 is 0 **[static]** |
| 0x100b5d00 | `cru_get_uid64` | returns `r0 = +0x0C`, `r1 = +0x10[19:0]` | 0x100bd28c, 0x100bfd82 (attestation/boot data), 0x100d7a50 (Power service) **[static]** |
| 0x100b5d18 | `cru_get_wake_status` | `+0x08 & 0xFF` | 0x100d7a92, 0x100d83e6 **[static]** |
| 0x100b5b54 | `cru_clear_wake_status` | `+0x08 = 0` | 0x100d7ab8 **[static]** |
| 0x100b5ca8 | `cru_get_reset_cause` | `+0x60 & 0xFF` | 0x100d7d48 **[static]** |
| 0x100b5b40 | `cru_clear_reset_cause` | `+0x60 = 0` | 0x100d7ac0 **[static]** |
| 0x100b5c04 / 0x100b5c24 | `cru_get_dom_mode(i)` / `cru_get_dom_flag(i)` | `+0x70` nibble i: bits [2:1] / bit 3 | 0x100d7cb2 / 0x100d7ca4 **[static]** |
| 0x100b5d74 | `cru_set_dom(i, flag, mode)` | `+0x70` nibble i = `flag<<3 \| (mode&3)<<1` (bit 0 cleared) | 0x100d7d0c **[static]** |
| 0x100b5e9c | `cru_wake_en(n, v)` | `+0x4C` bit n = v | 0x100d84f0 **[static]** |
| 0x100b5ec0 | `cru_wake_cfg_hi(n, v)` | `+0x4C` bit (13+n) = v | 0x100d8442 **[static]** |
| 0x100b5e38 | `cru_wake_cfg2(n, v)` | `+0x50` bit n = v | 0x100d847c **[static]** |
| 0x100b5e5c | `cru_get_wake_cfg(&a, &b, &c)` | a = `+0x4C[25:13]`, b = `+0x4C[12:0]`, c = `+0x50[12:0]` | 0x100d8494 **[static]** |
| 0x100b5b04 | `cru_set_lp_mode(v)` | `+0x94` bit 8 = v | 0x100d83dc (1), 0x100d8404 (0) **[static]** |

### Register map

| off | name (inferred) | bits used | access | reset/expected value |
|---|---|---|---|---|
| +0x00 | SYSCTRL? | bit 6, then bit 7 set | RMW `\|= 0x40`; `\|= 0x80` (SystemInit) | 0 |
| +0x04 | CLKSEL / DIV | bit 0 = sysclk from PLL; [10:8] = HCLK shift; [18:16] = PCLK shift | R (clock getters); RMW `&= ~1` / `\|= 1` (`pll_enable`) | **`0x0002_0001`** (post-BL2 state: 156 MHz sysclk, 39 MHz PCLK) |
| +0x08 | WAKE_STATUS (likely) | [7:0]; decoded bit0→0, bit1→1, bit6→2, bit3→3, bit4→4, else 5. Non-zero means WFI is skipped in deep sleep | R; W 0 to clear | 0 |
| +0x0C | UID_LO | 32 bits | R | any fixed value |
| +0x10 | CHIPINFO | bit 30 = **chip-variant/revision bit** (not a ready flag); [29:25] = strap/board variant (the hardware product ID, 1); [19:0] = UID_HI | R only | 0x0200_0000 (see below) |
| +0x14 | CLKEN0 | clock-enable bits 0..16 | RMW | 0. After boot: `0x0001_97D0` |
| +0x18 | CLKEN1 | clock-enable bits 17..31, stored shifted (bit k → bit k-17) | RMW | 0. After boot: `0x0000_0160` |
| +0x1C | PLLCTRL | bit 0 = enable, bit 1 = **LOCK (polled)**, [5:4] = post-div M (÷ `32>>M`), [14:8] = N | W 0x3430 then 0x3431; R (getters, poll) | **`0x0000_3433`** post-BL2, i.e. N = 0x34 (52), M = 3 (÷4), enabled and locked |
| +0x20 | PRSTN (reset release, 1 = out of reset) | [20:0]; bit 4 = backlight PWM 0x4010_4000 | RMW: `bfi` low 21 bits = `0x1F_FFEF` (all but bit 4), later `\|= 0x10` | 0 |
| +0x4C | WAKE_CFG | [12:0] enable(?), [25:13] second field (polarity/edge?) | RMW (SystemInit: `\|= 0x4000`, `\|= 0x4`) | 0 |
| +0x50 | WAKE_CFG2 | [12:0] | RMW (SystemInit: `\|= 0x1`, `\|= 0x100`) | 0 |
| +0x60 | RESET_CAUSE (likely) | [7:0]; decoded bit0..bit5 → 0..5, else 6 | R; W 0 to clear **[static]** | 0, or a power-on bit (unknown which) |
| +0x70 | DOMCFG | 8 nibbles: bit 3 flag, [2:1] mode | RMW; SystemInit clears bits 6:5 if `+0x10.30` | 0 |
| +0x94 | LPCTRL | bit 8 = deep-sleep mode select | RMW **[static]** | 0 |
| +0xFC | ID | `[31:16]` compared with `0x0011`; `[15:0]` ignored (masked) | R | **`0x0011_0000`** |

#### Clock formulas (0x100b5bac, 0x100b5c44, 0x100b5cbc)

```
ref    = (ID[31:16]==0x0011) ? 24 MHz : 48 MHz          (cru_get_refclk)
sysclk = (ID[31:16]==0x0011 && CLKSEL.0)
           ? 12 MHz * PLL[14:8] / (32 >> PLL[5:4])      (PLL path)
           : 48 MHz
hclk   = sysclk >> CLKSEL[10:8]       -> SystemCoreClock      (0x3000_19C0)
pclk   = hclk   >> CLKSEL[18:16]      -> SystemCoreClock[1]   (0x3000_19C4)
```

With `+0x04 = 0x0002_0001`, `+0x1C = 0x3433` and `+0xFC = 0x0011_0000`, these give
156 MHz, 39 MHz and 24 MHz. Those equal the link-time values of 0x3000_19C0..C8, so this
is the state BL2 leaves. 0x3000_19C8 is the reference clock that the secure UART driver
uses for its divisor (`uart.md`).

Nothing in TF-M turns the PLL on at cold boot: `pll_enable` is only on the deep-sleep
resume path. `SystemCoreClockUpdate` *is* re-run in `ti_platform_init` (0x100b279a). With
all-zero registers the secure world computes 48/48/48 MHz instead.

```
100b5bbc: ldr  r0, [r2, #4]        ; CLKSEL
100b5bbe: ldr.w r1, [r2, #252]     ; ID
100b5bc2: bics r1, r3              ; & 0xFFFF0000
100b5bc4: cmp.w r1, #0x110000
100b5bd2: ldr  r3, [r2, #4]
100b5bd4: lsls r3, r3, #31         ; CLKSEL.0 -> PLL?
```

#### What the CRU reset values change downstream

`SystemCoreClockUpdate` (0x100b1fc0) writes 0x3000_19C0/C4/C8 from the CRU getters, at
SystemInit and again in `ti_platform_init` (0x100b279a). The register values other docs
expect hold **only** with the recommended CRU values:

| consumer | formula | with `+0x04 = 0x0002_0001`, `+0x1C = 0x3433`, `+0xFC = 0x0011_0000` | with all-zero CRU |
|---|---|---|---|
| SPI CTRL1[15:0] (`spi-panel.md`) | `PCLK/(2·1 MHz) − 1` | 39 MHz → **18 (0x12)** | 48 MHz → 23 |
| Secure UART divisor (`uart.md`) | `ref/(16·115200)` | 24 MHz → **13** | 48 MHz → 26 |
| TIMER (`timer.md`) | fixed 32,768 Hz path | unchanged | unchanged |
| NS BSP UART | hard-coded 24 MHz (0x2001_0C80) | 13 | 13 |

Nothing fails either way. Use the recommended values, so that traces match real hardware.

#### `cru_system_init` (0x100b5eec), run in SystemInit before scatter-load (observed)

```
+0x20 = (+0x20 & ~0x1FFFFF) | 0x1FFFEF
+0x14 |= 0x400
+0x00 |= 0x40 ; +0x00 |= 0x80
SCB->SCR &= ~SLEEPDEEP
+0x4C |= 0x4000 ; +0x4C |= 0x4
+0x50 |= 0x1    ; +0x50 |= 0x100
if (+0x10 & (1<<30)) +0x70 &= ~0x60     ; else return
```

```
100b5f50: ldr  r1, [r0, #16]       ; CHIPINFO
100b5f52: lsls r1, r1, #1          ; bit 30 -> N
100b5f54: it   pl
100b5f56: bxpl lr                  ; bit 30 clear: done
100b5f58: ldr  r1, [r0, #112]      ; DOMCFG
100b5f5a: bic.w r1, r1, #96
100b5f5e: str  r1, [r0, #112]
```

+0x10 bit 30 is a chip-variant bit, which 0x100bcca2 also reads as "HW revision 9 or 10".
Nothing waits on it (`discovery-log.md` called it a status flag; corrected there).

#### Clock-enable calls in `ti_platform_init` (0x100b2798, observed)

Clock gates set: bits 6, 15, 7, 8, 16, 22, 12, 9, 4, 25 and 23.

- +0x14 goes from 0x400 to `0x197D0` (bits 4, 6, 7, 8, 9, 10, 12, 15, 16).
- +0x18 = `0x160` (bits 22, 23, 25, stored as 5, 6, 8).

The order sits between the 0x4010_5000 (GPIO) setup and the timer init.

#### `+0x20` bit 4 releases the backlight PWM

0x100b284c `cru_periph_reset(0x10, 0)` releases bit 4. The next instructions
(0x100b2850..0x100b285e) write 0x4010_4000 `+0x00 = 0xA0` and `+0x04 = 0xFF` (`lcdc.md`,
backlight PWM).

#### `cru_pll_enable` (0x100b5dc0): the only polled bit

- Steps:
  1. `+0x04 &= ~1` (back to the reference clock);
  2. `+0x1C &= ~1`;
  3. `+0x1C = 0x3430`;
  4. 20 `nop`s;
  5. `+0x1C = 0x3431`;
  6. poll until `+0x1C` bit 1 is set (no timeout);
  7. `+0x04 |= 1`.
- **The model must report LOCK (bit 1) = ENABLE (bit 0)**, at once or after a short delay.
  Otherwise deep-sleep resume hangs.

```
100b5e0c: movw r1, #0x3431
100b5e10: str  r1, [r0, #28]       ; PLL enable
100b5e12: ldr  r1, [r0, #28]
100b5e14: lsls r1, r1, #30         ; bit 1 -> N
100b5e16: bmi.n 0x100b5e2c         ; locked
100b5e2a: bpl.n 0x100b5e12         ; spin
100b5e2c: ldr  r1, [r0, #4]        ; then CLKSEL |= 1
```

#### Power-partition deep sleep (0x100d83d4..0x100d8408) **[static]**

The sequence:

1. `SCR.SLEEPDEEP = 1`.
2. CRU `+0x94` bit 8 = 1.
3. SPCTRL `+0xD4 = 0x10`.
4. If `(CRU+0x08 & 0xFF) == 0`, `WFI`.
5. `cru_pll_enable()`.
6. SPCTRL `+0xD4 = 0x100`.
7. `SLEEPDEEP = 0`.
8. CRU `+0x94` bit 8 = 0.

So a wake event (keypad, OnKey, RTC, ...) has to be visible as an NVIC interrupt (to end the
WFI), and the PLL has to report lock afterwards. +0x08 can stay 0 unless a model wants to
model "pending wake skips the sleep".

### Minimal CRU model

- Storage-backed 32-bit registers over 0x000..0xFFF, with these exceptions:
  - `+0xFC`: read-only `0x0011_0000`.
  - `+0x1C`: stored; on read, `bit1 = bit0`.
  - `+0x10`, `+0x0C`: read-only constants. `+0x10 = 0x0200_0000`: bit 30 clear (the
    HW-rev-10 path) and strap [29:25] = 1 (Task 24, below). `+0x0C` is any UID.
  - `+0x08`, `+0x60`: storage, reset 0, cleared by the firmware writing 0 (plain store
    semantics suffice).
- Reset values: `+0x04 = 0x0002_0001` and `+0x1C = 0x0000_3433`; everything else 0. Keep
  the values across `SYSRESETREQ`: the firmware re-applies its RMWs idempotently.
- No IRQs. No side effects needed: clock gates and resets can be logged and ignored. A
  later refinement could hold a peripheral in reset while its `+0x20` bit is 0, but the
  firmware never relies on that.

---

## 4. PPB `0xE004_3018`

- Only access: in `mpc_init_cfg`, the RMW `|= 2` (0x100b3992..0x100b39a2), after the four
  ConfigRegions and before the MPC lockdowns. It is the only `0xE004_xxxx` access in
  either image. The other `0xE004_xxxx` literal words are code or data bytes, never loaded
  as addresses.

  ```
  100b3992: movw r0, #0x3018
  100b3996: movt r0, #0xe004
  100b399a: ldr  r1, [r0, #0]
  100b399c: ldr  r2, [r5, #48]       ; flash MPC LockDown ptr
  100b399e: orr.w r1, r1, #2
  100b39a2: str  r1, [r0, #0]
  ```

- 0xE004_0000+ is the Cortex-M33 external PPB, so this is a vendor register.
- Plausible role: a bus-matrix or flash-interface security-enable bit set alongside
  SPCTRL+0x80 bit 0. Nothing reads it back.
- **Model: RAZ/WI is sufficient.** Make the access visible (log it) so it is not silent.

---

## 5. Must MPC/PPC filtering be enforced for boot? **No.**

Evidence:

1. The firmware never reads the LUT back during boot, and never reads BLK_IDX, INT_STAT,
   INT_INFO or SECPPCINTSTAT. GetRegionConfig and InterruptState exist but are not called
   on the boot path.
2. TF-M's memory checks (`tfm_hal_memory_check` / `cmse_check_address_range`) use `TT`,
   i.e. the SAU and IDAU in the CPU, not the MPC.
3. The MPC and PPC IRQs (7, 8) are routed Secure and enabled, but their handlers are the
   fatal "unexpected interrupt" stub. A model that raised them, even correctly for a
   genuine violation, can only cause a panic, never progress.
4. The PPC-position-to-peripheral mapping is unknown, so enforcing NS/S attributes on
   peripherals risks false faults.
   - The secure code itself drives the UART through its NS alias (0x4010_6000).
   - PPC#1 pos 10 and PPC#0 pos 1 end up Secure.
5. The research run reached the banner, all secure partitions and the ITS step with no
   enforcement.

If enforcement is added later for debugging, these semantics apply:

- **Transaction security.** It comes from address bit 28 (the alias), not the CPU state.
- **MPC.** A Non-secure access to an S block, and a Secure access through the S alias to
  an NS block, are blocked: RAZ/WI, or a bus error if SECRESPCFG.0 / CTRL.4.
  - The firmware's own pattern of reading NS data through the NS alias is consistent with
    that.
  - Do this **log-only, no fault, no IRQ**, and only after `LockDown`.
  - Before `mpc_init_cfg` the LUT is all 0 (all Secure), and blocking NS accesses then
    would be correct but pointless.

---

## 6. LED pattern generator `0x5020_2B00`

A two-channel status-LED pattern generator. It is **reached on every boot**; it is not SPIF
clocking or QSPI drive.

- It shares a device struct with the SHA engine: `0x3000_1AF8 = {cfg 0x100B_7C70 →
  0x5020_2B00, 0x100B_7C74 → 0x5000_2000}` (platform config table at 0x100b7c68).
- **Writes** of `pattern_set(idx)` (0x100b46e8), as logged on every boot:

  | PC | access |
  |---|---|
  | 0x100b4704 | W +0x08 = 0xFFFF_F800 |
  | 0x100b4706 | W +0x10 = 0xFFFF_F800 |
  | 0x100b471a | W +0x04 = pattern[idx].a |
  | 0x100b471e | W +0x0C = pattern[idx].b |
  | (then `delay_ms(5)`) | |
  | 0x100b4730 | W +0x00 = 0x8000_0603 |
  | (then `delay_ms(5 or 200)`, depending on `cru_is_id_0011`) | |
  | 0x100b474c | W +0x00 = 0x8000_1643 |

- **Callers.** All callers of `pattern_set` are in one TF-M partition: 0x100d85e8,
  0x100d861c, 0x100d86e4, 0x100d86f2, 0x100d871e and 0x100d87f0. None is in the SPIF
  driver.
  - The boot path runs `pattern_set(9)`, then `delay_ms(1900)` (0x100d86e8), then
    `pattern_set(0)`.
  - 0x100d861c maps a state byte through a 7-entry table at 0x100d8678 to pattern indices.
- **Pattern table.** A 10-entry table at 0x100b8184, not reproduced, holds `{a, b}` per
  index; an index above 9 is forced to 0. The two rows boot uses:

  | idx | +0x04 (a) | +0x0C (b) |
  |---|---|---|
  | 0 | 0 | 0 |
  | 9 | 0xFF00_FF00 | 0x00FF_FF00 |

- **Interpretation.** Two 32-step on/off sequences (+0x04 and +0x0C), each with a
  mask/length word (+0x08 and +0x10 = 0xFFFF_F800). +0x00 is a control word (bit 31 =
  enable): 1.9 s of pattern 9 at boot, then off, with later states from a PSA message.
- **Model.** Plain storage over 0x5020_2B00..0x5020_2BFF, reset 0, never read, no IRQ. No
  other offset in 0x5020_2xxx is touched. A front end may render it as an LED.

---

## Open questions

1. **Real MPC geometry** (BLK_CFG/BLK_MAX). The firmware is agnostic: any block ≤ 64 KiB
   with a matching BLK_MAX works. **Decided (emulator):** `evo_hw::dev::mpc::Mpc` reports
   BLK_CFG = 7 (4 KiB blocks) on every instance, BLK_MAX = 3 on the three SRAM MPCs and
   127 on the flash MPC (§1 "Geometry to report"). A dump from real hardware would still
   settle the silicon value; nothing in the firmware can observe the difference.
2. **PPC position → peripheral map** for PPC#0/#1/#2: which peripherals are PPC#1 pos 10
   and PPC#0 pos 1 (left Secure)? Needed only if PPC enforcement is ever modelled.
3. **CRU +0x08 vs +0x60:** which is the wake cause and which the reset cause, and which bit
   means power-on reset? The Power-partition enum decode is known (above); the bit
   semantics are not.
4. **CRU +0x10 bit 30 on real units:** is the variant bit set on shipping hardware
   (revision 9 vs 10)? Bit 30 = 1 clears DOMCFG bits 6:5 in SystemInit. **Strap [29:25]
   (decided, Task 24): 1.** With OTP item 50 reading 0 (the OTP is not modelled), the Info
   partition init (0x100d7d74..0x100d7da0) stores `cru_get_strap()` at S 0x3000_09F0 as the
   hardware product ID; secure service 0x4000_0106 type 16 returns it, and the OS service
   (0x4000_0107) rejects image 1 unless it equals the image's protected TLV 0xA0
   (0x100bdccc..0x100bdcf6, check 0x100b0ee0), which is 1 in OS 7.1. The real value for the
   Evo-T or other models is unknown.
5. **CRU +0x00 bits 6/7, the other +0x20 bit assignments, and the +0x14/+0x18 clock-gate bit
   → peripheral map.** Only +0x20 bit 4 (backlight PWM) is known.
6. **SPCTRL +0x80 bit 0, +0xC0 (bits 4, 6) and +0xD4 (0x10 / 0x100)** are custom. Only
   their use sites are known.
7. **PPB 0xE004_3018 bit 1**: unknown vendor register.
8. **Does `SYSRESETREQ` reset the CRU/SPCTRL/MPCs on silicon?** The firmware works either
   way, but `+0x04`/`+0x1C` (the PLL state left by BL2) would matter if they reset to 0.
   **Decided for the MPCs (emulator):** a System reset keeps CTRL (lock-down included),
   BLK_IDX, the LUT and INT_EN; only a PowerOn reset clears them. **Decided for the CRU and
   SPCTRL (emulator):** `evo_hw::dev::sysctl::{Cru, Spctrl}` keep every register across a
   System reset; a PowerOn reset restores the reset values (CRU `+0x04 = 0x0002_0001`,
   `+0x1C = 0x3433`, everything else 0). The CRU's read-only `+0x0C` UID_LO is `0x4556_4F31`
   ("EVO1") and `+0x10` CHIPINFO is `0x0200_0000` (strap [29:25] = 1, see the CRU table).
   The LED pattern generator (§6) and the ICACHE (`spif-nor.md` §7) are plain storage
   (`evo_hw::dev::storage::Storage`) that either reset clears to 0.
9. **QSPI drive strength.** The PSA service types 14/15 ("QSPI Drive Settings") reach a
   handler path at 0x100baf1c/0x100bb0b0 whose target block is not identified. It is not
   0x5020_2B00 (§6).
