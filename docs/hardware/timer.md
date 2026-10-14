# Timer block at 0x4010_1000: register map and model

Sources: the secure image (package bytes 0x0..0xDB2B8 at 0x100B_0000), the Non-secure part
of image 0 (the same bytes from file offset 0x50000, executing at NS 0x0010_0000+; this is
the NS RTOS/BSP, called "NS-part" below), and the TI-OS image 1 (slot 0x0038_0000). Static
analysis was confirmed by running the real bundle in a throwaway emulator build with register
fakes, using the model below plus dumb MPC/clock/SPCTRL/UART/SPI fakes.

## Summary

| item | value |
|---|---|
| Base used by firmware | **0x4010_1000 (NS alias) only**, by secure and NS code alike. 0x5010_1000 is never referenced (no literal, no movw/movt, no access in the boot run). The block is assigned Non-secure. |
| Window | 0x40 bytes used (+0x00..+0x3B). Map 0x1000. |
| Channels | 3 × 32-bit **down-counters**, stride 0x10: VALUE at +0x00/+0x10/+0x20, LOAD at +0x04/+0x14/+0x24. |
| Common registers | +0x30 CTRL (3 bits per channel + 1 extra bit per channel), +0x34 INT status (W1C), +0x38 INT enable. |
| IRQs | ch0 = IRQ 1, ch1 = IRQ 2, ch2 = IRQ 3 (byte 13 of the channel descriptor, and vector table entries 17-19). |
| Clock | CTRL bit (3n+1) set: **32,768 Hz**. Clear: the firmware converts ticks with **39,000 Hz** (39 ticks per ms). |
| Who uses it | Secure: ch0 = time base for every `delay_ms` and bounded wait; ch1 is started in platform init but never read by secure code. NS-part: same ch0 delay/wait API, plus ch1/ch2 used as ms stopwatches. TI-OS image 1: no direct access. |
| Interrupts | Never enabled by any code path found. IRQ 1-3 handlers exist (overflow counters) but init clears INT enable and NVIC-disables the line. |
| Other timers on the boot path | None. SecureWatchdog (IRQ 12) and RTC (IRQ 15) are "unexpected interrupt" stubs in the secure image; nothing in the secure image programs a watchdog or RTC. NS RTOS tick = **SysTick** (not this block). The RTC is at 0x4010_2000 (`rtc.md`). |

## Channel descriptor (const, in flash)

The driver is table-driven. Each channel has a const descriptor; RAM handle objects point to
it (handles found by decompressing the ARM scatter-load RW image, entry {0x100D97E0 →
0x3000_19C0, 0x520 bytes, decompressor 0x100B93FC}).

| | secure desc | NS-part desc | secure handle | NS-part handle |
|---|---|---|---|---|
| ch0 | 0x100B_79A0 | 0x0017_8720 | 0x3000_1A80 → {0x100B79A0, state 0x30001F94} | 0x2001_0210 → {0x00178720, 0x20013268} |
| ch1 | 0x100B_79B0 | 0x0017_8730 | 0x3000_1A88 | 0x2001_0218 |
| ch2 | 0x100B_79C0 | 0x0017_8740 | 0x3000_1A90 | 0x2001_0220 |

Descriptor layout `{u32 chan_base; u32 common_base; u8 b8, b9, b10, b11, b12; i8 irq}`:

| | chan_base | common_base | b8 EN | b9 | b10 | b11 CLK32K | b12 INT | irq |
|---|---|---|---|---|---|---|---|---|
| ch0 | 0x40101000 | 0x40101030 | 0 | 2 | 9 | 1 | 2 | 1 |
| ch1 | 0x40101010 | 0x40101030 | 3 | 5 | 10 | 4 | 5 | 2 |
| ch2 | 0x40101020 | 0x40101030 | 6 | 8 | 11 | 7 | 8 | 3 |

So per channel n: CTRL bit 3n = enable, 3n+1 = 32 kHz clock select, 3n+2 = unknown
(cleared at init), 9+n = unknown (cleared at init); INT status/enable bit = 3n+2.
State object: `{u8 initialized; u8 ?; u16 pad; u32 overflow_count}`.

## Register map

Offsets from 0x4010_1000. "n" = channel 0..2.

