mod common;

use armv8m::{Cpu, Instr, StepResult, decode32, disasm};
use common::{FlatBus, cpu_with_code, run};

fn dis(cpu: &Cpu, bus: &mut FlatBus) -> String {
    let (instr, len) = cpu.decode_at(bus, cpu.regs.pc).expect("fetch");
    disasm(&instr, cpu.regs.pc, len)
}

fn flags(cpu: &Cpu) -> (bool, bool, bool, bool) {
    let x = &cpu.regs.xpsr;
    (x.n, x.z, x.c, x.v)
}

/// Run a single 32-bit instruction after `setup`; returns the CPU and its disassembly.
fn exec1(code: [u16; 2], setup: impl FnOnce(&mut Cpu)) -> (Cpu, String) {
    let (mut cpu, mut bus) = cpu_with_code(&code);
    setup(&mut cpu);
    let text = dis(&cpu, &mut bus);
    assert!(matches!(cpu.step(&mut bus), StepResult::Executed { .. }));
    assert_eq!(cpu.regs.pc, 0x104);
    (cpu, text)
}

/// `MOV{S}.W r<rd>, #<imm12 encoding>`.
fn mov_imm12(rd: u16, imm12: u16, s: bool) -> [u16; 2] {
    let i = (imm12 >> 11) & 1;
    let imm3 = (imm12 >> 8) & 7;
    [
        0xF04F | (i << 10) | (u16::from(s) << 4),
        (imm3 << 12) | (rd << 8) | (imm12 & 0xFF),
    ]
}

#[test]
fn thumb_expand_imm_patterns() {
    for (imm12, want, text) in [
        (0x0ABu16, 0x0000_00ABu32, "mov.w r0, #171"),
        (0x1AB, 0x00AB_00AB, "mov.w r0, #0xab00ab"),
        (0x2AB, 0xAB00_AB00, "mov.w r0, #0xab00ab00"),
        (0x3AB, 0xABAB_ABAB, "mov.w r0, #0xabababab"),
        (0x4FF, 0x7F80_0000, "mov.w r0, #0x7f800000"),
    ] {
        let (cpu, t) = exec1(mov_imm12(0, imm12, false), |_| {});
        assert_eq!(cpu.regs.r[0], want, "imm12 {imm12:#x}");
        assert_eq!(t, text);
    }
}

#[test]
fn movs_w_rotated_imm_sets_carry_from_bit31() {
    // movs.w r0, #0x80000000 (imm12 = 0x400): rotated form, C = bit 31 = 1.
    let (cpu, t) = exec1(mov_imm12(0, 0x400, true), |_| {});
    assert_eq!(t, "movs.w r0, #0x80000000");
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    assert_eq!(flags(&cpu), (true, false, true, false));
    // Rotated form with bit 31 clear clears a set carry.
    let (cpu, _) = exec1(mov_imm12(0, 0x4FF, true), |c| c.regs.xpsr.c = true);
    assert_eq!(cpu.regs.r[0], 0x7F80_0000);
    assert!(!cpu.regs.xpsr.c);
    // Unrotated form leaves C alone.
    let (cpu, _) = exec1(mov_imm12(0, 0x0AB, true), |c| c.regs.xpsr.c = true);
    assert!(cpu.regs.xpsr.c);
}

#[test]
fn logical_imm_carry_and_tst() {
    // ands.w r0, r1, #0x80000000 ; C set from the rotated immediate
    let (cpu, t) = exec1([0xF011, 0x4000], |c| c.regs.r[1] = 0xFFFF_FFFF);
    assert_eq!(t, "ands.w r0, r1, #0x80000000");
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    assert_eq!(flags(&cpu), (true, false, true, false));
    // tst.w r1, #0x80000000
    let (cpu, t) = exec1([0xF011, 0x4F00], |c| c.regs.r[1] = 0x8000_0000);
    assert_eq!(t, "tst.w r1, #0x80000000");
    assert_eq!(flags(&cpu), (true, false, true, false));
}

#[test]
fn movw_movt_pair() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xF245, 0x6078, 0xF2C1, 0x2034]);
    assert_eq!(dis(&cpu, &mut bus), "movw r0, #0x5678");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x5678);
    assert_eq!(dis(&cpu, &mut bus), "movt r0, #0x1234");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x1234_5678);
}

