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
use evo_hw::dev::lcdc::Pwm;
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

/// IRQ lines that a healthy boot never asserts: 7 (MPC), 8 (PPC), 10, 17, 22, 23 and 26.
const FATAL_STUB_IRQS: u64 = 1 << 7 | 1 << 8 | 1 << 10 | 1 << 17 | 1 << 22 | 1 << 23 | 1 << 26;

/// The key source the secure image reads while it is unmapped (`boot-flow.md` §2): reads 0.
/// Its 4 KiB page: the offsets read depend on earlier data (with correct SHA-256 digests one
/// 16-byte read moved to 0x1800_E0F4, past the first 256 bytes; `boot-flow.md` "M4 notes").
const KEY_SOURCE: RangeInclusive<u32> = 0x1800_E000..=0x1800_EFFF;

/// The first BSP instruction, where the secure image's `bxns` lands (`boot-flow.md` §2 step 12).
const NS_ENTRY: u32 = 0x0011_6D50;

/// Log entries in 0x4000_0000..=0x5FFF_FFFF or 0xE000_0000..=0xE00F_FFFF not covered by
/// [`KNOWN_UNMODELLED`], plus any other non-memory entry outside [`KEY_SOURCE`].
fn unexpected_unmapped(m: &Machine) -> Vec<UnmappedAccess> {
    let known = |a: u32| KNOWN_UNMODELLED.iter().any(|(r, _)| r.contains(&a));
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

/// The BSP's boot-combo result byte (`keypad.md` "Boot key combos"): 10 = no combo, else the
/// index of the matched combo in the table at NS 0x0018_3DF0.
const COMBO_RESULT: u32 = 0x2001_0DB8;
/// No combo matched.
const COMBO_NONE: u32 = 10;
/// Combo 0: ALPHA + LN + ENTER, the Self-Test menu.
const COMBO_SELF_TEST: u32 = 0;
/// Combo 9: ALPHA + SIN + ENTER, the Engineering menu.
const COMBO_ENGINEERING: u32 = 9;
/// In the BSP main thread, just after `r4 = get_combo()` (0x0015_D482: `bl`, `mov r4, r0`).
const AFTER_GET_COMBO: u32 = 0x0015_D488;

/// Boot with `keys` held from power-on (as on hardware: hold them, then press ON) and return
/// what the BSP's boot-combo matcher picked: `get_combo()`'s result and the result byte.
/// `None` when the milestone tests are skipped.
fn boot_combo(keys: &[&str]) -> Option<(u32, Option<u32>)> {
    let mut os = Os::boot()?;
    for name in keys {
        let key = evo_core::Key::from_name(name).expect("a key name");
        os.m.set_key(key, true);
    }
    os.m.breakpoints.insert(AFTER_GET_COMBO);
    let stop = os.run_until(2_500_000_000, |_| false);
    let combo = os.m.cpu.regs.r[4];
    let keypad_misses: Vec<_> =
        os.m.bus
            .log
            .entries()
            .map(|(a, _)| a)
            .filter(|a| (0x4010_3000..=0x4010_3FFF).contains(&a.addr))
            .cloned()
            .collect();
    assert!(
        keypad_misses.is_empty(),
        "unhandled keypad accesses: {keypad_misses:#x?}"
    );
    let result = os.m.bus.peek(COMBO_RESULT, 1);
    println!(
        "m2: {keys:?}: get_combo returned {combo} by cycle {} (result byte {result:?})",
        os.m.sched.now
    );
    assert_eq!(
        stop,
        StopReason::Breakpoint(AFTER_GET_COMBO),
        "UART0:\n{}",
        os.uart_text()
    );
    Some((combo, result))
}

/// ALPHA + LN + ENTER held across the power-on reset selects Self-Test.
#[test]
fn m2_self_test_boot_combo() {
    if let Some((combo, result)) = boot_combo(&["alpha", "ln", "enter"]) {
        assert_eq!(combo, COMBO_SELF_TEST);
        assert_eq!(result, Some(COMBO_SELF_TEST));
    }
}

/// ALPHA + SIN + ENTER selects the Engineering menu.
#[test]
fn m2_engineering_boot_combo() {
    if let Some((combo, result)) = boot_combo(&["alpha", "sin", "enter"]) {
        assert_eq!(combo, COMBO_ENGINEERING);
        assert_eq!(result, Some(COMBO_ENGINEERING));
    }
}

/// With no key held the matcher finds nothing.
#[test]
fn m2_no_boot_combo() {
    if let Some((combo, result)) = boot_combo(&[]) {
        assert_eq!(combo, COMBO_NONE);
        assert_eq!(result, Some(COMBO_NONE));
    }
}

/// A cycle inside the window where the BSP's TI logo is on the screen alone (about 80 M to
/// 100 M: drawn with the `Starting BSP` banner, joined by a busy spinner by 105 M; `boot-flow.md`
/// "M3 notes"). `m3_find_settled_logo` cannot find this frame: its 50 M slices start after it,
/// and what it reports is a step of the charging animation that follows.
const M3_CYCLES: u64 = 90_000_000;
/// Lowercase hex SHA-256 of the approved logo frame's `Frame::rgb` (a hash holds no TI data).
const M3_SCREEN_SHA256: &str = "f90877dd5646659cb227dab64bbe301c88207c995eeb708a6843e50d3dc86c30";
/// `m3_find_settled_logo` slice length.
const M3_SLICE: u64 = 50_000_000;
/// `m3_find_settled_logo` stops searching here.
const M3_SEARCH_END: u64 = 3_000_000_000;

/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The workspace `target/` directory.
fn target_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

/// Write `frame` as an RGB8 PNG.
fn write_png(path: &std::path::Path, frame: &evo_hw::dev::lcdc::Frame) {
    let file = std::fs::File::create(path).expect("create the PNG");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.width, frame.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("PNG header");
    w.write_image_data(&frame.rgb).expect("PNG data");
}

/// Search helper, run by hand: from the M2 BSP-banner cycle, run in [`M3_SLICE`] slices up to
/// [`M3_SEARCH_END`]; after each slice hash `screen()`; stop after three identical non-blank
/// frames in a row; print the first cycle of the run of three and write that frame to
/// `target/m3.png`. With OS 7.1.0.4421 it stops on the charging animation (`boot-flow.md`
/// "M3 notes"), whose steps last longer than three slices; see [`M3_CYCLES`].
#[test]
#[ignore]
fn m3_find_settled_logo() {
    let Some(os) = Os::boot() else {
        return;
    };
    let mut os = boot_to_handoff(os);
    run_to_bsp(&mut os);
    // (first cycle of the current run of equal frames, its hash, run length)
    let mut run: Option<(u64, String, u32)> = None;
    while os.m.sched.now < M3_SEARCH_END {
        let stop = os.run_until(M3_SLICE, |_| false);
        assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
        let now = os.m.sched.now;
        let frame = os.m.screen().filter(|f| {
            let px = f.rgb.as_chunks::<3>().0;
            px.iter().any(|p| Some(p) != px.first())
        });
        let Some(frame) = frame else {
            println!("m3: {now}: blank or off");
            run = None;
            continue;
        };
        let hash = sha256_hex(&frame.rgb);
        println!("m3: {now}: {hash}");
        run = match run {
            Some((first, h, n)) if h == hash => Some((first, h, n + 1)),
            _ => Some((now, hash, 1)),
        };
        if let Some((first, h, 3)) = &run {
            println!("m3: settled at cycle {first}: {h}");
            let path = target_dir().join("m3.png");
            write_png(&path, &frame);
            println!("m3: wrote {}", path.display());
            return;
        }
    }
    panic!("no settled frame by cycle {M3_SEARCH_END}");
}

/// M3: the BSP draws its boot logo; the emulated screen shows the approved image, the
/// backlight is on, and the run is deterministic.
#[test]
fn m3_boot_logo() {
    let Some(mut os) = Os::boot() else {
        return;
    };
    let stop = os.run_until(M3_CYCLES, |_| false);
    assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    let frame = os.m.screen().expect("the LCD is on");
    assert_eq!((frame.width, frame.height), (320, 240));
    let hash = sha256_hex(&frame.rgb);
    println!("m3: screen at cycle {} is {hash}", os.m.sched.now);
    assert_eq!(hash, M3_SCREEN_SHA256);
    let pwm = os.m.bus.device::<Pwm>("pwm").expect("pwm device");
    assert!(pwm.brightness() > 0.0, "the backlight is off");
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
    assert_eq!(os.m.bus.irq_lines() & FATAL_STUB_IRQS, 0);
    let bad = unexpected_unmapped(&os.m);
    assert!(bad.is_empty(), "unexpected unmapped accesses: {bad:#x?}");

    let Some(mut os2) = Os::boot() else {
        return;
    };
    os2.run_until(M3_CYCLES, |_| false);
    let frame2 = os2.m.screen().expect("the LCD is on (second run)");
    assert_eq!(
        sha256_hex(&frame2.rgb),
        hash,
        "the boot is not deterministic"
    );
}

/// Image 1's reset handler (`boot-flow.md` §3.4): the BSP's launch code `blx`es to it.
const IMAGE1_ENTRY: u32 = 0x0044_BC90;
/// Where image 1's entry trampoline stores the BSP service table pointer (`boot-flow.md` §3.4).
const OS_SERVICE_TABLE_PTR: u32 = 0x2009_B100;
/// `m4_image1_launches` cycle budget: the measured launch cycle rounded up to the next 500 M.
const M4_BUDGET: u64 = 1_000_000_000;
/// Guest milliseconds to cycles.
const fn ms(n: u64) -> u64 {
    n * 156_000
}

impl Os {
    /// Tap `key` at cycle `at` (absolute): run to `at`, press for 50 ms, release.
    fn tap_at(&mut self, at: u64, key: &str) -> StopReason {
        let key = evo_core::Key::from_name(key).expect("a key name");
        let stop = self.run_until(at.saturating_sub(self.m.sched.now), |_| false);
        if stop != StopReason::CycleLimit {
            return stop;
        }
        self.m.set_key(key, true);
        let stop = self.run_until(ms(50), |_| false);
        self.m.set_key(key, false);
        stop
    }
}

/// M4: on a fresh unit (empty ITS) the BSP starts the Self-Test app; its menu's
/// "0. Exit this menu" launches OS image 1 (`boot-flow.md` "M4 notes").
#[test]
fn m4_image1_launches() {
    let Some(mut os) = Os::boot() else {
        return;
    };
    os.m.breakpoints.insert(IMAGE1_ENTRY);
    // The Self-Test app shows the unit info screen first; ENTER opens the menu, 0 exits it.
    for (at, key) in [(ms(2500), "enter"), (ms(3100), "0")] {
        let stop = os.tap_at(at, key);
        assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    }
    let stop = os.run_until(M4_BUDGET - os.m.sched.now, |_| false);
    println!(
        "m4: image 1 entered at cycle {} ({} system resets)",
        os.m.sched.now, os.m.resets
    );
    assert_eq!(
        stop,
        StopReason::Breakpoint(IMAGE1_ENTRY),
        "UART0:\n{}",
        os.uart_text()
    );
    assert_eq!(os.m.resets, 0);
    // r0 is the BSP service table, which the OS entry trampoline stores at 0x2009_B100.
    let table = os.m.cpu.regs.r[0];
    println!("m4: BSP service table at {table:#010x}");

    // What the OS does next: 500 M more cycles.
    let before = os.m.bus.log.len();
    let uart_before = os.uart0.len();
    os.m.breakpoints.remove(&IMAGE1_ENTRY);
    let stop = os.run_until(500_000_000, |_| false);
    println!("m4: +500 M cycles: {stop:?} at cycle {}", os.m.sched.now);
    assert_eq!(
        os.m.bus.peek(OS_SERVICE_TABLE_PTR, 4),
        Some(table),
        "the OS did not store the BSP service table"
    );
    for (a, n) in os.m.bus.log.entries().skip(before).take(16) {
        println!(
            "m4: next unmapped: {} {:#010x} w{} {:x?} pc {:#010x} x{n}",
            if a.write.is_some() { "write" } else { "read" },
            a.addr,
            a.width,
            a.write,
            a.pc
        );
    }
    println!(
        "m4: UART0 after the launch: {:?}",
        String::from_utf8_lossy(&os.uart0[uart_before..])
    );
    assert_eq!(os.m.bus.irq_lines() & FATAL_STUB_IRQS, 0);
}

// ---- Every key on the OS (`keypad.md` "Verification on the OS") ----

/// `all_keys_type_expected_text`: the app launcher is up by this cycle after the M4 Self-Test
/// exit.
const LAUNCHER_CYCLES: u64 = 2_500_000_000;
/// Guest time after the last tap of a sequence, for the OS to finish drawing.
const SETTLE_MS: u64 = 150;

/// Edit-line tokens of OS 7.1.0.4421: the 16-bit codes the calculator app stores for what a
/// key types (each was checked on the emulated screen, `keypad.md`).
mod tok {
    /// The digit `d`.
    pub const fn digit(d: u16) -> u16 {
        0xE401 + d
    }
    /// The letter `c` (`A`..=`Z`); θ follows Z.
    pub const fn letter(c: u8) -> u16 {
        0xE800 + (c - b'A') as u16
    }
    pub const DECPNT: u16 = 0xE40B;
    pub const IMAG_I: u16 = 0xE40E;
    pub const LPAREN: u16 = 0xE410;
    pub const RPAREN: u16 = 0xE411;
    pub const QUOTE: u16 = 0xE416;
    pub const COMMA: u16 = 0xE417;
    pub const COLON: u16 = 0xE418;
    pub const SPACE: u16 = 0xE419;
    pub const QUESTION: u16 = 0xE41B;
    pub const STORE: u16 = 0xE41D;
    pub const SQUARED: u16 = 0xE41F;
    pub const ADD: u16 = 0xE428;
    pub const SUB: u16 = 0xE429;
    pub const MUL: u16 = 0xE42A;
    pub const DIV: u16 = 0xE42B;
    pub const POWER: u16 = 0xE42C;
    pub const XROOT: u16 = 0xE42D;
    pub const NEG: u16 = 0xE42E;
    pub const ABS: u16 = 0xE430;
    pub const SQRT: u16 = 0xE439;
    pub const LN: u16 = 0xE43B;
    pub const EXP: u16 = 0xE43C;
    pub const LOG: u16 = 0xE43D;
    pub const TEN_POW: u16 = 0xE43E;
    pub const SIN: u16 = 0xE43F;
    pub const ASIN: u16 = 0xE440;
    pub const COS: u16 = 0xE441;
    pub const ACOS: u16 = 0xE442;
    pub const TAN: u16 = 0xE443;
    pub const ATAN: u16 = 0xE444;
    pub const TO_DEC: u16 = 0xE426;
    pub const TO_FRAC: u16 = 0xE427;
    pub const SORT_A: u16 = 0xE4D9;
    pub const CLR_DRAW: u16 = 0xE4EA;
    pub const FRAC: u16 = 0xE6C2;
    pub const THETA: u16 = 0xE81A;
    pub const ANS: u16 = 0xE81B;
    pub const Y1: u16 = 0xE840;
    pub const XMIN: u16 = 0xE98F;
    /// MathPrint's x²: a superscript template holding 2.
    pub const MP_SQUARED: [u16; 3] = [0xE12B, 0x0002, 0xE130];
}

/// Typed after every sequence, to find the edit line: 9 8.
const SUFFIX: [u16; 2] = [tok::digit(9), tok::digit(8)];

/// The calculator app, driven from save states taken once the OS is up.
struct Calc {
    m: Machine,
}

impl Calc {
    /// Tap each whitespace-separated key name of `keys` as a key script does (hold
    /// `TAP_HOLD_MS`, then wait `TAP_GAP_MS`).
    fn keys(&mut self, keys: &str) {
        for name in keys.split_whitespace() {
            let key = evo_core::Key::from_name(name).unwrap_or_else(|| panic!("no key {name}"));
            self.m.set_key(key, true);
            self.m.run(ms(evo_core::keys::TAP_HOLD_MS));
            self.m.set_key(key, false);
            self.m.run(ms(evo_core::keys::TAP_GAP_MS));
        }
    }

    /// From `state`, tap `keys`, then 9 8, and return the edit line before the cursor without
    /// the 9 8.
    fn line(&mut self, state: &[u8], keys: &str) -> Vec<u16> {
        self.m.load_state(state).expect("load the state");
        self.keys(keys);
        self.keys("9 8");
        self.m.run(ms(SETTLE_MS));
        let mut line = self.edit_line();
        assert!(line.ends_with(&SUFFIX), "[{keys}] {line:04x?}");
        line.truncate(line.len() - SUFFIX.len());
        line
    }

    /// From `state`, tap `keys` and return the screen.
    fn frame(&mut self, state: &[u8], keys: &str) -> Vec<u8> {
        self.m.load_state(state).expect("load the state");
        self.keys(keys);
        self.m.run(ms(SETTLE_MS));
        self.m.screen().expect("the LCD is on").rgb
    }

    /// The edit line up to the cursor. The OS keeps it in a gap buffer: a word pair {start,
    /// cursor} somewhere in RAM points at its tokens. The line ends in [`SUFFIX`], so this
    /// finds every copy of the suffix, then every word pair whose cursor is just past that copy
    /// and whose start is at most 1 KiB before it; exactly one line must come out.
    fn edit_line(&self) -> Vec<u16> {
        use evo_core::machine::SRAM_NS;
        let sram = self
            .m
            .bus
            .region_bytes(evo_hw::soc::SRAM_BUFFER)
            .expect("SRAM");
        let half = |a: usize| u16::from_le_bytes([sram[a], sram[a + 1]]);
        let word = |a: usize| u32::from_le_bytes([sram[a], sram[a + 1], sram[a + 2], sram[a + 3]]);
        let mut lines: Vec<Vec<u16>> = Vec::new();
        for at in (0..sram.len() - 4).step_by(2) {
            if [half(at), half(at + 2)] != SUFFIX {
                continue;
            }
            let cursor = SRAM_NS + at as u32 + 4;
            for d in (4..sram.len() - 3).step_by(4) {
                let start = word(d - 4);
                if word(d) == cursor
                    && start >= SRAM_NS
                    && start <= cursor
                    && cursor - start <= 0x400
                    && start % 2 == 0
                {
                    let from = (start - SRAM_NS) as usize;
                    let to = (cursor - SRAM_NS) as usize;
                    lines.push((from..to).step_by(2).map(half).collect());
                }
            }
        }
        lines.sort();
        lines.dedup();
        match <[_; 1]>::try_from(lines) {
            Ok([line]) => line,
            Err(lines) => panic!("no single edit line ending in 9 8: {lines:04x?}"),
        }
    }
}

/// Type `pairs` (key sequence, the token it types) on one line from `state` and compare token
/// by token; returns a line per mismatch, naming the sequence.
fn check_typing(calc: &mut Calc, state: &[u8], what: &str, pairs: &[(&str, u16)]) -> Vec<String> {
    let keys: Vec<&str> = pairs.iter().map(|&(k, _)| k).collect();
    let line = calc.line(state, &keys.join(" "));
    let want: Vec<u16> = pairs.iter().map(|&(_, t)| t).collect();
    let mut bad = Vec::new();
    if line.len() != want.len() {
        bad.push(format!("{what}: typed {line:04x?}, want {want:04x?}"));
    } else {
        for (&(keys, want), got) in pairs.iter().zip(&line) {
            if *got != want {
                bad.push(format!(
                    "{what}: `{keys}` typed {got:#06x}, want {want:#06x}"
                ));
            }
        }
    }
    println!("keys: {what}: {} sequences checked", pairs.len());
    bad
}

/// Every one of the 50 keys does on the real OS what its name says (`keypad.md` "Verification
/// on the OS"). After the M4 Self-Test exit and ENTER on the launcher (its first app is the
/// calculator), the calculator is switched to CLASSIC so that every key types flat tokens.
/// Keys that type are checked by the edit line's tokens; 2nd and ALPHA by the second and alpha
/// functions, whose letters are the ones TI's keypad labels give; the others by what they do:
/// menus by the token of a menu item, editing and navigation keys by the edit line they leave,
/// and the screens by comparing screens.
#[test]
fn all_keys_type_expected_text() {
    let Some(mut os) = Os::boot() else {
        return;
    };
    for (at, key) in [(ms(2500), "enter"), (ms(3100), "0")] {
        let stop = os.tap_at(at, key);
        assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    }
    let stop = os.run_until(LAUNCHER_CYCLES - os.m.sched.now, |_| false);
    assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    let mut calc = Calc { m: os.m };
    calc.keys("enter");
    calc.m.run(ms(SETTLE_MS));
    let mathprint = calc.m.save_state();
    let mathprint_home = calc.frame(&mathprint, "");

    let mut bad = Vec::new();
    // MODE: MathPrint types x² as a template; after CLASSIC (MODE, up to the first row, right,
    // ENTER, 2nd MODE = QUIT) it types the flat ².
    let mp_square = calc.line(&mathprint, "1 square");
    if mp_square != [&[tok::digit(1)][..], &tok::MP_SQUARED].concat() {
        bad.push(format!("MathPrint `1 square` typed {mp_square:04x?}"));
    }
    calc.m.load_state(&mathprint).expect("load the state");
    calc.keys("mode up up up right enter 2nd mode");
    calc.m.run(ms(SETTLE_MS));
    let classic = calc.m.save_state();

    // Keys that type.
    let mut typing: Vec<(String, u16)> = (0..10).map(|d| (d.to_string(), tok::digit(d))).collect();
    for (k, t) in [
        ("decpnt", tok::DECPNT),
        ("lparen", tok::LPAREN),
        ("rparen", tok::RPAREN),
        ("comma", tok::COMMA),
        ("sto", tok::STORE),
        ("square", tok::SQUARED),
        ("add", tok::ADD),
        ("sub", tok::SUB),
        ("mul", tok::MUL),
        ("div", tok::DIV),
        ("power", tok::POWER),
        ("chs", tok::NEG),
        ("ln", tok::LN),
        ("log", tok::LOG),
        ("sin", tok::SIN),
        ("cos", tok::COS),
        ("tan", tok::TAN),
        ("frac", tok::FRAC),
        ("xton", tok::letter(b'X')),
    ] {
        typing.push((k.to_owned(), t));
    }
    let typing: Vec<(&str, u16)> = typing.iter().map(|(k, t)| (k.as_str(), *t)).collect();
    bad.extend(check_typing(&mut calc, &classic, "typing", &typing));
    // 2nd: the second functions.
    bad.extend(check_typing(
        &mut calc,
        &classic,
        "2nd",
        &[
            ("2nd sin", tok::ASIN),
            ("2nd cos", tok::ACOS),
            ("2nd tan", tok::ATAN),
            ("2nd ln", tok::EXP),
            ("2nd log", tok::TEN_POW),
            ("2nd square", tok::SQRT),
            ("2nd power", tok::XROOT),
            ("2nd decpnt", tok::IMAG_I),
            ("2nd chs", tok::ANS),
        ],
    ));
    // ALPHA: the letters A..Z as TI's keypad labels them, then θ, space, :, ? and ".
    let mut alpha: Vec<(String, u16)> = [
        "math", "frac", "prgm", "power", "sin", "cos", "tan", "div", "square", "comma", "lparen",
        "rparen", "mul", "log", "7", "8", "9", "sub", "ln", "4", "5", "6", "add", "sto", "1", "2",
    ]
    .iter()
    .zip(b'A'..=b'Z')
    .map(|(k, c)| (format!("alpha {k}"), tok::letter(c)))
    .collect();
    for (k, t) in [
        ("3", tok::THETA),
        ("0", tok::SPACE),
        ("decpnt", tok::COLON),
        ("chs", tok::QUESTION),
        ("toggle", tok::QUOTE),
    ] {
        alpha.push((format!("alpha {k}"), t));
    }
    let alpha: Vec<(&str, u16)> = alpha.iter().map(|(k, t)| (k.as_str(), *t)).collect();
    bad.extend(check_typing(&mut calc, &classic, "alpha", &alpha));

    // Editing, navigation and menus, each from the CLASSIC state: (keys, the line they leave).
    let (one, four, seven) = (tok::digit(1), tok::digit(4), tok::digit(7));
    let cases: &[(&str, &[u16])] = &[
        ("1 4 7 del", &[one, four]),
        ("1 4 left", &[one]),
        ("1 4 left right", &[one, four]),
        ("1 4 7 clear", &[]),
        // ENTER runs the line; 2nd ENTER (ENTRY) brings it back.
        ("1 4 7 enter 2nd enter", &[one, four, seven]),
        // UP selects the last entry in the history; ENTER copies it.
        ("1 4 enter up enter", &[one, four]),
        // ON leaves for the launcher; ENTER comes back to the line.
        ("1 4 on enter", &[one, four]),
        // MATH: item 1 is ►Frac, item 2 ►Dec (Ans first on an empty line); DOWN and UP move.
        ("math enter", &[tok::ANS, tok::TO_FRAC]),
        ("math 2", &[tok::ANS, tok::TO_DEC]),
        ("math down enter", &[tok::ANS, tok::TO_DEC]),
        ("math down down up enter", &[tok::ANS, tok::TO_DEC]),
        // VARS 1 (Window…) 1 is Xmin; VARS right (Y-VARS) 1 (Function…) 1 is Y1.
        ("vars 1 1", &[tok::XMIN]),
        ("vars right 1 1", &[tok::Y1]),
        // STAT 2 is SortA(; 2nd PRGM (DRAW) 1 is ClrDraw.
        ("stat 2", &[tok::SORT_A]),
        ("2nd prgm 1", &[tok::CLR_DRAW]),
        // The ALPHA F-keys: F1 (Y=) 1 is n/d, F2 (WINDOW) 1 abs(, F4 (TRACE) 1 Y1.
        ("alpha yequ 1", &[tok::FRAC]),
        ("alpha window 1", &[tok::ABS]),
        ("alpha trace 1", &[tok::Y1]),
    ];
    for &(keys, want) in cases {
        let line = calc.line(&classic, keys);
        if line != want {
            bad.push(format!("`{keys}` left {line:04x?}, want {want:04x?}"));
        }
    }
    println!(
        "keys: {} editing, navigation and menu sequences checked",
        cases.len()
    );

    // The screens.
    let graph = calc.frame(&classic, "graph");
    // ZOOM 4 (Zoom Default) draws the default window, as GRAPH shows it.
    if calc.frame(&classic, "zoom 4") != graph {
        bad.push("`zoom 4` does not show the graph screen".into());
    }
    // ZOOM 6 (Zoom [-10,10]) changes what WINDOW shows.
    let window = calc.frame(&classic, "window");
    if calc.frame(&classic, "zoom 6 2nd mode window") == window {
        bad.push("`zoom 6` does not change the window".into());
    }
    // TOGGLE switches an answer between decimal and fraction, and back.
    let quarter = calc.frame(&classic, "1 div 4 enter");
    let toggled = calc.frame(&classic, "1 div 4 enter toggle");
    let back = calc.frame(&classic, "1 div 4 enter toggle toggle");
    if toggled == quarter || back != quarter {
        bad.push("`toggle` does not switch 1/4 between 0.25 and 1/4".into());
    }
    // Each screen key opens a screen of its own.
    let screens = [
        ("calculator", calc.frame(&classic, "")),
        ("graph", graph),
        ("window", window),
        ("zoom", calc.frame(&classic, "zoom")),
        ("yequ", calc.frame(&classic, "yequ")),
        ("mode", calc.frame(&classic, "mode")),
        ("on", calc.frame(&classic, "on")),
    ];
    for (i, (a, fa)) in screens.iter().enumerate() {
        for (b, fb) in &screens[i + 1..] {
            if fa == fb {
                bad.push(format!("`{a}` and `{b}` show the same screen"));
            }
        }
    }
    if screens[0].1 == mathprint_home {
        bad.push("CLASSIC does not change the status bar".into());
    }
    println!("keys: screens checked");
    assert!(bad.is_empty(), "keys that misbehave:\n{}", bad.join("\n"));
}

/// The selected tile's colour on OS 7.1.0.4421: image 1 picks RGB565 0x035E for it
/// (`movwne r0, #0x35e` at 0x004C_14E6; `lcdc.md` "Colour order"), decoded through
/// `Machine::screen` with the firmware's CONTROL (BGR = 1).
const SELECTION_BLUE: [u8; 3] = [0, 105, 247];

/// The colour order end to end: on the app launcher (after the M4 Self-Test exit) the selected
/// tile is blue, as on hardware, and nothing on the screen is that colour with red and blue
/// swapped (the orange of the old decode).
#[test]
fn launcher_selection_is_blue() {
    let Some(mut os) = Os::boot() else {
        return;
    };
    for (at, key) in [(ms(2500), "enter"), (ms(3100), "0")] {
        let stop = os.tap_at(at, key);
        assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    }
    let stop = os.run_until(LAUNCHER_CYCLES - os.m.sched.now, |_| false);
    assert_eq!(stop, StopReason::CycleLimit, "UART0:\n{}", os.uart_text());
    let frame = os.m.screen().expect("the LCD is on");
    let px = frame.rgb.as_chunks::<3>().0;
    let [r, g, b] = SELECTION_BLUE;
    let blue = px.iter().filter(|&&p| p == SELECTION_BLUE).count();
    let swapped = px.iter().filter(|&&p| p == [b, g, r]).count();
    println!("launcher: {blue} selection-blue pixels, {swapped} swapped");
    assert!(
        blue >= 1000,
        "the selected tile is not blue ({blue} pixels)"
    );
    assert_eq!(swapped, 0, "red and blue are swapped");
}