| offset | name | access | firmware use | reset value expected |
|---|---|---|---|---|
| +0x00 +0x10n | VALUE_n | R/W | Read: current count (`ldr [base]`, 0x100B6D00 / NS 0x168BC0). Write: load the counter (`value = LOAD`, 0x100B6DB4 / NS 0x168C80); always done while the channel is disabled. | don't care (written before enable) |
| +0x04 +0x10n | LOAD_n | R/W | Written 0xFFFF_FFFF at init (0x100B6D8C) and at start (0x100B6DD4 / NS 0x168CB8). Read back to load VALUE and, in NS-part, as the reference for elapsed = LOAD − VALUE (0x168BC8). Must read back what was written. | don't care |
| +0x30 | CTRL | R/W, RMW only | Every access is read-modify-write of single bits: enable (3n), clk32k (3n+1), bit 3n+2, bit 9+n. | 0 is fine (boot run starts at 0) |
| +0x34 | INT_STAT | W1C (write-only in practice) | Init writes `1 << (3n+2)` (0x100B6D72). IRQ handlers write the same (0x100B252C). Never read in any code found. | 0 |
| +0x38 | INT_EN | R/W, RMW | Init clears bit 3n+2 (0x100B6D90..94). Never set. | 0 |
| +0x08, +0x0C, +0x18, +0x1C, +0x28, +0x2C, +0x3C.. | — | — | never accessed | — |

Access counts in a 400M-cycle research run (4 boot passes): R +0x00 thousands, R/W +0x30
70/70, W +0x34 10, R/W +0x38 10/10, W +0x04 10, W +0x14 10, R +0x04 5, R +0x14 5, W +0x00 5,
W +0x10 5. No other offset, and nothing at 0x5010_1000.

## Firmware functions

### Secure image

| addr | inferred name | does |
|---|---|---|
| 0x100B6D08 | `timer_init(h)` | If `!state->initialized`: CTRL &= ~EN; NVIC_ICER[irq] (0xE000E180); CTRL &= ~bit(3n+2); INT_STAT = bit(3n+2); CTRL &= ~CLK32K; LOAD = 0xFFFFFFFF; INT_EN &= ~bit(3n+2); CTRL &= ~bit(9+n); overflow = 0; initialized = 1. |
| 0x100B6CD4 / 0x100B6CEC | `timer_disable` / `timer_enable` | CTRL &= ~/\|= bit(3n). |
| 0x100B6DC0 | `timer_set_clk32k` | CTRL \|= bit(3n+1). |
| 0x100B6DD4 | `timer_set_load(h, v)` | LOAD = v. |
| 0x100B6DB4 | `timer_reload(h)` | VALUE = LOAD. |
| 0x100B6D00 | `timer_read(h)` | return VALUE. |
| 0x100B6CCC | `timer_clear_overflow` | state->overflow = 0 (RAM only). |
| 0x100B6DAC | `timer_is_init` | return state->initialized. |
| 0x100B2FCC | `tick_init()` (ch0, lazy, flag 0x3000_5200) | init → disable → clear ovf → clk32k → LOAD=0xFFFFFFFF → reload → enable. |
| 0x100B6E2C | `ch1_start()` | same sequence for ch1. Called once from platform init 0x100B2798 (at 0x100B2816), right after clock-gate enables and before `cpsie i`. |
| 0x100B6DDC | `delay_ms(ms)` | ms clamped to 0x1FFFF; ticks = (ms << 15) / 1000; spins until `start − VALUE ≥ ticks`. |
| 0x100B6E7C | `wait_until(fn, arg, timeout_ms)` | Same tick math; loops `fn(arg)` until true or timeout. Returns `fn \| (elapsed < ticks)`. |
| 0x100B2510 / 2530 / 2550 | IRQ1/2/3 handlers | overflow++; INT_STAT = bit(3n+2). Dead code (never enabled). |

`delay_ms` excerpt (0x100B6E02..):

```
movw r1,#0x4dd3 ; movt r1,#0x1062   ; 1/1000 magic
lsls r0,r5,#15                      ; ms * 32768
umull r0,r1,r0,r1 ; lsrs r6,r1,#6   ; ticks = ms*32768/1000
bl   0x100b6d00                     ; now = VALUE
subs r0,r4,r0                       ; elapsed = start - now (down-counter, mod 2^32)
cmp  r0,r6 ; bcc 0x100b6e1c
```

Callers of `delay_ms` on the boot path: 0x100B325C (10 ms), 0x100B3270 (1 ms), 0x100B327E (1 ms),
0x100B2080 (120, 120, 150 ms; SPI-device power sequence), 0x100B31E4 (15 ms), 0x100B31F2 (100 ms),
0x100B4720/4740 (5 ms; LED pattern generator, `security.md` §6), 0x100D86EC (1900 ms),
0x100D8988 (100 ms). `wait_until` callers: QSPI waits with 5 or 100 ms timeouts (0x100B1B2A,
1C6C, 1D2C, 1E24, 3696), SPI 100 ms (0x100B4A6A), flash 500 ms (0x100B71B8, 7346, 741C, 74F2,
771C, 7910).

### NS-part of image 0 (NS RTOS/BSP, 0x0010_0000+)

