//! Opt-in real-firmware coverage of the GUI's native, same-machine USB adapter.
use evo_gui::backend::{CLOCK_HZ, EmuBackend, machine::MachineBackend};
use evo_gui::connect::types::{CsvTarget, SendOptions, UploadFile, UsbOperation, UsbResult};
use std::path::Path;

#[test]
fn same_calculator_python_roundtrip_cancel_and_persist() {
    if cfg!(debug_assertions) {
        eprintln!("skipping native GUI USB test: use --release and EVO_OS_IMAGE");
        return;
    }
    let Some(bundle) = std::env::var_os("EVO_OS_IMAGE") else {
        eprintln!("skipping native GUI USB test: EVO_OS_IMAGE is not set");
        return;
    };
    let dir = std::env::temp_dir().join(format!("evo-gui-native-connect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut backend = MachineBackend::open(Path::new(&bundle), &dir).unwrap();
    if backend.needs_provision() {
        backend.provision().unwrap();
    }
    assert!(backend.usb_available());
    let identity = backend.state_tag();
    let mut progress = |p: evo_gui::connect::types::UsbProgress| {
        if let Some(detail) = p.detail {
            eprintln!("{detail}");
        }
        true
    };
    let before_invalid = backend.cycles();
    assert!(
        backend
            .usb(
                UsbOperation::SendMany(vec![
                    UploadFile {
                        filename: "VALID.py".into(),
                        bytes: b"print(1)".to_vec(),
                        options: SendOptions::default()
                    },
                    UploadFile {
                        filename: "broken.8xl2".into(),
                        bytes: b"invalid".to_vec(),
                        options: SendOptions::default()
                    },
                ]),
                &mut progress
            )
            .is_err()
    );
    assert_eq!(
        backend.cycles(),
        before_invalid,
        "validate every host file before touching the guest"
    );
    let UsbResult::Directory { files, info } =
        backend.usb(UsbOperation::Refresh, &mut progress).unwrap()
    else {
        panic!("directory expected")
    };
    assert!(info.contains("product"), "{info}");
    assert!(files.iter().any(|f| f.name == "L1"));
    assert!(
        backend.cycles() > before_invalid,
        "USB advances the same running machine"
    );
    let UsbResult::Files(logs) = backend.usb(UsbOperation::Logs, &mut progress).unwrap() else {
        panic!("diagnostic files expected")
    };
    assert_eq!(
        logs.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["info.bin", "events.bin"]
    );
    assert!(logs.iter().all(|f| !f.bytes.is_empty()));
    let source = b"print('native GUI USB')\n";
    backend
        .usb(
            UsbOperation::Send {
                filename: "ZGUI.py".into(),
                bytes: source.to_vec(),
                options: SendOptions {
                    archived: true,
                    overwrite: true,
                    ..Default::default()
                },
            },
            &mut progress,
        )
        .unwrap();
    let UsbResult::Directory { files, .. } =
        backend.usb(UsbOperation::Refresh, &mut progress).unwrap()
    else {
        panic!("directory expected")
    };
    let python = files
        .into_iter()
        .find(|f| f.name == "ZGUI")
        .expect("sent Python program");
    let received_result = backend
        .usb(UsbOperation::Receive(vec![python.clone()]), &mut progress)
        .unwrap();
    let UsbResult::Files(received) = received_result else {
        panic!("files expected, got {received_result:?}")
    };
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].name, "ZGUI.py");
    assert_eq!(received[0].bytes, source);
    let mut missing = python.clone();
    missing.name = "ZMISS".into();
    missing.token_name = evo_core::connect_files::encode_name("ZMISS", 15).unwrap();
    let UsbResult::Batch {
        files: partial,
        errors,
        ..
    } = backend
        .usb(
            UsbOperation::Receive(vec![python.clone(), missing]),
            &mut progress,
        )
        .unwrap()
    else {
        panic!("partial receive expected")
    };
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].bytes, source);
    assert_eq!(errors.len(), 1);
    let uploads = vec![
        UploadFile {
            filename: "ZGONE.py".into(),
            bytes: source.to_vec(),
            options: SendOptions {
                overwrite: true,
                ..Default::default()
            },
        },
        UploadFile {
            filename: "ZGUI.py".into(),
            bytes: source.to_vec(),
            options: SendOptions::default(),
        },
    ];
    let UsbResult::Batch {
        errors, message, ..
    } = backend
        .usb(UsbOperation::SendMany(uploads), &mut progress)
        .unwrap()
    else {
        panic!("duplicate send should stop batch")
    };
    assert!(message.contains("Sent 1 of 2"), "{message}");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("ZGUI"));
    for (name, source, target) in [
        ("L6", "1\n2\n3\n", CsvTarget::Lists),
        ("B", "1,2\n3,4\n", CsvTarget::Matrix),
    ] {
        let result = backend
            .usb(
                UsbOperation::Send {
                    filename: "numbers.csv".into(),
                    bytes: source.as_bytes().to_vec(),
                    options: SendOptions {
                        name: name.into(),
                        csv_target: target,
                        overwrite: true,
                        ..Default::default()
                    },
                },
                &mut progress,
            )
            .unwrap();
        assert!(matches!(result, UsbResult::Done(_)), "{result:?}");
    }
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        &[255, 0, 0, 255],
        1,
        1,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    let result = backend
        .usb(
            UsbOperation::Send {
                filename: "red.png".into(),
                bytes: png,
                options: SendOptions {
                    overwrite: true,
                    ..Default::default()
                },
            },
            &mut progress,
        )
        .unwrap();
    assert!(matches!(result, UsbResult::Done(_)), "{result:?}");
    let UsbResult::Directory { files, .. } =
        backend.usb(UsbOperation::Refresh, &mut progress).unwrap()
    else {
        panic!("directory expected")
    };
    let list = files.iter().find(|f| f.name == "L6").unwrap();
    let UsbResult::Files(native) = backend
        .usb(UsbOperation::Receive(vec![list.clone()]), &mut progress)
        .unwrap()
    else {
        panic!("files expected")
    };
    assert!(native[0].name.ends_with(".8xl2"));
    let result = backend
        .usb(
            UsbOperation::Send {
                filename: native[0].name.clone(),
                bytes: native[0].bytes.clone(),
                options: SendOptions {
                    overwrite: true,
                    ..Default::default()
                },
            },
            &mut progress,
        )
        .unwrap();
    assert!(matches!(result, UsbResult::Done(_)), "{result:?}");
    assert!(files.iter().any(|f| f.kind == 6));
    assert!(files.iter().any(|f| f.name == "Image1" && f.archived));
    let UsbResult::Screen(frame) = backend.usb(UsbOperation::Screen, &mut progress).unwrap() else {
        panic!("screen expected")
    };
    assert!(frame.is_valid());
    assert_eq!((frame.width, frame.height), (320, 240));
    assert!(
        backend
            .usb(UsbOperation::Screen, &mut |p| !(p.phase == "Receiving"
                && p.transferred > 1000))
            .is_err()
    );
    backend
        .usb(UsbOperation::DeviceInfo, &mut progress)
        .unwrap();
    assert_eq!(backend.state_tag(), identity);
    backend.persist().unwrap();
    backend.reset();
    backend.run_for(CLOCK_HZ * 5);
    let UsbResult::Directory { files, .. } =
        backend.usb(UsbOperation::Refresh, &mut progress).unwrap()
    else {
        panic!("directory expected")
    };
    assert!(files.iter().any(|f| f.name == "ZGUI"));
    backend
        .usb(UsbOperation::Delete(vec![python]), &mut progress)
        .unwrap();
    let UsbResult::Directory { files, .. } =
        backend.usb(UsbOperation::Refresh, &mut progress).unwrap()
    else {
        panic!("directory expected")
    };
    assert!(!files.iter().any(|f| f.name == "ZGUI"));
    backend.persist().unwrap();
    let bundle_bytes = std::fs::read(Path::new(&bundle)).unwrap();
    let version = evo_gui::backend::machine::inspect_bundle(&bundle_bytes)
        .unwrap()
        .build_version;
    let result = backend
        .usb(UsbOperation::InstallOs(bundle_bytes), &mut progress)
        .unwrap();
    assert!(matches!(result, UsbResult::Done(_)), "{result:?}");
    backend.persist().unwrap();
    backend.reset();
    backend.run_for(CLOCK_HZ * 5);
    let UsbResult::Text(info) = backend
        .usb(UsbOperation::DeviceInfo, &mut progress)
        .unwrap()
    else {
        panic!("device information expected after OS reinstall")
    };
    assert!(info.contains(&format!("OS version: {version}")), "{info}");
    assert_eq!(backend.state_tag(), identity);
    backend.persist().unwrap();
}
