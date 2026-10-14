//! Security Extension (TrustZone): attribution (SAU + IDAU), SG/BXNS/BLXNS/TT, the Non-secure
//! alias of the SCS, banked SCS state, ITNS routing and secure exception stacking.

use armv8m::{
    Access, AccessKind, Attr, Bus, BusError, Cpu, Idau, Instr, Sec, StepResult, decode16, decode32,
    disasm,
};

// ---- Test bus: four 64 KiB regions on the Arm TrustZone layout ----

/// NS code, S code, NS RAM, S RAM.
const REGIONS: [u32; 4] = [0x0000_0000, 0x1000_0000, 0x2000_0000, 0x3000_0000];
const SIZE: u32 = 0x1_0000;

/// Memory for the four regions, recording every access the CPU makes.
struct TzBus {
    mem: Vec<Vec<u8>>,
    log: Vec<(u32, Access)>,
}

impl TzBus {
    fn new() -> Self {
        Self {
            mem: vec![vec![0; SIZE as usize]; REGIONS.len()],
            log: Vec::new(),
        }
    }

    fn slot(&mut self, addr: u32) -> Result<&mut u8, BusError> {
        for (i, &base) in REGIONS.iter().enumerate() {
            if (base..base + SIZE).contains(&addr) {
                return Ok(&mut self.mem[i][(addr - base) as usize]);
            }
        }
        Err(BusError::Decode)
    }

    fn read(&mut self, addr: u32, n: u32, a: Access) -> Result<u32, BusError> {
        self.log.push((addr, a));
        let mut v = 0;
        for i in 0..n {
            v |= u32::from(*self.slot(addr.wrapping_add(i))?) << (8 * i);
        }
        Ok(v)
    }

    fn write(&mut self, addr: u32, v: u32, n: u32, a: Access) -> Result<(), BusError> {
        self.log.push((addr, a));
        for i in 0..n {
            *self.slot(addr.wrapping_add(i))? = (v >> (8 * i)) as u8;
        }
        Ok(())
    }

    fn code(&mut self, addr: u32, hws: &[u16]) {
        for (i, hw) in hws.iter().enumerate() {
            let a = addr + 2 * i as u32;
            *self.slot(a).unwrap() = *hw as u8;
            *self.slot(a + 1).unwrap() = (*hw >> 8) as u8;
        }
    }

    fn put(&mut self, addr: u32, v: u32) {
        for i in 0..4 {
            *self.slot(addr + i).unwrap() = (v >> (8 * i)) as u8;
        }
    }

    fn get(&mut self, addr: u32) -> u32 {
        (0..4).fold(0, |v, i| {
            v | u32::from(*self.slot(addr + i).unwrap()) << (8 * i)
        })
    }

    /// The most recent access to `addr`.
    fn last_access(&self, addr: u32) -> Access {
        self.log
            .iter()
            .rev()
            .find(|(a, _)| *a == addr)
            .map(|&(_, acc)| acc)
            .unwrap_or_else(|| panic!("no access to {addr:#x}"))
    }
}

impl Bus for TzBus {
    fn read8(&mut self, addr: u32, a: Access) -> Result<u8, BusError> {
        self.read(addr, 1, a).map(|v| v as u8)
    }
    fn read16(&mut self, addr: u32, a: Access) -> Result<u16, BusError> {
        self.read(addr, 2, a).map(|v| v as u16)
    }
    fn read32(&mut self, addr: u32, a: Access) -> Result<u32, BusError> {
        self.read(addr, 4, a)
    }
    fn write8(&mut self, addr: u32, v: u8, a: Access) -> Result<(), BusError> {
        self.write(addr, u32::from(v), 1, a)
    }
    fn write16(&mut self, addr: u32, v: u16, a: Access) -> Result<(), BusError> {
        self.write(addr, u32::from(v), 2, a)
    }
    fn write32(&mut self, addr: u32, v: u32, a: Access) -> Result<(), BusError> {
        self.write(addr, v, 4, a)
    }
}

// ---- Registers and constants ----

const SAU_CTRL: u32 = 0xE000_EDD0;
const SAU_TYPE: u32 = 0xE000_EDD4;
const SAU_RNR: u32 = 0xE000_EDD8;
const SAU_RBAR: u32 = 0xE000_EDDC;
const SAU_RLAR: u32 = 0xE000_EDE0;
const SFSR: u32 = 0xE000_EDE4;
const SFAR: u32 = 0xE000_EDE8;
/// Offset of the Non-secure alias of the SCS.
const NS_ALIAS: u32 = 0x0002_0000;

const SYST_RVR: u32 = 0xE000_E014;
const NVIC_ISER0: u32 = 0xE000_E100;
const NVIC_ISPR0: u32 = 0xE000_E200;
const NVIC_ITNS0: u32 = 0xE000_E380;
const NVIC_IPR0: u32 = 0xE000_E400;
const ICSR: u32 = 0xE000_ED04;
const VTOR: u32 = 0xE000_ED08;
const AIRCR: u32 = 0xE000_ED0C;
const SHPR1: u32 = 0xE000_ED18;
const SHCSR: u32 = 0xE000_ED24;
const CFSR: u32 = 0xE000_ED28;
const HFSR: u32 = 0xE000_ED2C;
const MPU_CTRL: u32 = 0xE000_ED94;
const MPU_RNR: u32 = 0xE000_ED98;
const MPU_RBAR: u32 = 0xE000_ED9C;
const MPU_RLAR: u32 = 0xE000_EDA0;

// SFSR bits.
const INVEP: u32 = 1 << 0;
const INVIS: u32 = 1 << 1;
const INVER: u32 = 1 << 2;
const AUVIOL: u32 = 1 << 3;
const INVTRAN: u32 = 1 << 4;
const SFARVALID: u32 = 1 << 6;

const UNDEFINSTR: u32 = 1 << 16;
const FORCED: u32 = 1 << 30;
const USGFAULTENA: u32 = 1 << 18;
const SECUREFAULTENA: u32 = 1 << 19;

// TT response bits.
const MRVALID: u32 = 1 << 16;
const SRVALID: u32 = 1 << 17;
const TT_R: u32 = 1 << 18;
const TT_RW: u32 = 1 << 19;
const TT_NSR: u32 = 1 << 20;
const TT_NSRW: u32 = 1 << 21;
const TT_S: u32 = 1 << 22;
const IRVALID: u32 = 1 << 23;

/// Secure stack top (S RAM) and Non-secure stack top (NS RAM).
const SP_S: u32 = 0x3000_1000;
const SP_NS: u32 = 0x2000_1000;
/// Non-secure callable veneer region.
const NSC: u32 = 0x1000_8000;

