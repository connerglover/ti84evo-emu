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

## Optimization steps

`busy_loop` from here on is the fastest of three 300 M-cycle runs (the baseline above was a
single 200 M-cycle run). Per-piece timings are from the same isolated-timing harness as the
hot-spot table.

### 1. Page table in `MemoryMap`

Every 64 KiB page that lies wholly inside one RAM or ROM window gets an entry (buffer, page
index, ROM flag) in a flat table of 65 536 entries; an access that falls inside one such page
is a slice copy. Everything else (devices, unmapped space, partly covered pages, ROM writes,
accesses that cross a page end) takes the general path unchanged, so logging and device
behaviour are identical.

| | Before | After |
|---|---|---|
| `read16` flash / `read32` SRAM / `write32` SRAM | 14.1 / 11.2 / 11.5 ns | 1.3 / 1.2 / 1.3 ns |
| `Cpu::step` / `Machine::step` | 70.6 / 84.4 ns | 44.8 / 57.9 ns |
| `busy_loop` | 13.4 M cycles/s | 16.5 M cycles/s (11 %) |

### 2. Release profile: one codegen unit, fat LTO

The interpreter's hot path crosses three crates (`armv8m` step, `evo-hw` memory map,
`evo-core` machine loop); whole-program optimization lets it inline across them. Release builds
take longer to link.

| | Before | After |
|---|---|---|
| `Cpu::step` / `Machine::step` | 44.8 / 57.9 ns | 36.6 / 54.1 ns |
| `busy_loop` | 16.5 M cycles/s | 23.0 M cycles/s (15 %) |

### 3. `sync_bus` only after device activity

`MemoryMap` sets a flag whenever it calls into a device (access, event, reset, host call) or
loads device states; IRQ lines, pulses and event requests can only change then.
`Machine::sync_bus` skips the IRQ-line fold over every device, the pulse and the event queues
unless the flag is set (the PPB-miss drain stays unconditional).

| | Before | After |
|---|---|---|
| `Machine::step` | 54.1 ns | 44.8 ns |
| `busy_loop` | 23.0 M cycles/s | 22.9 M cycles/s (within noise) |

### 4. Skip the pending-exception scan when nothing is pending

`Cpu::step` looks for a pending exception before and after every instruction; the quick
rejection scanned all 15 NVIC pending words each time. `Nvic` now remembers that no IRQ is both
pending and enabled until `pending` or `enabled` changes (all changes go through its methods),
so the common case is two flag tests. The hint is not state: not serialized, ignored by
equality, clear after a restore.

From this step on, `busy_loop` is also timed by thread CPU time in an out-of-tree sampling
harness (same program, same `Machine::run`), which other load on the host does not disturb.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 26.1 M cycles/s | 28.7 M cycles/s (18 %) |
