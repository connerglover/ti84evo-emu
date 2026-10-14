mod common;

use armv8m::{Access, Bus, BusError, Cpu, Instr, Sec, StepResult, decode32, disasm};
use common::{FlatBus, RAM_BASE, cpu_with_code, step_hardfault};

const UNALIGNED: u32 = 1 << 24;

fn dis(cpu: &Cpu, bus: &mut FlatBus) -> String {
    let (instr, len) = cpu.decode_at(bus, cpu.regs.pc).expect("fetch");
    disasm(&instr, cpu.regs.pc, len)
}

fn put(bus: &mut FlatBus, addr: u32, v: u32) {
    let o = (addr - RAM_BASE) as usize;
    bus.ram[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn get(bus: &FlatBus, addr: u32) -> u32 {
    let o = (addr - RAM_BASE) as usize;
    u32::from_le_bytes(bus.ram[o..o + 4].try_into().unwrap())
}

/// Disassemble then execute one instruction that must complete.
fn exec(cpu: &mut Cpu, bus: &mut FlatBus) -> String {
    let text = dis(cpu, bus);
    assert!(
        matches!(cpu.step(bus), StepResult::Executed { .. }),
        "{text}"
    );
    text
}

fn is_undef(hw1: u16, hw2: u16) -> bool {
    matches!(decode32(hw1, hw2), Instr::Undefined { .. })
}

const A: u32 = RAM_BASE + 0x100;

// ---- single loads / stores ----

#[test]
fn ldr_post_index_writeback() {
    // ldr.w r0, [r1], #4
    let (mut cpu, mut bus) = cpu_with_code(&[0xF851, 0x0B04]);
    cpu.regs.r[1] = A;
    put(&mut bus, A, 0xDEAD_BEEF);
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w r0, [r1], #4");
    assert_eq!(cpu.regs.r[0], 0xDEAD_BEEF);
    assert_eq!(cpu.regs.r[1], A + 4);
    assert_eq!(cpu.regs.pc, 0x104);
}

#[test]
fn ldr_pre_index_and_negative_offset() {
    // ldr.w r0, [r1, #4]!
    let (mut cpu, mut bus) = cpu_with_code(&[0xF851, 0x0F04]);
    cpu.regs.r[1] = A;
    put(&mut bus, A + 4, 7);
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w r0, [r1, #4]!");
    assert_eq!((cpu.regs.r[0], cpu.regs.r[1]), (7, A + 4));
    // ldr.w r0, [r1, #-4]
    let (mut cpu, mut bus) = cpu_with_code(&[0xF851, 0x0C04]);
    cpu.regs.r[1] = A;
    put(&mut bus, A - 4, 9);
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w r0, [r1, #-4]");
    assert_eq!((cpu.regs.r[0], cpu.regs.r[1]), (9, A));
}

#[test]
fn ldr_imm12_literal_and_register() {
    // ldr.w r0, [r1, #8]
    let (mut cpu, mut bus) = cpu_with_code(&[0xF8D1, 0x0008]);
    cpu.regs.r[1] = A;
    put(&mut bus, A + 8, 0x1234_5678);
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w r0, [r1, #8]");
    assert_eq!(cpu.regs.r[0], 0x1234_5678);
    // ldr.w r0, [pc, #4]: Align(0x104, 4) + 4
    let (mut cpu, mut bus) = cpu_with_code(&[0xF8DF, 0x0004]);
    bus.code[0x108..0x10C].copy_from_slice(&0xCAFE_BABEu32.to_le_bytes());
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w r0, [pc, #4]");
    assert_eq!(cpu.regs.r[0], 0xCAFE_BABE);
    // ldrb.w r0, [pc, #-8]: byte literal with a negative offset
    let (mut cpu, mut bus) = cpu_with_code(&[0xF81F, 0x0008]);
    bus.code[0xFC] = 0x80;
    assert_eq!(exec(&mut cpu, &mut bus), "ldrb.w r0, [pc, #-8]");
    assert_eq!(cpu.regs.r[0], 0x80);
    // ldrsh.w r0, [r1, r2, lsl #2]
    let (mut cpu, mut bus) = cpu_with_code(&[0xF931, 0x0022]);
    cpu.regs.r[1] = A;
    cpu.regs.r[2] = 2;
    put(&mut bus, A + 8, 0x0000_8001);
    assert_eq!(exec(&mut cpu, &mut bus), "ldrsh.w r0, [r1, r2, lsl #2]");
    assert_eq!(cpu.regs.r[0], 0xFFFF_8001);
}

#[test]
fn ldr_to_pc_interworks() {
    // ldr.w pc, [r1]
    let (mut cpu, mut bus) = cpu_with_code(&[0xF8D1, 0xF000]);
    cpu.regs.r[1] = A;
    put(&mut bus, A, 0x1F1);
    assert_eq!(exec(&mut cpu, &mut bus), "ldr.w pc, [r1]");
    assert_eq!(cpu.regs.pc, 0x1F0);
    assert!(cpu.regs.xpsr.t);
    // Clearing the T bit makes the next fetch fault INVSTATE (see tests/exceptions.rs).
    let (mut cpu, mut bus) = cpu_with_code(&[0xF8D1, 0xF000]);
    cpu.regs.r[1] = A;
    put(&mut bus, A, 0x1F0);
    exec(&mut cpu, &mut bus);
    assert!(!cpu.regs.xpsr.t);
}

#[test]
fn stores_all_sizes() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xF8C1, 0x0004, 0xF8A1, 0x0008, 0xF881, 0x000C]);
    cpu.regs.r[1] = A;
    cpu.regs.r[0] = 0xAABB_CCDD;
    assert_eq!(exec(&mut cpu, &mut bus), "str.w r0, [r1, #4]");
    assert_eq!(exec(&mut cpu, &mut bus), "strh.w r0, [r1, #8]");
    assert_eq!(exec(&mut cpu, &mut bus), "strb.w r0, [r1, #12]");
    assert_eq!(get(&bus, A + 4), 0xAABB_CCDD);
    assert_eq!(get(&bus, A + 8), 0xCCDD);
    assert_eq!(get(&bus, A + 12), 0xDD);
    // str.w r0, [r1, #-4]!
    let (mut cpu, mut bus) = cpu_with_code(&[0xF841, 0x0D04]);
    cpu.regs.r[1] = A;
    cpu.regs.r[0] = 5;
    assert_eq!(exec(&mut cpu, &mut bus), "str.w r0, [r1, #-4]!");
    assert_eq!((get(&bus, A - 4), cpu.regs.r[1]), (5, A - 4));
    // str.w r0, [r1, r2, lsl #1]
    let (mut cpu, mut bus) = cpu_with_code(&[0xF841, 0x0012]);
    cpu.regs.r[1] = A;
    cpu.regs.r[2] = 4;
    cpu.regs.r[0] = 6;
    assert_eq!(exec(&mut cpu, &mut bus), "str.w r0, [r1, r2, lsl #1]");
    assert_eq!(get(&bus, A + 8), 6);
}

/// Bus that records the privilege of data accesses.
struct Spy {
    inner: FlatBus,
    privs: Vec<bool>,
}

impl Bus for Spy {
    fn read8(&mut self, a: u32, x: Access) -> Result<u8, BusError> {
        self.privs.push(x.privileged);
        self.inner.read8(a, x)
    }
    fn read16(&mut self, a: u32, x: Access) -> Result<u16, BusError> {
        self.inner.read16(a, x)
    }
    fn read32(&mut self, a: u32, x: Access) -> Result<u32, BusError> {
        self.privs.push(x.privileged);
        self.inner.read32(a, x)
    }
    fn write8(&mut self, a: u32, v: u8, x: Access) -> Result<(), BusError> {
        self.inner.write8(a, v, x)
    }
    fn write16(&mut self, a: u32, v: u16, x: Access) -> Result<(), BusError> {
        self.inner.write16(a, v, x)
    }
    fn write32(&mut self, a: u32, v: u32, x: Access) -> Result<(), BusError> {
        self.privs.push(x.privileged);
        self.inner.write32(a, v, x)
    }
}

#[test]
fn ldrt_strt_are_unprivileged() {
    // ldrt r0, [r1, #4]; strt r0, [r1, #8]; ldr.w r2, [r1]
    let (mut cpu, inner) = cpu_with_code(&[0xF851, 0x0E04, 0xF841, 0x0E08, 0xF8D1, 0x2000]);
    let mut bus = Spy {
        inner,
        privs: Vec::new(),
    };
    cpu.regs.r[1] = A;
    let want = ["ldrt r0, [r1, #4]", "strt r0, [r1, #8]"];
    for text in want {
        let (i, len) = cpu.decode_at(&mut bus, cpu.regs.pc).unwrap();
        assert_eq!(disasm(&i, cpu.regs.pc, len), text);
        cpu.step(&mut bus);
    }
    cpu.step(&mut bus);
    // Fetches are not recorded; the LDRT/STRT are unprivileged, the plain LDR is privileged.
    assert_eq!(bus.privs, [false, false, true]);
}

#[test]
fn pld_pli_are_noops() {
    for (code, text) in [
        ([0xF891, 0xF004], "pld [r1, #4]"),
        ([0xF991, 0xF004], "pli [r1, #4]"),
        ([0xF811, 0xF012], "pld [r1, r2, lsl #1]"),
        ([0xF811, 0xFC04], "pld [r1, #-4]"),
    ] {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        assert_eq!(exec(&mut cpu, &mut bus), text);
        assert_eq!(cpu.regs.pc, 0x104);
    }
}

// ---- dual, multiple ----

#[test]
fn ldrd_strd() {
    // strd r0, r1, [r2, #8]!
    let (mut cpu, mut bus) = cpu_with_code(&[0xE9E2, 0x0102]);
    cpu.regs.r[..3].copy_from_slice(&[0x11, 0x22, A]);
    assert_eq!(exec(&mut cpu, &mut bus), "strd r0, r1, [r2, #8]!");
    assert_eq!((get(&bus, A + 8), get(&bus, A + 12)), (0x11, 0x22));
    assert_eq!(cpu.regs.r[2], A + 8);
    // ldrd r3, r4, [r2, #-8]  (load back what was stored)
    let (mut cpu, mut bus) = cpu_with_code(&[0xE952, 0x3402]);
    cpu.regs.r[2] = A + 8;
    put(&mut bus, A, 0xAA);
    put(&mut bus, A + 4, 0xBB);
    assert_eq!(exec(&mut cpu, &mut bus), "ldrd r3, r4, [r2, #-8]");
    assert_eq!((cpu.regs.r[3], cpu.regs.r[4]), (0xAA, 0xBB));
    // ldrd r0, r1, [r2], #8
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8F2, 0x0102]);
    cpu.regs.r[2] = A;
    put(&mut bus, A, 1);
    put(&mut bus, A + 4, 2);
    assert_eq!(exec(&mut cpu, &mut bus), "ldrd r0, r1, [r2], #8");
    assert_eq!((cpu.regs.r[0], cpu.regs.r[1], cpu.regs.r[2]), (1, 2, A + 8));
    // ldrd r0, r1, [pc, #8]: Align(0x104, 4) + 8
    let (mut cpu, mut bus) = cpu_with_code(&[0xE9DF, 0x0102]);
    bus.code[0x10C..0x110].copy_from_slice(&5u32.to_le_bytes());
    bus.code[0x110..0x114].copy_from_slice(&6u32.to_le_bytes());
    assert_eq!(exec(&mut cpu, &mut bus), "ldrd r0, r1, [pc, #8]");
    assert_eq!((cpu.regs.r[0], cpu.regs.r[1]), (5, 6));
}

#[test]
fn ldrd_requires_word_alignment() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xE9D2, 0x0100]);
    cpu.regs.r[2] = A + 2;
    assert_eq!(step_hardfault(&mut cpu, &mut bus), UNALIGNED);
}