#[test]
fn addw_subw_adr() {
    let (cpu, t) = exec1([0xF201, 0x1023], |c| c.regs.r[1] = 0x1000);
    assert_eq!(t, "addw r0, r1, #0x123");
    assert_eq!(cpu.regs.r[0], 0x1123);
    let (cpu, t) = exec1([0xF2A1, 0x0001], |c| c.regs.r[1] = 0x1000);
    assert_eq!(t, "subw r0, r1, #1");
    assert_eq!(cpu.regs.r[0], 0xFFF);
    // addw r0, pc, #4 is ADR: Align(PC + 4, 4) + 4
    let (cpu, t) = exec1([0xF20F, 0x0004], |_| {});
    assert_eq!(t, "adr.w r0, 0x108");
    assert_eq!(cpu.regs.r[0], 0x108);
}

#[test]
fn bfi() {
    // bfi r0, r1, #8, #4
    let (cpu, t) = exec1([0xF361, 0x200B], |c| {
        c.regs.r[0] = 0xFFFF_FFFF;
        c.regs.r[1] = 0x0000_0005;
    });
    assert_eq!(t, "bfi r0, r1, #8, #4");
    assert_eq!(cpu.regs.r[0], 0xFFFF_F5FF);
    // bfc r0, #8, #4
    let (cpu, t) = exec1([0xF36F, 0x200B], |c| c.regs.r[0] = 0xFFFF_FFFF);
    assert_eq!(t, "bfc r0, #8, #4");
    assert_eq!(cpu.regs.r[0], 0xFFFF_F0FF);
}

#[test]
fn ubfx() {
    let (cpu, t) = exec1([0xF3C1, 0x1007], |c| c.regs.r[1] = 0x0000_0AB0);
    assert_eq!(t, "ubfx r0, r1, #4, #8");
    assert_eq!(cpu.regs.r[0], 0xAB);
    // sbfx r0, r1, #4, #8 sign-extends
    let (cpu, t) = exec1([0xF341, 0x1007], |c| c.regs.r[1] = 0x0000_0F80);
    assert_eq!(t, "sbfx r0, r1, #4, #8");
    assert_eq!(cpu.regs.r[0], 0xFFFF_FFF8);
}

#[test]
fn ssat_sets_q() {
    // ssat r0, #8, r1
    let (cpu, t) = exec1([0xF301, 0x0007], |c| c.regs.r[1] = 1000);
    assert_eq!(t, "ssat r0, #8, r1");
    assert_eq!(cpu.regs.r[0], 127);
    assert!(cpu.regs.xpsr.q);
    let (cpu, _) = exec1([0xF301, 0x0007], |c| c.regs.r[1] = (-1000i32) as u32);
    assert_eq!(cpu.regs.r[0] as i32, -128);
    assert!(cpu.regs.xpsr.q);
    let (cpu, _) = exec1([0xF301, 0x0007], |c| c.regs.r[1] = 5);
    assert_eq!(cpu.regs.r[0], 5);
    assert!(!cpu.regs.xpsr.q);
    // usat r0, #8, r1 (negative clamps to 0)
    let (cpu, t) = exec1([0xF381, 0x0008], |c| c.regs.r[1] = (-5i32) as u32);
    assert_eq!(t, "usat r0, #8, r1");
    assert_eq!(cpu.regs.r[0], 0);
    assert!(cpu.regs.xpsr.q);
}

#[test]
fn sdiv_negative() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xFB91, 0xF0F2]);
    cpu.regs.r[1] = (-7i32) as u32;
    cpu.regs.r[2] = 2;
    assert_eq!(dis(&cpu, &mut bus), "sdiv r0, r1, r2");
    assert_eq!(cpu.step(&mut bus), StepResult::Executed { cycles: 4 });
    assert_eq!(cpu.regs.r[0] as i32, -3);
}

#[test]
fn udiv_by_zero_returns_zero() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xFBB1, 0xF0F2]);
    cpu.regs.r[0] = 99;
    cpu.regs.r[1] = 100;
    cpu.regs.r[2] = 0;
    assert_eq!(dis(&cpu, &mut bus), "udiv r0, r1, r2");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0);
    // normal unsigned divide
    let (cpu, _) = exec1([0xFBB1, 0xF0F2], |c| {
        c.regs.r[1] = 0xFFFF_FFF0;
        c.regs.r[2] = 16;
    });
    assert_eq!(cpu.regs.r[0], 0x0FFF_FFFF);
    // sdiv by zero and INT_MIN / -1
    let (cpu, _) = exec1([0xFB91, 0xF0F2], |c| c.regs.r[1] = 5);
    assert_eq!(cpu.regs.r[0], 0);
    let (cpu, _) = exec1([0xFB91, 0xF0F2], |c| {
        c.regs.r[1] = 0x8000_0000;
        c.regs.r[2] = u32::MAX;
    });
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
}

