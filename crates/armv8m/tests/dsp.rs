//! DSP extension: parallel add/sub, saturating arithmetic, DSP multiplies, USAD8,
//! SSAT16/USAT16 and SXTB16/UXTB16. Encodings and disassembly come from arm-none-eabi-as/objdump.

mod common;

use armv8m::{Cpu, Instr, StepResult, decode32, disasm};
use common::{FlatBus, cpu_with_code, run};

fn dis(cpu: &Cpu, bus: &mut FlatBus) -> String {
    let (instr, len) = cpu.decode_at(bus, cpu.regs.pc).expect("fetch");
    disasm(&instr, cpu.regs.pc, len)
}

/// Run one 32-bit instruction after `setup`; returns the CPU.
fn exec1(code: [u16; 2], setup: impl FnOnce(&mut Cpu)) -> Cpu {
    let (mut cpu, mut bus) = cpu_with_code(&code);
    setup(&mut cpu);
    assert!(matches!(cpu.step(&mut bus), StepResult::Executed { .. }));
    assert_eq!(cpu.regs.pc, 0x104);
    cpu
}

/// Set r1/r2/r3 then run `code`.
fn with(code: [u16; 2], regs: [u32; 3]) -> Cpu {
    exec1(code, |c| {
        c.regs.r[1] = regs[0];
        c.regs.r[2] = regs[1];
        c.regs.r[3] = regs[2];
    })
}

