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

fn input(m: &mut Machine, ep: u8) -> Vec<u8> {
    for _ in 0..1000 {
        run(m, 100_000);
        match m.usb_in(ep) {
            Ok(bytes) => return bytes,
            Err(UsbError::Nak) => {}
            Err(e) => panic!("endpoint {ep} IN: {e}"),
        }
    }
    panic!(
        "endpoint {ep} IN timed out: {:?}",
        m.peripherals().into_iter().find(|d| d.name == "usb")
    );
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

#[test]
fn real_os_usb_enumerates_cdc_and_answers_serial_handshake() {
    if cfg!(debug_assertions) {
        eprintln!("skipping: USB guest test requires release");
        return;
    }
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping: EVO_OS_IMAGE not set");
        return;
    };
    let bundle = Bundle::open(path.as_ref()).unwrap();
    let mut m = Machine::from_bundle(&bundle, &Layout::EVO_7_1).unwrap();
    evo_core::provision(&mut m).unwrap();
    run(&mut m, CPU_HZ);
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
    println!("configuration: {config:02x?}");
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
}
