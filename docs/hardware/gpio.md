# GPIO controller at 0x4010_5000 (13 pins, IRQ 24)

The NS alias 0x4010_5000 is used by secure and NS code alike. (`discovery-log.md` first
guessed a display DMA helper; corrected there.)

BSP addresses are NS (0x001x_xxxx); the same bytes sit at the S alias 0x101x_xxxx
(`boot-flow.md`, address convention).

## Identity

- Secure handle 0x3000_1AA4 → cfg 0x100B_7BC0 = {0x4010_5000}.
- The IRQ 24 handlers (S 0x100b0f38, NS 0x0010_c094) loop `i < 13` over the status word at
  +0x28.
- Secure `gpio_*` functions at 0x100b2ec0..0x100b2fb2; BSP functions at
  0x0015_ca34..0x0015_cc74.

## Drivers

- **Secure**, through handle 0x3000_1AA4:
  - `gpio_config(pin, dir, pull)` at 0x100b2ec0;
  - `gpio_set` at 0x100b2f74;
  - `gpio_write(pin, v)` at 0x100b2f8c.
- **BSP**, through handle 0x2001_02A8, at NS 0x0015_ca34..0x0015_cc74:

  | address | function |
  |---|---|
  | 0x15ca34 | clear (+0x14) |
  | 0x15ca4c | config |
  | 0x15cafc | irq_mode |
  | 0x15cbb4 | irq_disable |
  | 0x15cbf0 | irq_enable |
  | 0x15cc20 | read |
  | 0x15cc38 | set (+0x10) |
  | 0x15cc50 | write |

## Register map

| off | name (inferred) | access | semantics from the code |
|---|---|---|---|
| +0x00 | DATA_OUT | RMW | `gpio_write`: `v = (v & ~(1<<p)) \| (!!val << p)` (0x100b2f9c..0x100b2fb0) |
| +0x04 | DATA_IN | R | `(v >> p) & 1` (0x0015cc2c) |
| +0x08 | DIR | RMW | `config(dir=1)` → `\|= 1<<p` (**1 = output**); `dir=0` → `&= ~(1<<p)`; any other dir leaves it unchanged |
| +0x10 | SET (W) | W `1<<p` | Secure platform init: `config(1, out, nopull)`, then `+0x10 = 2` (0x100b27aa) |
| +0x14 | CLR (W) | W `1<<p` | BSP "Shipmode" (0x001178c8, S-alias listing 0x101178c8): `config(1, out)`, `+0x14 = 2`, `delay(5000 ms)`, then `+0x10 = 2`. That is, it drops the **power-hold** pin and restores it if still alive |
| +0x18 | PULL_EN | RMW | `pull=0` → `&= ~b`; `pull=1/2` → `\|= b` |
| +0x1C | PULL_SEL | RMW | `pull=1` → `\|= b` (pull-up?); `pull=2` → `&= ~b` (pull-down?) |
| +0x20 | INT_EN | RMW | enable: `+0x30 = b; +0x20 \|= b`, then NVIC ICPR/ISER bit 24. Disable: `+0x20 &= ~b; +0x30 = b`; if `+0x20 == 0`, NVIC ICER bit 24 |
| +0x28 | INT_STATUS | R | the IRQ handler reads it once and services each set bit 0..12 |
| +0x30 | INT_CLR | W `1<<p` | write-1-to-clear of the +0x28 latch |
| +0x34 | INT_TYPE | RMW | irq_mode 0/1 → `\|= b`; modes 2/3/4 → `&= ~b` |
| +0x38 | INT_BOTH | RMW | mode 4 → `\|= b`; modes 2/3 → `&= ~b` |
| +0x3C | INT_POL | RMW | modes 1/3 → `\|= b`; modes 0/2 → `&= ~b` |

Not touched: +0x0C, +0x24 and +0x2C. Reset value 0 is expected everywhere. Secure code
read-modify-writes from 0, and the first DIR write happens before any output.

Accesses seen through the BSP idle loop: +00/+04/+08/+10/+18/+20/+30/+34/+38/+3C (29 in
the research run). The secure platform init sets pin 1 before the clock gates and the timer
(`security.md` §3).

Interrupt-mode decode (`irq_mode` 0x0015cafc `tbb`). This is inferred, but self-consistent:
- 0 = low level, 1 = high level (TYPE = 1 → level);
- 2 = falling edge, 3 = rising edge, 4 = both edges.

Pin 11 is armed alternately with modes 2 and 3 (0x00152b64, 0x00163cf8), which is the
classic way to track a level with edge interrupts. Pin 9 uses mode 4 (0x00152ec8) and is
then enabled.

