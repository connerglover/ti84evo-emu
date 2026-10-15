# TI-84 Evo Emulator — Plan 1: Foundation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A verified Cortex-M33 interpreter, OS-bundle loader, machine skeleton with unmapped-access logging, a headless CLI and a GDB stub — ending with the real TI-84 Evo OS executing and producing the first peripheral-discovery log.

**Architecture:** Cargo workspace per spec §3. `armv8m` is a hardware-agnostic ARMv8-M Mainline interpreter that owns the System Control Space (NVIC/SCB/SysTick/SAU/MPU at `0xE000_0000`) and talks to everything else through a `Bus` trait. `evo-hw` provides the memory map with a logging fallback, `evo-image` parses `.84b2`/MCUboot, `evo-core` ties them together with a cycle scheduler, `evo-cli` drives it.

**Tech Stack:** Rust 2024 edition (toolchain 1.98 installed), `thiserror`, `zip`, `clap`, `serde`, `log` + `env_logger`; dev: `unicorn-engine` 2.x (feature-gated differential testing), `proptest`. Arm GNU Toolchain (`arm-none-eabi-as/ld/objcopy`) for test programs.

**Spec:** `docs/superpowers/specs/2026-10-13-ti84evo-emulator-design.md`

**Follow-on plans (not in this plan):** Plan 2 — M1/M2 peripherals (UART, clocks, MPC/PPC, SHA, TRNG, QSPI), flash persistence, save states, CLI `--uart/--screenshot/--keys/--load-state`, written from this plan's discovery log. Plan 3 — `evo-gui` per spec §6.4.

## Global Constraints

- No TI code, images, artwork or dumps are committed. Tests needing the OS read the path from env `EVO_OS_IMAGE` and are `#[ignore]`-free but early-return with a printed skip when it is unset.
- The path of the user's RE folder never appears in committed code; only in local env vars.
- Library crates never panic on guest behavior or user input; host errors are `thiserror` enums. Guest misbehavior becomes an architectural fault.
- Unmapped accesses never panic: reads return 0, writes are dropped, both are logged.
- Unimplemented instructions raise UsageFault UNDEFINSTR (or NOCP for coprocessor space) and are logged with PC — never silently skipped.
- Every `Instr` variant implements disassembly (`Display`) in GNU/UAL syntax; every decode test also asserts the disassembly string.
- Interpreter only; cycle costs approximate (1 per instruction, +1 per taken branch, +N for LDM/STM/PUSH/POP of N regs, 2–12 for divide → use 4).
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass at every commit.
- CPU, register, SCS and memory-map state types derive `serde::Serialize`/`Deserialize` now (save states, spec §6.5, are implemented in Plan 2; deriving later means touching every type).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Unaligned and cross-region accesses** — an LDR spanning two regions or hitting an unmapped page must fault or log per-byte correctly, not panic or read garbage. Test owned by Task 12.
2. **A truncated or corrupt `.84b2`** (bad ZIP, missing `.84pk2`, MCUboot header with size past EOF) must return an `ImageError`, not panic. Test owned by Task 13.
3. **Runaway guest** (infinite loop, lockup, fault-in-fault) — the CLI must stop at `--max-cycles` and report the state (PC, lockup flag) with non-zero exit, not hang. Test owned by Task 15.
4. **PC/branch to an even address or into non-executable/unmapped space** — must produce the correct INVSTATE UsageFault / BusFault (IBUSERR), not interpret data. Test owned by Task 7.
5. **GDB client disconnect mid-session or malformed packet** — stub must NAK/ignore and keep the machine intact. Test owned by Task 16.

---

## File Structure

```
Cargo.toml                         workspace
.github/workflows/ci.yml
crates/armv8m/src/
  lib.rs        re-exports
  bus.rs        Bus trait, Access, BusError
  bits.rs       ARM ARM pseudocode helpers (AddWithCarry, Shift_C, ThumbExpandImm_C, Sat...)
  regs.rs       Registers, Xpsr, banked special registers, SecState
  instr.rs      Instr enum + operand types
  disasm.rs     Display for Instr
  decode/mod.rs, decode/t16.rs, decode/t32.rs
  exec/mod.rs, exec/alu.rs, exec/mem.rs, exec/branch.rs, exec/dsp.rs, exec/sys.rs
  scs/mod.rs (PPB dispatch), scs/nvic.rs, scs/scb.rs, scs/systick.rs, scs/sau.rs, scs/mpu.rs
  exception.rs  entry/return, stacking, fault escalation
  cpu.rs        Cpu, step(), StepResult
crates/armv8m/tests/
  common/mod.rs (FlatBus test bus, asm-less harness: load halfwords, run N steps)
  t16_*.rs, t32_*.rs, dsp.rs, exceptions.rs, trustzone.rs, difftest.rs, programs.rs
tools/testprogs/   *.S sources, link.ld, build.ps1, build.sh, out/*.bin (committed build outputs of our own code)
crates/evo-image/src/   lib.rs, bundle.rs, mcuboot.rs, layout.rs
crates/evo-hw/src/      lib.rs, memmap.rs, region.rs, unmapped.rs
crates/evo-core/src/    lib.rs, machine.rs, scheduler.rs, gdb.rs
apps/evo-cli/src/       main.rs, inspect.rs
docs/hardware/          memory-map.md, discovery-log.md
```

