# SPIF (QSPI NOR controller), attached NOR flash, and ICACHE: register map and model

Bundle: OS 7.1.0.4421. Secure image disassembled at VMA 0x100B_0000 (package offset 0). NS OS
image disassembled at VMA 0x0038_0000. All addresses below are secure-image addresses unless
marked otherwise.

Method: static disassembly, then a dynamic check in a throwaway emulator build with register
fakes, which added the SPIF model of §8 and logged every SPIF/ICACHE access. With this model,
boot gets past the reset loop of `discovery-log.md`:

```
Creating an empty ITS flash layout.
Creating an empty PS flash layout.
[DBG][Crypto] Init Mbed TLS 3.6.2... complete.
```

There are 0 resets in 150M cycles. The next stall is the TRNG (`crypto.md`), polled at
`[r2,#0x130]` in 0x100b55a4. Before the two fixes in §5.2 and §8.4 (FIFO level as a multiple
of 4; level-triggered START), the same build reset every ~4M cycles.

---

## 1. Blocks, aliases, IRQs

| block | secure base | NS alias | IRQ | used by |
|---|---|---|---|---|
| SPIF (QSPI NOR controller) | 0x5000_1000 | 0x4000_1000 (never used) | 22 `SPIF_Handler` | secure image only |
| ICACHE (XIP instruction cache) | 0x5000_5000 | 0x4000_5000 (never used) | 10 `ICACHE_Handler` | secure image only |
| NOR flash, memory-mapped (XIP) | 0x1000_0000-0x10FF_FFFF | 0x0000_0000-0x00FF_FFFF | - | both images (code fetch, `memcpy` reads, blank checks) |

- **IRQs are unused.** Both vector entries are the generic "unexpected interrupt" stubs
  (`adr r0,"SPIF_Handler"; bl 0x100b0e84`, for example at 0x100b1ef0). The driver polls
  everything, so the model never needs to raise IRQ 22 or IRQ 10.
- **The NS OS never touches either block.** An aligned literal scan and a movw/movt-pair scan
  of the NS image find no 0x4000_1xxx/0x5000_1xxx/0x4000_5xxx/0x5000_5xxx constants. The only
  SPIF literal anywhere is 0x5000_1000 at 0x100b7c68, in a const config table. ICACHE is
  built with movw/movt (0x5000_5004) at four sites.
- **The NS side changes QSPI settings only through a PSA service.** The diagnostic menu in
  the NS BSP (VMA 0x1010bb18 / 0x1010bc34, "QSPI Clock Divider", "QSPI Drive Settings")
  calls `psa_call(0x40000106, type, ...)` with type 12 (set divider), 14 (get drive),
  15 (set drive) or 18. The secure handler is in 0x100d7bxx.

---

## 2. Driver structure (secure image)

The firmware has three layers.

1. **SPIF HAL.** The context is at RAM 0x3000_1AF0 = `{ *regs_holder, *ops }`.
   `regs_holder[0]` is 0x5000_1000. `ops` holds function pointers that `spif_init` installs:
   - `+0x08` = 0x100b4505, `spif_fifo_read`;
   - `+0x0C` = 0x100b781d, `spif_fifo_write`;
   - `+0x10` = 0x100b5525, `spif_load_cmd_regs`.
2. **Command layer.** One routine per kind of transfer:

   | address | inferred name | what it does |
   |---|---|---|
   | 0x100b1bd4 | `spif_cmd_read` | Command plus data-in. |
   | 0x100b1d8c | `spif_cmd_write` | Command plus data-out. |
   | 0x100b3068 | `spif_cmd_nodata` | Command only. Also used by `spif_read_byte` (0x100b1cf4). |
   | 0x100b1af4 | wrapper | Wraps 0x100b3068. |

   All of them take a 20-byte "command descriptor" by value (§4).