#[test]
fn push_pop_and_ldm_stm_forms() {
    // push.w {r4, lr}; pop.w {r4, pc}
    let (mut cpu, mut bus) = cpu_with_code(&[0xE92D, 0x4010, 0xE8BD, 0x8010]);
    cpu.regs.r[4] = 0x44;
    cpu.regs.lr = 0x201;
    assert_eq!(exec(&mut cpu, &mut bus), "push.w {r4, lr}");
    assert_eq!(cpu.regs.msp.s, 0x2000_1000 - 8);
    cpu.regs.r[4] = 0;
    assert_eq!(exec(&mut cpu, &mut bus), "pop.w {r4, pc}");
    assert_eq!((cpu.regs.r[4], cpu.regs.pc), (0x44, 0x200));
    assert_eq!(cpu.regs.msp.s, 0x2000_1000);

    // stmia.w r0!, {r1, r2}; ldmia.w r0!, ... ; stmdb / ldmdb
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8A0, 0x0006, 0xE920, 0x0006]);
    cpu.regs.r[..3].copy_from_slice(&[A, 0x11, 0x22]);
    assert_eq!(exec(&mut cpu, &mut bus), "stmia.w r0!, {r1, r2}");
    assert_eq!(
        (get(&bus, A), get(&bus, A + 4), cpu.regs.r[0]),
        (0x11, 0x22, A + 8)
    );
    assert_eq!(exec(&mut cpu, &mut bus), "stmdb r0!, {r1, r2}");
    assert_eq!(
        (get(&bus, A), get(&bus, A + 4), cpu.regs.r[0]),
        (0x11, 0x22, A)
    );

    let (mut cpu, mut bus) = cpu_with_code(&[0xE8B0, 0x0006, 0xE930, 0x0018]);
    cpu.regs.r[0] = A;
    put(&mut bus, A, 7);
    put(&mut bus, A + 4, 8);
    assert_eq!(exec(&mut cpu, &mut bus), "ldmia.w r0!, {r1, r2}");
    assert_eq!((cpu.regs.r[1], cpu.regs.r[2], cpu.regs.r[0]), (7, 8, A + 8));
    // ldmdb r0!, {r3, r4}
    put(&mut bus, A, 3);
    put(&mut bus, A + 4, 4);
    assert_eq!(exec(&mut cpu, &mut bus), "ldmdb r0!, {r3, r4}");
    assert_eq!((cpu.regs.r[3], cpu.regs.r[4], cpu.regs.r[0]), (3, 4, A));
    // Without writeback: stmia.w r0, {r1, r2}
    let (mut cpu, mut bus) = cpu_with_code(&[0xE880, 0x0006]);
    cpu.regs.r[0] = A;
    assert_eq!(exec(&mut cpu, &mut bus), "stmia.w r0, {r1, r2}");
    assert_eq!(cpu.regs.r[0], A);
}