#[test]
fn disasm_matches_objdump() {
    for (code, want) in [
        ([0xFA91, 0xF002], "sadd16 r0, r1, r2"),
        ([0xFAA1, 0xF002], "sasx r0, r1, r2"),
        ([0xFAE1, 0xF002], "ssax r0, r1, r2"),
        ([0xFAD1, 0xF002], "ssub16 r0, r1, r2"),
        ([0xFA81, 0xF002], "sadd8 r0, r1, r2"),
        ([0xFAC1, 0xF002], "ssub8 r0, r1, r2"),
        ([0xFA91, 0xF012], "qadd16 r0, r1, r2"),
        ([0xFAA1, 0xF012], "qasx r0, r1, r2"),
        ([0xFAE1, 0xF012], "qsax r0, r1, r2"),
        ([0xFAD1, 0xF012], "qsub16 r0, r1, r2"),
        ([0xFA81, 0xF012], "qadd8 r0, r1, r2"),
        ([0xFAC1, 0xF012], "qsub8 r0, r1, r2"),
        ([0xFA91, 0xF022], "shadd16 r0, r1, r2"),
        ([0xFAA1, 0xF022], "shasx r0, r1, r2"),
        ([0xFAE1, 0xF022], "shsax r0, r1, r2"),
        ([0xFAD1, 0xF022], "shsub16 r0, r1, r2"),
        ([0xFA81, 0xF022], "shadd8 r0, r1, r2"),
        ([0xFAC1, 0xF022], "shsub8 r0, r1, r2"),
        ([0xFA91, 0xF042], "uadd16 r0, r1, r2"),
        ([0xFAA1, 0xF042], "uasx r0, r1, r2"),
        ([0xFAE1, 0xF042], "usax r0, r1, r2"),
        ([0xFAD1, 0xF042], "usub16 r0, r1, r2"),
        ([0xFA81, 0xF042], "uadd8 r0, r1, r2"),
        ([0xFAC1, 0xF042], "usub8 r0, r1, r2"),
        ([0xFA91, 0xF052], "uqadd16 r0, r1, r2"),
        ([0xFAA1, 0xF052], "uqasx r0, r1, r2"),
        ([0xFAE1, 0xF052], "uqsax r0, r1, r2"),
        ([0xFAD1, 0xF052], "uqsub16 r0, r1, r2"),
        ([0xFA81, 0xF052], "uqadd8 r0, r1, r2"),
        ([0xFAC1, 0xF052], "uqsub8 r0, r1, r2"),
        ([0xFA91, 0xF062], "uhadd16 r0, r1, r2"),
        ([0xFAA1, 0xF062], "uhasx r0, r1, r2"),
        ([0xFAE1, 0xF062], "uhsax r0, r1, r2"),
        ([0xFAD1, 0xF062], "uhsub16 r0, r1, r2"),
        ([0xFA81, 0xF062], "uhadd8 r0, r1, r2"),
        ([0xFAC1, 0xF062], "uhsub8 r0, r1, r2"),
        ([0xFA82, 0xF081], "qadd r0, r1, r2"),
        ([0xFA82, 0xF091], "qdadd r0, r1, r2"),
        ([0xFA82, 0xF0A1], "qsub r0, r1, r2"),
        ([0xFA82, 0xF0B1], "qdsub r0, r1, r2"),
        ([0xFB11, 0xF002], "smulbb r0, r1, r2"),
        ([0xFB11, 0xF012], "smulbt r0, r1, r2"),
        ([0xFB11, 0xF022], "smultb r0, r1, r2"),
        ([0xFB11, 0xF032], "smultt r0, r1, r2"),
        ([0xFB11, 0x3002], "smlabb r0, r1, r2, r3"),
        ([0xFB11, 0x3032], "smlatt r0, r1, r2, r3"),
        ([0xFB31, 0xF002], "smulwb r0, r1, r2"),
        ([0xFB31, 0xF012], "smulwt r0, r1, r2"),
        ([0xFB31, 0x3002], "smlawb r0, r1, r2, r3"),
        ([0xFB31, 0x3012], "smlawt r0, r1, r2, r3"),
        ([0xFBC2, 0x0183], "smlalbb r0, r1, r2, r3"),
        ([0xFBC2, 0x0193], "smlalbt r0, r1, r2, r3"),
        ([0xFBC2, 0x01A3], "smlaltb r0, r1, r2, r3"),
        ([0xFBC2, 0x01B3], "smlaltt r0, r1, r2, r3"),
        ([0xFB21, 0xF002], "smuad r0, r1, r2"),
        ([0xFB21, 0xF012], "smuadx r0, r1, r2"),
        ([0xFB21, 0x3002], "smlad r0, r1, r2, r3"),
        ([0xFB21, 0x3012], "smladx r0, r1, r2, r3"),
        ([0xFB41, 0xF002], "smusd r0, r1, r2"),
        ([0xFB41, 0xF012], "smusdx r0, r1, r2"),
        ([0xFB41, 0x3002], "smlsd r0, r1, r2, r3"),
        ([0xFB41, 0x3012], "smlsdx r0, r1, r2, r3"),
        ([0xFBC2, 0x01C3], "smlald r0, r1, r2, r3"),
        ([0xFBC2, 0x01D3], "smlaldx r0, r1, r2, r3"),
        ([0xFBD2, 0x01C3], "smlsld r0, r1, r2, r3"),
        ([0xFBD2, 0x01D3], "smlsldx r0, r1, r2, r3"),
        ([0xFB51, 0xF002], "smmul r0, r1, r2"),
        ([0xFB51, 0xF012], "smmulr r0, r1, r2"),
        ([0xFB51, 0x3002], "smmla r0, r1, r2, r3"),
        ([0xFB51, 0x3012], "smmlar r0, r1, r2, r3"),
        ([0xFB61, 0x3002], "smmls r0, r1, r2, r3"),
        ([0xFB61, 0x3012], "smmlsr r0, r1, r2, r3"),
        ([0xFB71, 0xF002], "usad8 r0, r1, r2"),
        ([0xFB71, 0x3002], "usada8 r0, r1, r2, r3"),
        ([0xF321, 0x0007], "ssat16 r0, #8, r1"),
        ([0xF3A1, 0x0008], "usat16 r0, #8, r1"),
        ([0xF321, 0x000F], "ssat16 r0, #16, r1"),
        ([0xF3A1, 0x0000], "usat16 r0, #0, r1"),
        ([0xFA2F, 0xF081], "sxtb16 r0, r1"),
        ([0xFA2F, 0xF091], "sxtb16 r0, r1, ror #8"),
        ([0xFA3F, 0xF0A1], "uxtb16 r0, r1, ror #16"),
        ([0xFA21, 0xF082], "sxtab16 r0, r1, r2"),
        ([0xFA31, 0xF0B2], "uxtab16 r0, r1, r2, ror #24"),
        ([0xFAA1, 0xF082], "sel r0, r1, r2"),
    ] {
        let (cpu, mut bus) = cpu_with_code(&code);
        assert_eq!(dis(&cpu, &mut bus), want, "{code:04x?}");
    }
}