3. **NOR device layer.** The device object is at RAM 0x3000_2C58. Its vtable is filled by
   `nor_probe` (0x100b23e0), and the opcode-to-descriptor builder is `nor_build_cmd`
   (0x100b2898). TF-M's CMSIS `ARM_DRIVER_FLASH` (context at 0x3000_1AA0) sits on top:

   | address | function |
   |---|---|
   | 0x100b0770 | `Initialize` |
   | 0x100b0868 | `ReadData` (a `memcpy` from the XIP window) |
   | 0x100b07c0 | `ProgramData` |
   | 0x100b06d8 | `EraseSector` |
   | 0x100b15d4 | `erase_range` (64K/32K/4K with blank-skip) |

---

## 3. SPIF register map

Offsets are from 0x5000_1000. "Seen" means the value appeared in the dynamic log.

| off | name (inferred) | access by firmware | semantics needed by the firmware |
|---|---|---|---|
| +0x00 | CMD_ADDR | W | Flash address for the command (24-bit; the firmware writes the full word, with upper bits 0). |
| +0x04 | CMD_CFG | W | `[2:0]` address bytes (0 or 3). `[23:16]` dummy cycles (0 or 8). `[24]` instruction phase enable (always 1). `[25]` unknown (always 0). `[28]` unknown (always 0). Layout under §4. |
| +0x08 | CMD_LEN | W | Data-phase length in bytes (0 = none). |
| +0x0C | CMD_OP | W | `[31:24]` opcode. `[23:16]` mode/alt byte (always 0). `[7:5]` data lanes (0 = 1-bit, 2 = quad). `[4]` unknown (always 0). `[3:1]` transfer type (0 = read/none, 1 = write/none, 3 = erase/address-only). |
| +0x10 | CFG0 | RMW | `[1:0]` clock-divider code (div = 2 × (code+1); "Must be 2,4,6, or 8"). `[4]` from a board-config byte (0 in this build). Seen: 0x0000_0000. |
| +0x18 | STATUS | R (polled) | `[0]` TX FIFO has space: the write path waits on it (predicate 0x100b6edc). `[1]` RX FIFO not empty: the read path waits on it (0x100b4644). Read-only. |
| +0x20 | CTRL | W, RMW | Init writes 0x0000_1100. `[1]` START/command-mode: `\|= 2` starts the descriptor loaded in +0x00..+0x0C; `&= ~2` after completion. Bits 8 and 12 come from the init value; their meaning is unknown (perhaps XIP enable). |
| +0x24 | INT_STATUS | R (polled), W1C | `[0]` command DONE. The firmware writes `\|= 1` before each command (W1C clear) and after each command, then polls for `[0] == 1` (predicate 0x100b0eb8). |
| +0x30 | XIP_CFG | RMW once | `v = (v & 0xEFF0_0000) \| 0x100E_B404`. Probably the memory-mapped read command: 0xEB is Fast Read Quad I/O in `[15:8]`; the meaning of `[19:16]=0xE`, `[7:0]=0x04` and `[28]` is unknown. Seen: 0 then 0x100E_B404. |
| +0x34 | XIP_CTRL | RMW, polled | `\|= 0x400`, then poll until `[10] == 0` (5 ms timeout, predicate 0x100b3030) after every flash operation, before the ICACHE invalidate (0x100b1b08). A self-clearing "resume XIP / flush prefetch" request. |
| +0x54 | FIFO_LEVEL | R | `[15:8]` byte count. In a read it is RX bytes available; in a write it is TX bytes free. The firmware uses it as the chunk size (§5.2). |
| +0x100 | DATA (FIFO port) | R/W, 8- or 32-bit | `ldrb` pops 1 RX byte. `ldr` pops 4 (little-endian; the first byte is in `[7:0]`). `strb` pushes 1 TX byte. `str` pushes 4 LE. Only +0x100 itself is used (0x100b4504, 0x100b781c). |

Registers never touched: +0x14, +0x1C, +0x28, +0x2C, +0x38..+0x50, +0x58..+0xFC.

Reset values: the firmware never depends on one. Every register it reads is either written
first or masked RMW. +0x30 keeps bits `[31:29,27:20]` of the old value; +0x34 keeps all
bits except 10. Reset value 0 for everything works (verified).

### Init sequence, `spif_init` (0x100b1b40, called from `nor_init` 0x100b16e8)

