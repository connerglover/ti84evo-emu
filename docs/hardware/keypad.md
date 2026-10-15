# Keypad scanner at 0x4010_3000 (IRQ 13) and the ON key (IRQ 0)

Status: modelled (`crates/evo-hw/src/dev/keypad.rs`). The key map and boot combos below come
from two independent static RE passes of the bundle 7.1.0.4421 BSP (image 0), reconciled; the
Self-Test and Engineering combos are confirmed in emulation (`m2_*_boot_combo` milestone tests).
All addresses are NS.

## Identity

- NS handle 0x2001_02AC → descriptor 0x0017_8840 = {0x4010_3000}.
- The Keypad_Handler (IRQ 13, NS vector → 0x0010_e05c) reads +0x08 and 8 halfword rows at
  +0x10..+0x1E.
- The OnKey handler (IRQ 0, 0x0011_4188) calls the callback stored at 0x2001_02AC+8, i.e. the
  ON key is dispatched through the same driver object.

(An early guess called this block "a second timer, watchdog or RTC"; the RTC is at
0x4010_2000, `rtc.md`.)

## What the BSP does with it

Init, `keypad_init(cb_keys, cb_on)` at 0x0015_f1c4 (called from 0x0010_d592 with 0x0015F07D,
0x0015F121; the caller first stores 10 = "no combo" in the combo byte 0x2001_0DB8), the BSP's
first device access after the hand-off:
```
+0x04 = 0x0000_0808
+0x00 = 0x5B68_0C33
+0x08 &= ~2
+0x0C |= 2
```

Keypad IRQ 13 handler (0x0010_e05c):
1. It writes NVIC ICPR and ICER bit 13.
2. `s = +0x08; +0x08 = s` (write-back: W1C).
3. If `s & 2` (key change): clear the scan counter byte 0x2001_3298 and `+0x0C |= 1`.
4. If `s & 1` (scan done): counter++; on the **6th** scan-done it clears the counter and
   `+0x0C` bit 0 (one decode per key change), reads the halfword rows
   `+0x1E, +0x1C, …, +0x10` (rows 7..0) and turns each set bit into a code
   **code = (7 − row)·8 + bit + 1** (the TI-84 Plus CE GetCSC `sk_*` numbering). Up to 3 codes
   are kept, ascending; no key or more than 3 keys down gives n = 0. Then `cb_keys(codes, n)` (0x0010_e1da).
5. `cb_keys` (0x0015_f07c): n = 1 → getkey queue (event type 1); n = 2 or 3 → the boot-combo
   matcher 0x0015_2d18; n = 0 → release.

The ON-key IRQ 0 handler (0x0011_4188) masks and unmasks NVIC 0 and calls `cb_on`
(0x0015_f120, event type 5). It touches no device register.

## Register map

The layout of the TI-84 Plus CE keypad controller (0xF50000 on that calculator):

| off | name | model |
|---|---|---|
| +0x00 | MODE | storage (BSP: 0x5B68_0C33; mode [1:0] = 3, continuous scan) |
| +0x04 | SIZE | storage (BSP: 0x0808, 8 rows × 8 columns) |
| +0x08 | STATUS | bit 1 = key change (a press or a release), bit 0 = scan done; write 1 to clear |
| +0x0C | MASK | the same two bits; other bits read 0 |
| +0x10 + 2·r | ROW r (r = 0..7) | read-only halfword: bit c set while key (r, c) is held |

MODE, SIZE, STATUS and MASK take aligned 32-bit accesses. The rows read at any aligned width
(a word read returns rows r and r + 1). Everything else reads 0, ignores writes and is logged
as unhandled.

## Model

- **Press / release** (matrix key): set / clear the row bit, set STATUS bit 1, schedule a scan
  `SCAN_CYCLES` (156 000 cycles = 1 ms) later. Pressing a held key or releasing a free one
  changes nothing; positions outside 0..8 log a warning. A release must be a key change: the
  BSP learns that a key went up only from the n = 0 decode after it. (Until fix wave Task 1 a
  release set no bit, so the BSP never saw a key go up and the OS dropped the second of two
  taps of the same key, e.g. `7 7` typed one 7.)
- **Scan** (the scheduled event): set STATUS bit 0; while any key is held or MASK bit 0 is set,
  schedule the next scan 1 ms later; setting MASK bit 0 with no key held starts the scanner.
  The BSP decodes the rows only on the 6th scan-done after a key change and masks scan-done
  again after it, so scanning stops 1 ms after the decode of a release. The boot-combo wait
  (`get_combo`, 0x0010_d528) gives up after 50 ms.
- **IRQ 13** (level) = `STATUS & MASK != 0`.
- **ON key**: pulse IRQ 0 on press (the only edge source), nothing on release. It is not in
  the matrix (see open question 1).
- **System reset**: MODE/SIZE/STATUS/MASK return to 0 but held keys stay held (the user's
  fingers do not move). If any key is held, STATUS bit 1 is set again and a scan scheduled, so a
  combo held across the reset is a key-down as soon as the BSP sets MASK bit 1 (its
  `+0x08 &= ~2` write-back keeps bit 1). **PowerOn** releases every key.
