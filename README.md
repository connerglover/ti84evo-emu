# TI-84 Evo Emulator

A low-level emulator for the TI-84 Evo graphing calculator, written in Rust.

This repository contains no TI code, OS images or artwork. **You must supply your own OS
bundle** (`.84b2`) to run anything beyond the unit tests.

## Workspace

| Crate | What it is |
|---|---|
| `crates/armv8m` | ARMv8-M Mainline interpreter: Thumb-2 + DSP, exceptions, TrustZone |
| `crates/evo-hw` | Memory map and peripheral models (flash, LCD, keypad, timers, UARTs, crypto, …) |
| `crates/evo-image` | `.84b2` / MCUboot bundle parsing and flash layout |
| `crates/evo-core` | The machine: scheduler, save states, provisioning, GDB stub |
| `apps/evo-gui` | Desktop app (egui): calculator view, keypad, debug panels |
| `apps/evo-cli` | Headless runner and reverse-engineering tool |

Hardware notes recovered while building it are in `docs/hardware/`.

## Usage

```sh
# Desktop app (first launch prepares a fresh calculator, then boots to the OS)
cargo run --release -p evo-gui

# Command line
cargo run --release -p evo-cli -- provision <bundle.84b2> --flash-file calc.flash
cargo run --release -p evo-cli -- run <bundle.84b2> --flash-file calc.flash \
    --keys "2 add 3 enter" --screenshot out.png --max-cycles 1500000000
cargo run --release -p evo-cli -- run <bundle.84b2> --gdb 3333   # attach arm-none-eabi-gdb
```

`provision` uses the calculator's own engineering menu to switch a fresh unit from its
factory self-test to the calculator OS, as a new unit leaves the factory.

Click **USB** on the desktop app's left toolbar for calculator files, Python programs,
images, CSV lists/matrices, screenshots, exam exit, and OS updates. These native tools
use the calculator already open in the app; no browser, Node installation, or physical
cable is needed. The Developer tab includes resource inspection and diagnostics.
See the [USB tools guide](docs/usb-tools.md).

The [local USB adapter](tools/usb-connect/README.md) remains available for testing the
official TI communications library against the emulator.


## Testing

```sh
cargo test --workspace
EVO_OS_IMAGE=<bundle.84b2> cargo test --release -p evo-core --test milestones -- --nocapture
```

Tests that need the real OS read its path from `EVO_OS_IMAGE` and skip themselves when it is
unset.

See [GitHub Actions](docs/ci.md) for platform checks, snapshot evidence, and Windows packages.

## License

MIT — see [LICENSE](LICENSE). TI-84 Evo is a trademark of Texas Instruments; this project is not affiliated with or
endorsed by Texas Instruments.

MP4 recording uses OpenH264 compiled from source; Cisco's patent licence for OpenH264 applies
only to Cisco's binaries. Use animated PNG if this matters to you.