// ---- exclusives, acquire/release, table branch ----

#[test]
fn strex_fails_without_monitor() {
    // strex r2, r0, [r1]
    let (mut cpu, mut bus) = cpu_with_code(&[0xE841, 0x0200]);
    cpu.regs.r[0] = 0x55;
    cpu.regs.r[1] = A;
    cpu.regs.r[2] = 0xFF;
    put(&mut bus, A, 0x99);
    assert_eq!(exec(&mut cpu, &mut bus), "strex r2, r0, [r1]");
    assert_eq!(cpu.regs.r[2], 1);
    assert_eq!(get(&bus, A), 0x99);
}

#[test]
fn strex_succeeds_after_ldrex() {
    // ldrex r3, [r1]; strex r2, r0, [r1]; strex r2, r0, [r1]
    let (mut cpu, mut bus) = cpu_with_code(&[0xE851, 0x3F00, 0xE841, 0x0200, 0xE841, 0x0200]);
    cpu.regs.r[0] = 0x55;
    cpu.regs.r[1] = A;
    put(&mut bus, A, 0x99);
    assert_eq!(exec(&mut cpu, &mut bus), "ldrex r3, [r1]");
    assert_eq!(cpu.regs.r[3], 0x99);
    exec(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.r[2], get(&bus, A)), (0, 0x55));
    // The monitor was cleared by the successful store.
    put(&mut bus, A, 0x99);
    exec(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.r[2], get(&bus, A)), (1, 0x99));
}

