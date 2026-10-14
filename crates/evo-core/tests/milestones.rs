//! Boot milestones of the real TI-84 Evo OS (`docs/hardware/boot-flow.md`, "Milestones").
//!
//! They read the user's own bundle from `EVO_OS_IMAGE`; no TI code is part of the repository.
//! Each boots hundreds of millions of cycles, so they run in release only:
//!
//! ```text
//! cargo test --release -p evo-core --test milestones -- --nocapture
//! ```
//!
//! Without `EVO_OS_IMAGE`, or in a debug build, each test prints a `skipping:` line and passes.

use std::ops::RangeInclusive;

use evo_core::{Machine, NvState, StopReason, UartPort};
use evo_hw::UnmappedAccess;
use evo_hw::dev::crypto::Sha256Engine;
use evo_hw::dev::spi::Spi;
use evo_hw::dev::uart::Uart16550;
use evo_image::{Bundle, Layout};

/// Cycles per `Os::run_until` slice: UART0 is drained after each.
const SLICE: u64 = 10_000_000;

/// The real OS, booting, with everything it wrote to UART0 so far.
struct Os {
    m: Machine,
    uart0: Vec<u8>,
}

impl Os {
    /// The machine at reset with `EVO_OS_IMAGE` loaded. `None`, printing `skipping: <reason>`,
    /// when `EVO_OS_IMAGE` is unset or this is a debug build.
    fn boot() -> Option<Os> {
        Self::boot_with(None)
    }

    /// [`boot`](Self::boot), powered on with `nv` (flash bytes and NOR security registers) as
    /// a flash file would restore it.
    fn boot_nv(nv: NvState) -> Option<Os> {
        Self::boot_with(Some(nv))
    }