Byte-identical driver: init 0x168BD4, disable/enable 0x168B94/0x168BAC, clk32k set/clear
0x168C8C/0x168CA0, set_load 0x168CB8, reload 0x168C80, read 0x168BC0, elapsed = LOAD − VALUE
0x168BC8, `tick_init` (ch0, flag 0x2004_5088) 0x15D9C4, `delay_ms` 0x168D2C (37 callers),
`wait_until` 0x168E98 (6 callers, incl. RTC sync waits), IRQ1-3 handlers 0x11872C/4C/6C (NS
vector table at 0x0010_0400). Extra:

| addr | name | does |
|---|---|---|
| 0x168D7C | `stopwatch1_start` | ch1: init, disable, CLK32K **set**, LOAD = ~0, reload, enable |
| 0x168DCC | `stopwatch2_start` | ch2: same but CLK32K **cleared** (0x168DFE `bl 0x168ca0`) |
| 0x168CC0 / 0x168E1C | `stopwatch1_ms` / `stopwatch1_stop_ms` | e = LOAD − VALUE; ms = (e >> 15) * 1000 + ((e >> 5) & 0x3FF) |
| 0x168CE0 / 0x168E44 | `stopwatch2_ms` / `stopwatch2_stop_ms` | e = LOAD − VALUE; ms = (e / 39000) * 1000 + (e % 39000) / 39 |

A function-pointer table at 0x0018_2174 holds thunks to the start/stop functions (0x114C74,
0x114C78, 0x114CA4, 0x114CA8).

ch2 excerpt (0x168CEE..), the only evidence for the non-32 kHz rate:

```
movw r1,#0xebbb ; movt r1,#0x35c5   ; 2^45/39000
umull r1,r2,r0,r1 ; lsrs r1,r2,#13  ; q = e / 39000
movw r2,#39000  ; mls r0,r1,r2,r0   ; r = e % 39000
...                                 ; r / 39  (0xa41b reciprocal)
```

### TI-OS image 1

No literal, movw/movt pair or other reference to 0x4010_1xxx (or to any peripheral). It uses
the NS-part services.

## Timing

- CPU 156 MHz (SystemCoreClock). 32,768 Hz → 4,760.74 CPU cycles per tick. 39,000 Hz → 4,000
  cycles per tick (156 MHz / 4000; 39 MHz APB / 1000 also gives it).
- In the research run, `wait_until(…, 100 ms)` timed out after 15,596,410 cycles (100 ms ✓),
  and `delay_ms(10)` took 1.55 M cycles. One full secure boot pass (to the SPM panic that a
  missing SPIF causes) costs about **97.6 M cycles**, nearly all in timer delays/timeouts
  (100 ms QSPI Read-ID timeout, 120+120+150 ms SPI-device delays, …). The model needs no
  host-time link; cycle-derived ticks are correct.
- The emulator's time base: one `Scheduler::now` unit is one CPU cycle, and
  `evo_hw::CPU_HZ = 156_000_000`. A device sees the cycle count as `DevCtx::now`
  (`memory-map.md`, "Device runtime decisions"). The measurement above confirms that this
  matches the firmware's arithmetic.
- Wrap: with LOAD = 0xFFFF_FFFF a 32 kHz channel wraps after 36.4 h. Firmware arithmetic is
  modulo 2^32 and clamps delays to 0x1FFFF ms so a single wait never exceeds one wrap.

## Model

Per channel n store `value[n]: u32`, `load[n]: u32` and `since[n]: u64` (a cycle count);
common `ctrl`, `ists`, `ien: u32`. VALUE is computed from the cycle count; reads do not
change state:

```
hz(n)   = if ctrl >> (3n+1) & 1 { 32_768 } else { 39_000 }
ticks   = (now - since) as u128 * hz / CPU_HZ                     // u128
cur(n)  = if disabled { value } else if ticks <= value { value - ticks }
          else { load - ((ticks - value - 1) % (load as u64 + 1)) }   // u64: LOAD+1 overflows u32
```

The arithmetic is u64/u128 because LOAD = 0xFFFF_FFFF, the only value the firmware writes,
makes `load + 1` overflow `u32`.

`latch(n)`: if channel n is disabled, `since = now`; else `value = cur(n)` and
`since += ceil(ticks * CPU_HZ / hz)` (u128; never past `now`). `since` advances by whole
ticks only, so the part-tick remainder is carried to the next latch, not dropped.

| offset | read | write |
|---|---|---|
| 0x00/0x10/0x20 | `cur(n)` | `latch(n)`, then `value[n] = v; since[n] = now` |
| 0x04/0x14/0x24 | `load[n]` | `load[n] = v` (does not touch the running count) |
| 0x30 | `ctrl` | `latch` every channel first, then `ctrl = v` |
| 0x34 | `ists` | `ists &= !v` (W1C) |
| 0x38 | `ien` | `ien = v` |
| other | 0 | ignore (log) |

