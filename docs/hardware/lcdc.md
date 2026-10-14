# LCD controller (Arm PL111 CLCD, 0x4000_4000, IRQ 14) and backlight PWM (0x4010_4000)

## LCDC: Arm PL111 CLCD at 0x4000_4000, IRQ 14

The register layout is PL111. Control sits at +0x18 and IMSC at +0x1C; PL110 has those two
swapped. Every write decodes to sensible 240x320 values:

| off | PL111 name | seen value | decode |
|---|---|---|---|
| +0x00 | LCDTiming0 | 0x0A0A_0A38 | PPL = ((0x38>>2)+1)·16 = **240**; HSW/HFP/HBP = 11 each |
| +0x04 | LCDTiming1 | 0x042D_093F | LPP = 0x13F+1 = **320**; VSW = 3; VFP = 45; VBP = 4 |
| +0x08 | LCDTiming2 | 0x00EF_7801 | PCD = 1 (÷3); IVS, IHS, IPC, IOE = 1; CPL = 239 (240 clocks/line) |
| +0x0C | LCDTiming3 | 0 | line-end disabled |
| +0x10 | LCDUPBASE | 0x2004_8000 | framebuffer |
| +0x14 | LCDLPBASE | 0x2004_8000 | unused (single panel) |
| +0x18 | LCDControl | 0x312D, then `\|= 0x800` | LcdEn = 1, LcdBpp = 6 (**RGB565**), LcdTFT = 1, **BGR = 1**, LcdVComp = 3 (front porch); bit 11 = **LcdPwr**. Secure `lcd_init` reads bit 11 (0x100b3152) and runs `panel_off` (DISPOFF, SLPIN; `&= ~0x800`; 100 ms; `&= ~1`) if it is set |
| +0x1C | LCDIMSC | BSP `\|= 8` (0x0015f3ac) | VComp interrupt enable |
| +0x20 / +0x24 | LCDRIS / LCDMIS | not read | |
| +0x28 | LCDICR | BSP RMW `\|= 8` (0x0015f39c) | clear VComp. PL111 ICR is write-only; read 0 |

Accesses seen through the BSP idle loop: +0x00..+0x1C and +0x28 (25 in the research run).
The BSP polls +0x28 about 110 times during its LCD init.

Seen writers:
- secure `panel_on` 0x100b3200, which computes them from cfg 0x100B_7BC4 =
  `{0x4000_4000, 0x2006_D800, 0x2004_8000, 320, 240}`;
- BSP 0x0015f41e..0x0015f45e (identical values).

The framebuffer at 0x2004_8000 is 240x320 RGB565, portrait; the panel (MADCTL 0x28,
landscape, `spi-panel.md`) shows it rotated. The secure splash and then the BSP's TI logo are
drawn there (`boot-flow.md`).

### How the BSP uses IRQ 14

In the BSP LCD init (0x0010_9b8e..0x0010_9bd8):
1. It creates thread 0x0015_4EAD (handle stored at 0x2004_5080) and a timer (0x2004_5084).
2. It clears ICR bit 3 and sets IMSC bit 3.
3. It writes NVIC ICPR, then ISER, for IRQ 14.

Its IRQ 14 handler (0x0010_e2d0) writes NVIC ICER bit 14 and posts event 0x800 to the
thread. It does not read or clear any LCDC register, so the thread must clear ICR and
re-enable. This makes LCDC VComp the first candidate for the BSP launch gate
(`boot-flow.md` §5): the model must latch RIS.VComp once per frame, not pend IRQ 14 once.

### Minimal model

```
regs: tim[4], upbase, lpbase, control, imsc, ris : u32
read  +00..+0C tim | +10 upbase | +14 lpbase | +18 control | +1C imsc | +20 ris | +24 ris & imsc
      +28 0 | +2C/+30 upbase/lpbase (current) | +FE0.. PrimeCell IDs (optional: 0x11,0x11,0x24,0x00 / 0x0D,0xF0,0x05,0xB1) | else 0
write +00..+18 store | +1C imsc = v & 0x1E | +28 ris &= !v | +20/+24 ignore
frame event every FRAME_CYCLES while control & 1 (LcdEn): ris |= 0x8 (VComp); optionally ris |= 0x4 (LNBU) once after an UPBASE write
IRQ 14 (level) = (ris & imsc) != 0
display: when control has bits 0 and 11 set, show 240x320 RGB565 from upbase; BGR=1 swaps the R and B fields
```

FRAME_CYCLES: the pixel clock source is unknown. The geometry gives 273·372 = 101,556 pixel
clocks per frame. Use 60 Hz (2,600,000 cycles at 156 MHz) unless hardware says otherwise.
Nothing measures it.

## Backlight PWM at 0x4010_4000

| off | seen writes | meaning (inferred) |
|---|---|---|
| +0x00 | 0xA0 (secure 0x100b285a), 0x75 / table value (BSP 0x00162a84), 0x000F_FFFF (BSP 0x00109c08) | duty / compare |
| +0x04 | 0xFF (secure 0x100b285e, BSP 0x00162a94), 0x000F_FFFF | period / top |

It is write-only, with no polling and no IRQ. Its NS descriptor is at 0x0017_88E8 (`uart.md`
§1). Model: storage. Brightness = `+0 / +4`, clamped to 1. Its reset is CRU +0x20 bit 4,
released at 0x100b284c just before the first write (`security.md` §3).

## Emulator model (`evo_hw::dev::lcdc`)

- `Lcdc` implements the minimal model above with aligned word accesses only; the PrimeCell
  IDs are implemented; LNBU (RIS bit 2) is never set. RIS, MIS, +0x2C/+0x30 and the IDs
  ignore writes.
- `FRAME_CYCLES` = 2,600,000 (60 Hz). The first VComp is one frame after LcdEn goes 0 -> 1;
  each next deadline is the previous one plus one frame. Clearing LcdEn cancels the event; a
  System reset returns the LCDC and the PWM to power-on state.
- `Lcdc::frame` (behind `Machine::screen`) decodes RGB565 at UPBASE with bit 28 cleared (the
  Secure SRAM alias 0x3xxx_xxxx is the same bytes as 0x2xxx_xxxx). It returns `None` unless
  LcdEn, LcdPwr and bpp = 6 are set and the whole buffer lies in SRAM. Working orientation
  (open question 2, until Task 23 checks it against the logo): the output is LPP x PPL
  (320x240) and output pixel (x, y) = buffer line x, pixel y; BGR = 1 swaps the R and B fields.
- `Pwm` stores +0x00 and +0x04; `brightness()` = duty / period clamped to 0..=1, 0 for
  period 0. Other offsets are unhandled.

## Open questions

1. The LCDC pixel clock and so the real frame rate.
2. The orientation and colour order of the image as the panel shows it (MADCTL 0x28 and
   CONTROL bit 8 BGR); to be confirmed against the real boot logo.
3. The PWM's real period and the meaning of the 0x000F_FFFF writes.
