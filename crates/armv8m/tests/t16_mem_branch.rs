mod common;

use armv8m::{Cpu, StepResult, disasm};
use common::{FlatBus, RAM_BASE, cpu_with_code, run};

/// Disassemble the instruction at the current PC.
fn dis(cpu: &Cpu, bus: &mut FlatBus) -> String {
    let (instr, len) = cpu.decode_at(bus, cpu.regs.pc).expect("fetch");
    disasm(&instr, cpu.regs.pc, len)
}

fn ram_word(bus: &mut FlatBus, off: usize, v: u32) {
    bus.ram[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn cycles(r: StepResult) -> u32 {
    match r {
        StepResult::Executed { cycles } => cycles,
        other => panic!("expected Executed, got {other:?}"),
    }
}

#[test]
fn ldr_literal_aligns_pc() {
    // ldr r0, [pc, #4] at 0x102: Align(0x106, 4) + 4 = 0x108
    let (mut cpu, mut bus) = cpu_with_code(&[0x0000, 0x4801]);
    cpu.regs.pc = 0x102;
    bus.code[0x108..0x10C].copy_from_slice(&0xCAFE_BABEu32.to_le_bytes());
    assert_eq!(dis(&cpu, &mut bus), "ldr r0, [pc, #4]");
    assert_eq!(cycles(cpu.step(&mut bus)), 1);
    assert_eq!(cpu.regs.r[0], 0xCAFE_BABE);
    assert_eq!(cpu.regs.pc, 0x104);
}

#[test]
fn push_pop_lr_pc() {
    // push {r4, lr}; pop {r4, pc}
    let (mut cpu, mut bus) = cpu_with_code(&[0xB510, 0xBD10]);
    cpu.regs.lr = 0x201;
    cpu.regs.r[4] = 0x44;
    assert_eq!(dis(&cpu, &mut bus), "push {r4, lr}");
    assert_eq!(cycles(cpu.step(&mut bus)), 3);
    assert_eq!(cpu.regs.sp(), 0x2000_0FF8);
    assert_eq!(bus.ram[0xFF8..0xFFC], 0x44u32.to_le_bytes());
    assert_eq!(bus.ram[0xFFC..0x1000], 0x201u32.to_le_bytes());
    cpu.regs.r[4] = 0;
    assert_eq!(dis(&cpu, &mut bus), "pop {r4, pc}");
    assert_eq!(cycles(cpu.step(&mut bus)), 3);
    assert_eq!(cpu.regs.r[4], 0x44);
    assert_eq!(cpu.regs.pc, 0x200);
    assert!(cpu.regs.xpsr.t);
    assert_eq!(cpu.regs.sp(), 0x2000_1000);
}

#[test]
fn ldm_writeback_base_in_list() {
    // ldmia r0!, {r0, r1}: base in list, no writeback
    let (mut cpu, mut bus) = cpu_with_code(&[0xC803, 0xCA03]);
    ram_word(&mut bus, 0, 0x11);
    ram_word(&mut bus, 4, 0x22);
    cpu.regs.r[0] = RAM_BASE;
    cpu.regs.r[2] = RAM_BASE;
    assert_eq!(dis(&cpu, &mut bus), "ldmia r0, {r0, r1}");
    assert_eq!(cycles(cpu.step(&mut bus)), 3);
    assert_eq!((cpu.regs.r[0], cpu.regs.r[1]), (0x11, 0x22));
    // ldmia r2!, {r0, r1}: writeback
    assert_eq!(dis(&cpu, &mut bus), "ldmia r2!, {r0, r1}");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[2], RAM_BASE + 8);
}

#[test]
fn stm_writes_and_writes_back() {
    // stmia r2!, {r0, r1}
    let (mut cpu, mut bus) = cpu_with_code(&[0xC203]);
    cpu.regs.r[0] = 0xAA;
    cpu.regs.r[1] = 0xBB;
    cpu.regs.r[2] = RAM_BASE + 0x10;
    assert_eq!(dis(&cpu, &mut bus), "stmia r2!, {r0, r1}");
    assert_eq!(cycles(cpu.step(&mut bus)), 3);
    assert_eq!(bus.ram[0x10..0x14], 0xAAu32.to_le_bytes());
    assert_eq!(bus.ram[0x14..0x18], 0xBBu32.to_le_bytes());
    assert_eq!(cpu.regs.r[2], RAM_BASE + 0x18);
}

#[test]
fn b_cond_not_taken() {
    // beq +6 (target 0x10a), Z clear
    let (mut cpu, mut bus) = cpu_with_code(&[0xD003]);
    assert_eq!(dis(&cpu, &mut bus), "beq 0x10a");
    assert_eq!(cycles(cpu.step(&mut bus)), 1);
    assert_eq!(cpu.regs.pc, 0x102);
}

#[test]
fn b_cond_taken() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xD003]);
    cpu.regs.xpsr.z = true;
    assert_eq!(cycles(cpu.step(&mut bus)), 2);
    assert_eq!(cpu.regs.pc, 0x10A);
}

