# RTC at 0x4010_2000 (IRQ 15)

Status: identity and the accesses seen; no full register map yet. The model is a first cut.

## Identity

0x4010_2000 is the **RTC**, serviced by IRQ 15. (An early guess called it "GPIO / keypad /
ON key"; the GPIO is at 0x4010_5000 (`gpio.md`) and the keypad at 0x4010_3000
(`keypad.md`).)

Evidence:

- The NS BSP's handle at 0x2001_0378 points to the descriptor at 0x0017_88EC = {0x4010_2000}.
  That descriptor sits between the backlight PWM descriptor (0x0017_88E8) and the SPI
  descriptor (0x0017_88F0).
- The IRQ 15 handler 0x116AF0 services it.
- In the secure image IRQ 15 is an "unexpected interrupt" stub (0x100B1AC8). Only the NS BSP
  uses the RTC.

## What the BSP does with it

- **IRQ 15 handler 0x116AF0.** Reads +0x34 and writes it back (W1C), clears NVIC pending and
  disables IRQ 15, then dispatches callbacks for status bits 0-5.
- **Alarm/set code at 0x163ACC..0x163BF0.** Writes +0x10/+0x14/+0x18 (alarm fields), ORs
  +0x20 with 0x80, 0x01 and 0x20 (control), and reads +0x44 (time: seconds [5:0],
  minutes [11:6], hours [16:12]).
- **Busy flags in +0x40.** Bit 3 is polled at 0x1542B0 and bit 8 at 0x1543E4, each with
  `wait_until(…, 1100 ms)` (`timer.md`). Which level means "busy" is not known.
- **Accesses seen before the BSP idles:** +0x40 read, +0x20 RMW (0x80, then 1). These happen
  after the NS hand-off (`boot-flow.md` §3.2); six accesses in the research run.

## Register map (first cut)

| off | name (inferred) | access | semantics |
|---|---|---|---|
| +0x10 / +0x14 / +0x18 | ALARM fields | W | stored |
| +0x20 | CTRL | RMW | bits 0x80, 0x01, 0x20 set by the BSP; stored |
| +0x34 | STATUS | R, W1C | bits 0-5 dispatched by the IRQ 15 handler |
| +0x40 | BUSY / SYNC | R (polled) | bit 3 and bit 8 polled; polarity open |
| +0x44 | TIME | R (and W when setting the clock) | `sec [5:0] \| min [11:6] \| hour [16:12]` |

## Model (first cut)

- +0x10/+0x14/+0x18 and +0x20: storage.
- +0x34: W1C, bits 0-5; nothing sets them yet.
- +0x40: reads a constant busy value (0 until the polarity is confirmed).
- +0x44: the time of day computed from the cycle count (`CPU_HZ`), starting at 00:00:00 at
  power-on (the machine is deterministic: no host clock). A write decodes the same fields
  and restarts the count from them.
- IRQ 15 (level) = `status & 0x3F != 0`.
- The RTC is an always-on block: it keeps its state across a System reset.

## Open questions

1. The full register map (date, alarm match, interrupt enables).
2. The polarity of the +0x40 busy bits 3 and 8.
3. Which status bit is the alarm and which the periodic tick.