---

### Task 1: Workspace, CI, bus and register model

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml` (channel `1.98`), `.gitignore` (`/target`, `*.flash`, `*.state`), `.github/workflows/ci.yml`, `crates/armv8m/{Cargo.toml,src/lib.rs,src/bus.rs,src/regs.rs}`, `README.md` (one paragraph + "you must supply your own OS bundle").
- Test: `crates/armv8m/src/regs.rs` (unit tests)

**Interfaces — Produces:**
```rust
pub enum AccessKind { Fetch, Read, Write }
pub struct Access { pub kind: AccessKind, pub secure: bool, pub privileged: bool }
pub enum BusError { Decode, SecureViolation }
pub trait Bus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError>;
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError>;
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError>;
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError>;
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError>;
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError>;
}
#[derive(Clone, Copy, PartialEq, Eq)] pub enum Sec { Secure, NonSecure }
pub struct Banked<T> { pub s: T, pub ns: T }   // get(sec)/get_mut(sec)
pub struct Xpsr { pub n: bool, pub z: bool, pub c: bool, pub v: bool, pub q: bool,
                  pub ge: u8, pub it: u8, pub t: bool, pub exception: u16 }
pub struct Registers {
    pub r: [u32; 13], pub lr: u32, pub pc: u32, pub xpsr: Xpsr,
    pub msp: Banked<u32>, pub psp: Banked<u32>, pub msplim: Banked<u32>, pub psplim: Banked<u32>,
    pub control: Banked<u32>, pub primask: Banked<bool>, pub basepri: Banked<u8>, pub faultmask: Banked<bool>,
    pub sec: Sec,
}
impl Registers { pub fn sp(&self) -> u32; pub fn set_sp(&mut self, v: u32); pub fn handler_mode(&self) -> bool; }
impl Xpsr { pub fn to_u32(&self) -> u32; pub fn from_u32(v: u32) -> Self; }
```
`sp()` selects MSP vs PSP from `handler_mode()` and `control.get(sec)` bit 1 (SPSEL).

- [ ] **Step 1:** Write tests `xpsr_roundtrip` (`from_u32(0xF80F_FC00 | 0x0100_0000 | 0x2B).to_u32()` equals input, incl. IT bits split across [26:25] and [15:10]) and `sp_selects_banked_psp` (thread mode, NS, `control.ns = 0b10` → `sp()` returns `psp.ns`; `set_sp` writes `psp.ns`).
- [ ] **Step 2:** `cargo test -p armv8m` → FAIL (types missing).
- [ ] **Step 3:** Implement the types above. CI workflow: jobs `fmt`, `clippy`, `test` on `windows-latest` and `ubuntu-latest`; job `difftest` on ubuntu runs `cargo test -p armv8m --features difftest`.
- [ ] **Step 4:** `cargo test -p armv8m` → PASS; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] **Step 5:** Commit `feat(armv8m): workspace, bus trait and register model`.

### Task 2: Pseudocode helpers, CPU skeleton, 16-bit data processing

**Files:**
- Create: `src/bits.rs`, `src/instr.rs`, `src/disasm.rs`, `src/decode/{mod,t16}.rs`, `src/exec/{mod,alu}.rs`, `src/cpu.rs`, `tests/common/mod.rs`, `tests/t16_alu.rs`

**Interfaces — Produces:**
```rust
// bits.rs (names follow the ARM ARM)
pub fn add_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool); // (result, carry, overflow)
pub enum SRType { Lsl, Lsr, Asr, Ror, Rrx }
pub fn shift_c(v: u32, t: SRType, amount: u32, carry_in: bool) -> (u32, bool);
pub fn thumb_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool);
pub fn decode_imm_shift(ty: u32, imm5: u32) -> (SRType, u32);
// cpu.rs
pub enum StepResult { Executed { cycles: u32 }, Sleep, Bkpt(u8), Lockup }
pub struct Cpu { pub regs: Registers, /* scs, pending exception state … */ }
impl Cpu {
    pub fn new() -> Self;                                   // reset state, secure, thread, privileged
    pub fn reset(&mut self, bus: &mut impl Bus);           // loads MSP_S/PC from VTOR_S (default 0)
    pub fn step(&mut self, bus: &mut impl Bus) -> StepResult;
    pub fn decode_at(&self, bus: &mut impl Bus, addr: u32) -> Option<(Instr, u8)>; // instr, length 2|4
}
// decode
pub fn decode16(hw: u16) -> Instr;
pub fn decode32(hw1: u16, hw2: u16) -> Instr;               // Task 4/5
// instr.rs: Instr enum with Instr::Undefined { raw: u32 } as the catch-all
// tests/common
pub struct FlatBus { pub mem: Vec<u8>, pub base: u32 }       // read/write within [base, base+len), else Decode
pub fn cpu_with_code(code: &[u16]) -> (Cpu, FlatBus);        // code at 0x0000_0100, SP=0x2000_1000 mapped, PC=0x100, NS-agnostic secure state
pub fn run(cpu: &mut Cpu, bus: &mut FlatBus, n: usize);
```
`FlatBus` maps code at `0x0000_0000..0x0001_0000` and RAM at `0x2000_0000..0x2001_0000`.

ITSTATE handling lives in `step()`: conditional execution check before execute, `ITAdvance` after; flags are not set by 16-bit "S" forms inside an IT block (ARM ARM `InITBlock()`).

- [ ] **Step 1:** Tests in `tests/t16_alu.rs`, each with the assembly as a comment and asserting both result and disassembly:
  - `movs_imm` `0x2005` → r0=5, Z=0,N=0; disasm `movs r0, #5`.
  - `adds_carry_overflow` (r0=0x7FFF_FFFF, `adds r0, #1` `0x3001`) → r0=0x8000_0000, N=1,V=1,C=0.
  - `subs_borrow` (r0=0, `subs r0,#1` `0x3801`) → 0xFFFF_FFFF, C=0, N=1.
  - `lsls_carry_out` (r1=0x8000_0001, `lsls r0,r1,#1` `0x0048`) → r0=2, C=1.
  - `lsrs_imm_zero_means_32` (`lsrs r0,r1,#32` encoded `0x0808`, r1=0x8000_0000) → r0=0, C=1.
  - `asrs_reg_shift_ge_32` (r0=0x8000_0000, r1=40, `asrs r0,r1` `0x4108`) → 0xFFFF_FFFF, C=1.
  - `muls`, `mvns`, `rsbs` (`negs`), `cmp_hi_regs` (`cmp r8, r9`), `add_sp_imm`, `adr`, `mov_hi_reg_to_pc_interworks` (`mov pc, r1` with r1 even → INVSTATE pending — assert via `StepResult` + CFSR in Task 7; here only assert PC = r1 & !1 and T bit from r1[0]).
  - `it_block_suppresses_flags` (`it eq; addeq r0,#1` → flags unchanged, executes only when Z).
  - `undefined_16bit` (`0xDE00` udf) → `Instr::Undefined`, disasm `udf #0`.
