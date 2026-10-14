# TI-84 Evo memory map

Status: flash placement of OS images 0 and 1 for OS 7.1 (bundle
`7.1.0.4421`), established by `evo-cli inspect --score-bases` and hard-coded
as `evo_image::Layout::EVO_7_1`. Everything else here is provisional and is
refined by the discovery loop (spec §5.3).

## Address conventions

- **Flash offsets are Non-secure (NS) alias addresses.** Flash is mapped at
  `0x0000_0000`; `Layout` fields and `Layout::place` addresses are flash
  offsets (place with `flash_base` 0).
- **The Secure (S) alias of flash offset `o` is `o | 0x1000_0000`.** Both
  aliases name the same bytes.
- The secure image runs from the S alias. Boot sets
  `VTOR_S = 0x1000_0000 | (secure_slot_base + header size of image 0)`
  = `0x1000_0000 | (0x000B_0000 + 0x400)` = **`0x100B_0400`**.
- SRAM, as the images use it (provisional): the secure initial SP is
  `0x3000_07F8` and the secure reset handler sets `MSPLIM = 0x3000_0000`
  (S alias at `0x3000_0000`); image 1's first word is `0x2009_3000` (NS
  alias at `0x2000_0000`).

## Result

| image | role | pkg offset | header | payload | slot incl. TLVs | slot base (flash offset) | runs from | slot end incl. TLVs |
|---|---|---|---|---|---|---|---|---|
| 0 | Secure | `0x0000_0000` | `0x400` | `0x000D_AEB8` | `0x000D_B4F3` | **`0x000B_0000`** | S alias `0x100B_0000` | `0x0018_B4F3` |
| 1 | Non-secure | `0x0010_0000` | `0x200` | `0x0029_3678` | `0x0029_3A99` | **`0x0038_0000`** | NS alias `0x0038_0000` | `0x0061_3A99` |

Images 2–9 (header `0x40`) are not placed yet; their roles and addresses are
open (see below).

The package layout is not the flash layout. Image 1 sits at package offset
`0x10_0000` but flash offset `0x38_0000`.

## Method

`evo-cli inspect <bundle> --score-bases`. The scorer lives in
`apps/evo-cli/src/score.rs`.

For each image, every candidate slot base `B` from `0x0000_0000` to
`0x0080_0000` (exclusive) in `0x1000` steps (2048 candidates) is tried under
both aliases: `B` (NS) and `B | 0x1000_0000` (S). The score for `B` is the
sum over both aliases, and the tables report the split. Offsets `o` are slot
offsets, header included.

1. **+10 per vector-table entry** (the words after the initial SP at
   `B + hdr`, up to the first even non-zero word, at most 495) that is odd,
   lands in `[B, B + hdr + size)` and does not decode to `Undefined` at
   `entry & !1`. This is scored for image 0 only. Image 1's words after its
   reset vector are code, not a vector table.
2. **+1 per word-aligned 32-bit literal** in the payload equal to `B + o`,
   where `o` is the start of a NUL-terminated run of at least 6 printable
   characters in the payload. The `distinct` column (not scored) counts how
   many different strings those hits name.
3. **+1 per BL in the first 64 KiB of payload** (linear sweep) whose target
   is in the payload and decodes as `PUSH`.

The brief's candidate range `[min_handler - size, min_handler]` is replaced
by this wider search. It is meaningless for image 1, and the wide range also
shows that nothing outside it competes.

### Stop rule as applied

The brief requires the winner to beat the runner-up by at least 2x. Two of
the three criteria cannot tell candidates apart, so a ratio of totals would
measure constants rather than evidence:

- Criterion 3 is identical for every base, because BL is PC-relative. It is
  621 for image 0 and 2134 for image 1.
- Criterion 1 is flat at 40 hits for every base whose slot covers all of
  image 0's handlers (177 candidates by address range). A handler address
  that lands on arbitrary code almost always decodes as something.

The rule is therefore applied to criterion 2. The winner (best total) is
accepted only if:

- its string-literal hits are at least twice those of the best other
  candidate, and
- no candidate explains more vector-table entries.

`real_bundle_scores_reproduce_evo_7_1` (env-gated on `EVO_OS_IMAGE`)
enforces this rule, together with a 2x margin on distinct strings, against
`Layout::EVO_7_1`.