#[test]
fn clrex_and_other_address_defeat_strex() {
    // ldrex r3, [r1]; clrex; strex r2, r0, [r1]
    let (mut cpu, mut bus) = cpu_with_code(&[0xE851, 0x3F00, 0xF3BF, 0x8F2F, 0xE841, 0x0200]);
    cpu.regs.r[1] = A;
    cpu.regs.r[0] = 1;
    exec(&mut cpu, &mut bus);
    assert_eq!(exec(&mut cpu, &mut bus), "clrex");
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[2], 1);
    // ldrex at A, strex at A+4 fails.
    let (mut cpu, mut bus) = cpu_with_code(&[0xE851, 0x3F00, 0xE841, 0x0200]);
    cpu.regs.r[1] = A;
    exec(&mut cpu, &mut bus);
    cpu.regs.r[1] = A + 4;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[2], 1);
}

#[test]
fn exclusive_byte_half_and_acquire_release() {
    let cases: [([u16; 2], &str); 10] = [
        ([0xE8D1, 0x0F4F], "ldrexb r0, [r1]"),
        ([0xE8D1, 0x0F5F], "ldrexh r0, [r1]"),
        ([0xE8D1, 0x0F8F], "ldab r0, [r1]"),
        ([0xE8D1, 0x0F9F], "ldah r0, [r1]"),
        ([0xE8D1, 0x0FAF], "lda r0, [r1]"),
        ([0xE8D1, 0x0FCF], "ldaexb r0, [r1]"),
        ([0xE8D1, 0x0FDF], "ldaexh r0, [r1]"),
        ([0xE8D1, 0x0FEF], "ldaex r0, [r1]"),
        ([0xE8D1, 0x0F4F], "ldrexb r0, [r1]"),
        ([0xE851, 0x0F01], "ldrex r0, [r1, #4]"),
    ];
    for (code, text) in cases {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        cpu.regs.r[1] = A;
        put(&mut bus, A, 0x1234_5678);
        put(&mut bus, A + 4, 0x1234_5678);
        assert_eq!(exec(&mut cpu, &mut bus), text);
        let want = if text.contains("b ") || text.contains("exb") {
            0x78
        } else if text.contains("h ") || text.contains("exh") {
            0x5678
        } else {
            0x1234_5678
        };
        assert_eq!(cpu.regs.r[0], want, "{text}");
    }
    // stl / stlb / stlh
    for (code, text, want) in [
        ([0xE8C1, 0x0FAF], "stl r0, [r1]", 0xAABB_CCDDu32),
        ([0xE8C1, 0x0F8F], "stlb r0, [r1]", 0xDD),
        ([0xE8C1, 0x0F9F], "stlh r0, [r1]", 0xCCDD),
    ] {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        cpu.regs.r[1] = A;
        cpu.regs.r[0] = 0xAABB_CCDD;
        assert_eq!(exec(&mut cpu, &mut bus), text);
        assert_eq!(get(&bus, A), want);
    }
    // stlex r2, r0, [r1] after ldaex r3, [r1]; strexb/strexh spelling.
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8D1, 0x3FEF, 0xE8C1, 0x0FE2]);
    cpu.regs.r[1] = A;
    cpu.regs.r[0] = 9;
    exec(&mut cpu, &mut bus);
    assert_eq!(exec(&mut cpu, &mut bus), "stlex r2, r0, [r1]");
    assert_eq!((cpu.regs.r[2], get(&bus, A)), (0, 9));
    for (code, text) in [
        ([0xE8C1, 0x0F42], "strexb r2, r0, [r1]"),
        ([0xE8C1, 0x0F52], "strexh r2, r0, [r1]"),
        ([0xE8C1, 0x0FC2], "stlexb r2, r0, [r1]"),
        ([0xE8C1, 0x0FD2], "stlexh r2, r0, [r1]"),
    ] {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        cpu.regs.r[1] = A;
        assert_eq!(exec(&mut cpu, &mut bus), text);
    }
}

