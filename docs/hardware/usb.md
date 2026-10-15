# USB: OTG controller (0x4000_3000, IRQ 16) and Type-C controller (0x4010_C000, IRQ 28)

Both are reached only by the NS BSP (MCCI USB stack, "UHIL"), after the hand-off
(`boot-flow.md` §3.2).

## USB OTG controller at 0x4000_3000

Accesses, in order (BSP): +E0 ID and +E4 config, read at 0x0012665c; then +88, +C4 = 3,
+100 = 0x10 (polled ~2000x), +100 = 0x220, +134/138/32C/33C/104/1C0/330/120,
+1B0..+1BC = 0x1000, +1A0..+1AC, +13C, +000 = 2 (soft reset, poll until bit 1 clears).

The ID check that kills the BSP:

```
0012665c: ldr.w r1, [r0, #508]
00126664: ldr.w r2, [r1, #224]   ; +0xE0 ID
00126668: ldr.w r1, [r1, #228]   ; +0xE4 config
0012666c: bic.w r3, r2, #0xffff
00126670: cmp.w r3, #0x10000     ; ID[31:16] must be 0x0001
00126674: bne   fail
```

- **Without a USB model**, the BSP prints a `UHIL_swc: fatal error` line (error code
  80010101) and resets.
- **With ID = 0x0001_0000 and config = 0x880**, it continues. The fields used are
  ID[15:12] (version, compared with 18) and config[12:9] / config[8:5] (endpoint counts?).
- **The soft-reset bit +0x000 bit 1 must self-clear.** The loop at 0x0012746a has no
  timeout.

### Minimal model (enough to get past the BSP's fatal error)

| off | access | needed behaviour |
|---|---|---|
| +E0 | R | ID: bits 31:16 = 0x0001. Bits 15:12 = version (≤ 18 takes path A) |
| +E4 | R | config: path A needs bit 15 = 0. Fields (bits 12:9) and (bits 8:5) are stored as endpoint counts (0x880 used) |
| +000 | R/W | bit 1 = soft reset, must self-clear (poll without timeout at 0x0012746a) |
| others | storage | +100 bit 4 is polled about 2000 times after `= 0x10`, then given up (bounded) |

Implemented as `evo_hw::dev::usb::Usb`: byte/halfword/word storage, +E0/+E4 read the
constants above and ignore writes, +000 bit 1 reads 0 at once. Every reset returns to the
power-on state.

IRQ 16 (OTG) is never needed for boot. The real ID and version values are unknown. Get them
from hardware or MCCI headers.

## USB Type-C controller at 0x4010_C000 (probable), IRQ 28 `USBTC`

Identity evidence:
- An MCCI-style record (`0xDEED0202` tag, then 0x4010_C000 and 0x01F4…) at 0x0018_3600, next
  to the USB record (`… 0x40003000` with a `0xDEED0200` tag) at 0x0018_4BBC. Its descriptor
  is at 0x0018_3610.
- 0x0011_9398 / 0x0011_93a8 (S-alias listing 0x1011_9398 / 0x1011_93a8) read `+0x1C[9:8]`
  and `+0x1C[11:10]` as two 2-bit fields (CC1/CC2-like attach state).
- IRQ 28 is enabled at BSP idle (`boot-flow.md` §3.3).

Accesses seen: +84 RMW 0, +1A4 = 0x95, and byte and halfword accesses over +00..+2F
(0x00130310 / 0x0013034e); 34 accesses in the research run, before the BSP idles.

### Model (stub)

- Byte/halfword storage for every offset, reset 0.
- +0x1C bits 9:8 and 11:10 read the host-controlled CC1/CC2 state (power-on 0 =
  unattached); guest writes to those bits are dropped.
- IRQ 28 never asserted.
- System reset clears the registers but keeps the host CC state (the cable stays plugged in);
  power-on clears both. The host sets it with `Machine::set_usb_cc`.

## Open questions

1. A full register map of the Type-C controller (it needs its own research pass).
2. Whether USB-C attach (IRQ 28 and the CC fields) is part of the BSP launch gate. Test it
   only after LCDC VComp, the keypad, GPIO pins 9/11 and the ON key are refuted
   (`boot-flow.md` §5).
3. The real OTG ID/version and config values.