#[test]
fn umull() {
    // umull r0, r1, r2, r3
    let (cpu, t) = exec1([0xFBA2, 0x0103], |c| {
        c.regs.r[2] = 0xFFFF_FFFF;
        c.regs.r[3] = 0xFFFF_FFFF;
    });
    assert_eq!(t, "umull r0, r1, r2, r3");
    assert_eq!((cpu.regs.r[1], cpu.regs.r[0]), (0xFFFF_FFFE, 1));
    // smull
    let (cpu, t) = exec1([0xFB82, 0x0103], |c| {
        c.regs.r[2] = (-2i32) as u32;
        c.regs.r[3] = 3;
    });
    assert_eq!(t, "smull r0, r1, r2, r3");
    assert_eq!(
        (cpu.regs.r[1], cpu.regs.r[0]),
        (0xFFFF_FFFF, (-6i32) as u32)
    );
}

#[test]
fn smlal_accumulates_64bit() {
    // smlal r0, r1, r2, r3 ; {r1,r0} = 0x0000_0001_FFFF_FFFF + (-2 * 3)
    let (cpu, t) = exec1([0xFBC2, 0x0103], |c| {
        c.regs.r[0] = 0xFFFF_FFFF;
        c.regs.r[1] = 1;
        c.regs.r[2] = (-2i32) as u32;
        c.regs.r[3] = 3;
    });
    assert_eq!(t, "smlal r0, r1, r2, r3");
    let got = (u64::from(cpu.regs.r[1]) << 32) | u64::from(cpu.regs.r[0]);
    assert_eq!(got, 0x1_FFFF_FFFFu64.wrapping_sub(6));
    // umlal
    let (cpu, t) = exec1([0xFBE2, 0x0103], |c| {
        c.regs.r[0] = 0xFFFF_FFFF;
        c.regs.r[2] = 2;
        c.regs.r[3] = 3;
    });
    assert_eq!(t, "umlal r0, r1, r2, r3");
    assert_eq!((cpu.regs.r[1], cpu.regs.r[0]), (1, 5));
    // umaal r0, r1, r2, r3: 3*4 + 0xFFFFFFFF + 0xFFFFFFFF
    let (cpu, t) = exec1([0xFBE2, 0x0163], |c| {
        c.regs.r[0] = 0xFFFF_FFFF;
        c.regs.r[1] = 0xFFFF_FFFF;
        c.regs.r[2] = 3;
        c.regs.r[3] = 4;
    });
    assert_eq!(t, "umaal r0, r1, r2, r3");
    let got = (u64::from(cpu.regs.r[1]) << 32) | u64::from(cpu.regs.r[0]);
    assert_eq!(got, 12 + 2 * 0xFFFF_FFFFu64);
}