Required for boot: VALUE must decrease at the CTRL-selected rate while enabled, LOAD and CTRL
must read back, and an enable through CTRL must start counting from the value last written.
That alone gets past every bounded wait.

Optional (no firmware path needs it today): on each underflow set `ists |= 1 << (3n+2)`;
raise IRQ 1+n while `ists & ien & (1 << (3n+2))`. For a scheduler-driven variant, schedule
the next underflow only when that IRQ is enabled; otherwise compute VALUE lazily on read, as
above.

Treat the block as reset only on power-on. The firmware re-initializes it on every system
reset (its init flags live in scatter-loaded RAM), so keeping state across SYSRESETREQ is
harmless.

Map it at 0x4010_1000 (size 0x1000). Mapping the same device at 0x5010_1000 is harmless, but
no code uses that address.

### Emulator implementation (`evo_hw::dev::timer::Timer`)

The emulator implements the model above, registered as `"timer"` at 0x4010_1000/0x1000 by
`evo_hw::soc::attach`. Decisions for the open questions below:

- **Interrupts are not modeled.** INT_STAT is never set, so its W1C writes have no visible
  effect, and IRQs 1-3 are never asserted. INT_EN stores and reads back; the first write of
  a non-zero INT_EN logs a `log::warn!` ("timer interrupts not modeled") once per device
  instance. The optional underflow/IRQ behaviour above is not implemented.
- **Underflow reloads from LOAD** (open question 3), with period LOAD + 1 ticks.
- **Disabled channels are frozen**, and a VALUE write while enabled restarts the count from
  the written value at the current cycle (open question 5).
- **CTRL bits 3n+2 and 9+n** are stored only (open question 2). The 39,000 Hz rate is used
  whenever CTRL bit 3n+1 is clear (open question 1).
- Only aligned 32-bit accesses reach registers. Other widths, and the offsets the table does
  not list, read 0, ignore writes and are logged as unhandled.
- A System reset keeps every register. A PowerOn reset clears them all to 0.
- The saved state is VALUE, LOAD and `since` for each channel, plus CTRL, INT_STAT and
  INT_EN. The warn-once flag is not saved.

## Related blocks the boot path touches (or does not)

- **Clock gates.** Platform init 0x100B2798 calls 0x100B5B68(mask, 1) for masks 0x40,
  0x8000, 0x80, 0x100, 0x10000, 0x400000, 0x1000, 0x200, 0x10, 0x2000000, 0x800000. The clock
  controller is 0x5020_0000 (descriptor 0x100B7C88): bits 0-16 go to +0x14 and `mask >> 17`
  to +0x18 (`security.md` §3). One of these is probably the timer gate. Nothing reads it
  back, so plain storage is enough.
- **SecureWatchdog (IRQ 12).** Secure: stub 0x100B1F88 → unexpected-IRQ 0x100B0E84. NS-part:
  stub 0x11786C. No watchdog base address appears in either image (no SSE-200 0x5002_E000 /
  0x4008_1000 style constant). No model needed.
- **RTC (IRQ 15), 0x4010_2000.** See `rtc.md`.
- **SysTick.** The NS RTOS tick: CTRL = 6, then the enable bit, at 0x114034 / 0x113FA8,
  handler vector 0x00100711. The secure SysTick is a stub.

## Open questions

1. **ch2's clock with CLK32K clear.** Is it 39 kHz (APB 39 MHz / 1000, or an LF RC
   oscillator)? The firmware's /39000 conversion is the only evidence. Nothing on the boot
   path uses it.
2. **CTRL bits 3n+2 and 9+n.** Init clears both, nothing sets them, and the meaning is
   unknown (IRQ enable? one-shot? cascade?). Store them only.
3. **Underflow behaviour.** Does the counter reload from LOAD, wrap through 0xFFFF_FFFF, or
   stop (one-shot)? With LOAD = ~0 the results are the same. Reload-from-LOAD is assumed,
   and the emulator implements it.
4. **INT_STAT bit position.** It equals bit 3n+2 (2, 5, 8), the same position as the unknown
   CTRL bit. It may be a shared bit layout, and the status may not be set unless CTRL bit
   3n+2 is set.
5. **VALUE reads while disabled, and VALUE writes while enabled.** The firmware does neither,
   so the model's choices (frozen; restart from the written value) are unverified. The
   emulator implements those choices.
6. **Clock-gate bit for the timer** in 0x5020_0014/18 is unidentified. The firmware does not
   depend on it.