const NOP: u16 = 0xBF00;
const B_SELF: u16 = 0xE7FE;
const BX_LR: u16 = 0x4770;
const BLX_R0: u16 = 0x4780;
const BXNS_LR: u16 = 0x4774;
const BLXNS_R0: u16 = 0x4784;
const SG: [u16; 2] = [0xE97F, 0xE97F];
const LDR_R1_R0: u16 = 0x6801;
const STR_R1_R0: u16 = 0x6001;
const UDF0: u16 = 0xDE00;

/// A Secure CPU on a TF-M-like SAU layout:
/// region 0 NS code `0x0000_0000..=0x0000_FFFF`, region 1 NS RAM `0x2000_0000..=0x2000_FFFF`,
/// region 2 NSC `0x1000_8000..=0x1000_80FF`; everything else is Secure (default IDAU: bit 28).
/// Secure vectors at `0x1000_0000`, Non-secure vectors at 0; SecureFault enabled; PC is
/// `0x1000_0100` in Secure Thread mode.
fn tz() -> (Cpu, TzBus) {
    let mut cpu = Cpu::new();
    for (n, (base, limit, nsc)) in [
        (0x0000_0000u32, 0x0000_FFE0u32, false),
        (0x2000_0000, 0x2000_FFE0, false),
        (NSC, NSC + 0xE0, true),
    ]
    .into_iter()
    .enumerate()
    {
        cpu.ppb_write32(SAU_RNR, n as u32);
        cpu.ppb_write32(SAU_RBAR, base);
        cpu.ppb_write32(SAU_RLAR, limit | (u32::from(nsc) << 1) | 1);
    }
    cpu.ppb_write32(SAU_CTRL, 1);
    cpu.set_vtor(Sec::Secure, 0x1000_0000);
    cpu.set_vtor(Sec::NonSecure, 0x0000_0000);
    cpu.ppb_write32(SHCSR, SECUREFAULTENA);
    cpu.regs.msp.s = SP_S;
    cpu.regs.msp.ns = SP_NS;
    cpu.regs.pc = 0x1000_0100;
    (cpu, TzBus::new())
}

/// Point vector `n` of the table at `table` at a Thumb handler at `addr` containing `hws`.
fn handler(bus: &mut TzBus, table: u32, n: u32, addr: u32, hws: &[u16]) {
    bus.put(table + 4 * n, addr | 1);
    bus.code(addr, hws);
}

fn s_handler(bus: &mut TzBus, n: u32, addr: u32, hws: &[u16]) {
    handler(bus, 0x1000_0000, n, addr, hws);
}

fn ns_handler(bus: &mut TzBus, n: u32, addr: u32, hws: &[u16]) {
    handler(bus, 0, n, addr, hws);
}

/// Continue in Non-secure Thread mode at `pc`.
fn enter_ns(cpu: &mut Cpu, pc: u32) {
    cpu.regs.sec = Sec::NonSecure;
    cpu.regs.pc = pc;
}

fn step(cpu: &mut Cpu, bus: &mut TzBus) {
    let r = cpu.step(bus);
    assert!(matches!(r, StepResult::Executed { .. }), "{r:?}");
}

fn ipsr(cpu: &Cpu) -> u16 {
    cpu.regs.xpsr.exception
}

// ---- Plan tests ----

#[test]
fn sg_entry_from_ns() {
    let (mut cpu, mut bus) = tz();
    // NS: blx r0 ; nop          veneer: sg ; sg ; bxns lr
    bus.code(0x100, &[BLX_R0, NOP]);
    bus.code(NSC, &[SG[0], SG[1], SG[0], SG[1], BXNS_LR]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = NSC | 1;

    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (NSC, 0x103));
    assert_eq!(cpu.regs.sec, Sec::NonSecure);
    assert!(
        !bus.last_access(0x100).secure,
        "NS code is fetched Non-secure"
    );

    // SG in the NSC region switches to Secure state and clears LR[0].
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.sec, Sec::Secure);
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (NSC + 4, 0x102));
    assert_eq!(ipsr(&cpu), 0);
    assert!(bus.last_access(NSC).secure, "NSC memory is fetched Secure");

    // A second SG, now in Secure state, is a NOP.
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.sec, cpu.regs.lr), (Sec::Secure, 0x102));

    // bxns lr: LR[0] = 0 returns to Non-secure state.
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.sec, cpu.regs.pc), (Sec::NonSecure, 0x102));
    assert!(cpu.regs.xpsr.t);
    step(&mut cpu, &mut bus);
    assert_eq!(
        (cpu.regs.sec, cpu.regs.pc, ipsr(&cpu)),
        (Sec::NonSecure, 0x104, 0)
    );
}

#[test]
fn ns_branch_to_secure_non_nsc_securefault() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[BLX_R0]);
    bus.code(0x1000_0200, &[NOP]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = 0x1000_0201;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x1000_0200);
    // The fetch from Secure, non-NSC memory in Non-secure state raises SecureFault INVEP.
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (7, Sec::Secure));
    assert_eq!(cpu.regs.pc, 0x1000_0400);
    assert_eq!(cpu.ppb_read32(SFSR), INVEP);
    // Taken from Non-secure Thread mode on MSP_NS to a Secure exception.
    assert_eq!(cpu.regs.lr, 0xFFFF_FFB9);
    assert_eq!(cpu.regs.msp.ns, SP_NS - 0x20);
    assert_eq!(cpu.regs.msp.s, SP_S);
    assert_eq!(bus.get(SP_NS - 0x20 + 0x18), 0x1000_0200, "stacked PC");
    // SFSR is write-one-to-clear.
    cpu.ppb_write32(SFSR, INVEP);
    assert_eq!(cpu.ppb_read32(SFSR), 0);

    // An NSC address whose first instruction is not SG also faults INVEP.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[BLX_R0]);
    bus.code(NSC + 0x10, &[NOP]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = (NSC + 0x10) | 1;
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 7);
    assert_eq!(cpu.ppb_read32(SFSR), INVEP);

    // With SecureFault disabled it escalates to the Secure HardFault.
    let (mut cpu, mut bus) = tz();
    cpu.ppb_write32(SHCSR, 0);
    bus.code(0x100, &[BLX_R0]);
    s_handler(&mut bus, 3, 0x1000_0500, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = 0x1000_0201;
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (3, Sec::Secure));
    assert_eq!(cpu.regs.pc, 0x1000_0500);
    assert_eq!(cpu.ppb_read32(HFSR), FORCED);
    assert_eq!(cpu.ppb_read32(SFSR), INVEP);
}