#[test]
fn b_unconditional() {
    // b +8 ; b . (negative offset)
    let (mut cpu, mut bus) = cpu_with_code(&[0xE002]);
    assert_eq!(dis(&cpu, &mut bus), "b 0x108");
    assert_eq!(cycles(cpu.step(&mut bus)), 2);
    assert_eq!(cpu.regs.pc, 0x108);
    let (mut cpu, mut bus) = cpu_with_code(&[0xE7FE]);
    assert_eq!(dis(&cpu, &mut bus), "b 0x100");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.pc, 0x100);
}

#[test]
fn cbz_forward() {
    // cbz r0, 0x10a ; cbnz r0, 0x10a
    let (mut cpu, mut bus) = cpu_with_code(&[0xB118]);
    assert_eq!(dis(&cpu, &mut bus), "cbz r0, 0x10a");
    assert_eq!(cycles(cpu.step(&mut bus)), 2);
    assert_eq!(cpu.regs.pc, 0x10A);
    let (mut cpu, mut bus) = cpu_with_code(&[0xB118]);
    cpu.regs.r[0] = 1;
    assert_eq!(cycles(cpu.step(&mut bus)), 1);
    assert_eq!(cpu.regs.pc, 0x102);
    let (mut cpu, mut bus) = cpu_with_code(&[0xB918]);
    cpu.regs.r[0] = 1;
    assert_eq!(dis(&cpu, &mut bus), "cbnz r0, 0x10a");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.pc, 0x10A);
}

#[test]
fn bkpt_returns_imm() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xBEAB]);
    assert_eq!(dis(&cpu, &mut bus), "bkpt #0xab");
    assert_eq!(cpu.step(&mut bus), StepResult::Bkpt(0xAB));
    assert_eq!(cpu.regs.pc, 0x100);
}

#[test]
fn wfi_returns_sleep() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF30]);
    assert_eq!(dis(&cpu, &mut bus), "wfi");
    assert_eq!(cpu.step(&mut bus), StepResult::Sleep);
    assert_eq!(cpu.regs.pc, 0x102);
}

#[test]
fn hints_nop_yield_wfe_sev() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF00, 0xBF10, 0xBF40, 0xBF20, 0xBF20]);
    for name in ["nop", "yield", "sev", "wfe"] {
        assert_eq!(dis(&cpu, &mut bus), name);
        // sev sets the event register so the first wfe does not sleep
        assert_eq!(cycles(cpu.step(&mut bus)), 1);
    }
    // the event register was consumed: the second wfe sleeps
    assert_eq!(cpu.step(&mut bus), StepResult::Sleep);
}

#[test]
fn svc_decodes() {
    let (cpu, mut bus) = cpu_with_code(&[0xDF01]);
    assert_eq!(dis(&cpu, &mut bus), "svc #1");
}