#[test]
fn sp_pc_operands_are_undefined() {
    for (h1, h2) in [
        (0xFA91, 0xFD02), // sadd16 rd = sp
        (0xFA9D, 0xF002), // sadd16 rn = sp
        (0xFA91, 0xF00F), // sadd16 rm = pc
        (0xFB21, 0xD002), // smlad ra = sp
        (0xFB11, 0xFF02), // smulbb rd = pc
        (0xFA82, 0xFF81), // qadd rd = pc
        (0xFBC2, 0xD183), // smlalbb rdlo = sp
        (0xFBC2, 0x0083), // smlalbb rdlo == rdhi
        (0xF32D, 0x0007), // ssat16 rn = sp
        (0xFA2D, 0xF081), // sxtb16 style with rn = sp
        (0xFA21, 0xFD82), // sxtab16 rd = sp
        (0xFB51, 0xFF02), // smmul rd = pc
        (0xFB71, 0xD002), // usada8 ra = sp
        (0xFB61, 0xF002), // smmls ra = pc
        (0xFB61, 0xF012), // smmlsr ra = pc
    ] {
        assert!(
            matches!(decode32(h1, h2), Instr::Undefined { .. }),
            "{h1:04x} {h2:04x}"
        );
    }
    // R12 is a valid operand everywhere.
    assert!(!matches!(decode32(0xFBC2, 0x0C83), Instr::Undefined { .. }));
}