#[test]
fn exclusive_requires_alignment() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xE851, 0x3F00]);
    cpu.regs.r[1] = A + 1;
    assert_eq!(step_hardfault(&mut cpu, &mut bus), UNALIGNED);
}

#[test]
fn tbh_jump() {
    // tbh [pc, r0, lsl #1] with the table right after the instruction.
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8DF, 0xF010, 0x0000, 0x0010]);
    cpu.regs.r[0] = 1;
    assert_eq!(exec(&mut cpu, &mut bus), "tbh [pc, r0, lsl #1]");
    assert_eq!(cpu.regs.pc, 0x104 + 2 * 0x10);
}

#[test]
fn tbb_jump() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xE8DF, 0xF000, 0x0500]);
    cpu.regs.r[0] = 1;
    assert_eq!(exec(&mut cpu, &mut bus), "tbb [pc, r0]");
    assert_eq!(cpu.regs.pc, 0x104 + 2 * 5);
}

// ---- branches ----

#[test]
fn bl_sets_lr_thumb_bit() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xF000, 0xF880]);
    assert_eq!(exec(&mut cpu, &mut bus), "bl 0x204");
    assert_eq!(cpu.regs.pc, 0x204);
    assert_eq!(cpu.regs.lr, 0x105);
    // bl .-4 (negative offset): S = 1, J1 = J2 = 1.
    let (mut cpu, mut bus) = cpu_with_code(&[0xF7FF, 0xFFFE]);
    assert_eq!(exec(&mut cpu, &mut bus), "bl 0x100");
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (0x100, 0x105));
}

#[test]
fn b_t4_long_negative_offset() {
    // S = 1, J1 = J2 = 1: I1 = I2 = 1 so the offset is -0x20_0000 from PC + 4.
    let (mut cpu, mut bus) = cpu_with_code(&[0xF600, 0xB800]);
    let t = dis(&cpu, &mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, (0x104u32).wrapping_sub(0x20_0000));
    assert_eq!(t, "b.w 0xffe00104");
    // b.w . : offset -4
    let (mut cpu, mut bus) = cpu_with_code(&[0xF7FF, 0xBFFE]);
    assert_eq!(exec(&mut cpu, &mut bus), "b.w 0x100");
    assert_eq!(cpu.regs.pc, 0x100);
    // S = 0, J1 = 0, J2 = 1: I1 = 1, I2 = 0 -> +0x80_0000.
    let (mut cpu, mut bus) = cpu_with_code(&[0xF000, 0x9800]);
    assert_eq!(exec(&mut cpu, &mut bus), "b.w 0x800104");
    assert_eq!(cpu.regs.pc, 0x80_0104);
}

#[test]
fn b_t3_conditional() {
    // beq.w +0x20 (imm11 = 0x10)
    let (mut cpu, mut bus) = cpu_with_code(&[0xF000, 0x8010]);
    cpu.regs.xpsr.z = true;
    assert_eq!(exec(&mut cpu, &mut bus), "beq.w 0x124");
    assert_eq!(cpu.regs.pc, 0x124);
    let (mut cpu, mut bus) = cpu_with_code(&[0xF000, 0x8010]);
    cpu.regs.xpsr.z = false;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x104);
    // bne.w -4: S = 1, cond = 1, imm6 = 0x3F, imm11 = 0x7FE, J1 = J2 = 1
    let (mut cpu, mut bus) = cpu_with_code(&[0xF47F, 0xAFFE]);
    assert_eq!(dis(&cpu, &mut bus), "bne.w 0x100");
    cpu.step(&mut bus);
    assert_eq!(cpu.regs.pc, 0x100);
    // S = 1, J1 = J2 = 0, all other fields zero: offset -0x10_0000.
    let (cpu, mut bus) = cpu_with_code(&[0xF400, 0x8000]);
    assert_eq!(dis(&cpu, &mut bus), "beq.w 0xfff00104");
}

