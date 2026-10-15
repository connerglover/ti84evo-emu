# SPI master at 0x4010_7000: the LCD panel's 3-wire configuration bus

Bundle 7.1.0.4421. Static analysis of the secure image (slot 0x000B_0000, executing at
0x100B_0000+), the NS BSP in image 0 (flash 0x0010_0000+, vector table at 0x0010_0400) and the
NS OS image (image 1, 0x0038_0000).

## Summary

- **Device on the bus: the TFT panel's driver IC.** It is ST7789V-class and is driven over
  3-wire SPI with **9-bit frames**. Bit 8 is D/C (0 = command, 1 = parameter) and bits 7:0 are
  the byte. Pixel data goes over the LCDC's parallel RGB interface (0x4000_4000, `lcdc.md`).
  The SPI bus only configures the panel and reads its ID. The bus has no PMIC, gauge or touch
  device.
- **Frame format.** 9 data bits, SPI mode 3 (CPOL = 1, CPHA = 1), 1 MHz, Motorola frame
  format and master mode. No code calls `Control`, so this default is the only configuration
  ever used. (`discovery-log.md` first guessed 16-bit frames; corrected there.)
- **Who uses it:**
  - Secure: TF-M platform init → LCD init (0x100b3124) → panel power-up (0x100b3200). That
    path pulses the panel reset over GPIO, reads the panel ID (RDDID 0x04) and sends one of 11
    init tables chosen from ID byte 2.
  - NS BSP (image 0, NS part): a second copy of the same driver, plus a diagnostic "LCD Info"
    screen that reads registers C2/C5/BB. **The BSP always runs** (`boot-flow.md`), so the
    panel is initialised twice on every boot: first by the secure splash, then by the BSP.
  - NS OS (image 1): **never touches it.** No literal, no movw/movt, no driver pattern.
- **IRQs.** IRQ 19 (`SPI1`) and IRQ 20 (`SPI2`) both go to the default "unexpected interrupt"
  stubs, in the secure table and in the NS table of image 0. The driver clears every
  interrupt enable (+0x1C bits 9:0). The firmware therefore cannot tell which IRQ belongs to
  0x4010_7000. Nothing references a second SPI instance.
- **Minimum to boot.** TX-not-full always 1, busy always 0, and one RX frame per TX frame. The
  RX frame can be zero or a loopback. A zero ID selects the default init table. A faithful
  model returns the RDDID bits in the dummy-bit layout described below.

## Base / aliases / clocks

