# Emulator performance

The guest CPU runs at 156 MHz, so real time is 156 M emulated cycles per host second.

## Benchmarks

`crates/evo-core/tests/bench.rs`, ignored by default, release only:

```text
cargo test --release -p evo-core --test bench -- --ignored --nocapture --test-threads=1
```

- `busy_loop`: a synthetic program (no TI code), a 7-instruction loop of SRAM load, add,
  store, ALU work and a branch, running from flash in Secure state. No sleep, no device
  traffic: this is the "busy guest code" number. `BENCH_CYCLES` sets the budget (default 1 G).
- `boot_to_self_test`: the user's bundle from `EVO_OS_IMAGE`, from power-on to the Self-Test
  info screen (390 M cycles = 2.5 s of guest time on a fresh unit). Prints `skipping:` without
  `EVO_OS_IMAGE`.

Each prints emulated cycles per host second and, separately, *awake* cycles per second
(`Machine::awake_cycles`: the time the CPU executed, without the time skipped while asleep in
WFI/WFE). Awake cycles/s is the interpreter's real throughput; total cycles/s also credits the
sleep skipping.

Numbers below are from the development machine (Windows 11, Rust 1.98 MSVC, release profile),
measured while other builds were running in parallel, so they carry roughly ±10 % noise.

## Baseline

| Benchmark | Cycles/s | Real time | Awake cycles/s |
|---|---|---|---|
| `busy_loop` (200 M cycles) | 13.4 M | 9 % | 13.4 M |
| `boot_to_self_test` | 27.0 M | 17 % | 9.6 M (6 %) |

## Hot spots

No sampling profiler could run (ETW needs an elevated shell, which this session does not
have), so the hot spots were found by timing the pieces of one step in isolation (5 M
iterations each, release, on the busy-loop machine):

| Piece | ns |
|---|---|
| `MemoryMap::read16` from flash (fetch) | 14.1 |
| `MemoryMap::read32` / `write32` SRAM | 11.2 / 11.5 |
| `Cpu::security_of` (SAU + IDAU) | 5.2 |
| `decode16` / `decode32` | 3.3 / 9.1 |
| `Cpu::step` (whole instruction) | 70.6 |
| `Machine::step` (adds trace ring, `sync_bus`, scheduler) | 84.4 |

So a step spends about a third of its time in the memory map (region binary search, span
classification and the generic read/write path for every access, including fetches), and the
rest in per-step bookkeeping around the instruction: two pending-exception checks, the SysTick
tick, attribution of the fetch and of every data access, and `sync_bus` after every step.