```
100b1b9e: movt r4, #0xEFF0           ; mask
100b1ba2: ldr  r3, [r1, #0x30]
100b1ba8: ands r3, r4
100b1baa: movw r4, #0xB404 ; movt #0x100E
100b1bb2: orrs r3, r4
100b1bb4: str  r3, [r1, #0x30]       ; XIP_CFG
100b1bca: mov.w r1, #0x1100
100b1bce: str  r1, [r0, #0x20]       ; CTRL
```

Dynamic order: R+10/W+10 (clear bit 4), R+10/W+10 (bit 4 = cfg), R+30/W+30, R+10/W+10
(clear `[1:0]`), R+10/W+10 (`[1:0]` = divider code), W+20 = 0x1100.

---

## 4. Command descriptor and its encoding

`nor_build_cmd` (0x100b2898) and the inline builders fill a 20-byte descriptor `d`:

| byte | field |
|---|---|
| d[0..3] | address |
| d[4] | address bytes |
| d[5] | dummy cycles |
| d[6] | instruction enable (=1) |
| d[7] bit 0 | unknown |
| d[8..11] | data length |
| d[12] | type |
| d[13] bit 0 | unknown |
| d[14] | data lanes |
| d[15] | mode byte |
| d[16] | opcode |

`spif_load_cmd_regs` (0x100b5524) writes the encoded words. The packing is the same
instruction sequence in 0x100b1bd4, 0x100b1d8c and 0x100b3068:

```
+0x00 = addr
+0x04 = (d[4] & 7) | ((d[5] | d[6]<<8) & 0x3FF) << 16 | (d[7] & 1) << 28
+0x08 = len
+0x0C = d[16]<<24 | d[15]<<16 | (d[14] & 7)<<5 | (d[13] & 1)<<4 | (d[12] & 7)<<1
```

Evidence, from 0x100b1bf8:

```
movs r2,#0 ; movt r2,#0x3FF ; and.w r2,r2,r1,lsl #8   ; d[5..6] -> [25:16]
and.w r6,r1,#7 ; add r2,r6                            ; d[4] -> [2:0]
and.w r1,r1,#0x01000000 ; orr.w r2,r2,r1,lsl #4       ; d[7].0 -> [28]
orr.w r6,r6,ip,lsl #24                                ; opcode -> [31:24]
```

Every command the driver issues. The values were seen dynamically unless marked static.

| op | purpose | +0x04 | +0x08 | +0x0C | data | issuing function |
|---|---|---|---|---|---|---|
| 0x9F | JEDEC Read-ID | 0x0100_0000 | 3 | 0x9F00_0000 | 3 bytes in | `nor_init` 0x100b1764 |
| 0x05 | Read SR1 (WIP poll) | 0x0100_0000 | 1 | 0x0500_0000 | 1 in | 0x100b75ac, poll predicate 0x100b454c |
| 0x35 | Read SR2 | 0x0100_0000 | 1 | 0x3500_0000 | 1 in | 0x100b762c |
| 0x15 | Read SR3 (static) | 0x0100_0000 | 1 | 0x1500_0000 | 1 in | 0x100b75ea |
| 0xFF | Mode-bit reset / exit continuous read | 0x0100_0000 | 0 | 0xFF00_0002 | - | 0x100b17ba (after a successful probe) |
| 0x06 | Write Enable | 0x0100_0000 | 0 | 0x0600_0002 | - | before every erase, program or WRSR |
| 0x04 | Write Disable (static, builder only) | 0x0100_0000 | 0 | 0x0400_0002 | - | - |
| 0x20 | Sector erase 4 KiB | 0x0100_0003 | 0 | 0x2000_0006 | - | `nor_erase` 0x100b71e8 to 0x100b7120 |
| 0x52 | Block erase 32 KiB (static) | 0x0100_0003 | 0 | 0x5200_0006 | - | 0x100b71e0 |
| 0xD8 | Block erase 64 KiB (static) | 0x0100_0003 | 0 | 0xD800_0006 | - | 0x100b71f0 |
| 0xC7 | Chip erase (builder only) | 0x0100_0000 | 0 | 0xC700_0006 | - | - |
| 0x02 | Page program (1-1-1) | 0x0100_0003 | n ≤ 256 | 0x0200_0002 | n out | `nor_program` 0x100b76b0 (dev+8 = 0x100b7804) |
| 0x01 / 0x31 / 0x11 | Write SR1 / SR2 / SR3 (static) | 0x0100_0000 | 1 | 0xXX00_0002 | 1 out | `nor_write_sr` 0x100b7868 |
| 0x03 | Read (1-1-1) (static) | 0x0100_0003 | n | 0x0300_0000 | n in | dev+0 0x100b74a8 |
| 0x6B | Quad output fast read (1-1-4) (static) | 0x0108_0003 | n | 0x6B00_0040 | n in | dev+4 0x100b72fc |
| 0x48 | Read security register | 0x0108_0003 | n | 0x4800_0000 | n in | dev+32 0x100b73b4 |
| 0x42 | Program security register | 0x0100_0003 | n | 0x4200_0002 | n out | dev+36 0x100b77bc |
| 0x44 | Erase security register | 0x0100_0003 | 0 | 0x4400_0006 | - | dev+28 0x100b71f8 |