## Pin roles known so far

| pin | dir | user | role |
|---|---|---|---|
| 1 | out | S 0x100b27a4 (set at platform init), BSP Shipmode | **power hold**: driving it low (CLR) powers the unit off |
| 2 | out | S `panel_on` 0x100b3244..0x100b3260; BSP 0x0015f444 | **LCD panel reset** (low, 10 ms, high; `spi-panel.md`) |
| 5, 7, 12 | out | BSP 0x001051a0.. | driven in patterns while pin 11 is sampled after `delay_ms(10)`: a strap/ID probe or a bit-banged link |
| 8 | in/out | BSP 0x001051d2 | read |
| 9 | in | BSP 0x001051da, IRQ mode 4 | cable/accessory detect (guess) |
| 11 | in | BSP 0x00104c94 and the probe loop; IRQ modes 2/3 | an input level tracked by IRQ |

Excerpt (secure handler, 0x100b0f80..):
```
ldr  r1, [r1, #0]      ; base
ldr  r2, [r1, #32]     ; INT_EN
bics r2, r0            ; one-shot: disable this pin
str  r2, [r1, #32]
str  r0, [r1, #48]     ; INT_CLR = 1<<pin
ldr  r0, [r1, #32]
cmp  r0, #0            ; any still enabled? (else NVIC ICER bit 24 again)
```

## Minimal model

```
state: out, dir, pull_en, pull_sel, ien, ists, itype, iboth, ipol : u32 (13 bits used); ext_in: u32 (host-driven)
pin_level(p) = if dir>>p&1 { out>>p&1 } else if host set ext { ext_in>>p&1 } else if pull_en>>p&1 { pull_sel>>p&1 } else { 0 }
read  +00 out | +04 Σ pin_level(p)<<p | +08 dir | +18 pull_en | +1C pull_sel | +20 ien
      +28 ists & ien | +34 itype | +38 iboth | +3C ipol | else 0
write +00 out=v | +08 dir=v | +10 out|=v | +14 out&=!v | +18/+1C/+20/+34/+38/+3C store
      +30 ists &= !v
after every write and every host input change: for each pin, compare the old and new level:
  level mode (itype=1): ists |= b while level == ipol
  edge mode: rising && (iboth || ipol) || falling && (iboth || !ipol) -> ists |= b
IRQ 24 (level) = (ists & ien) != 0
```

Emulator hooks:
- If pin 1 is an output and goes low, treat it as power-off: stop, or show "off".
- Pin 2 low→high can reset the panel model.

## Emulator model (`evo_hw::dev::gpio::Gpio`)

The model above, with these decisions:

- Only aligned 32-bit accesses reach registers. Other widths, unaligned offsets, the
  untouched offsets (+0x0C, +0x24, +0x2C, +0x40 and up) and writes to the read-only
  +0x04 and +0x28 read 0, are ignored and are logged as unmapped.
- Registers keep 13 bits (pins 0..12). Higher bits read 0. Host inputs for pins 13 and up
  are ignored.
- The interrupt latch is set for a pin whether or not INT_EN is set; +0x28 reads
  `latch & INT_EN`. Setting INT_TYPE, INT_POL or DIR also counts as a change, so a level
  that already matches latches at once. The driver writes INT_CLR before INT_EN.
- Host inputs: `Machine::set_gpio_input(pin, Some(level) | None)`. `None` means undriven,
  so the pin reads its pull, else 0. An output pin reads its output bit, and the host level
  applies again once the pin is an input.
- **Power hold.** A write that takes pin 1 from output-high to output-low logs
  `power-hold released` (info level) and arms a one-shot report.
  `Machine::take_power_off()` returns it. After a run, `evo-cli run` prints
  `power: off (GPIO pin 1 released)` on stderr. The machine keeps running, because Shipmode
  restores the pin after 5 s if the unit is still alive. Changing pin 1 from an output to an
  input is not a release.
- **Pin 2 (panel reset) is not hooked, and this is deferred to Plan 3.** The panel model
  tracks only sleep, display on/off and MADCTL, and the image comes from the LCDC, so a
  panel reset has nothing to reset yet.
- Reset: a System reset sets every register and the latch to 0 and drops IRQ 24. It keeps
  the host input levels (what is wired to the pins does not change) and a pending power-off
  report. PowerOn also clears both.

Unconnected inputs read 0. The probe at 0x001051d0 branches on pin 11, and both branches stay
bounded.

## Open questions

1. Real input values for pins 8, 9 and 11.
2. The polarity of PULL_SEL (+0x1C): is 1 a pull-up?
3. Whether the interrupt-mode decode above matches the hardware.
