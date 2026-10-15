//! Save states: `Machine::save_state` / `Machine::load_state` (Task 22, Review Focus 3).
//!
//! Every test boots a synthetic bundle whose secure image is a hand-assembled Thumb program,
//! placed by `Layout::EVO_7_1` like the real OS.

use evo_core::state::{STATE_MAGIC, STATE_VERSION};
use evo_core::{Machine, NvState, StateError, UartPort};
use evo_hw::dev::spif::Spif;
use evo_hw::nor::SECREGS_LEN;
use evo_hw::soc::{FLASH_BUFFER, SRAM_BUFFER};
use evo_hw::{DevCtx, Device, ResetKind};
use evo_image::testutil::secure_image_bundle;
use evo_image::{Bundle, Layout};

/// `VTOR_S` for `Layout::EVO_7_1`: Secure alias of slot 0x000B_0000 plus the 0x400 header.
const VTOR: u32 = 0x100B_0400;
/// Payload offset of the reset handler, after room for 128 vectors.
const CODE: u32 = 0x200;
/// Payload offset of the interrupt handler.
const HANDLER: u32 = 0x280;
/// Payload offset of the literal pool.
const LIT: u32 = 0x300;
const SP_INIT: u32 = 0x3000_0800;

const SYSTICK: u32 = 15;
const IRQ0: u32 = 16;
const WFI: u16 = 0xBF30;
const BX_LR: u16 = 0x4770;

/// SRAM word the SysTick handler counts in.
const COUNTER: u32 = 0x3000_0200;

/// One item of a test program.
#[derive(Clone, Copy)]
enum I {
    /// A 16-bit instruction.
    H(u16),
    /// `ldr rd, [pc, #..]` of the literal with index `n` in the pool at [`LIT`].
    Ld(u16, u32),
    /// `bne` to payload offset `to`.
    Bne(u32),
    /// `b` to payload offset `to`.
    B(u32),
}

/// The secure image's payload: a vector table at offset 0, code from `CODE` on.
#[derive(Clone)]
struct Payload(Vec<u8>);

impl Payload {
    fn new() -> Self {
        let mut p = Payload(vec![0; 0x400]);
        p.word(0, SP_INIT);
        p
    }

    fn word(&mut self, off: u32, v: u32) -> &mut Self {
        let o = off as usize;
        self.0[o..o + 4].copy_from_slice(&v.to_le_bytes());
        self
    }

    /// Point vector `n` at the Thumb code at payload offset `off`.
    fn vector(&mut self, n: u32, off: u32) -> &mut Self {
        self.word(4 * n, (VTOR + off) | 1)
    }

    /// The literal pool at [`LIT`], one word per entry.
    fn literals(&mut self, lits: &[u32]) -> &mut Self {
        for (i, &v) in (0..).zip(lits) {
            self.word(LIT + 4 * i, v);
        }
        self
    }

    /// Assemble `prog` at payload offset `off`.
    fn asm(&mut self, off: u32, prog: &[I]) -> &mut Self {
        for (at, ins) in (off..).step_by(2).zip(prog) {
            let rel = |to: u32| (i64::from(to) - i64::from(at + 4)) / 2;
            let hw = match *ins {
                I::H(hw) => hw,
                I::Ld(rd, n) => {
                    let base = (at + 4) & !3;
                    0x4800 | (rd << 8) | ((LIT + 4 * n - base) / 4) as u16
                }
                I::Bne(to) => 0xD100 | (rel(to) as u16 & 0xFF),
                I::B(to) => 0xE000 | (rel(to) as u16 & 0x7FF),
            };
            let o = at as usize;
            self.0[o..o + 2].copy_from_slice(&hw.to_le_bytes());
        }
        self
    }

    fn bundle(&self) -> Bundle {
        Bundle::from_bytes(&secure_image_bundle(self.0.clone())).expect("synthetic bundle")
    }

    fn boot(&self) -> Machine {
        Machine::from_bundle(&self.bundle(), &Layout::EVO_7_1).expect("boot")
    }
}

