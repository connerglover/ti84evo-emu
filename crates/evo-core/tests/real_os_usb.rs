//! Real guest USB enumeration and CDC traffic. Requires the user's own EVO_OS_IMAGE and
//! a release build; no firmware image, descriptor blob or response implementation is bundled.

use evo_core::{CPU_HZ, Machine, StopReason, UsbError};
use evo_image::{Bundle, Layout};

fn run(m: &mut Machine, cycles: u64) {
    let stop = m.run(cycles);
    assert!(
        matches!(stop, StopReason::CycleLimit | StopReason::Idle),
        "{stop:?}: {:?}",
        m.recent_trace()
    );
}

fn input_result(m: &mut Machine, ep: u8) -> Result<Vec<u8>, UsbError> {
    for _ in 0..1000 {
        run(m, 100_000);
        match m.usb_in(ep) {
            Ok(bytes) => return Ok(bytes),
            Err(UsbError::Nak) => {}
            Err(e) => return Err(e),
        }
    }
    panic!(
        "endpoint {ep} IN timed out: {:?}",
        m.peripherals().into_iter().find(|d| d.name == "usb")
    );
}

fn input(m: &mut Machine, ep: u8) -> Vec<u8> {
    input_result(m, ep).unwrap()
}

fn output(m: &mut Machine, ep: u8, bytes: &[u8]) {
    for _ in 0..1000 {
        run(m, 100_000);
        match m.usb_out(ep, bytes) {
            Ok(()) => return,
            Err(UsbError::Nak) => {}
            Err(e) => panic!("endpoint {ep} OUT: {e}"),
        }
    }
    panic!("endpoint {ep} OUT timed out");
}

fn control(m: &mut Machine, setup: [u8; 8], data: &[u8]) -> Vec<u8> {
    m.usb_setup(setup).unwrap();
    let mut reply = Vec::new();
    if setup[0] & 0x80 != 0 {
        let requested = u16::from_le_bytes([setup[6], setup[7]]) as usize;
        while reply.len() < requested {
            let packet = input(m, 0);
            let short = packet.len() < 64;
            reply.extend(packet);
            if short {
                break;
            }
        }
        output(m, 0, &[]);
    } else {
        for packet in data.chunks(64) {
            output(m, 0, packet);
        }
        assert!(input(m, 0).is_empty(), "control status must be zero length");
    }
    run(m, 100_000);
    reply
}

fn provisioned_machine() -> Option<Machine> {
    if cfg!(debug_assertions) {
        eprintln!("skipping: USB guest test requires release");
        return None;
    }
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping: EVO_OS_IMAGE not set");
        return None;
    };
    let bundle = Bundle::open(path.as_ref()).unwrap();
    let mut m = Machine::from_bundle(&bundle, &Layout::EVO_7_1).unwrap();
    evo_core::provision(&mut m).unwrap();
    Some(m)
}

#[test]
fn real_os_usb_enumerates_cdc_and_answers_serial_handshake() {
    let Some(mut m) = provisioned_machine() else {
        return;
    };
    run(&mut m, CPU_HZ * 5);
    m.usb_attach(true);
    run(&mut m, CPU_HZ);
    m.usb_bus_reset().unwrap();
    run(&mut m, 1_000_000);
    let device = control(&mut m, [0x80, 6, 0, 1, 0, 0, 18, 0], &[]);
    assert_eq!(device.len(), 18);
    assert_eq!(
        &device[8..12],
        &[0x51, 0x04, 0x18, 0xE0],
        "TI Connect Evo USB identity"
    );
    control(&mut m, [0, 5, 1, 0, 0, 0, 0, 0], &[]);
    let config = control(&mut m, [0x80, 6, 0, 2, 0, 0, 255, 0], &[]);
    assert!(config.len() >= 9);
    assert_eq!(
        config.len(),
        u16::from_le_bytes([config[2], config[3]]) as usize
    );
    let mut cdc_interface = None;
    let mut bulk_in = None;
    let mut bulk_out = None;
    let mut cdc_data = false;
    let mut pos = 0;
    while pos + 2 <= config.len() {
        let n = config[pos] as usize;
        assert!(n >= 2 && pos + n <= config.len());
        let d = &config[pos..pos + n];
        if d[1] == 4 && n >= 9 {
            cdc_data = d[5] == 0x0A;
            if d[5] == 2 {
                cdc_interface = Some(d[2]);
            }
        }
        if d[1] == 5 && n >= 7 && d[3] & 3 == 2 && cdc_data {
            if d[2] & 0x80 != 0 {
                bulk_in = Some(d[2] & 15);
            } else {
                bulk_out = Some(d[2] & 15);
            }
        }
        pos += n;
    }
    let cdc = cdc_interface.expect("CDC ACM interface");
    control(&mut m, [0, 9, config[5], 0, 0, 0, 0, 0], &[]);
    // Official TI Connect Evo asks Web Serial for 115200 baud, 8 data bits, no parity.
    control(
        &mut m,
        [0x21, 0x20, 0, 0, cdc, 0, 7, 0],
        &[0, 0xC2, 1, 0, 0, 0, 8],
    );
    control(&mut m, [0x21, 0x22, 3, 0, cdc, 0, 0, 0], &[]);
    // Kermit send-init emitted by the unchanged public TI Connect Evo client.
    let init = b"\x010 S~' @-#Y1~.\"5M5\r";
    output(&mut m, bulk_out.expect("bulk OUT endpoint"), init);
    let reply = input(&mut m, bulk_in.expect("bulk IN endpoint"));
    println!("serial handshake: {reply:02x?}");
    assert!(!reply.is_empty());
    assert_eq!(reply[0], 1, "guest returned a Kermit packet");
    assert_eq!(reply[3], b'Y', "guest acknowledged the send-init packet");

    let report = control(&mut m, [0x81, 6, 0, 0x22, 3, 0, 63, 0], &[]);
    assert_eq!(report.len(), 63);
    assert_eq!(&report[..4], &[5, 1, 9, 6], "Generic Desktop / Keyboard");
    // The 7.1 guest rejects these class requests; the controller must expose its
    // STALL and allow a subsequent SETUP to recover, rather than synthesize success.
    for setup in [
        [0x21, 0x0A, 0, 4, 3, 0, 0, 0], // SET_IDLE
        [0xA1, 1, 0, 1, 3, 0, 8, 0],    // GET_REPORT(input)
        [0x21, 0x0A, 0, 0, 3, 0, 0, 0], // SET_IDLE(0)
    ] {
        m.usb_setup(setup).unwrap();
        assert_eq!(input_result(&mut m, 0), Err(UsbError::Stall));
    }
    assert_eq!(control(&mut m, [0x80, 6, 0, 1, 0, 0, 18, 0], &[]), device);
    assert_eq!(m.usb_in(4), Err(UsbError::Nak));

    m.usb_attach(false);
    assert_eq!(m.usb_in(2), Err(UsbError::Disconnected));
    run(&mut m, CPU_HZ);
    m.usb_attach(true);
    run(&mut m, CPU_HZ);
    m.usb_bus_reset().unwrap();
    run(&mut m, 1_000_000);
    assert_eq!(control(&mut m, [0x80, 6, 0, 1, 0, 0, 18, 0], &[]), device);
    control(&mut m, [0, 5, 2, 0, 0, 0, 0, 0], &[]);
    control(&mut m, [0, 9, config[5], 0, 0, 0, 0, 0], &[]);
    control(
        &mut m,
        [0x21, 0x20, 0, 0, cdc, 0, 7, 0],
        &[0, 0xC2, 1, 0, 0, 0, 8],
    );
    control(&mut m, [0x21, 0x22, 3, 0, cdc, 0, 0, 0], &[]);
    output(&mut m, bulk_out.unwrap(), init);
    let reply = input(&mut m, bulk_in.unwrap());
    println!("reconnected serial handshake: {reply:02x?}");
    assert_eq!(reply[3], b'Y');
}

