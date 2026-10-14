# Crypto blocks: TRNG (0x5010_A000) and SHA-256 engine (0x5000_2000)

Both are used by the secure image during boot (`boot-flow.md` §2, steps 7-8). Neither raises
an interrupt: the drivers poll.

## TRNG: Arm CryptoCell-312-style TRNG

- **Base.** S 0x5010_A000 (used through the S alias). The NS alias 0x4010_A000 is never
  referenced.
- **IRQ.** 26 `TRNG_Handler` is an *unexpected-IRQ stub* (0x100b24f8 -> 0x100b0e84). The
  driver polls, and the IRQ must never fire.
- **Driver.** CC312 runtime "LLF RND" code. Error codes 0x00F1_0C0x and 0x00F1_0C3x
  (`CC_RND_*`). The base pointer is the global 0x3000_23CC, set at 0x100b0de4.

Firmware functions:

| addr | inferred name | notes |
|---|---|---|
| 0x100b0d60 | `cc_trng_get(base, buf, &len, bits)` | Fills {15, 0, 200, 200, 300, 100} params (SubSamplingRatio1..4 = 200/200/300/100). Calls init then get. Callers: 0x100b35b4 (mbedtls_hardware_poll, error -0x3C) and 0x100d8d34/0x100d8d6a (TRNG partition: 32 B, and 10000 B self-test) |
| 0x100b1524 / 0x100b150c | LLF init / get wrappers | Both call 0x100b298c |
| 0x100b298c | `LLF_RND_GetTrngSource` | Collects 0x210 (528) or 0x90 bytes, 24 B per EHR batch. Health test 0x100b4574 |
| 0x100b5540 | `LLF_RND_StartTrngHW` | See the write sequence below |
| 0x100b157c | `LLF_RND_TurnOffTrng` | +12C = 0; +108 = 0xFFFF_FFFF |
| 0x100b0cf8 | `LLF_RND_WaitRngInterrupt` | Polls +104 **with no timeout** |
| 0x100b4574 | SP 800-90B health tests | Repetition count (cutoff 81 equal bits) and adaptive proportion (1024-bit window). Constant data fails, error 0x00F1_0C36 |

Registers:

| off | name (CC312) | access | firmware use |
|---|---|---|---|
| 0x100 | RNG_IMR | W | 0xFFFF_FFC5 (= ~0x3A) |
| 0x104 | RNG_ISR | R (polled) | bit 0 EHR_VALID -> success. bit 1 AUTOCORR_ERR -> fail (0x00F1_0C02). bit 2 CRNGT_ERR -> count, W1C via ICR=4, fail after 12. bit 3 VN_ERR -> count, ICR=8, fail after 50 |
| 0x108 | RNG_ICR | W1C | 0xFFFF_FFFF at start and stop; 4 or 8 in the poll loop |
| 0x10C | TRNG_CONFIG | W | ring-oscillator index 0..3 (0x100b1564 maps 1/2/4/8 -> 0/1/2/3); only 0 seen |
| 0x114–0x128 | EHR_DATA[0..5] | R | read in order after EHR_VALID: 192 bits per batch |
| 0x12C | RND_SOURCE_ENABLE | W | 0 (stop) / 1 (start) |
| 0x130 | SAMPLE_CNT1 | R/W | writes 0xC8 (200) and **re-writes until the read-back matches** (0x100b55a4, no timeout) |
| 0x138 | TRNG_DEBUG_CONTROL | R/W | writes 0xA. In "continue" mode 0x100b29d2 reads +138 and +130 back and requires 0xA and the sample count |

The LLF code touches only +0x100..+0x138. (+0x1B8 and the CC312 RNG_VERSION at +0x1C0 /
RNG_CLK_ENABLE at +0x1C4 are never accessed.)

`LLF_RND_StartTrngHW` (0x100b5540) write sequence: +130 = count until read back, +12C = 0,
+10C = rosc, +138 = 0xA, +100 = 0xFFFF_FFC5, +108 = 0xFFFF_FFFF, +12C = 1.

Without a TRNG model the boot hangs forever in the SAMPLE_CNT1 read-back loop at 0x100b55a4,
during Mbed TLS init.

### TRNG model