// ---- system ----

#[test]
fn mrs_ipsr_zero_in_thread() {
    // mrs r0, ipsr
    let (mut cpu, mut bus) = cpu_with_code(&[0xF3EF, 0x8005]);
    cpu.regs.r[0] = 0xFFFF;
    assert_eq!(exec(&mut cpu, &mut bus), "mrs r0, IPSR");
    assert_eq!(cpu.regs.r[0], 0);
    let (mut cpu, mut bus) = cpu_with_code(&[0xF3EF, 0x8005]);
    cpu.regs.xpsr.exception = 11;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[0], 11);
}

#[test]
fn msr_mrs_apsr() {
    // msr APSR_nzcvq, r0 ; mrs r1, apsr
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8800, 0xF3EF, 0x8100]);
    cpu.regs.r[0] = 0xA800_0000;
    assert_eq!(exec(&mut cpu, &mut bus), "msr APSR_nzcvq, r0");
    let x = cpu.regs.xpsr;
    assert_eq!((x.n, x.z, x.c, x.v, x.q), (true, false, true, false, true));
    assert_eq!(exec(&mut cpu, &mut bus), "mrs r1, APSR");
    assert_eq!(cpu.regs.r[1], 0xA800_0000);
    // GE only: msr APSR_g, r0
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8400]);
    cpu.regs.r[0] = 0xFFFF_FFFF;
    assert_eq!(exec(&mut cpu, &mut bus), "msr APSR_g, r0");
    assert_eq!((cpu.regs.xpsr.ge, cpu.regs.xpsr.n), (0xF, false));
}

#[test]
fn msr_mrs_special_registers() {
    // (msr hw2, mrs hw2, check)
    let (mut cpu, mut bus) = cpu_with_code(&[
        0xF380, 0x8808, // msr MSP, r0
        0xF380, 0x8809, // msr PSP, r0
        0xF380, 0x8810, // msr PRIMASK, r0
        0xF380, 0x8813, // msr FAULTMASK, r0
        0xF380, 0x880A, // msr MSPLIM, r0
        0xF3EF, 0x8108, // mrs r1, MSP
    ]);
    cpu.regs.r[0] = 0x2000_0F07;
    assert_eq!(exec(&mut cpu, &mut bus), "msr MSP, r0");
    assert_eq!(cpu.regs.msp.s, 0x2000_0F04);
    assert_eq!(exec(&mut cpu, &mut bus), "msr PSP, r0");
    assert_eq!(cpu.regs.psp.s, 0x2000_0F04);
    assert_eq!(exec(&mut cpu, &mut bus), "msr PRIMASK, r0");
    assert!(cpu.regs.primask.s);
    assert_eq!(exec(&mut cpu, &mut bus), "msr FAULTMASK, r0");
    assert!(cpu.regs.faultmask.s);
    assert_eq!(exec(&mut cpu, &mut bus), "msr MSPLIM, r0");
    assert_eq!(cpu.regs.msplim.s, 0x2000_0F00);
    assert_eq!(exec(&mut cpu, &mut bus), "mrs r1, MSP");
    assert_eq!(cpu.regs.r[1], 0x2000_0F04);
}

#[test]
fn msr_control_spsel_switches_stack() {
    // msr CONTROL, r0 (SPSEL = 1) ; mrs r1, CONTROL
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8814, 0xF3EF, 0x8114]);
    cpu.regs.psp.s = 0x2000_0800;
    cpu.regs.r[0] = 2;
    assert_eq!(exec(&mut cpu, &mut bus), "msr CONTROL, r0");
    assert_eq!(cpu.regs.sp(), 0x2000_0800);
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 2);
    // In Handler mode the SPSEL write is ignored.
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8814]);
    cpu.regs.xpsr.exception = 3;
    cpu.regs.r[0] = 2;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.control.s & 2, 0);
}