#[test]
fn blxns_and_fnc_return() {
    let (mut cpu, mut bus) = tz();
    // Secure: blxns r0 ; nop       NS: bx lr
    bus.code(0x1000_0100, &[BLXNS_R0, NOP]);
    bus.code(0x200, &[BX_LR]);
    cpu.regs.r[0] = 0x200;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.sec, Sec::NonSecure);
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (0x200, 0xFEFF_FFFF));
    assert!(cpu.regs.xpsr.t);
    // Return address and partial xPSR on the Secure stack.
    assert_eq!(cpu.regs.msp.s, SP_S - 8);
    assert_eq!(bus.get(SP_S - 8), 0x1000_0103);
    assert_eq!(bus.get(SP_S - 4), 0);
    assert!(bus.last_access(SP_S - 8).secure);

    // FNC_RETURN pops it and returns to Secure state.
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.sec, Sec::Secure);
    assert_eq!((cpu.regs.pc, cpu.regs.msp.s), (0x1000_0102, SP_S));
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x1000_0104);

    // From Handler mode the Secure exception number is hidden (IPSR = 1) during the call.
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[BLXNS_R0, NOP]);
    bus.code(0x200, &[NOP, BX_LR]);
    cpu.regs.xpsr.exception = 11;
    cpu.regs.r[0] = 0x200;
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.sec, ipsr(&cpu)), (Sec::NonSecure, 1));
    assert_eq!(
        bus.get(SP_S - 4),
        11,
        "partial xPSR holds the exception number"
    );
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.sec, ipsr(&cpu)), (Sec::Secure, 11));
    assert_eq!(cpu.regs.pc, 0x1000_0102);

    // A target with bit 0 set stays in Secure state (plain BLX).
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[BLXNS_R0]);
    cpu.regs.r[0] = 0x1000_0201;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.sec, Sec::Secure);
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (0x1000_0200, 0x1000_0103));
    assert_eq!(cpu.regs.msp.s, SP_S);
}

#[test]
fn tt_reports_region() {
    let (mut cpu, mut bus) = tz();
    // tt r2, r1 ; tt r3, r4 ; tt r5, r6 ; tta r7, r1
    bus.code(
        0x1000_0100,
        &[
            0xE841, 0xF200, 0xE844, 0xF300, 0xE846, 0xF500, 0xE841, 0xF780,
        ],
    );
    // NS MPU (through the NS alias): region 3 = NS RAM, read-only at any privilege.
    cpu.ppb_write32(MPU_RNR + NS_ALIAS, 3);
    cpu.ppb_write32(MPU_RBAR + NS_ALIAS, 0x2000_0000 | (0b11 << 1));
    cpu.ppb_write32(MPU_RLAR + NS_ALIAS, 0x2000_FFE0 | 1);
    cpu.ppb_write32(MPU_CTRL + NS_ALIAS, 0b101);
    cpu.regs.r[1] = 0x2000_0010; // NS RAM: SAU region 1
    cpu.regs.r[4] = NSC + 8; // NSC: SAU region 2
    cpu.regs.r[6] = 0x3000_0000; // Secure RAM: no SAU region
    for _ in 0..4 {
        step(&mut cpu, &mut bus);
    }
    assert_eq!(
        cpu.regs.r[2],
        (1 << 8) | SRVALID | TT_R | TT_RW | TT_NSR | TT_NSRW
    );
    assert_eq!(cpu.regs.r[3], (2 << 8) | SRVALID | TT_R | TT_RW | TT_S);
    assert_eq!(cpu.regs.r[5], TT_R | TT_RW | TT_S);
    // TTA queries the Non-secure MPU.
    assert_eq!(
        cpu.regs.r[7],
        3 | MRVALID | (1 << 8) | SRVALID | TT_R | TT_NSR
    );

    // An IDAU region number is reported when the IDAU provides one.
    struct NumberedIdau;
    impl Idau for NumberedIdau {
        fn attr(&self, addr: u32) -> Option<(Attr, Option<u8>)> {
            let attr = if addr & 0x1000_0000 != 0 {
                Attr::NonSecureCallable
            } else {
                Attr::NonSecure
            };
            Some((attr, Some(0x10 + (addr >> 28) as u8)))
        }
    }
    let (mut cpu, mut bus) = tz();
    cpu.set_idau(Box::new(NumberedIdau));
    bus.code(0x1000_0100, &[0xE846, 0xF500]);
    cpu.regs.r[6] = 0x3000_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[5], (0x13 << 24) | IRVALID | TT_R | TT_RW | TT_S);
    // ... with the SAU disabled too.
    cpu.ppb_write32(SAU_CTRL, 0);
    bus.code(0x1000_0104, &[0xE846, 0xF500]);
    cpu.regs.r[5] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[5], (0x13 << 24) | IRVALID | TT_R | TT_RW | TT_S);

    // From Non-secure state only the MPU fields are reported, and TTA is UNDEFINED.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[0xE846, 0xF500, 0xE841, 0xF780]);
    s_handler(&mut bus, 3, 0x1000_0500, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[6] = NSC;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[5], TT_R | TT_RW);
    step(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 3);
    assert_eq!(cpu.ppb_read32(CFSR + NS_ALIAS), UNDEFINSTR);
}

#[test]
fn ns_irq_from_secure_thread_clears_regs() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[NOP, BX_LR]);
    for i in 0..13 {
        cpu.regs.r[i] = 0x1111_1111 * (i as u32 + 1);
    }
    cpu.regs.lr = 0x1000_0F01;
    cpu.regs.xpsr.n = true;
    cpu.regs.xpsr.z = true;
    cpu.regs.xpsr.ge = 0xF;
    let before = cpu.regs.clone();
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, 1 << 3);
    cpu.pulse_irq(3);

    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (19, Sec::NonSecure));
    assert_eq!(cpu.regs.pc, 0x400);
    assert_eq!(cpu.regs.r, [0; 13], "r0-r12 cleared");
    let x = cpu.regs.xpsr;
    assert!(!x.n && !x.z && x.ge == 0, "APSR cleared");
    // Secure background (S = 1), callee registers stacked by the default rule (DCRS = 1),
    // Non-secure exception (ES = 0).
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF8);
    // Additional state context below the basic frame on the Secure stack.
    let sp = SP_S - 0x48;
    assert_eq!(cpu.regs.msp.s, sp);
    assert_eq!(cpu.regs.msp.ns, SP_NS);
    assert_eq!(bus.get(sp), 0xFEFA_125B, "integrity signature");
    assert_eq!(bus.get(sp + 4), 0);
    for i in 0..8 {
        assert_eq!(
            bus.get(sp + 8 + 4 * i),
            before.r[4 + i as usize],
            "r{}",
            4 + i
        );
    }
    assert_eq!(bus.get(sp + 0x28), before.r[0]);
    assert_eq!(bus.get(sp + 0x28 + 0x18), 0x1000_0100);

    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[4..12], [0; 8], "still cleared in the handler");
    // bx lr restores everything, including r4-r11, and Secure state.
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::Secure));
    assert_eq!(cpu.regs.pc, 0x1000_0100);
    assert_eq!(cpu.regs.r, before.r);
    assert_eq!(cpu.regs.lr, before.lr);
    assert_eq!(cpu.regs.xpsr, before.xpsr);
    assert_eq!(cpu.regs.msp.s, SP_S);

    // A corrupted integrity signature makes the return a SecureFault (INVIS).
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[BX_LR]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, 1 << 3);
    cpu.pulse_irq(3);
    step(&mut cpu, &mut bus);
    bus.put(SP_S - 0x48, 0xFEFA_125A);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (7, Sec::Secure));
    assert_eq!(cpu.ppb_read32(SFSR), INVIS);
}