- [ ] **Step 2:** `cargo test -p armv8m --test t16_alu` → FAIL.
- [ ] **Step 3:** Implement helpers, `decode16` for the data-processing groups (shift/add/sub/mov/cmp immediate, data-processing register, special data/branch-exchange, ADR, ADD SP), executor and disasm for those variants.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `feat(armv8m): 16-bit data processing, IT blocks`.

### Task 3: 16-bit load/store, branches and misc

**Files:** Modify `decode/t16.rs`, `exec/{mem,branch,sys}.rs` (create), `disasm.rs`. Test: `tests/t16_mem_branch.rs`.

**Interfaces — Consumes:** Task 2. **Produces:** `Instr` variants for LDR/STR (imm, reg, SP-relative, literal), LDRB/H/SB/SH, PUSH/POP, LDM/STM, B (T1 cond, T2), BX/BLX, CBZ/CBNZ, SVC, BKPT, CPS, NOP/YIELD/WFE/WFI/SEV, SXTB/SXTH/UXTB/UXTH, REV/REV16/REVSH, IT.

- [ ] **Step 1:** Tests: `ldr_literal_aligns_pc` (`ldr r0,[pc,#4]` at 0x102 reads from 0x108); `push_pop_lr_pc` (push {r4,lr}; pop {r4,pc} returns, SP restored, cycle cost = 1+2 each); `ldm_writeback_base_in_list` (`ldmia r0!, {r0,r1}` → no writeback); `b_cond_not_taken`/`b_cond_taken` (+1 cycle); `cbz_forward`; `bkpt_returns_imm` (`0xBEAB` → `StepResult::Bkpt(0xAB)`, PC not advanced); `wfi_returns_sleep`; `rev16`; `sxth`; each asserts disasm (e.g. `pop {r4, pc}`, `cbz r0, 0x10a`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): 16-bit loads/stores, branches, misc`.

### Task 4: 32-bit data processing, multiply and divide

**Files:** Create `decode/t32.rs`; modify `exec/alu.rs`, `disasm.rs`, `cpu.rs` (32-bit fetch: halfword with top 5 bits `0b11101|0b11110|0b11111`). Test: `tests/t32_alu.rs`.

**Produces:** Instr variants for: data-processing (modified immediate) AND/TST/BIC/ORR/MOV/ORN/MVN/EOR/TEQ/ADD/CMN/ADC/SBC/SUB/CMP/RSB; plain binary immediate ADDW/SUBW/MOVW/MOVT/SSAT/USAT/SBFX/UBFX/BFI/BFC; shifted register forms incl. PKHBT/PKHTB; register-controlled shifts; SXTAB/UXTAH etc. extends with rotation; CLZ/RBIT/REV*/SEL; MUL/MLA/MLS; SMULL/UMULL/SMLAL/UMLAL/UMAAL; SDIV/UDIV (div-by-zero returns 0 when CCR.DIV_0_TRP=0, else UsageFault DIVBYZERO — Task 7 adds the trap).

- [ ] **Step 1:** Tests: `thumb_expand_imm_patterns` (imm12 `0x1AB`→0x00AB00AB, `0x2AB`→0xAB00AB00, `0x3AB`→0xABABABAB, `0x4FF` rotated → 0x7F800000 and carry = bit31); `movw_movt_pair`; `bfi`; `ubfx`; `sdiv_negative` (-7/2 = -3); `udiv_by_zero_returns_zero`; `umull`; `smlal_accumulates_64bit`; `orrs_shifted_reg_ror`; `adds_w_vs_add_w_flags` (`add.w` doesn't set flags inside or outside IT); `ssat_sets_q`; disasm for each (e.g. `movt r0, #0x1234`, `ubfx r0, r1, #4, #8`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): 32-bit data processing, multiply, divide`.

