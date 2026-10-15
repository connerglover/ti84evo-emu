# Keypad scanner at 0x4010_3000 (IRQ 13) and the ON key (IRQ 0)

Status: modelled (`crates/evo-hw/src/dev/keypad.rs`). The boot combos below come from two
independent static RE passes of the bundle 7.1.0.4421 BSP (image 0), reconciled; the Self-Test
and Engineering combos are confirmed in emulation (`m2_*_boot_combo` milestone tests). The key
map is checked key by key on the OS (`all_keys_type_expected_text`, "Key map").
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
   `+0x0C` bit 0 (one decode per key change), reads the halfword rows `+0x1E, +0x1C, …, +0x10`
   (rows 7..0) and turns each set bit into a code **code = (7 − row)·8 + bit + 1** (the TI-84
   Plus CE GetCSC numbering, though not every Evo key sits at its CE code: "Key map"). Up to 3
   codes are kept, ascending; no key or more than 3 keys down gives n = 0. Then
   `cb_keys(codes, n)` (0x0010_e1da).
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
| 6 | `enter` | `toggle` | `add` (+) | `sub` (−) | `mul` (×) | `div` (÷) | `clear` | |
| 5 | `chs` ((−)) | `3` | `6` | `9` | `rparen` | `tan` | `vars` | |
| 4 | `decpnt` (.) | `2` | `5` | `8` | `lparen` | `cos` | `prgm` | `stat` |
| 3 | `0` | `1` | `4` | `7` | `comma` | `sin` | `frac` (n/d) | `xton` (X,T,θ,n) |
| 2 | | `sto` | `ln` | `log` | `square` (x²) | `power` (^) | `math` | `alpha` |
| 1 | `graph` | `trace` | `zoom` | `window` | `yequ` (Y=) | `2nd` | `mode` | `del` |
| 0 | (ON, inferred) | | | | | | | |

`on` is the ON key (IRQ 0), not row 0 column 0.

This is the TI-84 Evo keypad, which is not the TI-84 Plus CE's. TOGGLE (answer ↔ fraction,
2nd MEM, alpha ") takes the CE's + position (code 0x0A) and pushes + − × ÷ one column on, so
÷ sits where the CE has ^ (0x0E); ^ (2nd ˣ√, alpha D) sits where the CE has x⁻¹ (0x2E), and
the fraction key (2nd ANGLE, alpha B) where it has APPS (0x27). The Evo has no x⁻¹ or APPS
key. Before fix wave Task 1 the table followed the CE, and `2 add 3` typed `23` on the OS.

Evidence: every key checked on the OS (below); TI's online TI-84 Evo emulator, whose keypad
gives each key's id (label, 2nd and alpha function) and BSP scan code, matching this table
for all 49 matrix keys; the USB HID table at 0x0018_421C (KP+ KP− KP* KP/ on 0x0B..0x0E,
arrows, ENTER, DEL); the digit table at 0x0017_85C2; getkey's "code == 9" ENTER test; and
the combo table below. The Self-Test Key Test grid (cells drawn by 0x0015_6244) passes with
49 keys, every key above except ON; its cell positions do not name the keys, and reading the
CE labels into them was what put + − × ÷ ^ one column off.

### Verification on the OS

The `all_keys_type_expected_text` milestone test (`crates/evo-core/tests/milestones.rs`,
needs `EVO_OS_IMAGE`, release) boots OS 7.1.0.4421 to the Calculator app and checks all 50
keys there, each by what it does:

- It reads the edit line from RAM: the calculator keeps the line before the cursor as 16-bit
  tokens in a gap buffer, located through a {start, cursor} word pair that points at it (the
  test types 9 8 last and finds the pair whose cursor is just past them). Tokens: digit d =
  0xE401 + d, letters A..Z = 0xE800.., θ 0xE81A, + − × ÷ ^ = 0xE428..0xE42C, sin( cos( tan(
  0xE43F/41/43 with their inverses one above, and so on (the test's `tok` module).
- In MathPrint, x² and ^ open templates, so the test first switches to CLASSIC (MODE, three
  UPs, RIGHT, ENTER, 2nd MODE); MathPrint's templated x² against CLASSIC's flat ² checks MODE.
- Typing keys type their token (29 sequences), 2nd gives each key's second function, and
  ALPHA gives the letters as the keypad labels them (A on MATH … Z on 2, θ, space, :, ?, ").
- DEL, LEFT, RIGHT, UP, CLEAR, ENTER (with 2nd ENTRY) and ON (to the launcher and back) are
  checked by the line they leave; MATH, VARS, STAT, PRGM (2nd DRAW), Y= (alpha F1), WINDOW
  (F2), TRACE (F4), DOWN and UP by the token a menu item types; GRAPH, ZOOM (item 4 shows
  the graph, item 6 changes WINDOW), TOGGLE (0.25 ↔ 1/4 and back) and the screen keys by
  comparing screens.

Swapping two keys in `KEY_NAMES` (e.g. `add`/`sub`, `zoom`/`window`, `graph`/`trace`) fails
the test with the keys named.

## Boot key combos

Table at **0x0018_3DF0**: 10 slots of {code0, code1, code2, len}. The matcher (0x0015_2d18)
compares the sorted held codes and stores the slot index in **0x2001_0DB8** (10 = none).

| idx | keys held | effect (cold boot unless noted) |
|---|---|---|
| 0 | `alpha` + `ln` + `enter` (alpha S) | **Self-Test menu** (ITS UID ..0004 := 1) |
| 1 | `alpha` + `power` + `enter` (alpha D) | wake path only: log event + message 0x0105 |
| 2 | `left` + `right` | hotkey message {0x0A, 1} |
| 3 | `2` + `8` | hotkey {0x0A, 3} |
| 4 | `3` + `9` | hotkey {0x0A, 4} |
| 5 | `enter` + `clear` | hotkey {0x0A, 5} |
| 6 | `enter` + `add` | hotkey {0x0A, 6} |
| 7 | `enter` + `mul` | hotkey {0x0A, 0} |
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

1. Is ON also wired into the matrix? The Key Test grid and a HID Esc entry suggest row 0,
   column 0 (code 0x39); TI's online emulator gives ON scan code 0x10 (row 6, column 7). No
   BSP code path needs either. On the OS, `r6c7` types nothing in the calculator and any
   `r0c*` shows "ERROR: UNEXPECTED". The model keeps ON on IRQ 0 only (which on the OS goes
   to the app launcher); `r0c0` and `r6c7` can be pressed separately.
2. Resolved (fix wave Task 1): the USB HID table was right that + − × ÷ are codes
   0x0B..0x0E; the Evo keypad is not the CE's ("Key map").
3. The meaning of the MODE fields other than the mode, and the real scan period.
4. Whether STATUS bit 0 needs MASK bit 0 (the model sets it regardless).
5. What combo 8 (2ND + DEL) and secure service 0x4000_0108 do.
6. The TI-84 Evo-T documents a similar engineering/self-test procedure; whether its combos
   match the Evo's (alpha E / alpha S + enter) is unconfirmed on hardware.
