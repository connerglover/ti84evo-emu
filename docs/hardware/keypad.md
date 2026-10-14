# Keypad scanner at 0x4010_3000 (IRQ 13) and the ON key (IRQ 0)

Status: first cut. The register semantics below are a hypothesis, to be verified when the
keypad matters.

## Identity

- NS handle 0x2001_02AC → descriptor 0x0017_8840 = {0x4010_3000}.
- The Keypad_Handler (IRQ 13, NS vector → 0x0010_e05c) reads +0x08 and 8 halfword rows at
  +0x10..+0x1E.
- The OnKey handler (IRQ 0, 0x0011_4188) calls the callback stored at 0x2001_02AC+8, i.e. the
  ON key is dispatched through the same driver object.

(An early guess called this block "a second timer, watchdog or RTC"; the RTC is at
0x4010_2000, `rtc.md`.)

## What the BSP does with it

Init, `keypad_init(cb_a, cb_b)` at 0x0015_f1c4 (called from 0x0010_d592 with 0x0015F07D,
0x0015F121), the BSP's first device access after the hand-off:
```
+0x04 = 0x0000_0808
+0x00 = 0x5B68_0C33
+0x08 &= ~2
+0x0C |= 2
```

Keypad IRQ 13 handler (0x0010_e05c):
1. It writes NVIC ICPR and ICER bit 13.
2. `s = +0x08; +0x08 = s` (write-back: W1C).
3. If `s & 2`: clear byte 0x2001_3298 and `+0x0C |= 1`.
4. If `s & 1`: read the halfword rows `+0x1E, +0x1C, …, +0x10` (8 rows) and decode up to 8
   bits per row (`cmp r2,#8`).

The ON-key IRQ 0 handler (0x0011_4188) masks and unmasks NVIC 0 and calls the callback at
0x2001_02AC+8. It touches no device register, so the ON-key level must live somewhere else.

Accesses seen through the BSP idle loop: +0x00..+0x0C (6 in the research run).

## Register map (first cut)

This matches the TI-84 Plus CE keypad controller layout (at 0xF50000 on that calculator):

| off | name (inferred) | semantics |
|---|---|---|
| +0x00 | MODE | mode [1:0] = 3 (continuous scan), row wait and scan wait |
| +0x04 | SIZE | 8 rows × 8 columns (0x0808) |
| +0x08 | INT_STATUS | bit 1 = key down detected, bit 0 = scan data ready; W1C |
| +0x0C | INT_MASK | bit 1 enabled at init; bit 0 enabled by the handler after a key-down |
| +0x10..+0x1E | ROW[0..7] | halfword per row: the pressed-key bitmap of that row |

## Proposed model

- **Status write.** A write to +0x08 clears the bits written (W1C).
- **IRQ 13.** It is `(+0x08 & +0x0C) != 0` (level).
- **Rows.** A row register reads the pressed-key bitmap of that row.
- **Key press.** Set the key's row bit, then set status bit 1. If `+0x0C & 1`, set status
  bit 0 after one scan period.
- **ON key.** Raise IRQ 0 as a pulse (the only edge source; its handler reads no register).
- Keys are addressed by matrix position (row, column); the row/column → key map is not part
  of this doc.

## Open questions

1. The row/column → key map, and the ON-key level register.
2. The meaning of the +0x00 fields other than the mode, and the real scan period.
3. Whether status bit 0 needs mask bit 0, or is set regardless.