#[test]
fn ns_read_of_secure_ram_faults() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[LDR_R1_R0]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    bus.put(0x3000_0010, 0x0BAD_F00D);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = 0x3000_0010;
    cpu.regs.r[1] = 0x77;
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (7, Sec::Secure));
    assert_eq!(cpu.ppb_read32(SFSR), AUVIOL | SFARVALID);
    assert_eq!(cpu.ppb_read32(SFAR), 0x3000_0010);
    assert!(
        !bus.log.iter().any(|&(a, _)| a == 0x3000_0010),
        "the bus never sees the access"
    );
    // Precise: r1 unchanged, stacked PC is the load.
    assert_eq!(bus.get(SP_NS - 0x20 + 4), 0x77);
    assert_eq!(bus.get(SP_NS - 0x20 + 0x18), 0x100);

    // A store to the NSC region faults the same way.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[STR_R1_R0]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = NSC + 0x20;
    step(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 7);
    assert_eq!(cpu.ppb_read32(SFAR), NSC + 0x20);

    // Secure code may access Non-secure memory; the bus sees a Non-secure access.
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[LDR_R1_R0, LDR_R1_R0]);
    bus.put(0x2000_0010, 0x1234);
    cpu.regs.r[0] = 0x2000_0010;
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.r[1]), (0, 0x1234));
    let acc = bus.last_access(0x2000_0010);
    assert_eq!(acc.kind, AccessKind::Read);
    assert!(!acc.secure);
    cpu.regs.r[0] = 0x3000_0010;
    step(&mut cpu, &mut bus);
    assert!(bus.last_access(0x3000_0010).secure);
}

#[test]
fn ns_alias_scs_reads_ns_vtor() {
    let (mut cpu, mut bus) = tz();
    // Secure: ldr r1, [r0] ; str r2, [r0] ; ldr r1, [r3]
    bus.code(0x1000_0100, &[LDR_R1_R0, 0x6002, 0x6819]);
    cpu.set_vtor(Sec::NonSecure, 0x0000_0400);
    cpu.regs.r[0] = VTOR + NS_ALIAS;
    cpu.regs.r[2] = 0x0000_0800;
    cpu.regs.r[3] = VTOR;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0x400, "alias reads VTOR_NS");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.scs.scb.vtor.ns, 0x800, "alias writes VTOR_NS");
    assert_eq!(cpu.scs.scb.vtor.s, 0x1000_0000);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0x1000_0000, "plain address reads VTOR_S");

    // Non-secure: the plain address is the NS view; the alias is RAZ/WI.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[0x6819, LDR_R1_R0, 0x6002]);
    cpu.set_vtor(Sec::NonSecure, 0x0000_0400);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = VTOR + NS_ALIAS;
    cpu.regs.r[2] = 0x0000_0800;
    cpu.regs.r[3] = VTOR;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0x400);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[1], 0, "alias is RAZ from Non-secure");
    step(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 0);
    assert_eq!(cpu.scs.scb.vtor.ns, 0x400, "alias is WI from Non-secure");
}

#[test]
fn sau_disabled_allns_bit() {
    let mut cpu = Cpu::new();
    // Reset: SAU disabled, ALLNS = 0: everything Secure (the IDAU can only add security).
    for addr in [0x0000_0000, 0x1000_0000, 0x2000_0000, 0x3000_0000] {
        assert_eq!(cpu.security_of(addr), Attr::Secure, "{addr:#x}");
    }
    assert_eq!(cpu.security_of(0xE000_ED00), Attr::Exempt);
    assert_eq!(cpu.ppb_read32(SAU_TYPE), 8);

    // ALLNS with the SAU disabled: the SAU says Non-secure everywhere, and the IDAU still
    // applies (most secure wins): the default IDAU's Secure half is NSC.
    cpu.ppb_write32(SAU_CTRL, 0b10);
    assert_eq!(cpu.ppb_read32(SAU_CTRL), 0b10);
    for addr in [0x0000_0000, 0x2000_0000] {
        assert_eq!(cpu.security_of(addr), Attr::NonSecure, "{addr:#x}");
    }
    for addr in [0x1000_0000, 0x3000_0000] {
        assert_eq!(cpu.security_of(addr), Attr::NonSecureCallable, "{addr:#x}");
    }
    assert_eq!(cpu.security_of(0xE000_E000), Attr::Exempt);

    // SAU enabled without regions: Secure (the default IDAU cannot make anything less secure).
    cpu.ppb_write32(SAU_CTRL, 0b01);
    assert_eq!(cpu.security_of(0x0000_0000), Attr::Secure);
    assert_eq!(cpu.security_of(0x1000_0000), Attr::Secure);
    // A Non-secure SAU region: the default IDAU decides by address bit 28.
    cpu.ppb_write32(SAU_RNR, 0);
    cpu.ppb_write32(SAU_RBAR, 0x0000_0000);
    cpu.ppb_write32(SAU_RLAR, 0x3FFF_FFE0 | 1);
    assert_eq!(cpu.security_of(0x0000_1000), Attr::NonSecure);
    assert_eq!(cpu.security_of(0x2000_0000), Attr::NonSecure);
    assert_eq!(cpu.security_of(0x1000_0000), Attr::NonSecureCallable);
    assert_eq!(cpu.security_of(0x4000_0000), Attr::Secure);
    // Region limits are inclusive at 32-byte granularity.
    assert_eq!(cpu.security_of(0x3FFF_FFFF), Attr::NonSecureCallable);
    cpu.ppb_write32(SAU_RLAR, 0x0000_0FE0 | 1);
    assert_eq!(cpu.security_of(0x0000_0FFF), Attr::NonSecure);
    assert_eq!(cpu.security_of(0x0000_1000), Attr::Secure);
    // Overlapping enabled regions make the address Secure.
    cpu.ppb_write32(SAU_RNR, 1);
    cpu.ppb_write32(SAU_RBAR, 0x0000_0800);
    cpu.ppb_write32(SAU_RLAR, 0x0000_0FE0 | 1);
    assert_eq!(cpu.security_of(0x0000_0800), Attr::Secure);
    assert_eq!(cpu.security_of(0x0000_0400), Attr::NonSecure);

    // The SAU registers are Secure-only: RAZ/WI through the Non-secure view.
    assert_eq!(cpu.ppb_read32(SAU_CTRL + NS_ALIAS), 0);
    cpu.ppb_write32(SAU_CTRL + NS_ALIAS, 0);
    assert_eq!(cpu.ppb_read32(SAU_CTRL), 1);

    // The IDAU's Exempt applies with the SAU disabled too; addresses it does not cover follow
    // the SAU alone.
    struct ExemptLow;
    impl Idau for ExemptLow {
        fn attr(&self, addr: u32) -> Option<(Attr, Option<u8>)> {
            (addr < 0x100).then_some((Attr::Exempt, None))
        }
    }
    let mut cpu = Cpu::new();
    cpu.set_idau(Box::new(ExemptLow));
    assert_eq!(cpu.security_of(0x10), Attr::Exempt);
    assert_eq!(cpu.security_of(0x1000_0100), Attr::Secure);
    cpu.ppb_write32(SAU_CTRL, 0b10);
    assert_eq!(cpu.security_of(0x10), Attr::Exempt);
    assert_eq!(cpu.security_of(0x1000_0100), Attr::NonSecure);
}