| item | value | evidence |
|---|---|---|
| Base used | **0x4010_7000** (NS alias, also from secure code, like the UART) | const config at 0x100b7c78 = `{base 0x4010_7000, 1 MHz, cpol 1, cpha 1, slave 0, b7 0, b8 0, b9 0, frame format 1, data bits 9}`. The same record is at NS 0x0017_88f0 (the BSP's SPI descriptor). |
| Secure alias | 0x5010_7000 (by the 0x1000_0000 rule). Never referenced. | search of all three images |
| Window size | at least 0x68 (highest offset read is +0x64) | |
| Peripheral clock | 39 MHz (`[0x300019c4]` = 0x025317C0; `SystemCoreClock` = 156 MHz at 0x300019c0) | decompressed secure `.data` (scatter entry 0x100d97e0 → 0x300019c0, LZ-compressed) |
| Divider | `CTRL1[15:0] = PCLK/(2·f) − 1` = 39e6/2e6 − 1 = **18** | 0x100b4bfa..0x100b4c28 |

The divider 18 (0x12) holds only with the recommended CRU reset values; with an all-zero CRU
the secure PCLK is 48 MHz and CTRL1[15:0] becomes 23 (`security.md` §3, downstream table).

## Register map

`RMW` means read-modify-write. Unless noted, every access is a 32-bit `ldr`/`str` through the
base pointer (`ip` or `r3`).

| off | name (inferred) | access | bits the firmware uses | firmware value / behavior |
|---|---|---|---|---|
| +0x00 | CTRL0 | RMW | [1:0] = CPHA \| CPOL<<1 · [3:2] = 0b11 in master mode (cleared in slave) · [6] = cfg+8 (LSB-first? default 0) · [7] = cfg+7 (default 0) · [14:12] = frame format (cfg+10: 0 TI-SSP, **1 Motorola**, 2 Microwire) · [15] = cfg+9 (default 0) · [17:16] = 0b11 in master mode (cleared in slave) | final value with the default config: **0x0003_100F** (bits 0, 1, 2, 3, 12, 16, 17) |
| +0x04 | CTRL1 | RMW | [15:0] = clock divider − 1 · [22:16] = data bits − 1 | **0x0008_0012** (9 bits, divider 19) |
| +0x08 | CTRL2 / ENABLE | RMW | [0] EN · [1] (set with EN, cleared on disable) · [3:2] set (`\|= 0xC`) in config · [6] set on every disable (FIFO clear?) · [8:7] set on enable, cleared on disable (TX/RX enable?) · [11:10] cleared in config | disable sequence: `&~1, \|0x40, &~0x180, &~2`. Enable sequence: `\|0x180, \|2, \|1`. |
| +0x0C | STATUS | R (polled) | **[1] TX FIFO not full** · **[2] BUSY** (shifting / TX not empty) · **[9:4] RX FIFO level** | see the polling loops below |
| +0x18 | DATA | W: push TX frame. R: pop RX frame. | low N bits (N = 9) | Transfer masks reads with `~(~0 << N)`. Receive stores them as a halfword when N > 8. |
| +0x1C | INT_EN / mask | RMW | [9:0] cleared (`bics #0x3ff`), then the same value is written back | interrupts are never enabled |
| +0x64 | PARAM / HW config (RO) | R | [23:16] = TX FIFO depth − 1 | Transfer prefills `min(len, field + 1)` frames before enabling |

Offsets 0x10, 0x14 and 0x20..0x60 are never touched.

### Polling patterns (exact)

TX-not-full predicate, passed to the bounded wait `0x100b6e7c(pred, regs, 100 ms)` from the
per-frame send routine 0x100b4a50:

```
100b4c30: ldr  r0, [r0, #12]      ; STATUS
100b4c32: ubfx r0, r0, #1, #1     ; bit 1 = TX not full
100b4c36: bx   lr
```

Main transfer loop at 0x100b49c8 (the unbounded one):

```
100b49ce: ldr.w r6, [ip, #12]     ; STATUS
100b49d2: lsls  r6, r6, #30       ; bit 1 -> N
100b49d4: bpl.n 0x100b49e0
100b49dc: str.w r5, [ip, #24]     ; push next TX frame
100b49e0: ldr.w r5, [ip, #12]
100b49e4: tst.w r5, #0x3f0        ; RX level [9:4]
100b49e8: beq.n 0x100b49c8
100b49ea: ldr.w r5, [ip, #24]     ; pop RX frame
```

Busy drain at the end of a transfer, and after each frame in 0x100b4a50 (both unbounded):

```
100b4a2a: ldr.w r0, [ip, #12]
100b4a2e: lsls  r0, r0, #29       ; bit 2 -> N
100b4a30: itt   pl
100b4a32: movpl r0, #0
100b4a34: ldmiapl.w sp!, {...,pc} ; return when BUSY clear
```

## Driver structure (secure image)

A CMSIS-Driver `ARM_DRIVER_SPI` sits on a small HAL. In the NS BSP the same code appears at
different addresses.

| function | secure addr | NS BSP addr | role |
|---|---|---|---|
| `ARM_DRIVER_SPI` vtable | 0x100b7b5c | 0x00178750 | GetVersion, GetCapabilities, Initialize, Uninitialize, PowerControl, **Send** = 0x100b09a8, **Receive** = 0x100b0940, **Transfer** = 0x100b0a14, GetDataCount, Control = 0x100b08d0 → 0x100b0a94, GetStatus |
| driver context | 0x30001b00 (`{dev*, count, …, cb}`) | 0x20010380 | |
| dev (`{cfg*, state*}`) | 0x30001b10 = `{0x100b7c78, 0x30002004}` | | |
| `Initialize` | 0x100b08f8 | 0x001044a0 | `spi_dev_init(dev, 39 MHz)`, then disable, then enable |
| `spi_dev_init` | 0x100b47d8 | 0x00165d3c | calls `spi_set_config`, copies cfg to state+8, state.init = 1 |
| `spi_set_config` | 0x100b4aac | 0x0016600c | writes CTRL0/CTRL1/CTRL2/+0x1C (table above). Returns 5 if the divider falls outside [2, 65536]. |
| `spi_disable` | 0x100b4768 | 0x00165ccc | CTRL2 `&~1, \|0x40, &~0x180, &~2` |
| `spi_enable` | 0x100b4790 | 0x00165cf4 | CTRL2 `\|0x180, \|2, \|1` |
| `spi_rx_frame` | 0x100b4810 | 0x00165d70 | returns 4 if RX level = 0, else pops DATA |
| `spi_tx_frame` | 0x100b4a50 | 0x00165fb0 (approx.) | bounded wait for TX not full (returns 3 on timeout), writes DATA, then spins while BUSY |
| `spi_transfer` | 0x100b4870 | 0x00165dd0 | disable, drain RX, prefill, enable, full-duplex loop, drain, wait for BUSY clear |
| `Control` | 0x100b0a94 | | **never called** (its only caller is the vtable thunk) |

`Send(buf, n)` calls `spi_tx_frame` n times. `Receive(buf, n)` calls `spi_rx_frame` n times
and returns −1 when the FIFO runs empty; its callers ignore the result. `Transfer(tx, rx, n)`
calls `spi_transfer`.

Config record layout (cfg + 4, 12 bytes): `u32 hz; u8 cpol; u8 cpha; u8 slave; u8 b7; u8 b8;
u8 b9; u8 frame_format; u8 data_bits`. The default is `{1000000, 1, 1, 0, 0, 0, 0, 1, 9}`.
`state+19` holds `data_bits` and selects byte or halfword buffers (`cmp #8`).

## Callers and protocol: the LCD panel

The call chain is platform init 0x100b2798 → (0x100b2844) `lcd_init(0x30001aa8)` at
0x100b3124:

1. It calls `SPI.Initialize(NULL)` (vtable+8).
2. It fills the 320×240×2 framebuffer from a run-length image (the boot splash).
3. If LCDC `+0x18` bit 11 is already set (panel on), `panel_off` 0x100b31d8 runs first. It
   sends `0x28` DISPOFF and `0x10` SLPIN, then clears LCDC +0x18 bits 11 and 0.
4. `panel_on` 0x100b3200 runs:
   - it programs the LCDC timing;
   - it pulses GPIO 2 (0x100b2ec0/0x100b2f8c), the panel reset: low, 10 ms, high
     (`gpio.md`);
   - it writes LCDC +0x18 = 0x312D, then sets bit 11;
   - it calls `panel_read_id` 0x100b2148, then tail-calls `panel_send_init` 0x100b2024.

The BSP repeats the same sequence with its own copy of the driver after the hand-off.

`panel_send_init` 0x100b2024 walks the NULL-terminated pointer array `*(0x300019d4)`. Each
entry is `{u8 cmd, u8 nparam, u16 delay_ms, u8 param[]}`. It builds 9-bit words (command with
bit 8 clear, each parameter `+0x100`) and calls `Send(words, n+1)`, then
`Receive(junk, n+1)` to drain the echo. After that it delays `delay_ms`.

```
100b20c2: ldrb r5, [r3, #4]       ; param byte
100b20c8: add.w r5, r5, #256      ; D/C = 1
100b20cc: strh r5, [r4, #2]
...
100b206a: mov  r0, r8             ; words
100b206c: blx  r9                 ; Send(words, nparam+1)
100b2074: blx  sl                 ; Receive(junk, nparam+1)
```

### Default init table (`0x300019e0`: used for indexes 0, 11, 13 and 14, which includes ID2 = 0 and any unknown ID2)

The table sends these ST7789V commands, in order (parameter bytes not reproduced):

SWRESET (0x01, then 120 ms), SLPOUT (0x11, then 120 ms), MADCTL (0x36; landscape), COLMOD
(0x3A; 18-bit colour), PORCTRL (0xB2), LCMCTRL (0xC0), VDVVRHEN (0xC2), VDVSET (0xC4),
FRCTRL2 (0xC6), PWCTRL1 (0xD0), RAMCTRL (0xB0; RGB interface selected), LCMCTRL (0xC0),
0xE9, GCTRL (0xB7), VCOMS (0xBB), VRHSET (0xC3), 0xD2, PVGAMCTRL (0xE0), NVGAMCTRL (0xE1),
RGBCTRL (0xB1), GAMSET (0x26), DISPON (0x29, then 150 ms).

The 10 alternative tables are at 0x30001b50, …bb0, …c00, …c50, …ca4, …cf0, …d44, …d98, …dec
and …e50. They differ only in tuning values.

### Panel ID read: `panel_read_id` 0x100b2148

The driver context at 0x300019d4 is `{table*, u8 cmd = 0x04, u8 n = 3, …, u8 id[3] @ +8,
table @ +12}`. The routine builds `tx = [0x004, 0x000, 0x000, 0x000]`, calls
`Transfer(tx, rx, 4)` and decodes as follows. ST7789 3-wire reads insert one dummy clock
before the 24 ID bits.

```
id1 = rx[1] & 0xFF                       ; 100b21b4 strb
id2 = rx[2] >> 1                         ; 100b21b8
id3 = ((rx[2] << 7) | (rx[3] >> 2)) & 0xFF
```

`id2` picks the panel module through a `tbb` switch at 0x100b2280. It stores the index at
`lcd_state+8` and the init table at `[0x300019d4]`. The names come from the BSP table at
0x001842c8:

| id2 | index | module | table |
|---|---|---|---|
| 0 | 0 | Giant Plus | default 0x300019e0 |
| 1 | 1 | SGD TN | 0x30001e50 |
| 2 | 2 | Jenson TN | 0x30001ca4 |
| 3 | 3 | GP TN | 0x30001bb0 |
| 4 | 4 | LXD TN | 0x30001d44 |
| 5 | 5 | LXD IPS | 0x30001cf0 |
| 6 | 6 | Holitech TN | 0x30001c50 |
| 0x11 | 7 | SGD IPS | 0x30001dec |
| 0x16 | 8 | Holitech IPS | 0x30001c00 |
| 0x21 | 9 | SGD CTC | 0x30001d98 |
| 0x12 | 10 | Jenson IPS | 0x30001cf0 |
| 0x22 | 11 | Jenson CTC | default |
| 0x13 | 12 | GP IPS | 0x30001b50 |
| 0x35 | 13 | Three Five TN | default |
| any other | 14 | "Unknown Display" | default |

The BSP (0x00117c80, 0x00117eb0, menu at 0x0010e5b0) holds a duplicate ID reader and a
diagnostic "LCD Info" screen. That screen prints "LCM ID" and "Name:", then reads **C2
"VDV/VRH", C5 "VCOMS Offset" and BB "VCOMS Setting", 5 bytes each** (table 0x00183e18). It
decodes them with the same dummy-bit unpacking, as five hex bytes.

## Reset values the firmware expects

- STATUS after reset: TX not full = 1, BUSY = 0, RX level = 0. Any value works for init,
  because Transfer drains RX first and the first wait is for TX not full.
- PARAM +0x64: any value. Zero gives a 1-frame prefill and still works. A real depth is
  plausible, for example 8 (field = 7).
- CTRL0/1/2 and +0x1C: the firmware read-modify-writes them but never depends on read-back.
  Plain storage, reset 0, is enough.

## Timing / interrupt behavior

- No IRQ use. IRQ 19 = `SPI1_Handler` (secure 0x100b1ec0, BSP 0x001175f4) and IRQ 20 =
  `SPI2_Handler` (0x100b1ed8 / 0x0011760c) are both default stubs. Modeling an IRQ line is
  optional.
- Real bus timing: 9 bits at 1 MHz = 9 µs per frame, about 1,400 cycles at 156 MHz. Nothing
  measures it. All per-frame waits are on STATUS bits, and inter-command delays use the timer
  (0x100b6ddc, `delay_ms`). Instant completion is safe.
- The only bounded wait is `spi_tx_frame`'s TX-not-full wait (100 ms, TIMER0). It needs
  TIMER0 ticking only if TX-not-full is ever 0.

## Minimal model specification

Map 0x1000 bytes at 0x4010_7000. Optionally alias 0x5010_7000.

State: `ctrl0, ctrl1, ctrl2, inten: u32`, `tx: VecDeque<u16>` (capacity `DEPTH` = 8),
`rx: VecDeque<u16>` (capacity 16), and `panel: Panel`.

- `read(+0x00/+0x04/+0x08/+0x1C)` returns the stored value.
- `write(+0x00/+0x04/+0x1C)` stores the value.
- `write(+0x08, v)`: store it, except bit 6, which is self-clearing (reads 0).
  - If v bit 6 is set and bit 0 is clear, clear `tx` and `rx`. The firmware drains RX
    itself, so clearing only `tx` is also acceptable.
  - Bit 6 must not be stored: the transfer at 0x100b4928 disables (`|0x40`), queues its TX
    frames, then enables with read-modify-writes (`|0x180`, `|2`, `|1`). A stored bit 6
    would make the first two of those writes flush the queued frames, and the RX wait at
    0x100b49c8 would hang the boot (found in Plan 2, Task 10).
  - On a 0→1 edge of bit 0, shift out every queued `tx` frame (see the shift rule below).
- `read(+0x0C)` returns `(1<<1 if tx.len() < DEPTH) | (0<<2 BUSY) | ((min(rx.len(), 63)) << 4)`.
  BUSY is 0 because transfers complete instantly.
- `read(+0x64)` returns `(DEPTH-1) << 16`.
- `write(+0x18, v)`: `f = v & mask(nbits)`, with `nbits = ((ctrl1 >> 16) & 0x7F) + 1`.
  - If `ctrl2 & 1` is set, shift the frame now.
  - Otherwise push it to `tx`; when `tx` is full, drop it.
- `read(+0x18)` pops `rx`. On empty it returns 0 (also while the core is disabled).
- **Shift rule:** for each frame, `miso = panel.clock(f, nbits)`, then push `miso` to `rx`.
  When `rx` is full, drop the oldest frame (overflow). The Send+Receive path pushes up to 15
  frames without reading; Receive tolerates loss and Transfer drains first.
- **Invariant:** every MOSI frame produces exactly one MISO frame. The transfer loop
  0x100b49c8..0x100b4a18 waits for `len` RX frames with no timeout, so a missing frame hangs
  boot.

### Minimal model: the panel responder

`Panel` is a minimal ST7789 responder that clocks MSB-first, 9-bit 3-wire.

- On a frame with bit 8 = 0 (command `c`), set `cmd = c`, clear the response bit queue, and
  return 0 for this frame.
  - For read commands, load the bit queue as `[dummy 0] ++ bytes` (MSB first):
    - `0x04` RDDID → `ID1, ID2, ID3` (3 bytes, with the dummy bit);
    - `0x09` RDDST → 4 bytes, with the dummy bit;
    - `0x0A..0x0F` → 1 byte, no dummy;
    - `0xDA/0xDB/0xDC` → ID1/ID2/ID3, 1 byte each, no dummy.
  - For the C2/C5/BB diagnostic reads, use the dummy bit plus the last written parameters,
    padded to 5 bytes.
- On a frame with bit 8 = 1 (parameter), record it as `params[cmd]` and return 0.
- Any other frame while a read is pending is a dummy clock frame (bit 8 = 0, value 0x00 =
  NOP). This conflicts with the command rule: treat command 0x00 as "keep clocking out the
  pending read" instead of starting a new command. Return the next 9 bits from the queue,
  MSB first, padded with 0.
- Consistency check with the decoder above: for ID `(a, b, c)` the three frames are
  `a`, `(b << 1) | (c >> 7)` and `(c << 2) & 0x1FF`.
- Default ID: `ID1 = 0x85, ID2 = 0x85, ID3 = 0x52` (generic ST7789V). That gives index 14
  ("Unknown Display") and the default table. `ID2 = 0x00` gives "Giant Plus" with the same
  table. A plain loopback, or an all-zero MISO, behaves like `ID2 = 0`.
- Optional tracking for a later display model: sleep (0x10/0x11), display on/off
  (0x28/0x29), MADCTL (0x36) and COLMOD (0x3A). The image itself comes from the LCDC
  framebuffer at 0x2004_8000.

## Open questions

1. **SPI1 or SPI2?** The firmware does not distinguish them, since it uses no IRQ. A guess by
   address order (UART 0x4010_6000 = IRQ 17, I2C = 18) gives 0x4010_7000 = SPI1 (IRQ 19),
   with SPI2 (IRQ 20) at an unreferenced address, perhaps 0x4010_8000.
2. **Real panel ID.** It is unknown; the vendor programs ID2 in OTP. A hardware dump of `LCD
   Info`, from the BSP diagnostic menu, would settle it. The emulator can use any ID in the
   table.
   **Emulator decision (Plan 2, Task 9):** `evo_hw::dev::spi::DEFAULT_PANEL_ID` =
   `ID1 0x85, ID2 0x85, ID3 0x52` (generic ST7789V), which selects index 14 ("Unknown
   Display") and the default init table. `St7789::new` takes any ID, so a confirmed ID can
   replace it later.
3. **Exact meaning of the CTRL0 bits** [3:2], [7], [17:16] and the CTRL2 bits [1], [3:2],
   [6], [8:7], [11:10]. The model only needs CTRL2[0] (enable) and CTRL1[22:16] (frame
   size). Bit 6 is presumably a FIFO flush.
   **Emulator decision (Plan 2, Task 10):** bit 6 is a self-clearing flush (write 1 with
   EN clear empties both FIFOs; it reads 0). A stored bit 6 hangs the boot (see the model
   above). The other bits are stored and have no effect.
4. **FIFO depth** (+0x64[23:16]) is unknown. 8 is a guess; the behavior does not depend on
   it. **Emulator decision (Task 9):** TX depth `SPI_FIFO_DEPTH` = 8 (+0x64 reads
   0x0007_0000), RX capacity `SPI_RX_CAP` = 16; neither line IRQ 19 nor IRQ 20 is driven.