## Evidence

### Image 0 (Secure): `0x000B_0000`

48 vector-table words checked, 1312 strings. The 48 words are 40 handler
addresses, 7 zero entries, and one SG-veneer word. The SG word is odd, so it
is counted as an entry; the run ends at the even word after it.

```
  rank  base         total  vectors NS/S  strings NS/S  distinct  BL->PUSH
     1  0x000b0000   1287          0/40        236/30       264       621
     2  0x00086000   1049          0/40          28/0         7       621
     3  0x0008a000   1049          0/40          28/0         1       621
     4  0x0000f000   1043          0/39          26/6         3       621
     5  0x0009a000   1043          0/40          21/1         5       621
image 0: ACCEPT 0x000b0000: string hits 266 vs 118 at runner-up 0x000ba000 (margin 2.3x, need 2x); 264 different strings vs at most 19 elsewhere; vector entries 40; total 1287
```

- Winner: 266 string hits naming 264 different strings through 264
  different literal values. The ratio of totals to rank 2 is 1.23x, which is
  the plateau effect described above.
- The runner-up by string hits is `0x000B_A000` (118). It is not in the
  top 5 because most handlers fall below its slot. All 118 hits are a single
  constant, `0x0018_F2C0`, repeated 118 times. That constant lands on one
  string start at `0xBA000` by coincidence. At the winning base it points to
  flash `0x0018_F2C0`, just past image 0's end (`0x0018_B4F3`); see the open
  questions.
- On distinct strings the next best is `0x000A_0000` with 19 (21 hits), so
  264 vs 19 = 13.9x.
- Alias split: the 30 S-alias hits come from literals in slot+`0x9000`..
  `0x23000`, and the 236 NS-alias hits from literals in slot+`0x5A000`..
  `0xD6000`; see the open questions.

Vector table (slot+`0x400` = `0x100B_0400` S), first 16 words:

| # | name | value | slot offset |
|---|---|---|---|
| 0 | SP | `0x3000_07F8` | |
| 1 | Reset | `0x100B_1ADD` | `+0x1ADC` |
| 2 | NMI | `0x100B_15C1` | `+0x15C0` |
| 3 | HardFault | `0x100B_B685` | `+0xB684` |
| 4 | MemManage | `0x100B_B8D5` | `+0xB8D4` |
| 5 | BusFault | `0x100B_AE15` | `+0xAE14` |
| 6 | UsageFault | `0x100B_D0D5` | `+0xD0D4` |
| 7 | SecureFault | `0x100B_CDE1` | `+0xCDE0` |
| 8–10 | reserved | 0 | |
| 11 | SVCall | `0x100B_1F09` | `+0x1F08` |
| 12 | DebugMonitor | `0x100B_0EC5` | `+0x0EC4` |
| 13 | reserved | 0 | |
| 14 | PendSV | `0x100B_1955` | `+0x1954` |
| 15 | SysTick | `0x100B_1FA9` | `+0x1FA8` |

At this base each of the 6 fault and SVCall handlers starts with
`mrs r0, MSP`.

Reset handler, first 8 instructions:

```
    100b1adc:  cpsid i
    100b1ade:  movw r0, #0
    100b1ae2:  movt r0, #0x3000
    100b1ae6:  msr MSPLIM, r0
    100b1aea:  bl 0x100b1fe0
    100b1aee:  bl 0x100b93c0
    100b1af2:  movs r0, r0
    100b1af4:  push {r7, lr}
```

The handler masks interrupts, sets the main stack limit to the bottom of
secure SRAM, and calls two functions. The second presumably does not return:
the next instruction is a padding halfword and then the next function's
prologue.

### Image 1 (Non-secure): `0x0038_0000`

Criterion 1 is not scored. 10968 strings.

```
  rank  base         total  vectors NS/S  strings NS/S  distinct  BL->PUSH
     1  0x00380000  16504             -       14370/0      6824      2134
     2  0x00378000   3427             -        1293/0       421      2134
     3  0x0030d000   3411             -      159/1118       111      2134
     4  0x002eb000   3291             -       41/1116        36      2134
     5  0x00350000   3285             -        1150/1       334      2134
image 1: ACCEPT 0x00380000: string hits 14370 vs 1293 at runner-up 0x00378000 (margin 11.1x, need 2x); 6824 different strings vs at most 445 elsewhere; vector entries 0; total 16504
```

