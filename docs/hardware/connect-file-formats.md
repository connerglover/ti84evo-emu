# Native Connect Evo file codecs

The emulator implements these formats independently in `evo-core/src/connect_files.rs`.
The evidence combines the public [TI Connect Evo app](https://connectevo.ti.com/ticevo/en/)
version 7.1.0.792, controlled calls to its unmodified conversion library, and raw resources
returned by the actual 7.1.0.4421 guest over emulated USB. TI scripts and firmware are not
included in this repository.

The examined app script is `main.4fb0b73ace793c03.js` (SHA-256
`b0c60d5ed6a655f142249df836d7c926851d6f1fadab19e137a4afaf63aa7054`). Its
`assets/library/ticonnect.min.js` has SHA-256
`edc9de9231233fe7a042e82ec333e50e4c0da2747441d954bb31e340c8fe660f`.
Static analysis identifies formats and UI options; the tests listed below separately
establish observed conversion behavior. The REA overview was time-limited and incomplete;
it is not evidence of complete semantic coverage of the application.

## Resources and variable files

| Resource | Observed payload |
| --- | --- |
| Attributes / dynamic info | CBOR map; preserve numeric, text and boolean value types |
| Directory | Map with `metaData={type:56,version:1}`, `data` array of entries |
| Directory entry | `tokName` little-endian 16-bit token bytes, `dispName` bytes, `type`, `version`, `size`, `mem` boolean |
| Screen | Map with `width:320`, `height:240`, `bpp:16`, and RGB565 little-endian `data` |
| Variable | Map with `metaData={type,version,name,flags?}` and a type-specific body |

Variable metadata names use little-endian token bytes; the host's token-name string
prints each 16-bit token as four hexadecimal digits. For example, list L1 is `E830` on
the host and bytes `30 E8` in CBOR. Some native names include a zero-token terminator.
Directory display names may contain calculator-font private-use characters, so native
display names are derived from the tokens. The `mem` flag indicates archive storage;
the low metadata `flags` bit carries the corresponding native-file setting.

Do not assume every variable contains `size` and byte `data`. For example, an empty
list uses outer fields `version:2`, `type:0`, `len:0`. The codec preserves these typed
bodies and unknown fields when changing import metadata. Screen and variable CBOR are
separate schemas.

Exported native files append the public client's two-byte checksum. Despite its `CRC`
function name, this is XOR of little-endian words, stored big-endian. For payload length
`n`, the client processes `(n-3)/2 - ((n-3)&1)` words using integer division. This unusual
convention is reproduced and tested against the official converter; it is independent
of the USB/Kermit transport CRC. Native resource transfers use the CBOR without this
disk-file suffix. Import accepts raw CBOR or a verified suffix and rejects other trailing
data. The desktop adapter validates files before sending anything to the guest.
Parsing limits inputs to 16 MiB, nesting to 64 levels, and total CBOR values to 131,072.
A header-only preflight checks those structure limits before allocating the value tree;
the CBOR library still performs semantic validation. Byte strings count as one value,
so image payloads retain their full size allowance.

## Python and images

Python type 15 uses a byte-data envelope. The inner payload starts with little-endian
words `0x113`, total payload length, and name length, followed by the uppercase ASCII
name and NUL. The next word stores source byte length in its low 24 bits and `2` in its
high byte; UTF-8 source and a final NUL follow. Native export extracts the source into
`.py`. Renaming a Python import updates both its metadata tokens and inner name.
Unsupported or ambiguous names are rejected instead of being silently truncated.

Image type 5 has 33,601 data bytes: byte `11`, then 160×105 little-endian RGB565 pixels
in bottom-to-top row order. Components round to 5/6/5 bits. Images occupy archive and
fixed names Image1 through Image9, then Image0. Before encoding, the official browser
fits the source aspect ratio into a centered 160×105 white canvas without smoothing;
the native GUI handles that preparation. The codec also composites alpha over white.

## CSV

The official client sends CSV via an ASCII expression envelope, leaving number parsing
to the guest. Its CBOR map contains `metaData={type:60,version:1}`, outer text `type`
(`List` or `Matrix`), `version:1`, text `name`, and byte `data`. Expressions are
`{1,2,3}` or `[[1,2][3,4]]`; scientific notation uses `@E`. Built-in list names send
`1` through `6`; matrices use `A` through `J`. Custom list theta becomes `theta` in
the text name. This goes through the normal variable PUT resource and its archive and
overwrite query options.

The GUI supports up to six list columns and 999 rows, with trailing empty cells allowed
but gaps rejected. Matrices allow up to 99 rows/columns and 400 cells. Explicit matrix
dimensions crop/pad zeros; zero dimensions infer the input geometry. Numeric CSV has no
quoted text fields. Nonfinite numbers and expression-like input are rejected locally.

## Validation

`connect_files` tests compare synthetic Python conversion and CSV envelopes byte for
byte with the unmodified TI client. A deterministic opaque image's full encoded SHA-256
also matches the official converter, checking row orientation and quantization. Other
tests cover typed list preservation, names, memory flags, malformed/truncated files,
checksums, screenshot colors and directory fields. `EVO_CONNECT_FIXTURES` optionally
checks external raw guest captures without adding their contents to the repository.

These codec tests prove the host representation. Real guest USB send/receive tests in
the GUI and transport layers separately verify that firmware accepts the generated data.
