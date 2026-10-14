mod common;

use armv8m::{Cpu, Instr, StepResult, decode16, disasm};
use common::{FlatBus, cpu_with_code, run};

/// Disassemble the instruction at the current PC.
fn dis(cpu: &Cpu, bus: &mut FlatBus) -> String {
    let (instr, len) = cpu.decode_at(bus, cpu.regs.pc).expect("fetch");
    disasm(&instr, cpu.regs.pc, len)
}

fn flags(cpu: &Cpu) -> (bool, bool, bool, bool) {
    let x = &cpu.regs.xpsr;
    (x.n, x.z, x.c, x.v)
}

#[test]
fn movs_imm() {
    // movs r0, #5
    let (mut cpu, mut bus) = cpu_with_code(&[0x2005]);
    assert_eq!(dis(&cpu, &mut bus), "movs r0, #5");
    assert_eq!(cpu.step(&mut bus), StepResult::Executed { cycles: 1 });
    assert_eq!(cpu.regs.r[0], 5);
    assert_eq!(flags(&cpu), (false, false, false, false));
    assert_eq!(cpu.regs.pc, 0x102);
}

#[test]
fn adds_carry_overflow() {
    // adds r0, #1
    let (mut cpu, mut bus) = cpu_with_code(&[0x3001]);
    cpu.regs.r[0] = 0x7FFF_FFFF;
    assert_eq!(dis(&cpu, &mut bus), "adds r0, #1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x8000_0000);
    assert_eq!(flags(&cpu), (true, false, false, true));
}

#[test]
fn subs_borrow() {
    // subs r0, #1
    let (mut cpu, mut bus) = cpu_with_code(&[0x3801]);
    assert_eq!(dis(&cpu, &mut bus), "subs r0, #1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_FFFF);
    assert_eq!(flags(&cpu), (true, false, false, false));
}

#[test]
fn lsls_carry_out() {
    // lsls r0, r1, #1
    let (mut cpu, mut bus) = cpu_with_code(&[0x0048]);
    cpu.regs.r[1] = 0x8000_0001;
    assert_eq!(dis(&cpu, &mut bus), "lsls r0, r1, #1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 2);
    assert!(cpu.regs.xpsr.c);
}

#[test]
fn lsrs_imm_zero_means_32() {
    // lsrs r0, r1, #32
    let (mut cpu, mut bus) = cpu_with_code(&[0x0808]);
    cpu.regs.r[1] = 0x8000_0000;
    assert_eq!(dis(&cpu, &mut bus), "lsrs r0, r1, #32");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0);
    assert!(cpu.regs.xpsr.c && cpu.regs.xpsr.z);
}

#[test]
fn asrs_reg_shift_ge_32() {
    // asrs r0, r1
    let (mut cpu, mut bus) = cpu_with_code(&[0x4108]);
    cpu.regs.r[0] = 0x8000_0000;
    cpu.regs.r[1] = 40;
    assert_eq!(dis(&cpu, &mut bus), "asrs r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_FFFF);
    assert!(cpu.regs.xpsr.c && cpu.regs.xpsr.n);
}

#[test]
fn muls() {
    // muls r0, r1, r0
    let (mut cpu, mut bus) = cpu_with_code(&[0x4348]);
    cpu.regs.r[0] = 0x1_0001;
    cpu.regs.r[1] = 0x1_0000;
    cpu.regs.xpsr.c = true;
    assert_eq!(dis(&cpu, &mut bus), "muls r0, r1, r0");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x0001_0000);
    assert_eq!(flags(&cpu), (false, false, true, false));
}

#[test]
fn mvns() {
    // mvns r0, r1
    let (mut cpu, mut bus) = cpu_with_code(&[0x43C8]);
    cpu.regs.r[1] = 0xFFFF_FFFF;
    assert_eq!(dis(&cpu, &mut bus), "mvns r0, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0);
    assert!(cpu.regs.xpsr.z);
}

#[test]
fn rsbs() {
    // rsbs r0, r1, #0   (negs r0, r1)
    let (mut cpu, mut bus) = cpu_with_code(&[0x4248]);
    cpu.regs.r[1] = 5;
    assert_eq!(dis(&cpu, &mut bus), "rsbs r0, r1, #0");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0xFFFF_FFFB);
    assert_eq!(flags(&cpu), (true, false, false, false));
}

#[test]
fn cmp_hi_regs() {
    // cmp r8, r9
    let (mut cpu, mut bus) = cpu_with_code(&[0x45C8]);
    cpu.regs.r[8] = 7;
    cpu.regs.r[9] = 7;
    assert_eq!(dis(&cpu, &mut bus), "cmp r8, r9");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(flags(&cpu), (false, true, true, false));
    assert_eq!(cpu.regs.r[8], 7);
}

#[test]
fn add_sp_imm() {
    // add r0, sp, #8 ; add sp, #16 ; sub sp, #16
    let (mut cpu, mut bus) = cpu_with_code(&[0xA802, 0xB004, 0xB084]);
    assert_eq!(dis(&cpu, &mut bus), "add r0, sp, #8");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x2000_1008);
    assert_eq!(dis(&cpu, &mut bus), "add sp, #16");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.sp(), 0x2000_1010);
    assert_eq!(dis(&cpu, &mut bus), "sub sp, #16");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.sp(), 0x2000_1000);
}