### Task 5: 32-bit load/store, branches, system instructions

**Files:** Modify `decode/t32.rs`, `exec/{mem,branch,sys}.rs`, `disasm.rs`. Test: `tests/t32_mem_branch.rs`.

**Produces:** LDR/STR{,B,H,SB,SH} all addressing modes (imm12, imm8 pre/post/offset with writeback, register LSL #0–3, literal, unprivileged LDRT/STRT), LDRD/STRD, LDM/STM (IA/DB) incl. POP.W/PUSH.W, LDREX/STREX{,B,H} with a local exclusive monitor (`Cpu::exclusive: Option<u32>` cleared on exception entry/return and CLREX), LDA/STL/LDAEX/STLEX (v8-M acquire/release), TBB/TBH, PLD/PLI (no-op), B (T3 cond, T4), BL, MSR/MRS (APSR, IPSR, EPSR, IAPSR, MSP/PSP, MSPLIM/PSPLIM, PRIMASK, BASEPRI, BASEPRI_MAX, FAULTMASK, CONTROL and the `_NS` variants SYSm 0x88–0x94 which are only accessible from Secure), DMB/DSB/ISB (no-op), CLREX, UDF.W. Coprocessor space (`hw1[15:9] = 0b1110110/0b1110111/0b1111110/0b1111111` with coproc 0–15) decodes to `Instr::Coprocessor { raw }`.

- [ ] **Step 1:** Tests: `ldr_post_index_writeback`; `ldrd_strd`; `strex_fails_without_monitor` (returns 1, memory unchanged); `strex_succeeds_after_ldrex`; `tbh_jump`; `bl_sets_lr_thumb_bit`; `b_t4_long_negative_offset` (J1/J2 sign logic: target PC-0x20_0000); `mrs_ipsr_zero_in_thread`; `msr_ns_from_nonsecure_is_ignored`; `msr_basepri_max_only_lowers`; `vldr_decodes_as_coprocessor`. Disasm e.g. `ldr.w r0, [r1], #4`, `tbh [pc, r0, lsl #1]`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): 32-bit loads/stores, branches, system regs`.

### Task 6: DSP extension

**Files:** Create `exec/dsp.rs`; modify `decode/t32.rs`, `disasm.rs`. Test: `tests/dsp.rs`.

**Produces:** Parallel add/sub (S/Q/SH/U/UQ/UH × ADD16/ASX/SAX/SUB16/ADD8/SUB8) setting GE; QADD/QSUB/QDADD/QDSUB; SMULxy/SMLAxy/SMULWy/SMLAWy/SMLALxy; SMUAD/SMUSD/SMLAD/SMLSD/SMLALD/SMLSLD (+X); SMMUL/SMMLA/SMMLS (+R); USAD8/USADA8; SSAT16/USAT16; SXTB16/UXTB16(+A).

- [ ] **Step 1:** Tests: `sadd16_sets_ge`; `uqsub8_saturates`; `qadd_sets_q_on_overflow`; `smlad_overflow_sets_q`; `smmulr_rounds`; `sel_uses_ge`; `usada8`; disasm strings.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): DSP extension`.

### Task 7: System Control Space and exceptions (single security state)

**Files:** Create `scs/{mod,nvic,scb,systick,mpu}.rs`, `exception.rs`; modify `cpu.rs`. Test: `tests/exceptions.rs`.

**Interfaces — Produces:**
```rust
impl Cpu {
    pub fn set_irq_level(&mut self, irq: u16, asserted: bool);  // level-sensitive line from peripherals
    pub fn pulse_irq(&mut self, irq: u16);                      // edge: sets pending
    pub fn tick_systick(&mut self, cycles: u32);                // called by step with executed cycles
    pub fn locked_up(&self) -> bool;
}
```
CPU intercepts all accesses to `0xE000_0000..=0xE00F_FFFF` (PPB) before the `Bus`; the NS alias `0xE002_0000..0xE002_FFFF` is handled in Task 8. Implemented: SCB (CPUID `0x410F_D213`, ICSR, VTOR, AIRCR with VECTKEY `0x05FA`, SCR, CCR, SHPR1-3, SHCSR, CFSR, HFSR, MMFAR, BFAR, CPACR stored, CPPWR), NVIC (480 IRQs: ISER/ICER/ISPR/ICPR/IABR/IPR), SysTick (CSR/RVR/CVR/CALIB; CLKSOURCE ignored), MPU registers stored but not enforced, ICTR, ACTLR, STIR. Unimplemented PPB register: read 0 + `log::warn!`.

