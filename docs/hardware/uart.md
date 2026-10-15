# UARTs: UART0 (0x4010_6000, IRQ 17) and UART2 (0x4010_B000, IRQ 27)

Status: static analysis of both images, confirmed by later boot runs (the TF-M banner and
`Starting BSP 7.1.0.4413` both appear on UART0).

## 0. Which code is where (this matters for the UART story)

There are three separate programs. Only two of them touch UART registers.

| program | location | runs as | UART use |
|---|---|---|---|
| TF-M secure firmware | image 0, slot+0x0000..~0x50000, executes at `0x100B_0000+` | Secure | UART0, TX only, polled (TF-M log) |
| **NS kernel/BSP** (RTOS, MCCI USB, drivers, factory tests) | image 0, slot+0x50000.., NS alias `0x0010_0000..0x0018_B4F3`. Vector table at **`0x0010_0400`** (SP `0x2004_6D20`, reset `0x0011_6D51`) | Non-secure | UART0 as C-library stdio (TX polled, RX polled-blocking); UART2 as an interrupt-driven RX / polled-TX link transport |
| NS OS / calculator app | image 1 at `0x0038_0000` | Non-secure | **No direct register access.** It reaches stdio through the kernel syscall table (see §5) |

The BSP always runs: secure boot hands off to it (`boot-flow.md`), and it prints
`Starting BSP 7.1.0.4413` on UART0.

Evidence for the NS kernel inside image 0:
- A vector table sits at slot+0x50400: SP `0x2004_6D20`, reset `0x0011_6D51`, and all its
  handlers point into `0x0010_xxxx-0x0011_xxxx`.
- The reset handler at `0x116d50` sets MSPLIM/PSP/PSPLIM, drops to unprivileged thread mode,
  and calls `__main` at `0x1004b4`.
- `__main` runs an armlink scatter-load. Its table at `0x0018_A644` decompresses
  `0x0018_A7A8` to `0x2001_0000` (len 0x2DE8) and zero-fills `0x2001_2DE8` (len 0x33B34).

Image 1's entry trampoline (`0x44bc90`) saves `r0` to `[0x2009_B100]`. Its 99+ stubs then do
`push {r0}; mov.w r0,#N; b 0x44bc9e`. That code loads `ip = (*0x2009B100)[N]` and branches.
The kernel hands over `r0 = 0x0018_2024`, the syscall table (`0x11474c`:
`movw r0,#0x2024; movt r0,#0x18; bx lr`).

## 1. Block bases, IRQs, configuration

Both UARTs are described by the same `{base, baud, irq}` triple:
- Secure: `0x100b7c90` = `{0x40106000, 0x1C200, 0x11}`.
- NS kernel (read-only data at NS `0x0017_8900`): `{0x40106000, 115200, 17}`,
  `{0x4010B000, 115200, 27}`.

Just before them sit three separate one-word device descriptors: the backlight PWM
`{0x4010_4000}` at 0x0017_88E8, the RTC `{0x4010_2000}` at 0x0017_88EC and the SPI
`{0x4010_7000, 1 MHz, …}` at 0x0017_88F0. Their NS RAM handles are 0x2001_0370, 0x2001_0378
and 0x2001_0390.

| | UART0 | UART2 |
|---|---|---|
| Base used by firmware | `0x4010_6000` (the NS alias; the secure side uses it too, through SAU region 3) | `0x4010_B000` |
| S alias (by the 0x4/0x5 rule) | `0x5010_6000`, never used | `0x5010_B000`, never used |
| IRQ | 17 (`UART_Handler`) | 27 (`UART2_Handler`) |
| Secure vector | `0x100b2589`: unexpected-IRQ stub (`adr r0,"UART_Handler"; bl 0x100b0e84`) | `0x100b2571`: stub |
| NS vector (`0x0010_0400` + 4*(16+n)) | `0x001189b1`: stub (`adr r0,"UART_Handler"; bl 0x10601c`) | **`0x001187d1`: real handler** |
| ITNS | secure `0x100b41f4` writes ITNS0..15 = 0xFFFFFFFF, then clears bits 7 and 8 (MPC, PPC). Both UART IRQs therefore target NS. | same |
| Clock and baud | Secure: Initialize passes `[0x3000_19C8]`, the CRU reference clock (24 MHz with the CRU values of `security.md` §3; its link-time value is also 24,000,000). NS: passes `[0x2001_0C80]` = **24 000 000** (decompressed `.data`). Divisor = clk/(16*baud) = 24e6/(16*115200) = **13** (DLL=13, DLH=0). | same, 115200 |
| Register window | 4 KiB (`0x4010_6000..0x4010_6FFF`). Only +0x00..+0x14 are touched. | 4 KiB, same |

