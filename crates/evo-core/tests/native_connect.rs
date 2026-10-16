use evo_core::{
    CPU_HZ, Machine,
    connect::{Connect, ConnectError, Phase},
    connect_files::{decode_screen, encode_python, parse_directory},
};
use evo_image::{Bundle, Layout};
use std::ops::ControlFlow;

#[test]
fn real_guest_native_resources_and_cancel() {
    if cfg!(debug_assertions) {
        return;
    }
    let Some(path) = std::env::var_os("EVO_OS_IMAGE") else {
        return;
    };
    let b = Bundle::open(path.as_ref()).unwrap();
    let mut m = Machine::from_bundle(&b, &Layout::EVO_7_1).unwrap();
    evo_core::provision(&mut m).unwrap();
    m.run(CPU_HZ * 5);
    let mut progress = |_| ControlFlow::Continue(());
    {
        let mut c = Connect::new(&mut m, &mut progress).unwrap();
        let attrs = c.get("hh01/sys/attributes").unwrap();
        println!("attributes {}", attrs.len());
        assert!(attrs.windows(7).any(|s| s == b"product"));
        let directory = c.get("hh01/inf/res?name=directory&gotohome=1").unwrap();
        println!("directory {}", directory.len());
        let screen = c.get("hh01/sys/screen").unwrap();
        println!("screen {}", screen.len());
        assert_eq!(decode_screen(&screen).unwrap().len(), 320 * 240 * 4);
        let uri = "hh01/xfr/var?name=%EE%A0%B0&type=1";
        let list = c.get(uri).unwrap();
        c.put(
            "hh01/xfr/var?name=%EE%A0%B0&type=1&memtarget=0&policy=1",
            &list,
        )
        .unwrap();
        assert_eq!(c.get(uri).unwrap(), list);
        assert!(!c.get("hh01/dbg/Get Event Info").unwrap().is_empty());
        assert!(!c.get("hh01/dbg/Get Events").unwrap().is_empty());
        let python = encode_python("ZUSBTEST", b"print(42)\n", false).unwrap();
        c.put("hh01/xfr/var?memtarget=0&policy=1", &python.bytes)
            .unwrap();
        let directory =
            parse_directory(&c.get("hh01/inf/res?name=directory&gotohome=1").unwrap()).unwrap();
        let file = directory.iter().find(|v| v.name == "ZUSBTEST").unwrap();
        let mut name = String::new();
        for token in file.token_name.as_bytes().chunks(4) {
            let code = u32::from_str_radix(std::str::from_utf8(token).unwrap(), 16).unwrap();
            for b in char::from_u32(code).unwrap().to_string().bytes() {
                name.push_str(&format!("%{b:02X}"));
            }
        }
        let received = c.get(&format!("hh01/xfr/var?name={name}&type=15")).unwrap();
        assert_eq!(
            evo_core::connect_files::export_variable(&received)
                .unwrap()
                .1,
            b"print(42)\n"
        );
        c.put(&format!("hh01/del/var?name={name}&type=15"), &[0])
            .unwrap();
        let directory =
            parse_directory(&c.get("hh01/inf/res?name=directory&gotohome=1").unwrap()).unwrap();
        assert!(!directory.iter().any(|v| v.name == "ZUSBTEST"));
        c.finish().unwrap();
    }
    {
        let mut cancel = |p: evo_core::connect::Progress| {
            if p.phase == Phase::Receiving && p.transferred > 1000 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut c = Connect::new(&mut m, &mut cancel).unwrap();
        assert!(matches!(
            c.get("hh01/sys/screen"),
            Err(ConnectError::Cancelled)
        ));
    }
    let mut c = Connect::new(&mut m, &mut progress).unwrap();
    assert!(!c.get("hh01/sys/attributes").unwrap().is_empty());
}