#[test]
fn msr_basepri_max_only_lowers() {
    let (mut cpu, mut bus) = cpu_with_code(&[
        0xF380, 0x8812, 0xF380, 0x8812, 0xF380, 0x8812, 0xF380, 0x8812, 0xF380, 0x8811,
    ]);
    // 0 -> 0x40 (BASEPRI was 0)
    cpu.regs.r[0] = 0x40;
    assert_eq!(exec(&mut cpu, &mut bus), "msr BASEPRI_MAX, r0");
    assert_eq!(cpu.regs.basepri.s, 0x40);
    // 0x60 is not lower than 0x40: ignored
    cpu.regs.r[0] = 0x60;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.basepri.s, 0x40);
    // 0x20 is lower: taken
    cpu.regs.r[0] = 0x20;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.basepri.s, 0x20);
    // 0 never lowers through BASEPRI_MAX
    cpu.regs.r[0] = 0;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.basepri.s, 0x20);
    // plain BASEPRI write is unconditional
    cpu.regs.r[0] = 0x80;
    assert_eq!(exec(&mut cpu, &mut bus), "msr BASEPRI, r0");
    assert_eq!(cpu.regs.basepri.s, 0x80);
}

#[test]
fn msr_ns_from_nonsecure_is_ignored() {
    // msr MSP_NS, r0 ; mrs r1, MSP_NS
    let code = [0xF380, 0x8888, 0xF3EF, 0x8188];
    let (mut cpu, mut bus) = cpu_with_code(&code);
    cpu.regs.sec = Sec::NonSecure;
    cpu.regs.msp.ns = 0x2000_0100;
    cpu.regs.msp.s = 0x2000_0200;
    cpu.regs.r[0] = 0x2000_0F00;
    cpu.regs.r[1] = 0x77;
    assert_eq!(exec(&mut cpu, &mut bus), "msr MSP_NS, r0");
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.msp.ns, 0x2000_0100);
    assert_eq!(cpu.regs.msp.s, 0x2000_0200);
    assert_eq!(cpu.regs.r[1], 0);
    // From Secure the same instructions reach the Non-Secure bank.
    let (mut cpu, mut bus) = cpu_with_code(&code);
    cpu.regs.r[0] = 0x2000_0F00;
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.msp.ns, 0x2000_0F00);
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0x2000_0F00);
    // Other _NS registers: PRIMASK_NS (0x90), BASEPRI_NS (0x91), CONTROL_NS (0x94).
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8890, 0xF380, 0x8891, 0xF380, 0x8894]);
    // BASEPRI keeps only the implemented priority bits (0x20 of 0x21).
    cpu.regs.r[0] = 0x21;
    assert_eq!(exec(&mut cpu, &mut bus), "msr PRIMASK_NS, r0");
    assert_eq!(exec(&mut cpu, &mut bus), "msr BASEPRI_NS, r0");
    assert_eq!(exec(&mut cpu, &mut bus), "msr CONTROL_NS, r0");
    assert!(cpu.regs.primask.ns && !cpu.regs.primask.s);
    assert_eq!(cpu.regs.basepri.ns, 0x20);
    assert_eq!(cpu.regs.control.ns, 1);
    assert_eq!(cpu.regs.control.s, 0);
}

#[test]
fn msr_unprivileged_is_ignored_except_apsr() {
    let (mut cpu, mut bus) = cpu_with_code(&[0xF380, 0x8810, 0xF380, 0x8800]);
    cpu.regs.control.s = 1;
    cpu.regs.r[0] = 0xF000_0000;
    exec(&mut cpu, &mut bus);
    assert!(!cpu.regs.primask.s);
    exec(&mut cpu, &mut bus);
    assert!(cpu.regs.xpsr.n && cpu.regs.xpsr.v);
}

#[test]
fn barriers_hints_and_udf() {
    for (code, text) in [
        ([0xF3BF, 0x8F4F], "dsb sy"),
        ([0xF3BF, 0x8F5F], "dmb sy"),
        ([0xF3BF, 0x8F6F], "isb sy"),
        ([0xF3BF, 0x8F5B], "dmb ish"),
        ([0xF3AF, 0x8000], "nop.w"),
        ([0xF3AF, 0x8001], "yield.w"),
        ([0xF3AF, 0x8004], "sev.w"),
    ] {
        let (mut cpu, mut bus) = cpu_with_code(&code);
        assert_eq!(exec(&mut cpu, &mut bus), text);
        assert_eq!(cpu.regs.pc, 0x104);
    }
    let (mut cpu, mut bus) = cpu_with_code(&[0xF3AF, 0x8003]);
    assert_eq!(dis(&cpu, &mut bus), "wfi.w");
    assert_eq!(cpu.step(&mut bus), StepResult::Sleep);
    // udf.w #0x1234
    let (mut cpu, mut bus) = cpu_with_code(&[0xF7F1, 0xA234]);
    assert_eq!(dis(&cpu, &mut bus), "udf.w #4660");
    assert_eq!(step_hardfault(&mut cpu, &mut bus), 1 << 16, "UNDEFINSTR");
}

