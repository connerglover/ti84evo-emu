//! Differential testing of instruction semantics against Unicorn (QEMU's Cortex-M33 model).
//!
//! Each case is a random sequence of 1-8 items drawn from an encoding-template table (16-bit
//! data processing, 32-bit data processing / multiply / divide, DSP, loads and stores). An item
//! is one instruction under test (memory items add their MOVW/MOVT setup; the exclusive-pair
//! template is LDREX..STREX) or an IT block of 1-4 of them. Each case runs from the same random
//! r0-r12, LR, NZCVQ, GE and memory contents on `Cpu` + `FlatBus` and on Unicorn (`Arch::ARM`,
//! `Mode::THUMB | Mode::MCLASS`, CPU model Cortex-M33), both in privileged Thread mode. Afterwards r0-r12, LR, SP, NZCVQ, GE and the
//! 4 KiB RAM window must match, and neither model may have taken an exception.
//!
//! Memory items first point their base register at `0x2000_0000 | (rand & 0xFF0)` (MOVW/MOVT)
//! and pick offsets that keep every access inside the window, which is all the RAM Unicorn maps.
//! SP starts at `0x2000_0800`; SP-relative accesses and PUSH/POP stay well inside the window.
//! Branches, exceptions, coprocessor, system and TrustZone instructions are not generated, nor
//! are UNPREDICTABLE encodings: every generated encoding must decode to a defined instruction.
//!
//! Templates where Unicorn, not our core, is wrong are listed in `KNOWN_UNICORN_DIVERGENCES`
//! with the DDI0553 citation and excluded.
//!
//! # Running
//!
//! `cargo test -p armv8m --features difftest` runs 256 cases per test; `DIFFTEST_CASES=10000`
//! sets the count and `PROPTEST_RNG_SEED=<n>` replays a run. A failure prints the shrunk
//! program (address, encoding, disassembly, template) and both final states.
//!
//! Unicorn is GPL-2.0: the `difftest` feature is test-only and never enabled for a shipped
//! binary. Building it needs CMake and a C compiler (the Ubuntu CI job installs cmake).
//! On Windows (MSVC) `unicorn-engine-sys` additionally needs Ninja and CMake on `PATH` (Visual
//! Studio's bundled `CommonExtensions\Microsoft\CMake\{CMake\bin,Ninja}` work), `LIBCLANG_PATH`
//! pointing at a `libclang.dll` for bindgen (e.g. from `pip install libclang`), and the
//! privilege to create symbolic links (Developer Mode), which unicorn's CMake uses after
//! linking. Without Developer Mode, a CMake toolchain file named by `CMAKE_TOOLCHAIN_FILE` can
//! override `add_custom_command` to turn that `create_symlink` into `copy`.
#![cfg(feature = "difftest")]

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Mutex, PoisonError};

use armv8m::{Cpu, Instr, StepResult, decode16, decode32, disasm};
use common::{FlatBus, RAM_BASE};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use unicorn_engine::unicorn_const::{Arch, Mode, Prot};
use unicorn_engine::{ArmCpuModel, RegisterARM, Unicorn};

/// Templates excluded because Unicorn (not our core) is wrong for them: template name, and what
/// Unicorn does against what the Armv8-M ARM (DDI0553) says.
const KNOWN_UNICORN_DIVERGENCES: &[(&str, &str)] = &[
    (
        "t16.sp_write",
        "ADD SP, Rm / MOV SP, Rm with Rm[1:0] != 0: Unicorn keeps SP[1:0]. DDI0553 (Armv8-M \
         ARM), SP_main/SP_process register descriptions: bits[1:0] are RES0H, hardwired to \
         zero with writes ignored, which our core implements. QEMU fixed this in 6.1 (commit \
         888f470f, 'target/arm: Enforce that M-profile SP low 2 bits are always zero'); \
         Unicorn 2.1 is based on QEMU 5.0.",
    ),
    (
        "t32.sp_write",
        "As t16.sp_write, for ADD.W/SUB.W SP, SP, Rm, ADD{S}.W/SUB{S}.W/ADDW/SUBW SP, SP, #imm \
         with an unaligned result, and MOV.W SP, Rm. (t32.sp_adjust keeps the aligned forms.)",
    ),
];

/// Where the generated code starts.
const CODE_ADDR: u32 = 0x100;
/// Bytes of the code region given random contents (around the code; literal loads read them).
const POOL: usize = 0x2000;
/// Size of the RAM window at `RAM_BASE` that Unicorn maps and the comparison covers.
const WINDOW: u32 = 0x1000;
const INITIAL_SP: u32 = RAM_BASE + 0x800;
/// Upper bound on instructions executed per case (by either model).
const MAX_STEPS: usize = 256;

// ---------------------------------------------------------------------------------------------
// Encodings and templates
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enc {
    T16(u16),
    T32(u16, u16),
}

impl Enc {
    fn len(self) -> u32 {
        match self {
            Enc::T16(_) => 2,
            Enc::T32(..) => 4,
        }
    }

    fn push_bytes(self, out: &mut Vec<u8>) {
        match self {
            Enc::T16(h) => out.extend_from_slice(&h.to_le_bytes()),
            Enc::T32(a, b) => {
                out.extend_from_slice(&a.to_le_bytes());
                out.extend_from_slice(&b.to_le_bytes());
            }
        }
    }

    fn decode(self) -> Instr {
        match self {
            Enc::T16(h) => decode16(h),
            Enc::T32(a, b) => decode32(a, b),
        }
    }

    fn hex(self) -> String {
        match self {
            Enc::T16(h) => format!("{h:04x}     "),
            Enc::T32(a, b) => format!("{a:04x} {b:04x}"),
        }
    }
}

fn t16(v: u32) -> Enc {
    Enc::T16(v as u16)
}

fn t32(hw1: u32, hw2: u32) -> Enc {
    Enc::T32(hw1 as u16, hw2 as u16)
}

/// Raw random material a template turns into a valid encoding.
type Fields = [u32; 6];

/// A template's output: `setup` (unconditional; points base registers into the RAM window)
/// followed by the instruction(s) under test.
struct Snip {
    setup: Vec<Enc>,
    body: Vec<Enc>,
}

fn one(e: Enc) -> Snip {
    Snip {
        setup: Vec::new(),
        body: vec![e],
    }
}

fn with_setup(setup: Vec<Enc>, e: Enc) -> Snip {
    Snip {
        setup,
        body: vec![e],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// One instruction, no setup: may go anywhere in an IT block.
    Plain,
    /// Setup plus one instruction: may lead an IT block (the setup is hoisted before the IT).
    Setup,
    /// Never placed in an IT block (several instructions, or moves SP).
    Multi,
}

struct Template {
    name: &'static str,
    kind: Kind,
    /// `(fields, in_it_block)`.
    make: fn(&Fields, bool) -> Snip,
}

// --- field helpers ---------------------------------------------------------------------------

fn bit(f: u32, n: u32) -> u32 {
    (f >> n) & 1
}

/// r0-r7.
fn lo(f: u32) -> u32 {
    f & 7
}

/// r0-r12 and LR: the registers both models' states are compared on.
const GP: [u32; 14] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14];

fn gp(f: u32) -> u32 {
    GP[(f % 14) as usize]
}

/// A `gp` register not in `avoid`.
fn gp_not(f: u32, avoid: &[u32]) -> u32 {
    let start = (f % 14) as usize;
    (0..14)
        .map(|k| GP[(start + k) % 14])
        .find(|r| !avoid.contains(r))
        .expect("a free register")
}

/// A low register other than `avoid`.
fn lo_not(f: u32, avoid: u32) -> u32 {
    let r = lo(f);
    if r == avoid { (r + 1) & 7 } else { r }
}

/// `f` reduced to `0..=max`.
fn pick(f: u32, max: u32) -> u32 {
    f % (max + 1)
}

/// The in-window base address `0x2000_0000 | (rand & 0xFF0)`.
fn base(rand: u32) -> u32 {
    RAM_BASE | (rand & 0xFF0)
}