#[test]
fn mul_mla_mls() {
    let (cpu, t) = exec1([0xFB01, 0xF002], |c| {
        c.regs.r[1] = 6;
        c.regs.r[2] = 7;
    });
    assert_eq!(t, "mul.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 42);
    let (cpu, t) = exec1([0xFB01, 0x3002], |c| {
        c.regs.r[1] = 6;
        c.regs.r[2] = 7;
        c.regs.r[3] = 100;
    });
    assert_eq!(t, "mla r0, r1, r2, r3");
    assert_eq!(cpu.regs.r[0], 142);
    let (cpu, t) = exec1([0xFB01, 0x3012], |c| {
        c.regs.r[1] = 6;
        c.regs.r[2] = 7;
        c.regs.r[3] = 100;
    });
    assert_eq!(t, "mls r0, r1, r2, r3");
    assert_eq!(cpu.regs.r[0], 58);
}

#[test]
fn orrs_shifted_reg_ror() {
    // orrs.w r0, r1, r2, ror #4
    let (cpu, t) = exec1([0xEA51, 0x1032], |c| {
        c.regs.r[1] = 0x1;
        c.regs.r[2] = 0x0000_000F;
    });
    assert_eq!(t, "orrs.w r0, r1, r2, ror #4");
    assert_eq!(cpu.regs.r[0], 0xF000_0001);
    assert_eq!(flags(&cpu), (true, false, true, false));
}

#[test]
fn adds_w_vs_add_w_flags() {
    // add.w r0, r1, r2 never sets flags
    let (cpu, t) = exec1([0xEB01, 0x0002], |c| {
        c.regs.r[1] = 0xFFFF_FFFF;
        c.regs.r[2] = 1;
    });
    assert_eq!(t, "add.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0);
    assert_eq!(flags(&cpu), (false, false, false, false));
    // adds.w sets them
    let (cpu, t) = exec1([0xEB11, 0x0002], |c| {
        c.regs.r[1] = 0xFFFF_FFFF;
        c.regs.r[2] = 1;
    });
    assert_eq!(t, "adds.w r0, r1, r2");
    assert_eq!(flags(&cpu), (false, true, true, false));
    // Inside an IT block: add.w still does not set flags, adds.w DOES (32-bit S form).
    for (hw1, want) in [(0xEB01u16, false), (0xEB11, true)] {
        let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, hw1, 0x0002]);
        cpu.regs.xpsr.z = true; // EQ passes
        cpu.regs.r[1] = 0xFFFF_FFFF;
        cpu.regs.r[2] = 1;
        run(&mut cpu, &mut bus, 2);
        assert_eq!(cpu.regs.r[0], 0);
        assert_eq!(cpu.regs.xpsr.c, want, "hw1 {hw1:#x}");
        assert_eq!(cpu.regs.pc, 0x106);
    }
}

#[test]
fn modified_imm_arith() {
    // sub.w r0, r1, #0x100
    let (cpu, t) = exec1([0xF5A1, 0x7080], |c| c.regs.r[1] = 0x300);
    assert_eq!(t, "sub.w r0, r1, #0x100");
    assert_eq!(cpu.regs.r[0], 0x200);
    // cmp.w r0, #0x100 (SUB with Rd=15, S=1)
    let (cpu, t) = exec1([0xF5B0, 0x7F80], |c| c.regs.r[0] = 0x100);
    assert_eq!(t, "cmp.w r0, #0x100");
    assert_eq!(flags(&cpu), (false, true, true, false));
    // cmn.w r0, #1 (ADD with Rd=15, S=1)
    let (cpu, t) = exec1([0xF110, 0x0F01], |c| c.regs.r[0] = u32::MAX);
    assert_eq!(t, "cmn.w r0, #1");
    assert_eq!(flags(&cpu), (false, true, true, false));
    // rsb r0, r1, #0x100
    let (cpu, t) = exec1([0xF5C1, 0x7080], |c| c.regs.r[1] = 1);
    assert_eq!(t, "rsb r0, r1, #0x100");
    assert_eq!(cpu.regs.r[0], 255);
    // mvn r0, #0
    let (cpu, t) = exec1([0xF06F, 0x0000], |_| {});
    assert_eq!(t, "mvn.w r0, #0");
    assert_eq!(cpu.regs.r[0], u32::MAX);
    // orn r0, r1, #0xff
    let (cpu, t) = exec1([0xF061, 0x00FF], |c| c.regs.r[1] = 0x1);
    assert_eq!(t, "orn r0, r1, #255");
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF01);
    // add.w sp, sp, #8 is allowed
    let (cpu, t) = exec1([0xF10D, 0x0D08], |_| {});
    assert_eq!(t, "add.w sp, sp, #8");
    assert_eq!(cpu.regs.sp(), 0x2000_1008);
    // cmp.w sp, #0
    let (_, t) = exec1([0xF1BD, 0x0F00], |_| {});
    assert_eq!(t, "cmp.w sp, #0");
}

