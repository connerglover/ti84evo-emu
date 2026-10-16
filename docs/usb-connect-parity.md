# Native USB menu and TI Connect Evo parity

Inventory date: 2026-10-16. This document distinguishes official UI features,
library-only diagnostics, proposed emulator UX, and verified behavior. Current
firmware evidence is in [link-compatibility.md](hardware/link-compatibility.md).
An inventory entry is not an end-to-end implementation claim.

## Official features

TI advertises file transfer, screenshots with automatic refresh, OS installation,
and Press-to-Test exit. The application is free and requires no sign-in.
([TI product page](https://education.ti.com/en/products/calculators/graphing-calculators/ti-84-evo/ti-connect-evo))

The following finer-grained inventory comes from the shipped
[TI Connect Evo application](https://connectevo.ti.com/ticevo/en/), specifically
`main.4fb0b73ace793c03.js` downloaded 2026-10-15 and its connectivity library.
These are static source observations unless the compatibility matrix records a
successful guest transfer.

| Area | Observed controls and behavior | Native menu arrangement |
|---|---|---|
| Connection | Connect/disconnect, device identification, busy/progress/cancel, connection-loss handling | Dedicated USB toolbar button and resizable side panel for the current calculator; explicit progress and cancellation |
| Files | Refresh directory; select individual/all files; sort; send, receive, delete; transfer summary and per-file errors | Files tab: name, type, size, RAM/Archive; Send, Save selected, Delete selected |
| Send choices | Calculator name, RAM/Archive, replace-existing checkbox, remove pending files | Show choices before transfer; preserve per-file errors and do not silently overwrite |
| Native formats | `.8ca2`, `.8ci2`, `.8mp2`, `.8xd2`, `.8xg2`, `.8xl2`, `.8xm2`, `.8xn2`, `.8xp2`, `.8xs2`, `.8xt2`, `.8xv2`, `.8xw2`, `.8xy2`, `.8xz2` | Native file chooser and lossless receive/export |
| Python | `.py` conversion, editable calculator name, variable upload | Send Python source, retrieve as source when the variable contains Python |
| Images | `.png` conversion through a 160x105 canvas; ten calculator image slots | Image preview/slot choice; resize with documented alpha handling |
| CSV | Import numeric CSV as lists or matrix; L1-L6 defaults; up to six list columns; matrix UI checks at most 400 cells | CSV target selector and names; reject malformed/non-numeric input before connecting |
| Screen | Capture, download PNG, copy to clipboard, automatic refresh; stop refresh on disconnect | Screen tab with capture/save/copy/auto-refresh |
| OS | Current/latest version, download/install, reinstall, cancellation and install errors; product-specific bundles | Calculator tab: current information, open local bundle, install; clearly report activation/restart status |
| Exam | Read mode state and request exit | Exit exam mode action, then verify reported mode changed |

CSV receive/export is not established as an official UI feature by this inventory;
native list/matrix export remains the safe lossless choice. A full program editor
is likewise not established here. Do not import TI Connect **CE** features into
the Evo parity claim merely because the older product has a similar name.

The default public OS manifest is
[`assets/os/version`](https://connectevo.ti.com/ticevo/en/assets/os/version).
On the inventory date its `ti84Bundle` value was `7.1.0.4421`; product 23 maps to
that key. The corresponding download path is
`assets/os/TI84Evo_Bundle_<version>.84b2` on the same origin. The app's
`version_latest` alternative is selected only by a special window name, so it is
not the default public update channel. Fetch/version checks belong in a bounded
background job; installation still requires the user to select the downloaded
bundle and uses the same firmware USB operation as a local file.

The connectivity library additionally exposes `getLogInfo` (downloads information
and event data, packaged as `info.bin` and `events.bin`) and `sendDebugMessage`.
These are developer operations, not observed everyday UI controls. Arbitrary
resource GET/PUT is an emulator diagnostic extension rather than an official
student feature.

## Protocol observations

Native operations must traverse the emulated USB CDC device and the running
firmware. `Connect::get` sends the Evo request-upload convention and receives a
Kermit resource; `put` uploads a resource. URI parameters identify firmware
resources, not HTTP endpoints.

| Operation | Observed URI / data |
|---|---|
| Attributes | `hh01/sys/attributes` |
| Dynamic status, free memory, exam status | `hh01/inf/res?name=dynamicinfo` |
| Directory | `hh01/inf/res?name=directory&gotohome=1` |
| Screen | `hh01/sys/screen` |
| Diagnostic information/events | `hh01/dbg/Get Event Info` and `hh01/dbg/Get Events` (literal spaces) |
| Debug message | `hh01/dbg/<message>`, one-byte `x` upload; observed in the official WASM URI builder |
| Receive variable | `hh01/xfr/var?name=<percent-encoded token characters>&type=<number>` |
| Send variable | Same path with `&memtarget=0` (RAM) or `1` (Archive), and `&policy=1` for replacement; official library chooses policy from the overwrite option |
| Delete variable | `hh01/del/var?name=<encoded tokens>&type=<number>`, upload one byte with value `0` |
| Explicit exam exit | `hh01/xfr/var?policy=4`; deliberate guest disconnect may precede completion |
| OS package | `hh01/upd/pkg?bundle=1&prodnum=23&version=7.1.0.4421` in the captured TI-84 Evo bundle transfer; package payload, not the ZIP envelope |

Token names are hexadecimal Unicode token code points (for example `E830` for
L1); the URI contains percent-encoded UTF-8 of those characters, not ASCII hex.
Never interpolate user text directly into a query. Values above were captured in
the official-client research logs under `%TEMP%/evo-usb-connect-research` and in
the checked-in native transport tests. Product number/version must come from the
selected bundle. OS completion is not proof of automatic bootloader activation;
see [boot-flow.md](hardware/boot-flow.md).

Native attribute CBOR uses `pkg-version`, `id`, and `product`; dynamic information
uses `ptt`. TI's JavaScript wrapper renames those fields, so its normalized
`os_version_major`/`serial_number`/`p2t` names must not be assumed in raw resources.
Guest Python storage may pad the inner logical file to a four-byte allocation;
source export accepts only the corresponding zero padding. Initial directory
reads can return `BZ` after USB enumeration but before the OS is ready. The GUI
adapter retries explicit busy rejections for read requests (at most 20 requests,
250 ms guest-time waits); it does not automatically replay failed uploads.

Files sent to Archive persist in the flash image across emulator reset/reopen.
RAM files require the running machine or a full save state; a reset from the NV
image does not preserve their RAM contents. Fixed-location variable types retain
their required RAM/Archive target even when generic send options differ.

The opt-in [GUI adapter regression](../apps/evo-gui/tests/connect_real.rs) passed
against OS 7.1.0.4421 in release mode. It exercises connection immediately after
provisioning, directory/information, diagnostics, Python source roundtrip,
partial receive, duplicate multi-send, CSV lists/matrix, PNG image upload,
native list export/reimport, screenshot cancellation/reconnection, archived-file
persistence/reset/deletion, and a full bundle reinstall followed by persisted
flash reset and OS-version verification. This is a same-version reinstall proof;
cross-version upgrades and individual native group/app/module payload classes
remain outside that test's real-firmware coverage.

## CEmu reference and same-machine integration

CEmu's published feature split places screen capture, resizing, speed, file
transfer and save states alongside a separate developer toolkit for execution,
memory and peripheral inspection. That separation is useful here: a student
should be able to send a program without seeing resource URIs or registers.
([CEmu official project](https://ce-programming.github.io/CEmu/))

Use Files, Screen and Calculator/OS as the everyday USB views. Put raw resources,
logs and debug messages in the Developer tab. Existing emulator
debugger controls remain outside this task; CEmu's eZ80-specific internals are
not an Evo feature specification.

The existing provisioning worker supplies the simplest integration pattern:
move the **actual** `Box<dyn EmuBackend>` to one worker, temporarily display a
placeholder, and restore that same backend after completion. The worker clocks
guest CPU/peripherals while performing transfers; it must not reconstruct a
second Machine from flash or save states. A serialized command channel is a
natural extension for repeated captures without concurrent machine access.

The integration must preserve these invariants:

- Do not lose backend ownership if thread creation fails. Keep a recoverable
  handoff until the worker has accepted it, or use a persistent worker.
- Disable machine replacement/reset/state load during a transfer, or cancel and
  join before replacement. Detached workers can otherwise mutate and persist an
  abandoned calculator after a new one opens.
- Restore the backend after success, error, cancellation or a caught panic;
  restore pause/run intent independently of USB guest execution.
- Cancellation is cooperative and must be checked while awaiting packets and
  during large uploads. Detach and allow the guest to observe cable loss on all
  exit paths. A cancelled write may already have changed firmware state.
- Persist the returned backend after mutation, including partial/cancelled
  transfers; never persist an older UI snapshot over its flash.
- Respect debugger stops. Report a stopped guest instead of silently clearing
  breakpoints/watchpoints or claiming successful transfer.
- Auto-refresh queues at most one capture. Stop on error/close and leave file
  operations responsive; it must not create an unbounded background backlog.

## Static evidence limits

The analyzed main JS has SHA-256
`b0c60d5ed6a655f142249df836d7c926851d6f1fadab19e137a4afaf63aa7054`.
REA parent Evidence:
`ev_aedba2a32ac196550950ffaf86646619b0947ec24b698cc3217c6e4a91a15aa4`;
summary Evidence:
`ev_46310215728bc45094290da2bd5ee69430dfb0715aae2ce803a9c55a8a2f2edd`.
It parsed the one 1,218,681-byte JS file without parse failures. Semantic coverage
is partial (100,000-node ceiling; 46,325 unresolved semantic observations).
Targeted source reads supplied the function/control inventory above. Static
analysis neither executes the Angular UI nor proves all controls are reachable;
unobserved features are unknown, not proven absent. Native transport and codec
regressions must supply the implementation proof separately.