// ---- Carried items and other Security Extension behaviour ----

#[test]
fn security_instructions_decode_and_disassemble() {
    for (hw, text) in [
        (0x4704, "bxns r0"),
        (0x4774, "bxns lr"),
        (0x4784, "blxns r0"),
        (0x479C, "blxns r3"),
    ] {
        let i = decode16(hw);
        assert!(!matches!(i, Instr::Undefined { .. }), "{hw:#x}");
        assert_eq!(disasm(&i, 0, 2), text);
    }
    for ((hw1, hw2), text) in [
        ((0xE97F, 0xE97F), "sg"),
        ((0xE841, 0xF000), "tt r0, r1"),
        ((0xE841, 0xF040), "ttt r0, r1"),
        ((0xE841, 0xF080), "tta r0, r1"),
        ((0xE841, 0xF0C0), "ttat r0, r1"),
        ((0xE849, 0xF200), "tt r2, r9"),
        ((0xF3EF, 0x8098), "mrs r0, SP_NS"),
        ((0xF380, 0x8898), "msr SP_NS, r0"),
    ] {
        let i = decode32(hw1, hw2);
        assert!(!matches!(i, Instr::Undefined { .. }), "{hw1:#x} {hw2:#x}");
        assert_eq!(disasm(&i, 0, 4), text);
    }
    // UNPREDICTABLE forms: BXNS/BLXNS with SP or PC; TT with Rd = SP/PC, Rn = PC or SBZ bits.
    for hw in [0x476C, 0x477C, 0x47EC, 0x47FC] {
        assert!(matches!(decode16(hw), Instr::Undefined { .. }), "{hw:#x}");
    }
    for (hw1, hw2) in [
        (0xE84F, 0xF000),
        (0xE841, 0xFD00),
        (0xE841, 0xFF00),
        (0xE841, 0xF001),
    ] {
        assert!(
            matches!(decode32(hw1, hw2), Instr::Undefined { .. }),
            "{hw1:#x} {hw2:#x}"
        );
    }
}

#[test]
fn bxns_blxns_are_undefined_in_non_secure_state() {
    for code in [BXNS_LR, BLXNS_R0] {
        let (mut cpu, mut bus) = tz();
        bus.code(0x100, &[code]);
        s_handler(&mut bus, 3, 0x1000_0500, &[B_SELF]);
        enter_ns(&mut cpu, 0x100);
        cpu.regs.lr = 0x201;
        step(&mut cpu, &mut bus);
        assert_eq!(ipsr(&cpu), 3, "{code:#x}");
        assert_eq!(cpu.ppb_read32(CFSR + NS_ALIAS), UNDEFINSTR);
    }
}

#[test]
fn secure_branch_to_ns_memory_is_invtran() {
    // bx r0 (not BXNS) from Secure to Non-secure memory.
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[0x4700]);
    bus.code(0x200, &[NOP]);
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    cpu.regs.r[0] = 0x201;
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.pc, cpu.regs.sec), (0x200, Sec::Secure));
    step(&mut cpu, &mut bus);
    assert_eq!(ipsr(&cpu), 7);
    assert_eq!(cpu.ppb_read32(SFSR), INVTRAN);
    // Taken from Secure Thread mode: the frame is on the Secure stack.
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF9);
    assert_eq!(bus.get(SP_S - 0x20 + 0x18), 0x200);
}

#[test]
fn secure_irq_preempts_ns_thread() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[B_SELF]);
    s_handler(&mut bus, 16 + 2, 0x1000_0600, &[BX_LR]);
    cpu.ppb_write32(NVIC_ISER0, 1 << 2);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[4] = 0x4444;
    cpu.regs.r[0] = 0x1010;
    cpu.pulse_irq(2);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (18, Sec::Secure));
    // Basic frame only, on the Non-secure stack; nothing is cleared.
    assert_eq!(cpu.regs.lr, 0xFFFF_FFB9);
    assert_eq!(cpu.regs.msp.ns, SP_NS - 0x20);
    assert_eq!(cpu.regs.msp.s, SP_S);
    assert_eq!((cpu.regs.r[0], cpu.regs.r[4]), (0x1010, 0x4444));
    assert!(
        !bus.last_access(SP_NS - 0x20).secure,
        "NS frame written Non-secure"
    );
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::NonSecure));
    assert_eq!((cpu.regs.pc, cpu.regs.msp.ns), (0x100, SP_NS));
}

// ---- CONTROL.SPSEL across security states ----
//
// EXC_RETURN.SPSEL is the transitory CONTROL.SPSEL of the state the exception is taken to (the
// state named by ES); the background state's CONTROL.SPSEL is left alone and selects the stack
// that holds the frame (`LookUpSP_with_security_mode`).

/// Process stack tops for the two states.
const PSP_S: u32 = 0x3000_2000;
const PSP_NS: u32 = 0x2000_2000;

#[test]
fn ns_irq_over_secure_thread_keeps_control_ns_spsel() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[BX_LR]);
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, 1 << 3);
    cpu.regs.control.ns = 0b10;
    cpu.regs.psp.ns = PSP_NS;
    cpu.pulse_irq(3);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (19, Sec::NonSecure));
    // SPSEL is CONTROL_NS.SPSEL (the handler's state), not the Secure background's.
    assert_eq!(cpu.regs.lr, 0xFFFF_FFFC);
    assert_eq!(cpu.regs.control.ns, 0, "Handler mode runs on MSP_NS");
    assert_eq!(cpu.regs.msp.s, SP_S - 0x48, "frame on MSP_S");
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::Secure));
    assert_eq!((cpu.regs.pc, cpu.regs.msp.s), (0x1000_0100, SP_S));
    assert_eq!(cpu.regs.control.ns, 0b10, "CONTROL_NS.SPSEL restored");
    assert_eq!(cpu.regs.control.s, 0);
    assert_eq!(cpu.regs.psp.ns, PSP_NS);

    // A Secure thread on PSP_S with CONTROL_NS.SPSEL = 0: EXC_RETURN.SPSEL is 0, and the frame
    // is still found on PSP_S through CONTROL_S.SPSEL.
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[BX_LR]);
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, 1 << 3);
    cpu.regs.control.s = 0b10;
    cpu.regs.psp.s = PSP_S;
    cpu.pulse_irq(3);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF8);
    assert_eq!(cpu.regs.psp.s, PSP_S - 0x48, "frame on PSP_S");
    assert_eq!(cpu.regs.control.s, 0b10, "background CONTROL_S untouched");
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::Secure));
    assert_eq!((cpu.regs.pc, cpu.regs.psp.s), (0x1000_0100, PSP_S));
    assert_eq!((cpu.regs.control.s, cpu.regs.control.ns), (0b10, 0));
}