- Plain storage for +100, +10C, +12C, +130, +138, so read-back is exact.
- ICR (+108) is write-1-to-clear on an ISR latch; writing it never sets bits.
- ISR (+104) reads `EHR_VALID (bit 0) = (RND_SOURCE_ENABLE & 1)`, optionally after N cycles
  of "sampling" (the firmware tolerates any delay). Error bits 1–3 are always 0.
- EHR_DATA (+114..+128) returns 32 bits each from a seeded generator (the machine is
  deterministic: a fixed seed, no host RNG); output must not be constant (health test).
  Reading +128 may clear EHR_VALID until the next batch, but the firmware restarts the
  source per 24 B batch (+12C 0 -> 1) anyway.
- No IRQ output (keep IRQ 26 deasserted). Reset value 0 for all registers.
- Expected volume at boot: 22 batches per 528-byte request, a few requests (mbedtls entropy
  plus the TRNG partition self-test of 10000 B, about 420 batches).

## SHA-256 engine

- **Base.** S 0x5000_2000. It is in the platform device-config table at 0x100b7c68:
  {0x5000_1000 SPIF, 0, 0x5020_2B00 (LED pattern generator, `security.md` §6),
  0x5000_2000}. Context pointer global 0x3000_1AFC.
- **IRQ.** 23 `SHA_Handler` is an unexpected-IRQ stub (0x100b1eac). Polled only.
- **Driver.** Mbed TLS `MBEDTLS_SHA256_ALT`, context 76 B: {u32 total[2]; u8 buf[64];
  u8 first @72}. Padding is done in software.

Firmware functions:

| addr | inferred name |
|---|---|
| 0x100b384c | `mbedtls_sha256_init` (memset 76) |
| 0x100b3840 | `mbedtls_sha256_free` |
| 0x100b3854 | `mbedtls_sha256_starts` (total = 0, first = 1; is224 ignored) |
| 0x100b3868 | `mbedtls_sha256_update` |
| 0x100b35e0 | `mbedtls_internal_sha256_process` (one 64 B block) |
| 0x100b36ac | `mbedtls_sha256_finish` (pad, last process, read 32 B) |
| 0x100b304c | wait condition: done when CTRL bit 11 == 0 (100 ms timeout via 0x100b6e7c) |

Block process (excerpt):

```
100b35f8: ldr  r0, [r1, #0]
100b35fc: rev  r0, r0            ; message word, big-endian
100b35fe: str  r0, [r2, #16]     ; +0x10 = W0 ... +0x4C = W15
100b367c: movs r0, #10           ; 0x0A: start, continue
100b3680: cmp.w lr, #0           ; ctx->first?
100b3686: moveq r0, #14          ; 0x0E: start + load IV (first block)
100b3688: str  r0, [r1, #0]      ; CTRL
```

Registers:

| off | name | access | semantics |
|---|---|---|---|
| 0x00 | CTRL/STATUS | W, R polled | W 0x0E = first block (bit 2 = load the SHA-256 IV, bit 1 = start, bit 3 = SHA-256 mode?). W 0x0A = next block. R bit 11 = BUSY (wait until 0) |
| 0x10–0x4C | DATA W[0..15] | W | 16 message words for this block, as standard big-endian SHA words (the firmware does `rev` on little-endian memory) |
| 0x60–0x7C | DIGEST H[0..7] | R | running hash state after the block, standard words. The firmware reads each word 4 times and takes bytes 31:24, 23:16, 15:8, 7:0 into the output in order |

### SHA model

- Keep W[16], H[8] and CTRL.
- On a CTRL write with bit 1 set: if bit 2 is set, set H = the standard SHA-256 initial hash
  value (FIPS 180-4). Then run one SHA-256 compression of W into H, synchronously. BUSY
  (bit 11) reads 0, so the 100 ms wait passes at once.
- Reads of +0x00 return the stored CTRL with bit 11 = 0.
- Reset: everything 0. No IRQ.
- About 40 blocks per secure boot; the NS side uses it through the crypto service.
- Correctness matters: with zero digests the boot still proceeds, but every hash, HMAC/HKDF
  and any signature check (the BSP's "Validating OS...") would be wrong.

## Open questions

- SHA CTRL bit 3: is it SHA-256 vs SHA-224 select? The firmware always sets it. Are there
  other modes (HMAC)? None seen.
- TRNG: is the real IP exactly the CC312 TRNG? Only rosc index 0 is used.