Exception model: entry (8-word stack frame, 8-byte alignment with xPSR[9], EXC_RETURN `0xFFFF_FFxx`), return via BX/POP/LDR to EXC_RETURN in handler mode, priority with BASEPRI/PRIMASK/FAULTMASK, tail-chaining, SVCall, PendSV, SysTick, NMI; fault escalation to HardFault when the fault's handler is disabled or of insufficient priority; LOCKUP (`StepResult::Lockup`) on fault in HardFault. Faults with CFSR bits: UNDEFINSTR, INVSTATE (branch to even address / T=0), INVPC, NOCP (coprocessor when CPACR CP10/11 = 0), UNALIGNED (when CCR.UNALIGN_TRP, and always for LDM/STM/LDRD/STREX), DIVBYZERO (CCR.DIV_0_TRP), IBUSERR (fetch BusError), PRECISERR + BFAR (data BusError), STKERR/UNSTKERR.

- [ ] **Step 1:** Tests: `svc_enters_handler_and_returns` (VTOR table in FlatBus; handler does `bx lr`; r0–r3,r12,lr,pc,xpsr preserved, back in thread mode); `systick_fires_after_reload`; `irq_preempts_by_priority` (IRQ 5 prio 0x40 preempts IRQ 3 prio 0x80 handler); `tail_chain` (two pending → no unstack/restack between, 1 fewer frame write observed by counting bus writes); `primask_masks_irq`; `undefined_escalates_to_hardfault` (UsageFault disabled → HardFault, HFSR.FORCED, CFSR.UNDEFINSTR); `branch_to_even_address_invstate` (Review Focus 4); `fetch_from_unmapped_ibuserr` (Review Focus 4); `vldr_raises_nocp`; `fault_in_hardfault_locks_up`; `stack_alignment_bit9`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): SCS, NVIC, SysTick, exception model`.

### Task 8: TrustZone

**Files:** Create `scs/sau.rs`; modify `exception.rs`, `cpu.rs`, `exec/{branch,sys}.rs`, `scs/mod.rs`. Test: `tests/trustzone.rs`.

**Interfaces — Produces:**
```rust
pub enum Attr { Secure, NonSecureCallable, NonSecure, Exempt }
pub trait Idau { fn attr(&self, addr: u32) -> Option<(Attr, Option<u8>)>; } // region number
impl Cpu {
    pub fn set_idau(&mut self, idau: Box<dyn Idau>);   // default: address bit 28 set → Secure (Arm convention), else NonSecure
    pub fn security_of(&self, addr: u32) -> Attr;     // SAU ∩ IDAU, most secure wins; 0xE000_0000.. is Exempt
}
```
SAU (CTRL, TYPE=8 regions, RNR, RBAR, RLAR, SFSR, SFAR). Instruction fetch security = attribution of the PC. SG at an NSC address switches NS→S (LR bit 0 cleared per spec), branching NS→S anywhere else → SecureFault INVEP. BXNS/BLXNS (BLXNS pushes return addr + partial xPSR to secure stack, LR = `0xFEFF_FFFF` FNC_RETURN). TT/TTT/TTA/TTAT result word. NS alias of SCS at `0xE002_xxxx` (accessible from Secure only). Banked SysTick/VTOR/SHPR/CCR/SHCSR/AIRCR.PRIS/BFHFNMINS. NVIC ITNS routes IRQs to NS. Exception entry from S to NS handler pushes the additional state context (integrity signature `0xFEFA_125B`, r4–r11) and clears registers; EXC_RETURN bits ES/S/DCRS/SPSEL honored. SecureFault on: invalid entry point, invalid integrity signature, invalid exception return, NS access to S memory (`AUVIOL`, with SFAR).

- [ ] **Step 1:** Tests: `sg_entry_from_ns`; `ns_branch_to_secure_non_nsc_securefault`; `blxns_and_fnc_return`; `tt_reports_region`; `ns_irq_from_secure_thread_clears_regs` (r4–r11 zeroed in NS handler, restored after); `ns_read_of_secure_ram_faults` (SFAR set); `ns_alias_scs_reads_ns_vtor`; `sau_disabled_allns_bit`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(armv8m): TrustZone (SAU, SG/BXNS, banked state, secure stacking)`.

### Task 9: Differential testing against Unicorn

**Files:** Modify `crates/armv8m/Cargo.toml` (feature `difftest = ["dep:unicorn-engine"]`, dev-dep `proptest`). Create `tests/difftest.rs` (`#![cfg(feature = "difftest")]`).

Generator: random sequences of 1–8 instructions drawn from an encoding-template table covering Tasks 2–6 instruction classes (excluding branches, exceptions, coprocessor, system and TrustZone instructions), random r0–r12 and flags, memory ops confined to a 4 KiB RAM window by masking the base register to `0x2000_0000 | (rand & 0xFF0)`. Run in Unicorn (`Arch::ARM`, `Mode::THUMB | Mode::MCLASS`, CPU model `cortex-m33`) and in `Cpu` on `FlatBus`; compare r0–r12, LR, NZCVQ, GE and the RAM window after each sequence. On mismatch, print the hex encoding, disasm and both states.