/// SysTick every 1000 cycles counts in [`COUNTER`]; the main loop writes 'A', 'B', ... to
/// UART0's THR with a delay loop between bytes, and counts passes in r5.
fn counter_uart() -> Payload {
    let mut p = Payload::new();
    p.literals(&[
        0xE000_E014, // 0: SYST_RVR
        999,         // 1
        0xE000_E010, // 2: SYST_CSR
        0x4010_6000, // 3: UART0
        COUNTER,     // 4
    ]);
    p.vector(1, CODE).vector(SYSTICK, HANDLER);
    p.asm(
        CODE,
        &[
            I::Ld(0, 0),
            I::Ld(1, 1),
            I::H(0x6001), // str r1, [r0]
            I::Ld(0, 2),
            I::H(0x2107), // movs r1, #7: enable, TICKINT, processor clock
            I::H(0x6001), // str r1, [r0]
            I::Ld(0, 3),
            I::H(0x2241), // movs r2, #'A'
            // 0x210: loop
            I::H(0x6002), // str r2, [r0]
            I::H(0x3201), // adds r2, #1
            I::H(0x23C8), // movs r3, #200
            // 0x216: delay
            I::H(0x3B01), // subs r3, #1
            I::Bne(CODE + 0x16),
            I::H(0x3501), // adds r5, #1
            I::B(CODE + 0x10),
        ],
    );
    p.asm(
        HANDLER,
        &[
            I::Ld(0, 4),
            I::H(0x6801), // ldr r1, [r0]
            I::H(0x3101), // adds r1, #1
            I::H(0x6001), // str r1, [r0]
            I::H(BX_LR),
        ],
    );
    p
}

/// LCDC enabled with IMSC = VComp and IRQ 14 on, then `wfi ; b wfi`; the IRQ 14 handler
/// clears VComp (ICR) and counts in r4.
fn lcdc_frames() -> Payload {
    let mut p = Payload::new();
    p.literals(&[
        0x4000_4000, // 0: LCDC
        0xE000_E100, // 1: NVIC_ISER0
        1 << 14,     // 2
    ]);
    p.vector(1, CODE).vector(IRQ0 + 14, HANDLER);
    p.asm(
        CODE,
        &[
            I::Ld(0, 0),
            I::H(0x2108), // movs r1, #8 (VComp)
            I::H(0x61C1), // str r1, [r0, #0x1C] (IMSC)
            I::H(0x2101), // movs r1, #1 (LcdEn)
            I::H(0x6181), // str r1, [r0, #0x18] (CONTROL)
            I::Ld(0, 1),
            I::Ld(1, 2),
            I::H(0x6001), // str r1, [r0]
            // 0x210: wfi ; b wfi
            I::H(WFI),
            I::B(CODE + 0x10),
        ],
    );
    p.asm(
        HANDLER,
        &[
            I::Ld(0, 0),
            I::H(0x2108), // movs r1, #8
            I::H(0x6281), // str r1, [r0, #0x28] (ICR)
            I::H(0x3401), // adds r4, #1
            I::H(BX_LR),
        ],
    );
    p
}

fn sram(m: &Machine) -> Vec<u8> {
    m.bus.region_bytes(SRAM_BUFFER).expect("sram").to_vec()
}

#[test]
fn state_roundtrip_resumes_identically() {
    let p = counter_uart();
    let mut a = p.boot();
    a.run(50_000);
    let state = a.save_state();
    assert_eq!(&state[..8], STATE_MAGIC);
    assert_eq!(state[8..12], STATE_VERSION.to_le_bytes());
    a.run(50_000);

    let mut b = p.boot();
    b.run(1_234);
    b.load_state(&state).expect("load");
    b.run(50_000);

    assert_eq!(a.cpu, b.cpu, "registers and system control space");
    assert_eq!(a.sched.now, b.sched.now);
    assert!(sram(&a) == sram(&b), "SRAM differs");
    let ticks = a.bus.peek(COUNTER, 4).expect("counter");
    assert!((95..=100).contains(&ticks), "SysTick ran: {ticks}");
    let (ua, ub) = (
        a.take_uart_tx(UartPort::Uart0),
        b.take_uart_tx(UartPort::Uart0),
    );
    assert!(ua.len() > 20, "UART wrote {} bytes", ua.len());
    assert_eq!(ua, ub);
    assert!(a.save_state() == b.save_state(), "whole state differs");
}

#[test]
fn state_keeps_device_events() {
    let p = lcdc_frames();
    let mut a = p.boot();
    a.run(1_000);
    let frame = a.sched.next_deadline().expect("LCDC frame event pending");
    assert!(frame > a.sched.now + 1_000);
    a.run(frame - a.sched.now - 500);
    assert!(a.asleep());
    assert_eq!(a.cpu.regs.r[4], 0);
    let state = a.save_state();

    let mut b = p.boot();
    b.load_state(&state).expect("load");
    assert!(b.asleep());
    assert_eq!(b.sched.next_deadline(), Some(frame));

    // The cycle the IRQ 14 handler first runs at.
    let handled_at = |m: &mut Machine| {
        for _ in 0..10_000 {
            if m.cpu.regs.r[4] == 1 {
                return m.sched.now;
            }
            m.step();
        }
        panic!("the frame event never fired");
    };
    let at = handled_at(&mut a);
    assert!(at >= frame);
    assert_eq!(handled_at(&mut b), at);
}