0x48/0x42/0x44 run in the NV-counter step of secure boot (`boot-flow.md` §2, step 9). Bulk
reads of flash contents never go through SPIF: `ReadData` (0x100b0868) and the blank check
(0x100bcea0) read the XIP window directly. No quad page program (0x32), no 4-byte
addressing, no QPI and no DTR appear anywhere.

---

## 5. Transfer protocol, exactly as the firmware drives it

### 5.1 Common start: `spif_cmd_*` (0x100b1bd4 / 0x100b1d8c / 0x100b3068)

1. `+0x24 |= 1`. This W1C-clears DONE.
2. Write +0x00, +0x04, +0x08, +0x0C, in that order.
3. `+0x20 |= 2`. This starts the command.

### 5.2 Data phase (`len ≥ 1`)

Read path (0x100b1c66 loop):

```
loop: wait_until(STATUS[1], 100 ms)          ; 0x100b4644
      lvl = FIFO_LEVEL[15:8]
      chunk = min(remaining, lvl); remaining -= chunk   (lvl==0 -> re-poll)
      read chunk bytes from +0x100: 32-bit reads while >3 left, then byte reads
```

The write path is the same, but it waits for `STATUS[0]` (predicate 0x100b6edc), treats
`lvl` as free space, and writes to +0x100. Its TX loop at 0x100b1e2a writes
`min(lvl, remaining)` bytes per pass and **spins forever when `lvl` is 0** (no timeout), so
FIFO_LEVEL must report free space while a program is in flight.

> **Firmware quirk (must shape the model):** the buffer pointer advances by 4 after every
> FIFO helper call, including the final 1-3-byte call. If `lvl` is not a multiple of 4 and
> more data remains, the next chunk lands 1-3 bytes too far. The model must report
> FIFO_LEVEL as a multiple of 4 except when it covers the whole remainder. With a 64-byte cap
> (`lvl = min(remaining, 64)`) this always holds. A larger cap breaks reads longer than the
> cap, and security-register reads reach 256 bytes.

### 5.3 Completion

`wait_until(INT_STATUS[0] == 1, timeout)` (predicate 0x100b0eb8), then `+0x24 |= 1` (W1C)
and `+0x20 &= ~2`. On timeout the routine returns -100 **without** clearing CTRL.START, so
the next command writes +0x20 with bit 1 already set.

`spif_read_byte` (0x100b1cf4: RDSR and friends) does things in a different order. It issues
through `spif_cmd_nodata`, which waits for DONE and clears DONE and START, and only **then**
polls `STATUS[1]` and `ldrb +0x100`. RX data must therefore survive DONE, the W1C, and the
clearing of START.

### 5.4 Status poll for program and erase

Predicate 0x100b454c sends RDSR (0x05) and returns `!(SR1 & 1)`. It is retried with
`wait_until(..., 500 ms)`:

- Erase (0x100b7120): WREN, then the erase command, then poll.
- Program (0x100b76b0) polls **before** each page chunk, then sends WREN and PP. It does not
  poll after the last chunk.