/// Largest offset in `scale` units (at most `max`) keeping a `bytes`-wide access at
/// `base + offset` inside the window.
fn room_up(base: u32, bytes: u32, scale: u32, max: u32) -> u32 {
    ((WINDOW - bytes - (base - RAM_BASE)) / scale).min(max)
}

/// Largest offset in `scale` units (at most `max`) keeping `base - offset` inside the window.
fn room_down(base: u32, scale: u32, max: u32) -> u32 {
    ((base - RAM_BASE) / scale).min(max)
}

/// `MOVW` (`top == false`) / `MOVT rd, #imm16`.
fn mov16(top: bool, rd: u32, imm: u32) -> Enc {
    let op = if top { 0xF2C0 } else { 0xF240 };
    t32(
        op | (bit(imm, 11) << 10) | ((imm >> 12) & 0xF),
        (((imm >> 8) & 7) << 12) | (rd << 8) | (imm & 0xFF),
    )
}

/// `MOVW rd, #lo16; MOVT rd, #hi16`.
fn set_reg(rd: u32, v: u32) -> Vec<Enc> {
    vec![mov16(false, rd, v & 0xFFFF), mov16(true, rd, v >> 16)]
}

/// The `IT` instruction for a block of `k` instructions; `elses` bit `i` makes instruction `i`
/// an else (never for AL, where an else would be the UNPREDICTABLE condition 0b1111).
fn it_instr(firstcond: u32, elses: u32, k: usize) -> Enc {
    let fc0 = firstcond & 1;
    let mut mask = 1 << (4 - k);
    for i in 1..k {
        let e = if firstcond == 14 {
            0
        } else {
            bit(elses, i as u32)
        };
        mask |= (fc0 ^ e) << (4 - i);
    }
    t16(0xBF00 | (firstcond << 4) | mask)
}

/// A 12-bit modified immediate that `ThumbExpandImm` accepts (a zero byte with a replicating
/// pattern is UNPREDICTABLE).
fn modimm(f: u32) -> u32 {
    let imm12 = f & 0xFFF;
    if imm12 >> 10 == 0 && (imm12 >> 8) & 3 != 0 && imm12 & 0xFF == 0 {
        imm12 | 1
    } else {
        imm12
    }
}

fn modimm_enc(op: u32, s: u32, rn: u32, rd: u32, imm12: u32) -> Enc {
    t32(
        0xF000 | (bit(imm12, 11) << 10) | (op << 5) | (s << 4) | rn,
        (((imm12 >> 8) & 7) << 12) | (rd << 8) | (imm12 & 0xFF),
    )
}

fn shreg_enc(op: u32, s: u32, rn: u32, rd: u32, rm: u32, ty: u32, imm5: u32) -> Enc {
    t32(
        0xEA00 | (op << 5) | (s << 4) | rn,
        ((imm5 >> 2) << 12) | (rd << 8) | ((imm5 & 3) << 6) | (ty << 4) | rm,
    )
}

/// `11111010 op Rn | 1111 Rd op2 Rm` (data processing, register).
fn reg_op(hw1: u32, op2: u32, rd: u32, rm: u32) -> Enc {
    t32(hw1, 0xF000 | (rd << 8) | (op2 << 4) | rm)
}

/// `11111011 op Rn | Ra Rd op2 Rm` (multiply and DSP multiply).
fn mul_op(hw1: u32, ra: u32, rd: u32, op2: u32, rm: u32) -> Enc {
    t32(hw1, (ra << 12) | (rd << 8) | (op2 << 4) | rm)
}

/// Optional accumulator: PC (the non-accumulating form) or a `gp` register.
fn acc(f: u32) -> u32 {
    if f.is_multiple_of(3) { 15 } else { gp(f >> 2) }
}

/// SSAT / USAT with an LSL or (non-zero) ASR shift.
fn ssat_usat(f: &Fields, _: bool) -> Snip {
    let sh = bit(f[0], 1);
    let mut imm5 = f[1] % 32;
    // ASR #0 is SSAT16/USAT16 (DSP table).
    if sh == 1 && imm5 == 0 {
        imm5 = 1;
    }
    let op = if bit(f[0], 0) != 0 { 0xF380 } else { 0xF300 };
    one(t32(
        op | (sh << 5) | gp(f[2]),
        ((imm5 >> 2) << 12) | (gp(f[3]) << 8) | ((imm5 & 3) << 6) | (f[4] & 0x1F),
    ))
}

/// MOVS/CMP/ADDS/SUBS Rd, #imm8.
fn t16_imm8(f: &Fields, _: bool) -> Snip {
    one(t16(0x2000
        | ((f[0] & 3) << 11)
        | (lo(f[1]) << 8)
        | (f[2] & 0xFF)))
}

/// 16-bit data processing (register).
fn t16_dp_reg(f: &Fields, _: bool) -> Snip {
    one(t16(0x4000
        | ((f[0] & 15) << 6)
        | (lo(f[1]) << 3)
        | lo(f[2])))
}

// --- 16-bit data processing --------------------------------------------------------------------

/// Registers for the 16-bit high-register forms: any but SP and PC.
fn hi(f: u32) -> u32 {
    gp(f)
}

const ALU16: &[Template] = &[
    Template {
        name: "t16.shift_imm",
        kind: Kind::Plain,
        make: |f, it| {
            let op = f[0] % 3;
            let mut imm5 = f[1] % 32;
            // LSLS #0 is MOVS (register), UNPREDICTABLE in an IT block.
            if it && op == 0 && imm5 == 0 {
                imm5 = 1;
            }
            one(t16((op << 11) | (imm5 << 6) | (lo(f[2]) << 3) | lo(f[3])))
        },
    },
    Template {
        name: "t16.addsub_reg",
        kind: Kind::Plain,
        make: |f, _| {
            one(t16(0x1800
                | (bit(f[0], 0) << 9)
                | (lo(f[1]) << 6)
                | (lo(f[2]) << 3)
                | lo(f[3])))
        },
    },
    Template {
        name: "t16.addsub_imm3",
        kind: Kind::Plain,
        make: |f, _| {
            one(t16(0x1C00
                | (bit(f[0], 0) << 9)
                | ((f[1] & 7) << 6)
                | (lo(f[2]) << 3)
                | lo(f[3])))
        },
    },
    Template {
        name: "t16.imm8",
        kind: Kind::Plain,
        make: t16_imm8,
    },
    Template {
        name: "t16.dp_reg",
        kind: Kind::Plain,
        make: t16_dp_reg,
    },
    Template {
        name: "t16.add_hi",
        kind: Kind::Plain,
        make: |f, _| {
            let (d, m) = (hi(f[0]), hi(f[1]));
            one(t16(0x4400 | ((d >> 3) << 7) | (m << 3) | (d & 7)))
        },
    },
    Template {
        name: "t16.cmp_hi",
        kind: Kind::Plain,
        make: |f, _| {
            // Two low registers are UNPREDICTABLE here (that is the 16-bit CMP encoding).
            let m = hi(f[1]);
            let n = if m < 8 {
                [8, 9, 10, 11, 12, 14][(f[0] % 6) as usize]
            } else {
                hi(f[0])
            };
            one(t16(0x4500 | ((n >> 3) << 7) | (m << 3) | (n & 7)))
        },
    },
    Template {
        name: "t16.mov_hi",
        kind: Kind::Plain,
        make: |f, _| {
            let (d, m) = (hi(f[0]), hi(f[1]));
            one(t16(0x4600 | ((d >> 3) << 7) | (m << 3) | (d & 7)))
        },
    },
    Template {
        name: "t16.add_sp_reg",
        kind: Kind::Plain,
        // ADD Rdm, SP, Rdm
        make: |f, _| {
            let dm = hi(f[0]);
            one(t16(0x4468 | ((dm >> 3) << 7) | (dm & 7)))
        },
    },
    Template {
        name: "t16.pc_sp_operand",
        kind: Kind::Plain,
        // ADD Rdn, PC / MOV Rd, PC / MOV Rd, SP (high-register forms reading PC or SP)
        make: |f, _| {
            let d = hi(f[1]);
            let op = [0x4478, 0x4678, 0x4668][(f[0] % 3) as usize];
            one(t16(op | ((d >> 3) << 7) | (d & 7)))
        },
    },
    Template {
        name: "t16.sp_write",
        kind: Kind::Plain,
        // ADD SP, Rm / MOV SP, Rm (no memory is accessed in this table, so SP may go anywhere)
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0x4685 } else { 0x4485 };
            one(t16(op | (hi(f[1]) << 3)))
        },
    },
    Template {
        name: "t16.adr",
        kind: Kind::Plain,
        make: |f, _| one(t16(0xA000 | (lo(f[0]) << 8) | (f[1] & 0xFF))),
    },
    Template {
        name: "t16.add_rd_sp_imm",
        kind: Kind::Plain,
        make: |f, _| one(t16(0xA800 | (lo(f[0]) << 8) | (f[1] & 0xFF))),
    },
    Template {
        name: "t16.addsub_sp_imm",
        kind: Kind::Plain,
        make: |f, _| one(t16(0xB000 | (bit(f[0], 0) << 7) | (f[1] & 0x7F))),
    },
    Template {
        name: "t16.extend",
        kind: Kind::Plain,
        make: |f, _| one(t16(0xB200 | ((f[0] & 3) << 6) | (lo(f[1]) << 3) | lo(f[2]))),
    },
    Template {
        name: "t16.rev",
        kind: Kind::Plain,
        make: |f, _| {
            let op = [0, 1, 3][(f[0] % 3) as usize];
            one(t16(0xBA00 | (op << 6) | (lo(f[1]) << 3) | lo(f[2])))
        },
    },
];