- The scan period is a choice, not measured; anything ≲ 7 ms meets the 6-scans-in-50-ms need.

## Key map

`row` = register +0x10 + 2·row, `col` = bit, code = (7 − row)·8 + col + 1. The names are what
key scripts (`evo_core::keys`) and `evo_hw::dev::keypad::KEY_NAMES` accept (ASCII case
ignored); `r<R>c<C>` is accepted too.

| row \ col | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
|---|---|---|---|---|---|---|---|---|
| 7 | `down` | `left` | `right` | `up` | | | | |
| 6 | `enter` | `add` (+) | `sub` (−) | `mul` (×) | `div` (÷) | `power` (^) | `clear` | |
| 5 | `chs` ((−)) | `3` | `6` | `9` | `rparen` | `tan` | `vars` | |
| 4 | `decpnt` (.) | `2` | `5` | `8` | `lparen` | `cos` | `prgm` | `stat` |
| 3 | `0` | `1` | `4` | `7` | `comma` | `sin` | `apps` | `xton` (X,T,θ,n) |
| 2 | | `sto` | `ln` | `log` | `square` (x²) | `recip` (x⁻¹) | `math` | `alpha` |
| 1 | `graph` | `trace` | `zoom` | `window` | `yequ` (Y=) | `2nd` | `mode` | `del` |
| 0 | (ON, inferred) | | | | | | | |

`on` is the ON key (IRQ 0), not row 0 column 0.

Evidence: the Self-Test Key Test grid (cells drawn by 0x0015_6244 reproduce the physical
layout), the CE GetCSC numbering, the digit table at 0x0017_85C2, getkey's "code == 9" ENTER
test, the USB HID table at 0x0018_421C (arrows, ENTER, DEL) and the combo table below. The
Key Test passes with 49 keys: every key above except ON.

## Boot key combos

Table at **0x0018_3DF0**: 10 slots of {code0, code1, code2, len}. The matcher (0x0015_2d18)
compares the sorted held codes and stores the slot index in **0x2001_0DB8** (10 = none).

| idx | keys held | effect (cold boot unless noted) |
|---|---|---|
| 0 | `alpha` + `ln` + `enter` (alpha S) | **Self-Test menu** (ITS UID ..0004 := 1) |
| 1 | `alpha` + `recip` + `enter` (alpha D) | wake path only: log event + message 0x0105 |
| 2 | `left` + `right` | hotkey message {0x0A, 1} |
| 3 | `2` + `8` | hotkey {0x0A, 3} |
| 4 | `3` + `9` | hotkey {0x0A, 4} |
| 5 | `enter` + `clear` | hotkey {0x0A, 5} |
| 6 | `enter` + `sub` | hotkey {0x0A, 6} |
| 7 | `enter` + `div` | hotkey {0x0A, 0} |
| 8 | `2nd` + `del` | boot state 2 (purpose unverified; likely OS receive / maintenance) |
| 9 | `alpha` + `sin` + `enter` (alpha E) | **Engineering menu** (ITS UID ..0004 := 2) |

On hardware: with the calculator off, hold the keys, then press ON. In the emulator: hold the
keys with `Machine::set_key` (or a key script such as `+alpha +ln +enter`) before or during
the boot; the BSP's main thread calls `get_combo` at 0x0015_d482 about 92 M cycles after
power-on.

Cold-boot decision in the BSP main thread (0x0015_d438): combo 8 is checked first (unless the
reset reason is 3); then, if secure service 0x4000_0108 (0x0011_5884) returns nonzero, the
Calc OS is forced and combos are ignored; otherwise combo 0 or 9 writes ITS UID ..0004 and
starts the diagnostic app, whose menu chooser (0x0011_7be0) shows Engineering when that UID
is 2 and Self-Test otherwise. With an empty ITS store (UID ..0001, "boot to OS", missing) the
BSP also starts the diagnostic app without any combo.

## Open questions

1. Is ON also wired into the matrix at row 0, column 0 (code 0x39)? Only the Key Test grid
   and a HID Esc entry suggest it; no code path needs it. The model keeps ON on IRQ 0 only;
   `r0c0` can be pressed separately.
2. The USB HID table maps codes 0x0B..0x0E to KP+/KP−/KP*/KP/, one off from the CE layout
   above (+ is 0x0A). Grid, combos and CE numbering agree with the table above; if it is wrong,
   only `add`/`sub`/`mul`/`div`/`power` move.
3. The meaning of the MODE fields other than the mode, and the real scan period.
4. Whether STATUS bit 0 needs MASK bit 0 (the model sets it regardless).
5. What combo 8 (2ND + DEL) and secure service 0x4000_0108 do.
6. The TI-84 Evo-T documents a similar engineering/self-test procedure; whether its combos
   match the Evo's (alpha E / alpha S + enter) is unconfirmed on hardware.