- WRSR (0x100b7868): WREN, WRSR, then poll.

`spif_read_byte` returns 0 on error, so a dead controller reads as "not busy".

### 5.5 After every NOR operation (0x100b15d4, 0x100b168c, 0x100b17e8)

```
100b1668: bl   0x100b1b08          ; SPIF+0x34 |= 0x400 ; wait [10]==0 (5 ms)
100b166c: movw r0,#0x5004 ; movt r0,#0x5000
100b1674: ldr  r1,[r0]
100b1676: orr.w r1,r1,#0x1D        ; ICACHE+0x04 |= 0x1D
100b167a: str  r1,[r0]
100b167c: dsb sy ... isb sy
```

All bounded waits use `wait_until` (0x100b6e7c). Its clock is TIMER0 (32.768 kHz,
`ticks = ms*32768/1000`, `timer.md`), so TIMER0 must tick for the timeouts to expire. With a
model that answers immediately, no timeout is ever reached (verified: 0 timeouts).

---

## 6. NOR device: probe, accepted IDs, geometry

### 6.1 Probe (0x100b16e8 → 0x100b23e0)

```
100b1774: ldrb.w r0,[sp,#37]      ; ID byte 0 (manufacturer)
100b1778: ldrb.w r1,[sp,#38]      ; ID byte 1 (memory type)
100b177c: strb.w r0,[r7,#56]
100b1780: movs   r0,#24           ; capacity code forced to 0x18
100b1782: strb.w r0,[r7,#57]
100b1788: strb.w r1,[r7,#58]      ; ID byte 2 (capacity) is ignored
```

- **Any JEDEC ID is accepted.** The probe indexes `{0x16: "4M Part", 0x17: "8M Part",
  0x18: "16M Part"}` (table at 0x100b24ec) with `dev[57] - 22`. Because `dev[57]` is
  hard-coded to 24, the driver always selects **"16M Part"**.
- The probe only fails, giving the 4M default geometry 0x100b79d0 and a disabled driver
  (`dev[72] = 0`), when the Read-ID transfer itself times out. Without a SPIF model this is
  what happens: the erase op stays NULL and ITS init fails (`boot-flow.md` §1).
- The manufacturer byte matters in one place: 0x1C (EON) switches security-register
  addressing (0x100b7426, 0x100b77e4). Everything else assumes Winbond-style opcodes:
  0x35/0x15/0x31/0x11, security registers 0x48/0x42/0x44 at 0x1000/0x2000/0x3000, and
  SR1 `0x24`/`0x04` block-protect presets.

Geometry structs (`{?, sector_count, sector_size, page_size, program_unit, erased_value}`):

| entry | geometry struct | sectors x size | page | erase value | total |
|---|---|---|---|---|---|
| 16M | 0x100b7958 | 0x1000 x 0x1000 | 0x100 | 0xFF | 16 MiB |
| 8M | 0x100b7988 | 0x800 x 0x1000 | 0x100 | 0xFF | 8 MiB |
| 4M | 0x100b7970 | 0x400 x 0x1000 | 0x100 | 0xFF | 4 MiB |

After Read-ID, the probe reads SR2 (0x35) and then SR1 (0x05). It sets
`dev[64] = !(SR1.SRP && SR2.SRL)`, which means "status register writable". On success it
sends 0xFF.

### 6.2 Chosen part: Winbond W25Q128JV-IQ

- JEDEC ID `EF 40 18`. The ID is ignored beyond byte 0, but it is self-consistent.
- 16 MiB, 4 KiB sectors, 32/64 KiB blocks, 256-byte pages.
- 3 security registers of 256 bytes at 0x001000, 0x002000 and 0x003000.
- SR1 reset value 0x00. SR2 reset value 0x02 (QE = 1, factory default for the -IQ part; the
  driver issues 0x6B quad reads). SR3 reset value 0x60 (the Winbond default drive bits; any
  value is accepted).

### 6.3 How the firmware uses flash during boot (dynamic)