#[test]
fn sadd16_sets_ge() {
    // sadd16 r0, r1, r2: low 1+1 = 2 (GE), high -2+1 = -1 (no GE).
    let cpu = with([0xFA91, 0xF002], [0xFFFE_0001, 0x0001_0001, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFFF_0002);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
    // -1 + 1 = 0 counts as >= 0 on both halves.
    let cpu = with([0xFA91, 0xF002], [0xFFFF_FFFF, 0x0001_0001, 0]);
    assert_eq!(cpu.regs.r[0], 0);
    assert_eq!(cpu.regs.xpsr.ge, 0b1111);
    // GE is rewritten, not OR-ed.
    let cpu = exec1([0xFA91, 0xF002], |c| {
        c.regs.xpsr.ge = 0b1111;
        c.regs.r[1] = 0x8000_8000;
        c.regs.r[2] = 0x0001_0001;
    });
    assert_eq!(cpu.regs.xpsr.ge, 0);
}

#[test]
fn unsigned_parallel_ge_and_results() {
    // uadd8: 0xFF + 1 carries out of byte 3 only.
    let cpu = with([0xFA81, 0xF042], [0xFF01_0203, 0x0101_0101, 0]);
    assert_eq!(cpu.regs.r[0], 0x0002_0304);
    assert_eq!(cpu.regs.xpsr.ge, 0b1000);
    // usub8: GE where no borrow.
    let cpu = with([0xFAC1, 0xF042], [0x0510_0A20, 0x0620_0A10, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFF0_0010);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
    // uasx: low = lo - hi', high = hi + lo'.
    let cpu = with([0xFAA1, 0xF042], [0x0005_0003, 0x0002_0004, 0]);
    assert_eq!(cpu.regs.r[0], 0x0009_0001);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
    // usax: low sum 0xFFFF + 0x0002 carries (GE set), high 5 - 0 does not borrow (GE set).
    let cpu = with([0xFAE1, 0xF042], [0x0005_FFFF, 0x0002_0000, 0]);
    assert_eq!(cpu.regs.r[0], 0x0005_0001);
    assert_eq!(cpu.regs.xpsr.ge, 0b1111);
    // usax: high 3 - 4 borrows (GE clear).
    let cpu = with([0xFAE1, 0xF042], [0x0003_FFFF, 0x0002_0004, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFFF_0001);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
}

#[test]
fn signed_asx_sax_sub() {
    // sasx r0, r1, r2
    let cpu = with([0xFAA1, 0xF002], [0x0005_0003, 0x0002_0004, 0]);
    assert_eq!(cpu.regs.r[0], 0x0009_0001);
    assert_eq!(cpu.regs.xpsr.ge, 0b1111);
    // ssax r0, r1, r2: low = 2 + 3, high = 1 - 4 = -3.
    let cpu = with([0xFAE1, 0xF002], [0x0001_0002, 0x0003_0004, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFFD_0005);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
    // ssub16: GE on non-negative differences.
    let cpu = with([0xFAD1, 0xF002], [0x0001_0005, 0x0002_0003, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFFF_0002);
    assert_eq!(cpu.regs.xpsr.ge, 0b0011);
    // ssub8
    let cpu = with([0xFAC1, 0xF002], [0x0102_0304, 0x0201_0403, 0]);
    assert_eq!(cpu.regs.r[0], 0xFF01_FF01);
    assert_eq!(cpu.regs.xpsr.ge, 0b0101);
}

#[test]
fn saturating_and_halving_parallel_leave_ge_and_q_alone() {
    // qadd16: low -32768 + -1 saturates, high 32767 + 1 saturates.
    let cpu = with([0xFA91, 0xF012], [0x7FFF_8000, 0x0001_FFFF, 0]);
    assert_eq!(cpu.regs.r[0], 0x7FFF_8000);
    assert!(!cpu.regs.xpsr.q);
    assert_eq!(cpu.regs.xpsr.ge, 0);
    // qsub8: -128 - 1 saturates to -128; 127 - (-1) saturates to 127.
    let cpu = with([0xFAC1, 0xF012], [0x7F80_0000, 0xFF01_0000, 0]);
    assert_eq!(cpu.regs.r[0], 0x7F80_0000);
    // shsub16: (3 - 6) >> 1 = -2, (4 - 1) >> 1 = 1.
    let cpu = with([0xFAD1, 0xF022], [0x0004_0003, 0x0001_0006, 0]);
    assert_eq!(cpu.regs.r[0], 0x0001_FFFE);
    // shadd8 is an arithmetic halve: (-4 + -8) >> 1 = -6.
    let cpu = with([0xFA81, 0xF022], [0x0000_00FC, 0x0000_00F8, 0]);
    assert_eq!(cpu.regs.r[0], 0x0000_00FA);
    // uhadd8
    let cpu = with([0xFA81, 0xF062], [0x0204_0608, 0x0002_0406, 0]);
    assert_eq!(cpu.regs.r[0], 0x0103_0507);
    // uhsub16: (1 - 2) >> 1 keeps bits 16..1 of the difference.
    let cpu = with([0xFAD1, 0xF062], [0x0000_0001, 0x0000_0002, 0]);
    assert_eq!(cpu.regs.r[0], 0x0000_FFFF);
    // uqadd16 saturates at 0xFFFF.
    let cpu = with([0xFA91, 0xF052], [0xFFF0_0001, 0x0020_0001, 0]);
    assert_eq!(cpu.regs.r[0], 0xFFFF_0002);
}

#[test]
fn uqsub8_saturates() {
    let cpu = with([0xFAC1, 0xF052], [0x1020_3040, 0x2010_4030, 0]);
    assert_eq!(cpu.regs.r[0], 0x0010_0010);
    assert!(!cpu.regs.xpsr.q);
    assert_eq!(cpu.regs.xpsr.ge, 0);
}

#[test]
fn qadd_sets_q_on_overflow() {
    // qadd r0, r1, r2
    let cpu = with([0xFA82, 0xF081], [0x7FFF_FFFF, 1, 0]);
    assert_eq!(cpu.regs.r[0], 0x7FFF_FFFF);
    assert!(cpu.regs.xpsr.q);
    let cpu = with([0xFA82, 0xF081], [5, 7, 0]);
    assert_eq!(cpu.regs.r[0], 12);
    assert!(!cpu.regs.xpsr.q);
    // qsub underflow
    let cpu = with([0xFA82, 0xF0A1], [0x8000_0000, 1, 0]);
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    assert!(cpu.regs.xpsr.q);
    // qdadd r0, r1, r2 = r1 + 2 * r2
    let cpu = with([0xFA82, 0xF091], [5, 3, 0]);
    assert_eq!(cpu.regs.r[0], 11);
    assert!(!cpu.regs.xpsr.q);
    // qdsub: doubling saturates (sets Q) and the subtraction uses the saturated value.
    let cpu = with([0xFA82, 0xF0B1], [0, 0x4000_0000, 0]);
    assert_eq!(cpu.regs.r[0], 0x8000_0001);
    assert!(cpu.regs.xpsr.q);
    // Q is sticky.
    let cpu = exec1([0xFA82, 0xF081], |c| {
        c.regs.xpsr.q = true;
        c.regs.r[1] = 1;
        c.regs.r[2] = 2;
    });
    assert!(cpu.regs.xpsr.q);
}

#[test]
fn halfword_multiplies() {
    let (r1, r2) = (0xFFFF_0003, 0x0005_FFFE);
    // smulbb / smulbt / smultb / smultt
    assert_eq!(
        with([0xFB11, 0xF002], [r1, r2, 0]).regs.r[0],
        (-6i32) as u32
    );
    assert_eq!(with([0xFB11, 0xF012], [r1, r2, 0]).regs.r[0], 15);
    assert_eq!(with([0xFB11, 0xF022], [r1, r2, 0]).regs.r[0], 2);
    assert_eq!(
        with([0xFB11, 0xF032], [r1, r2, 0]).regs.r[0],
        (-5i32) as u32
    );
    // smlabb adds with Q on signed overflow (no saturation).
    let cpu = with([0xFB11, 0x3002], [2, 3, 10]);
    assert_eq!(cpu.regs.r[0], 16);
    assert!(!cpu.regs.xpsr.q);
    let cpu = with([0xFB11, 0x3002], [2, 3, 0x7FFF_FFFF]);
    assert_eq!(cpu.regs.r[0], 0x8000_0005);
    assert!(cpu.regs.xpsr.q);
    // smulwb / smulwt / smlawt
    let (r1, r2) = (0x0002_0000, 0x0007_FFFF);
    assert_eq!(
        with([0xFB31, 0xF002], [r1, r2, 0]).regs.r[0],
        (-2i32) as u32
    );
    assert_eq!(with([0xFB31, 0xF012], [r1, r2, 0]).regs.r[0], 14);
    assert_eq!(with([0xFB31, 0x3012], [r1, r2, 100]).regs.r[0], 114);
    // smlalbb r0, r1, r2, r3: RdLo = r0, RdHi = r1, product -2 * 5 = -10 added to 5.
    let cpu = exec1([0xFBC2, 0x0183], |c| {
        c.regs.r[0] = 5;
        c.regs.r[1] = 0;
        c.regs.r[2] = 0x0000_FFFE;
        c.regs.r[3] = 5;
    });
    assert_eq!((cpu.regs.r[1], cpu.regs.r[0]), (0xFFFF_FFFF, 0xFFFF_FFFB));
    // smlaltt: top halves, carry into the high word.
    let cpu = exec1([0xFBC2, 0x01B3], |c| {
        c.regs.r[0] = 0xFFFF_FFFF;
        c.regs.r[1] = 0;
        c.regs.r[2] = 0x0002_0000;
        c.regs.r[3] = 0x0003_0000;
    });
    assert_eq!((cpu.regs.r[1], cpu.regs.r[0]), (1, 5));
}

#[test]
fn smlad_overflow_sets_q() {
    // 0x8000 * 0x8000 twice = 0x8000_0000 which overflows a signed 32-bit accumulator.
    let cpu = with([0xFB21, 0x3002], [0x8000_8000, 0x8000_8000, 0]);
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    assert!(cpu.regs.xpsr.q);
    // smuad without overflow does not touch Q.
    let (r1, r2) = (0x0002_0003, 0x0004_0005);
    let cpu = with([0xFB21, 0xF002], [r1, r2, 0]);
    assert_eq!(cpu.regs.r[0], 23);
    assert!(!cpu.regs.xpsr.q);
    // smladx swaps the halves of Rm: 3*4 + 2*5 + 100.
    assert_eq!(with([0xFB21, 0x3012], [r1, r2, 100]).regs.r[0], 122);
    // smusd: 3*5 - 2*4; smusdx: 3*4 - 2*5; smlsd adds Ra.
    assert_eq!(with([0xFB41, 0xF002], [r1, r2, 0]).regs.r[0], 7);
    assert_eq!(with([0xFB41, 0xF012], [r1, r2, 0]).regs.r[0], 2);
    assert_eq!(with([0xFB41, 0x3002], [r1, r2, 1]).regs.r[0], 8);
}

#[test]
fn dual_long_multiplies() {
    let long = |code: [u16; 2], acc: (u32, u32)| {
        let cpu = exec1(code, |c| {
            c.regs.r[0] = acc.0;
            c.regs.r[1] = acc.1;
            c.regs.r[2] = 0x0002_0003;
            c.regs.r[3] = 0x0004_0005;
        });
        (cpu.regs.r[1], cpu.regs.r[0])
    };
    // smlald: 0x1_0000_0000 + 3*5 + 2*4
    assert_eq!(long([0xFBC2, 0x01C3], (0, 1)), (1, 23));
    // smlaldx: 3*4 + 2*5
    assert_eq!(long([0xFBC2, 0x01D3], (0, 1)), (1, 22));
    // smlsld: 3*5 - 2*4
    assert_eq!(long([0xFBD2, 0x01C3], (0, 1)), (1, 7));
    // smlsldx: 3*4 - 2*5
    assert_eq!(long([0xFBD2, 0x01D3], (0, 0)), (0, 2));
}

#[test]
fn smmulr_rounds() {
    // product = 2^31: truncated high word 0, rounded 1.
    let (r1, r2) = (0x0000_8000, 0x0001_0000);
    assert_eq!(with([0xFB51, 0xF002], [r1, r2, 0]).regs.r[0], 0);
    assert_eq!(with([0xFB51, 0xF012], [r1, r2, 0]).regs.r[0], 1);
    // product = -1: smmul floors to -1, smmulr rounds to 0.
    assert_eq!(with([0xFB51, 0xF002], [u32::MAX, 1, 0]).regs.r[0], u32::MAX);
    assert_eq!(with([0xFB51, 0xF012], [u32::MAX, 1, 0]).regs.r[0], 0);
    // smmla: Ra + high word (product 2^32 -> 1).
    assert_eq!(with([0xFB51, 0x3002], [0x4000_0000, 4, 10]).regs.r[0], 11);
    // smmls: Ra - high word.
    assert_eq!(with([0xFB61, 0x3002], [0x4000_0000, 4, 10]).regs.r[0], 9);
    // smmlsr: (10 << 32) - 2^31 + 2^31 -> 10; smmls truncates to 9.
    assert_eq!(
        with([0xFB61, 0x3012], [0x0000_8000, 0x0001_0000, 10])
            .regs
            .r[0],
        10
    );
    assert_eq!(
        with([0xFB61, 0x3002], [0x0000_8000, 0x0001_0000, 10])
            .regs
            .r[0],
        9
    );
    // smmlar: (10 << 32) + 2^31 + 2^31 -> 11.
    assert_eq!(
        with([0xFB51, 0x3012], [0x0000_8000, 0x0001_0000, 10])
            .regs
            .r[0],
        11
    );
}

#[test]
fn usada8() {
    let (r1, r2) = (0x0A14_1E28, 0x281E_140A);
    assert_eq!(with([0xFB71, 0xF002], [r1, r2, 0]).regs.r[0], 80);
    assert_eq!(with([0xFB71, 0x3002], [r1, r2, 5]).regs.r[0], 85);
}

#[test]
fn sel_uses_ge() {
    // uadd8 r0, r1, r2 sets GE for bytes that carry; sel r0, r1, r2 then takes those bytes
    // from r1 and the rest from r2.
    let (mut cpu, mut bus) = cpu_with_code(&[0xFA81, 0xF042, 0xFAA1, 0xF082]);
    cpu.regs.r[1] = 0xFF11_FF22;
    cpu.regs.r[2] = 0x0133_0144;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.xpsr.ge, 0b1010);
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFF33_FF44);
}

#[test]
fn ssat16_usat16() {
    // ssat16 r0, #8, r1: 0xFF80 = -128 fits, 0x0100 saturates to 127.
    let cpu = with([0xF321, 0x0007], [0x0100_FF80, 0, 0]);
    assert_eq!(cpu.regs.r[0], 0x007F_FF80);
    assert!(cpu.regs.xpsr.q);
    let cpu = with([0xF321, 0x0007], [0x0010_FF80, 0, 0]);
    assert_eq!(cpu.regs.r[0], 0x0010_FF80);
    assert!(!cpu.regs.xpsr.q);
    // ssat16 r0, #16, r1 is the identity.
    let cpu = with([0xF321, 0x000F], [0x8000_7FFF, 0, 0]);
    assert_eq!(cpu.regs.r[0], 0x8000_7FFF);
    assert!(!cpu.regs.xpsr.q);
    // usat16 r0, #8, r1: 256 -> 255, -1 -> 0.
    let cpu = with([0xF3A1, 0x0008], [0xFFFF_0100, 0, 0]);
    assert_eq!(cpu.regs.r[0], 0x0000_00FF);
    assert!(cpu.regs.xpsr.q);
    // usat16 r0, #0, r1 clamps everything to 0.
    let cpu = with([0xF3A1, 0x0000], [0x0005_0007, 0, 0]);
    assert_eq!(cpu.regs.r[0], 0);
    assert!(cpu.regs.xpsr.q);
}

#[test]
fn extend16() {
    // sxtb16 r0, r1: bytes 0 and 2 sign-extended into the halfwords.
    let r1 = 0x1280_FF7F;
    assert_eq!(with([0xFA2F, 0xF081], [r1, 0, 0]).regs.r[0], 0xFF80_007F);
    // uxtb16 r0, r1, ror #16
    assert_eq!(with([0xFA3F, 0xF0A1], [r1, 0, 0]).regs.r[0], 0x007F_0080);
    // sxtb16 r0, r1, ror #8: bytes 1 and 3.
    assert_eq!(with([0xFA2F, 0xF091], [r1, 0, 0]).regs.r[0], 0x0012_FFFF);
    // sxtab16 r0, r1, r2: halfword-wise add, no carry between halves.
    assert_eq!(
        with([0xFA21, 0xF082], [0x1000_2000, 0x0080_00FF, 0]).regs.r[0],
        0x0F80_1FFF
    );
    // uxtab16 r0, r1, r2, ror #24: rotated Rm = 0x0034_0012; each half wraps at 16 bits.
    assert_eq!(
        with([0xFA31, 0xF0B2], [0x0001_FFFF, 0x1200_3400, 0]).regs.r[0],
        0x0035_0011
    );
}