#[test]
fn vldr_decodes_as_coprocessor() {
    // vldr s0, [r0]
    let i = decode32(0xED90, 0x0A00);
    assert!(format!("{i:?}").starts_with("Coprocessor"), "{i:?}");
    assert_eq!(disasm(&i, 0, 4), ".inst.w 0xed900a00 @ coprocessor");
    for hw1 in [0xEC00u16, 0xEE00, 0xFC00, 0xFE00, 0xEDFF, 0xFFFF] {
        assert!(
            format!("{:?}", decode32(hw1, 0x0A00)).starts_with("Coprocessor"),
            "{hw1:#x}"
        );
    }
    // Executing it raises NOCP rather than being skipped.
    let (mut cpu, mut bus) = cpu_with_code(&[0xED90, 0x0A00]);
    assert_eq!(step_hardfault(&mut cpu, &mut bus), 1 << 19, "NOCP");
}

// ---- UNPREDICTABLE / UNDEFINED encodings ----

#[test]
fn rn_pc_with_writeback_is_undefined() {
    // str.w r0, [pc, #4]! and strt/str with Rn = PC
    assert!(is_undef(0xF84F, 0x0F04));
    assert!(is_undef(0xF84F, 0x0B04));
    assert!(is_undef(0xF8CF, 0x0004));
    // ldr.w r0, [pc, #-0xB04] is the literal form: it decodes and must not panic when run.
    assert_eq!(
        decode32(0xF85F, 0x0B04),
        Instr::LdrLit {
            rt: 0,
            imm: 0xB04,
            add: false
        }
    );
    let (mut cpu, mut bus) = cpu_with_code(&[0xF85F, 0x0B04]);
    let _ = cpu.step(&mut bus);
}

#[test]
fn writeback_with_rt_equals_rn_is_undefined() {
    assert!(is_undef(0xF851, 0x1B04)); // ldr r1, [r1], #4
    assert!(is_undef(0xF841, 0x1F04)); // str r1, [r1, #4]!
    assert!(is_undef(0xE9F1, 0x1002)); // ldrd r1, r0, [r1, #8]!
    assert!(is_undef(0xE8B0, 0x0003)); // ldmia r0!, {r0, r1}
    assert!(is_undef(0xE8A0, 0x0003)); // stmia r0!, {r0, r1}
}

#[test]
fn sp_pc_operand_rules() {
    assert!(is_undef(0xF8C1, 0xF000)); // str.w pc, [r1]
    assert!(is_undef(0xF891, 0xD000)); // ldrb.w sp, [r1]
    assert!(is_undef(0xF8A1, 0xD000)); // strh.w sp, [r1]
    assert!(is_undef(0xE89F, 0x0006)); // ldm pc
    assert!(is_undef(0xE880, 0x8006)); // stm with pc in list
    assert!(is_undef(0xE880, 0x2006)); // stm with sp in list
    assert!(is_undef(0xE890, 0xC006)); // ldm with pc and lr
    assert!(is_undef(0xE8BD, 0x2010)); // pop.w with sp
    assert!(is_undef(0xE92D, 0x0010)); // push.w with one register
    assert!(is_undef(0xE9C2, 0x1101)); // strd t == t2
    assert!(is_undef(0xE9C2, 0xD001)); // strd sp
    assert!(is_undef(0xE9CF, 0x0102)); // strd [pc]
    assert!(is_undef(0xE8DF, 0xF00D)); // tbb rm = sp
    assert!(is_undef(0xE8DF, 0xF01F)); // tbh rm = pc
    assert!(is_undef(0xE841, 0x0100)); // strex d == n
    assert!(is_undef(0xF38D, 0x8808)); // msr msp, sp
    assert!(is_undef(0xF3EF, 0x8F08)); // mrs pc, msp
    assert!(is_undef(0xF3EF, 0x8D08)); // mrs sp, msp
}

#[test]
fn unsupported_32bit_encodings_stay_undefined() {
    assert!(is_undef(0xE800, 0x0000)); // srs
    assert!(is_undef(0xF7F0, 0xA000)); // udf.w
    assert!(is_undef(0xF3BF, 0x8F00)); // reserved barrier op
}

#[test]
fn unprivileged_mrs_reads_control_but_not_primask() {
    // mrs r0, CONTROL ; mrs r1, PRIMASK ; msr CONTROL, r2 (ignored)
    let (mut cpu, mut bus) = cpu_with_code(&[0xF3EF, 0x8014, 0xF3EF, 0x8110, 0xF382, 0x8814]);
    cpu.regs.control.s = 1;
    cpu.regs.primask.s = true;
    cpu.regs.r[1] = 0x77;
    cpu.regs.r[2] = 0;
    assert_eq!(exec(&mut cpu, &mut bus), "mrs r0, CONTROL");
    assert_eq!(cpu.regs.r[0], 1);
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0);
    exec(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.control.s, 1);
}