With an all-zero CRU the secure reference clock is 48 MHz and the secure divisor becomes 26
(`security.md` §3, downstream table). Only the value stored in DLL changes.

UART1 does not exist: no `{base, baud, irq}` triple other than these two appears, and no IRQ
is named `UART1`.

## 2. Register map (offsets touched by any image)

All accesses are 32-bit `LDR`/`STR`. Only the low 8 bits carry meaning. No byte or halfword
accesses were seen.

| off | name (DLAB=0 / DLAB=1) | access | who / how |
|---|---|---|---|
| +0x00 | RBR (R) / THR (W) / DLL (DLAB=1, W) | R: pop RX byte. W: TX byte. W with DLAB=1: divisor low = 13 | putc `str r1,[r0]` (S `0x100b70ec`, NS `0x169284`); getc `ldr r0,[r0]` (S `0x100b7054`, NS `0x1691ec`); DLL in init/set_baud |
| +0x04 | IER / DLH (DLAB=1) | W with DLAB=1: DLH = 0. **RMW** with DLAB=0: bit 0 ERBFI set/clear (UART2 only) | NS `0x1691c4` (`IER \|= 1`), NS `0x1691ac` (`IER &= ~1`) |
| +0x08 | FCR (W) / IIR (R) | W only: **7** (FIFOE \| RFIFOR \| XFIFOR, RX trigger 1 byte). **IIR is never read.** | init, last step |
| +0x0C | LCR | RMW sequence: `\|=0x80`, `&=~8`, `&=~4`, `\|=3`, `&=~0x80` (8N1, no parity). set_baud: `\|=0x80` ... `&=~0x80` | init / set_baud |
| +0x10 | MCR | **never touched** (`SetModemControl` returns UNSUPPORTED) | |
| +0x14 | LSR (R) | **bit 0 DR** and **bit 5 THRE** only. TEMT (bit 6) and the error bits are never read. | rx_ready / getc / tx_ready / putc |
| +0x18, +0x1C, +0x7C | MSR / SCR / DW USR | **never touched** | |

No writes are made to LSR, and no write-1-to-clear patterns appear. Interrupt acknowledge
works by draining RBR (§4).

### Low-level driver (the same code exists twice, S and NS)

The device is `{cfg*, data*}`. cfg = `{base, baud, irq}`. data = `{u8 initialized; u32 clk;
u32 baud}`.

| function | Secure | NS kernel | semantics |
|---|---|---|---|
| `uart_init(dev, clk)` | `0x100b6fc8` | `0x16912c` | returns early if clk == 0. Then `LCR\|=0x80`; DLH=(div>>8)&0xFF; DLL=div&0xFF; `LCR&=~8`; `LCR&=~4`; `LCR\|=3`; `LCR&=~0x80`; `FCR=7`; initialized=1 |
| `uart_getc(dev, *c)` | `0x100b7048` | `0x1691e0` | if `!(LSR&1)` return 4, else `*c = RBR` |
| `uart_rx_ready(dev)` | `0x100b705c` | `0x1691f4` | `LSR & 1` (0 if not initialized) |
| `uart_set_baud(dev, b)` | `0x100b7074` | `0x16920c` | checks clk >= 16*b, then the DLAB sequence |
| `uart_tx_ready(dev)` | `0x100b70c8` | `0x169260` | `(LSR>>5)&1` |
| `uart_putc(dev, c)` | `0x100b70e0` | `0x169278` | if `LSR.THRE` then `THR=c`, else return 4 |
| `uart_rx_irq_enable` | none | `0x1691c4` | `IER \|= 1` |
| `uart_rx_irq_disable` | none | `0x1691ac` | `IER &= ~1` |

