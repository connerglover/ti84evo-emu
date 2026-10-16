# USB/link compatibility and TI-Planet evidence

Status: 2026-10-15. This checklist separates a modeled device mechanism from a
verified TI Connect Evo operation. Controller development does **not** by itself
establish browser compatibility. End-to-end rows remain pending until exercised
through the actual guest firmware and host protocol.

## Acceptance matrix

| Behavior | Evidence | Status / required proof |
|---|---|---|
| Read either USB-C orientation | Self-tests [episode 5](https://tiplanet.org/forum/viewtopic.php?f=41&t=27368), PDF p5 shows CC pairs (3,0), (0,2), (0,0) | Supported: core exposes both fields; GUI attachment recognizes either field after state load. Regression test covers both orientations, detach, guest readback and reset attachment. Full electrical meaning of each code remains unverified. |
| Retain cable presence across system reset | Core host/device contract in [usb.md](usb.md) | Supported by model tests; not a claim about every Type-C role transition. |
| Connect TI Connect Evo to the emulator | [Episode 10](https://tiplanet.org/forum/viewtopic.php?f=41&t=27399), PDF pp1-2 | Pending: host-visible CDC/serial integration, enumeration and guest exchange. The app uses an Evo-specific protocol; CE link packets and UART2 traffic are not substitutes. |
| List, send and retrieve variables | Episode 10, PDF p2 | Pending end-to-end directory/read/write, completion, rejection, cancellation and reconnect checks. |
| Capture screenshots repeatedly | Episode 10, PDF pp2-4; [official TI page](https://education.ti.com/en/products/calculators/graphing-calculators/ti-84-evo/ti-connect-evo) advertises auto-refresh | Pending over USB. Existing direct framebuffer screenshots verify display output, not the transfer command. |
| Send Python and images | Episode 10, PDF pp5-6 | Pending. TI Connect converts `.py` and ordinary images on the host before transfer. |
| Update OS and Apps | Episode 10, PDF p2; [episode 3](https://tiplanet.org/forum/viewtopic.php?f=41&t=27363), PDF p7 | Pending package transfer, verification, interrupted-transfer handling and reboot. Flash-file import is not evidence of USB update compatibility. |
| Exit exam mode explicitly | [Episode 9](https://tiplanet.org/forum/viewtopic.php?f=41&t=27383), PDF p9 | Pending guest state transition through host command. |
| Exit exam mode by receiving a variable | Episode 9, PDF p9 | Pending separate scenario; article describes this behavior generally, so verify against the loaded firmware. |
| Recovery reception with 2nd+Del at startup | [Episode 1](https://tiplanet.org/forum/viewtopic.php?f=41&t=27360), PDF p6 | Pending; booting installed OS images directly does not emulate the original BL1/BL2 recovery path. |
| Peer calculator transfer / exam exit | Episode 9, PDF p9 | Pending host-role and peer integration; peer uses 2nd Link -> Test Mode -> Exit. |
| USB keyboard, accessory and loopback diagnostics | [Episode 6](https://tiplanet.org/forum/viewtopic.php?f=41&t=27370), PDF pp5-6 | Pending separate USB host/HID behaviors; not prerequisites for the browser's CDC transport. |

Page numbers refer to the supplied PDF's physical pages, not the printed phpBB
page count. These articles describe early physical Evo/Evo-T units; feature
presence in a menu is weaker evidence than successful execution.

## Host transport boundary

The current official client research identifies WebSerial / CDC with TI vendor
`0x0451`, product `0xE018`, opened at 115200 baud, 8 data bits, no parity and one
stop bit. This is static client evidence, not a successful connection to the
emulator. The application is served from
[TI Connect Evo](https://connectevo.ti.com/ticevo/en/); its connectivity library is
`assets/library/ticonnect.min.js`. Protocol and controller evidence belongs in
[usb.md](usb.md) as it is established.

A complete acceptance run needs a host-facing serial bridge or USB device
presentation as well as the emulated controller. Neither setting CC fields nor
exposing endpoint test APIs alone makes the emulator selectable in the browser.
The PDFs supply no descriptor bytes, endpoint numbers, packet layouts or register
maps. Derive those from client code, firmware and observed exchanges.

## Other link observations

Episode 6 p5 shows DRP, DRP-Sink, DRP-Source, SINK and SOURCE roles, independent
port/device/host power controls, and TCPD register/trace diagnostics. Episode 5 p5
also shows continuous device and host tests. These are broader than device-side
TI Connect transfers.

Episode 6 p6 explicitly reports the calculator acting as a computer keyboard,
including letters, arrows, function keys and Clear as Delete. The same review
reports two-calculator echo working but handheld test mode failing; Innovator
also failed, possibly because of an adapter. Do not mark every listed accessory
supported on the strength of its menu entry. XFR small/large get/put/delete and
loop variants on pp4-5 provide later transfer-test scenarios.

## Hardware and data-format cross-check

- [Episode 2](https://tiplanet.org/forum/viewtopic.php?f=41&t=27362), pp2,4-5:
  Winbond 25Q128JVSQ / 16 MiB NOR and 156 MHz agree with the model. Its RAM
  estimate is only **at least** 1 MiB; it does not contradict the firmware-backed
  1.5 MiB map. Its update-partition explanation is conjecture.
- [Episode 8](https://tiplanet.org/forum/viewtopic.php?f=41&t=27374), pp1-4:
  physical LCD is 320x240; graph area is 319x209. Pic `.8ci2` is 320x209 with a
  4-bit palette including transparency; background Image `.8ca2` is 160x105
  RGB565, doubled and clipped. Use Evo formats rather than CE dimensions.
- Episode 5 pp3-6 and episode 6 pp3-4: LCD patterns, brightness levels, NOR
  read/write/erase, keypad, QSPI divider/drive and battery GPIO diagnostics
  broadly match modeled blocks. Their presence is not evidence every diagnostic
  passes. Battery monitor pins 5/7/12 and 11 agree with [gpio.md](gpio.md).
- Episode 2 p2 and episode 9 p1: Evo-T has the exam LED; standard Evo lacks it.
  The charge LED is a School Property variant feature.
- Episode 1 p5: cold/reset-button boot differs from ordinary off/on resume.
  [Episode 7](https://tiplanet.org/forum/viewtopic.php?f=41&t=27371), p2 removes
  user-visible clock commands, not necessarily the internal RTC.
- [Episode 4](https://tiplanet.org/forum/viewtopic.php?f=41&t=27365), pp5-6 gives
  inconsistent 8.36/8.31-second calculation timing. Episode 7 pp4-5 reports a
  117.81-second TI-Basic test but does not establish deliberate throttling.
  Neither is an exact cycle-timing specification.