/// `load_state(bytes)` fails and leaves `m` byte for byte as it was (Review Focus 3).
fn rejected(m: &mut Machine, bytes: &[u8]) -> StateError {
    let before = m.save_state();
    let now = m.sched.now;
    let err = m.load_state(bytes).expect_err("load must fail");
    assert_eq!(m.sched.now, now);
    assert!(m.save_state() == before, "machine changed by a failed load");
    err
}

/// A machine that has run for a while and its state, and a second machine of the same bundle
/// that ran a different time.
fn two_machines() -> (Vec<u8>, Machine) {
    let p = counter_uart();
    let mut a = p.boot();
    a.run(50_000);
    let mut b = p.boot();
    b.run(10_000);
    (a.save_state(), b)
}

#[test]
fn load_state_rejects_bad_magic() {
    let (mut state, mut m) = two_machines();
    state[0] ^= 0x20;
    assert!(matches!(rejected(&mut m, &state), StateError::BadMagic));
}

#[test]
fn load_state_rejects_other_versions() {
    assert_eq!(STATE_VERSION, 2);
    for v in [1, 3] {
        let (mut state, mut m) = two_machines();
        state[8..12].copy_from_slice(&u32::to_le_bytes(v));
        assert!(matches!(
            rejected(&mut m, &state),
            StateError::Version { found, expected: 2 } if found == v
        ));
    }
}

#[test]
fn load_state_rejects_truncated() {
    let (state, mut m) = two_machines();
    for len in [
        0,
        7,
        8,
        11,
        12,
        43,
        44,
        45,
        75,
        76,
        107,
        108,
        109,
        state.len() / 2,
        state.len() - 1,
    ] {
        rejected(&mut m, &state[..len]);
    }
    // Trailing bytes are a different layout, not a state to load.
    let mut longer = state.clone();
    longer.push(0);
    rejected(&mut m, &longer);
    // The untouched state still loads.
    m.load_state(&state).expect("load");
}

/// A device the SoC does not have.
struct Extra;

impl Device for Extra {
    fn read(&mut self, _off: u32, _width: u8, _ctx: &mut DevCtx<'_>) -> u32 {
        0
    }

