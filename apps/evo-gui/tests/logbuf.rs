//! The debug panel's UART2 console and log view, fed by the app loop. One test only: the log
//! ring is global to the process.

mod common;

use common::{TempDir, harness_with, test_options};
use evo_gui::app::AppOptions;
use evo_gui::backend::fake::FakeBackend;
use evo_gui::backend::{EmuBackend, UartId};
use evo_gui::platform::FakePlatform;

#[test]
fn uart2_and_log_reach_the_debug_panel() {
    let dir = TempDir::new("evo-gui-logbuf", false);
    let mut fake = FakeBackend::new();
    fake.send_uart(UartId::Uart2, b"x");
    let opts = AppOptions {
        start_paused: true,
        ..test_options(&dir, FakePlatform::shared())
    };
    evo_gui::logbuf::record("INFO t: y".to_owned());
    let h = harness_with(fake, opts);
    assert_eq!(h.state().debug().console(UartId::Uart2).text(), "x");
    assert_eq!(h.state().debug().console(UartId::Uart0).text(), "");
    assert_eq!(
        h.state().debug().log_lines().last().map(String::as_str),
        Some("INFO t: y")
    );
}