#[test]
fn shifted_register_forms() {
    // mov.w r0, r1
    let (cpu, t) = exec1([0xEA4F, 0x0001], |c| c.regs.r[1] = 9);
    assert_eq!(t, "mov.w r0, r1");
    assert_eq!(cpu.regs.r[0], 9);
    // lsls.w r0, r1, #4 (ORRS Rn=15 type LSL imm 4)
    let (cpu, t) = exec1([0xEA5F, 0x1001], |c| c.regs.r[1] = 0x1000_0001);
    assert_eq!(t, "lsls.w r0, r1, #4");
    assert_eq!(cpu.regs.r[0], 0x10);
    assert!(cpu.regs.xpsr.c);
    // asr.w r0, r1, #4
    let (cpu, t) = exec1([0xEA4F, 0x1021], |c| c.regs.r[1] = 0x8000_0000);
    assert_eq!(t, "asr.w r0, r1, #4");
    assert_eq!(cpu.regs.r[0], 0xF800_0000);
    // rrx r0, r1
    let (cpu, t) = exec1([0xEA4F, 0x0031], |c| {
        c.regs.r[1] = 2;
        c.regs.xpsr.c = true;
    });
    assert_eq!(t, "rrx r0, r1");
    assert_eq!(cpu.regs.r[0], 0x8000_0001);
    // mvn.w r0, r1, lsl #2
    let (cpu, t) = exec1([0xEA6F, 0x0081], |c| c.regs.r[1] = 1);
    assert_eq!(t, "mvn.w r0, r1, lsl #2");
    assert_eq!(cpu.regs.r[0], !4);
    // orn r0, r1, r2
    let (cpu, t) = exec1([0xEA61, 0x0002], |c| {
        c.regs.r[1] = 1;
        c.regs.r[2] = 0xFF;
    });
    assert_eq!(t, "orn r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF01);
    // sub.w r0, r1, r2, lsl #1
    let (cpu, t) = exec1([0xEBA1, 0x0042], |c| {
        c.regs.r[1] = 10;
        c.regs.r[2] = 3;
    });
    assert_eq!(t, "sub.w r0, r1, r2, lsl #1");
    assert_eq!(cpu.regs.r[0], 4);
    // rsb r0, r1, r2
    let (cpu, t) = exec1([0xEBC1, 0x0002], |c| {
        c.regs.r[1] = 10;
        c.regs.r[2] = 3;
    });
    assert_eq!(t, "rsb r0, r1, r2");
    assert_eq!(cpu.regs.r[0] as i32, -7);
    // cmp.w r1, r2
    let (cpu, t) = exec1([0xEBB1, 0x0F02], |c| {
        c.regs.r[1] = 3;
        c.regs.r[2] = 3;
    });
    assert_eq!(t, "cmp.w r1, r2");
    assert!(cpu.regs.xpsr.z);
    // teq r1, r2 / tst.w r1, r2
    let (cpu, t) = exec1([0xEA91, 0x0F02], |c| {
        c.regs.r[1] = 3;
        c.regs.r[2] = 3;
    });
    assert_eq!(t, "teq r1, r2");
    assert!(cpu.regs.xpsr.z);
    let (_, t) = exec1([0xEA11, 0x0F02], |_| {});
    assert_eq!(t, "tst.w r1, r2");
    // adc.w / sbc.w with carry
    let (cpu, t) = exec1([0xEB41, 0x0002], |c| {
        c.regs.r[1] = 1;
        c.regs.r[2] = 2;
        c.regs.xpsr.c = true;
    });
    assert_eq!(t, "adc.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 4);
    let (cpu, t) = exec1([0xEB61, 0x0002], |c| {
        c.regs.r[1] = 5;
        c.regs.r[2] = 2;
        c.regs.xpsr.c = false;
    });
    assert_eq!(t, "sbc.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 2);
}

#[test]
fn pkh() {
    let (cpu, t) = exec1([0xEAC1, 0x2002], |c| {
        c.regs.r[1] = 0xAAAA_1111;
        c.regs.r[2] = 0x0000_BBCC;
    });
    assert_eq!(t, "pkhbt r0, r1, r2, lsl #8");
    assert_eq!(cpu.regs.r[0], 0x00BB_1111);
    let (cpu, t) = exec1([0xEAC1, 0x2022], |c| {
        c.regs.r[1] = 0xAAAA_1111;
        c.regs.r[2] = 0xBBCC_DDEE;
    });
    assert_eq!(t, "pkhtb r0, r1, r2, asr #8");
    assert_eq!(cpu.regs.r[0], 0xAAAA_CCDD);
}

#[test]
fn register_controlled_shifts() {
    let (cpu, t) = exec1([0xFA01, 0xF002], |c| {
        c.regs.r[1] = 1;
        c.regs.r[2] = 4;
    });
    assert_eq!(t, "lsl.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 16);
    let (cpu, t) = exec1([0xFA31, 0xF002], |c| {
        c.regs.r[1] = 0x8000_0000;
        c.regs.r[2] = 1;
    });
    assert_eq!(t, "lsrs.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0x4000_0000);
    let (cpu, t) = exec1([0xFA41, 0xF002], |c| {
        c.regs.r[1] = 0x8000_0000;
        c.regs.r[2] = 40;
    });
    assert_eq!(t, "asr.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], u32::MAX);
    let (cpu, t) = exec1([0xFA61, 0xF002], |c| {
        c.regs.r[1] = 1;
        c.regs.r[2] = 0x101;
    });
    assert_eq!(t, "ror.w r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
}

#[test]
fn extends_clz_rbit_rev_sel() {
    let (cpu, t) = exec1([0xFA1F, 0xF081], |c| c.regs.r[1] = 0x1234_ABCD);
    assert_eq!(t, "uxth.w r0, r1");
    assert_eq!(cpu.regs.r[0], 0xABCD);
    let (cpu, t) = exec1([0xFA4F, 0xF081], |c| c.regs.r[1] = 0x80);
    assert_eq!(t, "sxtb.w r0, r1");
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF80);
    let (cpu, t) = exec1([0xFA11, 0xF082], |c| {
        c.regs.r[1] = 0x10;
        c.regs.r[2] = 0x1_0005;
    });
    assert_eq!(t, "uxtah r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0x15);
    let (cpu, t) = exec1([0xFA51, 0xF092], |c| {
        c.regs.r[1] = 0x10;
        c.regs.r[2] = 0x0000_FF00;
    });
    assert_eq!(t, "uxtab r0, r1, r2, ror #8");
    assert_eq!(cpu.regs.r[0], 0x10 + 0xFF);
    let (cpu, t) = exec1([0xFAB1, 0xF081], |c| c.regs.r[1] = 0x0000_8000);
    assert_eq!(t, "clz r0, r1");
    assert_eq!(cpu.regs.r[0], 16);
    let (cpu, _) = exec1([0xFAB1, 0xF081], |_| {});
    assert_eq!(cpu.regs.r[0], 32);
    let (cpu, t) = exec1([0xFA91, 0xF0A1], |c| c.regs.r[1] = 1);
    assert_eq!(t, "rbit r0, r1");
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    let (cpu, t) = exec1([0xFA91, 0xF081], |c| c.regs.r[1] = 0x1122_3344);
    assert_eq!(t, "rev.w r0, r1");
    assert_eq!(cpu.regs.r[0], 0x4433_2211);
    let (cpu, t) = exec1([0xFA91, 0xF091], |c| c.regs.r[1] = 0x1122_3344);
    assert_eq!(t, "rev16.w r0, r1");
    assert_eq!(cpu.regs.r[0], 0x2211_4433);
    let (cpu, t) = exec1([0xFA91, 0xF0B1], |c| c.regs.r[1] = 0x0000_80FF);
    assert_eq!(t, "revsh.w r0, r1");
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF80);
    let (cpu, t) = exec1([0xFAA1, 0xF082], |c| {
        c.regs.r[1] = 0x1111_1111;
        c.regs.r[2] = 0x2222_2222;
        c.regs.xpsr.ge = 0b0101;
    });
    assert_eq!(t, "sel r0, r1, r2");
    assert_eq!(cpu.regs.r[0], 0x2211_2211);
}

#[test]
fn unpredictable_sp_pc_operands_are_undefined() {
    // and r13, r0, #1 ; sdiv r13, r1, r2
    for code in [
        [0xF000u16, 0x0D01],
        [0xFB91, 0xFDF2],
        [0xF01D, 0x0F01],
        [0xEA9D, 0x0F01],
        [0xEB0D, 0x0D50],
    ] {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        let (instr, len) = cpu.decode_at(&mut bus, 0x100).unwrap();
        assert!(matches!(instr, Instr::Undefined { .. }), "{code:x?}");
        assert_eq!(len, 4);
        assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    }
    assert!(matches!(
        decode32(0xF000, 0x0D01),
        Instr::Undefined { raw: 0xF000_0D01 }
    ));
}

#[test]
fn sp_rd_register_add_shift_limits() {
    // add.w sp, sp, r0, lsl #2 accepted; lsr #1 (EB0D 0D50) is Undefined (checked above).
    let (cpu, t) = exec1([0xEB0D, 0x0D80], |c| c.regs.r[0] = 1);
    assert_eq!(t, "add.w sp, sp, r0, lsl #2");
    assert_eq!(cpu.regs.sp(), 0x2000_1004);
}