Excerpt, putc (`0x100b70e0`):
```
ldr   r0,[r0]        ; dev->cfg
ldr   r0,[r0]        ; cfg->base
ldr   r2,[r0,#20]    ; LSR
lsls  r2,r2,#26      ; THRE -> N
itee  pl
movpl r0,#4          ; not ready
strmi r1,[r0]        ; THR = c
movmi r0,#0
```
Excerpt, init LCR/FCR tail (`0x100b7020`):
```
ldr  r3,[r1,#12] ; orr.w r3,r3,#3    ; str r3,[r1,#12]   ; LCR |= 3 (8 bits)
ldr  r3,[r1,#12] ; bic.w r3,r3,#0x80 ; str r3,[r1,#12]   ; DLAB off
movs r2,#7       ; str r2,[r1,#8]                        ; FCR = 7
```

## 3. Users of UART0 (0x4010_6000)

### Secure (TF-M log)
- Driver_USART table at `0x100b7b88`. Device RAM `0x3000_1A78`.
  - Initialize `0x100b0bed`;
  - PowerControl `0x100b0c15`;
  - Send `0x100b0c71`;
  - Receive `0x100b0c21`;
  - Control `0x100b0b81` (only mode 1 = ASYNC + baud; `Control(TX,1)` returns UNSUPPORTED,
    which is ignored);
  - GetStatus `0x100b0bdd` (always 0).
- `stdio_init` `0x100b561c`: Initialize, PowerControl(FULL), Control(ASYNC, 115200),
  Control(0x15, 1).
- `stdio_output_string` `0x100b5650`: spins on GetStatus (always idle), calls Send, spins
  again, then tail-calls GetTxCount.
- **Send** (`0x100b0c70`): for each byte it spins `while(!tx_ready)` and then calls putc.
  After the last byte it spins on THRE once more. **There is no timeout**, so LSR.THRE stuck
  at 0 hangs boot (seen in `discovery-log.md`).
- **RX is unused on the secure side.** `Receive` is reachable only through the table at
  +0x18, and the only two references to `0x100b7b88` (`0x100b561e`, `0x100b5654`) load
  offsets +8/+0x10/+0x28 and +0x14/+0x20/+0x2C.
- No interrupts: the IRQ 17 vector is the unexpected-IRQ stub.

### NS kernel: C library stdio retarget ("Driver_USART0")
- Driver table at NS `0x0017_877C`. Device RAM `0x2001_01A8` = `{cfg 0x00178900, data
  0x200132CC}`.
  - Initialize `0x104789`;
  - Send `0x10480d`: polled per byte, then waits for THRE;
  - Receive `0x1047bd`: **blocking poll of LSR.DR, no timeout**;
  - Control `0x10471d`: same as secure.
- `stdio_init` `0x1666cc` is the **first call in NS `main`** (`0x15fb64`, called from
  `__rt_entry` `0x100630`). It re-runs the full DLL/DLH/LCR/FCR sequence and sets a flag at
  `0x2004_5040`. The BSP banner `Starting BSP 7.1.0.4413` follows on UART0.
- `fputc` `0x166604` / `0x166700`: `Send(&c,1)`. If ITM is enabled (TCR bit 0 at
  `0xE000_0E80` and TER port 0 at `0xE000_0E00`), the byte is also written to ITM stimulus
  port 0 (`0xE000_0000`, spinning while the port reads 0).
- `fgetc` `0x16668c`: `Receive(&c,1)`, a blocking poll.
- armlib retarget functions:
  - `_sys_open` `0x151371`: ":tt" maps to handles 0x8001 (stdin), 0x8002 (stdout), 0x8003
    (stderr);
  - `_sys_write` `0x151401`: calls fputc per byte for 0x8002/0x8003;
  - `_sys_read` `0x1513d1`: calls fgetc for 0x8001;
  - `_ttywrch` `0x151451`: Send 1 byte;
  - `_sys_exit` `0x151325`: prints a return-code message, then hangs.
