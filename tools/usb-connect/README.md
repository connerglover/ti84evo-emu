# TI Connect USB adapter

The emulator now accepts real USB bus tokens. Its firmware supplies descriptors,
CDC data, and calculator protocol responses. This adapter runs TI Connect Evo's
communications library unchanged against that bus. It does not fake calculator
commands or read files/screenshots directly from emulator memory.

Build `cargo build --release -p evo-cli`. Node.js 22 or later is recommended.
Supply your own `.84b2` bundle. No TI firmware or client code is included here.

## Browser interface

```powershell
node tools/usb-connect/serve.mjs `
  --cli target/release/evo-cli.exe --bundle path/to/OS.84b2 `
  --flash path/to/calculator.evoflash --provision
```

Open the printed loopback URL in Chrome or Edge. The page supports connection,
screenshots with auto-refresh, variable download/upload, Python and image import,
exam exit and OS bundle transfer. It loads the official communications library
into a local host page;
the emulator is not a Windows USB device selectable on TI's public website.
The server downloads the three public TI scripts into memory at startup, or
accepts `--library path/to/client-library` for an existing local copy.
Ctrl-C saves flash and stops the emulator. Do not use the same flash file in two
running emulators.

Connection, attributes, screenshots, directory listing, Python/image transfers
and disconnect/reconnect were exercised in stock Chromium. Helium repeatedly
reloaded during the TI library's worker initialization in testing; use stock
Chrome or Edge if that occurs.

## Protocol verification

Download `ticonnect.min.js`, `ticonnect.js`, and `ticonnect.worker.js` from the
[official client library directory](https://connectevo.ti.com/ticevo/en/assets/library/ticonnect.min.js)
into a local directory. Then, from the repository root:

```powershell
node tools/usb-connect/client-smoke.mjs `
  --cli target/release/evo-cli.exe --bundle path/to/OS.84b2 `
  --library path/to/client-library --flash path/to/calculator.evoflash --provision
```

`--provision` performs first-use Engineering-menu setup through the normal
firmware/keypad path. Omit it after the first successful launch. A saved flash is
booted as-is, including OS updates received through USB; the original bundle
initializes a new flash only. An invalid saved image is an error, not a reason to
reinstall the bundle silently. EOF, `quit`, and Ctrl-C save the flash.

The smoke test activates the calculator, reads its attributes and directory, and
captures two 320×240 screenshots over USB. `--output screen.rgba` saves the first
capture as raw RGBA. `--roundtrip` additionally receives the standard L1 variable,
sends the same contents back with overwrite enabled, receives it again, and
requires byte-for-byte equality. Use a disposable flash copy for verification.
`--python-roundtrip` converts a small Python source file with the official
library, transfers it, receives identical source text, then deletes the test
variable. `--install-bundle path/to/OS.84b2` transfers an OS package. These options
modify the guest's flash; use a disposable copy for verification.
OS transfer success confirms staging. Reopening a saved, valid permanent update
activates it at the boot boundary. Automatic activation is not established:
letting a same-version reinstall run for five extra guest seconds erased its
staging slot without an observed system reset. Cross-version upgrades remain
unverified; see [boot-flow.md](../../docs/hardware/boot-flow.md).
`--recovery-checks` sends malformed L1 data, requires the guest's rejection and
unchanged readback, then cancels a screenshot after data begins arriving and
requires a successful directory read and full screenshot afterward. The observed
rejection is client code 11 (guest disconnect); cancellation is code 9.
`--trace` logs bus payloads. `--exit-ptt` verifies an active-to-inactive exam-mode
transition using the official dynamic device-info API. The firmware deliberately
disconnects before completing that operation; the adapter lets it finish and
checks the resulting state. `--skip-screenshots` isolates that workflow from
screen-capture restrictions in exam mode.

### Exam-mode verification

Create a disposable baseline through the real Left+Right boot combo and ZOOM
(START/OK) setup buttons. Run each exit path on a separate copy:

```powershell
cargo run --release -p evo-core --example usb_exam_fixture -- path/to/OS.84b2 ptt-baseline.evoflash
Copy-Item ptt-baseline.evoflash ptt-explicit.evoflash
node tools/usb-connect/client-smoke.mjs --cli target/release/evo-cli.exe --bundle path/to/OS.84b2 --library path/to/client-library --flash ptt-explicit.evoflash --keys enter --skip-screenshots --exit-ptt
Copy-Item ptt-baseline.evoflash ptt-variable.evoflash
node tools/usb-connect/client-smoke.mjs --cli target/release/evo-cli.exe --bundle path/to/OS.84b2 --library path/to/client-library --flash ptt-variable.evoflash --keys enter --skip-screenshots --receive-ptt
```

Both tests require official `getDeviceInfo().p2t` to change from `"true"` to
`"false"`. Explicit exit acknowledges the upload, sends Kermit `DI`, and returns
client code 11 (disconnect) while the guest finishes asynchronously. The shared
`exitExamMode` operation advances five guest seconds and checks the reported
state; a disconnect alone is never considered success. The variable scenario
sends the received L1 list back and verifies the transition and byte-identical
readback. `--keys "enter"` taps the real keypad before connecting.

## Transport details

The official app uses WebSerial, VID `0451`, PID `E018`, at 115200 8N1. Enumeration
and CDC controls run through endpoint zero. Bulk endpoint numbers and packet sizes
come from the guest configuration descriptor. All serial traffic then crosses the
emulated DMA/FIFO/IRQ path. The current firmware exposes CDC OUT1/IN2; later vendor
bulk endpoints are deliberately not mistaken for the serial port.

`evo-cli usb-bridge` provides newline-delimited JSON on stdin/stdout. Operations
are `attach`, `reset`, `setup`, `out`, `in`, `run`, `key`, and `quit`; responses are
`{"ok":true,"data":...}` or `{"ok":false,"error":...}`. `Nak` is retryable;
`Stall` and other device errors propagate to the caller. Diagnostics go to stderr.
`{"op":"key","name":"enter","pressed":true}` presses a named keypad key;
send `pressed:false` to release it and `run` to allow guest processing.

The official app initializes four globals before loading its library. Their
observed values are reproduced in `client-environment.mjs`; without that host
initialization, the library selects zero-duration read timeouts and substitutes
errors for completed transfers. The library files themselves are not modified.
These host integration details were observed in the public
`main.4fb0b73ace793c03.js` and library scripts on 2026-10-15 and may change when TI
updates its site.

Focused adapter tests:

```powershell
node --test tools/usb-connect/*.test.mjs
cargo test -p evo-cli usb_bridge
```

See [link compatibility](../../docs/hardware/link-compatibility.md) for the archive
checklist and distinctions between USB device support, USB host/HID modes, and
bootloader recovery.
