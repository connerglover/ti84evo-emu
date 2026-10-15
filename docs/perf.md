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

### 5. Attribution cache

Every fetch and data access asked the SAU and the IDAU for the attribution of its address.
`Cpu` now caches it in a 256-entry direct-mapped table by 32-byte block (the attribution
granule), flushed on every SAU register write, on `set_idau` and on `restore_from`; the `Idau`
trait documents that its answer must be constant per block. `TT`, `decode_at` and the public
`security_of` still compute it afresh.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 28.7 M cycles/s | 32.2 M cycles/s (21 %) |

### 6. Decoded-instruction cache

`Cpu` keeps decoded instructions in a 4096-entry direct-mapped table by halfword address. An
entry is used only while the bus's `code_epoch` is the one it was decoded under, and only for
bytes the bus accepted through `Bus::cache_code` (both new `Bus` methods default to "never
cache", so other buses behave as before). `MemoryMap` accepts code in whole RAM/ROM pages,
remembers which buffer pages hold cached code, and takes a fresh, process-unique epoch on any
write to such a page (through any alias, fast or general path), on `buffer_mut` and `poke`,
and when a device changed a buffer (`DevCtx::buffer_mut`; SPIF reads now use the read-only
`DevCtx::buffer`). The attribution and Execute Never checks still run on every fetch.

The real-OS boot now has a fingerprint: it reaches exactly the same number of awake cycles
(138 149 424 at 390 M cycles) as the baseline, which it would not if any step had changed
what the guest does.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 32.2 M cycles/s | 35.3 M cycles/s (23 %) |
| `boot_to_self_test`, thread CPU time | — | 96.4 M cycles/s, 34.1 M awake cycles/s (15 090 epoch changes) |

### 7. Lean per-step bookkeeping, rare paths out of line

- The recent-steps ring is a fixed array indexed by a step counter (no length checks).
- `sync_bus` is an inlined pair of flag tests; device sync and the PPB-miss drain are separate
  out-of-line functions (`Cpu::has_ppb_misses` guards the drain).
- The breakpoint lookup is skipped while there are no breakpoints.
- Rarely taken paths are kept out of the step functions (`#[cold]` / `#[inline(never)]`):
  `Machine::system_reset`, `Cpu::fault`, `raise_fault`, `raise_sync`, `enter_lockup`,
  exception entry, exception return and function return. Inlined, they bloated the hot loop
  enough to cost a fifth of the throughput (measured by removing the never-taken
  `take_sysreset_request` branch: 37.5 → 45.7 M cycles/s).

A raw `Cpu::step` loop on the same machine (no `Machine` bookkeeping) runs at 59.8 M cycles/s.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 35.3 M cycles/s | 50.8 M cycles/s (33 %) |
| `boot_to_self_test`, thread CPU time | 96.4 M cycles/s | 126.7 M cycles/s (awake cycles unchanged) |

### 8. Cheaper device accesses and event checks

- `MemoryMap` remembers the window its last general-path access hit and tries it before the
  binary search (device polling hits the same window over and over).
- The OR of every device's IRQ lines is kept up to date when a device's lines change, instead
  of being folded over all devices after every device access.
- `Scheduler` keeps the cycle of its earliest event, so `pop_due` answers "nothing due"
  without touching the heap.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 50.8 M cycles/s | 50.4 M cycles/s (no device or event traffic) |
| `boot_to_self_test`, thread CPU time | 126.7 M cycles/s | 132.8 M cycles/s (awake cycles unchanged) |

### 9. Attribution in the instruction cache

An instruction-cache entry also keeps the attribution of its address, so a hit skips the
attribution lookup and the Execute Never check (both passed when the entry was made) and only
re-checks the fetch against the current security state. `flush_attr_cache` (SAU write,
`set_idau`, restore) now empties the instruction cache too. `preemptable_pending`, the slow
half of the pending check, is out of line.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 51.2 M cycles/s | 54.2 M cycles/s (35 %) |
| raw `Cpu::step` loop | 59.8 M cycles/s | 64.6 M cycles/s |

### 10. Look for exceptions after an instruction only when one can have appeared

`Cpu::step` checks for a pending exception before the instruction (anything that pended
between steps) and again after it (anything the instruction pended, taken in the same step).
The second check now runs only if something was pending before the instruction, or if the
instruction did one of the things that can make an exception pending: a guest PPB write
(STIR, ISPR, ISER enabling an IRQ already pending, ICSR, ...), a fault, SVC, an exception
return (it can fault) or a SysTick wrap; these set `Cpu::pend_check`. Masks (PRIMASK,
BASEPRI, FAULTMASK, CPS) only matter for exceptions already pending, which the first
condition covers. Each flag site is pinned by a test that expects the exception entry in the
same step. `Machine::idle` and `wake_if_due` are now out of line too (sleep handling is rare
per instruction; inlined, it cost the busy loop a fifth of its speed).

The difftest bus now lets the core cache decoded code (every bus write, including stores into
the code window, starts a new epoch), so `DIFFTEST_CASES=20000` exercises the instruction
cache against Unicorn: 4 passed.

| | Before | After |
|---|---|---|
| `busy_loop`, thread CPU time | 54.2 M cycles/s | 64.3 M cycles/s (41 %) |
| raw `Cpu::step` loop | 64.6 M cycles/s | 77.1 M cycles/s |
| `boot_to_self_test`, thread CPU time | 132.8 M cycles/s | 156.0 M cycles/s (100 %; awake cycles unchanged) |