#[test]
fn rev16() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xBA48, 0xBA08, 0xBAC8]);
    cpu.regs.r[1] = 0x1122_3344;
    assert_eq!(dis(&cpu, &mut bus), "rev16 r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x2211_4433);
    assert_eq!(dis(&cpu, &mut bus), "rev r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x4433_2211);
    cpu.regs.r[1] = 0x0000_80FF;
    assert_eq!(dis(&cpu, &mut bus), "revsh r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF80);
}

#[test]
fn sxth() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xB208, 0xB248, 0xB288, 0xB2C8]);
    cpu.regs.r[1] = 0x1_8080;
    assert_eq!(dis(&cpu, &mut bus), "sxth r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_8080);
    assert_eq!(dis(&cpu, &mut bus), "sxtb r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_FF80);
    assert_eq!(dis(&cpu, &mut bus), "uxth r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x8080);
    assert_eq!(dis(&cpu, &mut bus), "uxtb r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x80);
}

#[test]
fn load_store_immediate() {
    // str r0,[r1,#4]; ldr r2,[r1,#4]; ldrb r3,[r1,#1]; strh r0,[r1,#2]; ldrh r4,[r1,#2]
    let (mut cpu, mut bus) = cpu_with_code(&[0x6048, 0x684A, 0x784B, 0x8048, 0x884C]);
    cpu.regs.r[0] = 0x1234_5678;
    cpu.regs.r[1] = RAM_BASE;
    for d in [
        "str r0, [r1, #4]",
        "ldr r2, [r1, #4]",
        "ldrb r3, [r1, #1]",
        "strh r0, [r1, #2]",
        "ldrh r4, [r1, #2]",
    ] {
        assert_eq!(dis(&cpu, &mut bus), d);
        run(&mut cpu, &mut bus, 1);
    }
    assert_eq!(cpu.regs.r[2], 0x1234_5678);
    assert_eq!(cpu.regs.r[3], 0);
    assert_eq!(cpu.regs.r[4], 0x5678);
}

#[test]
fn load_store_register_offset_and_signed() {
    // str r0,[r1,r2]; ldr r3,[r1,r2]; ldrsb r4,[r1,r2]; ldrsh r5,[r1,r2]
    let (mut cpu, mut bus) = cpu_with_code(&[0x5088, 0x588B, 0x568C, 0x5E8D]);
    cpu.regs.r[0] = 0x0000_80F0;
    cpu.regs.r[1] = RAM_BASE;
    cpu.regs.r[2] = 8;
    for d in [
        "str r0, [r1, r2]",
        "ldr r3, [r1, r2]",
        "ldrsb r4, [r1, r2]",
        "ldrsh r5, [r1, r2]",
    ] {
        assert_eq!(dis(&cpu, &mut bus), d);
        run(&mut cpu, &mut bus, 1);
    }
    assert_eq!(cpu.regs.r[3], 0x80F0);
    assert_eq!(cpu.regs.r[4], 0xFFFF_FFF0);
    assert_eq!(cpu.regs.r[5], 0xFFFF_80F0);
}

#[test]
fn sp_relative_load_store() {
    // str r0,[sp,#8]; ldr r1,[sp,#8]
    let (mut cpu, mut bus) = cpu_with_code(&[0x9002, 0x9902]);
    cpu.regs.r[0] = 0xDEAD_BEEF;
    assert_eq!(dis(&cpu, &mut bus), "str r0, [sp, #8]");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(dis(&cpu, &mut bus), "ldr r1, [sp, #8]");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[1], 0xDEAD_BEEF);
    assert_eq!(bus.ram[0x1008..0x100C], 0xDEAD_BEEFu32.to_le_bytes());
}

#[test]
fn unaligned_word_access_is_allowed() {
    // ldr r0, [r1]
    let (mut cpu, mut bus) = cpu_with_code(&[0x6808]);
    bus.ram[1..5].copy_from_slice(&0x0A0B_0C0Du32.to_le_bytes());
    cpu.regs.r[1] = RAM_BASE + 1;
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x0A0B_0C0D);
}

#[test]
fn bus_error_and_unaligned_ldm_fault() {
    // str r0, [r1] to unmapped memory
    let (mut cpu, mut bus) = cpu_with_code(&[0x6008]);
    cpu.regs.r[1] = 0x4000_0000;
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
    // ldmia r0, {r1} with unaligned base
    let (mut cpu, mut bus) = cpu_with_code(&[0xC802]);
    cpu.regs.r[0] = RAM_BASE + 2;
    assert_eq!(cpu.step(&mut bus), StepResult::Lockup);
}

#[test]
fn cps_changes_masks() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xB672, 0xB671, 0xB662, 0xB661]);
    for (d, primask, faultmask) in [
        ("cpsid i", true, false),
        ("cpsid f", true, true),
        ("cpsie i", false, true),
        ("cpsie f", false, false),
    ] {
        assert_eq!(dis(&cpu, &mut bus), d);
        run(&mut cpu, &mut bus, 1);
        assert_eq!(cpu.regs.primask.s, primask);
        assert_eq!(cpu.regs.faultmask.s, faultmask);
    }
}

#[test]
fn cps_unprivileged_is_noop() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xB672]);
    cpu.regs.control.s = 1;
    run(&mut cpu, &mut bus, 1);
    assert!(!cpu.regs.primask.s);
}