// --- 32-bit data processing, multiply, divide ---------------------------------------------------

const T32: &[Template] = &[
    Template {
        name: "t32.modimm_dp",
        kind: Kind::Plain,
        make: |f, _| {
            // AND BIC ORR ORN EOR ADD ADC SBC SUB RSB
            let op = [0, 1, 2, 3, 4, 8, 10, 11, 13, 14][(f[0] % 10) as usize];
            one(modimm_enc(
                op,
                bit(f[0], 8),
                gp(f[1]),
                gp(f[2]),
                modimm(f[3]),
            ))
        },
    },
    Template {
        name: "t32.modimm_mov",
        kind: Kind::Plain,
        // MOV / MVN (Rn == PC)
        make: |f, _| {
            one(modimm_enc(
                2 + bit(f[0], 0),
                bit(f[0], 1),
                15,
                gp(f[1]),
                modimm(f[2]),
            ))
        },
    },
    Template {
        name: "t32.modimm_cmp",
        kind: Kind::Plain,
        // TST TEQ CMN CMP (Rd == PC, S)
        make: |f, _| {
            let op = [0, 4, 8, 13][(f[0] % 4) as usize];
            // CMN/CMP may compare SP (TST/TEQ may not).
            let rn = if op >= 8 && bit(f[0], 4) != 0 {
                13
            } else {
                gp(f[1])
            };
            one(modimm_enc(op, 1, rn, 15, modimm(f[2])))
        },
    },
    Template {
        name: "t32.addw_subw",
        kind: Kind::Plain,
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0xF2A0 } else { 0xF200 };
            let imm12 = f[1] & 0xFFF;
            one(t32(
                op | (bit(imm12, 11) << 10) | gp(f[2]),
                (((imm12 >> 8) & 7) << 12) | (gp(f[3]) << 8) | (imm12 & 0xFF),
            ))
        },
    },
    Template {
        name: "t32.adr",
        kind: Kind::Plain,
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0xF2AF } else { 0xF20F };
            let imm12 = f[1] & 0xFFF;
            one(t32(
                op | (bit(imm12, 11) << 10),
                (((imm12 >> 8) & 7) << 12) | (gp(f[2]) << 8) | (imm12 & 0xFF),
            ))
        },
    },
    Template {
        name: "t32.add_sp_imm",
        kind: Kind::Plain,
        // ADD.W / SUB.W / ADDW / SUBW Rd, SP, #imm (reads SP; Rd is not SP)
        make: |f, _| {
            let rd = gp(f[1]);
            let imm12 = f[2] & 0xFFF;
            let hw2 = (((imm12 >> 8) & 7) << 12) | (rd << 8) | (imm12 & 0xFF);
            one(match f[0] % 4 {
                0 => t32(0xF20D | (bit(imm12, 11) << 10), hw2),
                1 => t32(0xF2AD | (bit(imm12, 11) << 10), hw2),
                2 => modimm_enc(8, bit(f[3], 0), 13, rd, modimm(f[2])),
                _ => modimm_enc(13, bit(f[3], 0), 13, rd, modimm(f[2])),
            })
        },
    },
    Template {
        name: "t32.movw_movt",
        kind: Kind::Plain,
        make: |f, _| one(mov16(bit(f[0], 0) != 0, gp(f[1]), f[2] & 0xFFFF)),
    },
    Template {
        name: "t32.ssat_usat",
        kind: Kind::Plain,
        make: ssat_usat,
    },
    Template {
        name: "t32.bfx",
        kind: Kind::Plain,
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0xF3C0 } else { 0xF340 };
            let lsb = f[1] % 32;
            let widthm1 = f[2] % (32 - lsb);
            one(t32(
                op | gp(f[3]),
                ((lsb >> 2) << 12) | (gp(f[4]) << 8) | ((lsb & 3) << 6) | widthm1,
            ))
        },
    },
    Template {
        name: "t32.bfi_bfc",
        kind: Kind::Plain,
        make: |f, _| {
            let lsb = f[1] % 32;
            let msb = lsb + f[2] % (32 - lsb);
            let rn = if f[0] % 4 == 0 { 15 } else { gp(f[3]) };
            one(t32(
                0xF360 | rn,
                ((lsb >> 2) << 12) | (gp(f[4]) << 8) | ((lsb & 3) << 6) | msb,
            ))
        },
    },
    Template {
        name: "t32.shreg_dp",
        kind: Kind::Plain,
        make: |f, _| {
            let op = [0, 1, 2, 3, 4, 8, 10, 11, 13, 14][(f[0] % 10) as usize];
            one(shreg_enc(
                op,
                bit(f[0], 8),
                gp(f[1]),
                gp(f[2]),
                gp(f[3]),
                f[4] & 3,
                f[5] % 32,
            ))
        },
    },
    Template {
        name: "t32.shreg_mov",
        kind: Kind::Plain,
        // MOV / LSL / LSR / ASR / ROR / RRX (immediate) and MVN (register): Rn == PC
        make: |f, _| {
            one(shreg_enc(
                2 + bit(f[0], 0),
                bit(f[0], 1),
                15,
                gp(f[1]),
                gp(f[2]),
                f[3] & 3,
                f[4] % 32,
            ))
        },
    },
    Template {
        name: "t32.shreg_cmp",
        kind: Kind::Plain,
        make: |f, _| {
            let op = [0, 4, 8, 13][(f[0] % 4) as usize];
            one(shreg_enc(
                op,
                1,
                gp(f[1]),
                15,
                gp(f[2]),
                f[3] & 3,
                f[4] % 32,
            ))
        },
    },
    Template {
        name: "t32.sp_reg",
        kind: Kind::Plain,
        // ADD{S}.W / SUB{S}.W Rd, SP, Rm{, shift}; CMN / CMP SP, Rm{, shift}; MOV.W Rd, SP
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 13 } else { 8 };
            let (rm, ty, imm5) = (gp(f[2]), f[3] & 3, f[4] % 32);
            one(match (f[0] >> 1) % 3 {
                0 => shreg_enc(op, bit(f[0], 3), 13, gp(f[1]), rm, ty, imm5),
                1 => shreg_enc(op, 1, 13, 15, rm, ty, imm5),
                _ => shreg_enc(2, 0, 15, gp(f[1]), 13, 0, 0),
            })
        },
    },
    Template {
        name: "t32.sp_write",
        kind: Kind::Plain,
        // ADD.W/SUB.W SP, SP, Rm, LSL #0-3; ADD{S}.W/SUB{S}.W/ADDW/SUBW SP, SP, #imm; MOV.W SP, Rm
        make: |f, _| {
            let rm = gp(f[2]);
            let imm12 = f[3] & 0xFFF;
            let hw2 = (((imm12 >> 8) & 7) << 12) | (13 << 8) | (imm12 & 0xFF);
            one(match f[0] % 7 {
                0 => shreg_enc(8, 0, 13, 13, rm, 0, f[1] % 4),
                1 => shreg_enc(13, 0, 13, 13, rm, 0, f[1] % 4),
                2 => modimm_enc(8, bit(f[1], 4), 13, 13, modimm(f[3])),
                3 => modimm_enc(13, bit(f[1], 4), 13, 13, modimm(f[3])),
                4 => t32(0xF20D | (bit(imm12, 11) << 10), hw2),
                5 => t32(0xF2AD | (bit(imm12, 11) << 10), hw2),
                _ => shreg_enc(2, 0, 15, 13, rm, 0, 0),
            })
        },
    },
    Template {
        name: "t32.sp_adjust",
        kind: Kind::Plain,
        // ADD{S}.W/SUB{S}.W/ADDW/SUBW SP, SP, #imm with a multiple-of-4 immediate
        make: |f, _| {
            let imm12 = f[1] & 0xFFC;
            let hw2 = (((imm12 >> 8) & 7) << 12) | (13 << 8) | (imm12 & 0xFF);
            // A rotation of 8..=24 places the 8-bit pattern clear of bits [1:0].
            let rot = (8 + f[2] % 17) << 7 | (f[3] & 0x7F);
            one(match f[0] % 4 {
                0 => t32(0xF20D | (bit(imm12, 11) << 10), hw2),
                1 => t32(0xF2AD | (bit(imm12, 11) << 10), hw2),
                2 => modimm_enc(8, bit(f[0], 4), 13, 13, rot),
                _ => modimm_enc(13, bit(f[0], 4), 13, 13, rot),
            })
        },
    },
    Template {
        name: "t32.shift_reg",
        kind: Kind::Plain,
        make: |f, _| {
            let hw1 = 0xFA00 | ((f[0] & 3) << 5) | (bit(f[0], 2) << 4) | gp(f[1]);
            one(reg_op(hw1, 0, gp(f[2]), gp(f[3])))
        },
    },
    Template {
        name: "t32.extend",
        kind: Kind::Plain,
        // SXTH UXTH SXTB UXTB (Rn == PC) and SXTAH UXTAH SXTAB UXTAB
        make: |f, _| {
            let op1 = [0, 1, 4, 5][(f[0] % 4) as usize];
            let rn = if bit(f[0], 4) != 0 { 15 } else { gp(f[1]) };
            one(reg_op(
                0xFA00 | (op1 << 4) | rn,
                8 | (f[2] & 3),
                gp(f[3]),
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "t32.misc",
        kind: Kind::Plain,
        // REV REV16 RBIT REVSH CLZ
        make: |f, _| {
            let rm = gp(f[1]);
            let (hw1, op2) = match f[0] % 5 {
                4 => (0xFAB0, 8),
                k => (0xFA90, 8 | k),
            };
            one(reg_op(hw1 | rm, op2, gp(f[2]), rm))
        },
    },
    Template {
        name: "t32.mul",
        kind: Kind::Plain,
        // MUL MLA MLS
        make: |f, _| {
            let k = f[0] % 3;
            let ra = if k == 0 { 15 } else { gp(f[1]) };
            one(mul_op(
                0xFB00 | gp(f[2]),
                ra,
                gp(f[3]),
                u32::from(k == 2),
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "t32.long_mul",
        kind: Kind::Plain,
        // SMULL UMULL SMLAL UMLAL
        make: |f, _| {
            let op1 = [0, 2, 4, 6][(f[0] % 4) as usize];
            let rdlo = gp(f[1]);
            let rdhi = gp_not(f[2], &[rdlo]);
            one(mul_op(
                0xFB80 | (op1 << 4) | gp(f[3]),
                rdlo,
                rdhi,
                0,
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "t32.div",
        kind: Kind::Plain,
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0xFBB0 } else { 0xFB90 };
            one(mul_op(op | gp(f[1]), 15, gp(f[2]), 0xF, gp(f[3])))
        },
    },
];

// --- DSP extension -------------------------------------------------------------------------------

const DSP: &[Template] = &[
    Template {
        name: "dsp.parallel",
        kind: Kind::Plain,
        make: |f, _| {
            let op1 = [0, 1, 2, 4, 5, 6][(f[0] % 6) as usize];
            let kind = [0, 1, 2, 4, 5, 6][(f[1] % 6) as usize];
            one(reg_op(
                0xFA80 | (op1 << 4) | gp(f[2]),
                kind,
                gp(f[3]),
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.qaddsub",
        kind: Kind::Plain,
        // QADD QDADD QSUB QDSUB
        make: |f, _| {
            one(reg_op(
                0xFA80 | gp(f[1]),
                8 | (f[0] & 3),
                gp(f[2]),
                gp(f[3]),
            ))
        },
    },
    Template {
        name: "dsp.sel",
        kind: Kind::Plain,
        make: |f, _| one(reg_op(0xFAA0 | gp(f[0]), 8, gp(f[1]), gp(f[2]))),
    },
    Template {
        name: "dsp.pkh",
        kind: Kind::Plain,
        make: |f, _| {
            let imm5 = f[0] % 32;
            one(t32(
                0xEAC0 | gp(f[1]),
                ((imm5 >> 2) << 12)
                    | (gp(f[2]) << 8)
                    | ((imm5 & 3) << 6)
                    | (bit(f[3], 0) << 5)
                    | gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.sat16",
        kind: Kind::Plain,
        // SSAT16 USAT16
        make: |f, _| {
            let op = if bit(f[0], 0) != 0 { 0xF3A0 } else { 0xF320 };
            one(t32(op | gp(f[1]), (gp(f[2]) << 8) | (f[3] & 0xF)))
        },
    },
    Template {
        name: "dsp.extend16",
        kind: Kind::Plain,
        // SXTB16 UXTB16 SXTAB16 UXTAB16
        make: |f, _| {
            let rn = if bit(f[0], 1) != 0 { 15 } else { gp(f[1]) };
            one(reg_op(
                0xFA20 | (bit(f[0], 0) << 4) | rn,
                8 | (f[2] & 3),
                gp(f[3]),
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.smulxy",
        kind: Kind::Plain,
        // SMULxy SMLAxy
        make: |f, _| {
            one(mul_op(
                0xFB10 | gp(f[1]),
                acc(f[0]),
                gp(f[2]),
                f[3] & 3,
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.smulwy",
        kind: Kind::Plain,
        // SMULWy SMLAWy
        make: |f, _| {
            one(mul_op(
                0xFB30 | gp(f[1]),
                acc(f[0]),
                gp(f[2]),
                f[3] & 1,
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.dual_mul",
        kind: Kind::Plain,
        // SMUAD SMLAD SMUSD SMLSD (+X)
        make: |f, _| {
            let op = if bit(f[3], 1) != 0 { 0xFB40 } else { 0xFB20 };
            one(mul_op(
                op | gp(f[1]),
                acc(f[0]),
                gp(f[2]),
                f[3] & 1,
                gp(f[4]),
            ))
        },
    },
    Template {
        name: "dsp.mul_msw",
        kind: Kind::Plain,
        // SMMUL SMMLA SMMLS (+R)
        make: |f, _| {
            let sub = bit(f[3], 1) != 0;
            let ra = if sub { gp(f[0]) } else { acc(f[0]) };
            let op = if sub { 0xFB60 } else { 0xFB50 };
            one(mul_op(op | gp(f[1]), ra, gp(f[2]), f[3] & 1, gp(f[4])))
        },
    },
    Template {
        name: "dsp.usad8",
        kind: Kind::Plain,
        make: |f, _| one(mul_op(0xFB70 | gp(f[1]), acc(f[0]), gp(f[2]), 0, gp(f[3]))),
    },
    Template {
        name: "dsp.long",
        kind: Kind::Plain,
        // SMLALxy SMLALD(X) SMLSLD(X) UMAAL
        make: |f, _| {
            let rdlo = gp(f[1]);
            let rdhi = gp_not(f[2], &[rdlo]);
            let (op1, op2) = match f[0] % 4 {
                0 => (4, 8 | (f[3] & 3)),
                1 => (4, 0xC | (f[3] & 1)),
                2 => (5, 0xC | (f[3] & 1)),
                _ => (6, 6),
            };
            one(mul_op(
                0xFB80 | (op1 << 4) | gp(f[4]),
                rdlo,
                rdhi,
                op2,
                gp(f[5]),
            ))
        },
    },
    Template {
        name: "dsp.ssat_usat",
        kind: Kind::Plain,
        make: ssat_usat,
    },
];

// --- loads and stores ----------------------------------------------------------------------------

/// `(load, size log2, signed)` for the single-register load/store forms.
const LDST: [(u32, u32, u32); 8] = [
    (0, 0, 0),
    (0, 1, 0),
    (0, 2, 0),
    (1, 0, 0),
    (1, 1, 0),
    (1, 2, 0),
    (1, 0, 1),
    (1, 1, 1),
];

/// A register list of at least two of r0-r12/LR, without `avoid` (when given).
fn reg_list(f: u32, g: u32, avoid: Option<u32>) -> u32 {
    let strip = |l: u32| avoid.map_or(l, |r| l & !(1 << r));
    let mut list = strip(f & 0x5FFF);
    let mut k = g;
    while list.count_ones() < 2 {
        list = strip(list | (1 << gp(k)));
        k = k.wrapping_add(1);
    }
    list
}

/// The exclusive load/store pair for `(size log2, acquire/release)`, base `rn`, word offset
/// `imm8` (only the plain word forms have one).
fn exclusive(size: u32, acq: u32, rn: u32, rt: u32, rt2: u32, rd: u32, imm8: u32) -> [Enc; 2] {
    if size == 2 && acq == 0 {
        [
            t32(0xE850 | rn, (rt << 12) | 0xF00 | imm8),
            t32(0xE840 | rn, (rt2 << 12) | (rd << 8) | imm8),
        ]
    } else {
        let op3 = (if acq != 0 { 0xC } else { 0x4 }) | size;
        [
            t32(0xE8D0 | rn, (rt << 12) | 0xF00 | (op3 << 4) | 0xF),
            t32(0xE8C0 | rn, (rt2 << 12) | 0xF00 | (op3 << 4) | rd),
        ]
    }
}

const MEM: &[Template] = &[
    Template {
        name: "t16.ldst_imm5",
        kind: Kind::Setup,
        // STR LDR STRB LDRB STRH LDRH (immediate)
        make: |f, _| {
            let k = (f[0] % 3) as usize;
            let (op, scale) = [(0x6000, 4), (0x7000, 1), (0x8000, 2)][k];
            let (rn, rt) = (lo(f[1]), lo(f[2]));
            let b = base(f[3]);
            let imm5 = pick(f[4], room_up(b, scale, scale, 31));
            with_setup(
                set_reg(rn, b),
                t16(op | (bit(f[0], 4) << 11) | (imm5 << 6) | (rn << 3) | rt),
            )
        },
    },
    Template {
        name: "t16.ldst_reg",
        kind: Kind::Setup,
        // STR STRH STRB LDRSB LDR LDRH LDRB LDRSH (register)
        make: |f, _| {
            let opc = f[0] % 8;
            let bytes = [4, 2, 1, 1, 4, 2, 1, 2][opc as usize];
            let rn = lo(f[1]);
            let rm = lo_not(f[2], rn);
            let b = base(f[4]);
            let idx = pick(f[5], room_up(b, bytes, 1, 0xFFF));
            let mut setup = set_reg(rn, b);
            setup.push(mov16(false, rm, idx));
            with_setup(
                setup,
                t16(0x5000 | (opc << 9) | (rm << 6) | (rn << 3) | lo(f[3])),
            )
        },
    },
    Template {
        name: "t16.ldr_lit",
        kind: Kind::Plain,
        make: |f, _| one(t16(0x4800 | (lo(f[0]) << 8) | (f[1] & 0xFF))),
    },
    Template {
        name: "t16.ldst_sp",
        kind: Kind::Plain,
        make: |f, _| {
            one(t16(0x9000
                | (bit(f[0], 0) << 11)
                | (lo(f[1]) << 8)
                | (f[2] & 0xFF)))
        },
    },
    Template {
        name: "t16.push_pop",
        kind: Kind::Multi,
        make: |f, _| {
            let mut list = f[1] & 0xFF;
            one(if bit(f[0], 0) != 0 {
                if list == 0 {
                    list = 1 << lo(f[2]);
                }
                t16(0xBC00 | list)
            } else {
                let m = bit(f[0], 1);
                if list == 0 && m == 0 {
                    list = 1 << lo(f[2]);
                }
                t16(0xB400 | (m << 8) | list)
            })
        },
    },
    Template {
        name: "t16.ldm_stm",
        kind: Kind::Setup,
        make: |f, _| {
            let load = bit(f[0], 0);
            let rn = lo(f[1]);
            let mut list = f[2] & 0xFF;
            if load == 0 {
                // STM with the base in the list (other than first) stores an UNKNOWN value.
                list &= !(1 << rn);
            }
            if list == 0 {
                list = 1 << lo_not(f[3], rn);
            }
            let n = list.count_ones();
            let b = RAM_BASE | (f[4] & 0xFF0).min((WINDOW - 4 * n) & !0xF);
            with_setup(
                set_reg(rn, b),
                t16(0xC000 | (load << 11) | (rn << 8) | list),
            )
        },
    },
    Template {
        name: "t32.ldst_imm12",
        kind: Kind::Setup,
        make: |f, _| {
            let (l, sz, s) = LDST[(f[0] % 8) as usize];
            let rn = gp(f[1]);
            let b = base(f[3]);
            let imm12 = pick(f[4], room_up(b, 1 << sz, 1, 0xFFF));
            with_setup(
                set_reg(rn, b),
                t32(
                    0xF880 | (s << 8) | (sz << 5) | (l << 4) | rn,
                    (gp(f[2]) << 12) | imm12,
                ),
            )
        },
    },
    Template {
        name: "t32.ldst_imm8",
        kind: Kind::Setup,
        // negative offset, pre-indexed and post-indexed forms
        make: |f, _| {
            let (l, sz, s) = LDST[(f[0] % 8) as usize];
            let (p, w, u) = match f[1] % 3 {
                0 => (1, 0, 0),
                1 => (1, 1, bit(f[1], 8)),
                _ => (0, 1, bit(f[1], 8)),
            };
            let rn = gp(f[2]);
            let rt = if w != 0 {
                gp_not(f[3], &[rn])
            } else {
                gp(f[3])
            };
            let b = base(f[4]);
            let imm8 = match (p, u) {
                (0, _) => f[5] & 0xFF,
                (_, 1) => pick(f[5], room_up(b, 1 << sz, 1, 255)),
                _ => pick(f[5], room_down(b, 1, 255)),
            };
            with_setup(
                set_reg(rn, b),
                t32(
                    0xF800 | (s << 8) | (sz << 5) | (l << 4) | rn,
                    (rt << 12) | 0x800 | (p << 10) | (u << 9) | (w << 8) | imm8,
                ),
            )
        },
    },
    Template {
        name: "t32.ldst_unpriv",
        kind: Kind::Setup,
        // LDRT STRT LDRBT ... (privileged here, so an ordinary access)
        make: |f, _| {
            let (l, sz, s) = LDST[(f[0] % 8) as usize];
            let rn = gp(f[1]);
            let b = base(f[3]);
            let imm8 = pick(f[4], room_up(b, 1 << sz, 1, 255));
            with_setup(
                set_reg(rn, b),
                t32(
                    0xF800 | (s << 8) | (sz << 5) | (l << 4) | rn,
                    (gp(f[2]) << 12) | 0xE00 | imm8,
                ),
            )
        },
    },
    Template {
        name: "t32.ldst_reg",
        kind: Kind::Setup,
        make: |f, _| {
            let (l, sz, s) = LDST[(f[0] % 8) as usize];
            let rn = gp(f[1]);
            let rm = gp_not(f[2], &[rn]);
            let shift = f[3] & 3;
            let b = base(f[4]);
            let idx = pick(f[5], room_up(b, 1 << sz, 1, 0xFFF) >> shift);
            let mut setup = set_reg(rn, b);
            setup.push(mov16(false, rm, idx));
            with_setup(
                setup,
                t32(
                    0xF800 | (s << 8) | (sz << 5) | (l << 4) | rn,
                    (gp(f[3] >> 2) << 12) | (shift << 4) | rm,
                ),
            )
        },
    },
    Template {
        name: "t32.ldst_sp",
        kind: Kind::Plain,
        // LDR/STR (B/H/W, signed) Rt, [SP, #imm12]; SP stays within 0x2000_0640..0x2000_0BC0
        make: |f, _| {
            let (l, sz, s) = LDST[(f[0] % 8) as usize];
            one(t32(
                0xF88D | (s << 8) | (sz << 5) | (l << 4),
                (gp(f[1]) << 12) | (f[2] & 0x3FF),
            ))
        },
    },
    Template {
        name: "t32.ldr_lit",
        kind: Kind::Plain,
        // LDR LDRB LDRH LDRSB LDRSH (literal); code starts at 0x100, so at most 0x100 back
        make: |f, _| {
            let (sz, s) = [(2, 0), (0, 0), (1, 0), (0, 1), (1, 1)][(f[0] % 5) as usize];
            let u = bit(f[0], 4);
            let imm12 = if u != 0 {
                f[1] & 0xFFF
            } else {
                pick(f[1], 0x100)
            };
            one(t32(
                0xF81F | (s << 8) | (u << 7) | (sz << 5),
                (gp(f[2]) << 12) | imm12,
            ))
        },
    },
    Template {
        name: "t32.ldrd_strd",
        kind: Kind::Setup,
        make: |f, _| {
            let l = bit(f[0], 0);
            let (p, w) = [(1, 0), (1, 1), (0, 1)][((f[0] >> 1) % 3) as usize];
            let u = bit(f[1], 0);
            let rn = gp(f[2]);
            let avoid: &[u32] = if w != 0 { &[rn] } else { &[] };
            let rt = gp_not(f[3], avoid);
            // LDRD needs Rt != Rt2; STRD does not (a quarter of the stores use Rt2 == Rt).
            let rt2 = if l == 0 && (f[4] >> 8) & 3 == 0 {
                rt
            } else {
                gp_not(f[4], &[rt, if w != 0 { rn } else { rt }])
            };
            let b = base(f[5]);
            let imm8 = match (p, u) {
                (0, _) => (f[1] >> 1) & 0xFF,
                (_, 1) => pick(f[1] >> 1, room_up(b, 8, 4, 255)),
                _ => pick(f[1] >> 1, room_down(b, 4, 255)),
            };
            with_setup(
                set_reg(rn, b),
                t32(
                    0xE840 | (p << 8) | (u << 7) | (w << 5) | (l << 4) | rn,
                    (rt << 12) | (rt2 << 8) | imm8,
                ),
            )
        },
    },
    Template {
        name: "t32.ldrd_lit",
        kind: Kind::Plain,
        make: |f, _| {
            let u = bit(f[0], 0);
            let imm8 = if u != 0 {
                f[1] & 0xFF
            } else {
                pick(f[1], 0x40)
            };
            let rt = gp(f[2]);
            let rt2 = gp_not(f[3], &[rt]);
            one(t32(0xE95F | (u << 7), (rt << 12) | (rt2 << 8) | imm8))
        },
    },
    Template {
        name: "t32.ldm_stm",
        kind: Kind::Setup,
        // LDM STM LDMDB STMDB (.W), with and without writeback
        make: |f, _| {
            let (l, db, w) = (bit(f[0], 0), bit(f[0], 1), bit(f[0], 2));
            let rn = gp(f[1]);
            let list = reg_list(f[2], f[3], (w != 0).then_some(rn));
            let n = list.count_ones();
            let off = if db != 0 {
                (f[4] & 0xFF0).max((4 * n + 15) & !0xF)
            } else {
                (f[4] & 0xFF0).min((WINDOW - 4 * n) & !0xF)
            };
            let op = if db != 0 { 0xE900 } else { 0xE880 };
            with_setup(
                set_reg(rn, RAM_BASE | off),
                t32(op | (w << 5) | (l << 4) | rn, list),
            )
        },
    },
    Template {
        name: "t32.push_pop",
        kind: Kind::Multi,
        make: |f, _| {
            // POP.W / PUSH.W of a list, or of one register (LDR Rt, [SP], #4 / STR Rt, [SP, #-4]!)
            let list = reg_list(f[1], f[2], None);
            let rt = gp(f[3]);
            one(match f[0] % 4 {
                0 => t32(0xE8BD, list),
                1 => t32(0xE92D, list),
                2 => t32(0xF85D, (rt << 12) | 0xB04),
                _ => t32(0xF84D, (rt << 12) | 0xD04),
            })
        },
    },
    Template {
        name: "t32.exclusive",
        kind: Kind::Multi,
        // LDREX/STREX (B/H/W) and LDAEX/STLEX to the same address: back to back (succeeds),
        // with CLREX between (fails), or with an unrelated ALU instruction between.
        make: |f, _| {
            let size = f[0] % 3;
            let acq = bit(f[0], 4);
            let rn = gp(f[1]);
            let rt = gp_not(f[2], &[rn]);
            let rt2 = gp(f[3]);
            let rd = gp_not(f[4], &[rn, rt2]);
            let b = base(f[5]);
            let imm8 = if size == 2 && acq == 0 {
                pick(f[5] >> 12, room_up(b, 4, 4, 255))
            } else {
                0
            };
            let [ld, st] = exclusive(size, acq, rn, rt, rt2, rd, imm8);
            let mut body = vec![ld];
            match (f[0] >> 8) % 3 {
                0 => {}
                1 => body.push(t32(0xF3BF, 0x8F2F)),
                _ => {
                    let r = if rn < 8 {
                        lo_not(f[3] >> 8, rn)
                    } else {
                        lo(f[3] >> 8)
                    };
                    body.push(t16(0x3000 | (r << 8) | ((f[4] >> 8) & 0xFF)));
                }
            }
            body.push(st);
            Snip {
                setup: set_reg(rn, b),
                body,
            }
        },
    },
    Template {
        name: "t32.lda_stl",
        kind: Kind::Setup,
        make: |f, _| {
            let rn = gp(f[1]);
            let op = if bit(f[0], 4) != 0 { 0xE8D0 } else { 0xE8C0 };
            with_setup(
                set_reg(rn, base(f[3])),
                t32(op | rn, (gp(f[2]) << 12) | 0xF0F | ((8 | (f[0] % 3)) << 4)),
            )
        },
    },
    Template {
        name: "t32.preload",
        kind: Kind::Plain,
        // PLD/PLI [Rn, #imm12], PLD [Rn, Rm, LSL #n]: hints, no architectural effect
        make: |f, _| {
            one(match f[0] % 3 {
                0 => t32(0xF890 | gp(f[1]), 0xF000 | (f[2] & 0xFFF)),
                1 => t32(0xF990 | gp(f[1]), 0xF000 | (f[2] & 0xFFF)),
                _ => t32(0xF810 | gp(f[1]), 0xF000 | ((f[2] & 3) << 4) | gp(f[3])),
            })
        },
    },
    Template {
        name: "t32.clrex",
        kind: Kind::Plain,
        make: |_, _| one(t32(0xF3BF, 0x8F2F)),
    },
    // A little data processing so loaded values feed later stores.
    Template {
        name: "mem.imm8",
        kind: Kind::Plain,
        make: t16_imm8,
    },
    Template {
        name: "mem.dp_reg",
        kind: Kind::Plain,
        make: t16_dp_reg,
    },
];

// ---------------------------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Pick {
    t: usize,
    f: Fields,
}

#[derive(Clone, Debug)]
enum Item {
    One(Pick),
    /// An IT block: `ops[0]` may need setup (hoisted before the IT), the rest are `Plain`.
    It {
        firstcond: u32,
        elses: u32,
        ops: Vec<Pick>,
    },
}

#[derive(Clone, Debug)]
struct Case {
    r: [u32; 13],
    lr: u32,
    /// N Z C V Q in bits 4..0.
    nzcvq: u8,
    ge: u8,
    /// Seeds the RAM window and the code-region pool.
    seed: u64,
    items: Vec<Item>,
}

/// Register values: uniform, plus the edges arithmetic and saturation care about.
fn value() -> impl Strategy<Value = u32> {
    const EDGES: &[u32] = &[
        0,
        1,
        2,
        0x7F,
        0x80,
        0xFF,
        0x7FFF,
        0x8000,
        0xFFFF,
        0x7FFF_FFFF,
        0x8000_0000,
        0x8000_0001,
        0xFFFF_FFFF,
        0xFFFF_FFFE,
        0xFFFF_8000,
        0xFFFF_FF80,
        0x7FFF_8000,
        0x8000_7FFF,
    ];
    prop_oneof![
        8 => any::<u32>(),
        3 => prop::sample::select(EDGES),
        2 => 0u32..=40,
        1 => any::<i16>().prop_map(|v| i32::from(v) as u32),
        1 => any::<u8>().prop_map(|b| u32::from(b) * 0x0101_0101),
        1 => any::<u16>().prop_map(|h| u32::from(h) * 0x0001_0001),
    ]
}

fn case_strategy(n: usize) -> impl Strategy<Value = Case> {
    let pick = (0..n, any::<Fields>()).prop_map(|(t, f)| Pick { t, f });
    let item = prop_oneof![
        5 => pick.clone().prop_map(Item::One),
        2 => (0u32..15, any::<u32>(), prop::collection::vec(pick, 1..=4))
            .prop_map(|(firstcond, elses, ops)| Item::It { firstcond, elses, ops }),
    ];
    (
        prop::array::uniform13(value()),
        value(),
        0u8..32,
        0u8..16,
        any::<u64>(),
        prop::collection::vec(item, 1..=8),
    )
        .prop_map(|(r, lr, nzcvq, ge, seed, items)| Case {
            r,
            lr,
            nzcvq,
            ge,
            seed,
            items,
        })
}

/// One generated instruction with the template it came from.
struct Line {
    enc: Enc,
    template: &'static str,
}

/// Lay out the program for `items`.
fn build(table: &[&'static Template], items: &[Item]) -> Vec<Line> {
    let mut out = Vec::new();
    let mut emit = |encs: Vec<Enc>, template| {
        out.extend(encs.into_iter().map(|enc| Line { enc, template }));
    };
    let leaders: Vec<_> = table
        .iter()
        .copied()
        .filter(|t| t.kind != Kind::Multi)
        .collect();
    let plain: Vec<_> = table
        .iter()
        .copied()
        .filter(|t| t.kind == Kind::Plain)
        .collect();
    for item in items {
        match item {
            Item::One(p) => {
                let t = table[p.t % table.len()];
                let s = (t.make)(&p.f, false);
                emit(s.setup, t.name);
                emit(s.body, t.name);
            }
            Item::It {
                firstcond,
                elses,
                ops,
            } => {
                let mut body = Vec::new();
                for (i, p) in ops.iter().enumerate() {
                    let pool = if i == 0 { &leaders } else { &plain };
                    let t = pool[p.t % pool.len()];
                    let s = (t.make)(&p.f, true);
                    assert_eq!(s.body.len(), 1, "{}: IT-block template", t.name);
                    emit(s.setup, t.name);
                    body.push((s.body[0], t.name));
                }
                emit(vec![it_instr(*firstcond, *elses, ops.len())], "it");
                for (enc, name) in body {
                    emit(vec![enc], name);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Running both models
// ---------------------------------------------------------------------------------------------

/// The compared architectural state after a run.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    r: [u32; 13],
    lr: u32,
    sp: u32,
    nzcvq: u8,
    ge: u8,
    ram: Vec<u8>,
    /// Why the run did not end cleanly at the end of the program, if it did not.
    trouble: Option<String>,
}

/// `splitmix64`, for the memory contents.
fn random_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n + 8);
    while out.len() < n {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(n);
    out
}

/// Initial memory: the code region (random pool with the program at `CODE_ADDR`) and the RAM
/// window.
struct Memory {
    code: Vec<u8>,
    ram: Vec<u8>,
    end: u32,
}

fn memory(case: &Case, prog: &[Line]) -> Memory {
    let mut code = random_bytes(case.seed, POOL);
    let mut bytes = Vec::new();
    for l in prog {
        l.enc.push_bytes(&mut bytes);
    }
    let start = CODE_ADDR as usize;
    code[start..start + bytes.len()].copy_from_slice(&bytes);
    Memory {
        code,
        ram: random_bytes(!case.seed, WINDOW as usize),
        end: CODE_ADDR + bytes.len() as u32,
    }
}

fn run_core(case: &Case, mem: &Memory) -> State {
    let mut bus = FlatBus::new();
    // The decoded-instruction cache is on, so stores into the code window exercise it too.
    bus.cache = true;
    bus.code[..mem.code.len()].copy_from_slice(&mem.code);
    bus.ram[..mem.ram.len()].copy_from_slice(&mem.ram);
    let mut cpu = Cpu::new();
    cpu.regs.pc = CODE_ADDR;
    cpu.regs.r = case.r;
    cpu.regs.lr = case.lr;
    cpu.regs.msp.s = INITIAL_SP;
    let x = &mut cpu.regs.xpsr;
    (x.n, x.z, x.c, x.v, x.q) = (
        case.nzcvq & 16 != 0,
        case.nzcvq & 8 != 0,
        case.nzcvq & 4 != 0,
        case.nzcvq & 2 != 0,
        case.nzcvq & 1 != 0,
    );
    x.ge = case.ge;
    let mut trouble = None;
    for _ in 0..MAX_STEPS {
        if cpu.regs.pc == mem.end || cpu.regs.xpsr.exception != 0 {
            break;
        }
        if let r @ (StepResult::Lockup | StepResult::Bkpt(_) | StepResult::Sleep(_)) =
            cpu.step(&mut bus)
        {
            trouble = Some(format!("{r:?} at {:#x}", cpu.regs.pc));
            break;
        }
    }
    let x = cpu.regs.xpsr;
    if trouble.is_none() && x.exception != 0 {
        trouble = Some(format!(
            "exception {} taken (CFSR {:#x})",
            x.exception,
            cpu.ppb_read32(0xE000_ED28)
        ));
    } else if trouble.is_none() && cpu.regs.pc != mem.end {
        trouble = Some(format!("stopped at {:#x}", cpu.regs.pc));
    } else if trouble.is_none() && x.it != 0 {
        trouble = Some(format!("ITSTATE {:#x} at the end", x.it));
    }
    State {
        r: cpu.regs.r,
        lr: cpu.regs.lr,
        sp: cpu.regs.sp(),
        nzcvq: (u8::from(x.n) << 4)
            | (u8::from(x.z) << 3)
            | (u8::from(x.c) << 2)
            | (u8::from(x.v) << 1)
            | u8::from(x.q),
        ge: x.ge,
        ram: bus.ram[..WINDOW as usize].to_vec(),
        trouble,
    }
}

const UC_R: [RegisterARM; 13] = [
    RegisterARM::R0,
    RegisterARM::R1,
    RegisterARM::R2,
    RegisterARM::R3,
    RegisterARM::R4,
    RegisterARM::R5,
    RegisterARM::R6,
    RegisterARM::R7,
    RegisterARM::R8,
    RegisterARM::R9,
    RegisterARM::R10,
    RegisterARM::R11,
    RegisterARM::R12,
];

/// Serializes all use of Unicorn. With the four tests creating, running and dropping instances
/// on parallel threads, the process crashed (STATUS_ACCESS_VIOLATION) after some 10^5 cases;
/// each test alone ran 300 000 cases cleanly.
static UNICORN: Mutex<()> = Mutex::new(());

fn run_unicorn(case: &Case, mem: &Memory) -> State {
    let _serial = UNICORN.lock().unwrap_or_else(PoisonError::into_inner);
    let mut uc = Unicorn::new(Arch::ARM, Mode::THUMB | Mode::MCLASS).expect("unicorn");
    uc.ctl_set_cpu_model(ArmCpuModel::CORTEX_M33 as i32)
        .expect("cortex-m33");
    uc.mem_map(0, 0x1_0000, Prot::ALL).expect("map code");
    uc.mem_map(u64::from(RAM_BASE), u64::from(WINDOW), Prot::ALL)
        .expect("map ram");
    uc.mem_write(0, &mem.code).expect("write code");
    uc.mem_write(u64::from(RAM_BASE), &mem.ram)
        .expect("write ram");
    for (reg, v) in UC_R.iter().zip(case.r) {
        uc.reg_write(*reg, u64::from(v)).expect("reg");
    }
    uc.reg_write(RegisterARM::LR, u64::from(case.lr))
        .expect("lr");
    uc.reg_write(RegisterARM::SP, u64::from(INITIAL_SP))
        .expect("sp");
    let apsr = (u32::from(case.nzcvq) << 27) | (u32::from(case.ge) << 16);
    uc.reg_write(RegisterARM::APSR_NZCVQG, u64::from(apsr))
        .expect("apsr");
    let res = uc.emu_start(u64::from(CODE_ADDR | 1), u64::from(mem.end), 0, MAX_STEPS);
    let rd = |reg| uc.reg_read(reg).expect("reg") as u32;
    let mut r = [0; 13];
    for (out, reg) in r.iter_mut().zip(UC_R) {
        *out = rd(reg);
    }
    let apsr = rd(RegisterARM::APSR);
    let (pc, ipsr, epsr) = (
        rd(RegisterARM::PC),
        rd(RegisterARM::IPSR),
        rd(RegisterARM::EPSR),
    );
    let it = ((epsr >> 25) & 3) | (((epsr >> 10) & 0x3F) << 2);
    let trouble = if let Err(e) = res {
        Some(format!("emu_start: {e:?} at {pc:#x}"))
    } else if ipsr != 0 {
        Some(format!("exception {ipsr} taken"))
    } else if pc != mem.end {
        Some(format!("stopped at {pc:#x}"))
    } else if it != 0 {
        Some(format!("ITSTATE {it:#x} at the end"))
    } else {
        None
    };
    State {
        r,
        lr: rd(RegisterARM::LR),
        sp: rd(RegisterARM::SP),
        nzcvq: (apsr >> 27) as u8,
        ge: ((apsr >> 16) & 0xF) as u8,
        ram: uc
            .mem_read_as_vec(u64::from(RAM_BASE), WINDOW as usize)
            .expect("read ram"),
        trouble,
    }
}

// ---------------------------------------------------------------------------------------------
// Checking and reporting
// ---------------------------------------------------------------------------------------------

fn listing(prog: &[Line]) -> String {
    let mut s = String::new();
    let mut addr = CODE_ADDR;
    for l in prog {
        let instr = l.enc.decode();
        let len = l.enc.len();
        let _ = writeln!(
            s,
            "  {addr:#06x}: {}  {:<36} ; {}",
            l.enc.hex(),
            disasm(&instr, addr, len as u8),
            l.template
        );
        addr += len;
    }
    s
}

fn flags(nzcvq: u8, ge: u8) -> String {
    let f: String = "NZCVQ"
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if nzcvq >> (4 - i) & 1 != 0 {
                c
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    format!("{f} GE={ge:04b}")
}

fn regs_line(r: &[u32; 13], lr: u32, sp: u32) -> String {
    let mut s = String::new();
    for (i, v) in r.iter().enumerate() {
        let _ = write!(s, "r{i}={v:08x} ");
    }
    let _ = write!(s, "lr={lr:08x} sp={sp:08x}");
    s
}

fn report(name: &str, prog: &[Line], case: &Case, core: &State, uc: &State) -> String {
    let mut s = format!("difftest {name}: our core and Unicorn disagree\nprogram:\n");
    s += &listing(prog);
    let _ = writeln!(
        s,
        "initial: {}\n         {}",
        regs_line(&case.r, case.lr, INITIAL_SP),
        flags(case.nzcvq, case.ge)
    );
    for (who, st) in [("core   ", core), ("unicorn", uc)] {
        let _ = writeln!(
            s,
            "{who}: {}\n         {}{}",
            regs_line(&st.r, st.lr, st.sp),
            flags(st.nzcvq, st.ge),
            st.trouble
                .as_ref()
                .map_or(String::new(), |t| format!("  !! {t}"))
        );
    }
    let mut diffs = Vec::new();
    for i in 0..13 {
        if core.r[i] != uc.r[i] {
            diffs.push(format!("r{i}"));
        }
    }
    for (n, a, b) in [("lr", core.lr, uc.lr), ("sp", core.sp, uc.sp)] {
        if a != b {
            diffs.push(n.into());
        }
    }
    if core.nzcvq != uc.nzcvq {
        diffs.push("nzcvq".into());
    }
    if core.ge != uc.ge {
        diffs.push("ge".into());
    }
    let ram: Vec<_> = (0..WINDOW as usize)
        .filter(|&i| core.ram[i] != uc.ram[i])
        .collect();
    if !ram.is_empty() {
        diffs.push(format!("{} RAM bytes", ram.len()));
    }
    let _ = writeln!(s, "differ: {}", diffs.join(", "));
    for &i in ram.iter().take(16) {
        let _ = writeln!(
            s,
            "  [{:#010x}] core {:02x} unicorn {:02x}",
            RAM_BASE as usize + i,
            core.ram[i],
            uc.ram[i]
        );
    }
    s
}

fn check(
    name: &str,
    table: &[&'static Template],
    case: &Case,
    uses: &RefCell<BTreeMap<&'static str, usize>>,
) -> Result<(), TestCaseError> {
    let prog = build(table, &case.items);
    for l in &prog {
        *uses.borrow_mut().entry(l.template).or_default() += 1;
    }
    for l in &prog {
        if let i @ (Instr::Undefined { .. } | Instr::Coprocessor { .. }) = l.enc.decode() {
            return Err(TestCaseError::fail(format!(
                "generator bug: template {} produced {} ({i:?})\n{}",
                l.template,
                l.enc.hex(),
                listing(&prog)
            )));
        }
    }
    let mem = memory(case, &prog);
    let core = run_core(case, &mem);
    let uc = run_unicorn(case, &mem);
    if core == uc && core.trouble.is_none() {
        Ok(())
    } else {
        Err(TestCaseError::fail(report(name, &prog, case, &core, &uc)))
    }
}

fn difftest(name: &str, templates: &'static [Template]) {
    let table: Vec<&'static Template> = templates
        .iter()
        .filter(|t| !KNOWN_UNICORN_DIVERGENCES.iter().any(|(n, _)| *n == t.name))
        .collect();
    let cases = std::env::var("DIFFTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    let uses = RefCell::new(BTreeMap::new());
    if let Err(e) = runner.run(&case_strategy(table.len()), |case| {
        check(name, &table, &case, &uses)
    }) {
        panic!("{e}");
    }
    let uses = uses.into_inner();
    println!("difftest {name}: {cases} cases, instructions per template: {uses:?}");
    // Every template must have been exercised (a 256-case run draws each hundreds of times).
    if cases >= 64 {
        for t in &table {
            assert!(
                uses.contains_key(t.name),
                "template {} never generated",
                t.name
            );
        }
    }
}

#[test]
fn difftest_alu_t16() {
    difftest("alu_t16", ALU16);
}

#[test]
fn difftest_t32() {
    difftest("t32", T32);
}

#[test]
fn difftest_dsp() {
    difftest("dsp", DSP);
}

#[test]
fn difftest_mem() {
    difftest("mem", MEM);
}