#[test]
fn secure_irq_over_ns_thread_keeps_control_s_spsel() {
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[B_SELF]);
    s_handler(&mut bus, 16 + 2, 0x1000_0600, &[BX_LR]);
    cpu.ppb_write32(NVIC_ISER0, 1 << 2);
    cpu.regs.control.s = 0b10;
    cpu.regs.psp.s = PSP_S;
    enter_ns(&mut cpu, 0x100);
    cpu.pulse_irq(2);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (18, Sec::Secure));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFBD);
    assert_eq!(cpu.regs.control.s, 0);
    assert_eq!(cpu.regs.msp.ns, SP_NS - 0x20, "frame on MSP_NS");
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::NonSecure));
    assert_eq!((cpu.regs.pc, cpu.regs.msp.ns), (0x100, SP_NS));
    assert_eq!(cpu.regs.control.s, 0b10, "CONTROL_S.SPSEL restored");
    assert_eq!((cpu.regs.psp.s, cpu.regs.msp.s), (PSP_S, SP_S));
}

#[test]
fn blxns_from_psp_s_survives_secure_irq_in_ns_code() {
    let (mut cpu, mut bus) = tz();
    // Secure: blxns r0 ; nop       NS: bx lr       Secure IRQ 2: bx lr
    bus.code(0x1000_0100, &[BLXNS_R0, NOP]);
    bus.code(0x200, &[BX_LR]);
    s_handler(&mut bus, 16 + 2, 0x1000_0600, &[BX_LR]);
    cpu.ppb_write32(NVIC_ISER0, 1 << 2);
    cpu.regs.control.s = 0b10;
    cpu.regs.psp.s = PSP_S;
    cpu.regs.r[0] = 0x200;
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.sec, cpu.regs.pc), (Sec::NonSecure, 0x200));
    assert_eq!(cpu.regs.psp.s, PSP_S - 8, "BLXNS frame on PSP_S");

    cpu.pulse_irq(2);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (18, Sec::Secure));
    step(&mut cpu, &mut bus);
    assert_eq!(
        (ipsr(&cpu), cpu.regs.sec, cpu.regs.pc),
        (0, Sec::NonSecure, 0x200)
    );
    assert_eq!(cpu.regs.control.s, 0b10);

    // FNC_RETURN pops the BLXNS frame from PSP_S.
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.sec, Sec::Secure);
    assert_eq!(cpu.regs.pc, 0x1000_0102);
    assert_eq!((cpu.regs.psp.s, cpu.regs.msp.s), (PSP_S, SP_S));
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x1000_0104);
}

#[test]
fn ns_irq_over_secure_handler_with_ns_thread_on_psp() {
    // NS thread on PSP_NS -> Secure IRQ 2 -> NS IRQ 3 (higher priority) preempts the Secure
    // handler. Its EXC_RETURN returns to Handler mode with SPSEL = CONTROL_NS.SPSEL = 1, which
    // is a valid value in v8-M, and both returns land on the right stacks.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[B_SELF]);
    s_handler(&mut bus, 16 + 2, 0x1000_0600, &[NOP, BX_LR]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[BX_LR]);
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, (1 << 2) | (1 << 3));
    cpu.ppb_write32(NVIC_IPR0, 0x80 << 16);
    cpu.regs.control.ns = 0b10;
    cpu.regs.psp.ns = PSP_NS;
    enter_ns(&mut cpu, 0x100);
    cpu.pulse_irq(2);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.lr), (18, 0xFFFF_FFB9));
    assert_eq!(cpu.regs.psp.ns, PSP_NS - 0x20);
    cpu.pulse_irq(3);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (19, Sec::NonSecure));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF4);
    assert_eq!(
        cpu.regs.msp.s,
        SP_S - 0x48,
        "frame on MSP_S (Secure Handler mode)"
    );
    step(&mut cpu, &mut bus);
    assert_eq!(
        (ipsr(&cpu), cpu.regs.sec, cpu.regs.pc),
        (18, Sec::Secure, 0x1000_0600)
    );
    assert_eq!((cpu.regs.msp.s, cpu.regs.control.ns), (SP_S, 0b10));
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!(
        (ipsr(&cpu), cpu.regs.sec, cpu.regs.pc),
        (0, Sec::NonSecure, 0x100)
    );
    assert_eq!((cpu.regs.psp.ns, cpu.regs.msp.ns), (PSP_NS, SP_NS));
    assert_eq!(cpu.regs.control.ns, 0b10);
}

#[test]
fn ns_exc_return_claiming_secure_is_inver() {
    // A Non-secure handler returns with ES = 1.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[0x2001, 0x4308, 0x4700]); // movs r0, #1 ; orrs r0, r1 ; bx r0
    s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, 1 << 3);
    // IRQ 3 below SecureFault's priority, so the fault is not escalated.
    cpu.ppb_write32(NVIC_IPR0, 0x80 << 24);
    enter_ns(&mut cpu, 0x100);
    cpu.pulse_irq(3);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.lr), (19, 0xFFFF_FFB8));
    cpu.regs.r[1] = 0xFFFF_FFB8;
    for _ in 0..3 {
        step(&mut cpu, &mut bus);
    }
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (7, Sec::Secure));
    assert_eq!(cpu.ppb_read32(SFSR), INVER);
}

#[test]
fn ns_fault_escalation_targets_hardfault_by_bfhfnmins() {
    // UsageFault_NS disabled: escalates to the Secure HardFault while BFHFNMINS = 0.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[UDF0]);
    s_handler(&mut bus, 3, 0x1000_0500, &[B_SELF]);
    ns_handler(&mut bus, 3, 0x500, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (3, Sec::Secure));
    assert_eq!(cpu.ppb_read32(CFSR + NS_ALIAS), UNDEFINSTR, "UFSR_NS");
    assert_eq!(cpu.ppb_read32(CFSR), 0, "UFSR_S");

    // With BFHFNMINS = 1 it is the Non-secure HardFault.
    let (mut cpu, mut bus) = tz();
    cpu.ppb_write32(AIRCR, 0x05FA_0000 | (1 << 13));
    bus.code(0x100, &[UDF0]);
    s_handler(&mut bus, 3, 0x1000_0500, &[B_SELF]);
    ns_handler(&mut bus, 3, 0x500, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (3, Sec::NonSecure));
    assert_eq!(cpu.regs.pc, 0x500);

    // With UsageFault_NS enabled it is taken in Non-secure state.
    let (mut cpu, mut bus) = tz();
    cpu.ppb_write32(SHCSR + NS_ALIAS, USGFAULTENA);
    bus.code(0x100, &[UDF0]);
    ns_handler(&mut bus, 6, 0x600, &[B_SELF]);
    enter_ns(&mut cpu, 0x100);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (6, Sec::NonSecure));
    assert_eq!(cpu.regs.pc, 0x600);
}

