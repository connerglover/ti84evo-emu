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
screenshots with auto-refresh, variable download/upload, exam exit and OS bundle
transfer. It loads the official communications library into a local host page;
the emulator is not a Windows USB device selectable on TI's public website.
The server downloads the three public TI scripts into memory at startup, or
accepts `--library path/to/client-library` for an existing local copy.
Ctrl-C saves flash and stops the emulator. Do not use the same flash file in two
running emulators.

Connection, attributes, screenshots and directory listing were exercised in
stock Chromium. Helium repeatedly reloaded during the TI library's worker
initialization in testing; use stock Chrome or Edge if that occurs.

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
`--trace` logs bus payloads. `--exit-ptt` invokes the official exam-exit command and
requires a successful response; it is not proof of an active-to-inactive exam
transition unless the calculator was first placed in exam mode.

## Transport details

The official app uses WebSerial, VID `0451`, PID `E018`, at 115200 8N1. Enumeration
and CDC controls run through endpoint zero. Bulk endpoint numbers and packet sizes
come from the guest configuration descriptor. All serial traffic then crosses the
emulated DMA/FIFO/IRQ path. The current firmware exposes CDC OUT1/IN2; later vendor
bulk endpoints are deliberately not mistaken for the serial port.

`evo-cli usb-bridge` provides newline-delimited JSON on stdin/stdout. Operations
are `attach`, `reset`, `setup`, `out`, `in`, `run`, and `quit`; responses are
`{"ok":true,"data":...}` or `{"ok":false,"error":...}`. `Nak` is retryable;
`Stall` and other device errors propagate to the caller. Diagnostics go to stderr.

The official app initializes four globals before loading its library. Their
observed values are reproduced in `client-environment.mjs`; without that host
initialization, the library selects zero-duration read timeouts and substitutes
errors for completed transfers. The library files themselves are not modified.
These host integration details were observed in the public
`main.4fb0b73ace793c03.js` and library scripts on 2026-10-15 and may change when TI
updates its site.

Focused adapter tests:

```powershell
node --test tools/usb-connect/usb-host.test.mjs tools/usb-connect/bridge.test.mjs
cargo test -p evo-cli usb_bridge
```

See [link compatibility](../../docs/hardware/link-compatibility.md) for the archive
checklist and distinctions between USB device support, USB host/HID modes, and
bootloader recovery.
