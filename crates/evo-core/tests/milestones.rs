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

use evo_core::{Machine, StopReason, UartPort};
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
        let m = Machine::from_bundle(&bundle, &Layout::EVO_7_1).expect("the bundle boots");
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