#[test]
fn adr() {
    // adr r0, #8   at 0x100: Align(PC,4) + 8 = 0x10c
    let (mut cpu, mut bus) = cpu_with_code(&[0xA002]);
    assert_eq!(dis(&cpu, &mut bus), "adr r0, 0x10c");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.r[0], 0x10C);
}

#[test]
fn mov_pc_branches_without_interworking() {
    // mov pc, r1 : ALUWritePC, T bit unchanged
    for (r1, expect_pc) in [(0x201u32, 0x200u32), (0x200, 0x200)] {
        let (mut cpu, mut bus) = cpu_with_code(&[0x468F]);
        cpu.regs.r[1] = r1;
        assert_eq!(dis(&cpu, &mut bus), "mov pc, r1");
        assert_eq!(cpu.step(&mut bus), StepResult::Executed { cycles: 2 });
        assert_eq!(cpu.regs.pc, expect_pc);
        assert!(cpu.regs.xpsr.t);
    }
}

#[test]
fn bx_and_blx() {
    // blx r1
    let (mut cpu, mut bus) = cpu_with_code(&[0x4788]);
    cpu.regs.r[1] = 0x301;
    assert_eq!(dis(&cpu, &mut bus), "blx r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (0x300, 0x103));
    // bx lr
    let (mut cpu, mut bus) = cpu_with_code(&[0x4770]);
    cpu.regs.lr = 0x401;
    assert_eq!(dis(&cpu, &mut bus), "bx lr");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.pc, 0x400);
}

#[test]
fn add_hi_reg_to_pc_branches() {
    // add pc, r1
    let (mut cpu, mut bus) = cpu_with_code(&[0x448F]);
    cpu.regs.r[1] = 0x10;
    assert_eq!(dis(&cpu, &mut bus), "add pc, r1");
    run(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.regs.pc, 0x114);
    assert!(cpu.regs.xpsr.t);
}

#[test]
fn it_block_suppresses_flags() {
    // it eq ; addeq r0, #1 ; (executes only when Z)
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, 0x3001]);
    assert_eq!(dis(&cpu, &mut bus), "it eq");
    cpu.regs.xpsr.z = true;
    cpu.regs.xpsr.n = true;
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.regs.r[0], 1);
    assert_eq!(flags(&cpu), (true, true, false, false)); // not recomputed
    assert_eq!(cpu.regs.xpsr.it, 0);

    let (mut cpu, mut bus) = cpu_with_code(&[0xBF08, 0x3001]);
    assert!(!cpu.regs.xpsr.z);
    run(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.regs.r[0], 0);
    assert_eq!(cpu.regs.pc, 0x104);
    assert_eq!(cpu.regs.xpsr.it, 0);
}

#[test]
fn it_then_else() {
    // ite ne ; movne r0, #1 ; moveq r0, #2
    let (mut cpu, mut bus) = cpu_with_code(&[0xBF14, 0x2001, 0x2002]);
    assert_eq!(dis(&cpu, &mut bus), "ite ne");
    cpu.regs.xpsr.z = true;
    run(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.regs.r[0], 2);
    assert_eq!(cpu.regs.xpsr.it, 0);
}

#[test]
fn undefined_16bit() {
    // udf #0
    assert_eq!(decode16(0xDE00), Instr::Undefined { raw: 0xDE00 });
    assert_eq!(disasm(&decode16(0xDE00), 0x100, 2), "udf #0");
    assert_eq!(disasm(&decode16(0xDE2A), 0x100, 2), "udf #42");
}