#[test]
fn ns_view_of_banked_scs_registers() {
    let (mut cpu, _bus) = tz();
    let ns = |a: u32| a + NS_ALIAS;

    // AIRCR: PRIS and BFHFNMINS are Secure-writable; NS sees BFHFNMINS only.
    cpu.ppb_write32(AIRCR, 0x05FA_0000 | (1 << 14) | (1 << 13) | (3 << 8));
    assert_eq!(cpu.ppb_read32(AIRCR) & 0x6700, 0x6300);
    assert_eq!(
        cpu.ppb_read32(ns(AIRCR)) & 0x6700,
        0x2000,
        "PRIGROUP_NS is 0"
    );
    cpu.ppb_write32(ns(AIRCR), 0x05FA_0500);
    assert_eq!(cpu.ppb_read32(ns(AIRCR)) & 0x6700, 0x2500);
    assert_eq!(
        cpu.ppb_read32(AIRCR) & 0x6700,
        0x6300,
        "NS cannot change PRIS"
    );

    // SHCSR: SecureFault state and enable are invisible to NS; enables are banked.
    assert_eq!(cpu.ppb_read32(SHCSR), SECUREFAULTENA);
    assert_eq!(cpu.ppb_read32(ns(SHCSR)), 0);
    cpu.ppb_write32(ns(SHCSR), USGFAULTENA | SECUREFAULTENA);
    assert_eq!(cpu.ppb_read32(ns(SHCSR)), USGFAULTENA);
    assert_eq!(cpu.ppb_read32(SHCSR), SECUREFAULTENA);

    // SHPR1: the SecureFault priority byte is RAZ/WI from NS; MemManage is banked.
    cpu.ppb_write32(SHPR1, 0x6000_0020);
    cpu.ppb_write32(ns(SHPR1), 0xE000_0040);
    assert_eq!(cpu.ppb_read32(SHPR1), 0x6000_0020);
    assert_eq!(cpu.ppb_read32(ns(SHPR1)), 0x0000_0040);

    // NVIC: Secure-targeted interrupts are RAZ/WI from NS; ITNS is Secure-only.
    cpu.ppb_write32(NVIC_ITNS0, 1 << 5);
    assert_eq!(cpu.ppb_read32(ns(NVIC_ITNS0)), 0);
    cpu.ppb_write32(ns(NVIC_ITNS0), 0xFF);
    assert_eq!(cpu.ppb_read32(NVIC_ITNS0), 1 << 5);
    cpu.ppb_write32(ns(NVIC_ISER0), (1 << 5) | (1 << 4));
    assert_eq!(cpu.ppb_read32(NVIC_ISER0), 1 << 5);
    cpu.ppb_write32(NVIC_ISPR0, (1 << 5) | (1 << 4));
    assert_eq!(cpu.ppb_read32(ns(NVIC_ISPR0)), 1 << 5);
    cpu.ppb_write32(ns(NVIC_IPR0 + 4), 0xFFFF_FFFF);
    assert_eq!(cpu.ppb_read32(NVIC_IPR0 + 4), 0x0000_E000);

    // SysTick is banked.
    cpu.ppb_write32(SYST_RVR, 100);
    cpu.ppb_write32(ns(SYST_RVR), 200);
    assert_eq!(cpu.ppb_read32(SYST_RVR), 100);
    assert_eq!(cpu.ppb_read32(ns(SYST_RVR)), 200);
    assert_eq!(cpu.scs.systick.ns.rvr, 200);

    // ICSR: PendSV set from NS pends the Non-secure PendSV.
    cpu.ppb_write32(ns(ICSR), 1 << 28);
    assert_ne!(cpu.ppb_read32(ns(ICSR)) & (1 << 28), 0);
    assert_eq!(cpu.ppb_read32(ICSR) & (1 << 28), 0);
}

#[test]
fn mrs_msr_sp_ns() {
    // Secure: mrs r0, SP_NS ; msr SP_NS, r1 ; mrs r2, SP_NS
    let (mut cpu, mut bus) = tz();
    bus.code(
        0x1000_0100,
        &[0xF3EF, 0x8098, 0xF381, 0x8898, 0xF3EF, 0x8298],
    );
    cpu.regs.psp.ns = 0x2000_0800;
    cpu.regs.r[1] = 0x2000_0A07;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[0], SP_NS, "MSP_NS while CONTROL_NS.SPSEL = 0");
    cpu.regs.control.ns = 0b10;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.psp.ns, 0x2000_0A04);
    assert_eq!(cpu.regs.msp.ns, SP_NS);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[2], 0x2000_0A04);

    // From Non-secure state SP_NS does not exist: reads as zero, writes ignored.
    let (mut cpu, mut bus) = tz();
    bus.code(0x100, &[0xF3EF, 0x8098, 0xF381, 0x8898]);
    enter_ns(&mut cpu, 0x100);
    cpu.regs.r[0] = 0x55;
    cpu.regs.r[1] = 0x2000_0A00;
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!((cpu.regs.r[0], cpu.regs.msp.ns), (0, SP_NS));
}

#[test]
fn control_fpca_is_not_stored_per_bank() {
    // msr CONTROL_NS, r0 ; msr CONTROL, r0 ; mrs r1, CONTROL ; mrs r2, CONTROL_NS
    let (mut cpu, mut bus) = tz();
    bus.code(
        0x1000_0100,
        &[
            0xF380, 0x8894, 0xF380, 0x8814, 0xF3EF, 0x8114, 0xF3EF, 0x8294,
        ],
    );
    cpu.regs.psp.s = SP_S - 0x100;
    cpu.regs.r[0] = 0xF;
    for _ in 0..4 {
        step(&mut cpu, &mut bus);
    }
    // No FPU: FPCA and SFPA are RES0; only nPRIV and SPSEL are banked.
    assert_eq!((cpu.regs.r[1], cpu.regs.r[2]), (0b11, 0b11));
    assert_eq!((cpu.regs.control.s, cpu.regs.control.ns), (0b11, 0b11));
}