- **Sector erases:** 0x20 on every 4 KiB sector of 0x0009_0000-0x000A_FFFF (PS at
  0x0009_0000-0x000A_BFFF, ITS at 0x000A_C000-0x000A_FFFF; 128 KiB). 0x0009_0000 and
  0x000A_C000 are erased twice.
- **Page programs:** 0x02 of 4-32 bytes, split at 256-byte boundaries (for example
  0x0910F8 len 8, then 0x091100). The first ITS program is 12 bytes at 0x0AD008.
- **ITS/PS reads** go through the XIP window, not SPIF read commands. Program and erase must
  therefore update the same bytes the CPU reads at 0x0009_0000-0x000A_FFFF, at once.
- **Security registers:** 0x100b3df4 reads word 28 of each of the 3 security registers
  (magic 0xC0DE_0042, absent = 0xFFFF_FFFF). If a register holds anything else it is erased
  (0x44). Erased registers are accepted, and the firmware then provisions them itself through
  0x42 (NV counters: 8 slots of 32 bytes per register, each ending with the magic at
  slot+28).
- **The mailbox at 0x00CF_0000: a conditional erase.** It is not part of normal boot.
  0x100d78b6..0x100d7902 does this:
  1. It blank-checks 0x2400 bytes at flash 0xCF_0000 (0x100bcea0 compares with 0xFF).
  2. If the bytes are blank, it clears flag 0x3000_09E3 and returns. **No erase.**
  3. If they are not blank, it copies 0x2400 bytes from 0xCF_0000 to RAM **0x2017_DC00**,
     sets the flag, prints a message, and calls `erase_range(0xCF_0000, 0x1_0000)`
     (0x100d78fe). 0x100d796e is the same erase on a second path. `erase_range`
     blank-checks each block through the NS alias before erasing it.

  ```
  100d78d0: movw r0, #0xdc00 ; movt r0, #0x2017
  100d78d8: mov.w r1, #0xcf0000
  100d78dc: mov.w r2, #0x2400
  100d78e0: bl   0x100c8880        ; memcpy(0x2017DC00, 0xCF0000, 0x2400)
  100d78f6: mov.w r0, #0xcf0000
  100d78fa: mov.w r1, #0x10000
  100d78fe: bl   0x100b15d4        ; erase_range
  ```

  This is a one-shot "hand data over to the OS through RAM" mailbox, probably the
  `Restored Saved Data.` path in `discovery-log.md`. **With 16 MiB of 0xFF the path never
  runs.** With an 8 MiB flash, 0xCF_0000 is unmapped and reads 0, looks "not blank", and
  the spurious erase follows. An emulator that wants to test the path pre-loads data at
  0xCF_0000.

---

## 7. ICACHE (0x5000_5000)

| off | access | semantics |
|---|---|---|
| +0x04 | RMW `\|= 0x1D`, followed by `dsb; isb` | Probably bit 0 = enable and bits 2-4 = invalidate/flush. The firmware only ORs and never polls. Sites: `SystemInit` 0x100b1ff8 (first read is 0) and after every NOR operation (0x100b166c, 0x100b16b2, 0x100b1818). In the boot run: 1 + 118 RMWs. |

No other offset is accessed. Model: plain read/write storage, reset value 0. The emulator
has no cache, so an invalidate is a no-op; returning the written value is enough. If
self-clearing invalidate bits are wanted, read bits 2-4 back as 0. The firmware does not care.

---

## 8. Minimal model specification

### 8.1 Flash backing and XIP coherence

- The flash is one **16 MiB** buffer, initialized from the image layout and padded with
  0xFF. It is the `MemoryMap` buffer named `"flash"`.
- The **same** buffer backs the XIP windows 0x0000_0000-0x00FF_FFFF (NS) and
  0x1000_0000-0x10FF_FFFF (S).