- Kernel printf users include the "Non-secure Usage Fault" report (`0x11d960`), an
  "Unexpected error in %s" report, and factory or diagnostic menus (LCDC commands, VSC
  loopback, HID reports, flash timing, "QSPI Divider").

## 4. Users of UART2 (0x4010_B000), NS kernel only

"Driver_USART2" table at NS `0x0017_87EC`. Control block at `0x2001_039C`: `[0]`
initialized, `[+4]` device `0x2001_01B0` = `{cfg 0x0017890C, data 0x200132D8}`, `[+0x14]` RX
thread, `[+0x18]` TX thread, RX ring at `+0x30` with 2200 (0x898) bytes.

- **Initialize** `0x104920`:
  - calls `uart_init(dev, 24 MHz)`;
  - ring init;
  - reads `irq = cfg->irq` (27). If irq >= 0, it writes `NVIC_ICPR[irq>>5] = 1<<(irq&31)`
    (`0xE000E280`) and then `NVIC_ISER` (`0xE000E100`);
  - **`IER |= 1` (ERBFI)**;
  - sets initialized = 1.
- **Uninitialize** `0x104afc`: writes `NVIC_ICER` for IRQ 27, then `dsb; isb`, then
  `IER &= ~1`. It signals 0x2000 to both threads and clears initialized.
- **Control** `0x104885`: accepts ASYNC+baud (calls set_baud), TX(0x15) and RX(0x16). The
  others return errors.
- **Send** `0x104a4d` and the TX thread `0x1192d1`:
  - writes bytes while `LSR.THRE` and parks on event 0x1000 otherwise; the TX thread polls
    THRE before each byte;
  - after the last byte it waits for THRE, then calls the callback with
    `ARM_USART_EVENT_SEND_COMPLETE` (1);
  - **THRE interrupts are never enabled** (only IER bit 0 is ever touched).
- **Receive** `0x1049b5` and the RX thread `0x119241`: copy from the ring, wait on event
  0x4000/0x1000, then call the callback with `RECEIVE_COMPLETE` (2).
- **UART2_Handler `0x1187d0`** (IRQ 27):
  ```
  ldr  r0,[r7,#4] ; bl 0x1691ac        ; IER &= ~ERBFI
  ldr  r0,[r7,#4] ; bl 0x1691f4        ; while (LSR & DR)
  ...             ; bl 0x1691e0        ;   c = RBR  (pop)
  ...             ; bl 0x1180d8        ;   ring_push(c)
  ldr  r0,[r7,#20]; mov.w r1,#0x4000 ; bl 0x162044   ; wake RX thread
  ldr  r0,[r7,#4] ; bl 0x1691c4        ; IER |= ERBFI
  ```
  The handler **does not read IIR**. It relies on the interrupt deasserting once the RX FIFO
  is empty, which makes it a level-sensitive source: (ERBFI && DR).
- Who opens it: a link-transport ops table in `.data` at `0x2001_1754` = `{0x14fd55, open
  0x1188d1, close 0x1188b1, read 0x118921, write 0x11896d}`. It sits next to a USB transport
  (`0x2001_1740`: `0x10c5ad`...) and the name `"USBIO"`.
  - The open function (`0x1188d0`) does: PowerControl(OFF), Uninitialize,
    Initialize(cb `0x1693e5`), PowerControl(FULL), Control(ASYNC, 115200), Control(TX,1),
    Control(RX,1).
  - It stores the driver in the link-manager state `0x2001_3580+0x3C`.
  - read/write use timeouts (`0x118920` waits on flag 0x11 with timeout = ms*ticks/1000).
  - Inference: UART2 is an **alternative wire transport for the same link/xfer protocol as
    USB** (cable/dock/test fixture). The USB path's `0x10c5ad` uses the same callback
    `0x1693e5`.

## 5. Does the NS OS (image 1) log to a UART?