#[test]
fn reset_keeps_installed_idau() {
    struct AllNs;
    impl Idau for AllNs {
        fn attr(&self, _addr: u32) -> Option<(Attr, Option<u8>)> {
            Some((Attr::NonSecure, None))
        }
    }
    let (mut cpu, mut bus) = tz();
    cpu.set_idau(Box::new(AllNs));
    cpu.ppb_write32(SAU_CTRL, 1);
    cpu.ppb_write32(SAU_RNR, 0);
    cpu.ppb_write32(SAU_RBAR, 0x1000_0000);
    cpu.ppb_write32(SAU_RLAR, 0x1000_0FE0 | 1);
    assert_eq!(cpu.security_of(0x1000_0000), Attr::NonSecure);
    cpu.reset(&mut bus);
    assert_eq!(cpu.ppb_read32(SAU_CTRL), 0, "SAU resets disabled");
    cpu.ppb_write32(SAU_CTRL, 1);
    cpu.ppb_write32(SAU_RNR, 0);
    cpu.ppb_write32(SAU_RBAR, 0x1000_0000);
    cpu.ppb_write32(SAU_RLAR, 0x1000_0FE0 | 1);
    assert_eq!(cpu.security_of(0x1000_0000), Attr::NonSecure, "IDAU kept");
}

// ---- Review fix round 1 ----

#[test]
fn ns_return_to_secure_requires_dcrs() {
    // A Secure thread is interrupted by a Non-secure IRQ whose handler returns with `bx r0`.
    for (exc_return, valid) in [(0xFFFF_FFF8u32, true), (0xFFFF_FFD8, false)] {
        let (mut cpu, mut bus) = tz();
        bus.code(0x1000_0100, &[B_SELF]);
        ns_handler(&mut bus, 16 + 3, 0x400, &[0x4700]);
        s_handler(&mut bus, 7, 0x1000_0400, &[B_SELF]);
        cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
        cpu.ppb_write32(NVIC_ISER0, 1 << 3);
        // IRQ 3 below SecureFault's priority, so the fault is not escalated.
        cpu.ppb_write32(NVIC_IPR0, 0x80 << 24);
        cpu.regs.r[5] = 0x5555;
        cpu.pulse_irq(3);
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.regs.lr, 0xFFFF_FFF8);
        cpu.regs.r[0] = exc_return;
        step(&mut cpu, &mut bus);
        if valid {
            // S = 1, DCRS = 1: the callee context and the basic frame are popped.
            assert_eq!(ipsr(&cpu), 0);
            assert_eq!((cpu.regs.sec, cpu.regs.pc), (Sec::Secure, 0x1000_0100));
            assert_eq!((cpu.regs.r[5], cpu.regs.msp.s), (0x5555, SP_S));
        } else {
            // S = 1, DCRS = 0 from Non-secure state: SecureFault INVER.
            assert_eq!((ipsr(&cpu), cpu.regs.sec), (7, Sec::Secure));
            assert_eq!(cpu.ppb_read32(SFSR), INVER);
            assert_eq!(cpu.regs.msp.s, SP_S - 0x48, "nothing unstacked");
        }
    }
}

#[test]
fn tail_chain_from_ns_to_secure_exception_keeps_callee_context() {
    // A Secure thread with Non-secure IRQ 3 (priority 0) and Secure IRQ 4 (priority 0x40)
    // pending together.
    let (mut cpu, mut bus) = tz();
    bus.code(0x1000_0100, &[B_SELF]);
    ns_handler(&mut bus, 16 + 3, 0x400, &[BX_LR]);
    s_handler(&mut bus, 16 + 4, 0x1000_0600, &[NOP, BX_LR]);
    for i in 0..13 {
        cpu.regs.r[i] = 0x100 + i as u32;
    }
    let before = cpu.regs.r;
    cpu.ppb_write32(NVIC_ITNS0, 1 << 3);
    cpu.ppb_write32(NVIC_ISER0, (1 << 3) | (1 << 4));
    cpu.ppb_write32(NVIC_IPR0 + 4, 0x40);
    cpu.pulse_irq(3);
    cpu.pulse_irq(4);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (19, Sec::NonSecure));
    assert_eq!(cpu.regs.lr, 0xFFFF_FFF8);

    // Returning from IRQ 3 tail-chains into the Secure IRQ 4 without unstacking. DCRS = 0
    // records that the callee context is on the stack although the exception is Secure.
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (20, Sec::Secure));
    assert_eq!((cpu.regs.pc, cpu.regs.lr), (0x1000_0600, 0xFFFF_FFD9));
    assert_eq!(cpu.regs.msp.s, SP_S - 0x48);

    // Its return pops the callee context and the basic frame.
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!((ipsr(&cpu), cpu.regs.sec), (0, Sec::Secure));
    assert_eq!(cpu.regs.pc, 0x1000_0100);
    assert_eq!(cpu.regs.r, before);
    assert_eq!(cpu.regs.msp.s, SP_S);
}

#[test]
fn tt_mpu_fields_need_privilege() {
    // tt r2, r1 ; ttt r3, r1 ; tt r4, r1 ; tt r5, r6 ; tta r7, r6
    let (mut cpu, mut bus) = tz();
    bus.code(
        0x1000_0100,
        &[
            0xE841, 0xF200, 0xE841, 0xF340, 0xE841, 0xF400, 0xE846, 0xF500, 0xE846, 0xF780,
        ],
    );
    // Secure MPU: region 1 = Secure RAM, read/write for privileged code only; region 2 = NS
    // RAM, read/write at any privilege.
    cpu.ppb_write32(MPU_RNR, 1);
    cpu.ppb_write32(MPU_RBAR, 0x3000_0000);
    cpu.ppb_write32(MPU_RLAR, 0x3000_FFE0 | 1);
    cpu.ppb_write32(MPU_RNR, 2);
    cpu.ppb_write32(MPU_RBAR, 0x2000_0000 | (0b01 << 1));
    cpu.ppb_write32(MPU_RLAR, 0x2000_FFE0 | 1);
    cpu.ppb_write32(MPU_CTRL, 0b101);
    cpu.regs.r[1] = 0x3000_0010;
    cpu.regs.r[6] = 0x2000_0010;
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.r[2], 1 | MRVALID | TT_R | TT_RW | TT_S);
    // TTT checks unprivileged permissions; privileged code still gets the region number.
    assert_eq!(cpu.regs.r[3], 1 | MRVALID | TT_S);

    // Unprivileged code gets no MPU information (so no NSR/NSRW either).
    cpu.regs.control.s = 1;
    for _ in 0..3 {
        step(&mut cpu, &mut bus);
    }
    assert_eq!(cpu.regs.r[4], TT_S);
    assert_eq!(cpu.regs.r[5], (1 << 8) | SRVALID);
    // ... except through TTA, which reports the Non-secure MPU (disabled; Non-secure Thread
    // mode is privileged).
    assert_eq!(
        cpu.regs.r[7],
        (1 << 8) | SRVALID | TT_R | TT_RW | TT_NSR | TT_NSRW
    );
}