    fn boot_with(nv: Option<NvState>) -> Option<Os> {
        if cfg!(debug_assertions) {
            println!("skipping: milestone tests need a release build");
            return None;
        }
        let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
            println!("skipping: EVO_OS_IMAGE is not set");
            return None;
        };
        let bundle =
            Bundle::open(path.as_ref()).expect("EVO_OS_IMAGE should be a readable .84b2 bundle");
        let m = Machine::from_bundle_nv(&bundle, &Layout::EVO_7_1, nv).expect("the bundle boots");
        Some(Os {
            m,
            uart0: Vec::new(),
        })
    }

    /// Run in [`SLICE`]-cycle slices, appending UART0 TX to `uart0` after each, until `done`
    /// holds (`Halted`), the machine stops on its own for a reason other than `CycleLimit`
    /// (that reason; a `Lockup` panics with the recent trace), or `budget` cycles have run
    /// (`CycleLimit`).
    fn run_until(&mut self, budget: u64, mut done: impl FnMut(&Os) -> bool) -> StopReason {
        let end = self.m.sched.now.saturating_add(budget);
        loop {
            if done(self) {
                return StopReason::Halted;
            }
            if self.m.sched.now >= end {
                return StopReason::CycleLimit;
            }
            let stop = self.m.run(SLICE.min(end - self.m.sched.now));
            let tx = self.m.take_uart_tx(UartPort::Uart0);
            self.uart0.extend_from_slice(&tx);
            match stop {
                StopReason::CycleLimit => {}
                StopReason::Lockup { pc } => panic!(
                    "the CPU locked up at {pc:#010x} after {} cycles ({} system resets); \
                     UART0:\n{}\nlast instructions executed:\n{}",
                    self.m.sched.now,
                    self.m.resets,
                    self.uart_text(),
                    self.m.recent_trace().join("\n")
                ),
                // Stopped (e.g. idle) just as the condition came true.
                _ if done(self) => return StopReason::Halted,
                other => return other,
            }
        }
    }

    /// UART0 output so far, as text (invalid UTF-8 replaced).
    fn uart_text(&self) -> String {
        String::from_utf8_lossy(&self.uart0).into_owned()
    }

    /// [`uart_text`](Self::uart_text) without ANSI escape sequences (`ESC [ … final byte`):
    /// TF-M colours parts of its log lines.
    fn uart_plain(&self) -> String {
        let mut out = String::new();
        let text = self.uart_text();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next();
                // Parameter and intermediate bytes, then one final byte in 0x40..=0x7E.
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

/// M1: TF-M prints its banner on UART0, after the platform init programmed the panel SPI and
/// UART0 (`boot-flow.md` steps 1–3).
#[test]
fn m1_tfm_banner() {
    let Some(mut os) = Os::boot() else {
        return;
    };
    let stop = os.run_until(400_000_000, |o| {
        o.uart_text().contains("Booting TF-M v2.1.1")
    });
    let text = os.uart_text();
    println!(
        "m1: banner by cycle {} ({} system resets)",
        os.m.sched.now, os.m.resets
    );
    assert_eq!(stop, StopReason::Halted, "no banner; UART0:\n{text}");
    assert!(
        text.contains("[Sec Thread] Secure image initializing!"),
        "{text}"
    );
    let spi = os.m.bus.device::<Spi>("spi").expect("spi device");
    assert_eq!(spi.ctrl1(), 0x0008_0012);
    let uart0 = os.m.bus.device::<Uart16550>("uart0").expect("uart0 device");
    assert_eq!(uart0.divisor(), 13);
}

/// Peripheral/PPB unmapped accesses that are expected: 0xE004_3018 (RAZ/WI by design) and
/// internal-PPB misses (DWT/ITM, 0xE000_0000..=0xE003_FFFF). Later tasks append (range, reason)
/// pairs only together with a `boot-flow.md` entry explaining them.
const KNOWN_UNMODELLED: &[(RangeInclusive<u32>, &str)] = &[
    (
        0xE004_3018..=0xE004_301B,
        "EPPB 0xE004_3018: RAZ/WI by design (security.md)",
    ),
    (
        0xE000_0000..=0xE003_FFFF,
        "internal PPB not modelled (DWT/ITM)",
    ),
];

/// Device windows the secure image already programs before the hand-off (its splash: GPIO
/// panel reset, PL111 timing/framebuffer, backlight PWM; `gpio.md`, `lcdc.md`) whose models
/// later tasks of this plan add. Not a permanent allowance: each entry names its task, and
/// [`assert_pending_still_unmodelled`] fails once the device is modelled, so that task removes
/// the entry (`boot-flow.md`, "Milestones").
const PENDING_DEVICES: &[(RangeInclusive<u32>, &str)] = &[
    (0x4000_4000..=0x4000_4FFF, "LCDC (Task 18)"),
    (0x5000_4000..=0x5000_4FFF, "LCDC, Secure alias (Task 18)"),
    (0x4010_4000..=0x4010_4FFF, "backlight PWM (Task 18)"),
    (
        0x5010_4000..=0x5010_4FFF,
        "backlight PWM, Secure alias (Task 18)",
    ),
];

/// Each NS-alias [`PENDING_DEVICES`] window still has unmapped accesses, i.e. its device is not
/// modelled yet. When this fails, the device is in: remove its entries.
fn assert_pending_still_unmodelled(m: &Machine) {
    for (range, what) in PENDING_DEVICES
        .iter()
        .filter(|(r, _)| *r.start() < 0x5000_0000)
    {
        assert!(
            m.bus.log.entries().any(|(a, _)| range.contains(&a.addr)),
            "{what} is modelled now: remove its PENDING_DEVICES entries"
        );
    }
}

/// IRQ lines that a healthy boot never asserts: 7 (MPC), 8 (PPC), 10, 17, 22, 23 and 26.
const FATAL_STUB_IRQS: u64 = 1 << 7 | 1 << 8 | 1 << 10 | 1 << 17 | 1 << 22 | 1 << 23 | 1 << 26;

/// The key source the secure image reads while it is unmapped (`boot-flow.md` §2): reads 0.
const KEY_SOURCE: RangeInclusive<u32> = 0x1800_E000..=0x1800_E0FF;

/// The first BSP instruction, where the secure image's `bxns` lands (`boot-flow.md` §2 step 12).
const NS_ENTRY: u32 = 0x0011_6D50;

/// Log entries in 0x4000_0000..=0x5FFF_FFFF or 0xE000_0000..=0xE00F_FFFF not covered by
/// [`KNOWN_UNMODELLED`] (or, until their tasks land, [`PENDING_DEVICES`]), plus any other
/// non-memory entry outside [`KEY_SOURCE`].
fn unexpected_unmapped(m: &Machine) -> Vec<UnmappedAccess> {
    let known = |a: u32| {
        KNOWN_UNMODELLED
            .iter()
            .chain(PENDING_DEVICES)
            .any(|(r, _)| r.contains(&a))
    };
    m.bus
        .log
        .entries()
        .map(|(a, _)| a)
        .filter(|a| {
            let periph = (0x4000_0000..=0x5FFF_FFFF).contains(&a.addr)
                || (0xE000_0000..=0xE00F_FFFF).contains(&a.addr);
            if periph {
                !known(a.addr)
            } else {
                !KEY_SOURCE.contains(&a.addr)
            }
        })
        .cloned()
        .collect()
}

/// Run `os` to the NS hand-off breakpoint.
fn boot_to_handoff(mut os: Os) -> Os {
    os.m.breakpoints.insert(NS_ENTRY);
    let stop = os.run_until(1_500_000_000, |_| false);
    println!(
        "m2: NS hand-off at cycle {} ({} system resets)",
        os.m.sched.now, os.m.resets
    );
    assert_eq!(
        stop,
        StopReason::Breakpoint(NS_ENTRY),
        "UART0:\n{}",
        os.uart_text()
    );
    os
}

/// Run from the hand-off until the BSP prints its banner.
fn run_to_bsp(os: &mut Os) {
    os.m.breakpoints.remove(&NS_ENTRY);
    let stop = os.run_until(300_000_000, |o| o.uart_text().contains("Starting BSP"));
    println!(
        "m2: `Starting BSP` by cycle {} ({} system resets)",
        os.m.sched.now, os.m.resets
    );
    assert_eq!(stop, StopReason::Halted, "UART0:\n{}", os.uart_text());
}

/// M2: the secure boot completes (ITS, PS, crypto, NV counters, attestation) and hands off to
/// the NS BSP, which prints its banner (`boot-flow.md` §2 steps 4–12, §3).
#[test]
fn m2_ns_handoff() {
    let Some(os) = Os::boot() else {
        return;
    };
    let mut os = boot_to_handoff(os);
    let text = os.uart_plain();
    assert_eq!(os.m.cpu.scs.scb.vtor.ns, 0x0010_0400);
    assert_eq!(os.m.cpu.regs.msp.ns, 0x2004_6D20);
    assert_eq!(os.m.resets, 0, "UART0:\n{text}");
    for line in [
        "Creating an empty ITS flash layout.",
        "Creating an empty PS flash layout.",
        "Init Mbed TLS 3.6.2... complete",
    ] {
        assert!(text.contains(line), "missing {line:?}; UART0:\n{text}");
    }
    assert!(!text.contains("FATAL"), "{text}");
    assert!(!text.contains("Restored Saved Data"), "{text}");
    let nv = os.m.nv_state();
    assert!(
        nv.secregs()
            .as_chunks::<32>()
            .0
            .iter()
            .any(|slot| slot[28..32] == [0x42, 0x00, 0xDE, 0xC0]),
        "no NV counter slot is provisioned"
    );
    let sha = os.m.bus.device::<Sha256Engine>("sha").expect("sha device");
    assert_ne!(sha.state(), [0; 8]);
    assert!(
        nv.flash()[0xCF_0000..0xCF_2400].iter().all(|&b| b == 0xFF),
        "the mailbox at 0xCF_0000 was erased or written"
    );
    assert_eq!(os.m.bus.irq_lines() & FATAL_STUB_IRQS, 0);
    let bad = unexpected_unmapped(&os.m);
    assert!(bad.is_empty(), "unexpected unmapped accesses: {bad:#x?}");
    assert_pending_still_unmodelled(&os.m);
    let before_ns = os.m.bus.log.len();

    run_to_bsp(&mut os);
    for (a, n) in os.m.bus.log.entries().skip(before_ns).take(12) {
        println!(
            "m2: first NS unmapped: {} {:#010x} w{} {:x?} pc {:#010x} x{n}",
            if a.write.is_some() { "write" } else { "read" },
            a.addr,
            a.width,
            a.write,
            a.pc
        );
    }
    assert_eq!(os.m.resets, 0);
    assert_eq!(os.m.bus.irq_lines() & FATAL_STUB_IRQS, 0);
}

/// M2: the ITS layout written on the first boot is found on the next power-on from the same
/// non-volatile state.
#[test]
fn m2_its_survives_power_cycle() {
    let Some(os) = Os::boot() else {
        return;
    };
    let mut os = boot_to_handoff(os);
    run_to_bsp(&mut os);
    let Some(os2) = Os::boot_nv(os.m.nv_state()) else {
        return;
    };
    let os2 = boot_to_handoff(os2);
    let text = os2.uart_plain();
    assert!(
        !text.contains("Creating an empty ITS flash layout."),
        "ITS was recreated; UART0:\n{text}"
    );
}

/// M2: nothing touches SRAM past 1.5 MiB (a 2 MiB SRAM would have hidden such accesses).
#[test]
fn m2_no_access_past_1_5_mib_sram() {
    let Some(os) = Os::boot() else {
        return;
    };
    let mut os = boot_to_handoff(os);
    run_to_bsp(&mut os);
    let past: Vec<_> =
        os.m.bus
            .log
            .entries()
            .map(|(a, _)| a)
            .filter(|a| {
                (0x2018_0000..0x2020_0000).contains(&a.addr)
                    || (0x3018_0000..0x3020_0000).contains(&a.addr)
            })
            .cloned()
            .collect();
    assert!(past.is_empty(), "accesses past 1.5 MiB of SRAM: {past:#x?}");
}
