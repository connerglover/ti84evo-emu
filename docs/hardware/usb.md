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

### Device-mode model and evidence

The basic map matches the [Linux FOTG210 UDC header](https://github.com/torvalds/linux/blob/master/drivers/usb/fotg210/fotg210-udc.h)
and [U-Boot FOTG210 OTG definitions](https://android.googlesource.com/platform/external/u-boot/+/refs/heads/android-tv-9.0.0_r1/include/usb/fotg210.h).
The Evo firmware adds per-FIFO virtual DMA and uses **write-one-to-clear**
interrupt status, unlike the older Linux driver's write-zero acknowledgment.
Those differences were established from BSP disassembly and guest MMIO traces.

| Offset | Modeled behavior |
|---|---|
| `+000`, `+100` | Host/device soft-reset requests self-clear; device reset aborts pending transfers |
| `+080` | PC attachment: B-device, peripheral role, B-session valid (bits 21, 20, 17) |
| `+0C0`, `+0C4` | Device interrupt summary and global masking |
| `+0E0`, `+0E4` | Read-only boot-compatible ID `0x00010000`, config `0x880`; physical values remain unknown |
| `+100`, `+104` | Device control/global interrupt enable; guest-written address/configured flag |
| `+120` | Control FIFO count/empty, FIFO empty bits, stall, clear, control-complete handshake |
| `+130..13C` | Group/source interrupt masks |
| `+140..14C` | Group summary and W1C sources: SETUP, IN/OUT, command end, reset, DMA completion/error |
| `+160..19C` | Endpoint packet sizes and stall flags |
| `+1A0..1AC` | Endpoint/FIFO mapping and configuration |
| `+1B0..1BC` | FIFO byte counts and self-clearing FIFO reset |
| `+1C0`, `+1C8`, `+1CC` | Legacy target-FIFO DMA, length/direction/start and memory address |
| `+1D0` | SETUP packet read port |
| `+300..324` | Virtual-DMA control/address pairs: control FIFO `300/304`, FIFO0 `308/30C`, stride 8 |
| `+328`, `+32C` | Virtual-DMA completion/error status and mask; group-summary bit 3 |

BSP `0x0012_6100` and `0x0012_63D4` acknowledge source bits by writing the
observed status. `0x0012_63E6` handles virtual-DMA completion/error.
`0x0012_7024` programs count (bits 24:8), address and START bit 0;
IN uses direction bit 1. Real SETUP, descriptor and CDC traces corroborate
these paths. Receive DMA waits for data, continues across full packets and
completes at its requested count or a short packet. DMA accesses are checked
against SRAM/flash buffers and their secure aliases; invalid ranges raise error.

Host APIs `Machine::usb_attach`, `usb_bus_reset`, `usb_setup`, `usb_out` and
`usb_in` carry tokens through guest registers, interrupts and FIFOs. They do not
manufacture descriptors, serial replies or calculator commands. IN/OUT report
NAK, stall, disconnected, endpoint and packet-size errors. The host negotiates
full speed; CDC bulk maximum packet size is 64. Bus reset and detach discard
pending transfers; new SETUP aborts old control data and clears control stall.

### Long-request wedge and ACK corruption (7.1 firmware)

Hardware report: a long directory GET (`hh01/inf/res?name=directory&x=` +
200 `A`s, answered with remote error `UN`) followed by a long transfer GET
(`hh01/xfr/var?name=%EE%A0%B0&type=1&x=` + 100 `%41`s) leaves USB
unresponsive; later directory GETs stay unanswered and replug restores
service. Either request alone, or the same sequence with short tails,
reportedly survives on hardware.

Confirmed emulator behavior (real firmware, `crates/evo-core/tests/usb_timing.rs`):

- The two-request sequence wedges exactly as reported and a replug revives
  USB, in both token pacings (`hardware_two_request_wedge`).
- The guest acknowledges a Kermit F packet by echoing its URI in the Y-1
  ACK. For the long transfer request the staged ACK frame is malformed:
  `MARK=0x31`, `SEQ=0x51`, declared length 256, with a **self-consistent
  checksum** — the guest firmware built it that way
  (`hardware_ack_staging_corruption_signature`).
- The corruption is a guest SRAM buffer overlap, not a USB-model fault: the
  Kermit frame builder (entry `0x0016_6194`, called from `0x0016_3a04`)
  loads MARK from `[r0+26]` of the session struct at `0x2003_bdbc`, and the
  Kermit decode loop (`strb` at `0x0015_46c8`) writes decoded URI bytes over
  that struct — exactly one write puts `0x31` (`'1'` of the URI) into the
  MARK field, which the builder then reads back
  (`hardware_ack_mark_field_corruption_chain`). The builder entry contract,
  staging buffer (`0x2003_ce84`) and struct base are pinned by
  `hardware_ack_builder_entry_contract`. VDMA, FIFO and host parsing carry
  the staged bytes unchanged.
- The overlap is length- and history-dependent: for the directory request
  the guest stays healthy through a 299-byte echo and breaks by a 309-byte
  echo (`hardware_ack_echo_boundary`); short-tail sequences pass in both
  pacings (`individual_long_requests_and_short_sequence_survive`).

Unresolved hardware difference: the **isolated** long transfer request
reportedly survives on hardware but fails in the emulator (fresh boot, both
pacings; `hardware_isolated_long_xfr_fresh_boot`, kept `#[ignore]`d).
Since the guest firmware and its buffer placement are the emulator's own, the
divergence needs a hardware capture of the isolated request (host F packet
and guest ACK bytes) to settle — candidate explanations are a different
client-side F composition or a different guest heap layout after a different
session history. No USB-model change fixes this without that evidence.

The OTG interrupt-enable `+0x088 = 0x40` observed in traces is
B-session-end per the FOTG210 definitions, not SOF; no verified SOF bit was
found and none is modeled.

### Verified guest behavior

`crates/evo-core/tests/real_os_usb.rs`, with a user's `EVO_OS_IMAGE` in release,
boots and provisions the guest, attaches through Type-C, resets the bus, reads
device/configuration descriptors, sets address/configuration, programs CDC
line coding (115200 8N1), asserts DTR/RTS and exchanges the TI Connect Evo Kermit
send-init and its guest-generated acknowledgment. It repeats enumeration and
the serial handshake after detach, reattach and bus reset. It does not force
guest memory, NVIC or USB-control-register state.

The guest advertises VID:PID **0451:E018** and four interfaces:

- Interface 0: CDC ACM control, interrupt IN endpoint 3.
- Interface 1: CDC data, bulk OUT endpoint 1 and IN endpoint 2.
- Interface 2: vendor interface, bulk OUT endpoint 5 and IN endpoint 6.
- Interface 3: HID keyboard, interrupt IN endpoint 4.

The main OS supplies the 63-byte HID keyboard report descriptor. In the tested
7.1 firmware, `GET_REPORT` and `SET_IDLE` return a guest-generated STALL; the next
SETUP recovers EP0 normally. Ordinary calculator operation does not turn every
key press into a computer keyboard report.

A second real-firmware test enters **Engineering > Tests > Link Test > Computer
Tests > Keyboard Simulator** using the actual boot combo and keypad. It verifies
an eight-byte Enter press report (HID usage `0x28`) and an all-zero release report
on endpoint 4. The BSP vendor interface also echoes 285 bytes, covering all byte
values, full 64-byte packets and a final short packet, through OUT5/IN6. These
are guest code paths through the same DMA/FIFO model, not host response stubs.
The keyboard simulator agrees with the observed physical behavior in
[TI-Planet episode 6](https://tiplanet.org/forum/viewtopic.php?f=41&t=27370).

The vendor echo was verified in the engineering BSP, not as a general-purpose
service in the main OS. Its complete protocol, calculator-as-host, accessory
input, peer-calculator transfers, isochronous traffic, high-speed negotiation
and electrical timing remain unverified. See the
[link acceptance matrix](link-compatibility.md) for broader coverage.

Save-state format **3** records USB cable, queued packets and DMA registers.
Versions 1 and 2 are rejected explicitly. Existing `.evoflash` files remain
compatible; boot them afresh to obtain the corrected Type-C search behavior.

## USB Type-C controller at 0x4010_C000, IRQ 28 `USBTC`

The low register window follows TCPCI. Its offsets and the observed firmware
operations agree with the [USB-IF TCPCI 2.0 v1.3 specification](https://www.usb.org/sites/default/files/USB_TCPCI_R2_V1p3_30122_CLEAN.pdf)
(physical PDF p62, CC_STATUS; p89, connection state machine) and
[Linux's TCPCI register definitions](https://github.com/torvalds/linux/blob/master/include/linux/usb/tcpci.h).
Vendor-specific registers remain only partially understood.

Firmware identity evidence, OS bundle 7.1.0.4421:

- MCCI record `0xDEED0202`, base `0x4010_C000`, at `0x0018_3600`; descriptor at
  `0x0018_3610`. The OTG record nearby uses tag `0xDEED0200`.
- `0x0011_9398` / `0x0011_93A8` read CC1/CC2 from word `+0x1C` bits 9:8/11:10.
- IRQ handler `0x0012_BECE` reads halfword ALERT at `+0x10`; `0x0012_BED0` reads
  ALERT_MASK at `+0x12`. Work is queued only when their intersection is nonzero.
- Deferred handler `0x0012_C85C` handles CC bit 0 and power bit 1. It skips CC
  processing while CC_STATUS bit 5 is set; `0x0012_C97A` extracts POWER_STATUS
  bit 2. Generic byte/halfword accessors are `0x0013_0328`/`0x0013_0310`
  (read) and `0x0013_034E`/`0x0013_0336` (write).

### Modeled attachment behavior

| Offset | Register | Behavior |
|---|---|---|
| `+10`, 16-bit | ALERT | Latched CC-change bit 0 and masked power-change bit 1; write-one-to-clear, including byte lanes |
| `+12`, 16-bit | ALERT_MASK | IRQ 28 is high while `ALERT & ALERT_MASK != 0` |
| `+14`, 8-bit | POWER_STATUS_MASK | Selects power-status changes that latch ALERT bit 1 |
| `+1D`, 8-bit | CC_STATUS | Host-controlled CC1/CC2 in bits 1:0/3:2; host attachment uses bit 4 (calculator presents Rd); bit 5 means connection search is pending |
| `+1E`, 8-bit | POWER_STATUS | Host connection supplies VBUS-present bit 2; modeled commands control sink bit 0 and detection bit 3 |
| `+23`, 8-bit | COMMAND | `22`/`33` disable/enable VBUS detection; `44`/`55` disable/enable sinking; `99` starts connection search while unattached (values hexadecimal) |

`Machine::usb_attach(true)` represents an ordinary powered computer connected
on CC1: CC_STATUS becomes `0x11`, VBUS appears, and alerts notify the guest.
`Machine::set_usb_cc` remains the raw two-field diagnostic interface; it does
not independently negotiate power or create an OTG connection. Both update
the Type-C interrupt when invoked through the core host API. Repeating an
unchanged host attachment creates no new event.

System reset clears configuration and masks interrupts but retains physical
CC, sink termination and VBUS presence. A pending CC alert lets restarted
firmware rediscover the cable. Power-on clears the connection. Save states
preserve the registers and cable fields.

### Why a fresh boot matters

The BSP initially writes ROLE_CONTROL `0x4A`, then COMMAND `0x99`, then reads
CC_STATUS. The old storage-only model returned zero immediately. That falsely
completed the dual-role search before a cable arrived, clearing the guest's
pending role-selection flag. Merely supplying CC values and IRQ 28 later left
the OTG controller disabled.

Returning CC_STATUS `0x20` during this search fixes the sequence. In a fresh
real-firmware run, attachment produces `0x11`; about 100 ms later the guest
writes ROLE_CONTROL `0x0A`, enables its device stack, changes OTG DMCR from
`0x220` to `0x224`, clears GMIR from 3 to 0, and enables NVIC IRQ 16. A subsequent
GET_DESCRIPTOR returns device VID:PID **0451:E018** through guest VDMA, without
forcing guest registers or firmware state. An old saved state can retain the
previously incorrect guest role state and is not equivalent to a fresh boot.

Dedicated tests in `crates/evo-hw/tests/usb_typec.rs` cover search completion,
masked alerts, W1C acknowledgments, host attach/detach, power commands, reset
and state restoration. Successful device enumeration is narrower than full
TI Connect compatibility; see [the acceptance matrix](link-compatibility.md).

### Remaining limits

This is a sink-side attachment model, not a full TCPCI/USB-PD implementation.
Source-role power, accessory/peer negotiation, PD messages, electrical timing,
and vendor-specific behavior are unverified. Unmodeled register bits remain
storage. Type-C identification/capability values and the real OTG ID/version
remain unknown; guessed capability bits were not needed for the verified
attachment path. TI-Planet's role/accessory menus describe later acceptance
targets, not currently verified emulation.