- [ ] **Step 1:** Write `difftest_alu_t16`, `difftest_t32`, `difftest_dsp`, `difftest_mem` as proptest cases (256 cases each locally; env `DIFFTEST_CASES` overrides).
- [ ] **Step 2:** Run `cargo test -p armv8m --features difftest` (Ubuntu CI or WSL; Windows needs cmake — VS 2022's bundled cmake on PATH). Expected: fix every divergence where Unicorn is right; where Unicorn is wrong (document in `tests/difftest.rs` `KNOWN_UNICORN_DIVERGENCES` with ARM ARM citation) exclude the template.
- [ ] **Step 3:** All four PASS with 10 000 cases (`DIFFTEST_CASES=10000`).
- [ ] **Step 4:** Commit `test(armv8m): differential testing vs Unicorn`.

### Task 10: Assembly test programs (exceptions + TrustZone end-to-end)

Prerequisite (one-time; user has pre-approved installing tools): `winget install Arm.GnuArmEmbeddedToolchain` (provides `arm-none-eabi-*`). CI installs it with `carlosperate/arm-none-eabi-gcc-action@v1`.

**Files:** Create `tools/testprogs/{link.ld,semihost.S,exc_basic.S,tz_roundtrip.S,systick.S,build.sh,build.ps1}`, `tools/testprogs/out/*.bin` (committed), `crates/armv8m/tests/programs.rs`.

Semihosting protocol in the runner: on `StepResult::Bkpt(0xAB)`, r0 = op: `0x18` SYS_EXIT (r1 = `0x20026` → pass, else fail with code), `0x03` SYS_WRITEC, `0x04` SYS_WRITE0; then PC += 2. Program image loaded at `0x1000_0000` (secure) with NS code at `0x0020_0000`; RAM `0x3000_0000` (S) / `0x2000_0000` (NS) — a test-only bus in `programs.rs`.

- [ ] **Step 1:** Write `programs.rs` with one test per `.bin` asserting exit `pass` and expected console output (e.g. `tz_roundtrip` prints `S>NS>S ok`).
- [ ] **Step 2:** Write the `.S` programs: `exc_basic` (SVC, PendSV, tail-chain, fault handler reading CFSR), `tz_roundtrip` (secure configures SAU, BLXNS into NS, NS calls back through an SG veneer, NS IRQ taken from secure thread), `systick` (both banks fire). Build with `build.ps1`.
- [ ] **Step 3:** `cargo test -p armv8m --test programs` → PASS (fix core bugs found).
- [ ] **Step 4:** Commit `test(armv8m): assembly test programs with semihosting`.

### Task 11: Scheduler

**Files:** Create `crates/evo-core/{Cargo.toml,src/lib.rs,src/scheduler.rs}`.

**Interfaces — Produces:**
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum EventKind { Peripheral(u16) }       // peripherals get stable ids in Plan 2
pub struct Scheduler { pub now: u64, /* min-heap */ }
impl Scheduler {
    pub fn schedule(&mut self, at: u64, kind: EventKind);
    pub fn cancel(&mut self, kind: EventKind);
    pub fn next_deadline(&self) -> Option<u64>;
    pub fn pop_due(&mut self) -> Option<EventKind>;   // event with at <= now
}
```
- [ ] **Step 1:** Tests: `events_fire_in_order`, `same_time_fifo`, `cancel_removes`, `reschedule_replaces` (scheduling an existing kind replaces it).
- [ ] **Step 2:** FAIL. **Step 3:** Implement with `BinaryHeap<Reverse<(u64, u64 seq, EventKind)>>` + lazy cancellation via generation map. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(evo-core): cycle scheduler`.

### Task 12: Memory map with unmapped-access log (`evo-hw`)

**Files:** Create `crates/evo-hw/{Cargo.toml,src/lib.rs,src/memmap.rs,src/region.rs,src/unmapped.rs}`.

**Interfaces — Produces:**
```rust
pub struct UnmappedAccess { pub addr: u32, pub width: u8, pub write: Option<u32>, pub pc: u32, pub secure: bool }
pub struct UnmappedLog { /* dedup by (addr,width,write.is_some(),pc); keeps count */ }
impl UnmappedLog { pub fn entries(&self) -> impl Iterator<Item = (&UnmappedAccess, u64)>; pub fn to_markdown(&self) -> String; }
pub trait Device { fn read(&mut self, off: u32, width: u8) -> u32; fn write(&mut self, off: u32, width: u8, v: u32); }
pub struct MemoryMap { pub log: UnmappedLog, pub pc_hint: u32, /* regions */ }
impl MemoryMap {
    pub fn new() -> Self;
    pub fn add_ram(&mut self, name: &'static str, size: u32, aliases: &[u32]);   // same backing at each alias
    pub fn add_rom(&mut self, name: &'static str, data: Vec<u8>, aliases: &[u32]); // writes → log + dropped (QSPI programming is Plan 2)
    pub fn add_device(&mut self, name: &'static str, base: u32, size: u32, dev: Box<dyn Device>);
    pub fn region_bytes(&self, name: &str) -> Option<&[u8]>;
}
impl armv8m::Bus for MemoryMap { … }   // unmapped → Ok(0)/Ok(()) + log; never BusError in Plan 1
```
Accesses that straddle a region boundary are split into byte accesses (each logged individually if unmapped). `Machine` sets `pc_hint` before each step.

- [ ] **Step 1:** Tests: `ram_alias_shares_backing` (write via `0x3000_0010`, read via `0x2000_0010`); `unmapped_read_returns_zero_and_logs` (entry has pc, width, secure); `log_dedups_and_counts`; `rom_write_is_logged_not_applied`; `straddling_word_read` (word at region_end-2: two bytes from region, two unmapped → logged; Review Focus 1); `device_dispatch_offsets`; `to_markdown_table_format` (header `| addr | width | R/W | value | pc | sec | count |`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement (regions in a `Vec` sorted by base, binary search; RAM as `Rc<RefCell<Vec<u8>>>`-free design: store backing in a `Vec<Vec<u8>>` arena and alias entries index into it). **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(evo-hw): memory map with unmapped-access logging`.

### Task 13: Bundle and MCUboot parsing (`evo-image`)

**Files:** Create `crates/evo-image/{Cargo.toml,src/lib.rs,src/bundle.rs,src/mcuboot.rs,src/layout.rs}`; test fixtures generated in-test (no TI data).

**Interfaces — Produces:**
```rust
#[derive(thiserror::Error, Debug)] pub enum ImageError { Zip(..), MissingPackage, BadMagic { offset: usize }, Truncated { offset: usize, need: usize, have: usize }, Io(..) }
pub struct Metadata { pub build_version: String, pub target_device: String, pub prodid: u32, pub kind: String }
pub struct McubootImage { pub pkg_offset: usize, pub header_size: u16, pub image_size: u32, pub protected_tlv_size: u16,
                          pub flags: u32, pub version: (u8, u8, u16, u32), pub body: std::ops::Range<usize> }
pub struct Package { pub bytes: Vec<u8>, pub images: Vec<McubootImage> }
pub struct Bundle { pub metadata: Metadata, pub package: Package }
impl Bundle { pub fn open(path: &Path) -> Result<Self, ImageError>; pub fn from_bytes(b: &[u8]) -> Result<Self, ImageError>; }
impl Package { pub fn parse(bytes: Vec<u8>) -> Result<Self, ImageError>; }   // also accepts a bare .84pk2
pub struct Layout { pub secure_slot_base: u32, pub ns_slot_base: u32 }      // flash addresses of image 0 / image 1 slots (header start)
impl Layout { pub fn place(&self, pkg: &Package, flash: &mut [u8], flash_base: u32) -> Result<(), ImageError>; }
```
MCUboot header (little-endian): magic `0x96f3b83d` @0, load_addr u32 @4, hdr_size u16 @8, protect_tlv_size u16 @10, img_size u32 @12, flags u32 @16, version {u8 major, u8 minor, u16 revision, u32 build} @20. Images are found by scanning the package for the magic at 4-byte alignment and validating `hdr_size + img_size` fits. `Package::parse` on the real file must yield 10 images with the offsets/sizes in spec §2.2.

- [ ] **Step 1:** Tests: `parses_synthetic_two_image_package`; `metadata_fields` (synthetic ZIP built with `zip::ZipWriter` in-test); `bare_pk2_accepted`; `truncated_image_errors` (Review Focus 2); `bad_zip_errors`; `missing_package_errors`; `real_bundle_matches_spec` (reads `EVO_OS_IMAGE`; skip if unset; asserts 10 images, image0 hdr 0x400 size 0xDAEB8, image1 at 0x100000 hdr 0x200 size 0x293678).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS (with `EVO_OS_IMAGE` set locally too).
- [ ] **Step 5:** Commit `feat(evo-image): .84b2 bundle and MCUboot parsing`.

### Task 14: Image layout discovery

The flash bases are unknown (spec §5.1). Determine them with evidence, then hard-code them as `Layout::EVO_7_1` defaults.

**Files:** Create `apps/evo-cli/src/inspect.rs` (subcommand `evo-cli inspect <bundle> [--score-bases]`), `docs/hardware/memory-map.md`.

Scoring algorithm (`--score-bases`), per image, for candidate slot bases `B` in `[min_handler - size, min_handler]` stepped by 0x100 and aligned to 0x400 for the vector table at `B + hdr_size`:
1. +10 per vector-table entry (non-zero, odd) that lands inside `[B, B+hdr+size)` and decodes (via `armv8m::decode16/32`) as a non-`Undefined` instruction at `entry & !1`.
2. +1 per 32-bit literal in the image equal to `B + o` where `o` is the start offset of a NUL-terminated printable string (≥ 6 chars) in the image.
3. +1 per BL instruction in the first 64 KiB of code whose target lands inside the image and decodes as a PUSH-type prologue.
Print the top 5 bases with score breakdown. For image 1, the vector table may not be at `B + hdr_size` (entries 2.. look like code); score only criteria 2–3 and report the reset vector's decoded instruction.

- [ ] **Step 1:** Unit test `score_picks_known_base` on a synthetic image built in-test (vector table + strings + literal pointers at base `0x1000_0000`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Run on the real bundle (`evo-cli inspect $EVO_OS_IMAGE --score-bases`); record the winners and the evidence (scores, the disassembly of the reset handler's first 8 instructions) in `docs/hardware/memory-map.md`; set `Layout::EVO_7_1`. If no candidate wins by a clear margin (≥2× runner-up), stop and report to the user instead of guessing.
- [ ] **Step 6:** Commit `feat(evo-cli): inspect + image base discovery; document memory map`.

### Task 15: Machine and headless CLI

**Files:** Create `crates/evo-core/src/machine.rs`, `apps/evo-cli/{Cargo.toml,src/main.rs}`; modify `crates/evo-core/src/lib.rs`.

**Interfaces — Consumes:** `Cpu`, `MemoryMap`, `Bundle`, `Layout::EVO_7_1`, `Scheduler`.
**Produces:**
```rust
pub enum StopReason { CycleLimit, Breakpoint(u32), Lockup { pc: u32 }, Bkpt { pc: u32, imm: u8 } }
pub struct Machine { pub cpu: Cpu, pub bus: MemoryMap, pub sched: Scheduler, pub breakpoints: BTreeSet<u32> }
impl Machine {
    pub fn from_bundle(b: &Bundle, layout: &Layout) -> Result<Self, ImageError>;
    pub fn run(&mut self, max_cycles: u64) -> StopReason;      // WFI with no pending event and no IRQ → advance `now` to next deadline; if none → CycleLimit
    pub fn step(&mut self) -> StopReason;
}
```
Initial map (provisional, refined in Plan 2): flash ROM aliased at NS `0x0000_0000` and S `0x1000_0000` sized to cover both slots (round up to 8 MiB); SRAM 2 MiB aliased at `0x2000_0000`/`0x3000_0000`. Boot: CPU `VTOR_S` = secure slot base + 0x400, reset per Task 7.

CLI: `evo-cli run <bundle> [--max-cycles N=1_000_000_000] [--trace] [--trace-range A..B] [--break ADDR]... [--unmapped-out FILE.md] [--gdb PORT]`; `evo-cli inspect …` (Task 14). Trace line format: `{cycles:>12} {S|N} {pc:08x}: {disasm}`. On exit prints `stop: <reason> pc=<hex> cycles=<n>`, exits 0 for CycleLimit/Breakpoint, 2 for Lockup.

- [ ] **Step 1:** Tests (`evo-core`, synthetic bundle built in-test whose secure image is a 6-instruction loop): `machine_boots_synthetic_bundle` (PC enters loop); `run_stops_at_cycle_limit` (Review Focus 3); `lockup_reported` (image whose reset handler is `udf`, HardFault vector → `udf` → `StopReason::Lockup`); `breakpoint_stops`; `wfi_skips_to_next_event`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** CLI integration test `cli_lockup_exit_code` with `assert_cmd` → exit code 2 and `stop: lockup` in stdout.
- [ ] **Step 6:** Commit `feat: Machine and evo-cli run`.

### Task 16: GDB remote stub

**Files:** Create `crates/evo-core/src/gdb.rs`; modify `apps/evo-cli/src/main.rs`.

**Interfaces — Produces:** `pub fn serve(machine: &mut Machine, listener: std::net::TcpListener) -> std::io::Result<()>` — single client, blocking, using the `gdbstub` crate (0.7) with the `armv7m`-style target description extended by an `org.gnu.gdb.arm.m-system` feature (msp, psp, primask, basepri, faultmask, control) and `org.gnu.gdb.arm.secext` (msp_s, msp_ns, psp_s, psp_ns, msplim_s/ns, psplim_s/ns). Supports: register read/write, memory read/write (through `Bus` with `secure` = current state; reads of unmapped memory return 0 and are **not** added to the unmapped log), sw breakpoints (via `Machine::breakpoints`, not memory patching), single step, continue, Ctrl-C interrupt, detach/kill.

- [ ] **Step 1:** Tests (in-process client over `TcpStream`, raw RSP packets): `reads_registers` (`g` returns 17+ regs, PC matches); `sets_breakpoint_and_continues` (`Z0`, `c` → `T05`); `malformed_packet_is_nacked` (bad checksum → `-`, then valid `?` still answered; Review Focus 5); `disconnect_leaves_machine_runnable` (drop client mid-session → `serve` returns Ok, `machine.step()` still works).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(evo-core): GDB remote stub`.

### Task 17: First boot of the real OS and discovery log

**Files:** Create `docs/hardware/discovery-log.md`; add `crates/evo-core/tests/real_os.rs`.

- [ ] **Step 1:** Write `real_os_runs_without_lockup_for_10m_cycles` (skip if `EVO_OS_IMAGE` unset): `Machine::run(10_000_000)` returns `CycleLimit` or `Breakpoint`, not `Lockup`; if it locks up, the test prints the last 64 trace lines (ring buffer in `Machine`, `pub fn recent_trace(&self) -> Vec<String>`; add it here).
- [ ] **Step 2:** Run `evo-cli run $EVO_OS_IMAGE --max-cycles 50000000 --unmapped-out docs/hardware/discovery-log.md`. Expected: either progress with a growing unmapped log, or a lockup/stall whose root cause is a CPU bug (fix with a regression test in the relevant `armv8m` test file) or missing hardware (record in the log; it becomes Plan 2 input).
- [ ] **Step 3:** Iterate Step 2 until the run reaches a steady state (polling loop on an unmapped register, or WFI with no events). Do **not** implement TI peripherals in this plan; stubs belong to Plan 2. Commit fixes individually as `fix(armv8m): …`.
- [ ] **Step 4:** Write `docs/hardware/discovery-log.md`: the unmapped table, the PC where execution settles with disassembly of the polling loop, and grouped first guesses (address block → likely peripheral from nearby IRQ-handler code). Contains no TI code bytes beyond short disassembly excerpts.
- [ ] **Step 5:** Commit `docs: first boot discovery log`; report to the user and hand off to writing Plan 2.