#[test]
fn real_os_engineering_keyboard_and_vendor_loopback() {
    let Some(mut m) = provisioned_machine() else {
        return;
    };
    m.power_cycle();
    // Same read-only boot gates as provisioning: keys select the guest's own
    // engineering menu. No firmware instruction or guest RAM is patched.
    let combo = ["alpha", "sin", "enter"].map(|s| evo_core::Key::from_name(s).unwrap());
    for key in combo {
        m.set_key(key, true);
    }
    m.breakpoints.insert(0x0015_D488);
    assert_eq!(m.run(CPU_HZ * 10), StopReason::Breakpoint(0x0015_D488));
    m.breakpoints.clear();
    for key in combo {
        m.set_key(key, false);
    }
    m.breakpoints.insert(0x0011_7AFC);
    assert_eq!(m.run(CPU_HZ * 10), StopReason::Breakpoint(0x0011_7AFC));
    m.breakpoints.clear();
    run(&mut m, CPU_HZ / 2);
    // Engineering > Tests > Link Test > Computer Tests > Keyboard Simulator.
    for name in ["enter", "7", "3", "4", "1"] {
        let key = evo_core::Key::from_name(name).unwrap();
        m.set_key(key, true);
        run(&mut m, CPU_HZ / 20);
        m.set_key(key, false);
        run(&mut m, CPU_HZ / 2);
    }
    let uart = m.take_uart_tx(evo_core::UartPort::Uart0);
    assert!(String::from_utf8_lossy(&uart).contains("Keyboard Simulator"));
    m.usb_attach(true);
    run(&mut m, CPU_HZ);
    m.usb_bus_reset().unwrap();
    run(&mut m, 1_000_000);
    assert_eq!(control(&mut m, [0x80, 6, 0, 1, 0, 0, 18, 0], &[]).len(), 18);
    control(&mut m, [0, 5, 1, 0, 0, 0, 0, 0], &[]);
    control(&mut m, [0, 9, 1, 0, 0, 0, 0, 0], &[]);
    run(&mut m, CPU_HZ / 2);
    assert_eq!(m.usb_in(4), Err(UsbError::Nak));
    let enter = evo_core::Key::from_name("enter").unwrap();
    m.set_key(enter, true);
    assert_eq!(input(&mut m, 4), [0, 0, 0x28, 0, 0, 0, 0, 0]);
    m.set_key(enter, false);
    assert_eq!(input(&mut m, 4), [0; 8]);
    // Every byte value, full-size packets and a final short packet cross guest
    // DMA and the BSP's vendor loopback. Drain each echo to respect backpressure.
    let payload: Vec<u8> = (0..285).map(|n| n as u8).collect();
    let mut echoed = Vec::new();
    for packet in payload.chunks(64) {
        output(&mut m, 5, packet);
        echoed.extend(input(&mut m, 6));
    }
    assert_eq!(echoed, payload);
}