- Winner: 14370 hits naming 6824 strings, all through the NS alias. The
  margin is 11.1x on hits and 15.3x on distinct strings (next best is
  `0x0037_A000` with 445).
- The runners-up are inflated by single repeated constants:
  - `0x0037_8000` and `0x0035_0000`: the value `0x0056_0D00` appears 444
    times. At the winning base it is an address inside image 1.
  - `0x0030_D000` and `0x002E_B000`: the S-alias value `0x1050_F64C` appears
    1115 times.
  - The rest are near neighbours where a shifted pointer into the dense
    string area lands on another string start.

First words at slot+`0x200`: SP `0x2009_3000`, reset `0x0044_BC91`
(slot+`0xCBC90`). Words 2–15 are not handler addresses (they are even, or
odd but outside the slot), so image 1's real NS vector table is elsewhere.

Reset vector target, first 8 instructions:

```
    0044bc90:  ldr.w r1, [pc, #-8]
    0044bc94:  str r0, [r1]
    0044bc96:  bl 0x380aee
    0044bc9a:  ldr r0, [pc, #20]
    0044bc9c:  bx r0
    0044bc9e:  ldr.w r12, [pc, #-20]
    0044bca2:  ldr.w r12, [r12]
    0044bca6:  ldr.w r12, [r12, r0, lsl #2]
```

This is an entry trampoline: it stores `r0` to a global, calls into early
image-1 code (slot+`0xAEE`), then branches to an address loaded from a
literal. The instructions from `0044bc9e` on belong to the next routine.

## Flash coverage

- `Layout::place` copies header, payload and TLVs. Flash must therefore cover
  offsets up to **`0x0061_3A99`** (header and payload alone end at
  `0x0061_3878`). An 8 MiB flash at offset 0 holds both images.
  `real_bundle_fits_8_mib_flash_with_evo_7_1` checks this.
- Occupied: `0x000B_0000..0x0018_B4F3` (image 0) and
  `0x0038_0000..0x0061_3A99` (image 1).
- Unknown: `0x0000_0000..0x000B_0000` (704 KiB),
  `0x0018_B4F3..0x0038_0000` (about 1.96 MiB) and everything above
  `0x0061_3A99`.

## Open questions

1. **What lives in `0x0..0xB_0000`?** Presumably BL1/BL2 (MCUboot) and
   provisioning or config data, none of which is in the package. The
   emulator replaces BL2 (spec §5.1). Until the OS reads this range, it is
   left as erased flash.
2. **What lives in `0x0018_B4F3..0x0038_0000`?** Image 0 uses the constant
   `0x0018_F2C0` 118 times, which is just past its own end. Candidates are a
   secondary slot, TF-M ITS/PS storage, or a data area. Watch for reads there
   in Task 17.
3. **Where do images 2–9 go, and what are they?** The package does not say
   and this scorer was not run on them. Their headers are `0x40` and none is
   placed by `Layout::place`.
4. **Does image 0 contain a part that runs Non-secure?** Its S-alias string
   references sit in its first `0x23000` bytes. Its NS-alias references sit
   in slot+`0x5A000..0xD6000`, and the strings they name include USB-stack
   identifiers. Either the upper part of image 0 runs NS, or secure code reads that
   data through the NS alias. The SAU/IDAU configuration seen at boot
   (Task 17) should settle this.
5. **Where is image 1's NS vector table?** Secure firmware presumably writes
   `VTOR_NS` before handing over. Record the value in Task 17.
6. **How big is the physical flash?** Image 0 holds strings naming 4M, 8M and
   16M parts (QSPI device identification), so the size is detected at run
   time. 8 MiB is the working assumption; it covers both images.
7. The search assumed 4 KiB-aligned slots (`step 0x1000`) inside the first
   8 MiB. Both winners are 64 KiB-aligned, and nothing in the range competes.

## Reproduce

Set `EVO_OS_IMAGE` to your own bundle, then:

```
cargo run --release -p evo-cli -- inspect "$EVO_OS_IMAGE" --score-bases
cargo test -p evo-cli -p evo-image real_bundle
```