    fn write(&mut self, _off: u32, _width: u8, _v: u32, _ctx: &mut DevCtx<'_>) {}

    fn reset(&mut self, _kind: ResetKind, _ctx: &mut DevCtx<'_>) {}

    fn save(&self) -> Vec<u8> {
        Vec::new()
    }

    fn load(&mut self, _state: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

#[test]
fn load_state_rejects_other_device_set() {
    let (state, mut m) = two_machines();
    m.bus
        .add_device("extra", 0x6000_0000, 0x100, Box::new(Extra));
    assert!(matches!(rejected(&mut m, &state), StateError::Mismatch(_)));

    // And the other way round: a state with the extra device on a plain machine.
    let extra_state = m.save_state();
    let mut plain = counter_uart().boot();
    assert!(matches!(
        rejected(&mut plain, &extra_state),
        StateError::Mismatch(_)
    ));
}

#[test]
fn load_state_rejects_other_bundle() {
    let (state, _) = two_machines();
    let mut other = counter_uart();
    // A byte the program never reads.
    other.0[0x3F0] ^= 1;
    let mut m = other.boot();
    m.run(10_000);
    let err = rejected(&mut m, &state);
    assert!(
        matches!(&err, StateError::Mismatch(msg) if msg.contains("another bundle")),
        "{err}"
    );
}

#[test]
fn load_state_keeps_secregs() {
    let mut m = counter_uart().boot();
    m.bus
        .with_device::<Spif, _>("spif", |s, _| s.nor_mut().set_secregs(&[0x12; SECREGS_LEN]))
        .expect("spif");
    m.run(10_000);
    let state = m.save_state();
    m.run(10_000);
    m.load_state(&state).expect("load");
    assert_eq!(m.nv_state().secregs()[0], 0x12);
}

/// Offset of the body checksum in the header.
const CHECKSUM_AT: usize = 8 + 4 + 32 + 32;
/// Bytes before the postcard body: magic, version, bundle id, flash fingerprint, body checksum.
const HEADER_LEN: usize = CHECKSUM_AT + 32;

/// `state` with its body checksum recomputed, as a writer of that body would make it.
fn resealed(mut state: Vec<u8>) -> Vec<u8> {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(&state[HEADER_LEN..]);
    state[CHECKSUM_AT..HEADER_LEN].copy_from_slice(&sum);
    state
}

#[test]
fn load_state_rejects_corrupt_body() {
    let (state, mut m) = two_machines();
    assert!(state == resealed(state.clone()), "checksum layout");
    // One flipped bit anywhere in the body, here in the middle of SRAM.
    let mut bad = state.clone();
    let at = HEADER_LEN + (state.len() - HEADER_LEN) / 2;
    bad[at] ^= 0x01;
    assert!(matches!(rejected(&mut m, &bad), StateError::Corrupt));
    // A corrupt checksum is the same error.
    let mut bad = state.clone();
    bad[CHECKSUM_AT] ^= 0x80;
    assert!(matches!(rejected(&mut m, &bad), StateError::Corrupt));
    m.load_state(&state).expect("load");
}

#[test]
fn load_state_rejects_other_flash() {
    let (state, _) = two_machines();
    // The same bundle, booted from a flash that differs outside the image slots (the ITS area).
    let mut flash = vec![0xFF; evo_hw::nor::NOR_SIZE];
    flash[0x0009_0000] = 0x00;
    let nv = NvState::new(flash, vec![0xFF; SECREGS_LEN]).unwrap();
    let p = counter_uart();
    let mut m = Machine::from_bundle_nv(&p.bundle(), &Layout::EVO_7_1, Some(nv)).unwrap();
    m.run(10_000);
    let err = rejected(&mut m, &state);
    assert!(
        matches!(&err, StateError::Mismatch(msg) if msg.contains("flash")),
        "{err}"
    );

    // Other security registers are other non-volatile state too.
    let mut m = p.boot();
    m.bus
        .with_device::<Spif, _>("spif", |s, _| s.nor_mut().set_secregs(&[0x12; SECREGS_LEN]))
        .expect("spif");
    let err = rejected(&mut m, &state);
    assert!(
        matches!(&err, StateError::Mismatch(msg) if msg.contains("flash")),
        "{err}"
    );

    // Flash the guest writes after the save is a change too: the state no longer fits.
    let mut a = p.boot();
    a.run(10_000);
    let state = a.save_state();
    a.bus.buffer_mut(FLASH_BUFFER).expect("flash")[0x0009_0000] = 0x00;
    let err = rejected(&mut a, &state);
    assert!(matches!(err, StateError::Mismatch(_)), "{err}");
}

/// A state of a machine that ran a while, with its CPU replaced by `edit(cpu)`: the CPU is the
/// first field of the body, so its postcard bytes are swapped in place and the header (bundle id
/// included) stays valid. Also returns a second machine of the same bundle to load it into.
fn with_cpu(edit: impl FnOnce(&mut armv8m::Cpu)) -> (Vec<u8>, Machine) {
    let p = counter_uart();
    let mut a = p.boot();
    a.run(50_000);
    let state = a.save_state();
    let old = postcard::to_stdvec(&a.cpu).unwrap();
    assert_eq!(&state[HEADER_LEN..HEADER_LEN + old.len()], &old[..]);
    let mut cpu = a.cpu.clone();
    edit(&mut cpu);
    let mut out = state[..HEADER_LEN].to_vec();
    out.extend(postcard::to_stdvec(&cpu).unwrap());
    out.extend_from_slice(&state[HEADER_LEN + old.len()..]);
    let mut b = p.boot();
    b.run(10_000);
    (resealed(out), b)
}

#[test]
fn with_cpu_unchanged_loads() {
    // Guards the splice in `with_cpu`: with no edit the state loads.
    let (state, mut m) = with_cpu(|_| {});
    m.load_state(&state).unwrap();
}

#[test]
fn load_state_rejects_short_nvic_priority() {
    for len in [0, 1, 479, 481] {
        let (state, mut m) = with_cpu(|cpu| cpu.scs.nvic.priority.resize(len, 0));
        let err = rejected(&mut m, &state);
        assert!(matches!(err, StateError::Decode(_)), "len {len}: {err}");
    }
}

#[test]
fn load_state_rejects_bad_prigroup() {
    for (s, ns) in [(8, 0), (0, 8), (32, 0), (255, 255)] {
        let (state, mut m) = with_cpu(|cpu| {
            cpu.scs.scb.prigroup.s = s;
            cpu.scs.scb.prigroup.ns = ns;
        });
        let err = rejected(&mut m, &state);
        assert!(
            matches!(err, StateError::Decode(_)),
            "prigroup {s}/{ns}: {err}"
        );
    }
}

/// The fingerprint is the SHA-256 of exactly the bytes `nv_state` returns (flash, then the
/// security registers), so equal fingerprints mean equal flash files.
#[test]
fn nv_fingerprint_hashes_the_nv_state_bytes() {
    use sha2::{Digest, Sha256};
    let mut m = counter_uart().boot();
    m.run(10_000);
    let nv = m.nv_state();
    let mut h = Sha256::new();
    h.update(nv.flash());
    h.update(nv.secregs());
    let want: [u8; 32] = h.finalize().into();
    assert_eq!(m.nv_fingerprint(), want);
}