- XIP is read-only: bus writes to the window are ignored (or logged) and never program.
- The SPIF reaches the bytes with `DevCtx::buffer_mut("flash")` (`memory-map.md`, "Device
  runtime decisions"). Erase is `fill(0xFF)` over the range and program is
  `buffer[a] &= b`, inside the device's own access, so the next XIP read sees the result at
  once. No cache model is needed, because ICACHE invalidates are no-ops. (Queuing pokes that
  a run loop applies later is correct only while no program and blank check land in the same
  step; it is not used.)
- Security registers: a separate `[[u8; 256]; 3]` inside the SPIF device, initially 0xFF.
  NV counters live there; persisting them across sessions (next to the flash image) is the
  flash file's job (`memory-map.md`, X8).

### 8.2 SPIF state

```
regs: cfg0(+10), xip_cfg(+30), xip_ctrl(+34), ctrl(+20), addr, cmdcfg, len, op : u32
done: bool
phase: Option<{op, addr, remaining, kind: Read | Write}>   // the open data phase
nor: { sr1: u8 (WIP=bit0, WEL=bit1, BP/TB/SEC/SRP bits 2..7), sr2: u8 = 0x02, sr3: u8 = 0x60 }
```

Read data is produced on demand from the open phase, one byte per FIFO pop; no buffer of
`len` bytes is ever built (a guest `LEN` of 0xFFFF_FFFF must not allocate).

### 8.3 Reads

| off | value |
|---|---|
| 0x00 / 0x04 / 0x08 / 0x0C | Last written value. |
| 0x10, 0x20, 0x30 | Stored value. |
| 0x18 | `1 \| ((read bytes available as u32) << 1)`. TX space (bit 0) is always 1. |
| 0x24 | `done as u32` (other bits 0). |
| 0x34 | `xip_ctrl & !0x400` (bit 10 self-clears immediately). |
| 0x54 | `lvl << 8`, `lvl = min(bytes left in the open data phase, 64)`; 0 when no phase is open. In a read that is RX bytes available; in a write it is TX free space (the firmware writes `min(lvl, remaining)`, so the cap is what matters). |
| 0x100 (8-bit) | next read byte, 0 when none. |
| 0x100 (32-bit) | 4 bytes, little-endian; missing bytes read as 0. 16-bit is unused; treat it as 2 bytes. |
| Others | 0, logged. |

### 8.4 Writes

| off | action |
|---|---|
| 0x00 / 0x04 / 0x08 / 0x0C | Store. |
| 0x10, 0x30 | Store. |
| 0x34 | Store; bit 10 never latches. |
| 0x24 | If `v & 1`, then `done = false`. |
| 0x20 | Store. **If `v & 2` is set, execute the descriptor (8.5).** START is level-triggered on every write with bit 1 set, not edge-triggered: after a timed-out command the firmware leaves START set, and the next command's `\|= 2` must still start it. Writing with bit 1 clear only ends the command; it must **not** clear the RX data. |
| 0x100 | If a write phase is open, push the 1 or 4 bytes (LE) into the operation (8.6); extra bytes are ignored. Otherwise ignore. |

### 8.5 Execute on START

1. Abort any open phase and set `done = false`.
2. Decode the descriptor:
   - `op = OP >> 24`;
   - `nab = CFG & 7`;
   - `addr = ADDR & 0xFF_FFFF` (when `nab == 3`);
   - `len = LEN`;
   - `write = ((OP >> 1) & 7) == 1`.
   The model can ignore the dummy count (`(CFG >> 16) & 0xFF`), the lane count
   (`(OP >> 5) & 7`) and the divider.
3. Dispatch on `op`:

| op | action |
|---|---|
| 0x9F | Read phase: `EF 40 18`, then `0x00` if `len > 3`. Done. |
| 0x05 / 0x35 / 0x15 | Read phase: `sr1` / `sr2` / `sr3`, `len` times. Done. |
| 0x06 | `sr1 \|= 0x02` (WEL). Done. |
| 0x04 | `sr1 &= !0x02`. Done. |
| 0xFF, 0x66, 0x99, 0xAB, 0xB9 and other unknown no-data ops | Done (log unknown ones). |
| 0x03, 0x0B, 0x3B, 0x6B, 0xBB, 0xEB | Read phase: `flash[(addr+i) & 0xFF_FFFF]` for `i < len`. Done. Dummy cycles have no effect on the data. |
| 0x48 | Read phase: `secreg[(addr>>12)-1][(addr+i) & 0xFF]` for `i < len` (0xFF if out of range). Done. |
| 0x20 / 0x52 / 0xD8 | If WEL is set, fill the aligned 4K/32K/64K with 0xFF. Then `sr1 &= !WEL`. Done. |
| 0xC7 / 0x60 | Same as above, applied to the whole 16 MiB. |
| 0x44 | If WEL is set, fill the security register with 0xFF. Clear WEL. Done. |
| 0x02 / 0x32 / 0x42 | If `len == 0`, done. Otherwise open a write phase `{op, addr, len}`, with `done` still false. |
| 0x01 / 0x31 / 0x11 | A write phase with `len = 1`; on the byte, write SR1/SR2/SR3 (if WEL; SR1 keeps bits 0-1 as computed). Clear WEL. Done. |

### 8.6 Program data (each byte `b` written to +0x100)

- **Page program:** `flash[(addr & !0xFF) | ((addr + k) & 0xFF)] &= b`. This is NOR AND
  semantics, and the address wraps inside the 256-byte page as on a real part.
- **Security registers:** the same, applied to `secreg`.
- When `k == len`, the operation finishes: clear WEL, close the phase, set `done = true`.
- If WEL was not set when the command started, data is accepted but discarded. This matches
  a real part, where the program is ignored.

### 8.7 Timing / WIP

The minimal model completes every operation instantly, so `sr1.WIP` reads 0 and the driver
never waits. Everything was verified with this model.

Optional realism: after erase or program, set WIP for N RDSR polls or for a cycle budget,
for example tSE = 45 ms, tBE64 = 150 ms and tPP = 0.4 ms. Each scaled value must stay well
under the 500 ms firmware timeout (TIMER0 based), or the operation is reported as -98.

DONE means "transfer finished"; it does not mean "flash finished". It is set immediately for
non-data and read commands, and after the last TX byte for writes.

### 8.8 System reset (research note B.6)

On `SYSRESETREQ`: abort the open phase (the RX data goes with it), set DONE = 0 and START
(CTRL bit 1) = 0, and clear WEL. Keep the flash bytes, the security registers and the other
status-register bits. A command cannot be in flight across a reset with the instant model.

---

## 9. Open questions

1. **Meaning of +0x20 bits 8 and 12, +0x30 (0x100E_B404) and +0x10 bit 4.** These are
   probably XIP enable, the XIP read opcode (0xEB) with its mode/dummy fields, and CPOL or
   DDR. They are not needed for the model, because XIP is modeled as a plain memory map.
2. **Whether XIP is really usable while a command runs.** The driver code runs from the
   0x100B_xxxx flash window and polls WIP during erase. On real silicon this needs either an
   XIP stall/suspend in the controller or a RAM shadow of the secure image. Is 0x1000_0000
   actually a boot-ROM-loaded SRAM copy? That would also change what "erase" means for code
   pages. The emulator is unaffected as long as erases never hit the running image; the
   writes seen so far are 0x9_0000-0xA_FFFF.
3. **Whether +0x54 has separate RX/TX fields** (for example `[7:0]` TX level) and the
   hardware FIFO depth. The firmware reads only `[15:8]`.
4. **+0x24 bits other than DONE, and the IRQ enable register.** None is used; IRQ 22 is
   never enabled.
5. **QSPI drive strength.** `psa_call` types 14 and 15 ("8/9 = Slow/Fast, 0-3 = 4/8/12/16
   mA") are handled in the secure service near 0x100d7a40 through 0x100baf1c/0x100bb0b0.
   Which block they write is not identified. It is not 0x5020_2B00, which is the LED
   pattern generator (`security.md` §6).
6. **The real flash part on hardware.** The driver does not care. If TI ships a non-Winbond
   part, only the EON branch differs: its security registers are at 0xFF_D000 +.
7. **Users of the vtable entries dev+40/44/48/52** (block-protect presets SR1 = 0x24 / 0x04,
   clear-protect, SR2 = 0x03 sets QE plus SRL). None ran during boot. If the OS sets SRL
   later, the model should honor SR write-protection only if something depends on it.
   Recommendation: accept every WRSR.