Not directly.
- Image 1 contains **no** UART base in literals or in `movw`/`movt` (no `movt #0x4010`
  anywhere in image 1). The 4-byte-aligned `0x4010_60xx` hits are RGB565 pixel data.
- Its C library I/O goes through the kernel syscall table at `0x0018_2024`. That table's
  indices 2..9 are the armlib retarget functions above:

| idx | kernel fn | role | image-1 caller |
|---|---|---|---|
| 2 | `0x151371` | `_sys_open` | `0x47d834` |
| 4 | `0x151401` | `_sys_write` -> UART0 TX (+ ITM) | `0x4475d2` (armlib `_writebuf`/fflush) |
| 5 | `0x1513d1` | `_sys_read` -> UART0 RX, **blocking poll** | `0x47d54a` (armlib `_readbuf`) |
| 6 | `0x151451` | `_ttywrch` -> UART0 TX | `0x39a2a4` (armlib error/signal message printer) |

So anything image 1 sends to `stdout` or `stderr` (printf, `__rt_raise` messages) ends up on
**UART0 at 0x4010_6000**, interleaved with TF-M's secure log on the same UART. Image 1 has
few format strings that look like logs. The obvious ones are MicroPython's bytecode-dump
strings (which probably use MicroPython's own print path, not checked) and error-code
dialogs, which go to the screen. Expect little or no UART output from image 1 in normal
operation. The kernel's own fault and diagnostic printf output also goes to UART0.

There is a **debug console input path**: `fgetc`/`_sys_read` on UART0. It is a blocking poll
of LSR.DR, with no IRQ 17 handler. It is reachable only if something reads stdin. No
boot-time caller is known. If one exists, the emulator must either provide bytes or the
guest blocks forever.

## 6. Reset values the firmware expects

Nothing is read before it is written, except LSR. Required:
- **LSR = 0x60** (THRE | TEMT) when the TX side is idle. DR = 0 when the RX FIFO is empty.
- IER = 0 at reset (UART2's IER bit 0 is only ever set by RMW).
- LCR = 0 (DLAB clear, so +0x00 and +0x04 decode as THR/IER after reset).
- IIR read: never done. Return 0x01 ("no interrupt pending"), or 0xC1 with FIFOs enabled,
  for realism.

## 7. Timing and interrupts

- TX: polled THRE everywhere. Making THRE permanently 1 (instant transmit) is enough for all
  code. A baud-accurate model (86.8 µs/byte, about 13.5k cycles at 156 MHz) is optional and
  buys nothing for the firmware.
- RX: only UART2 uses interrupts (IER bit 0 → IRQ 27). It is level-sensitive: asserted while
  `IER.ERBFI && rx_fifo non-empty`. The handler masks ERBFI, drains, then unmasks. An unmask
  with data still present must re-assert.
- UART0 RX: polled only. IRQ 17 has no NS or S handler. If IRQ 17 fired while enabled, the
  NS stub would call the unexpected-IRQ fatal path. UART0 never sets IER, so a correct model
  never asserts IRQ 17.
- THRE interrupt (ETBEI, IER bit 1), line-status interrupt (ELSI) and character timeout are
  never used.

## 8. Minimal model specification (one `Uart16550` type, two instances)

```
struct Uart16550 {
    irq: u16,                    // 17 / 27
    ier: u8, lcr: u8, mcr: u8, scr: u8, dll: u8, dlh: u8,
    fcr_fifo_en: bool,
    rx: VecDeque<u8>,            // cap 16 (hardware FIFO). Host injection queue is separate and unbounded.
    host_rx: VecDeque<u8>,       // bytes injected from host, fed into `rx` while rx.len() < 16
    tx: Vec<u8>,                 // transmitted bytes, drained by the Machine (take_tx)
}
```
Map with `add_device("uart0", 0x4010_6000, 0x1000, ..)` and `add_device("uart2", 0x4010_B000,
0x1000, ..)`. The S aliases `0x5010_6000` and `0x5010_B000` are never used by the firmware.

Reads (any width; use `off & !3`, return the byte in bits 7:0, 0 elsewhere):
- `0x00`:
  - DLAB=1: `dll`;
  - else pop `rx` (0 if empty), then refill one byte from `host_rx`.
- `0x04`: DLAB ? `dlh` : `ier & 0x0F`.
- `0x08` IIR:
  - if `ier&1 && !rx.empty` → `0x04` (RX data available);
  - else if `ier&2` (THRE always empty) → `0x02`;
  - else `0x01`.
  - OR in `0xC0` if `fcr_fifo_en`. Not read by firmware; this is only for realism.
- `0x0C`: `lcr`. `0x10`: `mcr`. `0x18` MSR: `0xB0` (CTS|DSR|DCD; unused). `0x1C`: `scr`.
- `0x14` LSR: `0x60 | (rx non-empty as bit 0)`. The error bits (1-4, 7) are always 0.
- Anything else: 0.

Writes:
- `0x00`: DLAB ? `dll = v` : append `v as u8` to `tx` (instant transmit; THRE stays 1).
- `0x04`: DLAB ? `dlh = v` : `ier = v & 0x0F`, then re-evaluate the IRQ line.
- `0x08` FCR: `fcr_fifo_en = v&1`; if `v&2`, clear `rx` (not `host_rx`); bit 2 (TX reset) is
  a no-op.
- `0x0C`: `lcr = v`. `0x10`: `mcr = v` (if `mcr&0x10` loopback: THR writes go to `rx`
  instead of `tx`; optional). `0x1C`: `scr = v`.
- `0x14`, `0x18`: ignored.

IRQ line (level), computed after every access and whenever host bytes are injected:
`asserted = (ier & 1 && !rx.empty) || (ier & 2 != 0)`. THRE is always empty, so ETBEI
asserts permanently. That matches 16550 behaviour closely enough, and the firmware never
sets it. The device drives the line with `DevCtx::set_irq(irq, asserted)` (`memory-map.md`,
"Device runtime decisions").

Host API:
- `inject_rx(bytes)`: push to `host_rx` and top up `rx` to 16. On UART2 with ERBFI set this
  raises IRQ 27.
- `take_tx() -> Vec<u8>`: drains the transmitted bytes (the CLI streams UART0 to stdout).

Divisor and LCR only need storing. Optionally log a warning when the divisor is not 13 or
LCR&0x3F != 3 when DLAB clears.

Tests to write:
1. Secure `stdio_init` sequence → DLL=13, DLH=0, LCR=0x03, FCR wrote 7; the TF-M banner
   appears in the TX bytes.
2. LSR reads 0x60 at reset, and 0x61 after `inject_rx(b"x")`; reading RBR returns b'x' and
   LSR returns to 0x60.
3. UART2: with IER=1, inject → IRQ 27 asserted. Writing IER=0 → deasserted. Writing IER=1
   with data still queued → asserted again. Draining → deasserted.
4. An FCR write of 7 clears pending RX.

## 9. Open questions

1. **UART2's peer.** It is an alternative transport to USB for the link/xfer layer, but
   whether real hardware connects it to anything (dock, test fixture, wireless/Innovator
   module) is unknown. The transport is selected at run time through `0x2001_3580`. Nothing
   shows it is opened during a normal boot. Confirm by running the NS kernel and watching for
   writes to `0x4010_B00C` / `ISER` bit 27.
2. **Is stdin ever read at boot?** `_sys_read` (idx 5) has one image-1 caller (armlib
   `_readbuf`). If anything calls `getchar`/`scanf` during boot, UART0 RX blocks until bytes
   arrive. The model's `inject_rx` covers this.
3. **PPC gating.** Whether the APB PPC must mark `0x4010_6000`/`0x4010_B000` NS-accessible
   before NS accesses work is for the SPCTRL/PPC model (`security.md` §2). The model should
   not enforce it.
4. **ITM mirroring.** Kernel fputc also writes ITM stimulus port 0 when TCR.ITMENA and TER
   bit 0 are set. If the emulator ever enables ITM, the same text would appear twice.
5. MicroPython's console path (`mp_hal_stdout_tx_strn`) in image 1 was not traced. It might
   use a different syscall index than stdio.
